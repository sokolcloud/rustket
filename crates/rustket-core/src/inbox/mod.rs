//! "All unreads", "Catch up" and "Activity": views of what is unread across
//! the rooms of one server, derived from the subscriptions (`ls`, `alert`,
//! `unread`, mentions, `tunread*`), which already sync between devices.
//! Nothing here is stored on the server; feeds ([`feeds`]) are memory only.
//!
//! - [`unread_rooms`]: the rooms "All unreads" lists, loudest first;
//! - [`activity_items`]: mentions and unread threads ("Activity");
//! - [`counts`]: the sidebar rows' counters;
//! - [`catch_up`]: the one-room-at-a-time reducer;
//! - [`load`]: the paced planner of feed and mention fetches.
//!
//! In channels and groups `unread` counts mentions only by default
//! (`Unread_Count = user_and_group_mentions_only`): plain unread messages
//! only set `alert`, so `alert` decides, and message counts shown in the
//! views come from the loaded feeds.

pub mod catch_up;
pub mod feeds;
pub mod load;

#[cfg(test)]
mod tests;

use std::collections::{BTreeMap, HashSet};

use rocketchat::RcDate;
use rocketchat::models::{Message, RoomType};

use crate::room_list::{self, Badge, RoomKind, ThreadListItem};
use crate::workspace::{RoomEntry, Workspace};
use feeds::FeedState;

/// How loud a room of "All unreads" is (the list's order).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Tier {
    /// A direct mention, or a direct message.
    Mention,
    /// `@all` / `@here` (not muted).
    GroupMention,
    /// Plain unread messages.
    Unread,
    /// A room with notifications disabled (listed on request only).
    Muted,
}

/// A room of "All unreads".
#[derive(Debug, Clone, PartialEq)]
pub struct UnreadRoom {
    pub rid: String,
    pub tier: Tier,
    /// The subscription's `ls` (read up to).
    pub ls: Option<RcDate>,
    /// Last activity (sort key; a newer one un-skips the room).
    pub lm: Option<RcDate>,
    pub badge: Badge,
}

/// Options of "All unreads".
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct InboxPrefs {
    /// List rooms with notifications disabled too.
    pub include_muted: bool,
}

/// Counters of the sidebar rows.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct InboxCounts {
    /// Rooms "All unreads" lists (muted ones aside).
    pub unread_rooms: u64,
    /// Direct mentions outside DMs, thread mentions included.
    pub mentions: u64,
    pub group_mentions: u64,
    /// Unread threads.
    pub threads: u64,
}

/// Rooms skipped in "All unreads" for this session: rid → the room's last
/// activity when it was skipped (newer activity brings the room back).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Skipped(BTreeMap<String, Option<RcDate>>);

impl Skipped {
    pub fn skip(&mut self, rid: &str, lm: Option<RcDate>) {
        self.0.insert(rid.to_owned(), lm);
    }

    /// Skipped and nothing newer arrived since.
    pub fn is_skipped(&self, rid: &str, lm: Option<RcDate>) -> bool {
        match (self.0.get(rid), lm) {
            (None, _) => false,
            (Some(Some(at)), Some(lm)) => lm <= *at,
            (Some(None), Some(_)) => false,
            (Some(_), None) => true,
        }
    }
}

/// A room whose rows a view of this module may show: listed by the
/// sidebar, not omnichannel.
fn shown(ws: &Workspace, e: &RoomEntry) -> bool {
    room_list::is_listed(ws, e.rid()) && room_list::room_kind(e) != RoomKind::Omnichannel
}

/// The room's newest main-timeline message (thread replies do not move it).
fn main_lm(e: &RoomEntry) -> Option<RcDate> {
    let r = e.room.as_ref()?;
    r.lm.or(r.last_message.as_ref().and_then(|m| m.ts))
}

/// The "All unreads" entry of room `e`, if it is listed there: unread
/// (`room_list::room_is_unread`) in the main timeline (a room with only
/// unread threads belongs to Activity), muted rooms only with
/// `include_muted`.
fn unread_room(ws: &Workspace, e: &RoomEntry, include_muted: bool) -> Option<UnreadRoom> {
    let s = e.sub.as_ref()?;
    if !shown(ws, e) || !room_list::room_is_unread(e) {
        return None;
    }
    let main_unread = s.unread > 0
        || (s.alert && !matches!((main_lm(e), s.ls), (Some(lm), Some(ls)) if lm <= ls));
    if !main_unread {
        return None;
    }
    let hide_mention = s.hide_mention_status == Some(true);
    let muted = s.disable_notifications == Some(true);
    let dm = e.room_type() == RoomType::Direct;
    let tier = if (s.user_mentions > 0 && !hide_mention) || (dm && !muted) {
        Tier::Mention
    } else if muted {
        Tier::Muted
    } else if s.group_mentions > 0 && !hide_mention && s.mute_group_mentions != Some(true) {
        Tier::GroupMention
    } else {
        Tier::Unread
    };
    if tier == Tier::Muted && !include_muted {
        return None;
    }
    Some(UnreadRoom {
        rid: e.rid().to_owned(),
        tier,
        ls: s.ls,
        lm: e.last_activity(),
        badge: room_list::badge(e),
    })
}

