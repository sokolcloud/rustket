//! URLs and deep links: opening URLs, room permalinks and `rocketchat://` links.

use std::time::Instant;

use iced::Task;
use rocketchat::auth::parse_deep_link;
use rocketchat::rest::LoginRequest;
use rocketchat::{Credentials, RestClient, ServerUrl};
use rust_i18n::t;
use rustket_core::ServerId;
use rustket_ui::widget::BannerKind;

use super::{App, Modal, ROOM_LINK_TTL};
use crate::deeplink::{self, Decision};
use crate::error::{Failure, Res};
use crate::message::Message;
use crate::room::UpdateCtx;
use crate::screens::directory::open;

impl App {
    /// Opens an absolute `http`/`https`/`mailto` URL in the browser; anything
    /// else (other schemes, paths) is refused.
    pub(crate) fn open_url(&mut self, url: String) -> Task<Message> {
        let Some(url) = rustket_core::uikit::openable_url(&url).map(str::to_owned) else {
            tracing::warn!("refusing to open a non-http URL");
            return self.toast(BannerKind::Error, t!("app.url_refused"), String::new());
        };
        if !self.open_urls {
            tracing::info!("not opening browser (disabled)");
            return Task::none();
        }
        Task::perform(
            async move {
                tokio::task::spawn_blocking(move || {
                    webbrowser::open(&url).map_err(|e| e.to_string())
                })
                .await
                .unwrap_or_else(|e| Err(e.to_string()))
            },
            |r| {
                if let Err(e) = r {
                    tracing::warn!(error = %e, "cannot open browser");
                }
                Message::Noop
            },
        )
    }

    /// A URL clicked in a room: permalinks to messages of this server open
    /// in the app (a channel the user has not joined as a preview, like the
    /// directory does), everything else in the browser, as does a permalink
    /// whose room the server does not show.
    pub(super) fn open_room_url(&mut self, sid: &ServerId, url: String) -> Task<Message> {
        let Some(s) = self.servers.get(sid) else {
            return self.open_url(url);
        };
        let mut bases = vec![s.url.join("")];
        if let Some(site) = s.workspace.settings().str("Site_Url") {
            bases.push(site.trim().to_owned());
        }
        let Some(path) = deeplink::permalink_path(&url, &bases) else {
            return self.open_url(url);
        };
        let local = deeplink::room_name_from_path(&path)
            .and_then(|(_, name)| find_room(&s.workspace, &deeplink::percent_decode(name)));
        match local {
            Some(rid) => self.open_room_link(sid, Some(rid), Some(path)),
            None => match self.resolve_room_link(sid, &path, Some(url.clone())) {
                Some(task) => task,
                None => self.open_url(url),
            },
        }
    }

    /// Fetches the room of link `path` that the workspace does not have (by
    /// name; a channel or private group the server lets the user see) to
    /// open it as a preview. `None` when the path names no such room or the
    /// session is down.
    fn resolve_room_link(
        &self,
        sid: &ServerId,
        path: &str,
        fallback: Option<String>,
    ) -> Option<Task<Message>> {
        let (kind, name) = deeplink::room_name_from_path(path)?;
        if !matches!(kind, "channel" | "group") {
            return None;
        }
        let rest = self.servers.get(sid)?.session.as_ref()?.rest().clone();
        let target = open::Target::Name(deeplink::percent_decode(name));
        let mid = deeplink::message_id_from_path(path);
        let sid = sid.clone();
        Some(Task::perform(open::resolve(rest, target), move |r| {
            Message::RoomLinkResolved(
                sid.clone(),
                mid.clone(),
                fallback.clone(),
                Box::new(r.map_err(Failure::from)),
            )
        }))
    }

    // --- deep links ------------------------------------------------------------

    pub(super) fn handle_link(&mut self, url: &str) -> Task<Message> {
        let Some(link) = parse_deep_link(url) else {
            tracing::info!("ignoring unrecognised argument");
            return Task::none();
        };
        let servers: Vec<(ServerId, ServerUrl)> = self
            .servers
            .iter()
            .map(|(id, s)| (id.clone(), s.url.clone()))
            .collect();
        let decision = deeplink::decide(&link, &self.pending_logins(), &servers, Instant::now());
        tracing::info!(?decision, "deep link");
        self.apply_decision(decision, url)
    }

