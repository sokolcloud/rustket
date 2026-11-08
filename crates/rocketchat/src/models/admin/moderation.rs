//! Moderation console (docs/admin.md §18).

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::RcDate;
use crate::models::{Email, Message, lenient};

/// Room reference inside reports.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ReportRoom {
    #[serde(rename = "_id")]
    pub id: String,
    pub name: Option<String>,
    pub fname: Option<String>,
    pub t: Option<String>,
    #[serde(deserialize_with = "lenient::opt")]
    pub federated: Option<bool>,
}

/// Row of `moderation.reportsByUsers` (tab "Messages"): one per reported author.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ReportedMessagesRow {
    pub user_id: String,
    pub username: Option<String>,
    pub name: Option<String>,
    /// Text of the latest reported message.
    pub message: Option<String>,
    pub msg_id: Option<String>,
    #[serde(deserialize_with = "lenient::opt")]
    pub ts: Option<RcDate>,
    pub count: u64,
    #[serde(deserialize_with = "lenient::vec")]
    pub rooms: Vec<ReportRoom>,
    #[serde(deserialize_with = "lenient::vec")]
    pub room_ids: Vec<String>,
    #[serde(deserialize_with = "lenient::opt")]
    pub is_user_deleted: Option<bool>,
}

/// A user reference with `emails`/`createdAt` as used in reports.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ReportUser {
    #[serde(rename = "_id")]
    pub id: String,
    pub username: Option<String>,
    pub name: Option<String>,
    #[serde(deserialize_with = "lenient::vec")]
    pub emails: Vec<Email>,
    #[serde(deserialize_with = "lenient::opt")]
    pub created_at: Option<RcDate>,
}

/// Row of `moderation.userReports` (tab "Users").
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct UserReportsRow {
    pub reported_user: ReportUser,
    pub count: u64,
    #[serde(deserialize_with = "lenient::opt")]
    pub ts: Option<RcDate>,
}

/// Entry of `moderation.user.reportedMessages`: `_id` is the report id.
#[derive(Debug, Clone, PartialEq, Default, Deserialize)]
#[serde(default)]
pub struct ReportedMessage {
    #[serde(rename = "_id")]
    pub id: String,
    #[serde(deserialize_with = "lenient::opt")]
    pub message: Option<Message>,
    #[serde(deserialize_with = "lenient::opt")]
    pub room: Option<ReportRoom>,
    #[serde(deserialize_with = "lenient::opt")]
    pub ts: Option<RcDate>,
}

/// `moderation.user.reportedMessages`.
#[derive(Debug, Clone, PartialEq, Default, Deserialize)]
#[serde(default)]
pub struct UserReportedMessages {
    #[serde(deserialize_with = "lenient::opt")]
    pub user: Option<ReportUser>,
    #[serde(deserialize_with = "lenient::vec")]
    pub messages: Vec<ReportedMessage>,
    pub count: u64,
    pub offset: u64,
    pub total: u64,
}

/// A report (`IModerationReport`): of a message (`message` set) or of a user
/// (`reportedUser` set).
#[derive(Debug, Clone, PartialEq, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ModerationReport {
    #[serde(rename = "_id")]
    pub id: String,
    pub description: Option<String>,
    #[serde(deserialize_with = "lenient::opt")]
    pub reported_by: Option<ReportUser>,
    #[serde(deserialize_with = "lenient::opt")]
    pub reported_user: Option<ReportUser>,
    #[serde(deserialize_with = "lenient::opt")]
    pub message: Option<Message>,
    #[serde(deserialize_with = "lenient::opt")]
    pub room: Option<ReportRoom>,
    #[serde(deserialize_with = "lenient::opt")]
    pub ts: Option<RcDate>,
    #[serde(rename = "_hidden", deserialize_with = "lenient::opt")]
    pub hidden: Option<bool>,
    pub moderation_info: Option<Value>,
}

/// `moderation.user.reportsByUserId`.
#[derive(Debug, Clone, PartialEq, Default, Deserialize)]
#[serde(default)]
pub struct UserReportsByUser {
    #[serde(deserialize_with = "lenient::opt")]
    pub user: Option<ReportUser>,
    #[serde(deserialize_with = "lenient::vec")]
    pub reports: Vec<ModerationReport>,
    pub count: u64,
    pub offset: u64,
    pub total: u64,
}
