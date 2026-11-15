//! "All unreads", "Catch up" and "Activity" (`rustket_core::inbox`): views
//! shown in place of the room area (`workspace::MainView::Inbox`), like the
//! omnichannel contact center. Strings: `locales/inbox.yml`.
//!
//! The component holds what the open view shows (mode, catch-up order,
//! filter, rooms asked for) and turns user input into [`Out`] requests;
//! the app wiring (`app/inbox.rs`) runs the loader, the read queue and the
//! room navigation. Feeds live in the workspace, so live updates, E2E
//! decryption and renames reach them like any timeline.

pub mod view;

use std::collections::{BTreeSet, HashMap};
use std::time::{Duration, Instant};

use rocketchat::models::Message as RcMessage;
use rustket_core::Workspace;
use rustket_core::inbox::catch_up::{Advance, CatchUp};
use rustket_core::inbox::load::{LoadState, Wanted};
use rustket_core::inbox::{self, ActivityFilter, ActivityItem, InboxPrefs, Skipped, UnreadRoom};
use rustket_core::reads::ReadReq;

use crate::room::timeline::{Viewer, ViewerMsg};
use crate::room::{ComposeIntent, Effect};

/// Which view is open.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum InboxKind {
    Unreads,
    CatchUp,
    Activity,
}

/// Rooms "All unreads" shows (and loads) at first and per "Load more".
pub const ROOMS_PAGE: usize = 30;
/// Rooms catch-up loads ahead (the current one included).
pub const PREFETCH: usize = 3;
/// A room marked read stays hidden this long after the server confirmed,
/// waiting for its subscription update.
const MARK_GRACE: Duration = Duration::from_secs(5);

/// Per account, kept while the views are closed (for the session):
/// skipped rooms and the loader's pacing.
#[derive(Debug, Default)]
pub struct InboxState {
    pub skipped: Skipped,
    pub load: LoadState,
    /// The pacing timer is armed for this time.
    pub timer: Option<Instant>,
    /// The session the loader follows (its pointer).
    pub session: Option<usize>,
    /// The inputs the loader last planned with (it plans again when they
    /// change or the timer is due).
    pub plan_key: Option<[u64; 5]>,
}

impl InboxState {
    pub fn reset(&mut self) {
        *self = Self::default();
    }
}

/// A key of the catch-up view (uncaptured, no modifiers).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CatchUpKey {
    /// Mark as read and next.
    Enter,
    /// Skip.
    Right,
    /// Back.
    Left,
}

impl CatchUpKey {
    pub fn of(key: &iced::keyboard::Key, m: iced::keyboard::Modifiers) -> Option<Self> {
        use iced::keyboard::Key;
        use iced::keyboard::key::Named;
        if !m.is_empty() {
            return None;
        }
        match key {
            Key::Named(Named::Enter) => Some(Self::Enter),
            Key::Named(Named::ArrowRight) => Some(Self::Right),
            Key::Named(Named::ArrowLeft) => Some(Self::Left),
            _ => None,
        }
    }
}

#[derive(Debug, Clone)]
pub enum Msg {
    /// Switch to another view (the header, the sidebar rows).
    Open(InboxKind),
    Close,
    StartCatchUp,
    /// Catch-up: mark the current room read and go on.
    ReadNext,
    /// Catch-up: go on without reading.
    SkipCurrent,
    Back,
    ReviewSkipped,
    /// All unreads: hide the room until something newer arrives.
    Skip(String),
    /// Mark the room read (its threads stay).
    MarkRead(String),
    MarkThreadRead {
        rid: String,
        tmid: String,
    },
    MarkAll,
    LoadEarlier(String),
    LoadMoreRooms,
    Retry(String),
    ToggleMuted,
    Filter(ActivityFilter),
    OpenRoom(String),
    /// Open the room at message `mid` (of its feed or thread mentions).
    Reveal {
        rid: String,
        mid: String,
    },
    OpenThread {
        rid: String,
        tmid: String,
    },
    /// Open the room with the message quoted in the composer.
    Reply {
        rid: String,
        mid: String,
    },
    ReplyInThread {
        rid: String,
        tmid: String,
    },
    /// A message body's interaction (links, spoilers, images).
    View {
        rid: String,
        m: ViewerMsg,
    },
    Key(CatchUpKey),
    // Loader results (handled by the app wiring).
    FeedDone {
        fetch: inbox::load::Fetch,
        req: inbox::feeds::FeedReq,
        res: crate::error::Res<rocketchat::models::History>,
    },
    MentionsDone {
        fetch: inbox::load::Fetch,
        res: crate::error::Res<rocketchat::models::Paginated<RcMessage>>,
    },
    /// The pacing timer fired.
    Pace,
}

