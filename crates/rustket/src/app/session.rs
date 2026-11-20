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
use rustket_core::e2e::private_key_secret;
use rustket_core::secrets::{SecretBackend, SecretStore, secret_key};
use rustket_core::workspace::ConnectionState;
use rustket_ui::widget::BannerKind;

use super::subscription::query_handler_task;
use super::{App, CONFIRM_ARM_DELAY, LOGOUT_TIMEOUT, Modal, Screen};
use crate::deeplink::Decision;
use crate::message::Message;
use crate::screens::add_server::{self, AddServer};
use crate::screens::login::{self, Login};
use crate::workspace::{ServerState, rail};

/// Failed deletes after which a purge stops (a blocked keyring makes every
/// further call wait for its timeout).
const MAX_PURGE_FAILURES: usize = 3;

/// The secrets a sign-out of `user_id` on `sid` deletes: the auth token and
/// the E2E private key.
fn account_secret_keys(sid: &ServerId, user_id: &str) -> [String; 2] {
    [secret_key(sid, user_id), private_key_secret(sid, user_id)]
}

/// Outcome of [`purge_secrets`].
#[derive(Debug, Default, PartialEq, Eq)]
struct SecretPurge {
    /// Keys that need no further delete.
    cleared: Vec<String>,
    /// The first failure.
    error: Option<String>,
}

