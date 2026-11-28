//! Opening the secret store and the tokens read from it; the memory-only
//! preference (`UiPrefs::secrets_memory_only`) applied at runtime.

use std::collections::BTreeMap;
use std::sync::Arc;

use iced::Task;
use rocketchat::Credentials;
use rust_i18n::t;
use rustket_core::ServerId;
use rustket_core::e2e::E2E_SECRET_PREFIX;
use rustket_core::policy::LockedPref;
use rustket_core::secrets::{
    self, FilePolicy, MemoryStore, SecretBackend, SecretNotice, SecretStore, SwitchableStore,
    secret_key,
};
use rustket_core::workspace::ConnectionState;
use rustket_ui::widget::BannerKind;

use super::{App, Modal, Screen, SecretsMode};
use crate::component::Nav;
use crate::message::{Go, Message, SecretsReady};

impl App {
    pub(super) fn secret_task(
        &self,
        op: impl FnOnce(&dyn SecretStore) -> Result<(), String> + Send + 'static,
    ) -> Task<Result<(), String>> {
        let Some(store) = self.secrets.clone() else {
            return Task::done(Err("secret store not ready".into()));
        };
        Task::perform(
            async move {
                tokio::task::spawn_blocking(move || op(store.as_ref()))
                    .await
                    .unwrap_or_else(|e| Err(e.to_string()))
            },
            |r| r,
        )
    }

    pub(super) fn on_secrets_ready(&mut self, s: SecretsReady) -> Task<Message> {
        let backend = s.store.backend();
        self.secrets = Some(s.store.clone());
        self.secret_switch = Some(s.store);
        self.secret_backend = Some(backend);
        let mut tasks = Vec::new();
        for n in &s.notices {
            tasks.push(self.toast(BannerKind::Warning, t!("app.secrets_notice"), n.clone()));
        }
        if backend == SecretBackend::MemoryForced {
            tasks.push(self.toast_with_action(
                BannerKind::Warning,
                t!("settings.secrets_keychain_title"),
                t!("settings.secrets_keychain_memory"),
                t!("app.keyring_file_action"),
                Message::Go(Go::Nav(Nav::SecretStoreSettings)),
            ));
        }
        // The preference changed while the store opened (a policy refresh):
        // the store follows it below.
        let moving = self.secrets_file_policy() != self.secrets_opened_with;
        // Memory by policy is the user's choice: the settings card shows it.
        // No warning for a file the store is about to leave.
        let leaving_file = moving && self.secrets_file_policy() != FilePolicy::Allowed;
        if backend == SecretBackend::File {
            if leaving_file {
                self.secrets_file_warning_deferred = true;
            } else {
                tasks.push(self.secrets_file_warning());
            }
        }
        // A token of a signed-out account (its delete did not reach this
        // store) is not a login. Decided now: the purge below may clear the
        // pending deletion before the accounts start.
        let tokens = s
            .tokens
            .into_iter()
            .map(|(sid, token)| {
                let signed_out = self
                    .config
                    .server(&sid)
                    .and_then(|e| e.user_id.as_deref())
                    .is_some_and(|uid| {
                        self.config
                            .is_secret_deletion_pending(&secret_key(&sid, uid))
                    });
                (sid, token.filter(|_| !signed_out))
            })
            .collect();
        self.boot_tokens = Some(tokens);
        if moving {
            tasks.push(self.apply_secrets_file_policy(self.config.ui.secrets_memory_only, false));
        }
        tasks.push(self.purge_pending_secrets());
        #[cfg(feature = "self-update")]
        tasks.push(crate::updater::app::boot(self));
        tasks.push(self.start_workspaces());
        Task::batch(tasks)
    }

    /// The warning that secrets are stored in an unencrypted file.
    fn secrets_file_warning(&mut self) -> Task<Message> {
        self.toast_with_action(
            BannerKind::Warning,
            t!("app.secrets_file_title"),
            t!("app.keyring_file"),
            t!("app.keyring_file_action"),
            Message::Go(Go::Nav(Nav::SecretStoreSettings)),
        )
    }

