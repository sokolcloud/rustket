//! Account key lifecycle (§7.1): start, unlock, passphrase change, reset, logout.

use std::sync::Arc;
use std::sync::atomic::Ordering;

use rocketchat::auth::TwoFactorCode;
use rocketchat::e2e::{
    E2eError, PrivateKeyEnvelope, UserKeyPair, UserPrivateKey, UserPublicKey, Zeroizing,
    encrypt_v2, generate_passphrase, validate_new_passphrase,
};

use crate::secrets::SecretError;

use super::{
    Account, AccountE2eState, E2eApi, E2eEvent, E2eManager, E2eManagerError, Result, Token,
    blocking, lock, write,
};

impl<A: E2eApi> E2eManager<A> {
    // ----- account lifecycle -------------------------------------------------

    /// Loads or creates the account keys (§7.1). Never generates keys when the
    /// server could not be asked. On success the rooms known so far are
    /// handshaken and the distribution loop starts. With `E2E_Enable` off only a
    /// cached key is loaded, read-only ([`AccountE2eState::DisabledReadOnly`]).
    /// Failures leave the account in [`AccountE2eState::Error`].
    pub async fn start(&self) -> Result<()> {
        let _op = self.inner.account_op.lock().await;
        let t = self.token();
        let r = if self.settings().enabled {
            self.set_account_state(AccountE2eState::LoadingKeys);
            self.start_enabled(t).await
        } else {
            self.start_read_only(t).await
        };
        if let Err(e) = &r
            && !matches!(e, E2eManagerError::Cancelled)
            && self.alive(t)
        {
            self.set_account_state(AccountE2eState::Error(e.to_string()));
        }
        r
    }

    /// The cached private key: `Ok(None)` when there is none (or it does not
    /// parse), `Err` when the secret store could not be read (nothing may be
    /// deleted then).
    async fn cached_private_key(&self) -> Result<Option<UserPrivateKey>, SecretError> {
        let Some(cached) = self.inner.secrets.private_key().await? else {
            return Ok(None);
        };
        Ok(blocking(move || UserPrivateKey::from_jwk_str(&cached).ok())
            .await
            .ok()
            .flatten())
    }

    /// `E2E_Enable` off: load the cached key (if it still matches the server's
    /// public key) for decryption only. Nothing is created or uploaded.
    async fn start_read_only(&self, t: Token) -> Result<()> {
        let Some(key) = self.cached_private_key().await? else {
            self.check(t)?;
            self.set_account_state(AccountE2eState::Disabled);
            return Ok(());
        };
        let matches = match self.inner.api.fetch_my_keys().await {
            Ok(server) => server
                .public_key
                .and_then(|p| UserPublicKey::from_jwk_str(&p).ok())
                .is_some_and(|p| p == key.public_key()),
            // Offline: decrypting with the cached key is harmless.
            Err(e) => {
                tracing::debug!(error = %e, "cannot reconcile the cached E2E key");
                true
            }
        };
        self.check(t)?;
        if !matches {
            self.set_account_state(AccountE2eState::Disabled);
            return Ok(());
        }
        {
            let mut acc = lock(&self.inner.account);
            acc.public_key = Some(key.public_key());
            acc.private_key = Some(Arc::new(key));
        }
        self.set_account_state(AccountE2eState::DisabledReadOnly);
        self.handshake_all().await;
        Ok(())
    }

