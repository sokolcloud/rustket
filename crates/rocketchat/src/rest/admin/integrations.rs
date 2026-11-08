//! Incoming / outgoing webhooks (docs/admin.md §10).

use serde_json::{Value, json};

use super::Page;
use crate::error::Result;
use crate::models::Paginated;
use crate::models::admin::{Integration, IntegrationHistory, IntegrationParams, IntegrationType};
use crate::rest::{Query, RestClient, field, paginated};

impl RestClient {
    /// `integrations.list` (`name`: case-insensitive substring; `kind: None` = both).
    pub async fn integrations_list(
        &self,
        name: &str,
        kind: Option<IntegrationType>,
        page: &Page,
    ) -> Result<Paginated<Integration>> {
        let mut q = page.apply(Query::new());
        if !name.is_empty() {
            q = q.push("name", name);
        }
        if let Some(k) = kind {
            q = q.push("type", k.as_str());
        }
        let v: Value = self.get("integrations.list", &q).await?;
        paginated(v, "integrations")
    }

    pub async fn integration_get(&self, id: &str) -> Result<Integration> {
        let v: Value = self
            .get("integrations.get", &Query::new().push("integrationId", id))
            .await?;
        field(v, "integration")
    }

    pub async fn integration_create(&self, params: &IntegrationParams) -> Result<Integration> {
        let mut body = serde_json::to_value(params)?;
        if let Some(o) = body.as_object_mut() {
            o.remove("integrationId");
        }
        let v: Value = self.post("integrations.create", &body).await?;
        field(v, "integration")
    }

    /// `PUT integrations.update` (`params.integration_id` required).
    pub async fn integration_update(&self, params: &IntegrationParams) -> Result<Integration> {
        if params.integration_id.as_deref().is_none_or(str::is_empty) {
            return Err(crate::Error::Protocol("missing integrationId".into()));
        }
        let v: Value = self.put_with("integrations.update", params, None).await?;
        field(v, "integration")
    }

    pub async fn integration_remove(&self, kind: IntegrationType, id: &str) -> Result<()> {
        let _: Value = self
            .post(
                "integrations.remove",
                &json!({ "type": kind.as_str(), "integrationId": id }),
            )
            .await?;
        Ok(())
    }

    /// History of an outgoing webhook.
    pub async fn integration_history(
        &self,
        id: &str,
        page: &Page,
    ) -> Result<Paginated<IntegrationHistory>> {
        let v: Value = self
            .get(
                "integrations.history",
                &page.apply(Query::new().push("id", id)),
            )
            .await?;
        paginated(v, "history")
    }

    /// Clears the history (no stream event follows; refetch).
    pub async fn integration_clear_history(&self, id: &str) -> Result<()> {
        self.post_ok("integrations.clearHistory", &json!({ "integrationId": id }))
            .await
    }

    pub async fn integration_replay(&self, id: &str, history_id: &str) -> Result<()> {
        self.post_ok(
            "integrations.replayOutgoing",
            &json!({ "integrationId": id, "historyId": history_id }),
        )
        .await
    }
}
