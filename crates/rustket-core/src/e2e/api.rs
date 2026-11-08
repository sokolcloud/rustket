//! The REST calls the E2E manager needs, behind a trait so the state machine can
//! be tested without a server.

use std::collections::HashMap;
use std::future::Future;

use rocketchat::auth::TwoFactorCode;
use rocketchat::rest::{MyKeys, SetKeyIdOutcome, SuggestedKey, UserPublicKeyEntry};
use rocketchat::{RestClient, Result};

/// Key management endpoints (`docs/e2e.md` §5). Implemented by [`RestClient`].
pub trait E2eApi: Clone + Send + Sync + 'static {
    /// The logged-in user's id.
    fn user_id(&self) -> Option<String>;
    fn fetch_my_keys(&self) -> impl Future<Output = Result<MyKeys>> + Send;
    fn set_user_keys(
        &self,
        public_key: &str,
        private_key: &str,
        force: bool,
    ) -> impl Future<Output = Result<()>> + Send;
    fn request_subscription_keys(&self) -> impl Future<Output = Result<()>> + Send;
    fn set_room_key_id(
        &self,
        rid: &str,
        key_id: &str,
    ) -> impl Future<Output = Result<SetKeyIdOutcome>> + Send;
    fn update_group_key(
        &self,
        rid: &str,
        uid: &str,
        key: &str,
    ) -> impl Future<Output = Result<()>> + Send;
    fn users_of_room_without_key(
        &self,
        rid: &str,
    ) -> impl Future<Output = Result<Vec<UserPublicKeyEntry>>> + Send;
    fn accept_suggested_group_key(&self, rid: &str) -> impl Future<Output = Result<()>> + Send;
    fn reject_suggested_group_key(&self, rid: &str) -> impl Future<Output = Result<()>> + Send;
    fn users_waiting_for_group_key(
        &self,
        room_ids: &[String],
    ) -> impl Future<Output = Result<HashMap<String, Vec<UserPublicKeyEntry>>>> + Send;
    fn provide_users_suggested_group_keys(
        &self,
        keys: &HashMap<String, Vec<SuggestedKey>>,
    ) -> impl Future<Output = Result<()>> + Send;
    fn reset_room_key(
        &self,
        rid: &str,
        e2e_key: &str,
        e2e_key_id: &str,
    ) -> impl Future<Output = Result<()>> + Send;
    fn reset_own_key(
        &self,
        two_factor: Option<&TwoFactorCode>,
    ) -> impl Future<Output = Result<()>> + Send;
    fn set_room_encrypted(
        &self,
        rid: &str,
        encrypted: bool,
    ) -> impl Future<Output = Result<()>> + Send;
}

impl E2eApi for RestClient {
    fn user_id(&self) -> Option<String> {
        RestClient::user_id(self)
    }

    async fn fetch_my_keys(&self) -> Result<MyKeys> {
        self.e2e_fetch_my_keys().await
    }

    async fn set_user_keys(&self, public_key: &str, private_key: &str, force: bool) -> Result<()> {
        self.e2e_set_user_keys(public_key, private_key, force).await
    }

    async fn request_subscription_keys(&self) -> Result<()> {
        self.e2e_request_subscription_keys().await
    }

    async fn set_room_key_id(&self, rid: &str, key_id: &str) -> Result<SetKeyIdOutcome> {
        self.e2e_set_room_key_id(rid, key_id).await
    }

    async fn update_group_key(&self, rid: &str, uid: &str, key: &str) -> Result<()> {
        self.e2e_update_group_key(rid, uid, key).await
    }

    async fn users_of_room_without_key(&self, rid: &str) -> Result<Vec<UserPublicKeyEntry>> {
        self.e2e_users_of_room_without_key(rid).await
    }

    async fn accept_suggested_group_key(&self, rid: &str) -> Result<()> {
        self.e2e_accept_suggested_group_key(rid).await
    }

    async fn reject_suggested_group_key(&self, rid: &str) -> Result<()> {
        self.e2e_reject_suggested_group_key(rid).await
    }

    async fn users_waiting_for_group_key(
        &self,
        room_ids: &[String],
    ) -> Result<HashMap<String, Vec<UserPublicKeyEntry>>> {
        let ids: Vec<&str> = room_ids.iter().map(String::as_str).collect();
        self.e2e_users_waiting_for_group_key(&ids).await
    }

    async fn provide_users_suggested_group_keys(
        &self,
        keys: &HashMap<String, Vec<SuggestedKey>>,
    ) -> Result<()> {
        self.e2e_provide_users_suggested_group_keys(keys).await
    }

    async fn reset_room_key(&self, rid: &str, e2e_key: &str, e2e_key_id: &str) -> Result<()> {
        self.e2e_reset_room_key(rid, e2e_key, e2e_key_id).await
    }

    async fn reset_own_key(&self, two_factor: Option<&TwoFactorCode>) -> Result<()> {
        self.e2e_reset_own_key(two_factor).await
    }

    async fn set_room_encrypted(&self, rid: &str, encrypted: bool) -> Result<()> {
        self.e2e_set_room_encrypted(rid, encrypted).await
    }
}
