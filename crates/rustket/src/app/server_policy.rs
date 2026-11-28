//! The server keys of the enterprise policy in the app (docs/policy.md
//! sections 9.1 and 9.2): which configured servers run, the reconcile of
//! `ManagedServers` at a refresh, suspending and resuming a server, the
//! notice when a signed-in server keeps pins that left effect, the guards of
//! adding and removing, and the plain policy screens. The standing of a server is
//! decided in `crate::policy::servers` only.

use iced::Task;
use rocketchat::{Credentials, ServerUrl};
use rust_i18n::t;
use rustket_core::ServerId;
use rustket_core::secrets::secret_key;
use rustket_core::workspace::ConnectionState;
use rustket_ui::widget::BannerKind;

use super::{App, Modal, Screen};
use crate::component::Nav;
use crate::message::Message;
use crate::policy::servers::{self, Standing, Suspension};
use crate::screens::add_server::AddServer;
use crate::screens::policy_notice;
use crate::workspace::ServerState;

impl App {
    /// The standing of configured server `sid` under the policy in force.
    pub fn standing_of(&self, sid: &ServerId) -> Option<Standing> {
        self.servers
            .get(sid)
            .map(|s| servers::standing(&self.policy, &s.url))
    }

    /// Whether the policy suspends configured server `sid`.
    pub fn is_suspended(&self, sid: &ServerId) -> bool {
        self.standing_of(sid).is_some_and(|s| !s.is_allowed())
    }

    /// Whether the user may add servers (`AllowUserServers`).
    pub fn may_add_servers(&self) -> bool {
        self.policy.allow_user_servers()
    }

    /// Whether the user may remove `sid`: adding and removing are allowed
    /// and it is not a managed server.
    pub fn may_remove_server(&self, sid: &ServerId) -> bool {
        self.may_add_servers()
            && self
                .servers
                .get(sid)
                .is_some_and(|s| !self.policy.is_managed_server(&s.url))
    }

    /// What is shown when no server is configured: the add-server screen, or
    /// the policy's plain screen while the user may not add one.
    pub(super) fn fallback_screen(&self) -> Screen {
        if self.may_add_servers() {
            Screen::AddServer(Box::new(AddServer::new(false)))
        } else {
            Screen::NoServers
        }
    }

    /// The screen for the active server or, without servers, the fallback.
    pub(super) fn workspace_screen(&self) -> Screen {
        if self.servers.is_empty() {
            self.fallback_screen()
        } else {
            Screen::Server
        }
    }

    /// The refusal text of a server the policy does not allow (add-server
    /// screen, links).
    pub(super) fn refusal_text(reason: Suspension) -> String {
        match reason {
            Suspension::NotAllowed => t!("policy.add_not_allowed").into_owned(),
            Suspension::HttpsRequired => t!("policy.add_https_required").into_owned(),
            Suspension::Blocked => t!("policy.server_blocked").into_owned(),
        }
    }

    /// Why a new server `url` may not be added now, if it may not: adding
    /// is off, or the policy does not allow it.
    pub(super) fn add_refusal(&self, url: &ServerUrl) -> Option<String> {
        if !self.may_add_servers() {
            return Some(t!("policy.adding_not_allowed").into_owned());
        }
        servers::refusal(&self.policy, url).map(Self::refusal_text)
    }

    /// A `rocketchat://` link to a server that is not configured, refused
    /// by the policy: a toast naming the reason.
    pub(super) fn refuse_link(&mut self, reason: String) -> Task<Message> {
        tracing::info!("deep link refused by the policy");
        self.toast(BannerKind::Warning, t!("policy.link_refused"), reason)
    }

    /// The tokens the start-up reads from the secret store: of every
    /// account except those of servers the policy suspends.
    pub fn startup_token_reads(&self) -> Vec<(ServerId, String)> {
        self.config
            .servers
            .iter()
            .filter(|s| !self.is_suspended(&s.id))
            .filter_map(|s| Some((s.id.clone(), s.user_id.clone()?)))
            .collect()
    }

