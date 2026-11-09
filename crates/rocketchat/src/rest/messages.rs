//! Messages, threads, reactions, pins, stars, search.

use std::collections::HashSet;

use serde_json::{Value, json};

use super::{Query, RestClient, field, paginated};
use crate::RcDate;
use crate::error::Result;
use crate::models::{History, Message, OutgoingMessage, Paginated, SyncMessages};

/// Options of [`RestClient::load_history_with`].
#[derive(Debug, Clone)]
pub struct HistoryOptions {
    /// Load messages with `ts <= before` (`None` = newest). Inclusive, so pass the
    /// oldest loaded `ts` and drop already-known `_id`s from the result.
    pub before: Option<RcDate>,
    pub limit: u32,
    /// The subscription's `ls`, so the server reports `firstUnread`/`unreadNotLoaded`.
    pub last_seen: Option<RcDate>,
    /// Include thread replies that were not also sent to the room (`tshow`).
    /// Default `false`, like the web client's main timeline (user preference
    /// `showThreadsInMainChannel`, default off); replies carry `tmid`.
    pub show_thread_messages: bool,
}

impl Default for HistoryOptions {
    fn default() -> Self {
        Self {
            before: None,
            limit: 50,
            last_seen: None,
            show_thread_messages: false,
        }
    }
}

impl RestClient {
    /// Room history, newest first, with unread info. Works for every room type.
    /// `before` is inclusive: dedup the boundary message(s) by `_id`.
    pub async fn load_history(
        &self,
        rid: &str,
        before: Option<RcDate>,
        limit: u32,
        last_seen: Option<RcDate>,
    ) -> Result<History> {
        self.load_history_with(
            rid,
            &HistoryOptions {
                before,
                limit,
                last_seen,
                ..Default::default()
            },
        )
        .await
    }

    /// Uses `GET rooms.history` (8.9+); servers without it get the `loadHistory`
    /// method, which 9.0 removed.
    pub async fn load_history_with(&self, rid: &str, opts: &HistoryOptions) -> Result<History> {
        // Both bounds are exclusive on the server; +1 ms makes `before` inclusive so
        // messages sharing the boundary timestamp are not skipped.
        let end = opts
            .before
            .and_then(|d| RcDate::from_millis(d.timestamp_millis() + 1));
        let q = Query::new()
            .push("roomId", rid)
            .push("count", opts.limit.max(1))
            .push("showThreadMessages", opts.show_thread_messages)
            .opt("previous", end.map(|d| d.timestamp_millis()))
            .opt("lastSeen", opts.last_seen.map(|d| d.to_iso()));
        match self.get::<History>("rooms.history", &q).await {
            // A missing route is a plain-text 404; a missing room is a JSON API error.
            Err(crate::Error::Status { status: 404, .. }) => {}
            r => return r,
        }
        let params = json!([
            rid,
            end.map(|d| d.ddp()),
            opts.limit,
            opts.last_seen.map(|d| d.ddp()),
            opts.show_thread_messages,
        ]);
        let v: Value = self.method_call("loadHistory", params).await?;
        match v {
            Value::Null => Ok(History::default()),
            // `false`: room not found or no access.
            Value::Bool(false) => Err(crate::Error::Api {
                status: 200,
                error: Some("loadHistory returned false".into()),
                error_type: Some("error-not-allowed".into()),
                message: Some("room not found or not allowed".into()),
                details: None,
            }),
            v => Ok(serde_json::from_value(v)?),
        }
    }

    /// Messages updated/deleted since `last_update` (delta sync after reconnect).
    pub async fn chat_sync_messages(&self, rid: &str, last_update: RcDate) -> Result<SyncMessages> {
        let v: Value = self
            .get(
                "chat.syncMessages",
                &Query::new()
                    .push("roomId", rid)
                    .push("lastUpdate", last_update.to_iso()),
            )
            .await?;
        field(v, "result")
    }

    /// `chat.sendMessage`; set `message.id` to match the realtime echo.
    pub async fn send_message(&self, message: &OutgoingMessage) -> Result<Message> {
        let v: Value = self
            .post("chat.sendMessage", &json!({ "message": message }))
            .await?;
        field(v, "message")
    }

    pub async fn update_message(&self, rid: &str, msg_id: &str, text: &str) -> Result<Message> {
        let v: Value = self
            .post(
                "chat.update",
                &json!({ "roomId": rid, "msgId": msg_id, "text": text }),
            )
            .await?;
        field(v, "message")
    }