/// Requests to the app.
#[derive(Debug, Clone)]
pub enum Out {
    Close,
    OpenRoom(String),
    /// Open the room and show `m` (its thread for a reply).
    Reveal {
        rid: String,
        m: Box<RcMessage>,
    },
    OpenThread {
        rid: String,
        tmid: String,
    },
    /// Open the room and ask its composer.
    Compose {
        rid: String,
        intent: ComposeIntent,
    },
    /// Queue a read mark.
    Read(ReadReq),
    /// "Include muted rooms" changed: save the preference.
    SetIncludeMuted(bool),
    /// Effects of a message body of room `rid`.
    Effects {
        rid: String,
        effects: Vec<Effect>,
    },
}

/// The open view.
#[derive(Debug)]
pub struct InboxScreen {
    kind: InboxKind,
    catch_up: Option<CatchUp>,
    filter: ActivityFilter,
    /// Rooms of All unreads shown (and loaded).
    rooms_wanted: usize,
    /// Rooms whose earlier messages were asked for.
    earlier: BTreeSet<String>,
    /// Rooms (rid) and threads (tmid) being marked read, hidden meanwhile;
    /// the time the server confirmed.
    marking: HashMap<String, Option<Instant>>,
    pub viewer: Viewer,
    /// The rooms revision catch-up last followed.
    rooms_rev: u64,
    /// Bumped by every change of what the view wants (loader replans).
    pub revision: u64,
}

impl InboxScreen {
    pub fn new(kind: InboxKind) -> Self {
        Self {
            kind,
            catch_up: None,
            filter: ActivityFilter::All,
            rooms_wanted: ROOMS_PAGE,
            earlier: BTreeSet::new(),
            marking: HashMap::new(),
            viewer: Viewer::new(),
            rooms_rev: 0,
            revision: 0,
        }
    }

    /// The kind as the sidebar rows see it (catch-up is All unreads).
    pub fn kind(&self) -> InboxKind {
        self.kind
    }

    /// Whether the open view is `kind`'s (Catch up counts as All unreads).
    pub fn shows(&self, kind: InboxKind) -> bool {
        match kind {
            InboxKind::Activity => self.kind == InboxKind::Activity,
            _ => self.kind != InboxKind::Activity,
        }
    }

    pub fn is_catch_up(&self) -> bool {
        self.kind == InboxKind::CatchUp
    }

    pub fn is_marking(&self, key: &str) -> bool {
        self.marking.contains_key(key)
    }

    fn prefs(include_muted: bool) -> InboxPrefs {
        InboxPrefs { include_muted }
    }

    /// The rooms of All unreads as listed now (rooms being marked aside).
    pub fn rooms(&self, ws: &Workspace, st: &InboxState, include_muted: bool) -> Vec<UnreadRoom> {
        let mut v = inbox::unread_rooms(ws, &Self::prefs(include_muted), &st.skipped);
        v.retain(|r| !self.marking.contains_key(&r.rid));
        v
    }

    /// The rows of Activity as listed now (items being marked aside).
    pub fn activity(&self, ws: &Workspace, use_real_names: bool) -> Vec<ActivityItem> {
        let mut v = inbox::activity_items(ws, self.filter, use_real_names);
        v.retain(|i| match i {
            ActivityItem::Mention { rid, .. } | ActivityItem::RoomMentions { rid, .. } => {
                !self.marking.contains_key(rid)
            }
            ActivityItem::ThreadMention { tmid, .. } => !self.marking.contains_key(tmid),
            ActivityItem::MentionedThread(t) | ActivityItem::Thread(t) => {
                !self.marking.contains_key(&t.tmid)
            }
        });
        v
    }

    pub fn filter(&self) -> ActivityFilter {
        self.filter
    }

    pub fn rooms_wanted(&self) -> usize {
        self.rooms_wanted
    }

    pub fn catch_up(&self) -> Option<&CatchUp> {
        self.catch_up.as_ref()
    }

