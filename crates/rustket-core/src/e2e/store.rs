//! E2E secrets in the [`SecretStore`]: the decrypted private JWK and the pending
//! generated passphrase. Never in the config or the cache.

use std::sync::Arc;

use rocketchat::e2e::Zeroizing;

use crate::config::ServerId;
use crate::secrets::{SecretError, SecretStore};

/// Key prefix of E2E secrets (`rustket-e2e:<server_id>:<user_id>:<what>`).
pub const E2E_SECRET_PREFIX: &str = "rustket-e2e";

pub fn private_key_secret(server: &ServerId, user_id: &str) -> String {
    format!("{E2E_SECRET_PREFIX}:{server}:{user_id}:private_key")
}

pub fn pending_passphrase_secret(server: &ServerId, user_id: &str) -> String {
    format!("{E2E_SECRET_PREFIX}:{server}:{user_id}:pending_passphrase")
}

/// Async access to one account's E2E secrets (store calls run on a blocking
/// thread: keyring backends may wait for an unlock prompt).
#[derive(Clone)]
pub struct E2eSecrets {
    store: Arc<dyn SecretStore>,
    private_key: String,
    pending: String,
}

impl std::fmt::Debug for E2eSecrets {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("E2eSecrets")
            .field("private_key", &self.private_key)
            .finish_non_exhaustive()
    }
}

async fn blocking<T: Send + 'static>(
    f: impl FnOnce() -> Result<T, SecretError> + Send + 'static,
) -> Result<T, SecretError> {
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|e| SecretError::Keyring(format!("secret store task failed: {e}")))?
}

impl E2eSecrets {
    pub fn new(store: Arc<dyn SecretStore>, server: &ServerId, user_id: &str) -> Self {
        Self {
            store,
            private_key: private_key_secret(server, user_id),
            pending: pending_passphrase_secret(server, user_id),
        }
    }

    async fn get(&self, key: &str) -> Result<Option<Zeroizing<String>>, SecretError> {
        let (store, key) = (self.store.clone(), key.to_owned());
        Ok(blocking(move || store.get(&key)).await?.map(Zeroizing::new))
    }

    async fn set(&self, key: &str, value: &str) -> Result<(), SecretError> {
        let (store, key, value) = (
            self.store.clone(),
            key.to_owned(),
            Zeroizing::new(value.to_owned()),
        );
        blocking(move || store.set(&key, &value)).await
    }

    async fn delete(&self, key: &str) -> Result<(), SecretError> {
        let (store, key) = (self.store.clone(), key.to_owned());
        blocking(move || store.delete(&key)).await
    }

    pub async fn private_key(&self) -> Result<Option<Zeroizing<String>>, SecretError> {
        self.get(&self.private_key).await
    }

    pub async fn set_private_key(&self, jwk: &str) -> Result<(), SecretError> {
        self.set(&self.private_key, jwk).await
    }

    pub async fn pending_passphrase(&self) -> Result<Option<Zeroizing<String>>, SecretError> {
        self.get(&self.pending).await
    }

    pub async fn set_pending_passphrase(&self, passphrase: &str) -> Result<(), SecretError> {
        self.set(&self.pending, passphrase).await
    }

    pub async fn delete_private_key(&self) -> Result<(), SecretError> {
        self.delete(&self.private_key).await
    }

    pub async fn delete_pending_passphrase(&self) -> Result<(), SecretError> {
        self.delete(&self.pending).await
    }

    /// Deletes both secrets (logout, `force_logout`, key reset).
    pub async fn clear(&self) -> Result<(), SecretError> {
        let a = self.delete(&self.private_key).await;
        let b = self.delete(&self.pending).await;
        a.and(b)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::secrets::MemoryStore;

    #[tokio::test]
    async fn keys_and_roundtrip() {
        let server = ServerId("srv".into());
        assert_eq!(
            private_key_secret(&server, "u"),
            "rustket-e2e:srv:u:private_key"
        );
        assert_eq!(
            pending_passphrase_secret(&server, "u"),
            "rustket-e2e:srv:u:pending_passphrase"
        );
        let mem = Arc::new(MemoryStore::new());
        let s = E2eSecrets::new(mem.clone(), &server, "u");
        assert!(s.private_key().await.unwrap().is_none());
        s.set_private_key("jwk").await.unwrap();
        s.set_pending_passphrase("words").await.unwrap();
        assert_eq!(
            s.private_key()
                .await
                .unwrap()
                .as_deref()
                .map(String::as_str),
            Some("jwk")
        );
        assert_eq!(
            mem.get("rustket-e2e:srv:u:pending_passphrase")
                .unwrap()
                .as_deref(),
            Some("words")
        );
        s.clear().await.unwrap();
        assert!(s.pending_passphrase().await.unwrap().is_none());
        assert!(s.private_key().await.unwrap().is_none());
    }
}
