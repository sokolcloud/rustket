//! Unread main-timeline messages per room ("All unreads", Activity's
//! mentions) and thread mentions per room, in memory only. Pages come from
//! `rooms.history` with the subscription's `ls` as `lastSeen`; live
//! messages of rooms with a feed are merged in, deletions and hidden
//! messages removed, and a feed drops what its room's `ls` passed (read
//! here, on another device or by the open room).

use std::collections::{HashMap, HashSet};

use rocketchat::RcDate;
use rocketchat::models::{History, Message};

/// Messages per history page.
pub const FEED_PAGE: u32 = 30;
/// Messages kept per room; older unread ones are left to the room itself.
pub const FEED_MAX_PER_ROOM: usize = 300;
/// Feeds kept; the least recently used ones go beyond that (feeds the open
/// view wants are never evicted, so more may stay while it wants more).
pub const FEED_MAX_ROOMS: usize = 200;
/// Thread mentions kept per room (the newest per thread).
pub const THREAD_MENTIONS_PER_ROOM: usize = 50;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum FeedState {
    /// Not loaded (or to load again).
    #[default]
    Idle,
    Loading,
    Loaded,
    /// Given up; the text is the error.
    Failed(String),
}

/// One room's unread messages.
#[derive(Debug, Clone, Default)]
pub struct Feed {
    /// Ascending by `(ts, _id)`; only messages after `since`.
    messages: Vec<Message>,
    /// The subscription's `ls` the feed follows.
    since: Option<RcDate>,
    /// Oldest loaded `ts` (cursor of the next earlier page).
    oldest: Option<RcDate>,
    /// Unread messages older than the loaded ones (`unreadNotLoaded`).
    earlier: u64,
    state: FeedState,
    loading_earlier: bool,
    /// Live updates may have been missed (disconnected): load again.
    stale: bool,
    /// Last use, for eviction.
    used: u64,
    /// The current first-page request.
    ticket: u64,
    /// The current earlier-page request.
    earlier_ticket: u64,
}

impl Feed {
    pub fn messages(&self) -> &[Message] {
        &self.messages
    }

    pub fn state(&self) -> &FeedState {
        &self.state
    }

    pub fn is_loaded(&self) -> bool {
        self.state == FeedState::Loaded
    }

    pub fn is_stale(&self) -> bool {
        self.stale
    }

    /// Unread messages older than the loaded ones.
    pub fn earlier(&self) -> u64 {
        self.earlier
    }

    /// An earlier page can be loaded (some exist and the cap is not reached).
    pub fn has_earlier(&self) -> bool {
        self.earlier > 0 && !self.is_full()
    }

    /// The cap is reached with older unread messages left: open the room.
    pub fn is_full(&self) -> bool {
        self.messages.len() >= FEED_MAX_PER_ROOM && self.earlier > 0
    }

    pub fn is_loading_earlier(&self) -> bool {
        self.loading_earlier
    }

    fn upsert(&mut self, m: Message) -> bool {
        if let Some(i) = self.messages.iter().position(|x| x.id == m.id) {
            let old = &self.messages[i];
            if matches!((m.updated_at, old.updated_at), (Some(n), Some(o)) if n < o) || *old == m {
                return false;
            }
            self.messages[i] = m;
            return true;
        }
        if !after(m.ts, self.since) {
            return false;
        }
        let at = {
            let key = (m.ts, m.id.as_str());
            self.messages
                .partition_point(|x| (x.ts, x.id.as_str()) < key)
        };
        self.messages.insert(at, m);
        true
    }

    /// Drops the oldest messages beyond the cap (they count as earlier).
    fn trim(&mut self) {
        let over = self.messages.len().saturating_sub(FEED_MAX_PER_ROOM);
        if over > 0 {
            self.messages.drain(..over);
            self.earlier += over as u64;
            self.oldest = self.messages.first().and_then(|m| m.ts);
        }
    }
}

/// `ts` is after `since` (no `since`: everything is).
fn after(ts: Option<RcDate>, since: Option<RcDate>) -> bool {
    match (ts, since) {
        (_, None) => true,
        (Some(t), Some(s)) => t > s,
        (None, Some(_)) => false,
    }
}