/// The rooms of "All unreads": tier, then newest activity first; rooms
/// skipped since their last activity are left out.
pub fn unread_rooms(ws: &Workspace, prefs: &InboxPrefs, skipped: &Skipped) -> Vec<UnreadRoom> {
    let mut out: Vec<UnreadRoom> = ws
        .rooms()
        .values()
        .filter_map(|e| unread_room(ws, e, prefs.include_muted))
        .filter(|r| !skipped.is_skipped(&r.rid, r.lm))
        .collect();
    out.sort_by(|a, b| {
        a.tier
            .cmp(&b.tier)
            .then_with(|| b.lm.cmp(&a.lm))
            .then_with(|| a.rid.cmp(&b.rid))
    });
    out
}

/// The sidebar rows' counters.
pub fn counts(ws: &Workspace) -> InboxCounts {
    let mut c = InboxCounts::default();
    for e in ws.rooms().values() {
        let Some(s) = e.sub.as_ref().filter(|_| shown(ws, e)) else {
            continue;
        };
        if unread_room(ws, e, false).is_some() {
            c.unread_rooms += 1;
        }
        c.threads += s
            .tunread
            .iter()
            .chain(&s.tunread_user)
            .chain(&s.tunread_group)
            .flatten()
            .collect::<HashSet<_>>()
            .into_iter()
            .filter(|t| room_list::thread_unread(s, t).is_some())
            .count() as u64;
        if s.hide_mention_status == Some(true) {
            continue;
        }
        c.mentions += s.tunread_user.as_ref().map_or(0, |v| v.len() as u64);
        if e.room_type() != RoomType::Direct {
            c.mentions += s.user_mentions;
            if s.mute_group_mentions != Some(true) {
                c.group_mentions += s.group_mentions;
            }
        }
    }
    c
}

/// `m` mentions the user (`username` / user id `uid`) by name; `@all` and
/// `@here` do not count.
pub fn mentions_me(m: &Message, username: &str, uid: &str) -> bool {
    m.mentions
        .iter()
        .flatten()
        .any(|x| x.username.as_deref() == Some(username) || (!uid.is_empty() && x.id == uid))
}

/// `m` mentions `@all` or `@here`.
pub fn is_group_mention(m: &Message) -> bool {
    m.mentions.iter().flatten().any(|x| {
        matches!(x.id.as_str(), "all" | "here")
            || matches!(x.username.as_deref(), Some("all" | "here"))
    })
}

/// Which part of Activity is shown.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ActivityFilter {
    #[default]
    All,
    Mentions,
    Threads,
}

impl ActivityFilter {
    pub const ALL: [Self; 3] = [Self::All, Self::Mentions, Self::Threads];
}

/// A row of Activity.
#[derive(Debug, Clone, PartialEq)]
pub enum ActivityItem {
    /// A mention in the main timeline, loaded in the room's feed.
    Mention {
        rid: String,
        mid: String,
        /// `@all` / `@here` rather than the user's name.
        group: bool,
        ts: Option<RcDate>,
    },
    /// A room with unread mentions none of which is loaded.
    RoomMentions {
        rid: String,
        group: bool,
        pending: PendingMentions,
    },
    /// A mention inside a thread (`chat.getMentionedMessages`).
    ThreadMention {
        rid: String,
        tmid: String,
        mid: String,
        ts: Option<RcDate>,
    },
    /// A thread with a direct mention (`tunreadUser`) whose message is not
    /// loaded (not fetched yet, or the fetches ran out): the thread's row
    /// stands in among the mentions.
    MentionedThread(ThreadListItem),
    /// An unread thread.
    Thread(ThreadListItem),
}

/// Why a room's unread mentions are not listed one by one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PendingMentions {
    /// Its feed is on the way.
    Loading,
    /// Its feed was given up (the error); "Retry" loads it again.
    Failed(String),
    /// They are older than the loaded page.
    Older,
}

impl ActivityItem {
    pub fn rid(&self) -> &str {
        match self {
            Self::Mention { rid, .. }
            | Self::RoomMentions { rid, .. }
            | Self::ThreadMention { rid, .. } => rid,
            Self::MentionedThread(t) | Self::Thread(t) => &t.rid,
        }
    }

    pub fn is_mention(&self) -> bool {
        !matches!(self, Self::Thread(_))
    }
}