    pub(super) fn apply_decision(&mut self, decision: Decision, url: &str) -> Task<Message> {
        match decision {
            Decision::LoginOAuth {
                server_id,
                credential_token,
                credential_secret,
            } => self.complete_login(
                &server_id,
                LoginRequest::OAuth {
                    credential_token,
                    credential_secret,
                },
            ),
            Decision::LoginSaml {
                server_id,
                credential_token,
            } => self.complete_login(&server_id, LoginRequest::Saml { credential_token }),
            Decision::ConfirmSaml {
                server_id,
                credential_token,
            } => {
                let Some(s) = self.servers.get_mut(&server_id) else {
                    return Task::none();
                };
                let Some(l) = s.login.as_mut() else {
                    return Task::none();
                };
                // The pending login accepts this one link only.
                l.claim_saml(&credential_token);
                let (url, tls) = (s.url.clone(), s.tls.clone());
                let token = credential_token.clone();
                Task::perform(
                    async move {
                        let rest = RestClient::new(url, &tls)?;
                        rest.login_saml(&token, None).await
                    },
                    move |r| {
                        Message::SamlLinkLogin(
                            server_id.clone(),
                            credential_token.clone(),
                            r.map_err(Failure::from),
                        )
                    },
                )
            }
            Decision::ConfirmToken {
                server_id,
                user_id,
                token,
            } => {
                let creds = Credentials::new(user_id, token);
                let Some(s) = self.servers.get_mut(&server_id) else {
                    return Task::none();
                };
                let Some(l) = s.login.as_mut() else {
                    return Task::none();
                };
                // The pending login accepts this one link only.
                l.claim_token(&creds);
                let (url, tls) = (s.url.clone(), s.tls.clone());
                let c = creds.clone();
                Task::perform(
                    async move {
                        let rest = RestClient::new(url, &tls)?;
                        rest.set_credentials(Some(c));
                        rest.me().await
                    },
                    move |r| {
                        Message::TokenUser(
                            server_id.clone(),
                            creds.clone(),
                            r.map_err(Failure::from),
                        )
                    },
                )
            }
            Decision::DuplicateToken { server_id } => {
                // A second account link for one browser login: someone else
                // is sending links. Start over.
                self.drop_token_confirmations(&server_id);
                if let Some(l) = self
                    .servers
                    .get_mut(&server_id)
                    .and_then(|s| s.login.as_mut())
                {
                    l.cancel_pending();
                    l.error = Some(t!("deeplink.duplicate_token").into_owned());
                }
                Task::none()
            }
            Decision::AuthError { server_id, error } => {
                if let Some(l) = self
                    .servers
                    .get_mut(&server_id)
                    .and_then(|s| s.login.as_mut())
                {
                    l.show_link_error(&error);
                }
                Task::none()
            }
            Decision::OpenRoom { .. } | Decision::OpenInvite { .. }
                if !self.may_leave(Message::DeepLinks(vec![url.to_owned()])) =>
            {
                Task::none()
            }
            Decision::OpenRoom {
                server_id,
                rid,
                path,
            } => {
                let a = self.activate_for(server_id.clone(), false);
                Task::batch([a, self.open_room_link(&server_id, rid, path)])
            }
            Decision::OpenInvite { server_id, .. } => {
                let a = self.activate(server_id);
                Task::batch([
                    a,
                    self.toast(BannerKind::Info, t!("app.not_yet"), String::new()),
                ])
            }
            Decision::OfferAddServer { url } => {
                if self
                    .modals
                    .iter()
                    .any(|m| matches!(m, Modal::OfferAddServer { .. }))
                {
                    return Task::none();
                }
                self.modals.push(Modal::OfferAddServer { url });
                Task::none()
            }
            Decision::OtherOrigin { server_id, url } => {
                let name = self
                    .servers
                    .get(&server_id)
                    .map(|s| s.url.as_str().to_owned())
                    .unwrap_or_default();
                self.toast(
                    BannerKind::Warning,
                    t!("deeplink.other_origin_title"),
                    t!(
                        "deeplink.other_origin_body",
                        link = url.as_str(),
                        server = name
                    ),
                )
            }
            Decision::Ignore(reason) => {
                tracing::info!(reason, "deep link ignored");
                Task::none()
            }
        }
    }