/// A message a feed keeps: main timeline (thread replies only when also
/// sent to the room), not hidden.
fn feed_message(m: &Message) -> bool {
    m.hidden != Some(true) && (m.tmid.is_none() || m.tshow == Some(true))
}

/// A history page to fetch for a feed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FeedReq {
    pub rid: String,
    /// Inclusive upper bound (`HistoryOptions::before`); `None`: newest.
    pub before: Option<RcDate>,
    /// The subscription's `ls` (`HistoryOptions::last_seen`).
    pub last_seen: Option<RcDate>,
    ticket: u64,
}

impl FeedReq {
    pub fn is_earlier(&self) -> bool {
        self.before.is_some()
    }
}

/// Thread mentions of one room.
#[derive(Debug, Clone, Default)]
struct ThreadMentions {
    /// The newest mention per thread, newest first.
    messages: Vec<Message>,
    state: FeedState,
    /// Live updates may have been missed (disconnected): ask again, the
    /// loaded ones still shown meanwhile.
    stale: bool,
}

impl ThreadMentions {
    fn upsert(&mut self, m: Message) -> bool {
        let Some(tmid) = m.tmid.as_deref() else {
            return false;
        };
        if let Some(i) = self
            .messages
            .iter()
            .position(|x| x.tmid.as_deref() == Some(tmid))
        {
            let old = &self.messages[i];
            if old.id != m.id && old.ts >= m.ts {
                return false;
            }
            if *old == m {
                return false;
            }
            self.messages.remove(i);
        }
        let at = self.messages.partition_point(|x| x.ts > m.ts);
        self.messages.insert(at, m);
        self.messages.truncate(THREAD_MENTIONS_PER_ROOM);
        true
    }
}

#[derive(Debug, Clone, Default)]
pub struct FeedStore {
    feeds: HashMap<String, Feed>,
    mentions: HashMap<String, ThreadMentions>,
    /// Rooms the open view wants: never evicted.
    kept: HashSet<String>,
    clock: u64,
    tickets: u64,
}

impl FeedStore {
    pub fn feed(&self, rid: &str) -> Option<&Feed> {
        self.feeds.get(rid)
    }