    /// Picks the server shown at start: the stored one, unless it is
    /// suspended and another one is allowed.
    pub(super) fn initial_active(&self, stored: Option<ServerId>) -> Option<ServerId> {
        let allowed = |id: &ServerId| !self.is_suspended(id);
        let sorted: Vec<ServerId> = self
            .config
            .sorted_servers()
            .into_iter()
            .map(|e| e.id.clone())
            .filter(|id| self.servers.contains_key(id))
            .collect();
        let stored = stored.filter(|id| self.servers.contains_key(id));
        match stored {
            Some(id) if allowed(&id) => Some(id),
            Some(id) => sorted.iter().find(|i| allowed(i)).cloned().or(Some(id)),
            None => sorted
                .iter()
                .find(|i| allowed(i))
                .or(sorted.first())
                .cloned(),
        }
    }

    /// Sections 9.1 and 9.2 at a refresh (`set_policy`): managed servers the
    /// config lacks are added, servers the policy suspends now are stopped,
    /// servers it allows again start as at start-up. The certificate rule is
    /// recorded for every server; a signed-in one keeps its session and the
    /// pins it started with, and a notice says when they left effect.
    /// Nothing of a server is deleted in either direction.
    pub(super) fn apply_server_policy(&mut self) -> Task<Message> {
        let mut tasks = Vec::new();
        let added = servers::reconcile(&self.policy, &mut self.config);
        for sid in &added {
            let Some(entry) = self.config.server(sid) else {
                continue;
            };
            match ServerState::from_entry(entry, &self.policy) {
                Ok(mut s) => {
                    // Started below once the accounts run; before that the
                    // start-up starts it with the others.
                    s.restoring = !self.booted;
                    self.servers.insert(sid.clone(), s);
                }
                Err(e) => tracing::warn!(error = %e, "skipping a managed server"),
            }
        }
        if !added.is_empty() {
            tasks.push(self.save_config());
        }
        let sids: Vec<ServerId> = self.servers.keys().cloned().collect();
        let mut pins_later = Vec::new();
        for sid in sids {
            let suspended = self.is_suspended(&sid);
            let policy = self.policy.clone();
            let Some(s) = self.servers.get_mut(&sid) else {
                continue;
            };
            // Never touches a session that exists or is starting
            // (`ServerState::set_pins_allowed`).
            let kept_pins = s.set_pins_allowed(servers::pins_allowed(&policy, &s.url));
            let running = s.credentials.is_some() || s.login.is_some() || s.restoring;
            if suspended && running {
                tracing::info!(server = %sid, "server suspended by the policy");
                tasks.push(self.suspend_server(&sid));
            } else if kept_pins {
                // After a failed start or an ended session Retry starts a
                // new session under the rule in force.
                let retry_applies = s.start_error.is_some()
                    || (s.session.is_none()
                        && *s.workspace.connection() == ConnectionState::Disconnected);
                if !retry_applies {
                    tracing::info!(server = %sid, "certificate pins left effect: next start");
                    pins_later.push(sid.clone());
                }
            } else if !suspended && !running && self.booted && self.terms_accepted() {
                tasks.push(self.start_server(&sid));
            }
        }
        // One notice for all of them, in the user's order: the app keeps
        // only a few toasts.
        if !pins_later.is_empty() {
            let names: Vec<String> = self
                .config
                .sorted_servers()
                .into_iter()
                .filter(|e| pins_later.contains(&e.id))
                .filter_map(|e| self.servers.get(&e.id).map(|s| s.name.clone()))
                .collect();
            tasks.push(self.toast(
                BannerKind::Warning,
                t!("policy.pins_later", servers = names.join(", ")),
                String::new(),
            ));
        }
        // Dialogs and screens the new policy refuses go.
        let removable: Vec<ServerId> = self
            .servers
            .keys()
            .filter(|sid| self.may_remove_server(sid))
            .cloned()
            .collect();
        self.modals
            .retain(|m| !matches!(m, Modal::ConfirmRemove(sid) if !removable.contains(sid)));
        if !self.may_add_servers() && matches!(self.screen, Screen::AddServer(_)) {
            self.screen = self.workspace_screen();
        }
        if matches!(self.screen, Screen::NoServers) && self.may_add_servers() {
            self.screen = self.workspace_screen();
        }
        if matches!(self.screen, Screen::NoServers) && !self.servers.is_empty() {
            self.screen = Screen::Server;
        }
        if self.active.is_none() && self.booted {
            self.active = self.initial_active(self.config.active_server.clone());
            if self.active.is_some() && matches!(self.screen, Screen::NoServers) {
                self.screen = Screen::Server;
            }
        }
        Task::batch(tasks)
    }

