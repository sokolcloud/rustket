//! User administration (docs/admin.md §4).

use serde::Deserialize;
use serde_json::{Value, json};

use super::{AdminUpload, Page, paged};
use crate::auth::TwoFactorCode;
use crate::error::Result;
use crate::models::Paginated;
use crate::models::admin::{ActiveStatusResult, AdminUser, NewUser, UserUpdate, UsersFilter};
use crate::rest::{Query, RestClient, UserLookup, field};

#[derive(Deserialize)]
struct SmtpCheck {
    #[serde(default, rename = "isSMTPConfigured")]
    configured: bool,
}

#[derive(Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
struct DeleteResult {
    deleted_rooms: Vec<Value>,
}

impl RestClient {
    /// `users.listByStatus` (the admin users table). `query`/`fields` would be
    /// ignored by the server, so only the explicit filters exist.
    pub async fn admin_users_list(
        &self,
        filter: &UsersFilter,
        page: &Page,
    ) -> Result<Paginated<AdminUser>> {
        let mut q = page
            .apply(Query::new())
            .opt("status", filter.status.map(|s| s.as_str()))
            .opt("type", filter.kind.as_deref())
            .opt(
                "searchTerm",
                filter.search_term.as_deref().filter(|s| !s.is_empty()),
            )
            .opt("hasLoggedIn", filter.has_logged_in);
        for r in &filter.inactive_reason {
            q = q.push("inactiveReason[]", r);
        }
        for r in &filter.roles {
            q = q.push("roles[]", r);
        }
        let v: Value = self.get("users.listByStatus", &q).await?;
        paged(v, "users", page)
    }

    /// `users.info` with the user's rooms (`includeUserRooms`).
    pub async fn admin_user_info(&self, user: UserLookup<'_>) -> Result<AdminUser> {
        let q = match user {
            UserLookup::Id(id) => Query::new().push("userId", id),
            UserLookup::Username(u) => Query::new().push("username", u),
        }
        .push("includeUserRooms", true);
        let v: Value = self.get("users.info", &q).await?;
        field(v, "user")
    }

    /// `users.create` (no 2FA).
    pub async fn admin_user_create(&self, user: &NewUser) -> Result<AdminUser> {
        let v: Value = self.post("users.create", user).await?;
        field(v, "user")
    }

    /// `users.update` (2FA, remembered for a while). Send only changed fields.
    pub async fn admin_user_update(
        &self,
        user_id: &str,
        data: &UserUpdate,
        confirm_relinquish: bool,
        two_factor: Option<&TwoFactorCode>,
    ) -> Result<AdminUser> {
        let mut body = json!({ "userId": user_id, "data": data });
        if confirm_relinquish {
            body["confirmRelinquish"] = Value::Bool(true);
        }
        let v: Value = self.post_with("users.update", &body, two_factor).await?;
        field(v, "user")
    }

    /// Activates or deactivates a user. On `user-last-owner` see
    /// [`crate::models::admin::LastOwnerDetails::from_error`].
    pub async fn admin_set_active_status(
        &self,
        user_id: &str,
        active: bool,
        confirm_relinquish: bool,
    ) -> Result<ActiveStatusResult> {
        let mut body = json!({ "userId": user_id, "activeStatus": active });
        if confirm_relinquish {
            body["confirmRelinquish"] = Value::Bool(true);
        }
        let v: Value = self.post("users.setActiveStatus", &body).await?;
        field(v, "user")
    }

    /// `users.delete`; returns the ids of rooms deleted with the user.
    pub async fn admin_user_delete(
        &self,
        user_id: &str,
        confirm_relinquish: bool,
    ) -> Result<Vec<Value>> {
        let mut body = json!({ "userId": user_id });
        if confirm_relinquish {
            body["confirmRelinquish"] = Value::Bool(true);
        }
        let r: DeleteResult = self.post("users.delete", &body).await?;
        Ok(r.deleted_rooms)
    }

    /// Resets another user's TOTP (2FA every time). The user id is mandatory: the
    /// server would reset the caller's own 2FA without it.
    pub async fn admin_reset_totp(
        &self,
        user_id: &str,
        two_factor: Option<&TwoFactorCode>,
    ) -> Result<()> {
        let _: Value = self
            .post_with(
                "users.resetTOTP",
                &json!({ "userId": non_empty(user_id)? }),
                two_factor,
            )
            .await?;
        Ok(())
    }

    /// Resets another user's E2E key (2FA every time). The user id is mandatory: the
    /// server would reset the caller's own key without it.
    pub async fn admin_reset_e2e_key(
        &self,
        user_id: &str,
        two_factor: Option<&TwoFactorCode>,
    ) -> Result<()> {
        let _: Value = self
            .post_with(
                "users.resetE2EKey",
                &json!({ "userId": non_empty(user_id)? }),
                two_factor,
            )
            .await?;
        Ok(())
    }

    pub async fn admin_send_welcome_email(&self, email: &str) -> Result<()> {
        self.post_ok("users.sendWelcomeEmail", &json!({ "email": email }))
            .await
    }

    /// Sets another user's avatar from an image (multipart `image` + `userId`).
    pub async fn admin_set_user_avatar(&self, user_id: &str, image: AdminUpload) -> Result<()> {
        self.upload_form(
            "users.setAvatar",
            vec![("userId", user_id.to_owned())],
            Some(("image", image)),
            None,
        )
        .await?;
        Ok(())
    }

    /// Whether SMTP is configured (welcome e-mails can be sent).
    pub async fn smtp_check(&self) -> Result<bool> {
        let r: SmtpCheck = self.get("smtp.check", &Query::new()).await?;
        Ok(r.configured)
    }

    /// `sendInvitationEmail` to a list of addresses (`bulk-register-user`).
    pub async fn send_invitation_email(&self, emails: &[String]) -> Result<()> {
        self.post_ok("sendInvitationEmail", &json!({ "emails": emails }))
            .await
    }
}

fn non_empty(id: &str) -> Result<&str> {
    if id.trim().is_empty() {
        return Err(crate::Error::Protocol("empty user id".into()));
    }
    Ok(id)
}