    /// Rooms with a feed or thread mentions.
    pub fn rooms(&self) -> impl Iterator<Item = &String> + '_ {
        self.feeds.keys().chain(self.mentions.keys())
    }

    /// The rooms the open view wants (they are not evicted).
    pub fn keep<'a>(&mut self, rids: impl IntoIterator<Item = &'a String>) {
        self.kept.clear();
        self.kept.extend(rids.into_iter().cloned());
    }

    /// Starts a page: the newest one (`earlier` false: a new feed, or one
    /// loaded again) or the next earlier one of a loaded feed. `ls`: the
    /// subscription's. `None` when there is nothing to start.
    pub fn begin(&mut self, rid: &str, earlier: bool, ls: Option<RcDate>) -> Option<FeedReq> {
        self.clock += 1;
        self.tickets += 1;
        let (clock, ticket) = (self.clock, self.tickets);
        if earlier {
            let f = self.feeds.get_mut(rid)?;
            if !f.is_loaded() || !f.has_earlier() || f.loading_earlier {
                return None;
            }
            f.loading_earlier = true;
            f.earlier_ticket = ticket;
            f.used = clock;
            return Some(FeedReq {
                rid: rid.to_owned(),
                before: f.oldest,
                last_seen: ls,
                ticket,
            });
        }
        if !self.feeds.contains_key(rid) {
            self.evict();
        }
        let f = self.feeds.entry(rid.to_owned()).or_default();
        if f.state == FeedState::Loading {
            return None;
        }
        f.state = FeedState::Loading;
        f.loading_earlier = false;
        f.ticket = ticket;
        f.used = clock;
        if after(ls, f.since) {
            f.since = ls;
            f.messages.retain(|m| after(m.ts, ls));
        }
        Some(FeedReq {
            rid: rid.to_owned(),
            before: None,
            last_seen: ls,
            ticket,
        })
    }

    /// Drops the least recently used feeds beyond the cap that are not
    /// loading or wanted.
    fn evict(&mut self) {
        while self.feeds.len() >= FEED_MAX_ROOMS {
            let Some(rid) = self
                .feeds
                .iter()
                .filter(|(r, f)| {
                    f.state != FeedState::Loading && !f.loading_earlier && !self.kept.contains(*r)
                })
                .min_by_key(|(_, f)| f.used)
                .map(|(r, _)| r.clone())
            else {
                return;
            };
            self.feeds.remove(&rid);
        }
    }

    /// A page ended. `Err(None)`: it will be tried again (the feed waits
    /// as not loaded); `Err(Some(e))`: given up. `ls`: the subscription's
    /// now. Returns whether the feed changed.
    pub fn finish(
        &mut self,
        req: &FeedReq,
        res: Result<History, Option<String>>,
        ls: Option<RcDate>,
    ) -> bool {
        let Some(f) = self.feeds.get_mut(&req.rid) else {
            return false;
        };
        let current = if req.is_earlier() {
            f.loading_earlier && f.earlier_ticket == req.ticket
        } else {
            f.state == FeedState::Loading && f.ticket == req.ticket
        };
        if !current {
            return false;
        }
        let h = match res {
            Ok(h) => h,
            Err(e) => {
                if req.is_earlier() {
                    f.loading_earlier = false;
                } else {
                    f.state = match e {
                        Some(e) => FeedState::Failed(e),
                        None => FeedState::Idle,
                    };
                }
                return true;
            }
        };
        if after(ls, f.since) {
            f.since = ls;
            f.messages.retain(|m| after(m.ts, ls));
        }
        let page_oldest = h.messages.iter().filter_map(|m| m.ts).min();
        let end =
            (h.raw_len as u32) < FEED_PAGE || page_oldest.is_none_or(|t| !after(Some(t), f.since));
        for m in h.messages.into_iter().filter(feed_message) {
            f.upsert(m);
        }
        f.oldest = [f.oldest, page_oldest].into_iter().flatten().min();
        f.earlier = if end {
            0
        } else {
            h.unread_not_loaded.unwrap_or(0)
        };
        if req.is_earlier() {
            f.loading_earlier = false;
        } else {
            f.state = FeedState::Loaded;
            f.stale = false;
        }
        f.trim();
        true
    }

    /// Forgets a failure so the feed loads again.
    pub fn reset(&mut self, rid: &str) -> bool {
        match self.feeds.get_mut(rid) {
            Some(f) if matches!(f.state, FeedState::Failed(_)) => {
                f.state = FeedState::Idle;
                true
            }
            _ => false,
        }
    }

    /// A message from the stream (or any other copy): merged into its
    /// room's feed, or into its thread mentions when it mentions the user
    /// (`me`: username and id). Hidden messages leave both.
    pub fn apply(&mut self, m: &Message, me: Option<(&str, &str)>) -> bool {
        if m.hidden == Some(true) {
            return self.remove(&m.rid, &m.id);
        }
        let mut changed = false;
        if m.tmid.is_some()
            && let Some(tm) = self.mentions.get_mut(&m.rid)
            && me.is_some_and(|(u, id)| super::mentions_me(m, u, id))
        {
            changed |= tm.upsert(m.clone());
        }
        if feed_message(m)
            && let Some(f) = self.feeds.get_mut(&m.rid)
            && matches!(f.state, FeedState::Loaded | FeedState::Loading)
            && f.upsert(m.clone())
        {
            f.trim();
            changed = true;
        }
        changed
    }

    /// A deleted (or hidden) message.
    pub fn remove(&mut self, rid: &str, id: &str) -> bool {
        let mut changed = false;
        if let Some(f) = self.feeds.get_mut(rid) {
            let before = f.messages.len();
            f.messages.retain(|m| m.id != id);
            changed |= f.messages.len() != before;
        }
        if let Some(tm) = self.mentions.get_mut(rid) {
            let before = tm.messages.len();
            tm.messages.retain(|m| m.id != id);
            changed |= tm.messages.len() != before;
        }
        changed
    }

    /// The room's `ls` moved: messages up to it are read.
    pub fn prune(&mut self, rid: &str, ls: Option<RcDate>) -> bool {
        let Some(f) = self.feeds.get_mut(rid) else {
            return false;
        };
        if ls.is_none() || !after(ls, f.since) {
            return false;
        }
        f.since = ls;
        f.messages.retain(|m| after(m.ts, ls));
        if f.oldest.is_some_and(|o| !after(Some(o), ls)) || f.messages.is_empty() {
            f.earlier = 0;
        }
        true
    }

    /// Live updates were missed: every feed loads again when wanted.
    pub fn mark_all_stale(&mut self) -> bool {
        let mut changed = false;
        for f in self.feeds.values_mut() {
            changed |= !f.stale;
            f.stale = true;
        }
        // Thread mentions keep what was loaded (their fetches are few per
        // session: once they run out the old ones still show).
        for tm in self.mentions.values_mut() {
            changed |= !tm.stale;
            tm.stale = true;
        }
        changed
    }

    /// The views closed a while ago: the room feeds go (they would keep
    /// absorbing live messages nobody sees; opening a view loads them
    /// again). Thread mentions stay: their fetches are few per session.
    pub fn release(&mut self) -> bool {
        self.kept.clear();
        let changed = !self.feeds.is_empty();
        self.feeds.clear();
        changed
    }

    /// The room is gone (left, removed).
    pub fn drop_room(&mut self, rid: &str) -> bool {
        let a = self.feeds.remove(rid).is_some();
        let b = self.mentions.remove(rid).is_some();
        a || b
    }

    /// Rewrites the room's feed and thread mentions in place (decryption,
    /// renamed users).
    pub fn rewrite(&mut self, rid: &str, f: &mut dyn FnMut(&mut Message) -> bool) -> bool {
        let mut changed = false;
        if let Some(feed) = self.feeds.get_mut(rid) {
            for m in &mut feed.messages {
                changed |= f(m);
            }
        }
        if let Some(tm) = self.mentions.get_mut(rid) {
            for m in &mut tm.messages {
                changed |= f(m);
            }
        }
        changed
    }

    // --- thread mentions -----------------------------------------------------------

    /// Whether the room's thread mentions are to be asked for: never
    /// asked, to ask again, or loaded before live updates were missed.
    pub fn mentions_due(&self, rid: &str) -> bool {
        self.mentions.get(rid).is_none_or(|tm| match tm.state {
            FeedState::Idle => true,
            FeedState::Loaded => tm.stale,
            FeedState::Loading | FeedState::Failed(_) => false,
        })
    }

    /// The newest loaded mention in thread `tmid` of room `rid`.
    pub fn thread_mention(&self, rid: &str, tmid: &str) -> Option<&Message> {
        self.mentions
            .get(rid)?
            .messages
            .iter()
            .find(|m| m.tmid.as_deref() == Some(tmid))
    }

    pub fn begin_mentions(&mut self, rid: &str) -> bool {
        let tm = self.mentions.entry(rid.to_owned()).or_default();
        if tm.state == FeedState::Loading {
            return false;
        }
        tm.state = FeedState::Loading;
        true
    }

    /// `chat.getMentionedMessages` ended: keeps the newest thread reply per
    /// thread. `Err(None)`: asked for again later.
    pub fn finish_mentions(
        &mut self,
        rid: &str,
        res: Result<Vec<Message>, Option<String>>,
    ) -> bool {
        let Some(tm) = self.mentions.get_mut(rid) else {
            return false;
        };
        match res {
            Ok(list) => {
                for m in list.into_iter().filter(|m| m.hidden != Some(true)) {
                    tm.upsert(m);
                }
                tm.state = FeedState::Loaded;
                tm.stale = false;
            }
            Err(None) => {
                if tm.messages.is_empty() {
                    self.mentions.remove(rid);
                } else {
                    tm.state = FeedState::Idle;
                }
            }
            Err(Some(e)) if tm.messages.is_empty() => tm.state = FeedState::Failed(e),
            // A reload given up: what was loaded stays.
            Err(Some(_)) => {
                tm.state = FeedState::Loaded;
                tm.stale = false;
            }
        }
        true
    }
}
