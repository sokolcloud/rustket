//! OAuth apps: Rocket.Chat as OAuth2 provider (docs/admin.md §11).

use serde_json::{Value, json};

use crate::error::Result;
use crate::models::admin::OAuthApp;
use crate::rest::{Query, RestClient, field};

impl RestClient {
    pub async fn oauth_apps_list(&self) -> Result<Vec<OAuthApp>> {
        let v: Value = self.get("oauth-apps.list", &Query::new()).await?;
        field(v, "oauthApps")
    }

    pub async fn oauth_app_get(&self, id: &str) -> Result<OAuthApp> {
        let v: Value = self
            .get("oauth-apps.get", &Query::new().push("_id", id))
            .await?;
        field(v, "oauthApp")
    }

    /// `redirect_uri` may hold several comma-separated URIs.
    pub async fn oauth_app_create(
        &self,
        name: &str,
        active: bool,
        redirect_uri: &str,
    ) -> Result<OAuthApp> {
        let v: Value = self
            .post(
                "oauth-apps.create",
                &json!({ "name": name, "active": active, "redirectUri": redirect_uri }),
            )
            .await?;
        field(v, "application")
    }

    /// Answers the app's fields at the top level.
    pub async fn oauth_app_update(
        &self,
        id: &str,
        name: &str,
        active: bool,
        redirect_uri: &str,
    ) -> Result<OAuthApp> {
        self.post(
            "oauth-apps.update",
            &json!({ "appId": id, "name": name, "active": active, "redirectUri": redirect_uri }),
        )
        .await
    }

    /// Answers a bare `true`.
    pub async fn oauth_app_delete(&self, id: &str) -> Result<()> {
        let _: Value = self
            .post("oauth-apps.delete", &json!({ "appId": id }))
            .await?;
        Ok(())
    }
}
