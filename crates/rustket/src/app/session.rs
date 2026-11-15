//! Server lifecycle: login, logout, adding and removing servers, token
//! confirmations.

use std::time::Instant;

use iced::Task;
use rocketchat::models::User;
use rocketchat::rest::{LoginRequest, LoginResult};
use rocketchat::{Credentials, RestClient};
use rust_i18n::t;
use rustket_core::ServerId;
use rustket_core::config::{PinnedCertificate, ServerEntry, TokenKind};
use rustket_core::secrets::secret_key;
use rustket_core::workspace::ConnectionState;
use rustket_ui::widget::BannerKind;

use super::subscription::query_handler_task;
use super::{App, CONFIRM_ARM_DELAY, LOGOUT_TIMEOUT, Modal, Screen};
use crate::deeplink::Decision;
use crate::message::Message;
use crate::screens::add_server::{self, AddServer};
use crate::screens::login::{self, Login};
use crate::workspace::{ServerState, rail};

impl App {
    pub(super) fn show_login(&mut self, sid: &ServerId, reason: Option<String>) -> Task<Message> {
        let handler_registered = self.handler_registered;
        let Some(s) = self.servers.get_mut(sid) else {
            return Task::none();
        };
        s.restoring = false;
        match Login::new(sid.clone(), s.url.clone(), s.tls.clone(), None, reason) {
            Ok((mut login, task)) => {
                login.handler_registered = handler_registered;
                s.login = Some(login);
                let sid = sid.clone();
                task.map(move |m| Message::Login(sid.clone(), m))
            }
            Err(e) => {
                let body = crate::error::user_text(&e);
                self.toast(BannerKind::Error, t!("app.error"), body)
            }
        }
    }

    /// Removes token confirmations of `sid` (login cancelled, finished, …) and
    /// shows the next queued one.
    pub(super) fn drop_token_confirmations(&mut self, sid: &ServerId) {
        self.modals
            .retain(|m| !matches!(m, Modal::ConfirmToken { server_id, .. } if server_id == sid));
        self.confirm_queue.retain(|(s, _, _)| s != sid);
    }

    /// One confirmation at a time: shows the next queued one when the slot is
    /// free; a confirmation covered by another dialog is disarmed, and the one
    /// on top arms after a delay. Runs after every update.
    pub(super) fn show_next_confirmation(&mut self) -> Task<Message> {
        let shown = self
            .modals
            .iter()
            .any(|m| matches!(m, Modal::ConfirmToken { .. }));
        if !shown && let Some((server_id, credentials, user)) = self.confirm_queue.pop_front() {
            self.modals.push(Modal::ConfirmToken {
                server_id,
                credentials,
                user,
                armed: false,
                stamp: 0,
            });
        }
        self.disarm_covered_confirmations();
        self.rearm_top_confirmation()
    }

    /// When a confirmation is (or became) the top dialog it starts disarmed and
    /// arms after [`CONFIRM_ARM_DELAY`].
    fn rearm_top_confirmation(&mut self) -> Task<Message> {
        let stamp = self.next_stamp();
        let Some(Modal::ConfirmToken {
            armed, stamp: s, ..
        }) = self.modals.last_mut()
        else {
            return Task::none();
        };
        if *s != 0 {
            // Armed, or counting down.
            return Task::none();
        }
        *armed = false;
        *s = stamp;
        Task::perform(tokio::time::sleep(CONFIRM_ARM_DELAY), move |_| {
            Message::ArmConfirm(stamp)
        })
    }

    /// Disarms a confirmation that was covered by another dialog.
    fn disarm_covered_confirmations(&mut self) {
        let n = self.modals.len();
        for (i, m) in self.modals.iter_mut().enumerate() {
            if i + 1 < n
                && let Modal::ConfirmToken { armed, stamp, .. } = m
            {
                *armed = false;
                *stamp = 0;
            }
        }
    }

    pub(super) fn activate(&mut self, sid: ServerId) -> Task<Message> {
        self.activate_for(sid, true)
    }

