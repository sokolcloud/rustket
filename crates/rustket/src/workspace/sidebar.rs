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
use rustket_core::room_list::{
    self, Avatar, BadgeVariant, Preview, PreviewKind, RoomKind, RoomListItem, RoomListOptions,
    Section, SectionKind,
};
use rustket_core::typing::TypingLabel;
use rustket_core::{ServerId, Workspace};
use rustket_ui::icon::{self, Icon};
use rustket_ui::style;
use rustket_ui::theme::{SPACE_1, SPACE_2, SPACE_3, TEXT_BODY};
use rustket_ui::widget::{
    Presence, RoomListItemProps, empty_state, icon_button, keyed_list, room_list_item,
    section_header,
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
    Toggle(SectionKind),
    Open(String),
    /// The status menu in the header (`workspace/status_menu.rs`).
    Status(super::status_menu::Message),
    Directory,
    Create,
    /// Omnichannel groups and buttons (`crate::omnichannel::sidebar`).
    Omni(crate::omnichannel::sidebar::Msg),
    /// The room list scrolled or was resized.
    Scrolled(scrollable::Viewport),
}

#[derive(Debug, Clone)]
pub enum Out {
    None,
    OpenRoom(String),
    /// Collapsed state of a section changed (`SectionKind::i18n_key`).
    ToggleSection(String),
    Status(super::status_menu::Message),
    Directory,
    Create,
    Omni(crate::omnichannel::sidebar::Msg),
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
    (p.unread, p.user_mentions, p.group_mentions).hash(&mut h);
    (p.thread_unread, p.is_muted, p.is_selected, p.typing).hash(&mut h);
    p.presence.hash(&mut h);
    p.icon.as_ref().map(std::mem::discriminant).hash(&mut h);
    p.typing_label.hash(&mut h);
    std::mem::discriminant(&p.density).hash(&mut h);
    h.finish()
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
            Message::Toggle(kind) => Out::ToggleSection(kind.i18n_key().to_owned()),
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
        }
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
        let opts = RoomListOptions::from_prefs(prefs, ws);
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
                border: iced::border::rounded(rustket_ui::theme::RADIUS_MD)
                    .width(1)
                    .color(t.border),
                ..container::Style::default()
            }
        })
        .padding([0.0, SPACE_1]);
        let search = rustket_ui::widget::focus_within(search).radius(rustket_ui::theme::RADIUS_MD);
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
                density: ctx.prefs.density as u8,
                clock_24h: ctx.prefs.clock_24h,
                images: ctx.images.server_revision(ctx.server_id),
                window: self.window(ctx),
                workspace: revs,
                locale: rust_i18n::locale().to_string(),
                day: chrono::Local::now().date_naive(),
                over_mac: crate::omnichannel::shared::get(ctx.server_id)
                    .is_some_and(|o| o.over_mac),
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
            let n = if is_collapsed { 0 } else { s.items.len() };
            if found.is_none() {
                y += HEADER_H;
                match s.items[..n].iter().position(|i| i.rid == rid) {
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
            .filter(|s| s.kind != SectionKind::Omnichannel)
            .map(move |s| (s, !filtering && collapsed.contains(s.kind.i18n_key())))
    }

    /// Slot range `[start, end)` of the list (a slot is a section header or a
    /// room row) that covers the viewport plus [`OVERSCAN_PX`]; only these
    /// are built, laid out, drawn and have their avatars requested.
    fn window(&self, ctx: &SidebarCtx<'_>) -> (usize, usize) {
        let row_h = row_height(ctx.prefs.density);
        let (mut slots, mut total) = (0usize, 0.0f32);
        for (s, collapsed) in self.listed(ctx) {
            let n = if collapsed { 0 } else { s.items.len() };
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
            let n = if collapsed { 0 } else { s.items.len() };
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
        for (section, collapsed) in self.listed(ctx) {
            if slot < start {
                before += HEADER_H;
            } else if slot >= end {
                after += HEADER_H;
            } else {
                let header = section_header(
                    t!(section.kind.i18n_key()).into_owned(),
                    collapsed,
                    Some(section.items.len() as u32),
                    Message::Toggle(section.kind),
                );
                built.push((
                    hash_of(("section", section.kind.i18n_key())),
                    container(header).center_y(HEADER_H).into(),
                ));
            }
            slot += 1;
            if collapsed {
                continue;
            }
            let n = section.items.len();
            let lo = start.clamp(slot, slot + n) - slot;
            let hi = end.clamp(slot, slot + n) - slot;
            before += lo as f32 * row_h;
            after += (n - hi) as f32 * row_h;
            for item in &section.items[lo..hi] {
                built.push((hash_of(&item.rid), self.row(item, ctx)));
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
        list.push(hash_of("below"), iced::widget::space().height(after))
            .into()
    }

    fn row(&self, item: &RoomListItem, ctx: &SidebarCtx<'_>) -> Element<'static, Message> {
        let p = self.row_props(item, ctx);
        let key = row_key(&item.rid, &p);
        lazy(key, move |_| {
            count_build(0, 1);
            room_list_item(p.clone())
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
        let b = &item.badge;
        p.unread = count_u32(b.count);
        p.user_mentions = if b.variant == BadgeVariant::Mention {
            count_u32(b.mentions)
        } else {
            0
        };
        p.group_mentions = if b.variant == BadgeVariant::GroupMention {
            count_u32(b.group_mentions)
        } else {
            0
        };
        p.thread_unread = b.threads > 0;
        if b.dot && p.unread == 0 {
            // `alert` without a counter: show a dot via thread/unread styling.
            p.thread_unread = true;
        }
        p.is_muted = item.muted;
        p.is_selected = ctx.active_room == Some(item.rid.as_str());
        p.presence = item.dm_status.map(presence);
        p.icon = Some(room_icon(item.kind));
        crate::omnichannel::sidebar::decorate(&mut p, item, ctx);
        if let Some(label) = ctx.workspace.typing_label(&item.rid, None, ctx.now) {
            p.typing = true;
            p.typing_label = typing_text(&label);
        }
        p
    }
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
                .find(|s| s.kind == SectionKind::Channels)
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
                now: Instant::now(),
            };
            assert_eq!(fx.sidebar.window(&ctx), (0, 2));
        }

        /// Builds and lays out the sidebar at 300x800; returns the rows built
        /// and the time it took.
        fn build_and_layout(fx: &Fx) -> (usize, std::time::Duration) {
            use iced_runtime::user_interface::{Cache, UserInterface};
            let mut renderer = iced::Renderer::Secondary(iced_tiny_skia::Renderer::new(
                rustket_ui::theme::DEFAULT_FONT,
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
            let channels: BTreeSet<String> = [SectionKind::Channels.i18n_key().to_owned()].into();
            assert_eq!(fx.sidebar.reveal_offset("r0250", d, &channels), None);
        }

        fn first_keyed(tree: &Tree) -> Option<&Tree> {
            if rustket_ui::widget::keyed_list_keys::<u64>(tree).is_some() {
                return Some(tree);
            }
            tree.children.iter().find_map(first_keyed)
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
