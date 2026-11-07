//! Users, presence, search.

use serde_json::{Value, json};

use super::{Query, RestClient, field};
use crate::RcDate;
use crate::error::Result;
use crate::models::{Presence, RoomSummary, SpotlightResult, User, UserStatus, UserSummary};

/// How to look a user up.
#[derive(Debug, Clone, Copy)]
pub enum UserLookup<'a> {
    Id(&'a str),
    Username(&'a str),
}

impl RestClient {
    pub async fn users_info(&self, user: UserLookup<'_>) -> Result<User> {
        let q = match user {
            UserLookup::Id(id) => Query::new().push("userId", id),
            UserLookup::Username(u) => Query::new().push("username", u),
        };
        let v: Value = self.get("users.info", &q).await?;
        field(v, "user")
    }

    /// Sets the current user's status and optional custom status text.
    pub async fn users_set_status(&self, status: UserStatus, message: Option<&str>) -> Result<()> {
        let mut body = json!({ "status": status.as_str() });
        if let Some(m) = message {
            body["message"] = Value::String(m.into());
        }
        self.post_ok("users.setStatus", &body).await
    }

    /// Changes only the custom status text: the server keeps the status the
    /// user chose (`statusDefault`), so a transient auto-away is not pinned.
    pub async fn users_set_status_message(&self, message: &str) -> Result<()> {
        self.post_ok("users.setStatus", &json!({ "message": message }))
            .await
    }

    /// Presence of users changed since `from` (all users with a non-offline status
    /// when `None`), optionally limited to `ids`.
    pub async fn users_presence(&self, from: Option<RcDate>, ids: &[&str]) -> Result<Presence> {
        let mut q = Query::new().opt("from", from.map(|d| d.to_iso()));
        for id in ids {
            q = q.push("ids[]", id);
        }
        self.get("users.presence", &q).await
    }

    /// Quick switcher search over users and rooms (`@user`, `#room` prefixes allowed).
    pub async fn spotlight(&self, query: &str) -> Result<SpotlightResult> {
        self.get("spotlight", &Query::new().push("query", query))
            .await
    }

    /// `@` autocomplete.
    pub async fn users_autocomplete(&self, term: &str) -> Result<Vec<UserSummary>> {
        let v: Value = self
            .get(
                "users.autocomplete",
                &Query::new().push("selector", json!({ "term": term })),
            )
            .await?;
        field(v, "items")
    }

    /// `#` autocomplete over channels and private groups the user can see.
    pub async fn rooms_autocomplete_channel_and_private(
        &self,
        term: &str,
    ) -> Result<Vec<RoomSummary>> {
        let v: Value = self
            .get(
                "rooms.autocomplete.channelAndPrivate",
                &Query::new().push("selector", json!({ "name": term })),
            )
            .await?;
        field(v, "items")
    }
}