    /// What the loader should fetch for this view.
    pub fn wanted(&self, ws: &Workspace, st: &InboxState, include_muted: bool) -> Wanted {
        let mut w = Wanted {
            earlier: self.earlier.iter().cloned().collect(),
            ..Wanted::default()
        };
        match self.kind {
            InboxKind::Unreads => {
                w.feeds = self
                    .rooms(ws, st, include_muted)
                    .into_iter()
                    .take(self.rooms_wanted)
                    .map(|r| r.rid)
                    .collect();
            }
            InboxKind::CatchUp => {
                if let Some(c) = &self.catch_up {
                    w.feeds = c.upcoming(PREFETCH).map(str::to_owned).collect();
                }
            }
            InboxKind::Activity => {
                w.feeds = inbox::mention_rooms(ws)
                    .into_iter()
                    .map(|(rid, _, _)| rid)
                    .collect();
                w.thread_mentions = inbox::thread_mention_rooms(ws);
            }
        }
        w
    }

    /// Per update: catch-up follows the unread rooms; marks confirmed a
    /// while ago are dropped (the subscription update hides the item).
    pub fn tick(&mut self, ws: &Workspace, st: &InboxState, include_muted: bool, now: Instant) {
        let rev = ws.revision(rustket_core::Dirty::ROOMS);
        if rev != self.rooms_rev {
            self.rooms_rev = rev;
            if self.catch_up.is_some() {
                let rooms = inbox::unread_rooms(ws, &Self::prefs(include_muted), &st.skipped);
                if let Some(c) = self.catch_up.as_mut() {
                    c.refresh(&rooms);
                }
            }
            let before = self.marking.len();
            self.marking
                .retain(|key, done| done.is_none() || still_unread(ws, key));
            if self.marking.len() != before {
                self.revision += 1;
            }
        }
        let before = self.marking.len();
        self.marking
            .retain(|_, done| done.is_none_or(|at| now.saturating_duration_since(at) < MARK_GRACE));
        if self.marking.len() != before {
            self.revision += 1;
        }
    }

    /// An earlier page of `rid` arrived: the next one waits for another
    /// "Load earlier".
    pub fn earlier_done(&mut self, rid: &str) {
        if self.earlier.remove(rid) {
            self.revision += 1;
        }
    }

    /// A read mark of this view ended.
    pub fn read_done(&mut self, req: &ReadReq, ok: bool, now: Instant) {
        let key = match req {
            ReadReq::Room { rid, .. } => rid,
            ReadReq::Thread { tmid, .. } => tmid,
        };
        if ok {
            if let Some(done) = self.marking.get_mut(key) {
                *done = Some(now);
            }
        } else {
            self.marking.remove(key);
        }
        self.revision += 1;
    }

    fn mark_room(&mut self, rid: &str, out: &mut Vec<Out>) {
        self.marking.insert(rid.to_owned(), None);
        out.push(Out::Read(ReadReq::Room {
            rid: rid.to_owned(),
            threads: false,
        }));
    }

    fn mark_thread(&mut self, rid: &str, tmid: &str, out: &mut Vec<Out>) {
        self.marking.insert(tmid.to_owned(), None);
        out.push(Out::Read(ReadReq::Thread {
            rid: rid.to_owned(),
            tmid: tmid.to_owned(),
        }));
    }

