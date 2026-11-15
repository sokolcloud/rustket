//! Opening the secret store and the tokens read from it.

use std::collections::BTreeMap;
use std::sync::Arc;

use iced::Task;
use rocketchat::Credentials;
use rust_i18n::t;
use rustket_core::ServerId;
use rustket_core::secrets::{
    self, MemoryStore, SecretBackend, SecretNotice, SecretStore, secret_key,
};
use rustket_core::workspace::ConnectionState;
use rustket_ui::widget::BannerKind;

use super::{App, Screen, SecretsMode};
use crate::message::{Message, SecretsReady};
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
        self.secrets = Some(s.store);
        self.secret_backend = Some(s.backend);
        let mut tasks = Vec::new();
        for n in &s.notices {
            tasks.push(self.toast(BannerKind::Warning, t!("app.secrets_notice"), n.clone()));
        }
        if s.backend == SecretBackend::File && self.secrets_mode == SecretsMode::System {
            tasks.push(self.toast(
                BannerKind::Warning,
                t!("app.keyring_missing"),
                t!("app.keyring_file"),
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
        SecretNotice::FileRecovered { moved_to } => {
            t!("app.secret_file_recovered", path = moved_to.display()).into_owned()
        }
    }
}

/// Opens the secret store and reads the tokens (blocking).
pub(super) fn open_secrets(mode: SecretsMode, wanted: Vec<(ServerId, String)>) -> SecretsReady {
    let (store, backend, notices): (Box<dyn SecretStore>, SecretBackend, Vec<String>) = match mode {
        SecretsMode::Memory => (
            Box::new(MemoryStore::new()),
            SecretBackend::Memory,
            Vec::new(),
        ),
        SecretsMode::System => {
            let o = secrets::open_default_report();
            let notices = o.notices.iter().map(notice_text).collect();
            (o.store, o.backend, notices)
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
        store: Arc::from(store),
        backend,
        tokens,
        notices,
    }
}