    pub async fn delete_message(&self, rid: &str, msg_id: &str) -> Result<()> {
        self.post_ok("chat.delete", &json!({ "roomId": rid, "msgId": msg_id }))
            .await
    }

    /// Toggle (`should_react = None`) or set a reaction; `emoji` like `:thumbsup:`.
    pub async fn react(&self, msg_id: &str, emoji: &str, should_react: Option<bool>) -> Result<()> {
        let mut body = json!({ "messageId": msg_id, "emoji": emoji });
        if let Some(s) = should_react {
            body["shouldReact"] = Value::Bool(s);
        }
        self.post_ok("chat.react", &body).await
    }

    /// Pin a message; returns the `message_pinned` system message.
    pub async fn pin_message(&self, msg_id: &str) -> Result<Message> {
        let v: Value = self
            .post("chat.pinMessage", &json!({ "messageId": msg_id }))
            .await?;
        field(v, "message")
    }

    pub async fn unpin_message(&self, msg_id: &str) -> Result<()> {
        self.post_ok("chat.unPinMessage", &json!({ "messageId": msg_id }))
            .await
    }

    pub async fn star_message(&self, msg_id: &str) -> Result<()> {
        self.post_ok("chat.starMessage", &json!({ "messageId": msg_id }))
            .await
    }

    pub async fn unstar_message(&self, msg_id: &str) -> Result<()> {
        self.post_ok("chat.unStarMessage", &json!({ "messageId": msg_id }))
            .await
    }

    pub async fn get_message(&self, msg_id: &str) -> Result<Message> {
        let v: Value = self
            .get("chat.getMessage", &Query::new().push("msgId", msg_id))
            .await?;
        field(v, "message")
    }