/// Rooms whose unread mentions Activity lists from their feed: shown,
/// not DMs (those are "All unreads"' first tier), with direct mentions or
/// group mentions that are not muted. `(rid, direct, group)`.
pub fn mention_rooms(ws: &Workspace) -> Vec<(String, bool, bool)> {
    let mut out: Vec<(String, bool, bool, Option<RcDate>)> = ws
        .rooms()
        .values()
        .filter(|e| shown(ws, e) && e.room_type() != RoomType::Direct)
        .filter_map(|e| {
            let s = e.sub.as_ref()?;
            if s.hide_mention_status == Some(true) {
                return None;
            }
            let direct = s.user_mentions > 0;
            let group = s.group_mentions > 0 && s.mute_group_mentions != Some(true);
            (direct || group).then(|| (s.rid.clone(), direct, group, e.last_activity()))
        })
        .collect();
    out.sort_by(|a, b| b.3.cmp(&a.3).then_with(|| a.0.cmp(&b.0)));
    out.into_iter().map(|(r, d, g, _)| (r, d, g)).collect()
}

/// Rooms with unread mentions inside threads (`tunreadUser`).
pub fn thread_mention_rooms(ws: &Workspace) -> Vec<String> {
    let mut out: Vec<(String, Option<RcDate>)> = ws
        .rooms()
        .values()
        .filter(|e| shown(ws, e))
        .filter_map(|e| {
            let s = e.sub.as_ref()?;
            (s.hide_mention_status != Some(true)
                && s.tunread_user.as_ref().is_some_and(|v| !v.is_empty()))
            .then(|| (s.rid.clone(), e.last_activity()))
        })
        .collect();
    out.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    out.into_iter().map(|(r, _)| r).collect()
}

/// Activity: the mentions (newest first; rooms whose mentions are not
/// loaded after them), then the unread threads (newest first). A thread
/// with a mention row is not listed again.
pub fn activity_items(
    ws: &Workspace,
    filter: ActivityFilter,
    use_real_names: bool,
) -> Vec<ActivityItem> {
    let (username, uid) = (
        ws.username().unwrap_or_default(),
        ws.user_id().unwrap_or_default(),
    );
    let feeds = ws.feeds();
    let mut mentions: Vec<ActivityItem> = Vec::new();
    let mut pending: Vec<ActivityItem> = Vec::new();
    let mut covered: HashSet<String> = HashSet::new();
    for (rid, direct, group) in mention_rooms(ws) {
        let ls = ws.subscription(&rid).and_then(|s| s.ls);
        let feed = feeds.feed(&rid);
        let loaded = feed.filter(|f| f.is_loaded());
        let mut found = false;
        for m in loaded.map(|f| f.messages()).unwrap_or_default() {
            if ls.is_some_and(|ls| m.ts.is_none_or(|t| t <= ls)) {
                continue;
            }
            let me = !username.is_empty() && mentions_me(m, username, uid);
            let all = group && is_group_mention(m);
            if me || all {
                found = true;
                mentions.push(ActivityItem::Mention {
                    rid: rid.clone(),
                    mid: m.id.clone(),
                    group: !me,
                    ts: m.ts,
                });
            }
        }
        if !found {
            let state = match feed.map(|f| f.state()) {
                Some(FeedState::Loaded) => PendingMentions::Older,
                Some(FeedState::Failed(e)) => PendingMentions::Failed(e.clone()),
                _ => PendingMentions::Loading,
            };
            pending.push(ActivityItem::RoomMentions {
                rid: rid.clone(),
                group: !direct,
                pending: state,
            });
        }
    }
    // Threads with a direct mention not loaded: their rows stand in.
    let mut mentioned: HashSet<String> = HashSet::new();
    for rid in thread_mention_rooms(ws) {
        let Some(s) = ws.subscription(&rid) else {
            continue;
        };
        for tmid in s.tunread_user.iter().flatten() {
            covered.insert(tmid.clone());
            match feeds.thread_mention(&rid, tmid) {
                Some(m) => mentions.push(ActivityItem::ThreadMention {
                    rid: rid.clone(),
                    tmid: tmid.clone(),
                    mid: m.id.clone(),
                    ts: m.ts,
                }),
                None => {
                    mentioned.insert(tmid.clone());
                }
            }
        }
    }
    let mut threads = Vec::new();
    for t in room_list::unread_thread_items(ws, use_real_names) {
        if mentioned.contains(&t.tmid) {
            mentions.push(ActivityItem::MentionedThread(t));
        } else if !covered.contains(&t.tmid) {
            threads.push(ActivityItem::Thread(t));
        }
    }
    let ts_of = |i: &ActivityItem| match i {
        ActivityItem::Mention { ts, .. } | ActivityItem::ThreadMention { ts, .. } => *ts,
        ActivityItem::MentionedThread(t) => t.tlm,
        _ => None,
    };
    mentions.sort_by_key(|a| std::cmp::Reverse(ts_of(a)));
    mentions.extend(pending);
    let mut out = Vec::new();
    if filter != ActivityFilter::Threads {
        out.extend(mentions);
    }
    if filter != ActivityFilter::Mentions {
        out.extend(threads);
    }
    out
}