    /// Stops running server `sid` for the policy (section 9.1): its session
    /// is closed without signing out on the server; its token, cache,
    /// config entry and drafts stay. What a sign-out does, minus the
    /// server-side logout and the deletions. Queued sends and image
    /// requests stop; running uploads are cancelled as at a sign-out and
    /// named in a warning at once, a download already running may finish.
    pub(super) fn suspend_server(&mut self, sid: &ServerId) -> Task<Message> {
        self.large_rooms.forget(sid);
        self.drop_token_confirmations(sid);
        // Images still queued for its host are not fetched; the cache stays.
        self.images.forget_requests(sid);
        let Some(s) = self.servers.get_mut(sid) else {
            return Task::none();
        };
        let handle = s.session.clone();
        // Cancelled by the reset below; named now, not as "closed while
        // uploading" at the next start.
        let uploads = crate::room::upload::running_names(sid.as_str());
        // Composer texts become drafts of the workspace, kept below.
        let typing = s.close_rooms().discard();
        let drafts: Vec<(String, Option<String>, String)> = s
            .workspace
            .drafts()
            .iter()
            .map(|((rid, tmid), text)| (rid.clone(), tmid.clone(), text.clone()))
            .collect();
        // The cache gets what quitting writes, then closes after its queued
        // writes; it opens again (with the outbox) when the server resumes.
        crate::cache::close(s);
        s.reset();
        for (rid, tmid, text) in drafts {
            s.workspace.set_draft(&rid, tmid.as_deref(), &text);
        }
        if let Some(l) = s.login.as_mut() {
            l.cancel_pending();
        }
        s.login = None;
        s.restoring = false;
        s.pending_room = None;
        let close = match handle {
            Some(h) => Task::perform(async move { h.session().close().await }, |()| Message::Noop),
            None => Task::none(),
        };
        let page = self.leave_pages_of(sid);
        let notice = if uploads.is_empty() {
            Task::none()
        } else {
            self.toast(
                BannerKind::Warning,
                t!("policy.uploads_suspended", names = uploads.join(", ")),
                String::new(),
            )
        };
        Task::batch([typing, close, page, notice])
    }

    /// Starts server `sid` as at start-up: its stored token is read and its
    /// session starts, or its login screen shows.
    pub(super) fn start_server(&mut self, sid: &ServerId) -> Task<Message> {
        if self.is_suspended(sid) {
            return Task::none();
        }
        let uid = self.config.server(sid).and_then(|e| e.user_id.clone());
        match uid {
            Some(uid) => self.read_server_token(sid.clone(), uid),
            None => self.show_login(sid, None),
        }
    }

    /// Reads the token of `uid` on `sid` from the secret store
    /// (`Message::ServerTokenRead`); the login screen when the store is not
    /// open.
    pub(super) fn read_server_token(&mut self, sid: ServerId, uid: String) -> Task<Message> {
        let Some(store) = self.secrets.clone() else {
            return self.show_login(&sid, None);
        };
        if let Some(s) = self.servers.get_mut(&sid) {
            s.restoring = true;
        }
        let key = secret_key(&sid, &uid);
        let user = uid.clone();
        Task::perform(
            async move {
                tokio::task::spawn_blocking(move || {
                    store.get(&key).unwrap_or_else(|e| {
                        tracing::warn!(error = %e, "cannot read token");
                        None
                    })
                })
                .await
                .ok()
                .flatten()
                .map(|token| Credentials::new(user, token))
            },
            move |creds| Message::ServerTokenRead(sid.clone(), uid.clone(), creds),
        )
    }

