//! LDAP test and sync (docs/admin.md §17). Results and errors are i18n keys
//! (`LDAP_Connection_successful`, `LDAP_disabled`, …).

use serde_json::json;

use crate::auth::TwoFactorCode;
use crate::error::Result;
use crate::models::admin::MessageResult;
use crate::rest::RestClient;

impl RestClient {
    pub async fn ldap_test_connection(&self) -> Result<String> {
        let r: MessageResult = self.post("ldap.testConnection", &json!({})).await?;
        Ok(r.message)
    }

    pub async fn ldap_test_search(&self, username: &str) -> Result<String> {
        let r: MessageResult = self
            .post("ldap.testSearch", &json!({ "username": username }))
            .await?;
        Ok(r.message)
    }

    /// `ldap.syncNow` (2FA; on CE asked every time).
    pub async fn ldap_sync_now(&self, two_factor: Option<&TwoFactorCode>) -> Result<String> {
        let r: MessageResult = self
            .post_with("ldap.syncNow", &json!({}), two_factor)
            .await?;
        Ok(r.message)
    }
}
