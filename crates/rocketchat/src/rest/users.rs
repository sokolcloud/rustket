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

/// Body of `users.deleteOwnAccount`: the secret as SHA-256 hex, as the web
/// client sends it.
pub fn delete_own_account_body(secret: &str, confirm_relinquish: bool) -> Value {
    let password = crate::auth::sha256_hex(secret);
    if confirm_relinquish {
        json!({ "password": password, "confirmRelinquish": true })
    } else {
        json!({ "password": password })
    }
}

/// First Rocket.Chat version whose `users.setStatus` takes `expiresAt` and
/// clears the status itself (`statusExpiresAt`, the 8.6.0 presence engine).
/// Older servers do not know the field (newer request schemas reject
/// unknown ones).
pub const STATUS_EXPIRY_SINCE: (u32, u32, u32) = (8, 6, 0);

/// Body of `users.setStatus` (`status: None`: text only).
pub fn set_status_body(
    status: Option<UserStatus>,
    message: Option<&str>,
    expires_at: Option<RcDate>,
) -> Value {
    let mut body = json!({});
    if let Some(s) = status {
        body["status"] = Value::String(s.as_str().into());
    }
    if let Some(m) = message {
        body["message"] = Value::String(m.into());
    }
    if let Some(at) = expires_at {
        body["expiresAt"] = Value::String(at.to_iso());
    }
    body
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

    /// Publishes the current user's UTC offset in hours (`users.setPreferences`
    /// `{data:{utcOffset}}`; the server stores it on the user, where
    /// `users.info` returns it as `utcOffset`).
    pub async fn set_utc_offset(&self, hours: f64) -> Result<()> {
        self.post_ok(
            "users.setPreferences",
            &json!({ "data": { "utcOffset": hours } }),
        )
        .await
    }

    /// Sets the current user's status and optional custom status text.
    /// `expires_at`: when the server clears it (`statusExpiresAt`, Rocket.Chat
    /// 8.6+, see [`STATUS_EXPIRY_SINCE`]); without it the server drops any
    /// expiry the status had.
    pub async fn users_set_status(
        &self,
        status: UserStatus,
        message: Option<&str>,
        expires_at: Option<RcDate>,
    ) -> Result<()> {
        self.post_ok(
            "users.setStatus",
            &set_status_body(Some(status), message, expires_at),
        )
        .await
    }

    /// Changes only the custom status text: the server keeps the status the
    /// user chose (`statusDefault`), so a transient auto-away is not pinned.
    /// `expires_at` as in [`Self::users_set_status`].
    pub async fn users_set_status_message(
        &self,
        message: &str,
        expires_at: Option<RcDate>,
    ) -> Result<()> {
        self.post_ok(
            "users.setStatus",
            &set_status_body(None, Some(message), expires_at),
        )
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

    /// Sets the current user's avatar from an image (`users.setAvatar`,
    /// multipart field `image`). Sent like file uploads: no total timeout.
    pub async fn users_set_avatar(&self, filename: &str, mime: &str, bytes: Vec<u8>) -> Result<()> {
        let part = reqwest::multipart::Part::bytes(bytes)
            .file_name(filename.to_owned())
            .mime_str(mime)
            .map_err(|e| crate::Error::Protocol(format!("invalid MIME type {mime}: {e}")))?;
        let form = reqwest::multipart::Form::new().part("image", part);
        let url = self.server().api("users.setAvatar");
        let _: Value = self
            .send(
                self.authed(self.upload_http(), reqwest::Method::POST, &url, None)
                    .multipart(form),
            )
            .await?;
        Ok(())
    }

    /// Deletes the signed-in user's account (`users.deleteOwnAccount`,
    /// needs `Accounts_AllowDeleteOwnAccount`). `secret`: the password, or the
    /// username for accounts without one. Without `confirm_relinquish` a
    /// last owner of rooms gets `user-last-owner`
    /// ([`crate::Error::last_owner_rooms`]).
    pub async fn users_delete_own_account(
        &self,
        secret: &str,
        confirm_relinquish: bool,
    ) -> Result<()> {
        self.post_ok(
            "users.deleteOwnAccount",
            &delete_own_account_body(secret, confirm_relinquish),
        )
        .await
    }

    /// Back to the default avatar (`users.resetAvatar`).
    pub async fn users_reset_avatar(&self, user_id: &str) -> Result<()> {
        self.post_ok("users.resetAvatar", &json!({ "userId": user_id }))
            .await
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn delete_own_account_hashes_the_secret() {
        let b = delete_own_account_body("pw", false);
        assert_eq!(
            b,
            json!({"password": "30c952fab122c3f9759f02a6d95c3758b246b4fee239957b2d4fee46e26170c4"})
        );
        assert_eq!(
            delete_own_account_body("pw", true)["confirmRelinquish"],
            true
        );
    }
}
