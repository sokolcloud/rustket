//! Moderation console (docs/admin.md §18). Reading needs
//! `view-moderation-console`, actions `manage-moderation-actions`.

use serde_json::{Value, json};

use super::{Page, paged};
use crate::RcDate;
use crate::error::Result;
use crate::models::Paginated;
use crate::models::admin::{
    ModerationReport, ReportedMessagesRow, UserReportedMessages, UserReportsByUser, UserReportsRow,
};
use crate::rest::{Query, RestClient, field};

/// Date range and search text of the report lists.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ModerationFilter {
    pub oldest: Option<RcDate>,
    pub latest: Option<RcDate>,
    pub selector: Option<String>,
}

impl ModerationFilter {
    fn apply(&self, q: Query) -> Query {
        q.opt("oldest", self.oldest.map(|d| d.to_iso()))
            .opt("latest", self.latest.map(|d| d.to_iso()))
            .opt(
                "selector",
                self.selector.as_deref().filter(|s| !s.is_empty()),
            )
    }
}

fn dismiss_body(key: &str, id: &str, reason: Option<&str>, action: Option<&str>) -> Value {
    let mut body = json!({ key: id });
    if let Some(r) = reason {
        body["reason"] = Value::String(r.to_owned());
    }
    if let Some(a) = action {
        body["action"] = Value::String(a.to_owned());
    }
    body
}

impl RestClient {
    /// Tab "Messages": one row per reported author.
    pub async fn moderation_reports_by_users(
        &self,
        filter: &ModerationFilter,
        page: &Page,
    ) -> Result<Paginated<ReportedMessagesRow>> {
        let v: Value = self
            .get(
                "moderation.reportsByUsers",
                &filter.apply(page.apply(Query::new())),
            )
            .await?;
        paged(v, "reports", page)
    }

    /// Tab "Users": reported users.
    pub async fn moderation_user_reports(
        &self,
        filter: &ModerationFilter,
        page: &Page,
    ) -> Result<Paginated<UserReportsRow>> {
        let v: Value = self
            .get(
                "moderation.userReports",
                &filter.apply(page.apply(Query::new())),
            )
            .await?;
        paged(v, "reports", page)
    }

    pub async fn moderation_user_reported_messages(
        &self,
        user_id: &str,
        selector: Option<&str>,
        page: &Page,
    ) -> Result<UserReportedMessages> {
        let q = page.apply(
            Query::new()
                .push("userId", user_id)
                .opt("selector", selector.filter(|s| !s.is_empty())),
        );
        self.get("moderation.user.reportedMessages", &q).await
    }

    pub async fn moderation_user_reports_by_user_id(
        &self,
        user_id: &str,
        page: &Page,
    ) -> Result<UserReportsByUser> {
        let q = page.apply(Query::new().push("userId", user_id));
        self.get("moderation.user.reportsByUserId", &q).await
    }

    /// Reports of one message.
    pub async fn moderation_message_reports(
        &self,
        msg_id: &str,
        page: &Page,
    ) -> Result<Paginated<ModerationReport>> {
        let v: Value = self
            .get(
                "moderation.reports",
                &page.apply(Query::new().push("msgId", msg_id)),
            )
            .await?;
        paged(v, "reports", page)
    }

    pub async fn moderation_report_info(
        &self,
        report_id: &str,
    ) -> Result<Option<ModerationReport>> {
        let v: Value = self
            .get(
                "moderation.reportInfo",
                &Query::new().push("reportId", report_id),
            )
            .await?;
        Ok(field::<Option<ModerationReport>>(v, "report").unwrap_or(None))
    }

    /// Deletes a reported message as its author (`chat.delete` with
    /// `asUser: true`, like the web's moderation console).
    pub async fn moderation_delete_message(&self, rid: &str, msg_id: &str) -> Result<()> {
        self.post_ok(
            "chat.delete",
            &json!({ "roomId": rid, "msgId": msg_id, "asUser": true }),
        )
        .await
    }

    /// Dismisses the reports of one message.
    pub async fn moderation_dismiss_message_reports(
        &self,
        msg_id: &str,
        reason: Option<&str>,
        action: Option<&str>,
    ) -> Result<()> {
        self.post_ok(
            "moderation.dismissReports",
            &dismiss_body("msgId", msg_id, reason, action),
        )
        .await
    }

    /// Dismisses the message reports of all messages of a user.
    pub async fn moderation_dismiss_reports_of_user(
        &self,
        user_id: &str,
        reason: Option<&str>,
        action: Option<&str>,
    ) -> Result<()> {
        self.post_ok(
            "moderation.dismissReports",
            &dismiss_body("userId", user_id, reason, action),
        )
        .await
    }

    /// Dismisses the reports about a user (tab "Users").
    pub async fn moderation_dismiss_user_reports(
        &self,
        user_id: &str,
        reason: Option<&str>,
        action: Option<&str>,
    ) -> Result<()> {
        self.post_ok(
            "moderation.dismissUserReports",
            &dismiss_body("userId", user_id, reason, action),
        )
        .await
    }

    /// Deletes every reported message of a user.
    pub async fn moderation_delete_reported_messages(
        &self,
        user_id: &str,
        reason: Option<&str>,
    ) -> Result<()> {
        self.post_ok(
            "moderation.user.deleteReportedMessages",
            &dismiss_body("userId", user_id, reason, None),
        )
        .await
    }

    /// Reports a user to the moderators.
    pub async fn moderation_report_user(&self, user_id: &str, description: &str) -> Result<()> {
        self.post_ok(
            "moderation.reportUser",
            &json!({ "userId": user_id, "description": description }),
        )
        .await
    }

    /// Reports a message to the moderators (`chat.reportMessage`).
    pub async fn moderation_report_message(&self, msg_id: &str, description: &str) -> Result<()> {
        self.post_ok(
            "chat.reportMessage",
            &json!({ "messageId": msg_id, "description": description }),
        )
        .await
    }
}