    /// Shows server `sid`; `open_last` is false when the caller opens a
    /// specific room right after, so its last room is not opened first.
    pub(crate) fn activate_for(&mut self, sid: ServerId, open_last: bool) -> Task<Message> {
        if !self.servers.contains_key(&sid) {
            return Task::none();
        }
        // The rooms of the server left behind stay open but hidden.
        let mut tasks = Vec::new();
        if let Some(prev) = self.active.clone().filter(|p| *p != sid)
            && let Some(s) = self.servers.get_mut(&prev)
        {
            for rv in s.rooms.values_mut() {
                tasks.push(rv.hide(s.session.as_ref()).discard());
            }
        }
        self.active = Some(sid.clone());
        self.screen = Screen::Server;
        // Its last room, not opened while the server was in the background.
        if open_last
            && let Some(s) = self.servers.get(&sid)
            && s.active_room.is_none()
            && s.pending_room.is_none()
            && let Some(rid) = self
                .config
                .server(&sid)
                .and_then(|e| e.last_room.clone())
                .filter(|r| s.workspace.room(r).is_some())
        {
            tasks.push(self.open_room(&sid, &rid));
        }
        if self.config.active_server.as_ref() != Some(&sid) {
            self.config.active_server = Some(sid);
            tasks.push(self.save_config());
        }
        Task::batch(tasks)
    }

    pub(super) fn on_add_server(&mut self, m: add_server::Message) -> Task<Message> {
        let existing = self.server_urls();
        let Screen::AddServer(a) = &mut self.screen else {
            return Task::none();
        };
        let (task, out) = a.update(m, &existing);
        let task = task.map(Message::AddServer);
        match out {
            add_server::Out::None => task,
            add_server::Out::Cancel => {
                self.screen = if self.servers.is_empty() {
                    Screen::AddServer(Box::new(AddServer::new(false)))
                } else {
                    Screen::Server
                };
                task
            }
            add_server::Out::Added { probe, pins } => {
                let mut entry = ServerEntry::new(probe.url.as_str(), probe.meta.site_name.clone());
                entry.pinned_certificates = pins
                    .iter()
                    .map(|(host, fp)| PinnedCertificate {
                        host: host.clone(),
                        sha256: rocketchat::tls::fingerprint_hex(fp),
                    })
                    .collect();
                let mut state = match ServerState::from_entry(&entry) {
                    Ok(s) => s,
                    Err(e) => {
                        let body = crate::error::user_text(&e);
                        return self.toast(BannerKind::Error, t!("app.error"), body);
                    }
                };
                state.restoring = false;
                let (mut login, login_task) = match Login::new(
                    entry.id.clone(),
                    state.url.clone(),
                    state.tls.clone(),
                    Some(probe.meta.clone()),
                    None,
                ) {
                    Ok(pair) => pair,
                    Err(e) => {
                        let body = crate::error::user_text(&e);
                        return self.toast(BannerKind::Error, t!("app.error"), body);
                    }
                };
                login.handler_registered = self.handler_registered;
                state.login = Some(login);
                let sid = self.config.add_server(entry);
                self.config.active_server = Some(sid.clone());
                self.servers.insert(sid.clone(), state);
                self.active = Some(sid.clone());
                self.screen = Screen::Server;
                let sid2 = sid.clone();
                let login_task = login_task.map(move |m| Message::Login(sid2.clone(), m));
                Task::batch([
                    task,
                    login_task,
                    self.save_config(),
                    iced::widget::operation::focus("login-user"),
                ])
            }
        }
    }

    pub(super) fn on_login(&mut self, sid: ServerId, m: login::Message) -> Task<Message> {
        let Some(s) = self.servers.get_mut(&sid) else {
            return Task::none();
        };
        let Some(l) = s.login.as_mut() else {
            return Task::none();
        };
        let (task, out) = l.update(m);
        if l.pending.is_none() {
            self.drop_token_confirmations(&sid);
        }
        let sid2 = sid.clone();
        let task = task.map(move |m| Message::Login(sid2.clone(), m));
        let out_task = match out {
            login::Out::None => Task::none(),
            login::Out::LoggedIn(r, kind) => self.logged_in(&sid, *r, kind),
            login::Out::OpenUrl(url) => self.open_url(url),
            login::Out::DeepLink(link) => self.handle_link(&link),
            login::Out::RegisterHandler => self.open_register_handler(),
        };
        Task::batch([task, out_task])
    }