    /// [`Self::read_server_token`] finished: the session starts unless the
    /// server was suspended meanwhile, another start came first, or the
    /// account signed out (as `on_secrets_ready` decides).
    pub(super) fn on_server_token(
        &mut self,
        sid: ServerId,
        uid: String,
        creds: Option<Credentials>,
    ) -> Task<Message> {
        let same_user =
            self.config.server(&sid).and_then(|e| e.user_id.as_deref()) == Some(uid.as_str());
        let suspended = self.is_suspended(&sid);
        let signed_out = self
            .config
            .is_secret_deletion_pending(&secret_key(&sid, &uid));
        let Some(s) = self.servers.get_mut(&sid) else {
            return Task::none();
        };
        if suspended || !same_user || !s.restoring || s.credentials.is_some() || s.login.is_some() {
            return Task::none();
        }
        match creds.filter(|c| !signed_out && c.user_id == uid) {
            Some(c) => {
                s.restoring = false;
                s.credentials = Some(c);
                s.workspace.apply_connection(ConnectionState::Connecting);
                Task::none()
            }
            None => self.show_login(&sid, None),
        }
    }

    /// A managed server whose entry is still named by its host takes the
    /// server's site name once it answers (section 9.1).
    pub(super) fn adopt_site_name(&mut self, sid: &ServerId, site_name: &str) -> Task<Message> {
        let name = site_name.trim();
        if name.is_empty() {
            return Task::none();
        }
        let Some(s) = self.servers.get_mut(sid) else {
            return Task::none();
        };
        let Some(entry) = self.config.server_mut(sid) else {
            return Task::none();
        };
        if !servers::takes_site_name(&self.policy, &s.url, &entry.display_name) || name == s.name {
            return Task::none();
        }
        entry.display_name = name.to_owned();
        s.name = name.to_owned();
        self.save_config()
    }

    /// After every update: the status menus offer "Remove workspace" only
    /// where the policy allows it, and managed servers still named by their
    /// host take their site name once a signed-in session knows it.
    pub(super) fn server_policy_after_update(&mut self) -> Task<Message> {
        let mut named = Vec::new();
        // `may_remove_server` per server, without cloning the ids.
        let may_add = self.may_add_servers();
        let policy = &self.policy;
        let config = &self.config;
        for (sid, s) in &mut self.servers {
            s.status_menu.remove_refused = !may_add || policy.is_managed_server(&s.url);
            if let Some(site) = s.workspace.settings().str("Site_Name")
                && site.trim() != s.name
                && config
                    .server(sid)
                    .is_some_and(|e| servers::takes_site_name(policy, &s.url, &e.display_name))
            {
                named.push((sid.clone(), site.to_owned()));
            }
        }
        let tasks: Vec<_> = named
            .into_iter()
            .map(|(sid, name)| self.adopt_site_name(&sid, &name))
            .collect();
        Task::batch(tasks)
    }

