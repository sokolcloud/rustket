//! Sidebar of the active server: user header with the account menu, search,
//! collapsible sections of rooms. Strings: `locales/sidebar.yml`,
//! `locales/preview.yml`, `locales/typing.yml`.

use std::collections::BTreeSet;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::time::Instant;

use iced::widget::{Column, column, container, lazy, row, scrollable, text_input};
use iced::{Alignment, Element, Length};
use rocketchat::ServerUrl;
use rocketchat::models::UserStatus;
use rust_i18n::t;
use rustket_core::config::{Density, UiPrefs};
use rustket_core::config::{SidebarSort, ThreadsMode};
use rustket_core::flags::{FlagColor, FlagEdit};
use rustket_core::room_list::{
    self, Avatar, BadgeVariant, Preview, PreviewKind, RoomKind, RoomListItem, RoomListOptions,
    Section, SectionKind, ThreadListItem,
};
use rustket_core::sidebar_layout::{Layout, LayoutEdit, SectionId};
use rustket_core::typing::TypingLabel;
use rustket_core::{ServerId, Workspace};
use rustket_ui::icon::{self, Icon};
use rustket_ui::style;
use rustket_ui::theme::{SPACE_1, SPACE_2, SPACE_3, TEXT_BODY};
use rustket_ui::widget::{
    BadgeKind, MenuEntry, MenuItem, Presence, RoomListItemProps, TooltipPosition, context_menu,
    empty_state, icon_button, keyed_list, reorder_area, room_list_item, section_header_full,
    spinner, tooltip,
};

use crate::format::{count_u32, short_time};
use crate::i18n::tr_dyn;
use crate::images::Images;

pub const SEARCH_INPUT_ID: &str = "sidebar-search";
/// The room list's scrollable (scrolled by [`Sidebar::reveal`]).
pub const LIST_ID: &str = "sidebar-list";

#[derive(Debug, Clone)]
pub enum Message {
    Filter(String),
    ClearFilter,
    Toggle(SectionId),
    Open(String),
    /// The status menu in the header (`workspace/status_menu.rs`).
    Status(super::status_menu::Message),
    Directory,
    Create,
    /// Omnichannel groups and buttons (`crate::omnichannel::sidebar`).
    Omni(crate::omnichannel::sidebar::Msg),
    /// The room list scrolled or was resized.
    Scrolled(scrollable::Viewport),
    /// A row of the Threads section.
    OpenThread {
        rid: String,
        tmid: String,
    },
    /// An item of a thread row's context menu.
    ThreadAct {
        rid: String,
        tmid: String,
        act: ThreadAct,
    },
    /// An item of a room row's context menu.
    RoomAct {
        rid: String,
        act: RoomAct,
    },
    /// An item of a section header's context menu.
    Header(HeaderAct),
    /// A header was dragged before another one (`None`: to the end); keys
    /// from [`section_key`].
    Reorder {
        dragged: u64,
        before: Option<u64>,
    },
    /// A header drag is at the list's edge: scroll by this much.
    AutoScroll(f32),
}

/// Context menu actions of a section header.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HeaderAct {
    /// Built when the menu is built (Move anchors are the neighbours it
    /// showed, not the ones at click time).
    Layout(LayoutEdit),
    /// Load the Threads section again.
    Refresh,
    /// Scan every room for starred threads again (Favorites).
    RefreshStarred,
    /// Settings, with the deletion of this group to confirm.
    Ungroup(String),
    /// Settings, at this flag's name.
    RenameFlag(FlagColor),
    /// Settings, scrolled to the Sidebar card.
    OpenSettings,
}

/// Context menu actions of a thread row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThreadAct {
    OpenRoom,
    MarkRead,
    /// For rows whose parent is unavailable (`chat.readThread` needs it).
    MarkRoomRead,
    /// Star the parent: the thread joins Favorites.
    Star,
    Unstar,
    Keep,
    Unkeep,
    Unfollow,
    /// Set the thread's flag (`None`: remove it).
    Flag(Option<FlagColor>),
}

/// Context menu actions of a room row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RoomAct {
    Favorite(bool),
    /// Set the room's flag (`None`: remove it).
    Flag(Option<FlagColor>),
}

/// Server calls made for a thread row (see [`Out::ThreadCall`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThreadCall {
    /// `chat.readThread`.
    MarkRead,
    /// `subscriptions.read` with `readThreads`.
    MarkRoomRead,
    /// `chat.unfollowMessage`.
    Unfollow,
}

#[derive(Debug, Clone)]
pub enum Out {
    None,
    OpenRoom(String),
    /// Collapsed state of a section changed (`SectionId::collapse_key`).
    ToggleSection(String),
    Status(super::status_menu::Message),
    Directory,
    Create,
    Omni(crate::omnichannel::sidebar::Msg),
    /// Open the room with the thread in its panel.
    OpenThread {
        rid: String,
        tmid: String,
    },
    /// Keep the thread in the sidebar, or stop keeping it (server config).
    KeepThread {
        rid: String,
        tmid: String,
        keep: bool,
    },
    /// Star or unstar the thread's parent message.
    StarThread {
        rid: String,
        tmid: String,
        on: bool,
    },
    ThreadCall {
        rid: String,
        tmid: String,
        call: ThreadCall,
    },
    /// Add the room to Favorites or remove it (`rooms.favorite`).
    Favorite {
        rid: String,
        on: bool,
    },
    /// Set or remove a flag (the flag sync stores it).
    Flag(FlagEdit),
    /// Edit the sidebar layout (`sidebar_layout::apply`) and save.
    Layout(LayoutEdit),
    RefreshThreads,
    /// Scan every room for starred threads again.
    RefreshStarred,
    /// Open the Sidebar settings (`ungroup`: with that group's deletion to
    /// confirm; `flag`: at that flag's name).
    OpenSettings {
        ungroup: Option<String>,
        flag: Option<FlagColor>,
    },
    /// Scroll the room list ([`LIST_ID`]) by this much.
    ScrollBy(f32),
}

#[derive(Clone, Copy)]
pub struct SidebarCtx<'a> {
    pub server_id: &'a ServerId,
    pub server: &'a ServerUrl,
    pub workspace: &'a Workspace,
    pub images: &'a Images,
    pub prefs: &'a UiPrefs,
    pub collapsed: &'a BTreeSet<String>,
    pub active_room: Option<&'a str>,
    /// The thread open in the active room's panel.
    pub active_thread: Option<&'a str>,
    pub now: Instant,
}

#[derive(Default)]
pub struct Sidebar {
    pub filter: String,
    pub sections: Vec<Section>,
    /// Revision of the inputs the sections were computed from.
    computed_for: Option<SectionsKey>,
    /// Presence revision the DM partners' statuses were last read at.
    presence_seen: u64,
    /// Bumped whenever `sections` change (recomputed or patched).
    sections_rev: u64,
    /// Last reported scroll position of the list: (offset, viewport height,
    /// content height).
    viewport: Option<(f32, f32, f32)>,
    /// The open room, kept in the Unread section after it was read.
    kept_unread: Option<String>,
    /// The open room, kept only in the unread block of unread-first sections
    /// (it had a counter or dot without counting as unread).
    kept_unread_first: Option<String>,
    /// `(rid, tmid)` of the open thread, kept in the Threads section after it
    /// was read until another thread or room opens.
    kept_thread: Option<(String, String)>,
    /// `(rid, tmid)` of the thread open in the panel: never cut by the
    /// section's row limit.
    open_thread: Option<(String, String)>,
    /// Following crawl progress `(done, rooms)` shown in the Threads header.
    thread_progress: Option<(usize, usize)>,
    /// Starred threads are tracked (Favorites offers a refresh); set by
    /// [`Self::refresh`], whose inputs cover it.
    starred_tracked: bool,
}

/// Height of a section header slot (the header is centred in it; its own
/// height is about 26.3 px).
const HEADER_H: f32 = 27.0;
/// Viewport height assumed before the scrollable reported one.
const DEFAULT_VIEWPORT_H: f32 = 1600.0;
/// Rows are built this far above and below the viewport.
const OVERSCAN_PX: f32 = 400.0;
/// The built slot range snaps to multiples of this, so scrolling rebuilds the
/// list only every few rows.
const WINDOW_STEP: usize = 16;

/// Height of a room row; `rustket_ui::widget::room_list_item` has these
/// fixed heights.
fn row_height(d: Density) -> f32 {
    match d {
        Density::Condensed => 32.0,
        Density::Extended => 56.0,
    }
}

/// Inputs of [`room_list::compute`]. Presence only matters for the sorted
/// list while filtering (names of DM partners); the DM status dots are
/// patched in place, so another user's status change does not re-sort the
/// whole list.
#[derive(Debug, Clone, PartialEq)]
struct SectionsKey {
    filter: String,
    rooms: u64,
    settings: u64,
    drafts_me: u64,
    presence: u64,
    /// Thread index revision; 0 while nothing lists threads (the Threads
    /// section is off and starred threads are not tracked).
    threads: u64,
    /// Synced flags (routing, flag section titles).
    flags: u64,
    opts: RoomListOptions,
}

/// Everything the room rows read besides `sections` (see [`Sidebar::row`]):
/// the rows are rebuilt only when this or the sections change, not on every
/// message of the app (timeline scroll, composer keystrokes…).
#[derive(Hash)]
struct BodyKey<'k> {
    server: &'k str,
    sections: u64,
    filter: &'k str,
    collapsed: &'k BTreeSet<String>,
    active: Option<&'k str>,
    active_thread: Option<&'k str>,
    /// Thread rows offer "Keep in sidebar".
    kept_enabled: bool,
    thread_progress: Option<(usize, usize)>,
    /// Sort overrides ("Use default" in the header menus); the resolved
    /// layout is part of the sections' inputs.
    section_prefs:
        &'k std::collections::BTreeMap<String, rustket_core::sidebar_layout::SectionPrefs>,
    density: u8,
    clock_24h: bool,
    /// This server's image revision (not bumped by failures or by other
    /// servers' and attachment images).
    images: u64,
    /// Built slot range (see [`Sidebar::window`]).
    window: (usize, usize),
    /// ROOMS, SETTINGS, ME, DRAFTS, TYPING revisions.
    workspace: [u64; 5],
    locale: String,
    day: chrono::NaiveDate,
    over_mac: bool,
    /// Theme generation (radii and colours given to the row widgets).
    theme: u64,
}

#[cfg(test)]
thread_local! {
    /// (section bodies, rows) built, for the rebuild-count tests.
    static BUILDS: std::cell::Cell<(usize, usize)> = const { std::cell::Cell::new((0, 0)) };
}

fn count_build(body: usize, rows: usize) {
    #[cfg(test)]
    BUILDS.with(|b| {
        let (x, y) = b.get();
        b.set((x + body, y + rows));
    });
    let _ = (body, rows);
}

fn hash_of(v: impl Hash) -> u64 {
    let mut h = DefaultHasher::new();
    v.hash(&mut h);
    h.finish()
}

/// Stable key of a section's header (list key and drag handle).
fn section_key(id: &SectionId) -> u64 {
    hash_of(("section", id.collapse_key()))
}

/// Hash of everything a row shows: the row's widgets are rebuilt (and its
/// texts reshaped) only when it changes.
fn row_key(rid: &str, p: &RoomListItemProps<Message>) -> u64 {
    let mut h = DefaultHasher::new();
    rid.hash(&mut h);
    p.avatar.name.hash(&mut h);
    p.avatar.key.hash(&mut h);
    p.avatar
        .image
        .as_ref()
        .map(iced::widget::image::Handle::id)
        .hash(&mut h);
    p.title.hash(&mut h);
    p.subtitle.hash(&mut h);
    p.time.hash(&mut h);
    (p.unread, p.thread_count, p.user_mentions, p.group_mentions).hash(&mut h);
    (p.thread_unread, p.is_muted, p.is_selected, p.typing).hash(&mut h);
    p.presence.hash(&mut h);
    p.icon.as_ref().map(std::mem::discriminant).hash(&mut h);
    p.badge_dot.hash(&mut h);
    p.flag.hash(&mut h);
    p.typing_label.hash(&mut h);
    std::mem::discriminant(&p.density).hash(&mut h);
    h.finish()
}

/// Header title: the group name, else the built-in's translated name.
fn section_title(s: &Section) -> String {
    match (&s.title, s.id.i18n_key()) {
        (Some(t), _) => t.clone(),
        (None, Some(k)) => t!(k).into_owned(),
        (None, None) => String::new(),
    }
}

pub fn presence(s: UserStatus) -> Presence {
    match s {
        UserStatus::Online => Presence::Online,
        UserStatus::Away => Presence::Away,
        UserStatus::Busy => Presence::Busy,
        UserStatus::Offline => Presence::Offline,
        _ => Presence::Unknown,
    }
}

impl Sidebar {
    pub fn update(&mut self, msg: Message) -> Out {
        match msg {
            Message::Filter(s) => {
                self.filter = s;
                Out::None
            }
            Message::ClearFilter => {
                self.filter.clear();
                Out::None
            }
            Message::Toggle(id) => Out::ToggleSection(id.collapse_key().into_owned()),
            Message::Open(rid) => Out::OpenRoom(rid),
            Message::Status(super::status_menu::Message::Omni(
                crate::omnichannel::status::Msg::ContactCenter,
            )) => Out::Omni(crate::omnichannel::sidebar::Msg::ContactCenter),
            Message::Status(m) => Out::Status(m),
            Message::Directory => Out::Directory,
            Message::Create => Out::Create,
            Message::Omni(crate::omnichannel::sidebar::Msg::Toggle(key)) => Out::ToggleSection(key),
            Message::Omni(m) => Out::Omni(m),
            Message::Scrolled(v) => {
                self.viewport = Some((
                    v.absolute_offset().y,
                    v.bounds().height,
                    v.content_bounds().height,
                ));
                Out::None
            }
            Message::OpenThread { rid, tmid } => Out::OpenThread { rid, tmid },
            Message::Reorder { dragged, before } => self.reorder(dragged, before),
            Message::AutoScroll(dy) => Out::ScrollBy(dy),
            Message::Header(act) => match act {
                HeaderAct::Layout(e) => Out::Layout(e),
                HeaderAct::Refresh => Out::RefreshThreads,
                HeaderAct::RefreshStarred => Out::RefreshStarred,
                HeaderAct::Ungroup(id) => Out::OpenSettings {
                    ungroup: Some(id),
                    flag: None,
                },
                HeaderAct::RenameFlag(c) => Out::OpenSettings {
                    ungroup: None,
                    flag: Some(c),
                },
                HeaderAct::OpenSettings => Out::OpenSettings {
                    ungroup: None,
                    flag: None,
                },
            },
            Message::ThreadAct { rid, tmid, act } => match act {
                ThreadAct::OpenRoom => Out::OpenRoom(rid),
                ThreadAct::MarkRead => Out::ThreadCall {
                    rid,
                    tmid,
                    call: ThreadCall::MarkRead,
                },
                ThreadAct::MarkRoomRead => Out::ThreadCall {
                    rid,
                    tmid,
                    call: ThreadCall::MarkRoomRead,
                },
                ThreadAct::Unfollow => Out::ThreadCall {
                    rid,
                    tmid,
                    call: ThreadCall::Unfollow,
                },
                ThreadAct::Star | ThreadAct::Unstar => Out::StarThread {
                    rid,
                    tmid,
                    on: act == ThreadAct::Star,
                },
                ThreadAct::Keep | ThreadAct::Unkeep => Out::KeepThread {
                    rid,
                    tmid,
                    keep: act == ThreadAct::Keep,
                },
                ThreadAct::Flag(color) => Out::Flag(FlagEdit::Thread { rid, tmid, color }),
            },
            Message::RoomAct { rid, act } => match act {
                RoomAct::Favorite(on) => Out::Favorite { rid, on },
                RoomAct::Flag(color) => Out::Flag(FlagEdit::Room { rid, color }),
            },
        }
    }

    /// Resolves a drop against the current sections: a section that is gone
    /// by now cancels it.
    fn reorder(&self, dragged: u64, before: Option<u64>) -> Out {
        if !self.filter.trim().is_empty() {
            return Out::None;
        }
        let ids: Vec<&SectionId> = self
            .sections
            .iter()
            .filter(|s| !s.is(SectionKind::Omnichannel))
            .map(|s| &s.id)
            .collect();
        let find = |key: u64| ids.iter().copied().find(|id| section_key(id) == key);
        let Some(id) = find(dragged) else {
            return Out::None;
        };
        let (anchor, after) = match before {
            Some(key) => match find(key) {
                Some(a) => (a, false),
                None => return Out::None,
            },
            None => match ids.last() {
                Some(a) => (*a, true),
                None => return Out::None,
            },
        };
        if anchor == id {
            return Out::None;
        }
        Out::Layout(LayoutEdit::MoveTo {
            id: id.clone(),
            anchor: anchor.clone(),
            after,
        })
    }

    /// Following crawl progress for the Threads header (`None`: not loading).
    pub fn set_thread_progress(&mut self, progress: Option<(usize, usize)>) {
        self.thread_progress = progress;
    }