    /// A successful login: persist the token and the server entry, start the session.
    pub(super) fn logged_in(
        &mut self,
        sid: &ServerId,
        r: LoginResult,
        kind: TokenKind,
    ) -> Task<Message> {
        self.drop_token_confirmations(sid);
        let username = r.me.as_ref().and_then(|m| m.username.clone());
        let old_uid = self.config.server(sid).and_then(|e| e.user_id.clone());
        if let Some(entry) = self.config.server_mut(sid) {
            entry.user_id = Some(r.user_id.clone());
            entry.username = username;
            entry.token_kind = Some(kind);
        }
        let Some(s) = self.servers.get_mut(sid) else {
            return Task::none();
        };
        // A fresh login: no session of the old one is left to tell.
        s.reset();
        s.login = None;
        s.restoring = false;
        s.credentials = Some(r.credentials());
        s.workspace.apply_connection(ConnectionState::Connecting);
        if let Some(me) = r.me.clone() {
            s.workspace.apply_me(me);
        }
        let key = secret_key(sid, &r.user_id);
        let old_key = old_uid
            .filter(|u| u != &r.user_id)
            .map(|u| secret_key(sid, &u));
        let token = r.auth_token.clone();
        let sid2 = sid.clone();
        let store = self
            .secret_task(move |st| {
                if let Some(old) = old_key {
                    let _ = st.delete(&old);
                }
                st.set(&key, &token).map_err(|e| e.to_string())
            })
            .map(move |r| Message::TokenStored(sid2.clone(), r));
        Task::batch([store, self.save_config()])
    }

    fn purge_images(&mut self, sid: &ServerId) -> Task<Message> {
        let Some(url) = self.servers.get(sid).map(|s| s.url.clone()) else {
            return Task::none();
        };
        // Opened media is not kept per server: all of it goes.
        let media = crate::room::timeline::transfer::purge_media_task(
            self.images.disk_dir(),
            false,
            Message::Noop,
        );
        let images = match self.images.purge_server(sid, &url) {
            Some(dir) => Task::perform(
                async move {
                    let _ = tokio::task::spawn_blocking(move || std::fs::remove_dir_all(dir)).await;
                },
                |_| Message::Noop,
            ),
            None => Task::none(),
        };
        Task::batch([images, media])
    }

    fn delete_token(&mut self, sid: &ServerId) -> Task<Message> {
        let Some(uid) = self.config.server(sid).and_then(|e| e.user_id.clone()) else {
            return Task::none();
        };
        let key = secret_key(sid, &uid);
        self.secret_task(move |st| st.delete(&key).map_err(|e| e.to_string()))
            .map(|_| Message::Noop)
    }

    /// The server rejected the token: drop it and show the login screen.
    pub(super) fn logged_out(&mut self, sid: &ServerId, reason: Option<String>) -> Task<Message> {
        let mut e2e = Task::none();
        if let Some(s) = self.servers.get_mut(sid) {
            // Any rejected token also drops the cached E2E key (an unsaved
            // generated passphrase is kept), as the web does on every logout:
            // an E2E key reset closes the connections before its
            // `force_logout` goes out, so it shows up as a failed resume.
            let uid = self.config.server(sid).and_then(|e| e.user_id.clone());
            e2e = crate::e2e::forget_keys(s, self.secrets.as_ref(), uid.as_deref());
            // The token was rejected: nothing can be sent with it any more.
            s.reset();
        }
        let delete = self.delete_token(sid);
        let purge = self.purge_images(sid);
        let purge = Task::batch([purge, crate::cache::purge(self, sid)]);
        let reason = reason.map(|r| match r.as_str() {
            "force_logout" => t!("app.force_logout").into_owned(),
            _ => t!("app.session_expired").into_owned(),
        });
        let page = self.leave_pages_of(sid);
        // A server out of sight would otherwise go silent (the rail marks it too).
        let visible = self.active.as_ref() == Some(sid) && matches!(self.screen, Screen::Server);
        let notice = match self.servers.get(sid) {
            Some(s) if !visible => {
                let title = t!("app.signed_out_of", server = s.name.clone());
                let body = reason
                    .clone()
                    .unwrap_or_else(|| t!("app.session_expired").into_owned());
                self.toast(BannerKind::Warning, title, body)
            }
            _ => Task::none(),
        };
        Task::batch([
            e2e,
            delete,
            purge,
            page,
            notice,
            self.show_login(sid, reason),
        ])
    }