/// Deletes `keys` from `store` (blocking). `durable`: the store is the
/// keyring, so a deleted key is gone for good; with a fallback store the
/// keyring may still hold a copy that is unreachable now, and the key stays
/// pending.
fn purge_secrets(store: &dyn SecretStore, keys: &[String], durable: bool) -> SecretPurge {
    let mut out = SecretPurge::default();
    let mut failures = 0;
    for key in keys {
        match store.delete(key) {
            Ok(()) if durable => out.cleared.push(key.clone()),
            Ok(()) => {}
            Err(e) => {
                tracing::warn!(error = %e, "cannot delete a secret of a signed-out account");
                out.error.get_or_insert_with(|| e.to_string());
                failures += 1;
                if failures >= MAX_PURGE_FAILURES {
                    break;
                }
            }
        }
    }
    out
}

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
            login::Out::Unconfirmed(r) => self.on_link_login(sid, *r),
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
        // The secrets of this account are in use again; the token of the
        // account it replaces is not.
        self.config
            .clear_secret_deletions(&account_secret_keys(sid, &r.user_id));
        if let Some(old) = old_uid.as_deref().filter(|u| *u != r.user_id) {
            self.config.mark_secret_deletion(&secret_key(sid, old));
        }
        let purge = self.purge_pending_secrets();
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
        let token = r.auth_token.clone();
        let sid2 = sid.clone();
        let store = self
            .secret_task(move |st| st.set(&key, &token).map_err(|e| e.to_string()))
            .map(move |r| Message::TokenStored(sid2.clone(), r));
        Task::batch([purge, store, self.save_config()])
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

    /// Signs the account of `sid` out of the secret store. Its keys are first
    /// recorded in the config, so the sign-out holds when the store cannot
    /// delete them now: a recorded key is never restored as a login, and its
    /// delete is retried on every start and sign-out.
    fn delete_token(&mut self, sid: &ServerId) -> Task<Message> {
        let Some(uid) = self.config.server(sid).and_then(|e| e.user_id.clone()) else {
            return Task::none();
        };
        for key in account_secret_keys(sid, &uid) {
            self.config.mark_secret_deletion(&key);
        }
        Task::batch([self.save_config(), self.purge_pending_secrets()])
    }

    /// Deletes the secrets of signed-out accounts
    /// (`AppConfig::pending_secret_deletions`) from the secret store.
    pub(super) fn purge_pending_secrets(&self) -> Task<Message> {
        let keys = self.config.pending_secret_deletions.clone();
        let Some(store) = self.secrets.clone() else {
            return Task::none();
        };
        if keys.is_empty() {
            return Task::none();
        }
        let durable = self.secret_backend == Some(SecretBackend::Keyring);
        Task::perform(
            async move {
                tokio::task::spawn_blocking(move || purge_secrets(store.as_ref(), &keys, durable))
                    .await
                    .unwrap_or_else(|e| SecretPurge {
                        cleared: Vec::new(),
                        error: Some(e.to_string()),
                    })
            },
            |p| Message::SecretsPurged(p.cleared, p.error),
        )
    }

    /// [`Self::purge_pending_secrets`] finished.
    pub(super) fn on_secrets_purged(
        &mut self,
        cleared: &[String],
        error: Option<String>,
    ) -> Task<Message> {
        let mut tasks = Vec::new();
        if self.config.clear_secret_deletions(cleared) {
            tasks.push(self.save_config());
        }
        if let Some(e) = error {
            tasks.push(self.toast(BannerKind::Warning, t!("app.secrets_not_deleted"), e));
        }
        Task::batch(tasks)
    }

    /// The server rejected the token: drop it and show the login screen.
    pub(super) fn logged_out(&mut self, sid: &ServerId, reason: Option<String>) -> Task<Message> {
        self.large_rooms.forget(sid);
        let mut e2e = Task::none();
        let mut scheduled = 0;
        if let Some(s) = self.servers.get_mut(sid) {
            // Scheduled messages live in the offline cache purged below.
            scheduled = s.workspace.scheduled().len();
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
        // A toast goes by unseen while the app runs unattended: the count
        // also stays on the login screen until the user signs in again.
        let mut login_reason = reason;
        let discarded = match self.servers.get(sid) {
            Some(s) if scheduled > 0 => {
                let body = t!(
                    "scheduled.discarded_body",
                    server = s.name.clone(),
                    count = scheduled
                )
                .into_owned();
                login_reason = Some(match login_reason {
                    Some(r) => format!("{r} {body}"),
                    None => body.clone(),
                });
                self.toast(BannerKind::Warning, t!("scheduled.discarded_title"), body)
            }
            _ => Task::none(),
        };
        Task::batch([
            e2e,
            delete,
            purge,
            page,
            notice,
            discarded,
            self.show_login(sid, login_reason),
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

    /// A login with a link-supplied SAML token succeeded (directly, or after
    /// its 2FA round): the account is confirmed like the one of a Token link,
    /// while the pending login that accepted the link is still live.
    fn on_link_login(&mut self, sid: ServerId, res: LoginResult) -> Task<Message> {
        let now = Instant::now();
        let Some(l) = self.servers.get_mut(&sid).and_then(|s| s.login.as_mut()) else {
            return Task::none();
        };
        let live = l
            .pending_login()
            .is_some_and(|p| !p.is_expired(now) && p.saml_claim.is_some());
        if !live {
            tracing::info!("SAML login finished without a matching pending login");
            return Task::none();
        }
        let creds = Credentials::new(res.user_id.clone(), res.auth_token.clone());
        l.claim_token(&creds);
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

    /// Runs the login with a link-supplied SAML token again for its 2FA
    /// round; the result comes back through [`Self::on_link_login`].
    fn complete_link_login(&mut self, sid: &ServerId, req: LoginRequest) -> Task<Message> {
        let a = self.activate_for_login(sid);
        let Some(l) = self.servers.get_mut(sid).and_then(|s| s.login.as_mut()) else {
            return a;
        };
        let sid = sid.clone();
        let t = l
            .complete_unconfirmed(req)
            .map(move |m| Message::Login(sid.clone(), m));
        Task::batch([a, t])
    }

    /// The login with a server-generated SAML token finished: confirm the
    /// account like a Token link. A 2FA challenge is answered in the regular
    /// dialog, but its result needs the same confirmation: the link is
    /// unauthenticated and the dialog names no account.
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
            Ok(res) => self.on_link_login(sid, res),
            Err(e) if matches!(e.error(), rocketchat::Error::TwoFactorRequired(_)) => {
                self.complete_link_login(&sid, LoginRequest::Saml { credential_token })
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

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use rocketchat::models::LoginServiceConfig;
    use rustket_core::config::AppConfig;
    use rustket_core::secrets::{MemoryStore, SecretError, SwitchableStore};

    use super::*;
    use crate::app::{Flags, SecretsMode};
    use crate::error::Failure;
    use crate::message::{ModalMessage, SecretsReady};
    use crate::screens::login::LoginMeta;

    const SERVER: &str = "http://localhost:3000";

    /// One configured server, started with a store reported as `backend`.
    /// With `uid` the store holds that account's secrets; `signed_out`
    /// records them as pending deletions first.
    fn started(
        dir: &std::path::Path,
        uid: Option<&str>,
        signed_out: bool,
        backend: SecretBackend,
    ) -> (App, ServerId, Arc<MemoryStore>) {
        let mut cfg = AppConfig::default();
        let mut entry = ServerEntry::new(SERVER, "Local");
        entry.user_id = uid.map(str::to_owned);
        let sid = cfg.add_server(entry);
        let store = Arc::new(MemoryStore::new());
        let mut tokens = Vec::new();
        if let Some(uid) = uid {
            for key in account_secret_keys(&sid, uid) {
                store.set(&key, "stale").unwrap();
                if signed_out {
                    cfg.mark_secret_deletion(&key);
                }
            }
            tokens.push((sid.clone(), Some("stale".to_owned())));
        }
        cfg.save(dir).unwrap();
        let (mut app, _task) = App::new(Flags {
            config_dir: Some(dir.to_owned()),
            cache_dir: Some(dir.join("cache")),
            secrets: SecretsMode::Memory,
            ..Flags::default()
        });
        let _ = app.update(Message::SecretsReady(SecretsReady {
            store: Arc::new(SwitchableStore::new(Box::new(store.clone()), backend, None)),
            tokens,
            notices: vec![],
        }));
        (app, sid, store)
    }

    fn signed_in(app: &App, sid: &ServerId) -> Option<String> {
        app.servers[sid]
            .credentials
            .as_ref()
            .map(|c| c.user_id.clone())
    }

    fn login_result(uid: &str, token: &str) -> LoginResult {
        LoginResult {
            user_id: uid.into(),
            auth_token: token.into(),
            me: Some(User {
                id: uid.into(),
                username: Some(uid.to_lowercase()),
                ..Default::default()
            }),
        }
    }

    // --- F13 ---------------------------------------------------------------------

    fn totp_required() -> Failure {
        let challenge = rocketchat::TwoFactorChallenge::from_details(Some(
            &serde_json::json!({"method": "totp"}),
        ));
        Failure::from(rocketchat::Error::TwoFactorRequired(challenge))
    }

    fn saml_link(token: &str) -> String {
        format!("rocketchat://auth?host={SERVER}&type=saml&credentialToken={token}")
    }

    /// A pending SAML login that received a link with the attacker's token,
    /// for which the server asked for 2FA; the code was submitted.
    fn saml_link_two_factor(dir: &std::path::Path) -> (App, ServerId) {
        let (mut app, sid, _) = started(dir, None, false, SecretBackend::Memory);
        let l = app
            .servers
            .get_mut(&sid)
            .and_then(|s| s.login.as_mut())
            .unwrap();
        l.meta = Some(LoginMeta {
            site_name: "x".into(),
            show_form: true,
            services: vec![LoginServiceConfig {
                service: Some("saml".into()),
                ..Default::default()
            }],
            ..Default::default()
        });
        let _ = app.update(Message::Login(sid.clone(), login::Message::Service(0)));
        let _ = app.update(Message::DeepLinks(vec![saml_link("ATTACKER")]));
        let _ = app.update(Message::SamlLinkLogin(
            sid.clone(),
            "ATTACKER".into(),
            Err(totp_required()),
        ));
        // The login runs again through the login screen, for its 2FA dialog.
        let _ = app.update(Message::Login(
            sid.clone(),
            login::Message::LinkLoginDone(Err(totp_required())),
        ));
        let l = app.servers[&sid].login.as_ref().unwrap();
        assert!(l.two_factor.as_ref().is_some_and(|tf| tf.unconfirmed));
        assert!(l.pending.is_some());
        let _ = app.update(Message::Login(
            sid.clone(),
            login::Message::Code("123456".into()),
        ));
        let _ = app.update(Message::Login(sid.clone(), login::Message::SubmitCode));
        (app, sid)
    }

    fn link_login_done(app: &mut App, sid: &ServerId) {
        let _ = app.update(Message::Login(
            sid.clone(),
            login::Message::LinkLoginDone(Ok(login_result("EVE", "T"))),
        ));
    }

    /// F13: the 2FA round of a link-supplied SAML token ends in the account
    /// confirmation, not in a session.
    #[tokio::test]
    async fn saml_link_login_needs_confirmation_after_two_factor() {
        let dir = tempfile::tempdir().unwrap();
        let (mut app, sid) = saml_link_two_factor(dir.path());
        link_login_done(&mut app, &sid);
        assert!(signed_in(&app, &sid).is_none(), "2FA alone never signs in");
        let Some(Modal::ConfirmToken {
            credentials, stamp, ..
        }) = app.modals.last()
        else {
            panic!("the account confirmation is shown");
        };
        assert_eq!(credentials.user_id, "EVE");
        let stamp = *stamp;
        // Not armed yet: clicking does nothing.
        let _ = app.update(Message::Modal(ModalMessage::ConfirmToken));
        assert!(signed_in(&app, &sid).is_none());
        let _ = app.update(Message::ArmConfirm(stamp));
        let _ = app.update(Message::Modal(ModalMessage::ConfirmToken));
        assert_eq!(signed_in(&app, &sid).as_deref(), Some("EVE"));
    }

    /// F13: without the pending login that accepted the link (a second link
    /// cancelled it), the 2FA result is dropped.
    #[tokio::test]
    async fn saml_link_login_is_dropped_without_its_pending_login() {
        let dir = tempfile::tempdir().unwrap();
        let (mut app, sid) = saml_link_two_factor(dir.path());
        let _ = app.update(Message::DeepLinks(vec![saml_link("ATTACKER2")]));
        assert!(app.servers[&sid].login.as_ref().unwrap().pending.is_none());
        link_login_done(&mut app, &sid);
        assert!(signed_in(&app, &sid).is_none());
        assert!(
            !app.modals
                .iter()
                .any(|m| matches!(m, Modal::ConfirmToken { .. }))
        );
    }

    // --- F17 ---------------------------------------------------------------------

    struct FailingStore {
        deletes: AtomicUsize,
    }

    impl SecretStore for FailingStore {
        fn get(&self, _: &str) -> Result<Option<String>, SecretError> {
            Ok(None)
        }
        fn set(&self, _: &str, _: &str) -> Result<(), SecretError> {
            Ok(())
        }
        fn delete(&self, _: &str) -> Result<(), SecretError> {
            self.deletes.fetch_add(1, Ordering::SeqCst);
            Err(SecretError::Keyring("locked".into()))
        }
    }

    #[test]
    fn failed_deletes_are_reported_and_clear_nothing() {
        let store = FailingStore {
            deletes: AtomicUsize::new(0),
        };
        let keys: Vec<String> = (0..10).map(|i| format!("k{i}")).collect();
        let p = purge_secrets(&store, &keys, true);
        assert!(p.cleared.is_empty());
        assert!(p.error.is_some());
        assert_eq!(store.deletes.load(Ordering::SeqCst), MAX_PURGE_FAILURES);
    }

    /// F17: a token still in the store for an account that signed out (the
    /// store was unreachable then, or the delete failed) is not a login.
    #[tokio::test]
    async fn signed_out_account_is_not_restored_from_a_stale_token() {
        let dir = tempfile::tempdir().unwrap();
        let (app, sid, _) = started(dir.path(), Some("U1"), true, SecretBackend::Keyring);
        assert!(signed_in(&app, &sid).is_none());
        assert!(app.servers[&sid].login.is_some());
        // Without the record the same start restores the session.
        let dir = tempfile::tempdir().unwrap();
        let (app, sid, _) = started(dir.path(), Some("U1"), false, SecretBackend::Keyring);
        assert_eq!(signed_in(&app, &sid).as_deref(), Some("U1"));
    }

    /// F17: a sign-out stays recorded until the keyring deleted the secrets.
    #[tokio::test]
    async fn sign_out_is_recorded_until_the_keyring_deleted_the_secrets() {
        let dir = tempfile::tempdir().unwrap();
        let (mut app, sid, store) = started(dir.path(), Some("U1"), false, SecretBackend::File);
        let keys = account_secret_keys(&sid, "U1").to_vec();
        let _ = app.logout(&sid, false);
        assert_eq!(app.config.pending_secret_deletions, keys);
        // A failed delete keeps the record.
        let _ = app.on_secrets_purged(&[], Some("locked".into()));
        assert_eq!(app.config.pending_secret_deletions, keys);
        // A fallback store deletes its copy; the keyring may hold another.
        let p = purge_secrets(&*store, &keys, false);
        assert_eq!(p, SecretPurge::default());
        assert!(store.get(&keys[0]).unwrap().is_none());
        let _ = app.on_secrets_purged(&p.cleared, p.error);
        assert_eq!(app.config.pending_secret_deletions, keys);
        let p = purge_secrets(&*store, &keys, true);
        assert_eq!(p.cleared, keys);
        let _ = app.on_secrets_purged(&p.cleared, p.error);
        assert!(app.config.pending_secret_deletions.is_empty());
    }

    /// F17: a new login takes its own secrets off the record and puts the
    /// token of the account it replaces on it.
    #[tokio::test]
    async fn login_updates_the_pending_deletions() {
        let dir = tempfile::tempdir().unwrap();
        let (mut app, sid, _) = started(dir.path(), Some("U1"), false, SecretBackend::Memory);
        let _ = app.logout(&sid, false);
        let _ = app.logged_in(&sid, login_result("U2", "T2"), TokenKind::Login);
        assert_eq!(
            app.config.pending_secret_deletions,
            account_secret_keys(&sid, "U1")
        );
        let _ = app.logged_in(&sid, login_result("U1", "T1"), TokenKind::Login);
        assert_eq!(
            app.config.pending_secret_deletions,
            [secret_key(&sid, "U2")]
        );
    }
}