    /// A room opened while unread is kept as unread until another room is
    /// opened: it stays in the Unread section, and in the unread block of a
    /// section that lists unread rooms first, so reading it does not move its
    /// row away. The same holds for the open thread in the Threads section.
    /// Call before [`Self::refresh`].
    pub fn track_active(
        &mut self,
        ws: &Workspace,
        active: Option<&str>,
        active_thread: Option<&str>,
        prefs: &UiPrefs,
    ) {
        self.track_thread(ws, active, active_thread, prefs.sidebar_threads);
        let layout = rustket_core::sidebar_layout::Layout::from_prefs(prefs);
        let needed = prefs.unread_on_top
            || layout
                .unread_first
                .iter()
                .any(|id| id != &SectionId::Builtin(SectionKind::Threads));
        let Some(rid) = active.filter(|_| needed) else {
            self.kept_unread = None;
            self.kept_unread_first = None;
            return;
        };
        if self.kept_unread.as_deref() == Some(rid)
            || self.kept_unread_first.as_deref() == Some(rid)
        {
            return;
        }
        // The sections are from before the room was opened (and maybe already
        // marked read).
        let listed = |pred: &dyn Fn(&Section, &RoomListItem) -> bool| {
            self.sections
                .iter()
                .any(|s| s.items.iter().any(|i| i.rid == rid && pred(s, i)))
        };
        let unread = listed(&|s, _| s.is(SectionKind::Unread))
            || ws.room(rid).is_some_and(room_list::room_is_unread);
        // A counter or dot without counting as unread (e.g. a mention in a room
        // with hidden unread status) keeps the row in an unread-first block
        // only, never moves it to the Unread section.
        let badged = !unread
            && listed(&|s, i| layout.is_unread_first(&s.id) && (i.badge.count > 0 || i.badge.dot));
        self.kept_unread = unread.then(|| rid.to_owned());
        self.kept_unread_first = badged.then(|| rid.to_owned());
    }

    /// An open thread listed unread in the Threads section (or unread) stays listed,
    /// and in its unread block, until another thread or room opens; closing
    /// the panel while the room stays open keeps it.
    fn track_thread(
        &mut self,
        ws: &Workspace,
        room: Option<&str>,
        thread: Option<&str>,
        mode: rustket_core::config::ThreadsMode,
    ) {
        self.open_thread = room
            .zip(thread)
            .filter(|_| mode != rustket_core::config::ThreadsMode::Off)
            .map(|(r, t)| (r.to_owned(), t.to_owned()));
        let Some(rid) = room.filter(|_| mode != rustket_core::config::ThreadsMode::Off) else {
            self.kept_thread = None;
            return;
        };
        let Some(tmid) = thread else {
            if self.kept_thread.as_ref().is_some_and(|(r, _)| r != rid) {
                self.kept_thread = None;
            }
            return;
        };
        if self
            .kept_thread
            .as_ref()
            .is_some_and(|(r, t)| r == rid && t == tmid)
        {
            return;
        }
        // The sections are from before the thread was opened (and maybe
        // already marked read).
        // Only an unread row: a read one stays where it is (it is listed
        // anyway, followed or kept), and kept it would sort into the unread
        // block.
        let listed = self.sections.iter().any(|s| {
            s.threads
                .iter()
                .any(|t| t.tmid == tmid && t.unread.is_some())
        });
        let unread = ws
            .room(rid)
            .and_then(|e| e.sub.as_ref())
            .is_some_and(|s| room_list::thread_unread(s, tmid).is_some());
        self.kept_thread = (listed || unread).then(|| (rid.to_owned(), tmid.to_owned()));
    }

    /// Recomputes the sections when the workspace or the filter changed and asks
    /// the image service for the avatars of all rows.
    pub fn refresh(
        &mut self,
        ws: &Workspace,
        prefs: &UiPrefs,
        images: &Images,
        sid: &ServerId,
        server: &ServerUrl,
        prefetch: usize,
    ) {
        use rustket_core::Dirty;
        self.starred_tracked = ws.thread_index().pins_enabled();
        let mut opts = RoomListOptions::from_prefs(prefs, ws);
        opts.kept_unread.clone_from(&self.kept_unread);
        opts.kept_unread_first.clone_from(&self.kept_unread_first);
        opts.kept_thread.clone_from(&self.kept_thread);
        opts.open_thread.clone_from(&self.open_thread);
        let presence = ws.revision(Dirty::PRESENCE);
        let key = SectionsKey {
            filter: self.filter.clone(),
            rooms: ws.revision(Dirty::ROOMS),
            settings: ws.revision(Dirty::SETTINGS),
            drafts_me: ws.revision(Dirty::DRAFTS) ^ ws.revision(Dirty::ME).rotate_left(32),
            presence: if self.filter.trim().is_empty() {
                0
            } else {
                presence
            },
            threads: if ws.thread_index().indexing() {
                ws.revision(Dirty::THREAD_INDEX)
            } else {
                0
            },
            flags: ws.revision(Dirty::FLAGS),
            opts,
        };
        if self.computed_for.as_ref() == Some(&key) {
            if presence != self.presence_seen {
                self.presence_seen = presence;
                if self.patch_presence(ws) {
                    self.sections_rev = self.sections_rev.wrapping_add(1);
                }
            }
            return;
        }
        self.sections = room_list::compute(ws, &key.opts, &self.filter);
        self.computed_for = Some(key);
        self.presence_seen = presence;
        self.sections_rev = self.sections_rev.wrapping_add(1);
        // Prefetch the first rows' avatars (and the user's own); the rest are
        // requested when they are shown.
        if let Some(me) = ws.username() {
            images.want(crate::images::ImageRequest {
                server_id: sid.clone(),
                url: Images::avatar_url(
                    server,
                    &Avatar::User {
                        username: me.to_owned(),
                    },
                ),
            });
        }
        for item in self
            .sections
            .iter()
            .flat_map(|s| s.items.iter())
            .take(prefetch)
        {
            images.want(crate::images::ImageRequest {
                server_id: sid.clone(),
                url: Images::avatar_url(server, &item.avatar),
            });
        }
    }

    /// Re-reads the status of 1:1 DM partners (as `room_list` derives
    /// `dm_status`). Returns whether any changed.
    fn patch_presence(&mut self, ws: &Workspace) -> bool {
        let mut changed = false;
        for item in self.sections.iter_mut().flat_map(|s| s.items.iter_mut()) {
            if item.kind != RoomKind::Direct {
                continue;
            }
            let status = item
                .participants
                .first()
                .map(|u| ws.presence().status_by_username(u));
            if item.dm_status != status {
                item.dm_status = status;
                changed = true;
            }
        }
        changed
    }

    pub fn room_count(&self) -> usize {
        self.sections.iter().map(|s| s.items.len()).sum()
    }

    /// `status`: the status menu (avatar trigger and popover) for the header.
    pub fn view<'a>(
        &'a self,
        ctx: &SidebarCtx<'a>,
        status: Element<'a, super::status_menu::Message>,
    ) -> Element<'a, Message> {
        let header = self.header(status);
        let search = container(
            row![
                icon::sized(Icon::Search, 16.0).style(style::text_hint),
                text_input(&t!("sidebar.search"), &self.filter)
                    .id(SEARCH_INPUT_ID)
                    .on_input(Message::Filter)
                    .style(style::text_input_bare)
                    .size(TEXT_BODY)
                    .padding([6, 0]),
            ]
            .spacing(SPACE_2)
            .align_y(Alignment::Center)
            .padding([0.0, SPACE_2]),
        )
        .style(|theme: &iced::Theme| {
            let t = rustket_ui::theme::tokens(theme);
            container::Style {
                background: Some(t.surface_sunken.into()),
                border: iced::border::rounded(rustket_ui::theme::radius_md())
                    .width(1)
                    .color(t.border),
                ..container::Style::default()
            }
        })
        .padding([0.0, SPACE_1]);
        let search =
            rustket_ui::widget::focus_within(search).radius(rustket_ui::theme::radius_md());
        let queue = crate::omnichannel::sidebar::queue_group(ctx, &self.filter);
        let body: Element<'a, Message> = if self.sections.is_empty() && queue.is_none() {
            if self.filter.trim().is_empty() {
                empty_state(Icon::Inbox, t!("sidebar.no_rooms"), String::new())
            } else {
                empty_state(Icon::Search, t!("sidebar.no_results"), String::new())
            }
        } else {
            let mut omni = Column::new()
                .spacing(0)
                .padding([0.0, SPACE_2])
                .width(Length::Fill);
            // Omnichannel groups first (queue, in progress, on hold).
            let row = |item: &'a RoomListItem| -> Element<'a, Message> { self.row(item, ctx) };
            omni = crate::omnichannel::sidebar::push_groups(
                omni,
                queue,
                &self.sections,
                ctx,
                &self.filter,
                &row,
            );
            let ws = ctx.workspace;
            let revs = [
                rustket_core::Dirty::ROOMS,
                rustket_core::Dirty::SETTINGS,
                rustket_core::Dirty::ME,
                rustket_core::Dirty::DRAFTS,
                rustket_core::Dirty::TYPING,
            ]
            .map(|d| ws.revision(d));
            let key = BodyKey {
                server: ctx.server_id.as_str(),
                sections: self.sections_rev,
                filter: &self.filter,
                collapsed: ctx.collapsed,
                active: ctx.active_room,
                active_thread: ctx.active_thread,
                kept_enabled: ws.thread_index().kept_enabled(),
                thread_progress: self.thread_progress,
                section_prefs: &ctx.prefs.sidebar_section_prefs,
                density: ctx.prefs.density as u8,
                clock_24h: ctx.prefs.clock_24h,
                images: ctx.images.server_revision(ctx.server_id),
                window: self.window(ctx),
                workspace: revs,
                locale: crate::i18n::cache_tag(),
                day: chrono::Local::now().date_naive(),
                over_mac: crate::omnichannel::shared::get(ctx.server_id)
                    .is_some_and(|o| o.over_mac),
                theme: rustket_ui::theme::generation(),
            };
            let ctx = *ctx;
            let window = key.window;
            let sections = lazy(key, move |_| self.sections_list(&ctx, window));
            scrollable(column![omni, sections].width(Length::Fill))
                .id(LIST_ID)
                .on_scroll(Message::Scrolled)
                .height(Length::Fill)
                .style(style::scrollable)
                .into()
        };
        container(
            column![header, container(search).padding([0.0, SPACE_3]), body]
                .spacing(SPACE_2)
                .height(Length::Fill),
        )
        .id(crate::focus::SIDEBAR)
        .height(Length::Fill)
        .width(Length::Fill)
        .style(style::container_sidebar)
        .into()
    }

    /// Scrolls the list so that the row of `rid` is visible (after
    /// Alt+Up/Down opened it). Rooms of the omnichannel groups, and rows
    /// already in view, need no scroll.
    pub fn reveal<M: Send + 'static>(
        &self,
        rid: &str,
        prefs: &UiPrefs,
        collapsed: &BTreeSet<String>,
    ) -> iced::Task<M> {
        match self.reveal_offset(rid, prefs.density, collapsed) {
            Some(y) => iced::widget::operation::scroll_to(
                LIST_ID,
                scrollable::AbsoluteOffset { x: 0.0, y },
            ),
            None => iced::Task::none(),
        }
    }

    /// Scroll offset that shows the row of `rid`, if it is not in view.
    fn reveal_offset(
        &self,
        rid: &str,
        density: Density,
        collapsed: &BTreeSet<String>,
    ) -> Option<f32> {
        let row_h = row_height(density);
        let mut y = 0.0f32;
        let mut found = None;
        let mut total = 0.0f32;
        for (s, is_collapsed) in self.listed_with(collapsed) {
            total += HEADER_H;
            let n = if is_collapsed { 0 } else { s.len() };
            if found.is_none() {
                y += HEADER_H;
                // Room rows come first; thread rows never match.
                match s.items.iter().take(n).position(|i| i.rid == rid) {
                    Some(k) => found = Some(y + k as f32 * row_h),
                    None => y += n as f32 * row_h,
                }
            }
            total += n as f32 * row_h;
        }
        let row_top = found?;
        let (offset, height, content) = self.viewport.unwrap_or((0.0, DEFAULT_VIEWPORT_H, total));
        let above = (content - total).max(0.0);
        let top = above + row_top;
        if top >= offset && top + row_h <= offset + height {
            return None;
        }
        // Up: the row at the top; down: at the bottom.
        let to = if top < offset {
            top
        } else {
            top + row_h - height
        };
        Some(to.clamp(0.0, (content - height).max(0.0)))
    }

    fn header<'a>(
        &'a self,
        status: Element<'a, super::status_menu::Message>,
    ) -> Element<'a, Message> {
        let menu = status.map(Message::Status);
        container(
            row![
                container(menu).width(Length::Fill),
                icon_button(
                    Icon::Globe,
                    Some(Message::Directory),
                    t!("sidebar.directory")
                ),
                icon_button(Icon::SquarePen, Some(Message::Create), t!("sidebar.create")),
            ]
            .spacing(SPACE_1)
            .align_y(Alignment::Center),
        )
        .padding([SPACE_2, SPACE_2])
        .into()
    }

    /// The listed sections (not omnichannel) and whether each is collapsed.
    fn listed<'s>(
        &'s self,
        ctx: &'s SidebarCtx<'_>,
    ) -> impl Iterator<Item = (&'s Section, bool)> + 's {
        self.listed_with(ctx.collapsed)
    }

    fn listed_with<'s>(
        &'s self,
        collapsed: &'s BTreeSet<String>,
    ) -> impl Iterator<Item = (&'s Section, bool)> + 's {
        let filtering = !self.filter.trim().is_empty();
        self.sections
            .iter()
            .filter(|s| !s.is(SectionKind::Omnichannel))
            .map(move |s| {
                (
                    s,
                    !filtering && collapsed.contains(s.id.collapse_key().as_ref()),
                )
            })
    }

    /// Slot range `[start, end)` of the list (a slot is a section header or a
    /// room row) that covers the viewport plus [`OVERSCAN_PX`]; only these
    /// are built, laid out, drawn and have their avatars requested.
    fn window(&self, ctx: &SidebarCtx<'_>) -> (usize, usize) {
        let row_h = row_height(ctx.prefs.density);
        let (mut slots, mut total) = (0usize, 0.0f32);
        for (s, collapsed) in self.listed(ctx) {
            let n = if collapsed { 0 } else { s.len() };
            slots += 1 + n;
            total += HEADER_H + n as f32 * row_h;
        }
        let (offset, height, content) = self.viewport.unwrap_or((0.0, DEFAULT_VIEWPORT_H, total));
        // The omnichannel groups are drawn above the list in the same scrollable.
        let above = (content - total).max(0.0);
        let top = (offset - above).clamp(0.0, (total - height).max(0.0));
        let (lo, hi) = (top - OVERSCAN_PX, top + height + OVERSCAN_PX);
        let (mut y, mut slot) = (0.0f32, 0usize);
        let (mut first, mut last) = (None, 0usize);
        for (s, collapsed) in self.listed(ctx) {
            if first.is_none() && y + HEADER_H > lo {
                first = Some(slot);
            }
            if y < hi {
                last = slot + 1;
            }
            y += HEADER_H;
            slot += 1;
            let n = if collapsed { 0 } else { s.len() };
            if n > 0 {
                let k0 = (((lo - y) / row_h).floor().max(0.0) as usize).min(n);
                let k1 = (((hi - y) / row_h).ceil().max(0.0) as usize).min(n);
                if first.is_none() && k0 < n {
                    first = Some(slot + k0);
                }
                if k1 > 0 {
                    last = slot + k1;
                }
            }
            y += n as f32 * row_h;
            slot += n;
        }
        let start = first.unwrap_or(0) / WINDOW_STEP * WINDOW_STEP;
        let end = (last.div_ceil(WINDOW_STEP) * WINDOW_STEP).min(slots);
        (start.min(end), end)
    }

    /// The sections other than omnichannel, rows keyed by room (a room moving
    /// to the top keeps its widgets) and each row memoised on what it shows.
    /// Only the slots in `window` are built; spacers stand for the rest.
    fn sections_list(
        &self,
        ctx: &SidebarCtx<'_>,
        (start, end): (usize, usize),
    ) -> Element<'static, Message> {
        count_build(1, 0);
        let row_h = row_height(ctx.prefs.density);
        let (mut before, mut after) = (0.0f32, 0.0f32);
        let mut built: Vec<(u64, Element<'static, Message>)> = Vec::new();
        let mut slot = 0usize;
        let from_prefs;
        let layout = match &self.computed_for {
            Some(k) => &k.opts.layout,
            None => {
                from_prefs = Layout::from_prefs(ctx.prefs);
                &from_prefs
            }
        };
        let ids: Vec<&SectionId> = self.listed(ctx).map(|(s, _)| &s.id).collect();
        let filtering = !self.filter.trim().is_empty();
        // Drag handles of every header, built or not (same slot math).
        let mut handles = Vec::new();
        let mut y = 0.0f32;
        for (i, (section, collapsed)) in self.listed(ctx).enumerate() {
            if !filtering {
                handles.push((section_key(&section.id), y, HEADER_H));
            }
            y += HEADER_H
                + if collapsed {
                    0.0
                } else {
                    section.len() as f32 * row_h
                };
            if slot < start {
                before += HEADER_H;
            } else if slot >= end {
                after += HEADER_H;
            } else {
                // A collapsed section still shows that something in it is unread.
                let unread = collapsed
                    .then(|| collapsed_unread_status(section))
                    .flatten();
                let status = if section.is(SectionKind::Threads) {
                    self.thread_progress
                        .map(progress_status)
                        .or(unread)
                        .or_else(|| {
                            (section.threads_hidden > 0).then(|| {
                                hidden_status(section.threads_hidden, layout.threads_limit)
                            })
                        })
                } else {
                    unread.or_else(|| {
                        (section.threads_hidden > 0).then(|| {
                            hidden_status(
                                section.threads_hidden,
                                rustket_core::sidebar_layout::MAX_STARRED_THREADS as u32,
                            )
                        })
                    })
                };
                let header = section_header_full(
                    section_title(section),
                    collapsed,
                    Some(section.len() as u32),
                    section.id.flag().map(FlagColor::index),
                    status,
                    Message::Toggle(section.id.clone()),
                );
                let menu = self.header_menu_at(section, i, &ids, collapsed, layout, ctx.prefs);
                built.push((
                    section_key(&section.id),
                    context_menu(container(header).center_y(HEADER_H), menu),
                ));
            }
            slot += 1;
            if collapsed {
                continue;
            }
            let n = section.len();
            let lo = start.clamp(slot, slot + n) - slot;
            let hi = end.clamp(slot, slot + n) - slot;
            before += lo as f32 * row_h;
            after += (n - hi) as f32 * row_h;
            // Room rows, then thread rows (Favorites may have both).
            let rooms = section.items.len();
            for item in &section.items[lo.min(rooms)..hi.min(rooms)] {
                built.push((hash_of(&item.rid), self.row(item, ctx)));
            }
            for t in &section.threads[lo.max(rooms) - rooms..hi.max(rooms) - rooms] {
                built.push((hash_of(("thread", &t.tmid)), thread_row(t, ctx)));
            }
            slot += n;
        }
        let mut list = keyed_list::<u64, Message, _, _>()
            .spacing(0)
            .padding([0.0, SPACE_2])
            .width(Length::Fill)
            .push(hash_of("above"), iced::widget::space().height(before));
        for (key, el) in built {
            list = list.push(key, el);
        }
        let list = list.push(hash_of("below"), iced::widget::space().height(after));
        reorder_area(list, handles, |dragged, before| Message::Reorder {
            dragged,
            before,
        })
        .on_auto_scroll(Message::AutoScroll)
        .into()
    }

    /// Context menu of `section`, the `i`-th of the listed sections `ids`.
    fn header_menu_at(
        &self,
        section: &Section,
        i: usize,
        ids: &[&SectionId],
        collapsed: bool,
        layout: &Layout,
        prefs: &UiPrefs,
    ) -> Vec<MenuEntry<Message>> {
        header_menu(&HeaderMenu {
            section,
            prev: i.checked_sub(1).and_then(|j| ids.get(j).copied()),
            next: ids.get(i + 1).copied(),
            collapsed,
            filtering: !self.filter.trim().is_empty(),
            layout,
            prefs,
            starred_tracked: self.starred_tracked,
        })
    }

    fn row(&self, item: &RoomListItem, ctx: &SidebarCtx<'_>) -> Element<'static, Message> {
        let p = self.row_props(item, ctx);
        let menu = RoomMenu::of(item, ctx.workspace);
        let key = hash_of((
            row_key(&item.rid, &p),
            &menu,
            menu.flag
                .then(|| super::flag_menu::names_key(ctx.workspace.flags())),
            crate::i18n::cache_tag(),
            rustket_ui::theme::generation(),
        ));
        let entries = menu.entries(&item.rid, ctx.workspace);
        lazy(key, move |_| {
            count_build(0, 1);
            if entries.is_empty() {
                room_list_item(p.clone())
            } else {
                context_menu(room_list_item(p.clone()), entries.clone())
            }
        })
        .into()
    }

    fn row_props(&self, item: &RoomListItem, ctx: &SidebarCtx<'_>) -> RoomListItemProps<Message> {
        let src = ctx
            .images
            .avatar(ctx.server_id, ctx.server, &item.avatar, &item.display_name);
        let mut p = RoomListItemProps::new(
            src,
            item.display_name.clone(),
            Message::Open(item.rid.clone()),
        );
        p.density = match ctx.prefs.density {
            Density::Extended => rustket_ui::widget::Density::Extended,
            Density::Condensed => rustket_ui::widget::Density::Condensed,
        };
        p.subtitle = item
            .preview
            .as_ref()
            .map(preview_text)
            .or_else(|| crate::teams::sidebar_caption(ctx.workspace, &item.rid));
        p.time = item
            .last_activity
            .map(|ts| short_time(ts, chrono::Local::now(), ctx.prefs.clock_24h));
        let b = row_badge(&item.badge);
        p.unread = b.unread;
        p.thread_count = b.thread_count;
        p.user_mentions = b.user_mentions;
        p.group_mentions = b.group_mentions;
        p.thread_unread = b.thread_unread;
        p.is_muted = item.muted;
        p.is_selected = ctx.active_room == Some(item.rid.as_str());
        p.presence = item.dm_status.map(presence);
        p.icon = Some(room_icon(item.kind));
        p.flag = item
            .flag
            .map(|c| super::flag_menu::mark(ctx.workspace.flags(), c));
        crate::omnichannel::sidebar::decorate(&mut p, item, ctx);
        if let Some(label) = ctx.workspace.typing_label(&item.rid, None, ctx.now) {
            p.typing = true;
            p.typing_label = typing_text(&label);
        }
        p
    }
}

/// What a room row's context menu offers (hashed into the row's key).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct RoomMenu {
    /// "Add to Favorites" (`Some(false)`) or "Remove from Favorites".
    favorite: Option<bool>,
    /// The flag block, with the current color.
    flag: bool,
    current: Option<FlagColor>,
}