    /// Starts the saved accounts (sessions, login screens) and handles the
    /// queued links, once the secret store is open and the terms are
    /// accepted: from [`Self::on_secrets_ready`] or from accepting the
    /// terms, whichever comes last. The accounts start only once. Under a
    /// blocked policy every server is suspended, so none starts, and the
    /// links wait for the block to end.
    pub(super) fn start_workspaces(&mut self) -> Task<Message> {
        if !self.terms_accepted() {
            return Task::none();
        }
        let mut tasks = Vec::new();
        if let Some(tokens) = self.boot_tokens.take() {
            tasks.push(self.start_accounts(tokens));
        }
        tasks.push(self.handle_queued_links());
        tasks.push(self.apply_launch_at_login());
        Task::batch(tasks)
    }

    /// Handles the links that waited for the accounts to start, unless the
    /// accounts may not run yet ([`Self::accounts_live`]).
    pub(super) fn handle_queued_links(&mut self) -> Task<Message> {
        if !self.booted || !self.accounts_live() {
            return Task::none();
        }
        let links = std::mem::take(&mut self.queued_links);
        let tasks: Vec<_> = links.iter().map(|l| self.handle_link(l)).collect();
        Task::batch(tasks)
    }

    fn start_accounts(&mut self, tokens: BTreeMap<ServerId, Option<String>>) -> Task<Message> {
        let mut tasks = Vec::new();
        let sids: Vec<ServerId> = self.servers.keys().cloned().collect();
        for sid in sids {
            // Suspended by the policy: no session, no login, no token read.
            if self.is_suspended(&sid) {
                if let Some(st) = self.servers.get_mut(&sid) {
                    st.restoring = false;
                }
                continue;
            }
            let uid = self.config.server(&sid).and_then(|e| e.user_id.clone());
            let token = tokens.get(&sid).cloned().flatten();
            match (uid, token) {
                (Some(uid), Some(token)) => {
                    if let Some(st) = self.servers.get_mut(&sid) {
                        st.restoring = false;
                        st.credentials = Some(Credentials::new(uid, token));
                        st.workspace.apply_connection(ConnectionState::Connecting);
                    }
                }
                // Not read at start (suspended then): read it now.
                (Some(uid), None) if !tokens.contains_key(&sid) => {
                    tasks.push(self.read_server_token(sid, uid));
                }
                _ => tasks.push(self.show_login(&sid, None)),
            }
        }
        // Servers added before the accounts ran (a refresh over the terms
        // brought managed servers to an empty list) need an active one.
        if self.active.is_none() {
            self.active = self.initial_active(self.config.active_server.clone());
        }
        // A screen opened over the terms (a toast's settings link) stays.
        if matches!(self.screen, Screen::Loading) {
            self.screen = self.workspace_screen();
        }
        self.booted = true;
        Task::batch(tasks)
    }
}

