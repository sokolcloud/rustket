//! Invite links (docs/admin.md §6).

use serde::Deserialize;
use serde_json::{Value, json};

use crate::error::Result;
use crate::models::admin::Invite;
use crate::models::lenient;
use crate::rest::{Query, RestClient};

#[derive(Deserialize)]
struct Valid {
    #[serde(default)]
    valid: bool,
}

impl RestClient {
    /// All invites (bare array; `url` is empty in stored invites).
    pub async fn list_invites(&self) -> Result<Vec<Invite>> {
        let v: Value = self.get("listInvites", &Query::new()).await?;
        Ok(lenient::counted(v).items)
    }

    /// `DELETE removeInvite/:_id` (answers a bare `true`).
    pub async fn remove_invite(&self, id: &str) -> Result<()> {
        let _: Value = self
            .delete(
                &format!("removeInvite/{}", crate::server_url::encode_segment(id)),
                &Query::new(),
            )
            .await?;
        Ok(())
    }

    /// `findOrCreateInvite`: `days` ∈ [`Invite::DAYS`], `max_uses` ∈
    /// [`Invite::MAX_USES`]. The caller must be subscribed to `rid`.
    pub async fn find_or_create_invite(
        &self,
        rid: &str,
        days: u32,
        max_uses: u32,
    ) -> Result<Invite> {
        self.post(
            "findOrCreateInvite",
            &json!({ "rid": rid, "days": days, "maxUses": max_uses }),
        )
        .await
    }

    pub async fn validate_invite_token(&self, token: &str) -> Result<bool> {
        let v: Valid = self
            .post("validateInviteToken", &json!({ "token": token }))
            .await?;
        Ok(v.valid)
    }
}