impl RoomMenu {
    /// Omnichannel rows keep their own actions (the room's header).
    fn of(item: &RoomListItem, ws: &Workspace) -> Self {
        if item.kind == RoomKind::Omnichannel {
            return Self {
                favorite: None,
                flag: false,
                current: None,
            };
        }
        let favorite = crate::room::panel::common::room_action_allowed(
            ws,
            &item.rid,
            crate::room::panel::common::RoomAction::Favorite(!item.favorite),
        )
        .then_some(item.favorite);
        Self {
            favorite,
            flag: super::flag_menu::can_flag_room(ws, &item.rid),
            current: item.flag,
        }
    }

    /// Favorites, then the flag block.
    fn entries(&self, rid: &str, ws: &Workspace) -> Vec<MenuEntry<Message>> {
        let act = |act: RoomAct| Message::RoomAct {
            rid: rid.to_owned(),
            act,
        };
        let mut v: Vec<MenuEntry<Message>> = Vec::new();
        if let Some(fav) = self.favorite {
            let a = crate::room::panel::common::RoomAction::Favorite(!fav);
            v.push(
                MenuItem::new(a.label(), act(RoomAct::Favorite(!fav)))
                    .icon(a.icon())
                    .into(),
            );
        }
        if self.flag {
            if !v.is_empty() {
                v.push(MenuEntry::Separator);
            }
            v.extend(super::flag_menu::entries(ws.flags(), self.current, |c| {
                act(RoomAct::Flag(c))
            }));
        }
        v
    }
}

/// "Loading followed threads… n/m" as a spinner and "n/m", the full text in
/// a tooltip.
fn progress_status((done, total): (usize, usize)) -> Element<'static, Message> {
    let full = t!(
        "sidebar.threads_loading_progress",
        done = done,
        total = total
    )
    .into_owned();
    let content = row![
        spinner(10.0),
        iced::widget::text(format!("{done}/{total}"))
            .size(rustket_ui::theme::TEXT_MICRO + 1.0)
            .style(style::text_hint),
    ]
    .spacing(SPACE_1)
    .align_y(Alignment::Center);
    tooltip(content, full, TooltipPosition::Bottom)
}

/// "+N": thread rows left out by the limit, explained in a tooltip.
/// A dot for the unread rows of a collapsed section (the strongest kind),
/// their number in a tooltip.
fn collapsed_unread_status(section: &Section) -> Option<Element<'static, Message>> {
    let kinds: Vec<BadgeKind> = section
        .items
        .iter()
        .filter_map(room_badge_kind)
        .chain(section.threads.iter().filter_map(thread_badge_kind))
        .collect();
    let kind = kinds.iter().copied().max_by_key(|&k| badge_rank(k))?;
    let full = t!("sidebar.unread_collapsed", count = kinds.len()).into_owned();
    Some(tooltip(
        rustket_ui::widget::dot(kind, 8.0),
        full,
        TooltipPosition::Bottom,
    ))
}

/// How loud a badge kind is: mentions, then plain unread, then the quiet kinds.
fn badge_rank(k: BadgeKind) -> u8 {
    match k {
        BadgeKind::Mention => 4,
        BadgeKind::GroupMention => 3,
        BadgeKind::Unread => 2,
        BadgeKind::Thread => 1,
        BadgeKind::Muted => 0,
    }
}

/// What a room row's badge is made of.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct RowBadge {
    /// The counter: unread messages and unread threads.
    unread: u32,
    /// The part of `unread` that is threads.
    thread_count: u32,
    user_mentions: u32,
    group_mentions: u32,
    /// Unread threads, or `alert` without a counter (a dot).
    thread_unread: bool,
}

/// A room's badge inputs; nothing when the room shows neither a counter nor
/// a dot (its unread status is hidden), as Rocket.Chat does.
fn row_badge(b: &room_list::Badge) -> RowBadge {
    if b.count == 0 && !b.dot {
        return RowBadge::default();
    }
    let unread = count_u32(b.count);
    RowBadge {
        unread,
        thread_count: count_u32(b.threads),
        user_mentions: if b.variant == BadgeVariant::Mention {
            count_u32(b.mentions)
        } else {
            0
        },
        group_mentions: if b.variant == BadgeVariant::GroupMention {
            count_u32(b.group_mentions)
        } else {
            0
        },
        thread_unread: b.threads > 0 || (b.dot && unread == 0),
    }
}

/// The badge kind of a room row (the same as [`RoomListItemProps::badge_kind`]).
fn room_badge_kind(item: &RoomListItem) -> Option<BadgeKind> {
    let b = row_badge(&item.badge);
    BadgeKind::for_room(
        b.user_mentions,
        b.group_mentions,
        b.unread.saturating_sub(b.thread_count),
        b.thread_unread,
        item.muted,
    )
}

/// The dot of a thread row: red/orange for mentions, a quiet grey for
/// plain replies (blue reads like a mention next to room badges).
fn thread_badge_kind(t: &ThreadListItem) -> Option<BadgeKind> {
    t.unread.map(|v| match v {
        BadgeVariant::Mention => BadgeKind::Mention,
        BadgeVariant::GroupMention => BadgeKind::GroupMention,
        BadgeVariant::Thread | BadgeVariant::Normal => BadgeKind::Muted,
    })
}

fn hidden_status(hidden: usize, limit: u32) -> Element<'static, Message> {
    let full = t!("sidebar.threads_hidden_tip", count = hidden, limit = limit).into_owned();
    let content = iced::widget::text(t!("sidebar.threads_hidden", count = hidden).into_owned())
        .size(rustket_ui::theme::TEXT_MICRO + 1.0)
        .style(style::text_hint);
    tooltip(content, full, TooltipPosition::Bottom)
}

/// "20", or "Unlimited" for 0 (a thread limit).
pub fn threads_limit_label(n: u32) -> String {
    if n == 0 {
        t!("sidebar.threads_limit_none").into_owned()
    } else {
        n.to_string()
    }
}

/// A row of the Threads section: the parent's text (its author's avatar),
/// the room and replies, the last reply's time; a dot while unread. Opens
/// the thread; the context menu reads it, keeps it or unfollows it.
fn thread_row(t: &ThreadListItem, ctx: &SidebarCtx<'_>) -> Element<'static, Message> {
    let p = thread_row_props(t, ctx);
    let kept_enabled = ctx.workspace.thread_index().kept_enabled();
    let star_enabled = can_star_thread(ctx.workspace, &t.rid);
    let flags =
        super::flag_menu::can_flag_thread(ctx.workspace, &t.rid).then(|| ctx.workspace.flags());
    let key = hash_of((
        "thread",
        row_key(&t.tmid, &p),
        (
            t.kept,
            t.starred,
            t.flag,
            t.unread.is_some(),
            t.following,
            std::mem::discriminant(&t.title),
        ),
        (kept_enabled, star_enabled),
        flags.map(super::flag_menu::names_key),
        crate::i18n::cache_tag(),
        rustket_ui::theme::generation(),
    ));
    let entries = thread_menu(t, kept_enabled, star_enabled, flags);
    lazy(key, move |_| {
        count_build(0, 1);
        context_menu(room_list_item(p.clone()), entries.clone())
    })
    .into()
}

fn thread_title_text(t: &ThreadListItem) -> String {
    match (t.title.text(), t.title.i18n_key()) {
        (Some(s), _) => s.to_owned(),
        (None, Some(k)) => t!(k).into_owned(),
        (None, None) => String::new(),
    }
}

fn thread_row_props(t: &ThreadListItem, ctx: &SidebarCtx<'_>) -> RoomListItemProps<Message> {
    let name = t.author.as_deref().unwrap_or(&t.room_name);
    let src = ctx
        .images
        .avatar(ctx.server_id, ctx.server, &t.avatar, name);
    let title = thread_title_text(t);
    let condensed = ctx.prefs.density == Density::Condensed;
    // Condensed rows have no subtitle: the room goes into the title.
    let title = if condensed {
        t!("sidebar.thread_in_room", room = t.room_name, title = title).into_owned()
    } else {
        title
    };
    let mut p = RoomListItemProps::new(
        src,
        title,
        Message::OpenThread {
            rid: t.rid.clone(),
            tmid: t.tmid.clone(),
        },
    );
    p.density = if condensed {
        rustket_ui::widget::Density::Condensed
    } else {
        rustket_ui::widget::Density::Extended
    };
    p.subtitle = Some(if t.tcount > 0 {
        let replies = t!("sidebar.thread_replies", count = t.tcount).into_owned();
        t!(
            "sidebar.thread_subtitle",
            room = t.room_name,
            replies = replies
        )
        .into_owned()
    } else {
        t.room_name.clone()
    });
    p.time = t
        .tlm
        .map(|ts| short_time(ts, chrono::Local::now(), ctx.prefs.clock_24h));
    p.icon = Some(Icon::MessageCircle);
    p.flag = t
        .flag
        .map(|c| super::flag_menu::mark(ctx.workspace.flags(), c));
    p.badge_dot = thread_badge_kind(t);
    p.is_selected = ctx.active_thread == Some(t.tmid.as_str());
    if let Some(label) = ctx.workspace.typing_label(&t.rid, Some(&t.tmid), ctx.now) {
        p.typing = true;
        p.typing_label = typing_text(&label);
    }
    p
}

/// Thread rows of `rid` offer "Add thread to Favorites": starred threads are
/// tracked and the user may star messages there.
fn can_star_thread(ws: &Workspace, rid: &str) -> bool {
    ws.thread_index().pins_enabled() && rustket_core::permissions::can_star(ws, rid)
}

/// Open the room, mark read, star, keep, unfollow; then the flag block
/// (`flags`: the thread can carry a flag).
fn thread_menu(
    t: &ThreadListItem,
    kept_enabled: bool,
    star_enabled: bool,
    flags: Option<&rustket_core::flags::Flags>,
) -> Vec<MenuEntry<Message>> {
    let act = |act: ThreadAct| Message::ThreadAct {
        rid: t.rid.clone(),
        tmid: t.tmid.clone(),
        act,
    };
    let loaded = t.is_loaded() && t.title != room_list::ThreadTitle::Removed;
    let mut entries: Vec<MenuEntry<Message>> = vec![
        MenuItem::new(t!("sidebar.thread_open_room"), act(ThreadAct::OpenRoom))
            .icon(room_icon(t.room_kind))
            .into(),
    ];
    if t.unread.is_some() {
        // `chat.readThread` fails for a parent that is not found: only
        // reading the whole room (with its threads) clears the row.
        let (label, a) = if t.title == room_list::ThreadTitle::Unavailable {
            (t!("sidebar.thread_mark_room_read"), ThreadAct::MarkRoomRead)
        } else {
            (t!("sidebar.thread_mark_read"), ThreadAct::MarkRead)
        };
        entries.push(MenuItem::new(label, act(a)).icon(Icon::CheckCheck).into());
    }
    if star_enabled && t.starred {
        entries.push(
            MenuItem::new(t!("sidebar.unstar_thread"), act(ThreadAct::Unstar))
                .icon(Icon::StarOff)
                .into(),
        );
    } else if star_enabled && loaded && t.tcount > 0 {
        entries.push(
            MenuItem::new(t!("sidebar.star_thread"), act(ThreadAct::Star))
                .icon(Icon::Star)
                .into(),
        );
    }
    if t.kept {
        entries.push(
            MenuItem::new(t!("sidebar.unkeep_thread"), act(ThreadAct::Unkeep))
                .icon(Icon::BookmarkX)
                .into(),
        );
    } else if kept_enabled && loaded {
        entries.push(
            MenuItem::new(t!("sidebar.keep_thread"), act(ThreadAct::Keep))
                .icon(Icon::Bookmark)
                .into(),
        );
    }
    if loaded && t.following {
        entries.push(
            MenuItem::new(t!("room_timeline.act_unfollow"), act(ThreadAct::Unfollow))
                .icon(Icon::BellOff)
                .into(),
        );
    }
    if let Some(flags) = flags {
        entries.push(MenuEntry::Separator);
        entries.extend(super::flag_menu::entries(flags, t.flag, |c| {
            act(ThreadAct::Flag(c))
        }));
    }
    entries
}

/// What a section header's context menu is built from.
struct HeaderMenu<'a> {
    section: &'a Section,
    /// The visible sections above and below (anchors of Move up/down).
    prev: Option<&'a SectionId>,
    next: Option<&'a SectionId>,
    collapsed: bool,
    /// Sections are search results: no moving, no collapsing.
    filtering: bool,
    layout: &'a Layout,
    prefs: &'a UiPrefs,
    /// Favorites lists starred threads: offer scanning for them again.
    starred_tracked: bool,
}