impl App {
    /// The memory-only switch of the settings: turning it on asks first.
    /// Nothing happens while the policy forces the preference.
    pub(super) fn on_secrets_memory_only(&mut self, on: bool) -> Task<Message> {
        if self.policy.locks(LockedPref::SecretsMemoryOnly) {
            return Task::none();
        }
        if !on {
            return self.set_secrets_memory_only(false);
        }
        // A switch still running would drop the confirmation.
        if self.secrets_switching
            || self.config.ui.secrets_memory_only
            || self
                .modals
                .iter()
                .any(|m| matches!(m, Modal::SecretsMemoryOnly { .. }))
        {
            return Task::none();
        }
        // A generated E2E password not saved yet would be lost on quit unless
        // the keyring holds it.
        let at_risk = self.secret_backend != Some(SecretBackend::Keyring);
        let pending: Vec<ServerId> = if at_risk {
            self.servers
                .values()
                .filter(|s| {
                    s.e2e
                        .manager()
                        .and_then(rustket_core::e2e::E2eManager::pending_passphrase)
                        .is_some()
                })
                .map(|s| s.id.clone())
                .collect()
        } else {
            Vec::new()
        };
        self.modals.push(Modal::SecretsMemoryOnly { pending });
        if !at_risk {
            return Task::none();
        }
        // Accounts not running now may have one in the store.
        let Some(store) = self.secret_switch.clone() else {
            return Task::none();
        };
        let sids: Vec<ServerId> = self.servers.keys().cloned().collect();
        Task::perform(
            async move {
                tokio::task::spawn_blocking(move || pending_in_store(store.as_ref(), &sids))
                    .await
                    .unwrap_or_default()
            },
            Message::SecretsPendingFound,
        )
    }

    /// Stores the preference and applies it to the secret store; nothing
    /// while the policy forces it.
    pub(super) fn set_secrets_memory_only(&mut self, on: bool) -> Task<Message> {
        if self.secrets_switching || self.policy.locks(LockedPref::SecretsMemoryOnly) {
            return Task::none();
        }
        self.config.ui.secrets_memory_only = on;
        let save = self.save_config();
        Task::batch([save, self.apply_secrets_file_policy(on, true)])
    }

    /// Moves the secret store to the file policy of the memory-only value
    /// `on` (`Message::SecretsPolicyApplied` reports the outcome).
    /// `by_user`: the user's switch, not a policy follow-up (section 7: only
    /// the user's failed switch rolls the preference back).
    pub(super) fn apply_secrets_file_policy(&mut self, on: bool, by_user: bool) -> Task<Message> {
        let Some(store) = self.secret_switch.clone() else {
            return Task::none();
        };
        self.secrets_switching = true;
        self.secrets_switch_by_user = by_user;
        let policy = file_policy(on);
        Task::perform(
            async move {
                let sw = store.clone();
                tokio::task::spawn_blocking(move || {
                    let r = sw.apply_policy(policy);
                    (r.backend, r.error.map(|e| e.to_string()))
                })
                .await
                .unwrap_or_else(|e| (store.backend(), Some(e.to_string())))
            },
            move |(backend, error)| Message::SecretsPolicyApplied(on, backend, error),
        )
    }

    pub(super) fn on_secrets_policy_applied(
        &mut self,
        on: bool,
        backend: SecretBackend,
        error: Option<String>,
    ) -> Task<Message> {
        self.secrets_switching = false;
        let by_user = std::mem::take(&mut self.secrets_switch_by_user);
        self.secret_backend = Some(backend);
        let locked = self.policy.locks(LockedPref::SecretsMemoryOnly);
        let mut tasks = Vec::new();
        let mut rolled_back = false;
        if let Some(e) = error {
            tasks.push(self.toast(BannerKind::Error, t!("settings.secrets_switch_failed"), e));
            // Nothing moved: the preference follows the store again, unless
            // the policy forces it. A move the policy started never touches
            // the user's preference: the store keeps its backend until the
            // next start.
            let unchanged = match self.secret_backend {
                Some(SecretBackend::File) => on,
                Some(SecretBackend::MemoryByPolicy) => !on,
                _ => false,
            };
            if by_user && unchanged && !locked {
                self.config.ui.secrets_memory_only = !on;
                tasks.push(self.save_config());
                rolled_back = true;
            }
        }
        // The effective value changed while the store switched (a policy
        // replaced or removed meanwhile): follow it. A failed switch to the
        // forced value is not retried.
        let want = self.config.ui.secrets_memory_only;
        if !rolled_back && want != on {
            tasks.push(self.apply_secrets_file_policy(want, false));
        } else if std::mem::take(&mut self.secrets_file_warning_deferred)
            && backend == SecretBackend::File
        {
            // The switches settled and the store ended on the file it opened
            // with: the warning deferred at the opening is due now.
            tasks.push(self.secrets_file_warning());
        }
        Task::batch(tasks)
    }

