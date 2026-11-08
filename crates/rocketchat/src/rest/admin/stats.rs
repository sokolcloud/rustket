//! Workspace info: statistics, instances, license (docs/admin.md §1, §3).

use serde_json::Value;

use super::{Page, paged};
use crate::error::Result;
use crate::models::admin::{
    CloudRegistrationStatus, Instance, LicenseInfo, MaxActiveUsers, Statistics,
};
use crate::models::{Paginated, ServerInfo};
use crate::rest::{Query, RestClient, field};

impl RestClient {
    /// `GET /api/info` with the auth token (the full build info needs it).
    pub async fn admin_server_info(&self) -> Result<ServerInfo> {
        let url = self.server().api_root("info");
        self.send(self.request(reqwest::Method::GET, &url, None))
            .await
    }

    /// `GET statistics`: the last stored snapshot, or a new one with `refresh`.
    pub async fn statistics(&self, refresh: bool) -> Result<Statistics> {
        let mut s: Statistics = self
            .get("statistics", &Query::new().push("refresh", refresh))
            .await?;
        s.extra.remove("success");
        Ok(s)
    }

    /// Stored statistics snapshots.
    pub async fn statistics_list(&self, page: &Page) -> Result<Paginated<Statistics>> {
        let v: Value = self
            .get("statistics.list", &page.apply(Query::new()))
            .await?;
        paged(v, "statistics", page)
    }

    pub async fn instances_get(&self) -> Result<Vec<Instance>> {
        let v: Value = self.get("instances.get", &Query::new()).await?;
        field(v, "instances")
    }

    /// `licenses.info`; `load_values` adds current usage to each limit.
    pub async fn admin_licenses_info(&self, load_values: bool) -> Result<LicenseInfo> {
        let q = if load_values {
            Query::new().push("loadValues", true)
        } else {
            Query::new()
        };
        let v: Value = self.get("licenses.info", &q).await?;
        field(v, "license")
    }

    pub async fn licenses_max_active_users(&self) -> Result<MaxActiveUsers> {
        self.get("licenses.maxActiveUsers", &Query::new()).await
    }

    /// `cloud.registrationStatus` (needs `manage-cloud`).
    pub async fn cloud_registration_status(&self) -> Result<CloudRegistrationStatus> {
        let v: Value = self.get("cloud.registrationStatus", &Query::new()).await?;
        field(v, "registrationStatus")
    }
}
