//! Settings, permissions, roles, emoji, statuses, slash commands.

use serde::Deserialize;
use serde_json::{Value, json};

use super::{Query, RestClient, field, paginated};
use crate::RcDate;
use crate::error::Result;
use crate::models::{
    CustomEmoji, CustomUserStatus, Paginated, Permission, Role, Setting, SlashCommand, UpdatedSince,
};

/// Page size used when fetching complete lists (server cap: `API_Upper_Count_Limit`, 100).
const PAGE: u64 = 100;

#[derive(Deserialize)]
struct EmojiEnvelope {
    emojis: UpdatedSince<CustomEmoji>,
}

impl RestClient {
    /// One page of `settings.public` (works without login).
    pub async fn settings_public(&self, offset: u64, count: u64) -> Result<Paginated<Setting>> {
        let v: Value = self
            .get(
                "settings.public",
                &Query::new().push("offset", offset).push("count", count),
            )
            .await?;
        paginated(v, "settings")
    }

    /// All public settings (pages of 100).
    pub async fn settings_public_all(&self) -> Result<Vec<Setting>> {
        let mut all = Vec::new();
        let mut offset = 0usize;
        loop {
            let page = self.settings_public(offset as u64, PAGE).await?;
            let n = page.raw_len;
            offset += n;
            all.extend(page.items);
            if n == 0 || offset as u64 >= page.total {
                return Ok(all);
            }
        }
    }

    /// Selected public settings: `settings.public?_id=a,b,c`.
    pub async fn settings_public_by_ids(&self, ids: &[&str]) -> Result<Vec<Setting>> {
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        let v: Value = self
            .get(
                "settings.public",
                &Query::new().push("_id", ids.join(",")).push("count", PAGE),
            )
            .await?;
        field(v, "settings")
    }

    /// Custom emoji (delta when `updated_since` is set).
    pub async fn emoji_custom_list(
        &self,
        updated_since: Option<RcDate>,
    ) -> Result<UpdatedSince<CustomEmoji>> {
        let e: EmojiEnvelope = self
            .get(
                "emoji-custom.list",
                &Query::new().opt("updatedSince", updated_since.map(|d| d.to_iso())),
            )
            .await?;
        Ok(e.emojis)
    }

    pub async fn custom_user_status_list(&self) -> Result<Vec<CustomUserStatus>> {
        let mut all = Vec::new();
        let mut offset = 0usize;
        loop {
            let v: Value = self
                .get(
                    "custom-user-status.list",
                    &Query::new().push("offset", offset).push("count", PAGE),
                )
                .await?;
            let page: Paginated<CustomUserStatus> = paginated(v, "statuses")?;
            let n = page.raw_len;
            offset += n;
            all.extend(page.items);
            if n == 0 || offset as u64 >= page.total {
                return Ok(all);
            }
        }
    }

    /// Permissions (delta when `updated_since` is set).
    pub async fn permissions_list_all(
        &self,
        updated_since: Option<RcDate>,
    ) -> Result<UpdatedSince<Permission>> {
        self.get(
            "permissions.listAll",
            &Query::new().opt("updatedSince", updated_since.map(|d| d.to_iso())),
        )
        .await
    }

    pub async fn roles_list(&self) -> Result<Vec<Role>> {
        let v: Value = self.get("roles.list", &Query::new()).await?;
        field(v, "roles")
    }

    /// All slash commands.
    pub async fn commands_list(&self) -> Result<Vec<SlashCommand>> {
        let mut all = Vec::new();
        let mut offset = 0usize;
        loop {
            let v: Value = self
                .get(
                    "commands.list",
                    &Query::new().push("offset", offset).push("count", PAGE),
                )
                .await?;
            let page: Paginated<SlashCommand> = paginated(v, "commands")?;
            let n = page.raw_len;
            offset += n;
            all.extend(page.items);
            if n == 0 || offset as u64 >= page.total {
                return Ok(all);
            }
        }
    }

    /// Runs a slash command (`command` without the leading `/`).
    pub async fn commands_run(
        &self,
        command: &str,
        params: &str,
        rid: &str,
        tmid: Option<&str>,
        trigger_id: Option<&str>,
    ) -> Result<Value> {
        let mut body = json!({
            "command": command.trim_start_matches('/'),
            "params": params,
            "roomId": rid,
        });
        if let Some(t) = tmid {
            body["tmid"] = Value::String(t.into());
        }
        if let Some(t) = trigger_id {
            body["triggerId"] = Value::String(t.into());
        }
        self.post("commands.run", &body).await
    }
}