    /// The secret file policy the store opens with: of the effective
    /// `secrets_memory_only`, so a policy that forbids the file applies
    /// from the start.
    pub fn secrets_file_policy(&self) -> FilePolicy {
        file_policy(self.config.ui.secrets_memory_only)
    }
}

/// The secret file policy of the `secrets_memory_only` preference: the Mac
/// App Store flavor never writes the plaintext file, whatever it says.
pub(super) fn file_policy(memory_only: bool) -> FilePolicy {
    if cfg!(feature = "mas") {
        FilePolicy::Forbidden
    } else {
        FilePolicy::memory_only(memory_only)
    }
}

/// Servers of `sids` with a generated E2E password in `store` that was not
/// confirmed as saved (blocking).
fn pending_in_store(store: &dyn SecretStore, sids: &[ServerId]) -> Vec<ServerId> {
    let Ok(Some(entries)) = store.list_entries() else {
        return Vec::new();
    };
    sids.iter()
        .filter(|sid| {
            let prefix = format!("{E2E_SECRET_PREFIX}:{sid}:");
            entries
                .keys()
                .any(|k| k.starts_with(&prefix) && k.ends_with(":pending_passphrase"))
        })
        .cloned()
        .collect()
}

fn notice_text(n: &SecretNotice) -> String {
    match n {
        SecretNotice::KeyringUnavailable(why) => {
            t!("app.keyring_unavailable", reason = why).into_owned()
        }
        SecretNotice::KeyringTokensUnreachable => t!("app.keyring_unreachable").into_owned(),
        SecretNotice::MigratedToKeyring { count } => {
            t!("app.keyring_migrated", count = count).into_owned()
        }
        SecretNotice::PlaintextTokensLeft { count } => {
            t!("app.plaintext_left", count = count).into_owned()
        }
        SecretNotice::FileRemoved { count } => {
            t!("app.secrets_file_removed", count = count).into_owned()
        }
        SecretNotice::FileNotRemoved { path } => {
            t!("app.secret_file_left", path = path.display()).into_owned()
        }
        SecretNotice::FileRecovered { moved_to } => {
            t!("app.secret_file_recovered", path = moved_to.display()).into_owned()
        }
    }
}

/// Opens the secret store and reads the tokens (blocking).
pub(super) fn open_secrets(
    mode: SecretsMode,
    policy: FilePolicy,
    wanted: Vec<(ServerId, String)>,
) -> SecretsReady {
    let (store, notices) = match mode {
        SecretsMode::Memory => (
            SwitchableStore::new(Box::new(MemoryStore::new()), SecretBackend::Memory, None),
            Vec::new(),
        ),
        SecretsMode::System => {
            let (store, notices) = SwitchableStore::opened(secrets::open_default_report(policy));
            (store, notices.iter().map(notice_text).collect())
        }
    };
    let tokens = wanted
        .into_iter()
        .map(|(sid, uid)| {
            let token = store.get(&secret_key(&sid, &uid)).unwrap_or_else(|e| {
                tracing::warn!(error = %e, "cannot read token");
                None
            });
            (sid, token)
        })
        .collect();
    SecretsReady {
        store: Arc::new(store),
        tokens,
        notices,
    }
}

#[cfg(test)]
mod policy_tests {
    use super::*;

    /// The store flavor never falls back to the plaintext file; the others
    /// follow the preference.
    #[test]
    fn file_policy_follows_the_flavor() {
        let got = [file_policy(false), file_policy(true)];
        if cfg!(feature = "mas") {
            assert_eq!(got, [FilePolicy::Forbidden; 2]);
        } else {
            assert_eq!(got, [FilePolicy::Allowed, FilePolicy::Never]);
        }
    }
}