    /// The account, directory or admin page of a server that was just logged
    /// out gives way to its login screen; its dialogs close.
    fn leave_pages_of(&mut self, sid: &ServerId) -> Task<Message> {
        self.modals.retain(
            |m| !matches!(m, Modal::CreateRoom(id, _) | Modal::QuickSwitcher(id, _) if id == sid),
        );
        let shown = match &self.screen {
            Screen::Account(id, _) | Screen::Directory(id, _) | Screen::Admin(id, _) => id == sid,
            _ => false,
        };
        if shown {
            return self.activate(sid.clone());
        }
        Task::none()
    }

    /// User logout. A login token is revoked on the server (through the
    /// session, or a plain REST call when the session is not up); a personal
    /// access token or a token of unknown kind is only forgotten locally,
    /// unless `everywhere` (the user chose "sign out everywhere").
    pub(super) fn logout(&mut self, sid: &ServerId, everywhere: bool) -> Task<Message> {
        let Some(s) = self.servers.get(sid) else {
            return Task::none();
        };
        let handle = s.session.clone();
        let credentials = s.credentials.clone();
        let (url, tls) = (s.url.clone(), s.tls.clone());
        let kind = self.config.server(sid).and_then(|e| e.token_kind);
        let revoke = everywhere || kind == Some(TokenKind::Login);
        let agent = crate::omnichannel::AgentSignOut::of(&s.workspace);
        // Stop-typing calls of the open rooms; not chained before the
        // logout, which must not wait for a DDP call while the socket is down
        // (the server drops the activity with the connection anyway).
        let uid = self.config.server(sid).and_then(|e| e.user_id.clone());
        let typing = match self.servers.get_mut(sid) {
            Some(s) => {
                let t = s.close_rooms().discard();
                let e2e = crate::e2e::forget_secrets(s, self.secrets.as_ref(), uid.as_deref());
                s.reset();
                Task::batch([t, e2e])
            }
            None => Task::none(),
        };
        let call = match (revoke, handle, credentials) {
            (true, Some(h), _) => {
                let sid = sid.clone();
                Task::perform(
                    async move {
                        if agent.revoke(&h).await {
                            h.session().close().await;
                            return Ok(());
                        }
                        tokio::time::timeout(LOGOUT_TIMEOUT, h.session().logout())
                            .await
                            .map_err(|_| "timeout".to_owned())?
                            .map_err(|e| e.to_string())
                    },
                    move |r| Message::LoggedOut(sid.clone(), r),
                )
            }
            (true, None, Some(creds)) => {
                let sid = sid.clone();
                Task::perform(
                    async move {
                        let rest = RestClient::new(url, &tls).map_err(|e| e.to_string())?;
                        rest.set_credentials(Some(creds));
                        tokio::time::timeout(LOGOUT_TIMEOUT, rest.logout())
                            .await
                            .map_err(|_| "timeout".to_owned())?
                            .map_err(|e| e.to_string())
                    },
                    move |r| Message::LoggedOut(sid.clone(), r),
                )
            }
            (false, Some(h), _) => Task::perform(
                async move {
                    agent.keep_token(&h).await;
                    h.session().close().await;
                },
                |_| Message::Noop,
            ),
            _ => Task::none(),
        };
        let delete = self.delete_token(sid);
        let purge = self.purge_images(sid);
        let purge = Task::batch([purge, crate::cache::purge(self, sid)]);
        let page = self.leave_pages_of(sid);
        Task::batch([
            typing,
            call,
            delete,
            purge,
            page,
            self.show_login(sid, None),
        ])
    }

    pub(super) fn remove_server(&mut self, sid: &ServerId) -> Task<Message> {
        let t = self.logout(sid, false);
        self.servers.remove(sid);
        self.config.remove_server(sid);
        self.drop_token_confirmations(sid);
        self.active = self
            .config
            .active_server
            .clone()
            .filter(|id| self.servers.contains_key(id))
            .or_else(|| self.servers.keys().next().cloned());
        if self.servers.is_empty() {
            self.screen = Screen::AddServer(Box::new(AddServer::new(false)));
        }
        Task::batch([t, self.save_config()])
    }

