//! EE-gated admin areas (docs/admin.md §20) and banners (§19). Without the module
//! the server answers `error-action-not-allowed` ("This is an enterprise feature")
//! or `error-unauthorized`; check `LicenseInfo::has_module` first.

use serde_json::{Value, json};

use super::Page;
use crate::RcDate;
use crate::auth::TwoFactorCode;
use crate::error::Result;
use crate::models::admin::Banner;
use crate::rest::{Query, RestClient, field};

impl RestClient {
    /// `GET engagement-dashboard/<path>?start&end` (module `engagement-dashboard`),
    /// e.g. `users/active-users`. The response shape differs per chart.
    pub async fn engagement_dashboard(
        &self,
        path: &str,
        start: RcDate,
        end: Option<RcDate>,
    ) -> Result<Value> {
        self.get(
            &format!("engagement-dashboard/{}", path.trim_start_matches('/')),
            &Query::new()
                .push("start", start.to_iso())
                .opt("end", end.map(|d| d.to_iso())),
        )
        .await
    }

    /// `sessions/list.all` (module `device-management`, 2FA).
    pub async fn sessions_list_all(
        &self,
        filter: Option<&str>,
        page: &Page,
        two_factor: Option<&TwoFactorCode>,
    ) -> Result<Value> {
        let q = page.apply(Query::new().opt("filter", filter.filter(|f| !f.is_empty())));
        self.get_with("sessions/list.all", &q, two_factor).await
    }

    /// `sessions/info.admin` (2FA).
    pub async fn sessions_info_admin(
        &self,
        session_id: &str,
        two_factor: Option<&TwoFactorCode>,
    ) -> Result<Value> {
        self.get_with(
            "sessions/info.admin",
            &Query::new().push("sessionId", session_id),
            two_factor,
        )
        .await
    }

    /// `sessions/logout` (needs `logout-device-management`, 2FA).
    pub async fn sessions_logout(
        &self,
        session_id: &str,
        two_factor: Option<&TwoFactorCode>,
    ) -> Result<()> {
        let _: Value = self
            .post_with(
                "sessions/logout",
                &json!({ "sessionId": session_id }),
                two_factor,
            )
            .await?;
        Ok(())
    }

    /// `audit.settings` (module `auditing`, `can-audit`): settings change log.
    pub async fn audit_settings(
        &self,
        setting_id: Option<&str>,
        actor: Option<&str>,
        start: Option<RcDate>,
        end: Option<RcDate>,
        page: &Page,
    ) -> Result<Value> {
        let q = page.apply(
            Query::new()
                .opt("settingId", setting_id)
                .opt("actor", actor)
                .opt("start", start.map(|d| d.to_iso()))
                .opt("end", end.map(|d| d.to_iso())),
        );
        self.get("audit.settings", &q).await
    }

    /// `GET banners?platform=` (`web` or `mobile`).
    pub async fn admin_banners_list(&self, platform: &str) -> Result<Vec<Banner>> {
        let v: Value = self
            .get("banners", &Query::new().push("platform", platform))
            .await?;
        Ok(crate::models::lenient::vec_from_value(field::<Value>(
            v, "banners",
        )?))
    }
}