    /// Replies of a thread (oldest first). Its offsets do not pair with
    /// `rustket-core`'s `begin_load_thread`, which counts from the newest
    /// reply: use [`Self::thread_messages_latest`] for that.
    pub async fn thread_messages(
        &self,
        tmid: &str,
        offset: u64,
        count: u64,
    ) -> Result<Paginated<Message>> {
        let v: Value = self
            .get(
                "chat.getThreadMessages",
                &Query::new()
                    .push("tmid", tmid)
                    .push("offset", offset)
                    .push("count", count)
                    .push("sort", r#"{"ts":1}"#),
            )
            .await?;
        paginated(v, "messages")
    }

    /// Replies of a thread, newest first (`offset` counts from the newest
    /// reply), like the web client's thread view.
    pub async fn thread_messages_latest(
        &self,
        tmid: &str,
        offset: u64,
        count: u64,
    ) -> Result<Paginated<Message>> {
        let v: Value = self
            .get(
                "chat.getThreadMessages",
                &Query::new()
                    .push("tmid", tmid)
                    .push("offset", offset)
                    .push("count", count)
                    .push("sort", r#"{"ts":-1}"#),
            )
            .await?;
        paginated(v, "messages")
    }

    /// Marks thread `tmid` read for the current user (the subscription's
    /// `tunread` loses it).
    pub async fn read_thread(&self, tmid: &str) -> Result<()> {
        self.post_ok("chat.readThread", &json!({ "tmid": tmid }))
            .await
    }

    /// Thread parents of a room; `kind` is `all`, `following` or `unread`.
    pub async fn threads_list(
        &self,
        rid: &str,
        kind: Option<&str>,
        text: Option<&str>,
        offset: u64,
        count: u64,
    ) -> Result<Paginated<Message>> {
        let v: Value = self
            .get(
                "chat.getThreadsList",
                &Query::new()
                    .push("rid", rid)
                    .opt("type", kind)
                    .opt("text", text)
                    .push("offset", offset)
                    .push("count", count),
            )
            .await?;
        paginated(v, "threads")
    }

    pub async fn follow_message(&self, msg_id: &str) -> Result<()> {
        self.post_ok("chat.followMessage", &json!({ "mid": msg_id }))
            .await
    }

    pub async fn unfollow_message(&self, msg_id: &str) -> Result<()> {
        self.post_ok("chat.unfollowMessage", &json!({ "mid": msg_id }))
            .await
    }

    /// Full-text search in a room.
    pub async fn search_messages(
        &self,
        rid: &str,
        text: &str,
        offset: u64,
        count: u64,
    ) -> Result<Vec<Message>> {
        let v: Value = self
            .get(
                "chat.search",
                &Query::new()
                    .push("roomId", rid)
                    .push("searchText", text)
                    .push("offset", offset)
                    .push("count", count),
            )
            .await?;
        field(v, "messages")
    }

    async fn room_message_list(
        &self,
        endpoint: &str,
        rid: &str,
        offset: u64,
        count: u64,
    ) -> Result<Paginated<Message>> {
        let v: Value = self
            .get(
                endpoint,
                &Query::new()
                    .push("roomId", rid)
                    .push("offset", offset)
                    .push("count", count),
            )
            .await?;
        paginated(v, "messages")
    }

    pub async fn get_pinned(
        &self,
        rid: &str,
        offset: u64,
        count: u64,
    ) -> Result<Paginated<Message>> {
        self.room_message_list("chat.getPinnedMessages", rid, offset, count)
            .await
    }

    pub async fn get_starred(
        &self,
        rid: &str,
        offset: u64,
        count: u64,
    ) -> Result<Paginated<Message>> {
        self.room_message_list("chat.getStarredMessages", rid, offset, count)
            .await
    }

    pub async fn get_mentioned(
        &self,
        rid: &str,
        offset: u64,
        count: u64,
    ) -> Result<Paginated<Message>> {
        self.room_message_list("chat.getMentionedMessages", rid, offset, count)
            .await
    }
}

/// Pages backwards through a room's history without skipping or repeating messages.
///
/// `loadHistory` takes a timestamp cursor, so messages sharing the boundary
/// millisecond need care: the cursor asks inclusively, drops known `_id`s, decides
/// the end on the raw page length (invalid messages are skipped by the parser), and
/// if a whole page shares one millisecond it doubles the limit (up to `max_limit`)
/// before stepping past that millisecond as a last resort.
#[derive(Debug, Clone)]
pub struct HistoryCursor {
    rid: String,
    limit: u32,
    max_limit: u32,
    before: Option<RcDate>,
    seen: HashSet<String>,
    done: bool,
    show_thread_messages: bool,
}

impl HistoryCursor {
    /// Starts at the newest message.
    pub fn new(rid: impl Into<String>, limit: u32) -> Self {
        let limit = limit.max(1);
        Self {
            rid: rid.into(),
            limit,
            max_limit: limit.saturating_mul(8).max(400),
            before: None,
            seen: HashSet::new(),
            done: false,
            show_thread_messages: false,
        }
    }

    /// Also page through thread replies (see [`HistoryOptions::show_thread_messages`]).
    pub fn with_thread_messages(mut self, show: bool) -> Self {
        self.show_thread_messages = show;
        self
    }

    /// Continues below already loaded messages (`before` = oldest known `ts`).
    pub fn resume(
        rid: impl Into<String>,
        limit: u32,
        before: RcDate,
        known_ids: impl IntoIterator<Item = String>,
    ) -> Self {
        let mut c = Self::new(rid, limit);
        c.before = Some(before);
        c.seen.extend(known_ids);
        c
    }

    /// No older messages are left.
    pub fn is_done(&self) -> bool {
        self.done
    }

    /// The next batch of older, not yet returned messages (newest first). An empty
    /// result means the start of the room was reached.
    pub async fn next_page(&mut self, rest: &RestClient) -> Result<Vec<Message>> {
        let mut limit = self.limit;
        while !self.done {
            let page = rest
                .load_history_with(
                    &self.rid,
                    &HistoryOptions {
                        before: self.before,
                        limit,
                        last_seen: None,
                        show_thread_messages: self.show_thread_messages,
                    },
                )
                .await?;
            let full = page.raw_len >= limit as usize;
            let oldest = page.messages.iter().filter_map(|m| m.ts).min();
            let new: Vec<Message> = page
                .messages
                .into_iter()
                .filter(|m| self.seen.insert(m.id.clone()))
                .collect();
            if !full {
                self.done = true;
            }
            if !new.is_empty() {
                if oldest.is_some() {
                    self.before = oldest;
                }
                return Ok(new);
            }
            if self.done {
                break;
            }
            if limit < self.max_limit {
                limit = limit.saturating_mul(2).min(self.max_limit);
                continue;
            }
            // More than `max_limit` messages in one millisecond: step past it.
            let Some(b) = self.before else {
                self.done = true;
                break;
            };
            tracing::warn!(rid = %self.rid, at = %b, "history: too many messages in one millisecond, skipping past it");
            self.before = RcDate::from_millis(b.timestamp_millis() - 1);
            limit = self.limit;
        }
        Ok(Vec::new())
    }
}
