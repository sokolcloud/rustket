//! Permissions and roles (docs/admin.md §13). `permissions.listAll` and
//! `roles.list` are in `rest/misc.rs`.

use serde::Deserialize;
use serde_json::{Value, json};

use super::Page;
use crate::RcDate;
use crate::error::Result;
use crate::models::admin::{PermissionDef, RoleParams, UserInRole};
use crate::models::{Paginated, Role, UpdatedSince};
use crate::rest::{Query, RestClient, field, paginated};

#[derive(Deserialize)]
struct RolesSync {
    #[serde(default)]
    roles: UpdatedSince<Role>,
}

impl RestClient {
    /// `permissions.listAll` with the setting-permission fields (`level`, `settingId`,
    /// `group`, …).
    pub async fn permissions_list_all_full(
        &self,
        updated_since: Option<RcDate>,
    ) -> Result<UpdatedSince<PermissionDef>> {
        self.get(
            "permissions.listAll",
            &Query::new().opt("updatedSince", updated_since.map(|d| d.to_iso())),
        )
        .await
    }

    /// Adds `role` to a permission (atomic; also updates the group permission of a
    /// setting permission).
    pub async fn permissions_add_role(&self, permission_id: &str, role: &str) -> Result<()> {
        self.post_ok(
            "permissions.addRole",
            &json!({ "permissionId": permission_id, "role": role }),
        )
        .await
    }

    pub async fn permissions_remove_role(&self, permission_id: &str, role: &str) -> Result<()> {
        self.post_ok(
            "permissions.removeRole",
            &json!({ "permissionId": permission_id, "role": role }),
        )
        .await
    }

    /// Replaces whole role arrays (race-prone; prefer add/remove).
    pub async fn permissions_update(&self, permissions: &[(String, Vec<String>)]) -> Result<()> {
        let list: Vec<Value> = permissions
            .iter()
            .map(|(id, roles)| json!({ "_id": id, "roles": roles }))
            .collect();
        self.post_ok("permissions.update", &json!({ "permissions": list }))
            .await
    }

    /// `roles.sync?updatedSince=`.
    pub async fn roles_sync(&self, updated_since: RcDate) -> Result<UpdatedSince<Role>> {
        let r: RolesSync = self
            .get(
                "roles.sync",
                &Query::new().push("updatedSince", updated_since.to_iso()),
            )
            .await?;
        Ok(r.roles)
    }

    /// `roles.create` (EE `custom-roles` only).
    pub async fn role_create(&self, params: &RoleParams) -> Result<Role> {
        let mut body = serde_json::to_value(params)?;
        if let Some(o) = body.as_object_mut() {
            o.remove("roleId");
        }
        let v: Value = self.post("roles.create", &body).await?;
        field(v, "role")
    }

    /// `roles.update`; use [`RoleParams::update_of`] so name, scope and description
    /// are repeated.
    pub async fn role_update(&self, params: &RoleParams) -> Result<Role> {
        let v: Value = self.post("roles.update", params).await?;
        field(v, "role")
    }

    pub async fn role_delete(&self, role_id: &str) -> Result<()> {
        self.post_ok("roles.delete", &json!({ "roleId": role_id }))
            .await
    }

    /// Users holding `role` (globally, or in `room_id` for Subscriptions roles).
    pub async fn roles_get_users_in_role(
        &self,
        role: &str,
        room_id: Option<&str>,
        page: &Page,
    ) -> Result<Paginated<UserInRole>> {
        let q = page.apply(Query::new().push("role", role).opt("roomId", room_id));
        let v: Value = self.get("roles.getUsersInRole", &q).await?;
        paginated(v, "users")
    }

    /// `roles.addUserToRole` (8.9 takes `roleId`).
    pub async fn roles_add_user(
        &self,
        role_id: &str,
        username: &str,
        room_id: Option<&str>,
    ) -> Result<Role> {
        let mut body = json!({ "roleId": role_id, "username": username });
        if let Some(r) = room_id {
            body["roomId"] = Value::String(r.to_owned());
        }
        let v: Value = self.post("roles.addUserToRole", &body).await?;
        field(v, "role")
    }

    pub async fn roles_remove_user(
        &self,
        role_id: &str,
        username: &str,
        room_id: Option<&str>,
    ) -> Result<Role> {
        let mut body = json!({ "roleId": role_id, "username": username });
        if let Some(r) = room_id {
            body["roomId"] = Value::String(r.to_owned());
        }
        let v: Value = self.post("roles.removeUserFromRole", &body).await?;
        field(v, "role")
    }
}