pub(crate) fn sort_label(sort: SidebarSort, threads: bool) -> String {
    let label = match (sort, threads) {
        (SidebarSort::Activity, false) => t!("sidebar.sort_newest"),
        (SidebarSort::ActivityAsc, false) => t!("sidebar.sort_oldest"),
        (SidebarSort::Alphabetical, false) => t!("sidebar.sort_alpha"),
        (SidebarSort::Activity, true) => t!("sidebar.sort_reply_newest"),
        (SidebarSort::ActivityAsc, true) => t!("sidebar.sort_reply_oldest"),
        (SidebarSort::Alphabetical, true) => t!("sidebar.sort_by_room"),
    };
    label.into_owned()
}

/// Move up/down, sort, unread first; the Threads mode, Refresh and Hide;
/// Hide for Unread, Refresh for Favorites with starred threads, Rename for
/// flags, Ungroup for groups; collapse and the settings.
fn header_menu(m: &HeaderMenu<'_>) -> Vec<MenuEntry<Message>> {
    let id = &m.section.id;
    let edit = |e: LayoutEdit| Message::Header(HeaderAct::Layout(e));
    let threads = m.section.is(SectionKind::Threads);
    let unread = m.section.is(SectionKind::Unread);
    let move_to = |anchor: Option<&SectionId>, after: bool| {
        let item = edit(LayoutEdit::MoveTo {
            id: id.clone(),
            anchor: anchor.unwrap_or(id).clone(),
            after,
        });
        (item, anchor.is_none() || m.filtering)
    };
    let (up, up_off) = move_to(m.prev, false);
    let (down, down_off) = move_to(m.next, true);
    let mut entries: Vec<MenuEntry<Message>> = vec![
        MenuItem::new(t!("sidebar.menu_move_up"), up)
            .icon(Icon::ArrowUp)
            .disabled(up_off)
            .into(),
        MenuItem::new(t!("sidebar.menu_move_down"), down)
            .icon(Icon::ArrowDown)
            .disabled(down_off)
            .into(),
        MenuEntry::Separator,
        MenuEntry::Header(t!("sidebar.menu_sort").into_owned()),
    ];
    let current = m.layout.sort_of(id);
    for sort in [
        SidebarSort::Activity,
        SidebarSort::ActivityAsc,
        SidebarSort::Alphabetical,
    ] {
        entries.push(
            MenuItem::new(
                sort_label(sort, threads),
                edit(LayoutEdit::SetSort {
                    id: id.clone(),
                    sort: Some(sort),
                }),
            )
            .checked(sort == current)
            .into(),
        );
    }
    let overridden = m
        .prefs
        .sidebar_section_prefs
        .get(&id.as_config_str())
        .is_some_and(|p| p.sort.is_some());
    if overridden {
        let default = sort_label(m.layout.default_sort, threads);
        entries.push(
            MenuItem::new(
                t!("sidebar.sort_default", sort = default),
                edit(LayoutEdit::SetSort {
                    id: id.clone(),
                    sort: None,
                }),
            )
            .checked(false)
            .into(),
        );
    }
    if !unread {
        let on = !m.layout.is_unread_first(id);
        // Back to `None` when that is the default (on for Threads only).
        entries.push(
            MenuItem::new(
                t!("sidebar.menu_unread_first"),
                edit(LayoutEdit::SetUnreadFirst {
                    id: id.clone(),
                    on: (on != threads).then_some(on),
                }),
            )
            .checked(!on)
            .into(),
        );
    }
    entries.push(MenuEntry::Separator);
    if threads {
        entries.push(MenuEntry::Header(t!("sidebar.menu_show").into_owned()));
        for (mode, label) in [
            (ThreadsMode::Unread, t!("sidebar.threads_unread")),
            (ThreadsMode::UnreadKept, t!("sidebar.threads_unread_kept")),
            (ThreadsMode::Following, t!("sidebar.threads_following")),
        ] {
            entries.push(
                MenuItem::new(label, edit(LayoutEdit::SetThreadsMode(mode)))
                    .checked(m.layout.threads == mode)
                    .into(),
            );
        }
        entries.push(MenuEntry::Header(
            t!("sidebar.menu_show_at_most").into_owned(),
        ));
        let limit = m.layout.threads_limit;
        for n in rustket_core::sidebar_layout::threads_limit_choices(limit) {
            entries.push(
                MenuItem::new(threads_limit_label(n), edit(LayoutEdit::SetThreadsLimit(n)))
                    .checked(n == limit)
                    .into(),
            );
        }
        entries.extend([
            MenuEntry::Separator,
            MenuItem::new(
                t!("sidebar.menu_refresh"),
                Message::Header(HeaderAct::Refresh),
            )
            .icon(Icon::RefreshCw)
            .into(),
            MenuItem::new(
                t!("sidebar.menu_hide_threads"),
                edit(LayoutEdit::SetThreadsMode(ThreadsMode::Off)),
            )
            .icon(Icon::EyeOff)
            .into(),
            MenuEntry::Separator,
        ]);
    } else if unread {
        entries.extend([
            MenuItem::new(
                t!("sidebar.menu_hide_unread"),
                edit(LayoutEdit::SetUnreadOnTop(false)),
            )
            .icon(Icon::EyeOff)
            .into(),
            MenuEntry::Separator,
        ]);
    } else if m.starred_tracked && m.section.is(SectionKind::Favorites) {
        entries.extend([
            MenuItem::new(
                t!("sidebar.menu_refresh_starred"),
                Message::Header(HeaderAct::RefreshStarred),
            )
            .icon(Icon::RefreshCw)
            .into(),
            MenuEntry::Separator,
        ]);
    } else if let Some(c) = id.flag() {
        entries.extend([
            MenuItem::new(
                t!("sidebar.menu_rename_flag"),
                Message::Header(HeaderAct::RenameFlag(c)),
            )
            .icon(Icon::Pencil)
            .into(),
            MenuEntry::Separator,
        ]);
    } else if let Some(gid) = id.group_id() {
        entries.extend([
            MenuItem::new(
                t!("sidebar.menu_ungroup"),
                Message::Header(HeaderAct::Ungroup(gid.to_owned())),
            )
            .icon(Icon::Ungroup)
            .danger(true)
            .into(),
            MenuEntry::Separator,
        ]);
    }
    // Left-click collapses too; the long Threads menu leaves it out. While
    // filtering every section shows expanded, so the label would lie.
    if !threads && !m.filtering {
        let (label, icon) = if m.collapsed {
            (t!("sidebar.menu_expand"), Icon::ChevronsUpDown)
        } else {
            (t!("sidebar.menu_collapse"), Icon::ChevronsDownUp)
        };
        entries.push(
            MenuItem::new(label, Message::Toggle(id.clone()))
                .icon(icon)
                .into(),
        );
    }
    entries.push(
        MenuItem::new(
            t!("sidebar.menu_settings"),
            Message::Header(HeaderAct::OpenSettings),
        )
        .icon(Icon::SlidersHorizontal)
        .into(),
    );
    entries
}

pub fn room_icon(kind: RoomKind) -> Icon {
    match kind {
        RoomKind::Channel => Icon::Hash,
        RoomKind::Private => Icon::Lock,
        RoomKind::Direct => Icon::AtSign,
        RoomKind::GroupDirect => Icon::Users,
        RoomKind::Discussion => Icon::MessagesSquare,
        RoomKind::Team { private: false } => Icon::Users,
        RoomKind::Team { private: true } => Icon::UserLock,
        RoomKind::Omnichannel => Icon::MessageCircle,
    }
}

/// Last-message preview line: "You: …", "bob: …", "📷 Image" …
pub fn preview_text(p: &Preview) -> String {
    let body = match p.kind {
        PreviewKind::Text => p.text.clone().unwrap_or_default(),
        PreviewKind::System => {
            let arg = p.text.clone().unwrap_or_default();
            let s = rust_i18n::t!(p.i18n_key.as_str(), text = arg).into_owned();
            if s.ends_with(p.i18n_key.as_str()) {
                t!("preview.system.generic").into_owned()
            } else {
                s
            }
        }
        _ => {
            let label = tr_dyn(&p.i18n_key, &t!("preview.attachment"));
            match p.text.as_deref() {
                Some(name) if !name.is_empty() => format!("{label}: {name}"),
                _ => label,
            }
        }
    };
    if p.own {
        t!("preview.you", text = body).into_owned()
    } else if let Some(author) = &p.author {
        t!("preview.author", author = author, text = body).into_owned()
    } else {
        body
    }
}