    async fn start_enabled(&self, t: Token) -> Result<()> {
        // A secret store read error is not "no cached key": fail (retried
        // later) and delete nothing (§7.1).
        let cached_key = self.cached_private_key().await.inspect_err(|e| {
            tracing::warn!(error = %e, "cannot read the cached E2E key");
        })?;
        let server = self.inner.api.fetch_my_keys().await?;
        self.check(t)?;
        let pending = self.inner.secrets.pending_passphrase().await.inspect_err(
            |e| tracing::warn!(error = %e, "cannot read the pending E2E passphrase"),
        )?;

        if let (Some(server_pub), Some(envelope)) = (server.public_key, server.private_key) {
            let server_public = UserPublicKey::from_jwk_str(&server_pub).ok();
            let stale_key = cached_key.is_some();
            let usable = cached_key.filter(|k| server_public.as_ref() == Some(&k.public_key()));
            self.check(t)?;
            if let Some(key) = usable {
                {
                    let mut acc = lock(&self.inner.account);
                    acc.public_key = Some(key.public_key());
                    acc.private_key = Some(Arc::new(key));
                    acc.server_public = Some(server_pub);
                    acc.server_envelope = Some(envelope);
                    acc.pending_passphrase = pending.clone();
                }
                self.became_ready(t, pending.is_some()).await;
                return Ok(());
            }
            // No usable cached key. A generated passphrase the user has not
            // saved yet survives a forced logout: it unlocks the backup again.
            if let Some(pw) = pending.clone() {
                let restored = self
                    .unlock_envelope(&envelope, server_public.as_ref(), &pw)
                    .await?;
                self.check(t)?;
                if let Some((key, jwk)) = restored {
                    if let Err(e) = self.inner.secrets.set_private_key(&jwk).await {
                        tracing::warn!(error = %e, "cannot cache the E2E key");
                    }
                    self.check_after_secret_write(t).await?;
                    {
                        let mut acc = lock(&self.inner.account);
                        acc.public_key = Some(key.public_key());
                        acc.private_key = Some(Arc::new(key));
                        acc.server_public = Some(server_pub);
                        acc.server_envelope = Some(envelope);
                        acc.pending_passphrase = Some(pw);
                    }
                    self.became_ready(t, true).await;
                    return Ok(());
                }
            }
            // Keys were created or reset elsewhere: drop the stale local
            // secrets (only what was read and found not to match).
            if stale_key && let Err(e) = self.inner.secrets.delete_private_key().await {
                tracing::warn!(error = %e, "cannot delete the stale E2E key");
            }
            if pending.is_some()
                && let Err(e) = self.inner.secrets.delete_pending_passphrase().await
            {
                tracing::warn!(error = %e, "cannot delete the stale E2E passphrase");
            }
            self.check(t)?;
            {
                let mut acc = lock(&self.inner.account);
                acc.public_key = None;
                acc.private_key = None;
                acc.server_public = Some(server_pub);
                acc.server_envelope = Some(envelope);
                acc.pending_passphrase = None;
            }
            self.set_account_state(AccountE2eState::EnterPassword { failed: false });
            return Ok(());
        }

        // No keys on the server: reuse a cached pair or create one, upload it
        // with a fresh generated passphrase.
        let pair = match cached_key {
            Some(k) => UserKeyPair::from_private(k),
            None => blocking(UserKeyPair::generate).await??,
        };
        let private_jwk = pair.private.to_jwk_string()?;
        let public_jwk = pair.public.to_jwk_string();
        let passphrase = generate_passphrase();
        let envelope = {
            let (jwk, pw, uid) = (
                private_jwk.clone(),
                passphrase.clone(),
                self.inner.user_id.clone(),
            );
            blocking(move || encrypt_v2(&jwk, &pw, &uid)).await??
        };
        self.check(t)?;
        // A secret store failure is not fatal: the key and the passphrase stay in
        // memory for this session (the user is asked again next start).
        if let Err(e) = self.inner.secrets.set_private_key(&private_jwk).await {
            tracing::warn!(error = %e, "cannot cache the E2E key");
        }
        if let Err(e) = self.inner.secrets.set_pending_passphrase(&passphrase).await {
            tracing::warn!(error = %e, "cannot store the generated E2E passphrase");
        }
        self.check_after_secret_write(t).await?;
        match self
            .inner
            .api
            .set_user_keys(&public_jwk, &envelope, false)
            .await
        {
            Ok(()) => {}
            Err(e) if e.api_error_code() == Some("error-keys-already-set") => {
                // Another device uploaded keys meanwhile: unlock those instead.
                let _ = self.inner.secrets.clear().await;
                let theirs = self.inner.api.fetch_my_keys().await?;
                self.check(t)?;
                {
                    let mut acc = lock(&self.inner.account);
                    acc.server_public = theirs.public_key;
                    acc.server_envelope = theirs.private_key;
                    acc.pending_passphrase = None;
                }
                self.set_account_state(AccountE2eState::EnterPassword { failed: false });
                return Ok(());
            }
            Err(e) => return Err(e.into()),
        }
        self.check(t)?;
        if let Err(e) = self.inner.api.request_subscription_keys().await {
            tracing::debug!(error = %e, "e2e.requestSubscriptionKeys failed");
        }
        self.check(t)?;
        {
            let mut acc = lock(&self.inner.account);
            acc.public_key = Some(pair.public.clone());
            acc.private_key = Some(Arc::new(pair.private));
            acc.server_public = Some(public_jwk);
            acc.server_envelope = Some(envelope);
            acc.pending_passphrase = Some(passphrase);
        }
        self.became_ready(t, true).await;
        Ok(())
    }