    /// Applies a user action. `online`: the session is connected (read
    /// marks need it).
    pub fn update(
        &mut self,
        msg: Msg,
        ws: &Workspace,
        st: &mut InboxState,
        include_muted: bool,
        online: bool,
    ) -> Vec<Out> {
        self.revision += 1;
        let mut out = Vec::new();
        match msg {
            Msg::Open(kind) => {
                self.kind = kind;
                if kind == InboxKind::CatchUp {
                    self.catch_up = Some(CatchUp::start(&self.rooms(ws, st, include_muted)));
                } else {
                    self.catch_up = None;
                }
            }
            Msg::Close => out.push(Out::Close),
            Msg::StartCatchUp => {
                return self.update(Msg::Open(InboxKind::CatchUp), ws, st, include_muted, online);
            }
            Msg::ReadNext | Msg::Key(CatchUpKey::Enter) => {
                let current: Option<String> = self
                    .catch_up
                    .as_ref()
                    .and_then(CatchUp::current)
                    .map(String::from);
                if let Some(rid) = current {
                    if online {
                        self.mark_room(&rid, &mut out);
                    }
                    if let Some(c) = self.catch_up.as_mut() {
                        c.advance(if online { Advance::Read } else { Advance::Skip });
                    }
                }
            }
            Msg::SkipCurrent | Msg::Key(CatchUpKey::Right) => {
                if let Some(c) = self.catch_up.as_mut() {
                    c.advance(Advance::Skip);
                }
            }
            Msg::Back | Msg::Key(CatchUpKey::Left) => {
                if let Some(c) = self.catch_up.as_mut() {
                    c.back();
                }
            }
            Msg::ReviewSkipped => {
                if let Some(c) = self.catch_up.as_mut() {
                    c.review_skipped();
                }
            }
            Msg::Skip(rid) => {
                let lm = ws.room(&rid).and_then(|e| e.last_activity());
                st.skipped.skip(&rid, lm);
            }
            Msg::MarkRead(rid) => {
                if online {
                    self.mark_room(&rid, &mut out);
                }
            }
            Msg::MarkThreadRead { rid, tmid } => {
                if online {
                    self.mark_thread(&rid, &tmid, &mut out);
                }
            }
            Msg::MarkAll if !online => {}
            Msg::MarkAll => match self.kind {
                // The listed rooms; their unread threads stay for Activity.
                InboxKind::Unreads | InboxKind::CatchUp => {
                    for r in self.rooms(ws, st, include_muted) {
                        self.mark_room(&r.rid, &mut out);
                    }
                }
                InboxKind::Activity => {
                    let use_real = ws.use_real_names(None);
                    let mut rooms = BTreeSet::new();
                    for item in self.activity(ws, use_real) {
                        match item {
                            ActivityItem::Mention { rid, .. }
                            | ActivityItem::RoomMentions { rid, .. } => {
                                rooms.insert(rid);
                            }
                            ActivityItem::ThreadMention { rid, tmid, .. } => {
                                self.mark_thread(&rid, &tmid, &mut out);
                            }
                            ActivityItem::MentionedThread(t) | ActivityItem::Thread(t) => {
                                self.mark_thread(&t.rid, &t.tmid, &mut out);
                            }
                        }
                    }
                    for rid in rooms {
                        self.mark_room(&rid, &mut out);
                    }
                }
            },
            Msg::LoadEarlier(rid) => {
                self.earlier.insert(rid);
            }
            Msg::LoadMoreRooms => self.rooms_wanted += ROOMS_PAGE,
            Msg::ToggleMuted => out.push(Out::SetIncludeMuted(!include_muted)),
            Msg::Filter(f) => self.filter = f,
            Msg::OpenRoom(rid) => out.push(Out::OpenRoom(rid)),
            Msg::Reveal { rid, mid } => {
                let feeds = ws.feeds();
                let m = feeds
                    .feed(&rid)
                    .and_then(|f| f.messages().iter().find(|m| m.id == mid))
                    .or_else(|| {
                        ws.subscription(&rid)
                            .into_iter()
                            .flat_map(|s| s.tunread_user.iter().flatten())
                            .filter_map(|t| feeds.thread_mention(&rid, t))
                            .find(|m| m.id == mid)
                    });
                match m {
                    Some(m) => out.push(Out::Reveal {
                        rid,
                        m: Box::new(m.clone()),
                    }),
                    None => out.push(Out::OpenRoom(rid)),
                }
            }
            Msg::OpenThread { rid, tmid } => out.push(Out::OpenThread { rid, tmid }),
            Msg::Reply { rid, mid } => out.push(Out::Compose {
                rid,
                intent: ComposeIntent::Quote(mid),
            }),
            Msg::ReplyInThread { rid, tmid } => out.push(Out::Compose {
                rid,
                intent: ComposeIntent::ReplyInThread(tmid),
            }),
            // Handled by the app wiring (they need the workspace mutably).
            Msg::Retry(_)
            | Msg::View { .. }
            | Msg::FeedDone { .. }
            | Msg::MentionsDone { .. }
            | Msg::Pace => {}
        }
        out
    }
}

/// Whether the room (rid) or thread (tmid) `key` is still unread.
fn still_unread(ws: &Workspace, key: &str) -> bool {
    if let Some(e) = ws.room(key) {
        return rustket_core::room_list::room_is_unread(e);
    }
    ws.rooms().values().any(|e| {
        e.sub
            .as_ref()
            .is_some_and(|s| rustket_core::room_list::thread_unread(s, key).is_some())
    })
}

#[cfg(test)]
mod tests;