    /// Shows server `sid` for a login that finishes from outside (browser
    /// redirect, token link). Unsaved admin settings are asked about first;
    /// if they are kept the login goes on behind the admin screen.
    pub(super) fn activate_for_login(&mut self, sid: &ServerId) -> Task<Message> {
        if self.may_leave(Message::Rail(rail::Message::Select(sid.clone()))) {
            self.activate(sid.clone())
        } else {
            Task::none()
        }
    }

    pub(super) fn complete_login(&mut self, sid: &ServerId, req: LoginRequest) -> Task<Message> {
        let a = self.activate_for_login(sid);
        let Some(l) = self.servers.get_mut(sid).and_then(|s| s.login.as_mut()) else {
            return a;
        };
        let sid = sid.clone();
        let t = l
            .complete_with(req)
            .map(move |m| Message::Login(sid.clone(), m));
        Task::batch([a, t])
    }

    /// The login with a server-generated SAML token finished: confirm the
    /// account like a Token link. A 2FA challenge goes through the regular
    /// login flow (only the account owner can answer it).
    pub(super) fn on_saml_link_login(
        &mut self,
        sid: ServerId,
        credential_token: String,
        r: crate::error::Res<LoginResult>,
    ) -> Task<Message> {
        let live = self
            .servers
            .get(&sid)
            .and_then(|s| s.login.as_ref())
            .is_some_and(|l| l.saml_claim_matches(&credential_token, Instant::now()));
        if !live {
            tracing::info!("SAML login finished without a matching pending login");
            return Task::none();
        }
        match r {
            Ok(res) => {
                let creds = Credentials::new(res.user_id.clone(), res.auth_token.clone());
                if let Some(l) = self.servers.get_mut(&sid).and_then(|s| s.login.as_mut()) {
                    l.claim_token(&creds);
                }
                match res.me {
                    Some(me) => self.on_token_user(sid, creds, Ok(me)),
                    None => self.apply_decision(
                        Decision::ConfirmToken {
                            server_id: sid,
                            user_id: creds.user_id.clone(),
                            token: creds.auth_token.clone(),
                        },
                        "",
                    ),
                }
            }
            Err(e) if matches!(e.error(), rocketchat::Error::TwoFactorRequired(_)) => {
                self.complete_login(&sid, LoginRequest::Saml { credential_token })
            }
            Err(e) => self.toast(
                BannerKind::Error,
                t!("deeplink.token_rejected"),
                e.user_text(),
            ),
        }
    }

    /// The `me` lookup of a Token link finished: ask for confirmation, but only
    /// while the browser login that accepted this exact link is still pending.
    pub(super) fn on_token_user(
        &mut self,
        sid: ServerId,
        creds: Credentials,
        r: crate::error::Res<User>,
    ) -> Task<Message> {
        let live = self
            .servers
            .get(&sid)
            .and_then(|s| s.login.as_ref())
            .is_some_and(|l| l.token_claim_matches(&creds, Instant::now()));
        if !live {
            tracing::info!("token lookup finished without a matching pending login");
            return Task::none();
        }
        let user = match r {
            Ok(u) if u.id == creds.user_id => u,
            Ok(_) => {
                return self.toast(
                    BannerKind::Error,
                    t!("deeplink.token_rejected"),
                    String::new(),
                );
            }
            Err(e) => {
                if e.error().is_transient()
                    && let Some(l) = self.servers.get_mut(&sid).and_then(|s| s.login.as_mut())
                {
                    // The same link may be delivered again.
                    l.release_token(&creds);
                }
                return self.toast(
                    BannerKind::Error,
                    t!("deeplink.token_rejected"),
                    e.user_text(),
                );
            }
        };
        self.drop_token_confirmations(&sid);
        self.confirm_queue.push_back((sid, creds, Box::new(user)));
        Task::none()
    }

    pub(super) fn open_register_handler(&mut self) -> Task<Message> {
        self.modals.push(Modal::RegisterHandler {
            current: None,
            loaded: false,
            busy: false,
            error: None,
            warnings: Vec::new(),
        });
        query_handler_task()
    }
}