    /// Opens the room of a link, or queues it until the server has loaded.
    pub(super) fn open_room_link(
        &mut self,
        sid: &ServerId,
        rid: Option<String>,
        path: Option<String>,
    ) -> Task<Message> {
        let stamp = self.next_stamp();
        let Some(s) = self.servers.get_mut(sid) else {
            return Task::none();
        };
        if s.workspace.last_sync().is_none() {
            s.pending_room = Some((rid, path, stamp));
            let sid = sid.clone();
            return Task::perform(tokio::time::sleep(ROOM_LINK_TTL), move |_| {
                Message::RoomLinkExpired(sid.clone(), stamp)
            });
        }
        let target = rid.filter(|r| s.workspace.room(r).is_some()).or_else(|| {
            let (_, name) = deeplink::room_name_from_path(path.as_deref()?)?;
            find_room(&s.workspace, &deeplink::percent_decode(name))
        });
        match target {
            Some(rid) => {
                let mid = path.as_deref().and_then(deeplink::message_id_from_path);
                self.open_room_at(sid, &rid, mid.as_deref())
            }
            None => match path
                .as_deref()
                .and_then(|p| self.resolve_room_link(sid, p, None))
            {
                Some(task) => task,
                None => self.toast(
                    BannerKind::Warning,
                    t!("deeplink.room_not_found"),
                    String::new(),
                ),
            },
        }
    }

    /// Opens room `rid` and shows message `mid` in it, if given.
    fn open_room_at(&mut self, sid: &ServerId, rid: &str, mid: Option<&str>) -> Task<Message> {
        let open = self.open_room(sid, rid);
        let focus = match mid {
            Some(mid) => self.focus_message(sid, rid, mid),
            None => Task::none(),
        };
        Task::batch([open, focus])
    }

    /// The room of a link was fetched: put it into the workspace (a preview
    /// when not joined) and open it.
    pub(super) fn room_link_resolved(
        &mut self,
        sid: &ServerId,
        mid: Option<String>,
        fallback: Option<String>,
        r: Res<open::Opened>,
    ) -> Task<Message> {
        match (r, self.servers.get_mut(sid)) {
            (Ok(o), Some(s)) => {
                let rid = open::apply(&mut s.workspace, o);
                self.open_room_at(sid, &rid, mid.as_deref())
            }
            (Ok(_), None) => Task::none(),
            (Err(e), _) => {
                tracing::debug!(server = %sid, error = %e, "room of a link not resolved");
                match fallback {
                    Some(url) => self.open_url(url),
                    None => self.toast(
                        BannerKind::Warning,
                        t!("deeplink.room_not_found"),
                        String::new(),
                    ),
                }
            }
        }
    }

    /// Shows message `mid` in the open view of `rid` (permalinks).
    fn focus_message(&mut self, sid: &ServerId, rid: &str, mid: &str) -> Task<Message> {
        let Some(s) = self.servers.get_mut(sid) else {
            return Task::none();
        };
        let epoch = s.generation;
        let Some(rv) = s.rooms.get_mut(rid) else {
            return Task::none();
        };
        let mut ctx = UpdateCtx {
            server_id: &s.id,
            server: &s.url,
            rid,
            workspace: &mut s.workspace,
            session: s.session.as_ref(),
            images: &self.images,
            prefs: &self.config.ui,
        };
        let action = rv.focus_message(mid, &mut ctx);
        Task::batch([
            Self::room_task(sid, epoch, rid, action.task),
            self.handle_effects(sid, rid, action.effects),
        ])
    }
}

/// Room of a link segment: a room id, or a room name / display name.
fn find_room(ws: &rustket_core::Workspace, name: &str) -> Option<String> {
    if ws.room(name).is_some() {
        return Some(name.to_owned());
    }
    ws.rooms()
        .values()
        .find(|e| e.name() == name || e.fname() == Some(name))
        .map(|e| e.rid().to_owned())
}