/// "bob is typing…", "bob and alice are typing…", "a, b, c and 2 others are typing…".
pub fn typing_text(l: &TypingLabel) -> String {
    let names = match (l.usernames.as_slice(), l.others()) {
        ([one], 0) => one.clone(),
        (list, 0) if list.len() > 1 => {
            let (last, rest) = list
                .split_last()
                .map_or((String::new(), &[][..]), |(l, r)| (l.clone(), r));
            t!("typing.and", first = rest.join(", "), last = last).into_owned()
        }
        (list, others) => {
            let key = format!(
                "typing.and_others_{}",
                crate::room::render::plural(others as u64)
            );
            t!(key, first = list.join(", "), count = others).into_owned()
        }
    };
    let key = l.activity.i18n_key();
    if l.count > 1 {
        rust_i18n::t!(format!("{key}_many"), names = names).into_owned()
    } else {
        rust_i18n::t!(key, names = names).into_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustket_core::typing::Activity;

    mod rebuilds {
        #![allow(clippy::unwrap_used)]
        use super::super::*;
        use iced::advanced::widget::Tree;
        use rocketchat::RcDate;
        use rocketchat::models::{Room, RoomType, Subscription, User, UserRef};
        use rustket_core::Change;

        const ROOMS: usize = 300;

        fn sub(rid: &str, name: &str, t: RoomType) -> Subscription {
            Subscription {
                id: format!("sub-{rid}"),
                rid: rid.into(),
                name: name.into(),
                t,
                u: UserRef {
                    id: "me".into(),
                    username: Some("me".into()),
                    name: None,
                },
                open: true,
                ..Default::default()
            }
        }

        fn room(rid: &str, name: &str, t: RoomType, lm: i64) -> Room {
            Room {
                id: rid.into(),
                t,
                name: Some(name.into()),
                lm: RcDate::from_millis(lm),
                ..Default::default()
            }
        }

        struct Fx {
            ws: Workspace,
            images: Images,
            prefs: UiPrefs,
            collapsed: BTreeSet<String>,
            sid: ServerId,
            url: ServerUrl,
            sidebar: Sidebar,
        }

        impl Fx {
            fn new() -> Self {
                Self::with_rooms(ROOMS)
            }

            fn with_rooms(rooms: usize) -> Self {
                let mut ws = Workspace::new();
                ws.apply_me(User {
                    id: "me".into(),
                    username: Some("me".into()),
                    ..Default::default()
                });
                for i in 0..rooms {
                    let rid = format!("r{i:04}");
                    ws.apply_subscription(Change::Inserted, sub(&rid, &rid, RoomType::Channel));
                    // r0000 is the newest: at the top.
                    let lm = 1_700_000_000_000 - i as i64 * 1000;
                    ws.apply_room(Change::Inserted, room(&rid, &rid, RoomType::Channel, lm));
                }
                ws.apply_subscription(Change::Inserted, sub("dm", "bob", RoomType::Direct));
                let mut dm = room("dm", "", RoomType::Direct, 1_600_000_000_000);
                dm.usernames = Some(vec!["me".into(), "bob".into()]);
                dm.uids = Some(vec!["me".into(), "bobid".into()]);
                ws.apply_room(Change::Inserted, dm);
                Self {
                    ws,
                    images: Images::new(None),
                    prefs: UiPrefs::default(),
                    collapsed: BTreeSet::new(),
                    sid: ServerId("s".into()),
                    url: ServerUrl::parse("http://localhost:3000").unwrap(),
                    sidebar: Sidebar::default(),
                }
            }

            fn refresh(&mut self) -> u64 {
                self.sidebar
                    .refresh(&self.ws, &self.prefs, &self.images, &self.sid, &self.url, 0);
                self.sidebar.sections_rev
            }

            fn view(&self) -> Element<'_, Message> {
                let ctx = SidebarCtx {
                    server_id: &self.sid,
                    server: &self.url,
                    workspace: &self.ws,
                    images: &self.images,
                    prefs: &self.prefs,
                    collapsed: &self.collapsed,
                    active_room: None,
                    active_thread: None,
                    now: Instant::now(),
                };
                self.sidebar.view(&ctx, iced::widget::space().into())
            }

            /// (bodies, rows) built while diffing the current view into `tree`.
            fn diff(&mut self, tree: &mut Tree) -> (usize, usize) {
                self.refresh();
                BUILDS.with(|b| b.set((0, 0)));
                tree.diff(self.view().as_widget());
                BUILDS.with(std::cell::Cell::get)
            }
        }

        fn dm_status(sb: &Sidebar) -> Option<UserStatus> {
            sb.sections
                .iter()
                .flat_map(|s| &s.items)
                .find(|i| i.rid == "dm")
                .and_then(|i| i.dm_status)
        }

        /// Content height of the list (no omnichannel groups).
        fn total(sb: &Sidebar) -> f32 {
            let rows: usize = sb.sections.iter().map(|s| s.items.len()).sum();
            sb.sections.len() as f32 * HEADER_H + rows as f32 * row_height(Density::Extended)
        }

        /// Scrolls the list so that the row of `rid` is in the viewport.
        fn scroll_to(fx: &mut Fx, rid: &str) {
            let mut y = 0.0;
            for s in &fx.sidebar.sections {
                y += HEADER_H;
                if let Some(i) = s.items.iter().position(|i| i.rid == rid) {
                    y += i as f32 * row_height(Density::Extended);
                    break;
                }
                y += s.items.len() as f32 * row_height(Density::Extended);
            }
            let total = total(&fx.sidebar);
            fx.sidebar.viewport = Some(((y - 100.0).max(0.0), 600.0, total));
        }

        /// App review #54/#55/#56: rows used to be rebuilt (and reshaped)
        /// on every message, and all of them on a reorder.
        #[test]
        fn rows_are_rebuilt_only_when_they_change() {
            rust_i18n::set_locale("en");
            let mut fx = Fx::new();
            fx.refresh();
            BUILDS.with(|b| b.set((0, 0)));
            let mut tree = Tree::new(fx.view().as_widget());
            let (bodies, rows) = BUILDS.with(std::cell::Cell::get);
            assert_eq!(bodies, 1);
            assert!((20..64).contains(&rows), "only the rows on screen: {rows}");

            // A message that does not touch the sidebar (scroll, keystroke).
            assert_eq!(fx.diff(&mut tree), (0, 0));

            // A new message in the lowest channel moves it to the top: only
            // that row changes.
            let last = format!("r{:04}", ROOMS - 1);
            fx.ws.apply_room(
                Change::Updated,
                room(&last, &last, RoomType::Channel, 1_800_000_000_000),
            );
            assert_eq!(fx.diff(&mut tree), (1, 1));
            let channels = fx
                .sidebar
                .sections
                .iter()
                .find(|s| s.is(SectionKind::Channels))
                .unwrap();
            assert_eq!(channels.items[0].rid, last);
            let keys = rustket_ui::widget::keyed_list_keys::<u64>(first_keyed(&tree).unwrap());
            assert!(keys.is_some_and(|k| k.contains(&hash_of(&last))));

            // A stranger's status: no recompute, no rebuild.
            let rev = fx.refresh();
            fx.ws
                .apply_user_status("zid", Some("zed"), UserStatus::Online, None);
            assert_eq!(fx.refresh(), rev, "room list not recomputed");
            assert_eq!(fx.diff(&mut tree), (0, 0));

            // The DM partner's status: the dot changes, one row rebuilt.
            scroll_to(&mut fx, "dm");
            fx.diff(&mut tree);
            fx.ws
                .apply_user_status("bobid", Some("bob"), UserStatus::Away, None);
            assert_ne!(fx.refresh(), rev);
            assert_eq!(dm_status(&fx.sidebar), Some(UserStatus::Away));
            assert_eq!(fx.diff(&mut tree), (1, 1));

            // Filtering re-runs the list with presence (names) as an input.
            fx.sidebar.filter = "bob".into();
            fx.diff(&mut tree);
            assert_eq!(fx.sidebar.room_count(), 1);
            fx.sidebar.filter.clear();
            fx.diff(&mut tree);
            assert_eq!(fx.sidebar.room_count(), ROOMS + 1);
        }

        /// Threads mode Off: thread index changes do not recompute the list;
        /// with the mode on, they do.
        #[test]
        fn thread_index_changes_recompute_only_with_threads_on() {
            use rocketchat::models::Message as RcMessage;
            use rustket_core::Dirty;
            use rustket_core::config::ThreadsMode;
            let mut fx = Fx::with_rooms(3);
            let rev = fx.refresh();
            let before = fx.ws.revision(Dirty::THREAD_INDEX);
            fx.ws.set_thread_config(ThreadsMode::Off, [("r0000", "t")]);
            fx.ws.set_thread_config(ThreadsMode::Off, [("r0001", "t")]);
            assert_ne!(fx.ws.revision(Dirty::THREAD_INDEX), before);
            assert_eq!(fx.refresh(), rev, "not recomputed while Off");

            fx.prefs.sidebar_threads = ThreadsMode::Unread;
            fx.ws.set_thread_config(ThreadsMode::Unread, []);
            let mut s = sub("r0000", "r0000", RoomType::Channel);
            s.tunread = Some(vec!["t".into()]);
            fx.ws.apply_subscription(Change::Updated, s);
            let rev = fx.refresh();
            assert_eq!(fx.refresh(), rev);
            let parent = RcMessage {
                id: "t".into(),
                rid: "r0000".into(),
                tcount: Some(1),
                tlm: RcDate::from_millis(1_700_000_000_000),
                ..Default::default()
            };
            fx.ws.apply_thread_parents(vec![parent]);
            assert_ne!(fx.refresh(), rev, "recomputed with the mode on");
            assert!(
                fx.sidebar
                    .sections
                    .iter()
                    .any(|s| s.is(SectionKind::Threads))
            );

            // Starred threads are listed in Favorites with the mode off.
            fx.prefs.sidebar_threads = ThreadsMode::Off;
            fx.ws.set_thread_config(ThreadsMode::Off, []);
            fx.ws.set_starred_threads(true);
            let rev = fx.refresh();
            fx.ws.set_thread_starred("r0000", "t", true);
            assert_ne!(fx.refresh(), rev, "recomputed for starred threads");
            let fav = fx
                .sidebar
                .sections
                .iter()
                .find(|s| s.is(SectionKind::Favorites))
                .unwrap();
            assert_eq!(fav.threads.len(), 1);
            assert!(fav.threads[0].starred);
        }

        /// A room opened from the Unread section stays there once read, until
        /// another room is opened; opening a read room does not pull it in.
        #[test]
        fn the_open_room_stays_unread_until_another_is_opened() {
            fn unread(fx: &Fx) -> Vec<String> {
                fx.sidebar
                    .sections
                    .iter()
                    .filter(|s| s.is(SectionKind::Unread))
                    .flat_map(|s| s.items.iter().map(|i| i.rid.clone()))
                    .collect()
            }
            fn open(fx: &mut Fx, rid: Option<&str>) {
                fx.sidebar.track_active(&fx.ws, rid, None, &fx.prefs);
                fx.refresh();
            }
            let mut fx = Fx::with_rooms(3);
            fx.prefs.unread_on_top = true;
            let mut s = sub("r0001", "r0001", RoomType::Channel);
            s.unread = 2;
            fx.ws.apply_subscription(Change::Updated, s.clone());
            fx.refresh();
            assert_eq!(unread(&fx), ["r0001"]);

            // Marked read in the same update that opened it.
            s.unread = 0;
            fx.ws.apply_subscription(Change::Updated, s);
            open(&mut fx, Some("r0001"));
            assert_eq!(unread(&fx), ["r0001"]);
            open(&mut fx, Some("r0001"));
            assert_eq!(unread(&fx), ["r0001"]);

            open(&mut fx, Some("r0000"));
            assert!(unread(&fx).is_empty());
            open(&mut fx, None);
            assert!(unread(&fx).is_empty());
        }

        /// Review: image results (failures, other servers) and typing in
        /// rooms off screen rebuilt every row's props; avatars of all rooms
        /// were requested in row order.
        #[test]
        fn only_visible_rows_are_built_and_ask_for_avatars() {
            rust_i18n::set_locale("en");
            let mut fx = Fx::new();
            fx.refresh();
            let mut tree = Tree::new(fx.view().as_widget());
            let queued = fx.images.queued();
            assert!(
                queued > 0 && queued < 64,
                "avatars of the rows on screen only: {queued}"
            );
            assert_eq!(fx.diff(&mut tree), (0, 0));

            // A failed image and another server's image change nothing here.
            let wanted = fx.images.take_wanted(|_| true);
            fx.images
                .finish(&fx.sid, 0, wanted[0].url.clone(), Err("x".into()));
            assert_eq!(fx.diff(&mut tree), (0, 0));
            let other = ServerId("other".into());
            fx.images.get(&other, "http://other/avatar/x");
            fx.images.take_wanted(|_| true);
            fx.images.finish(
                &other,
                0,
                "http://other/avatar/x".into(),
                Ok(crate::images::Fetched::Initials),
            );
            assert_eq!(fx.diff(&mut tree), (0, 0));

            // Typing in a room far below: no row is rebuilt.
            let far = format!("r{:04}", ROOMS - 5);
            fx.ws
                .apply_user_activity(&far, "bob", &["user-typing".into()], None);
            assert_eq!(fx.diff(&mut tree).1, 0);

            // Scrolling there builds (and asks for) the rows on screen only;
            // the content height stays the same.
            scroll_to(&mut fx, &far);
            let (bodies, rows) = fx.diff(&mut tree);
            assert_eq!(bodies, 1);
            assert!((10..64).contains(&rows), "{rows}");
            let (start, end) = fx.sidebar.window(&SidebarCtx {
                server_id: &fx.sid,
                server: &fx.url,
                workspace: &fx.ws,
                images: &fx.images,
                prefs: &fx.prefs,
                collapsed: &fx.collapsed,
                active_room: None,
                active_thread: None,
                now: Instant::now(),
            });
            assert!(start > 200 && end <= ROOMS + 3, "{start}..{end}");
            // Small scrolls inside the built range do not rebuild.
            let (o, h, c) = fx.sidebar.viewport.unwrap();
            fx.sidebar.viewport = Some((o + 20.0, h, c));
            assert_eq!(fx.diff(&mut tree), (0, 0));
            // A stale offset past the end (the list got shorter) still shows
            // the last rows.
            fx.sidebar.viewport = Some((1e7, h, c));
            fx.diff(&mut tree);
            fx.sidebar.filter = "r0001".into();
            fx.diff(&mut tree);
            assert_eq!(fx.sidebar.room_count(), 1);
            let ctx = SidebarCtx {
                server_id: &fx.sid,
                server: &fx.url,
                workspace: &fx.ws,
                images: &fx.images,
                prefs: &fx.prefs,
                collapsed: &fx.collapsed,
                active_room: None,
                active_thread: None,
                now: Instant::now(),
            };
            assert_eq!(fx.sidebar.window(&ctx), (0, 2));
        }

        /// Builds and lays out the sidebar at 300x800; returns the rows built
        /// and the time it took.
        fn build_and_layout(fx: &Fx) -> (usize, std::time::Duration) {
            use iced_runtime::user_interface::{Cache, UserInterface};
            let mut renderer = iced::Renderer::Secondary(iced_tiny_skia::Renderer::new(
                rustket_ui::theme::font_regular(),
                iced::Pixels(rustket_ui::theme::TEXT_BODY),
            ));
            BUILDS.with(|b| b.set((0, 0)));
            let t = Instant::now();
            let ui = UserInterface::build(
                fx.view(),
                iced::Size::new(300.0, 800.0),
                Cache::default(),
                &mut renderer,
            );
            let took = t.elapsed();
            drop(ui);
            (BUILDS.with(std::cell::Cell::get).1, took)
        }

        /// Phase 13: the first build (and any full rebuild) of a 2000-room
        /// list builds and lays out only the rows near the viewport (without
        /// virtualisation it built all 2001).
        #[test]
        fn first_build_lays_out_only_the_rows_on_screen() {
            rust_i18n::set_locale("en");
            let mut fx = Fx::with_rooms(2000);
            fx.refresh();
            let total = total(&fx.sidebar);
            fx.sidebar.viewport = Some((0.0, 800.0, total));
            let (rows_virtual, _) = build_and_layout(&fx);
            assert!(rows_virtual < 48, "{rows_virtual}");
            // Scrolled to the middle: still only the rows around it.
            fx.sidebar.viewport = Some((total / 2.0, 800.0, total));
            let (rows_mid, _) = build_and_layout(&fx);
            assert!(rows_mid < 64, "{rows_mid}");
        }

        /// The timings behind the row counts (wall clock, so not part of
        /// the normal run). "Before" is the list with a window covering the
        /// rows; building all 2001 takes a minute in a debug build, so it is
        /// measured on 192 rows and extrapolated (rows cost the same each): `cargo test -p rustket sidebar_build_timings --
        /// --ignored --nocapture`.
        #[test]
        #[ignore = "timing benchmark"]
        fn sidebar_build_timings() {
            rust_i18n::set_locale("en");
            let mut fx = Fx::with_rooms(2000);
            fx.refresh();
            let total = total(&fx.sidebar);
            fx.sidebar.viewport = Some((0.0, 800.0, total));
            // Warm up fonts and caches so the timings compare the lists.
            build_and_layout(&fx);
            let (rows_virtual, virtual_time) = build_and_layout(&fx);
            let many = 192.0 * row_height(Density::Extended);
            fx.sidebar.viewport = Some((0.0, many, total));
            let (rows_many, many_time) = build_and_layout(&fx);
            let all_time = many_time.mul_f64(2001.0 / rows_many as f64);
            println!(
                "2000 rooms: virtual {rows_virtual} rows in {virtual_time:?}; \
                 {rows_many} rows in {many_time:?}, all 2001 ~{all_time:?}"
            );
            assert!(rows_many >= 192, "{rows_many}");
        }

        /// Alt+Up/Down to a room off screen scrolls its row into view;
        /// rows in view and rooms not in the list need no scroll.
        #[test]
        fn reveal_scrolls_the_opened_room_into_view() {
            rust_i18n::set_locale("en");
            let mut fx = Fx::new();
            fx.refresh();
            let total = total(&fx.sidebar);
            let row_h = row_height(Density::Extended);
            fx.sidebar.viewport = Some((0.0, 600.0, total));
            let none = BTreeSet::new();
            let d = Density::Extended;
            assert_eq!(fx.sidebar.reveal_offset("r0002", d, &none), None);
            assert_eq!(fx.sidebar.reveal_offset("nope", d, &none), None);
            // Down to r0250: its row ends at the bottom of the viewport.
            let to = fx.sidebar.reveal_offset("r0250", d, &none).unwrap();
            let row_top = HEADER_H + 250.0 * row_h;
            assert!((to - (row_top + row_h - 600.0)).abs() < 0.5, "{to}");
            // Scrolled there, back up to r0000: its row at the top.
            fx.sidebar.viewport = Some((to, 600.0, total));
            assert_eq!(fx.sidebar.reveal_offset("r0250", d, &none), None);
            let up = fx.sidebar.reveal_offset("r0000", d, &none).unwrap();
            assert!((up - HEADER_H).abs() < 0.5, "{up}");
            // Omnichannel groups drawn above the list shift it down.
            fx.sidebar.viewport = Some((0.0, 600.0, total + 100.0));
            let to = fx.sidebar.reveal_offset("r0250", d, &none).unwrap();
            assert!((to - (100.0 + row_top + row_h - 600.0)).abs() < 0.5, "{to}");
            // A room in a collapsed section has no row.
            let channels: BTreeSet<String> = [SectionId::from(SectionKind::Channels)
                .collapse_key()
                .into_owned()]
            .into();
            assert_eq!(fx.sidebar.reveal_offset("r0250", d, &channels), None);
        }

        /// A group gets its name as the title and collapses by its own key;
        /// built-ins keep their old collapse keys.
        #[test]
        fn group_sections_collapse_by_group_key() {
            use rustket_core::sidebar_layout::{GroupType, LayoutEdit, apply};
            let mut fx = Fx::with_rooms(3);
            apply(
                &mut fx.prefs,
                LayoutEdit::AddGroup {
                    name: "People".into(),
                    types: vec![GroupType::DirectMessages],
                },
            );
            let gid = fx.prefs.sidebar_groups[0].id.clone();
            fx.refresh();
            let group = &fx.sidebar.sections[1];
            assert_eq!(group.id, SectionId::group(&gid));
            assert_eq!(section_title(group), "People");
            assert!(matches!(
                fx.sidebar.update(Message::Toggle(group.id.clone())),
                Out::ToggleSection(k) if k == format!("sidebar.group.{gid}")
            ));
            assert!(matches!(
                fx.sidebar.update(Message::Toggle(SectionKind::Channels.into())),
                Out::ToggleSection(k) if k == "sidebar.channels"
            ));
            fx.collapsed.insert(format!("sidebar.group.{gid}"));
            let listed: Vec<(String, bool)> = fx
                .sidebar
                .listed_with(&fx.collapsed)
                .map(|(s, c)| (s.id.as_config_str(), c))
                .collect();
            assert_eq!(
                listed,
                [
                    ("channels".to_owned(), false),
                    (format!("group:{gid}"), true)
                ]
            );
            let order = crate::omnichannel::sidebar::step_order(
                &fx.sid,
                &fx.ws,
                &fx.sidebar.sections,
                &fx.collapsed,
            );
            assert!(order.iter().all(|i| i.rid != "dm"));
        }

        /// With "unread first" (and no Unread section) the opened unread room
        /// keeps its place in the unread block until another room opens.
        #[test]
        fn the_open_room_stays_in_the_unread_block() {
            use rustket_core::sidebar_layout::{LayoutEdit, apply};
            fn channels(fx: &Fx) -> Vec<String> {
                fx.sidebar
                    .sections
                    .iter()
                    .filter(|s| s.is(SectionKind::Channels))
                    .flat_map(|s| s.items.iter().map(|i| i.rid.clone()))
                    .collect()
            }
            fn open(fx: &mut Fx, rid: Option<&str>) {
                fx.sidebar.track_active(&fx.ws, rid, None, &fx.prefs);
                fx.refresh();
            }
            let mut fx = Fx::with_rooms(3);
            apply(
                &mut fx.prefs,
                LayoutEdit::SetUnreadFirst {
                    id: SectionKind::Channels.into(),
                    on: Some(true),
                },
            );
            let mut s = sub("r0002", "r0002", RoomType::Channel);
            s.unread = 2;
            fx.ws.apply_subscription(Change::Updated, s.clone());
            fx.refresh();
            assert_eq!(channels(&fx), ["r0002", "r0000", "r0001"]);

            s.unread = 0;
            fx.ws.apply_subscription(Change::Updated, s);
            open(&mut fx, Some("r0002"));
            assert_eq!(channels(&fx), ["r0002", "r0000", "r0001"]);
            open(&mut fx, Some("r0001"));
            assert_eq!(channels(&fx), ["r0000", "r0001", "r0002"]);
        }

        /// A room with hidden unread status and a mention has a counter but
        /// is not unread: opening it never moves it to the Unread section;
        /// with "unread first" it keeps its place in the unread block.
        #[test]
        fn a_hidden_unread_room_is_not_moved_to_unread_when_opened() {
            use rustket_core::sidebar_layout::{LayoutEdit, apply};
            fn rids(fx: &Fx, kind: SectionKind) -> Vec<String> {
                fx.sidebar
                    .sections
                    .iter()
                    .filter(|s| s.is(kind))
                    .flat_map(|s| s.items.iter().map(|i| i.rid.clone()))
                    .collect()
            }
            fn open(fx: &mut Fx, rid: Option<&str>) {
                fx.sidebar.track_active(&fx.ws, rid, None, &fx.prefs);
                fx.refresh();
            }
            let mut fx = Fx::with_rooms(3);
            fx.prefs.unread_on_top = true;
            let mut s = sub("r0002", "r0002", RoomType::Channel);
            s.unread = 1;
            s.user_mentions = 1;
            s.hide_unread_status = Some(true);
            fx.ws.apply_subscription(Change::Updated, s.clone());
            fx.refresh();
            assert!(rids(&fx, SectionKind::Unread).is_empty());
            let badge = &fx.sidebar.sections[0].items[2].badge;
            assert_eq!(badge.count, 1, "a counter, but not unread");
            open(&mut fx, Some("r0002"));
            assert!(rids(&fx, SectionKind::Unread).is_empty());
            assert_eq!(
                rids(&fx, SectionKind::Channels),
                ["r0000", "r0001", "r0002"]
            );

            open(&mut fx, None);
            apply(
                &mut fx.prefs,
                LayoutEdit::SetUnreadFirst {
                    id: SectionKind::Channels.into(),
                    on: Some(true),
                },
            );
            fx.refresh();
            assert_eq!(
                rids(&fx, SectionKind::Channels),
                ["r0002", "r0000", "r0001"]
            );
            s.unread = 0;
            s.user_mentions = 0;
            fx.ws.apply_subscription(Change::Updated, s);
            open(&mut fx, Some("r0002"));
            assert!(rids(&fx, SectionKind::Unread).is_empty());
            assert_eq!(
                rids(&fx, SectionKind::Channels),
                ["r0002", "r0000", "r0001"]
            );
            open(&mut fx, Some("r0000"));
            assert_eq!(
                rids(&fx, SectionKind::Channels),
                ["r0000", "r0001", "r0002"]
            );
        }

        /// Threads mode Unread with `n` unread threads `t000…` in r0000
        /// (parents known except for the `missing` last ones).
        fn with_threads(fx: &mut Fx, n: usize, missing: usize) {
            use rustket_core::config::ThreadsMode;
            fx.prefs.sidebar_threads = ThreadsMode::Unread;
            fx.ws.set_thread_config(ThreadsMode::Unread, []);
            let tmids: Vec<String> = (0..n).map(|i| format!("t{i:03}")).collect();
            let mut s = sub("r0000", "r0000", RoomType::Channel);
            s.tunread = Some(tmids.clone());
            fx.ws.apply_subscription(Change::Updated, s);
            let parents = tmids
                .iter()
                .enumerate()
                .take(n - missing)
                .map(|(i, t)| parent(t, "r0000", i))
                .collect();
            fx.ws.apply_thread_parents(parents);
        }

        /// Parent `tmid` whose last reply is newer the smaller `i` is.
        fn parent(tmid: &str, rid: &str, i: usize) -> rocketchat::models::Message {
            rocketchat::models::Message {
                id: tmid.into(),
                rid: rid.into(),
                msg: format!("topic {tmid}"),
                tcount: Some(2),
                tlm: RcDate::from_millis(1_750_000_000_000 - i as i64 * 1000),
                u: UserRef {
                    id: "bobid".into(),
                    username: Some("bob".into()),
                    name: None,
                },
                ..Default::default()
            }
        }

        fn threads(sb: &Sidebar) -> Vec<String> {
            sb.sections
                .iter()
                .filter(|s| s.is(SectionKind::Threads))
                .flat_map(|s| s.threads.iter().map(|t| t.tmid.clone()))
                .collect()
        }

        fn ctx(fx: &Fx) -> SidebarCtx<'_> {
            SidebarCtx {
                server_id: &fx.sid,
                server: &fx.url,
                workspace: &fx.ws,
                images: &fx.images,
                prefs: &fx.prefs,
                collapsed: &fx.collapsed,
                active_room: None,
                active_thread: None,
                now: Instant::now(),
            }
        }

        /// Thread rows take part in the windowing: only those near the
        /// viewport are built; the slots count them.
        #[test]
        fn thread_rows_are_built_only_in_the_window() {
            rust_i18n::set_locale("en");
            let mut fx = Fx::with_rooms(5);
            with_threads(&mut fx, 300, 0);
            fx.prefs.sidebar_threads_limit = 0;
            fx.refresh();
            assert_eq!(threads(&fx.sidebar).len(), 300);
            BUILDS.with(|b| b.set((0, 0)));
            let mut tree = Tree::new(fx.view().as_widget());
            let (_, rows) = BUILDS.with(std::cell::Cell::get);
            assert!((20..64).contains(&rows), "only the rows on screen: {rows}");
            // Threads (header + 300) and Channels (header + 5) and the DM.
            let (start, end) = fx.sidebar.window(&ctx(&fx));
            assert_eq!(start, 0);
            assert!(end < 64, "{end}");
            // Scrolled to the channels below the threads: those rows only.
            let row_h = row_height(Density::Extended);
            let total = total_len(&fx.sidebar);
            fx.sidebar.viewport = Some((total - 600.0, 600.0, total));
            let (start, end) = fx.sidebar.window(&ctx(&fx));
            assert!(
                start > 250 && end == 1 + 300 + 1 + 5 + 1 + 1,
                "{start}..{end}"
            );
            let (bodies, rows) = fx.diff(&mut tree);
            assert_eq!(bodies, 1);
            assert!(rows < 64 && (rows as f32) > 600.0 / row_h, "{rows}");
        }

        /// A parent that arrives for a placeholder row rebuilds that row only.
        #[test]
        fn a_new_parent_rebuilds_only_its_row() {
            rust_i18n::set_locale("en");
            let mut fx = Fx::with_rooms(3);
            with_threads(&mut fx, 5, 1);
            fx.refresh();
            let rows = &fx.sidebar.sections[0].threads;
            assert_eq!(rows.len(), 5);
            assert!(!rows[4].is_loaded(), "placeholder last: no activity yet");
            let mut tree = Tree::new(fx.view().as_widget());
            assert_eq!(fx.diff(&mut tree), (0, 0));
            // Oldest of all: it stays the last row.
            fx.ws
                .apply_thread_parents(vec![parent("t004", "r0000", 10)]);
            assert_eq!(fx.diff(&mut tree), (1, 1));
            assert!(fx.sidebar.sections[0].threads[4].is_loaded());
        }

        /// The open thread stays listed once read (closing the panel keeps
        /// it) until another thread or room opens.
        #[test]
        fn the_open_thread_stays_listed_until_another_opens() {
            fn open(fx: &mut Fx, rid: Option<&str>, tmid: Option<&str>) {
                fx.sidebar.track_active(&fx.ws, rid, tmid, &fx.prefs);
                fx.refresh();
            }
            fn read_all(fx: &mut Fx) {
                let s = sub("r0000", "r0000", RoomType::Channel);
                fx.ws.apply_subscription(Change::Updated, s);
            }
            let mut fx = Fx::with_rooms(3);
            with_threads(&mut fx, 2, 0);
            fx.refresh();
            assert_eq!(threads(&fx.sidebar), ["t000", "t001"]);

            // Marked read in the same update that opened it.
            read_all(&mut fx);
            open(&mut fx, Some("r0000"), Some("t001"));
            assert_eq!(threads(&fx.sidebar), ["t001"]);
            // The panel closed (or shows something else): still listed.
            open(&mut fx, Some("r0000"), None);
            assert_eq!(threads(&fx.sidebar), ["t001"]);
            open(&mut fx, Some("r0000"), Some("t001"));
            assert_eq!(threads(&fx.sidebar), ["t001"]);
            // Another thread that is not listed: the kept one goes.
            open(&mut fx, Some("r0000"), Some("t000"));
            assert!(threads(&fx.sidebar).is_empty());

            // Another room drops it too.
            with_threads(&mut fx, 2, 0);
            fx.refresh();
            read_all(&mut fx);
            open(&mut fx, Some("r0000"), Some("t000"));
            assert_eq!(threads(&fx.sidebar), ["t000"]);
            open(&mut fx, Some("r0001"), None);
            assert!(threads(&fx.sidebar).is_empty());
        }

        /// The limit cuts the thread rows (counted in the section); changing
        /// it recomputes the sections.
        #[test]
        fn thread_rows_follow_the_limit() {
            rust_i18n::set_locale("en");
            let mut fx = Fx::with_rooms(3);
            with_threads(&mut fx, 8, 0);
            fx.prefs.sidebar_threads_limit = 5;
            fx.refresh();
            assert_eq!(
                threads(&fx.sidebar),
                ["t000", "t001", "t002", "t003", "t004"]
            );
            assert_eq!(fx.sidebar.sections[0].threads_hidden, 3);
            let _ = fx.view();
            fx.prefs.sidebar_threads_limit = 0;
            fx.refresh();
            assert_eq!(threads(&fx.sidebar).len(), 8);
            assert_eq!(fx.sidebar.sections[0].threads_hidden, 0);
            assert_eq!(threads_limit_label(0), "Unlimited");
            assert_eq!(threads_limit_label(50), "50");
        }

        /// Opening a read thread that is listed anyway (kept or followed)
        /// does not move it into the unread block.
        #[test]
        fn opening_a_read_listed_thread_keeps_the_order() {
            use rustket_core::config::ThreadsMode;
            let mut fx = Fx::with_rooms(3);
            with_threads(&mut fx, 3, 0);
            fx.prefs.sidebar_threads = ThreadsMode::UnreadKept;
            fx.ws.set_thread_config(
                ThreadsMode::UnreadKept,
                [("r0000", "t001"), ("r0000", "t002")],
            );
            let mut s = sub("r0000", "r0000", RoomType::Channel);
            s.tunread = Some(vec!["t000".into()]);
            fx.ws.apply_subscription(Change::Updated, s);
            fx.refresh();
            assert_eq!(threads(&fx.sidebar), ["t000", "t001", "t002"]);
            fx.sidebar
                .track_active(&fx.ws, Some("r0000"), Some("t002"), &fx.prefs);
            fx.refresh();
            assert_eq!(threads(&fx.sidebar), ["t000", "t001", "t002"]);
            // With a limit, a new unread thread does not push the open one
            // out: it takes the last row.
            fx.prefs.sidebar_threads_limit = 3;
            let mut s = sub("r0000", "r0000", RoomType::Channel);
            s.tunread = Some(vec!["t000".into(), "t003".into()]);
            fx.ws.apply_subscription(Change::Updated, s);
            fx.ws.apply_thread_parents(vec![parent("t003", "r0000", 3)]);
            fx.sidebar
                .track_active(&fx.ws, Some("r0000"), Some("t002"), &fx.prefs);
            fx.refresh();
            assert_eq!(threads(&fx.sidebar), ["t000", "t003", "t002"]);
            // Its panel closed: cut like any other row.
            fx.sidebar
                .track_active(&fx.ws, Some("r0000"), None, &fx.prefs);
            fx.refresh();
            assert_eq!(threads(&fx.sidebar), ["t000", "t003", "t001"]);
        }

        /// Alt+Up/Down steps through rooms only, skipping the thread rows;
        /// revealing a room counts the thread rows above it.
        #[test]
        fn thread_rows_are_skipped_by_steps_and_counted_by_reveal() {
            let mut fx = Fx::with_rooms(300);
            with_threads(&mut fx, 20, 0);
            fx.refresh();
            assert!(fx.sidebar.sections[0].is(SectionKind::Threads));
            let order = crate::omnichannel::sidebar::step_order(
                &fx.sid,
                &fx.ws,
                &fx.sidebar.sections,
                &fx.collapsed,
            );
            assert_eq!(order.len(), 301);
            assert_eq!(order[0].rid, "r0000");

            let row_h = row_height(Density::Extended);
            let total = total_len(&fx.sidebar);
            fx.sidebar.viewport = Some((0.0, 600.0, total));
            let none = BTreeSet::new();
            let d = Density::Extended;
            // Threads header + 20 rows, Channels header, 250 rooms.
            let row_top = 2.0 * HEADER_H + (20.0 + 250.0) * row_h;
            let to = fx.sidebar.reveal_offset("r0250", d, &none).unwrap();
            assert!((to - (row_top + row_h - 600.0)).abs() < 0.5, "{to}");
            // Collapsed threads take no rows.
            let collapsed: BTreeSet<String> = [SectionId::from(SectionKind::Threads)
                .collapse_key()
                .into_owned()]
            .into();
            fx.sidebar.viewport = Some((0.0, 600.0, total - 20.0 * row_h));
            let to = fx.sidebar.reveal_offset("r0250", d, &collapsed).unwrap();
            let row_top = 2.0 * HEADER_H + 250.0 * row_h;
            assert!((to - (row_top + row_h - 600.0)).abs() < 0.5, "{to}");
        }

        /// Thread rows and rooms light a collapsed header with their
        /// strongest kind: plain thread replies are grey, room unread beats
        /// them, mentions beat both.
        #[test]
        fn collapsed_header_takes_strongest_row() {
            rust_i18n::set_locale("en");
            let mut fx = Fx::with_rooms(3);
            with_threads(&mut fx, 2, 1);
            assert!(
                fx.sidebar
                    .sections
                    .iter()
                    .all(|s| collapsed_unread_status(s).is_none())
            );
            let mut s = sub("r0000", "r0000", RoomType::Channel);
            s.tunread = Some(vec!["t000".into()]);
            fx.ws.apply_subscription(Change::Updated, s);
            fx.refresh();
            let threads = fx.sidebar.sections[0].clone();
            assert!(threads.is(SectionKind::Threads));
            let kinds = |sec: &Section| -> Vec<Option<BadgeKind>> {
                sec.items
                    .iter()
                    .map(room_badge_kind)
                    .chain(sec.threads.iter().map(thread_badge_kind))
                    .collect()
            };
            assert!(kinds(&threads).contains(&Some(BadgeKind::Muted)));
            // A channel with only an unread thread: the quiet thread colour,
            // its counter still counts the thread.
            let r0 = fx
                .sidebar
                .sections
                .iter()
                .flat_map(|s| &s.items)
                .find(|i| i.rid == "r0000")
                .unwrap()
                .clone();
            assert_eq!(room_badge_kind(&r0), Some(BadgeKind::Thread));
            let p = fx.sidebar.row_props(&r0, &ctx(&fx));
            assert_eq!((p.unread, p.badge_kind()), (1, Some(BadgeKind::Thread)));
            assert!(collapsed_unread_status(&threads).is_some());
            let mut s = sub("r0001", "r0001", RoomType::Channel);
            s.unread = 2;
            fx.ws.apply_subscription(Change::Updated, s);
            fx.refresh();
            let sec = fx
                .sidebar
                .sections
                .iter()
                .find(|s| s.items.iter().any(|i| i.rid == "r0001"))
                .unwrap();
            assert!(kinds(sec).contains(&Some(BadgeKind::Unread)));
            assert!(collapsed_unread_status(sec).is_some());
            let max = [BadgeKind::Muted, BadgeKind::Unread, BadgeKind::Mention]
                .into_iter()
                .max_by_key(|&k| badge_rank(k));
            assert_eq!(max, Some(BadgeKind::Mention));
            assert!(badge_rank(BadgeKind::Unread) > badge_rank(BadgeKind::Thread));

            // Hidden unread status: no badge, also not in a collapsed header.
            let mut s = sub("r0002", "r0002", RoomType::Channel);
            s.tunread = Some(vec!["t9".into()]);
            s.user_mentions = 1;
            s.hide_unread_status = Some(true);
            s.hide_mention_status = Some(true);
            fx.ws.apply_subscription(Change::Updated, s);
            fx.refresh();
            let r2 = fx
                .sidebar
                .sections
                .iter()
                .flat_map(|s| &s.items)
                .find(|i| i.rid == "r0002")
                .unwrap()
                .clone();
            assert_eq!(room_badge_kind(&r2), None);
            assert_eq!(fx.sidebar.row_props(&r2, &ctx(&fx)).badge_kind(), None);
        }

        /// Row contents: unread dot without a count, the room in the
        /// subtitle (in the title when condensed), placeholders and the
        /// context menu by state; opening and menu items map to outputs.
        #[test]
        fn thread_row_props_menu_and_outputs() {
            rust_i18n::set_locale("en");
            let mut fx = Fx::with_rooms(3);
            with_threads(&mut fx, 2, 1);
            let mut s = sub("r0000", "r0000", RoomType::Channel);
            s.tunread = Some(vec!["t000".into(), "t001".into()]);
            s.tunread_user = Some(vec!["t000".into()]);
            fx.ws.apply_subscription(Change::Updated, s);
            fx.refresh();
            let rows = fx.sidebar.sections[0].threads.clone();
            let c = ctx(&fx);
            let p = thread_row_props(&rows[0], &c);
            assert_eq!(p.title, "topic t000");
            assert_eq!(p.subtitle.as_deref(), Some("r0000 · Replies: 2"));
            assert_eq!(p.badge_dot, Some(BadgeKind::Mention));
            assert_eq!(p.badge_kind(), Some(BadgeKind::Mention));
            assert_eq!((p.unread, p.user_mentions), (0, 0), "no count");
            let p = thread_row_props(&rows[1], &c);
            assert_eq!(p.title, "Loading…");
            assert_eq!(p.subtitle.as_deref(), Some("r0000"));
            assert_eq!(p.badge_dot, Some(BadgeKind::Muted));
            fx.ws.thread_gone("t001");
            fx.refresh();
            let gone = fx.sidebar.sections[0].threads[1].clone();
            assert_eq!(
                thread_row_props(&gone, &ctx(&fx)).title,
                "Message unavailable"
            );

            fx.prefs.density = Density::Condensed;
            let p = thread_row_props(&rows[0], &ctx(&fx));
            assert_eq!(p.title, "r0000 › topic t000");

            let labels = |t: &ThreadListItem, kept_enabled: bool| -> Vec<String> {
                thread_menu(t, kept_enabled, false, None)
                    .into_iter()
                    .filter_map(|e| match e {
                        MenuEntry::Item(i) => Some(i.label),
                        _ => None,
                    })
                    .collect()
            };
            // Unfollow only for a followed thread.
            assert_eq!(labels(&rows[0], false), ["Open room", "Mark as read"]);
            let mut followed = rows[0].clone();
            followed.following = true;
            assert_eq!(
                labels(&followed, false),
                ["Open room", "Mark as read", "Unfollow thread"]
            );
            assert_eq!(
                labels(&followed, true),
                [
                    "Open room",
                    "Mark as read",
                    "Keep in sidebar",
                    "Unfollow thread"
                ]
            );
            // `chat.readThread` fails without the parent: the room is read.
            assert_eq!(labels(&gone, true), ["Open room", "Mark room as read"]);
            let mut kept = followed.clone();
            kept.kept = true;
            kept.unread = None;
            assert_eq!(
                labels(&kept, true),
                ["Open room", "Remove from sidebar", "Unfollow thread"]
            );
            // Starring comes before keeping; a placeholder row can only be
            // unstarred.
            let star_labels = |t: &ThreadListItem| -> Vec<String> {
                thread_menu(t, true, true, None)
                    .into_iter()
                    .filter_map(|e| match e {
                        MenuEntry::Item(i) => Some(i.label),
                        _ => None,
                    })
                    .collect()
            };
            assert_eq!(
                star_labels(&kept),
                [
                    "Open room",
                    "Add thread to Favorites",
                    "Remove from sidebar",
                    "Unfollow thread"
                ]
            );
            let mut starred = kept.clone();
            starred.starred = true;
            assert_eq!(
                star_labels(&starred)[1..2],
                ["Remove thread from Favorites"]
            );
            let mut gone_starred = gone.clone();
            assert!(!star_labels(&gone_starred).contains(&"Add thread to Favorites".to_owned()));
            gone_starred.starred = true;
            assert!(
                star_labels(&gone_starred).contains(&"Remove thread from Favorites".to_owned())
            );

            let (rid, tmid) = ("r0000".to_owned(), "t000".to_owned());
            let act = |act| Message::ThreadAct {
                rid: rid.clone(),
                tmid: tmid.clone(),
                act,
            };
            let sb = &mut fx.sidebar;
            assert!(matches!(
                sb.update(Message::OpenThread { rid: rid.clone(), tmid: tmid.clone() }),
                Out::OpenThread { rid, tmid } if rid == "r0000" && tmid == "t000"
            ));
            assert!(
                matches!(sb.update(act(ThreadAct::OpenRoom)), Out::OpenRoom(r) if r == "r0000")
            );
            assert!(matches!(
                sb.update(act(ThreadAct::MarkRead)),
                Out::ThreadCall {
                    call: ThreadCall::MarkRead,
                    ..
                }
            ));
            assert!(matches!(
                sb.update(act(ThreadAct::MarkRoomRead)),
                Out::ThreadCall {
                    rid,
                    call: ThreadCall::MarkRoomRead,
                    ..
                } if rid == "r0000"
            ));
            assert!(matches!(
                sb.update(act(ThreadAct::Unfollow)),
                Out::ThreadCall {
                    call: ThreadCall::Unfollow,
                    ..
                }
            ));
            assert!(matches!(
                sb.update(act(ThreadAct::Keep)),
                Out::KeepThread { keep: true, .. }
            ));
            assert!(matches!(
                sb.update(act(ThreadAct::Unkeep)),
                Out::KeepThread { keep: false, .. }
            ));
            assert!(matches!(
                sb.update(act(ThreadAct::Star)),
                Out::StarThread { rid, tmid, on: true } if rid == "r0000" && tmid == "t000"
            ));
            assert!(matches!(
                sb.update(act(ThreadAct::Unstar)),
                Out::StarThread { on: false, .. }
            ));
        }

        /// The row's menu depends on more than its props: a change of the
        /// kept flag, the read state or the "keep" availability rebuilds it.
        #[test]
        fn thread_row_key_covers_the_menu_state() {
            use rustket_core::config::ThreadsMode;
            rust_i18n::set_locale("en");
            let mut fx = Fx::with_rooms(3);
            with_threads(&mut fx, 2, 0);
            fx.refresh();
            let mut tree = Tree::new(fx.view().as_widget());
            assert_eq!(fx.diff(&mut tree), (0, 0));
            // Keeping becomes available (mode change): both rows rebuilt.
            fx.prefs.sidebar_threads = ThreadsMode::UnreadKept;
            fx.ws.set_thread_config(ThreadsMode::UnreadKept, []);
            assert_eq!(fx.diff(&mut tree), (1, 2));
            // One thread kept: its row only.
            fx.ws
                .set_thread_config(ThreadsMode::UnreadKept, [("r0000", "t001")]);
            assert_eq!(fx.diff(&mut tree), (1, 1));
            // Starring becomes available: both rows rebuilt.
            fx.ws.set_starred_threads(true);
            assert_eq!(fx.diff(&mut tree), (1, 2));
        }

        /// Favorites with starred threads: a refresh in its header menu, an
        /// unread dot while collapsed.
        #[test]
        fn favorites_with_starred_threads() {
            use rustket_core::config::ThreadsMode;
            rust_i18n::set_locale("en");
            let mut fx = Fx::with_rooms(3);
            fx.ws.set_thread_config(ThreadsMode::Off, []);
            fx.ws.set_starred_threads(true);
            fx.ws.set_thread_starred("r0000", "t", true);
            fx.refresh();
            let menus = header_menus(&fx);
            let refresh = |m: &Message| matches!(m, Message::Header(HeaderAct::RefreshStarred));
            assert_eq!(items(menu_of(&menus, "favorites"), refresh).len(), 1);
            assert!(items(menu_of(&menus, "channels"), refresh).is_empty());
            assert!(matches!(
                fx.sidebar
                    .update(Message::Header(HeaderAct::RefreshStarred)),
                Out::RefreshStarred
            ));
            let fav = fx
                .sidebar
                .sections
                .iter()
                .find(|s| s.is(SectionKind::Favorites))
                .unwrap()
                .clone();
            assert!(collapsed_unread_status(&fav).is_none());
            let mut s = sub("r0000", "r0000", RoomType::Channel);
            s.tunread = Some(vec!["t".into()]);
            fx.ws.apply_subscription(Change::Updated, s);
            fx.refresh();
            let fav = fx
                .sidebar
                .sections
                .iter()
                .find(|s| s.is(SectionKind::Favorites))
                .unwrap();
            assert!(fav.threads[0].unread.is_some());
            assert!(collapsed_unread_status(fav).is_some());
            // Without tracking there is no refresh.
            fx.ws.set_starred_threads(false);
            fx.refresh();
            let menus = header_menus(&fx);
            assert!(menus.iter().all(|(_, m)| items(m, refresh).is_empty()));
        }

        fn set_flags(ws: &mut Workspace, edits: &[FlagEdit]) {
            let mut overlay = ws.flag_overlay().clone();
            for e in edits {
                overlay.edit(ws.flag_blob(), e, 1_000, "dev").unwrap();
            }
            ws.set_flag_overlay(overlay);
        }

        fn item_labels(menu: &[MenuEntry<Message>]) -> Vec<String> {
            menu.iter()
                .filter_map(|e| match e {
                    MenuEntry::Item(i) => Some(i.label.clone()),
                    _ => None,
                })
                .collect()
        }

        /// Room rows: the flag mark (named by the user), a context menu with
        /// Favorites and the flag block, mapped to outputs; none for
        /// omnichannel rows.
        #[test]
        fn room_row_menu_flag_items() {
            use rustket_core::flags::FlagColor::{Blue, Red};
            use rustket_ui::widget::FlagMark;
            rust_i18n::set_locale("en");
            let mut fx = Fx::with_rooms(3);
            set_flags(
                &mut fx.ws,
                &[
                    FlagEdit::Room {
                        rid: "r0001".into(),
                        color: Some(Red),
                    },
                    FlagEdit::Rename {
                        color: Red,
                        name: Some("Urgent".into()),
                    },
                ],
            );
            fx.refresh();
            let section = fx
                .sidebar
                .sections
                .iter()
                .find(|s| s.id == SectionId::Flag(Red))
                .unwrap()
                .clone();
            assert_eq!(section_title(&section), "Urgent");
            let item = section.items[0].clone();
            assert_eq!(item.rid, "r0001");
            let c = ctx(&fx);
            let p = fx.sidebar.row_props(&item, &c);
            assert_eq!(p.flag, Some(FlagMark::new(Red.index(), "Urgent")));
            let menu = RoomMenu::of(&item, &fx.ws);
            let entries = menu.entries(&item.rid, &fx.ws);
            assert_eq!(
                item_labels(&entries),
                [
                    "Add to favorites",
                    "Urgent",
                    "Orange",
                    "Yellow",
                    "Green",
                    "Blue",
                    "Purple",
                    "Remove flag"
                ]
            );
            let checked: Vec<_> = items(&entries, |_| true)
                .into_iter()
                .filter(|i| i.checked == Some(true))
                .map(|i| i.swatch)
                .collect();
            assert_eq!(checked, [Some(Red.index())]);
            // An unflagged room has nothing to remove; its row has no mark.
            let plain = fx
                .sidebar
                .sections
                .iter()
                .flat_map(|s| &s.items)
                .find(|i| i.rid == "r0000")
                .unwrap()
                .clone();
            assert!(fx.sidebar.row_props(&plain, &c).flag.is_none());
            let plain_menu = RoomMenu::of(&plain, &fx.ws).entries("r0000", &fx.ws);
            assert!(!item_labels(&plain_menu).contains(&"Remove flag".to_owned()));
            // The row key follows the flag.
            assert_ne!(
                row_key("r0001", &p),
                row_key("r0001", &fx.sidebar.row_props(&plain, &c))
            );
            // Outputs.
            assert!(matches!(
                fx.sidebar.update(Message::RoomAct {
                    rid: "r0001".into(),
                    act: RoomAct::Flag(Some(Blue)),
                }),
                Out::Flag(FlagEdit::Room { rid, color: Some(Blue) }) if rid == "r0001"
            ));
            assert!(matches!(
                fx.sidebar.update(Message::RoomAct {
                    rid: "r0001".into(),
                    act: RoomAct::Flag(None),
                }),
                Out::Flag(FlagEdit::Room { color: None, .. })
            ));
            assert!(matches!(
                fx.sidebar.update(Message::RoomAct {
                    rid: "r0000".into(),
                    act: RoomAct::Favorite(true),
                }),
                Out::Favorite { rid, on: true } if rid == "r0000"
            ));
            // A favorite offers removal.
            let mut fav = plain.clone();
            fav.favorite = true;
            let labels = item_labels(&RoomMenu::of(&fav, &fx.ws).entries("r0000", &fx.ws));
            assert_eq!(labels[0], "Remove from favorites");
            // Omnichannel rows keep their own actions.
            let mut omni = plain.clone();
            omni.kind = RoomKind::Omnichannel;
            assert!(
                RoomMenu::of(&omni, &fx.ws)
                    .entries("r0000", &fx.ws)
                    .is_empty()
            );
            // Renaming the color renames the mark.
            set_flags(
                &mut fx.ws,
                &[FlagEdit::Rename {
                    color: Red,
                    name: None,
                }],
            );
            fx.refresh();
            let item = fx
                .sidebar
                .sections
                .iter()
                .find(|s| s.id == SectionId::Flag(Red))
                .unwrap()
                .items[0]
                .clone();
            let p = fx.sidebar.row_props(&item, &ctx(&fx));
            assert_eq!(p.flag, Some(FlagMark::new(Red.index(), "Red")));
        }

        /// Thread rows: the flag block after the other items, mapped to a
        /// thread edit; the mark on the row.
        #[test]
        fn thread_row_flag_items() {
            use rustket_core::flags::FlagColor::Green;
            rust_i18n::set_locale("en");
            let mut fx = Fx::with_rooms(3);
            with_threads(&mut fx, 1, 0);
            fx.refresh();
            let t = fx.sidebar.sections[0].threads[0].clone();
            let menu = thread_menu(&t, false, false, Some(fx.ws.flags()));
            assert_eq!(
                item_labels(&menu),
                [
                    "Open room",
                    "Mark as read",
                    "Red",
                    "Orange",
                    "Yellow",
                    "Green",
                    "Blue",
                    "Purple"
                ]
            );
            assert!(
                menu.iter()
                    .any(|e| matches!(e, MenuEntry::Header(h) if h == "Flag"))
            );
            let green = items(&menu, |m| {
                matches!(
                    m,
                    Message::ThreadAct {
                        act: ThreadAct::Flag(Some(Green)),
                        ..
                    }
                )
            });
            assert_eq!(green.len(), 1);
            assert!(matches!(
                fx.sidebar.update(green[0].on_select.clone()),
                Out::Flag(FlagEdit::Thread { rid, tmid, color: Some(Green) })
                    if rid == "r0000" && tmid == "t000"
            ));
            assert!(crate::workspace::flag_menu::can_flag_thread(
                &fx.ws, "r0000"
            ));
            set_flags(
                &mut fx.ws,
                &[FlagEdit::Thread {
                    rid: "r0000".into(),
                    tmid: "t000".into(),
                    color: Some(Green),
                }],
            );
            fx.refresh();
            let flagged = fx
                .sidebar
                .sections
                .iter()
                .find(|s| s.id == SectionId::Flag(Green))
                .unwrap()
                .threads[0]
                .clone();
            assert_eq!(flagged.flag, Some(Green));
            assert_eq!(
                thread_row_props(&flagged, &ctx(&fx)).flag.map(|f| f.index),
                Some(Green.index())
            );
            let menu = thread_menu(&flagged, false, false, Some(fx.ws.flags()));
            assert_eq!(
                item_labels(&menu).last().map(String::as_str),
                Some("Remove flag")
            );
        }

        /// A flag section's header offers "Rename flag…", which opens the
        /// settings at that flag; other sections do not.
        #[test]
        fn flag_header_menu_items() {
            use rustket_core::flags::FlagColor::Purple;
            rust_i18n::set_locale("en");
            let mut fx = Fx::with_rooms(3);
            set_flags(
                &mut fx.ws,
                &[FlagEdit::Room {
                    rid: "r0002".into(),
                    color: Some(Purple),
                }],
            );
            fx.refresh();
            let menus = header_menus(&fx);
            let rename = |m: &Message| matches!(m, Message::Header(HeaderAct::RenameFlag(_)));
            let items_ = items(menu_of(&menus, "flag:purple"), rename);
            assert_eq!(items_.len(), 1);
            assert_eq!(items_[0].label, "Rename flag…");
            assert!(items(menu_of(&menus, "channels"), rename).is_empty());
            assert!(matches!(
                fx.sidebar.update(items_[0].on_select.clone()),
                Out::OpenSettings {
                    ungroup: None,
                    flag: Some(Purple)
                }
            ));
            // Flag sections sort and collapse like any other.
            let menu = menu_of(&menus, "flag:purple");
            assert!(
                !items(menu, |m| matches!(
                    layout_edit(m),
                    Some(LayoutEdit::SetSort { .. })
                ))
                .is_empty()
            );
            assert!(
                !items(menu, |m| matches!(
                    m,
                    Message::Toggle(SectionId::Flag(Purple))
                ))
                .is_empty()
            );
        }

        /// Content height of the list, thread rows included.
        fn total_len(sb: &Sidebar) -> f32 {
            let rows: usize = sb.sections.iter().map(Section::len).sum();
            sb.sections.len() as f32 * HEADER_H + rows as f32 * row_height(Density::Extended)
        }

        fn first_keyed(tree: &Tree) -> Option<&Tree> {
            if rustket_ui::widget::keyed_list_keys::<u64>(tree).is_some() {
                return Some(tree);
            }
            tree.children.iter().find_map(first_keyed)
        }

        /// The header menu of each listed section, keyed by its config id.
        fn header_menus(fx: &Fx) -> Vec<(String, Vec<MenuEntry<Message>>)> {
            let layout = rustket_core::sidebar_layout::Layout::from_prefs(&fx.prefs);
            let listed: Vec<(&Section, bool)> = fx.sidebar.listed_with(&fx.collapsed).collect();
            let ids: Vec<&SectionId> = listed.iter().map(|(s, _)| &s.id).collect();
            listed
                .iter()
                .enumerate()
                .map(|(i, (s, c))| {
                    let menu = fx
                        .sidebar
                        .header_menu_at(s, i, &ids, *c, &layout, &fx.prefs);
                    (s.id.as_config_str(), menu)
                })
                .collect()
        }

        fn menu_of<'m>(
            menus: &'m [(String, Vec<MenuEntry<Message>>)],
            id: &str,
        ) -> &'m [MenuEntry<Message>] {
            &menus.iter().find(|(i, _)| i == id).unwrap().1
        }

        /// The enabled items whose message matches `pred`, with their
        /// checked state.
        fn items(
            menu: &[MenuEntry<Message>],
            pred: impl Fn(&Message) -> bool,
        ) -> Vec<&rustket_ui::widget::MenuItem<Message>> {
            menu.iter()
                .filter_map(|e| match e {
                    MenuEntry::Item(i) if pred(&i.on_select) => Some(i),
                    _ => None,
                })
                .collect()
        }

        fn layout_edit(m: &Message) -> Option<&LayoutEdit> {
            match m {
                Message::Header(HeaderAct::Layout(e)) => Some(e),
                _ => None,
            }
        }

        /// Drops resolve their stable header keys against the current
        /// sections; vanished sections and own slots do nothing.
        #[test]
        fn reorder_maps_keys_to_section_ids() {
            let mut fx = Fx::with_rooms(3);
            fx.refresh();
            let channels = SectionId::from(SectionKind::Channels);
            let dms = SectionId::from(SectionKind::DirectMessages);
            let unread = SectionId::from(SectionKind::Unread);
            let mut drop = |dragged: &SectionId, before: Option<&SectionId>| match fx
                .sidebar
                .update(Message::Reorder {
                    dragged: section_key(dragged),
                    before: before.map(section_key),
                }) {
                Out::Layout(e) => Some(e),
                Out::None => None,
                other => panic!("{other:?}"),
            };
            assert_eq!(
                drop(&dms, Some(&channels)),
                Some(LayoutEdit::MoveTo {
                    id: dms.clone(),
                    anchor: channels.clone(),
                    after: false
                })
            );
            assert_eq!(
                drop(&channels, None),
                Some(LayoutEdit::MoveTo {
                    id: channels.clone(),
                    anchor: dms.clone(),
                    after: true
                })
            );
            assert_eq!(drop(&dms, None), None);
            assert_eq!(drop(&dms, Some(&dms)), None);
            // Not listed (no unread rooms): gone since the drag started.
            assert_eq!(drop(&unread, Some(&channels)), None);
            assert_eq!(drop(&channels, Some(&unread)), None);
            fx.sidebar.filter = "r0".into();
            assert!(matches!(
                fx.sidebar.update(Message::Reorder {
                    dragged: section_key(&dms),
                    before: Some(section_key(&channels)),
                }),
                Out::None
            ));
            assert!(matches!(
                fx.sidebar.update(Message::AutoScroll(-8.0)),
                Out::ScrollBy(dy) if dy == -8.0
            ));
        }

        /// The drag handles sit on the built headers: dragging the DM header
        /// above Channels publishes the matching drop.
        #[test]
        fn dragging_a_header_reorders_sections() {
            use iced::advanced::widget::{Id, Operation};
            use iced::{Point, Rectangle, Size, Vector, mouse};
            use iced_runtime::user_interface::{Cache, UserInterface};

            struct ListTop(Option<f32>);
            impl Operation for ListTop {
                fn traverse(&mut self, operate: &mut dyn FnMut(&mut dyn Operation)) {
                    operate(self);
                }
                fn scrollable(
                    &mut self,
                    id: Option<&Id>,
                    _bounds: Rectangle,
                    content: Rectangle,
                    _translation: Vector,
                    _state: &mut dyn iced::advanced::widget::operation::Scrollable,
                ) {
                    if id == Some(&Id::new(LIST_ID)) {
                        self.0 = Some(content.y);
                    }
                }
            }

            let mut fx = Fx::with_rooms(3);
            fx.refresh();
            let mut r = iced::Renderer::Secondary(iced_tiny_skia::Renderer::new(
                rustket_ui::theme::font_regular(),
                iced::Pixels(rustket_ui::theme::TEXT_BODY),
            ));
            let mut ui =
                UserInterface::build(fx.view(), Size::new(300.0, 800.0), Cache::default(), &mut r);
            let mut probe = ListTop(None);
            ui.operate(&r, &mut probe);
            let top = probe.0.unwrap();
            let row_h = row_height(fx.prefs.density);
            let dm_header = Point::new(150.0, top + HEADER_H + 3.0 * row_h + HEADER_H / 2.0);
            let above = Point::new(150.0, top + 4.0);
            let mut msgs = Vec::new();
            let ev = |e| iced::Event::Mouse(e);
            for (at, e) in [
                (
                    dm_header,
                    mouse::Event::CursorMoved {
                        position: dm_header,
                    },
                ),
                (dm_header, mouse::Event::ButtonPressed(mouse::Button::Left)),
                (above, mouse::Event::CursorMoved { position: above }),
                (above, mouse::Event::ButtonReleased(mouse::Button::Left)),
            ] {
                ui.update(
                    &[ev(e)],
                    mouse::Cursor::Available(at),
                    &mut r,
                    &mut iced::advanced::clipboard::Null,
                    &mut msgs,
                );
            }
            drop(ui);
            let reorders: Vec<_> = msgs
                .into_iter()
                .filter(|m| !matches!(m, Message::Scrolled(_) | Message::AutoScroll(_)))
                .collect();
            let [m @ Message::Reorder { .. }] = reorders.as_slice() else {
                panic!("{reorders:?}");
            };
            let out = fx.sidebar.update(m.clone());
            assert!(
                matches!(
                    &out,
                    Out::Layout(LayoutEdit::MoveTo { id, anchor, after: false })
                        if *id == SectionId::from(SectionKind::DirectMessages)
                            && *anchor == SectionId::from(SectionKind::Channels)
                ),
                "{out:?}"
            );
        }

        /// Move up/down anchor on the visible neighbours (empty sections in
        /// between are skipped), built with the menu; the ends are disabled.
        #[test]
        fn header_menu_moves_next_to_visible_neighbours() {
            let mut fx = Fx::with_rooms(3);
            fx.refresh();
            let menus = header_menus(&fx);
            assert_eq!(
                menus.iter().map(|(i, _)| i.as_str()).collect::<Vec<_>>(),
                ["channels", "direct_messages"]
            );
            let moves = |id: &str| -> Vec<(LayoutEdit, bool)> {
                items(menu_of(&menus, id), |m| {
                    matches!(layout_edit(m), Some(LayoutEdit::MoveTo { .. }))
                })
                .into_iter()
                .map(|i| (layout_edit(&i.on_select).unwrap().clone(), i.disabled))
                .collect()
            };
            let channels = SectionId::from(SectionKind::Channels);
            let dms = SectionId::from(SectionKind::DirectMessages);
            let [(_, up_off), (down, down_off)] = moves("channels").try_into().unwrap();
            assert!(up_off && !down_off);
            assert_eq!(
                down,
                LayoutEdit::MoveTo {
                    id: channels.clone(),
                    anchor: dms.clone(),
                    after: true
                }
            );
            let [(up, up_off), (_, down_off)] = moves("direct_messages").try_into().unwrap();
            assert!(!up_off && down_off);
            assert_eq!(
                up,
                LayoutEdit::MoveTo {
                    id: dms.clone(),
                    anchor: channels.clone(),
                    after: false
                }
            );
            // The click carries the edit; the app applies it.
            let out = fx
                .sidebar
                .update(Message::Header(HeaderAct::Layout(down.clone())));
            assert!(matches!(&out, Out::Layout(e) if *e == down));
            rustket_core::sidebar_layout::apply(&mut fx.prefs, down);
            fx.refresh();
            let order: Vec<String> = fx
                .sidebar
                .listed_with(&fx.collapsed)
                .map(|(s, _)| s.id.as_config_str())
                .collect();
            assert_eq!(order, ["direct_messages", "channels"]);
            // Search results cannot be moved.
            fx.sidebar.filter = "r".into();
            fx.refresh();
            let menus = header_menus(&fx);
            assert!(
                items(menu_of(&menus, "channels"), |m| matches!(
                    layout_edit(m),
                    Some(LayoutEdit::MoveTo { .. })
                ))
                .iter()
                .all(|i| i.disabled)
            );
        }

        /// Sort, "Use default", unread-first and Threads mode items map to
        /// the right edits; the current choices are checked.
        #[test]
        fn header_menu_sort_and_mode_items() {
            use rustket_core::config::{SidebarSort, ThreadsMode};
            use rustket_core::sidebar_layout::apply;
            let mut fx = Fx::with_rooms(3);
            fx.prefs.sidebar_threads = ThreadsMode::Unread;
            fx.ws.set_thread_config(ThreadsMode::Unread, []);
            fx.refresh();
            let menus = header_menus(&fx);
            // Shown with no rows while the mode is on.
            assert_eq!(menus[0].0, "threads");
            let threads = SectionId::from(SectionKind::Threads);
            let channels = SectionId::from(SectionKind::Channels);
            let sorts = |menu: &[MenuEntry<Message>]| -> Vec<(Option<SidebarSort>, bool)> {
                items(menu, |m| {
                    matches!(layout_edit(m), Some(LayoutEdit::SetSort { .. }))
                })
                .into_iter()
                .map(|i| match layout_edit(&i.on_select) {
                    Some(LayoutEdit::SetSort { sort, .. }) => (*sort, i.checked == Some(true)),
                    _ => unreachable!(),
                })
                .collect()
            };
            assert_eq!(
                sorts(menu_of(&menus, "channels")),
                [
                    (Some(SidebarSort::Activity), true),
                    (Some(SidebarSort::ActivityAsc), false),
                    (Some(SidebarSort::Alphabetical), false),
                ]
            );
            let unread_first = |menu: &[MenuEntry<Message>]| {
                items(menu, |m| {
                    matches!(layout_edit(m), Some(LayoutEdit::SetUnreadFirst { .. }))
                })
                .into_iter()
                .map(|i| (layout_edit(&i.on_select).unwrap().clone(), i.checked))
                .collect::<Vec<_>>()
            };
            assert_eq!(
                unread_first(menu_of(&menus, "threads")),
                [(
                    LayoutEdit::SetUnreadFirst {
                        id: threads.clone(),
                        on: Some(false)
                    },
                    Some(true)
                )]
            );
            assert_eq!(
                unread_first(menu_of(&menus, "channels")),
                [(
                    LayoutEdit::SetUnreadFirst {
                        id: channels.clone(),
                        on: Some(true)
                    },
                    Some(false)
                )]
            );
            let modes: Vec<(ThreadsMode, bool)> = items(menu_of(&menus, "threads"), |m| {
                matches!(layout_edit(m), Some(LayoutEdit::SetThreadsMode(_)))
            })
            .into_iter()
            .map(|i| match layout_edit(&i.on_select) {
                Some(LayoutEdit::SetThreadsMode(m)) => (*m, i.checked == Some(true)),
                _ => unreachable!(),
            })
            .collect();
            assert_eq!(
                modes,
                [
                    (ThreadsMode::Unread, true),
                    (ThreadsMode::UnreadKept, false),
                    (ThreadsMode::Following, false),
                    (ThreadsMode::Off, false),
                ]
            );
            // "Show at most": the offered limits, the current one checked.
            let limits = |menu: &[MenuEntry<Message>]| -> Vec<(u32, bool)> {
                items(menu, |m| {
                    matches!(layout_edit(m), Some(LayoutEdit::SetThreadsLimit(_)))
                })
                .into_iter()
                .map(|i| match layout_edit(&i.on_select) {
                    Some(LayoutEdit::SetThreadsLimit(n)) => (*n, i.checked == Some(true)),
                    _ => unreachable!(),
                })
                .collect()
            };
            assert_eq!(
                limits(menu_of(&menus, "threads")),
                [
                    (5, false),
                    (10, false),
                    (20, true),
                    (50, false),
                    (100, false),
                    (0, false),
                ]
            );
            assert!(limits(menu_of(&menus, "channels")).is_empty());
            let mut custom = fx.prefs.clone();
            custom.sidebar_threads_limit = 30;
            let layout = rustket_core::sidebar_layout::Layout::from_prefs(&custom);
            let listed: Vec<(&Section, bool)> = fx.sidebar.listed_with(&fx.collapsed).collect();
            let ids: Vec<&SectionId> = listed.iter().map(|(s, _)| &s.id).collect();
            let menu = fx
                .sidebar
                .header_menu_at(listed[0].0, 0, &ids, false, &layout, &custom);
            assert_eq!(
                limits(&menu)
                    .into_iter()
                    .filter(|(_, c)| *c)
                    .collect::<Vec<_>>(),
                [(30, true)]
            );
            assert!(matches!(
                fx.sidebar.update(Message::Header(HeaderAct::Refresh)),
                Out::RefreshThreads
            ));
            assert!(matches!(
                fx.sidebar.update(Message::Header(HeaderAct::OpenSettings)),
                Out::OpenSettings {
                    ungroup: None,
                    flag: None
                }
            ));

            // An override: checked, and "Use default" offered.
            let alpha = LayoutEdit::SetSort {
                id: channels.clone(),
                sort: Some(SidebarSort::Alphabetical),
            };
            assert!(apply(&mut fx.prefs, alpha).changed);
            fx.refresh();
            let menus = header_menus(&fx);
            assert_eq!(
                sorts(menu_of(&menus, "channels")),
                [
                    (Some(SidebarSort::Activity), false),
                    (Some(SidebarSort::ActivityAsc), false),
                    (Some(SidebarSort::Alphabetical), true),
                    (None, false),
                ]
            );
            assert_eq!(
                sorts(menu_of(&menus, "direct_messages")).len(),
                3,
                "no override"
            );
        }

        /// An override equal to the default does not change the resolved
        /// layout, yet the menus must offer "Default (…)": the body is
        /// rebuilt (its rows are not).
        #[test]
        fn a_sort_override_equal_to_the_default_rebuilds_the_body() {
            use rustket_core::config::SidebarSort;
            use rustket_core::sidebar_layout::apply;
            rust_i18n::set_locale("en");
            let mut fx = Fx::with_rooms(3);
            fx.refresh();
            let mut tree = Tree::new(fx.view().as_widget());
            assert_eq!(fx.diff(&mut tree), (0, 0));
            let channels = SectionId::from(SectionKind::Channels);
            let same = LayoutEdit::SetSort {
                id: channels,
                sort: Some(SidebarSort::Activity),
            };
            assert!(apply(&mut fx.prefs, same).changed);
            assert_eq!(fx.diff(&mut tree), (1, 0));
            let menus = header_menus(&fx);
            let defaults = items(menu_of(&menus, "channels"), |m| {
                matches!(layout_edit(m), Some(LayoutEdit::SetSort { sort: None, .. }))
            });
            assert_eq!(defaults.len(), 1);
            assert_eq!(defaults[0].label, "Default (Newest first)");
        }

        /// While filtering every section shows expanded: no Collapse/Expand
        /// item (its label would not match what Toggle does to the stored
        /// state), and no moves.
        #[test]
        fn header_menu_while_filtering_has_no_collapse_item() {
            let mut fx = Fx::with_rooms(3);
            fx.collapsed.insert(
                SectionId::from(SectionKind::Channels)
                    .collapse_key()
                    .into_owned(),
            );
            fx.refresh();
            let toggles = |fx: &Fx| {
                let menus = header_menus(fx);
                items(menu_of(&menus, "channels"), |m| {
                    matches!(m, Message::Toggle(_))
                })
                .len()
            };
            assert_eq!(toggles(&fx), 1);
            fx.sidebar.filter = "r0".into();
            fx.refresh();
            assert_eq!(toggles(&fx), 0);
        }

        /// Unread offers Hide; a group offers Ungroup…, which opens the
        /// settings with the deletion to confirm.
        #[test]
        fn header_menu_hide_unread_and_ungroup() {
            use rustket_core::sidebar_layout::{GroupType, apply};
            let mut fx = Fx::with_rooms(3);
            fx.prefs.unread_on_top = true;
            apply(
                &mut fx.prefs,
                LayoutEdit::AddGroup {
                    name: "People".into(),
                    types: vec![GroupType::DirectMessages],
                },
            );
            let gid = fx.prefs.sidebar_groups[0].id.clone();
            let mut s = sub("r0001", "r0001", RoomType::Channel);
            s.unread = 2;
            s.alert = true;
            fx.ws.apply_subscription(Change::Updated, s);
            fx.refresh();
            let menus = header_menus(&fx);
            let unread = menu_of(&menus, "unread");
            assert!(
                items(unread, |m| matches!(
                    layout_edit(m),
                    Some(LayoutEdit::SetUnreadOnTop(false))
                ))
                .len()
                    == 1
            );
            assert!(
                items(unread, |m| matches!(
                    layout_edit(m),
                    Some(LayoutEdit::SetUnreadFirst { .. })
                ))
                .is_empty(),
                "always unread"
            );
            let group = menu_of(&menus, &format!("group:{gid}"));
            let ungroup = items(group, |m| {
                matches!(m, Message::Header(HeaderAct::Ungroup(_)))
            });
            assert!(ungroup.len() == 1 && ungroup[0].danger);
            assert!(matches!(
                fx.sidebar.update(ungroup[0].on_select.clone()),
                Out::OpenSettings {
                    ungroup: Some(id),
                    flag: None
                } if id == gid
            ));
            for (id, menu) in &menus {
                let n = items(menu, |m| {
                    matches!(m, Message::Header(HeaderAct::Ungroup(_)))
                })
                .len();
                assert_eq!(n, usize::from(id.starts_with("group:")), "{id}");
                let collapse = items(menu, |m| matches!(m, Message::Toggle(_))).len();
                assert_eq!(collapse, 1, "{id}");
            }
        }
    }

    #[test]
    fn typing_labels() {
        rust_i18n::set_locale("en");
        let l = |names: &[&str], count: usize| TypingLabel {
            usernames: names.iter().map(|s| (*s).to_owned()).collect(),
            count,
            activity: Activity::Typing,
        };
        assert_eq!(typing_text(&l(&["bob"], 1)), "bob is typing…");
        assert_eq!(
            typing_text(&l(&["bob", "eve"], 2)),
            "bob and eve are typing…"
        );
        assert_eq!(
            typing_text(&l(&["a", "b", "c"], 5)),
            "a, b, c and 2 others are typing…"
        );
    }

    #[test]
    fn previews() {
        rust_i18n::set_locale("en");
        let p = Preview {
            kind: PreviewKind::Text,
            i18n_key: "preview.text".into(),
            own: true,
            author: None,
            text: Some("hello".into()),
            system_type: None,
            ts: None,
        };
        assert_eq!(preview_text(&p), "You: hello");
        let p = Preview {
            kind: PreviewKind::Image,
            i18n_key: "preview.image".into(),
            own: false,
            author: Some("bob".into()),
            text: Some("cat.png".into()),
            system_type: None,
            ts: None,
        };
        assert_eq!(preview_text(&p), "bob: Image: cat.png");
        let p = Preview {
            kind: PreviewKind::System,
            i18n_key: "preview.system.unknown-type".into(),
            own: false,
            author: None,
            text: None,
            system_type: Some("unknown-type".into()),
            ts: None,
        };
        assert_eq!(preview_text(&p), "System message");
    }
}
