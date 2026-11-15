//! Opening the secret store and the tokens read from it; the memory-only
//! preference (`UiPrefs::secrets_memory_only`) applied at runtime.

use std::collections::BTreeMap;
use std::sync::Arc;

use iced::Task;
use rocketchat::Credentials;
use rust_i18n::t;
use rustket_core::ServerId;
use rustket_core::e2e::E2E_SECRET_PREFIX;
use rustket_core::secrets::{
    self, FilePolicy, MemoryStore, SecretBackend, SecretNotice, SecretStore, SwitchableStore,
    secret_key,
};
use rustket_core::workspace::ConnectionState;
use rustket_ui::widget::BannerKind;

use super::{App, Modal, Screen, SecretsMode};
use crate::component::Nav;
use crate::message::{Go, Message, SecretsReady};
use crate::screens::add_server::AddServer;

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
        // Memory by policy is the user's choice: the settings card shows it.
        if backend == SecretBackend::File {
            tasks.push(self.toast_with_action(
                BannerKind::Warning,
                t!("app.secrets_file_title"),
                t!("app.keyring_file"),
                t!("app.keyring_file_action"),
                Message::Go(Go::Nav(Nav::SecretStoreSettings)),
            ));
        }
        let tokens: BTreeMap<ServerId, Option<String>> = s.tokens.into_iter().collect();
        let sids: Vec<ServerId> = self.servers.keys().cloned().collect();
        for sid in sids {
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
                _ => tasks.push(self.show_login(&sid, None)),
            }
        }
        self.screen = if self.servers.is_empty() {
            Screen::AddServer(Box::new(AddServer::new(false)))
        } else {
            Screen::Server
        };
        self.booted = true;
        #[cfg(feature = "self-update")]
        tasks.push(crate::updater::app::boot(self));
        let links = std::mem::take(&mut self.queued_links);
        for l in links {
            tasks.push(self.handle_link(&l));
        }
        Task::batch(tasks)
    }
}

impl App {
    /// The memory-only switch of the settings: turning it on asks first.
    pub(super) fn on_secrets_memory_only(&mut self, on: bool) -> Task<Message> {
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

    /// Stores the preference and applies it to the secret store.
    pub(super) fn set_secrets_memory_only(&mut self, on: bool) -> Task<Message> {
        if self.secrets_switching {
            return Task::none();
        }
        self.config.ui.secrets_memory_only = on;
        let save = self.save_config();
        let Some(store) = self.secret_switch.clone() else {
            return save;
        };
        self.secrets_switching = true;
        let policy = FilePolicy::memory_only(on);
        let apply = Task::perform(
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
        );
        Task::batch([save, apply])
    }

    pub(super) fn on_secrets_policy_applied(
        &mut self,
        on: bool,
        backend: SecretBackend,
        error: Option<String>,
    ) -> Task<Message> {
        self.secrets_switching = false;
        self.secret_backend = Some(backend);
        let Some(e) = error else {
            return Task::none();
        };
        let mut tasks =
            vec![self.toast(BannerKind::Error, t!("settings.secrets_switch_failed"), e)];
        // Nothing moved: the preference follows the store again.
        let unchanged = match self.secret_backend {
            Some(SecretBackend::File) => on,
            Some(SecretBackend::MemoryByPolicy) => !on,
            _ => false,
        };
        if unchanged {
            self.config.ui.secrets_memory_only = !on;
            tasks.push(self.save_config());
        }
        Task::batch(tasks)
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
