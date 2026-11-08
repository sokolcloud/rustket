//! Custom user statuses (docs/admin.md §9).

use serde_json::{Value, json};

use super::{Page, paged};
use crate::error::Result;
use crate::models::{CustomUserStatus, Paginated, UserStatus};
use crate::rest::{Query, RestClient, field};

impl RestClient {
    /// One page of `custom-user-status.list` (`name`: substring filter).
    pub async fn custom_user_status_page(
        &self,
        name: &str,
        page: &Page,
    ) -> Result<Paginated<CustomUserStatus>> {
        let mut q = page.apply(Query::new());
        if !name.is_empty() {
            q = q.push("name", name);
        }
        let v: Value = self.get("custom-user-status.list", &q).await?;
        paged(v, "statuses", page)
    }

    pub async fn custom_user_status_create(
        &self,
        name: &str,
        status_type: UserStatus,
    ) -> Result<CustomUserStatus> {
        let v: Value = self
            .post(
                "custom-user-status.create",
                &json!({ "name": name, "statusType": status_type.as_str() }),
            )
            .await?;
        field(v, "customUserStatus")
    }

    /// Both fields are always sent: an omitted `statusType` would clear it.
    pub async fn custom_user_status_update(
        &self,
        id: &str,
        name: &str,
        status_type: UserStatus,
    ) -> Result<CustomUserStatus> {
        let v: Value = self
            .post(
                "custom-user-status.update",
                &json!({ "_id": id, "name": name, "statusType": status_type.as_str() }),
            )
            .await?;
        field(v, "customUserStatus")
    }

    pub async fn custom_user_status_delete(&self, id: &str) -> Result<()> {
        self.post_ok(
            "custom-user-status.delete",
            &json!({ "customUserStatusId": id }),
        )
        .await
    }
}