    pub(super) fn on_policy_notice(&mut self, m: policy_notice::Message) -> Task<Message> {
        match m {
            policy_notice::Message::Details => self.navigate(Nav::PolicySettings),
            policy_notice::Message::Support => match self.policy.support_url() {
                Some(url) => self.open_url(url.to_owned()),
                None => Task::none(),
            },
            // The confirmation and `remove_server` check the policy again.
            policy_notice::Message::Remove(sid) => {
                if self.may_remove_server(&sid)
                    && !self
                        .modals
                        .iter()
                        .any(|m| matches!(m, Modal::ConfirmRemove(s) if *s == sid))
                {
                    self.modals.push(Modal::ConfirmRemove(sid));
                }
                Task::none()
            }
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use std::sync::Arc;

    use rustket_core::config::{AppConfig, ServerEntry};
    use rustket_core::policy::{RawValue, Snapshot, SourceId, Trust, resolve};
    use rustket_core::secrets::{MemoryStore, SecretBackend, SwitchableStore};

    use crate::app::{App, Flags, SecretsMode};
    use crate::message::{Message, SecretsReady};

    /// A suspension cancels the server's running uploads and names them at
    /// once; it does not leave them for the "closed while uploading" notice
    /// of the next start (`crate::cache::close`).
    #[tokio::test]
    async fn a_suspension_names_the_cancelled_uploads() {
        let dir = tempfile::tempdir().unwrap();
        let mut cfg = AppConfig::default();
        crate::legal::accept_terms(&mut cfg);
        // English: `App::new` selects it itself and never switches the
        // process locale to the system's language.
        cfg.ui.language = rustket_core::config::Language::new("en").unwrap();
        let mut e = ServerEntry::new("https://chat.corp.example", "Corp");
        e.user_id = Some("U1".into());
        let sid = cfg.add_server(e);
        cfg.save(dir.path()).unwrap();
        let (mut a, _) = App::new(Flags {
            config_dir: Some(dir.path().to_owned()),
            cache_dir: Some(dir.path().join("cache")),
            secrets: SecretsMode::Memory,
            ..Flags::default()
        });
        let _ = a.update(Message::SecretsReady(SecretsReady {
            store: Arc::new(SwitchableStore::new(
                Box::new(MemoryStore::new()),
                SecretBackend::Memory,
                None,
            )),
            tokens: vec![(sid.clone(), Some("T-U1".into()))],
            notices: vec![],
        }));
        assert!(a.servers[&sid].credentials.is_some());
        let uploads = [
            crate::room::upload::register_pending(sid.as_str(), "report.pdf"),
            crate::room::upload::register_pending(sid.as_str(), "photo.jpg"),
        ];
        let toasts = a.toasts.len();
        let snap = Snapshot::new(SourceId::Test("admin".into()), Trust::Full).with(
            "AllowedServers",
            RawValue::List(vec![RawValue::String("other.example".into())]),
        );
        let _ = a.set_policy(Arc::new(resolve(&[snap], &crate::policy::context())));
        assert!(a.is_suspended(&sid));
        assert!(uploads.iter().all(iced::task::Handle::is_aborted));
        assert_eq!(a.toasts.len(), toasts + 1);
        let notice = a.toasts.last().unwrap();
        assert_eq!(notice.kind, rustket_ui::widget::BannerKind::Warning);
        assert_eq!(
            notice.title,
            "Upload interrupted: the workspace was suspended by your organization's policy. \
             Files: report.pdf, photo.jpg"
        );
        assert!(crate::room::upload::running_names(sid.as_str()).is_empty());
    }

    /// (d) A pinned server on its login screen (no credentials) under a
    /// change that takes the pins out of effect: nothing starts, the login
    /// screen stays and uses the new TLS configuration.
    #[tokio::test]
    async fn pinned_login_screen_takes_the_new_tls() {
        let dir = tempfile::tempdir().unwrap();
        let mut cfg = AppConfig::default();
        crate::legal::accept_terms(&mut cfg);
        cfg.ui.language = rustket_core::config::Language::new("en").unwrap();
        let mut e = ServerEntry::new("https://self-signed.example", "S");
        e.user_id = Some("U1".into());
        e.pinned_certificates = vec![rustket_core::config::PinnedCertificate {
            host: "self-signed.example".into(),
            sha256: rocketchat::tls::fingerprint_hex(&[7; 32]),
        }];
        let sid = cfg.add_server(e);
        cfg.save(dir.path()).unwrap();
        let (mut a, _) = App::new(Flags {
            config_dir: Some(dir.path().to_owned()),
            cache_dir: Some(dir.path().join("cache")),
            secrets: SecretsMode::Memory,
            ..Flags::default()
        });
        let tokens = a
            .startup_token_reads()
            .into_iter()
            .map(|(sid, _)| (sid, None))
            .collect();
        let _ = a.update(Message::SecretsReady(SecretsReady {
            store: Arc::new(SwitchableStore::new(
                Box::new(MemoryStore::new()),
                SecretBackend::Memory,
                None,
            )),
            tokens,
            notices: vec![],
        }));
        assert!(a.servers[&sid].login.is_some());
        let before = a.servers[&sid].generation;
        let snap = Snapshot::new(SourceId::Test("admin".into()), Trust::Full)
            .with("AllowCertificateExceptions", RawValue::Bool(false));
        let task = a.set_policy(Arc::new(resolve(&[snap], &crate::policy::context())));
        assert_eq!(task.units(), 0, "a task");
        let s = &a.servers[&sid];
        let l = s.login.as_ref().expect("the login screen went");
        assert_eq!(s.generation, before);
        assert!(s.pins.is_empty());
        assert!(s.credentials.is_none() && !s.restoring);
        assert!(
            Arc::ptr_eq(&l.tls().client_config(), &s.tls.client_config()),
            "the login screen keeps the old TLS configuration"
        );
    }
}