    async fn became_ready(&self, t: Token, save_password: bool) {
        if !self.alive(t) {
            return;
        }
        self.set_account_state(if save_password {
            AccountE2eState::SavePassword
        } else {
            AccountE2eState::Ready
        });
        self.handshake_all().await;
        if self.inner.config.auto_distribution && self.alive(t) {
            self.start_distribution();
        }
    }

    /// Decrypts the key backup `envelope` with `password`: `Ok(None)` when the
    /// password is wrong or the key does not match `server_public`.
    async fn unlock_envelope(
        &self,
        envelope: &str,
        server_public: Option<&UserPublicKey>,
        password: &str,
    ) -> Result<Option<(UserPrivateKey, Zeroizing<String>)>> {
        let (envelope, uid) = (envelope.to_owned(), self.inner.user_id.clone());
        let pw = Zeroizing::new(password.to_owned());
        let decrypted = blocking(move || -> Result<(UserPrivateKey, Zeroizing<String>)> {
            let env = PrivateKeyEnvelope::parse(&envelope, &uid)?;
            let jwk = env.decrypt(&pw)?;
            let key = UserPrivateKey::from_jwk_str(&jwk).map_err(|_| E2eError::WrongPassword)?;
            Ok((key, jwk))
        })
        .await?;
        match decrypted {
            Ok((key, jwk)) if server_public.is_none_or(|p| *p == key.public_key()) => {
                Ok(Some((key, jwk)))
            }
            Ok(_) | Err(E2eManagerError::Crypto(_)) => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// Unlocks the key backup with the E2E passphrase (state `EnterPassword`).
    pub async fn submit_password(&self, password: &str) -> Result<()> {
        let _op = self.inner.account_op.lock().await;
        let t = self.token();
        let (envelope, server_public) = {
            let acc = lock(&self.inner.account);
            if !matches!(acc.state, AccountE2eState::EnterPassword { .. }) {
                return Err(E2eManagerError::NotReady);
            }
            (acc.server_envelope.clone(), acc.server_public.clone())
        };
        let envelope = envelope.ok_or(E2eManagerError::NotReady)?;
        let uid = self.inner.user_id.clone();
        let pw = Zeroizing::new(password.to_owned());
        let decrypted = blocking(move || -> Result<(UserPrivateKey, Zeroizing<String>)> {
            let env = PrivateKeyEnvelope::parse(&envelope, &uid)?;
            let jwk = env.decrypt(&pw)?;
            // Garbage that happens to parse (CBC) still fails the import.
            let key = UserPrivateKey::from_jwk_str(&jwk).map_err(|_| E2eError::WrongPassword)?;
            Ok((key, jwk))
        })
        .await?;
        let (key, jwk) = match decrypted {
            Ok(v) => v,
            Err(E2eManagerError::Crypto(E2eError::WrongPassword)) => {
                self.check(t)?;
                self.set_account_state(AccountE2eState::EnterPassword { failed: true });
                return Err(E2eManagerError::WrongPassword);
            }
            Err(e) => return Err(e),
        };
        self.check(t)?;
        if let Some(public) = server_public.and_then(|p| UserPublicKey::from_jwk_str(&p).ok())
            && public != key.public_key()
        {
            tracing::warn!("decrypted E2E key does not match the server public key");
            self.set_account_state(AccountE2eState::EnterPassword { failed: true });
            return Err(E2eManagerError::WrongPassword);
        }
        // Unlocking works without the secret store (asked again next start).
        if let Err(e) = self.inner.secrets.set_private_key(&jwk).await {
            tracing::warn!(error = %e, "cannot cache the E2E key");
        }
        self.check_after_secret_write(t).await?;
        {
            let mut acc = lock(&self.inner.account);
            acc.public_key = Some(key.public_key());
            acc.private_key = Some(Arc::new(key));
            acc.pending_passphrase = None;
        }
        self.became_ready(t, false).await;
        Ok(())
    }

    /// "I saved my password": forgets the generated passphrase.
    pub async fn confirm_passphrase_saved(&self) -> Result<()> {
        self.inner.secrets.delete_pending_passphrase().await?;
        lock(&self.inner.account).pending_passphrase = None;
        if self.account_state() == AccountE2eState::SavePassword {
            self.set_account_state(AccountE2eState::Ready);
        }
        Ok(())
    }

    /// Re-encrypts the key backup with a new passphrase (web policy enforced) and
    /// uploads it with `force: true`.
    pub async fn change_passphrase(&self, new_passphrase: &str) -> Result<()> {
        validate_new_passphrase(new_passphrase)?;
        let _op = self.inner.account_op.lock().await;
        let t = self.token();
        let (private, public) = {
            let acc = lock(&self.inner.account);
            if !acc.state.is_ready() {
                return Err(E2eManagerError::NotReady);
            }
            (acc.private_key.clone(), acc.public_key.clone())
        };
        let (private, public) = private.zip(public).ok_or(E2eManagerError::NotReady)?;
        let jwk = private.to_jwk_string()?;
        let (pw, uid) = (
            Zeroizing::new(new_passphrase.to_owned()),
            self.inner.user_id.clone(),
        );
        let envelope = blocking(move || encrypt_v2(&jwk, &pw, &uid)).await??;
        self.check(t)?;
        self.inner
            .api
            .set_user_keys(&public.to_jwk_string(), &envelope, true)
            .await?;
        self.check(t)?;
        let _ = self.inner.secrets.delete_pending_passphrase().await;
        {
            let mut acc = lock(&self.inner.account);
            acc.server_envelope = Some(envelope);
            acc.pending_passphrase = None;
        }
        self.set_account_state(AccountE2eState::Ready);
        Ok(())
    }

    /// `users.resetE2EKey {}`: the server revokes all login tokens, so the caller
    /// must log out afterwards. Local E2E secrets are deleted on success. A
    /// [`rocketchat::Error::TwoFactorRequired`] asks for a code (often
    /// `method: password`).
    pub async fn reset_own_keys(&self, two_factor: Option<&TwoFactorCode>) -> Result<()> {
        self.inner.api.reset_own_key(two_factor).await?;
        self.logout().await;
        Ok(())
    }

    /// Stops the manager and forgets keys in memory (keeps the secret store).
    /// Operations still in flight drop their results.
    pub fn stop(&self) {
        self.inner.stops.fetch_add(1, Ordering::SeqCst);
        if let Some(h) = lock(&self.inner.distribution).take() {
            h.abort();
        }
        *lock(&self.inner.account) = Account::default();
        lock(&self.inner.rooms).clear();
        lock(&self.inner.key_requests).clear();
        write(&self.inner.summaries).clear();
        write(&self.inner.keys).clear();
        lock(&self.inner.cache).clear();
        self.emit(E2eEvent::AccountState(AccountE2eState::Disabled));
    }

    /// Explicit logout / key reset: stop and delete the E2E secrets of this
    /// account (the cached key and a pending generated passphrase).
    pub async fn logout(&self) {
        self.inner.logouts.fetch_add(1, Ordering::SeqCst);
        self.stop();
        if let Err(e) = self.inner.secrets.clear().await {
            tracing::warn!(error = %e, "cannot delete E2E secrets");
        }
    }

    /// A logout the user did not choose (`force_logout`, a rejected token):
    /// stop and delete the cached key, but keep a generated passphrase the
    /// user has not saved yet. It is the only copy; the next start unlocks the key backup with it and
    /// asks the user to save it again (web: the stored `e2e.randomPassword`
    /// survives logout).
    pub async fn forced_logout(&self) {
        self.inner.logouts.fetch_add(1, Ordering::SeqCst);
        self.stop();
        if let Err(e) = self.inner.secrets.delete_private_key().await {
            tracing::warn!(error = %e, "cannot delete the cached E2E key");
        }
    }
}
