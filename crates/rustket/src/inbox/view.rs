//! Rendering of the inbox views. Message bodies go through the shared
//! renderer (`room::timeline::Viewer`), rows through the timeline's row
//! layout; badges use the sidebar's colours, so only mentions are loud.

use std::time::Instant;

use iced::widget::{Column, column, container, row, scrollable, space, text};
use iced::{Alignment, Element, Length};
use rocketchat::ServerUrl;
use rocketchat::models::Message as RcMessage;
use rust_i18n::t;
use rustket_core::config::UiPrefs;
use rustket_core::inbox::feeds::FeedState;
use rustket_core::inbox::{ActivityFilter, ActivityItem, PendingMentions, Tier, UnreadRoom};
use rustket_core::indicators::{IndicatorScheme, rc_badge};
use rustket_core::room_list::{self, Avatar, ThreadListItem};
use rustket_core::{ServerId, Workspace};
use rustket_ui::icon::{self, Icon};
use rustket_ui::style;
use rustket_ui::theme::{
    SPACE_1, SPACE_2, SPACE_3, SPACE_4, TEXT_BODY, TEXT_CAPTION, TEXT_SMALL, font_semibold,
};
use rustket_ui::widget::{
    AvatarShape, BadgeCap, BadgeKind, BadgePlace, ButtonSize, MessageRowProps, TooltipPosition,
    avatar, badge, badge_in, divider, dot, dot_in, ellipsis, empty_state, header_bar, icon_button,
    icon_button_with, message_row, message_toolbar, modal_keyed, primary_button, secondary_button,
    spinner, tooltip,
};

use super::{InboxKind, InboxScreen, InboxState, Msg, ROOMS_PAGE};
use crate::images::Images;
use crate::room::ViewCtx;
use crate::room::panel::common;
use crate::room::render::plural;

/// What the views read besides the screen.
pub struct Ctx<'a> {
    pub server_id: &'a ServerId,
    pub server: &'a ServerUrl,
    pub workspace: &'a Workspace,
    pub images: &'a Images,
    pub prefs: &'a UiPrefs,
    pub state: &'a InboxState,
    /// The session is connected (read marks need it).
    pub online: bool,
    pub now: Instant,
}

impl<'a> Ctx<'a> {
    /// The room context of a body (`rid` lives as long as the data).
    fn view_ctx(&self, rid: &'a str) -> ViewCtx<'a> {
        ViewCtx {
            server_id: self.server_id,
            server: self.server,
            rid,
            workspace: self.workspace,
            images: self.images,
            prefs: self.prefs,
            connected: self.online,
            panel: None,
            now: self.now,
        }
    }

    fn room_ctx<'r>(&'r self, rid: &'r str) -> ViewCtx<'r> {
        ViewCtx {
            server_id: self.server_id,
            server: self.server,
            rid,
            workspace: self.workspace,
            images: self.images,
            prefs: self.prefs,
            connected: self.online,
            panel: None,
            now: self.now,
        }
    }

    fn rc(&self) -> bool {
        self.prefs.indicator_scheme == IndicatorScheme::RocketChat
    }

    fn use_real(&self) -> bool {
        self.workspace.use_real_names(self.prefs.use_real_names)
    }

    fn room_name(&self, rid: &str) -> String {
        self.workspace
            .room(rid)
            .map(|e| room_list::display_name(self.workspace, e, self.use_real()))
            .unwrap_or_default()
    }

    fn room_avatar<M: 'static>(&self, rid: &str, size: f32) -> Element<'static, M> {
        let e = self.workspace.room(rid);
        let av = match e.map(room_list::room_kind) {
            Some(room_list::RoomKind::Direct) => Avatar::User {
                username: e.map(|e| e.name().to_owned()).unwrap_or_default(),
            },
            _ => Avatar::Room {
                rid: rid.to_owned(),
                etag: e
                    .and_then(|e| e.room.as_ref())
                    .and_then(|r| r.avatar_etag.clone()),
            },
        };
        let src = self
            .images
            .avatar(self.server_id, self.server, &av, &self.room_name(rid));
        avatar(src, size, AvatarShape::Rounded).into()
    }

    fn user_avatar<M: 'static>(
        &self,
        username: &str,
        name: &str,
        size: f32,
    ) -> Element<'static, M> {
        let src = self.images.avatar(
            self.server_id,
            self.server,
            &Avatar::User {
                username: username.to_owned(),
            },
            name,
        );
        avatar(src, size, AvatarShape::Rounded).into()
    }
}

/// The room's badge as its sidebar row shows it.
fn room_badge(r: &UnreadRoom) -> Option<Element<'static, Msg>> {
    let kind = crate::workspace::sidebar::badge_kind(&r.badge)?;
    let count = crate::format::count_u32(r.badge.count);
    Some(if count > 0 {
        badge(count, kind)
    } else {
        container(dot(kind, 8.0)).padding([0.0, SPACE_1]).into()
    })
}

/// The room's badge under the Rocket.Chat scheme: its full counter in the
/// content palette with the itemised tooltip; never a dot.
fn room_badge_rc(ws: &Workspace, rid: &str) -> Option<Element<'static, Msg>> {
    let b = ws.room(rid).map(rc_badge)?;
    let kind = crate::workspace::sidebar::widget_kind(b.kind()?);
    Some(tip(
        badge_in(b.count, kind, BadgePlace::Content, BadgeCap::None),
        crate::workspace::sidebar::rc_unread_title(&b.title),
    ))
}

/// Whether a view's message row takes the mention tint: Rustket's row
/// highlight; Rocket.Chat tints no row for a mention.
fn mention_tint(scheme: IndicatorScheme, ws: &Workspace, m: &RcMessage) -> bool {
    scheme == IndicatorScheme::Rustket && crate::room::timeline::row::highlights_in(ws, m)
}

/// A mention or thread dot of the views (content palette under
/// Rocket.Chat).
fn kind_dot(kind: BadgeKind) -> Element<'static, Msg> {
    dot_in(kind, 8.0, BadgePlace::Content)
}

fn tip(content: Element<'static, Msg>, label: String) -> Element<'static, Msg> {
    tooltip(content, label, TooltipPosition::Bottom)
}

fn caption(s: String) -> Element<'static, Msg> {
    text(s)
        .size(TEXT_CAPTION)
        .style(style::text_secondary)
        .into()
}

fn link(label: String, on: Msg) -> Element<'static, Msg> {
    common::link(label, on)
}

impl InboxScreen {
    pub fn view<'a>(&'a self, c: &Ctx<'a>) -> Element<'a, Msg> {
        let body = match self.kind {
            InboxKind::Unreads => self.unreads(c),
            InboxKind::CatchUp => self.catch_up_view(c),
            InboxKind::Activity => self.activity_view(c),
        };
        // The image lightbox of a body covers the view, as in the room.
        let lightbox = self.image_rid.as_ref().and_then(|rid| {
            let e = self.viewer.overlay(&c.room_ctx(rid))?;
            let (r1, r2) = (rid.clone(), rid.clone());
            Some((
                e.map(move |m| Msg::View { rid: r1.clone(), m }),
                Msg::View {
                    rid: r2,
                    m: crate::room::timeline::ViewerMsg::CloseImage,
                },
            ))
        });
        let (overlay, dismiss) = match lightbox {
            Some((e, d)) => (Some((1u8, e)), Some(d)),
            None => (None, None),
        };
        let user = self
            .user
            .as_ref()
            .map(|u| u.view(&c.view_ctx(&u.rid)).map(Msg::User));
        let body = crate::room::panel::user_info::beside(body, user);
        container(modal_keyed(body, overlay, dismiss))
            .style(style::container_room)
            .width(Length::Fill)
            .height(Length::Fill)
            .into()
    }

    fn close_button() -> Element<'static, Msg> {
        icon_button(Icon::X, Some(Msg::Close), t!("inbox.close"))
    }

    fn include_muted(c: &Ctx<'_>) -> bool {
        c.prefs.inbox_include_muted
    }

    // --- All unreads -----------------------------------------------------------------

    fn unreads<'a>(&'a self, c: &Ctx<'_>) -> Element<'a, Msg> {
        let rooms = self.rooms(c.workspace, c.state, Self::include_muted(c));
        let total = rooms.len();
        let mut summary = t!(
            format!("inbox.summary_{}", plural(total as u64)),
            count = total
        )
        .into_owned();
        // Shift+Esc and the window title count rooms with unread threads
        // too; this view leaves those to Activity.
        let threads = c.workspace.rooms().values().any(|e| {
            e.sub
                .as_ref()
                .is_some_and(rocketchat::models::Subscription::has_unread_threads)
        });
        if threads {
            summary = format!("{summary} · {}", t!("inbox.threads_in_activity"));
        }
        let any = total > 0;
        let mark_all: Element<'static, Msg> = secondary_button(
            t!("inbox.mark_all_read"),
            (any && c.online).then_some(Msg::MarkAll),
        )
        .icon(Icon::CheckCheck)
        .size(ButtonSize::Small)
        .into();
        let mark_tip = if c.online {
            t!("inbox.mark_all_read_tip").into_owned()
        } else {
            t!("inbox.offline").into_owned()
        };
        let muted_on = Self::include_muted(c);
        let actions = vec![
            secondary_button(t!("inbox.catch_up"), any.then_some(Msg::StartCatchUp))
                .icon(Icon::ListChecks)
                .size(ButtonSize::Small)
                .into(),
            tip(mark_all, mark_tip),
            icon_button_with(
                Icon::BellOff,
                Some(Msg::ToggleMuted),
                t!("inbox.include_muted").into_owned(),
                muted_on,
                32.0,
            ),
            Self::close_button(),
        ];
        let header = header_bar(
            Some(icon::sized(Icon::Inbox, 18.0).into()),
            t!("inbox.all_unreads").into_owned(),
            Some(summary),
            actions,
        );
        if !any {
            return column![
                header,
                empty_state(
                    Icon::CheckCheck,
                    t!("inbox.caught_up_title"),
                    t!("inbox.caught_up_subtitle")
                )
            ]
            .into();
        }
        let mut list: Column<'a, Msg> = column![].spacing(SPACE_2).padding([SPACE_3, 0.0]);
        for r in rooms.iter().take(self.rooms_wanted()) {
            list = list.push(self.group(c, r, false));
            list = list.push(container(divider()).padding([0.0, SPACE_4]));
        }
        let left = total.saturating_sub(self.rooms_wanted());
        if left > 0 {
            let n = left.min(ROOMS_PAGE);
            list = list.push(
                container(secondary_button(
                    t!(
                        format!("inbox.load_more_rooms_{}", plural(n as u64)),
                        count = n
                    ),
                    Some(Msg::LoadMoreRooms),
                ))
                .center_x(Length::Fill)
                .padding(SPACE_2),
            );
        }
        column![
            header,
            scrollable(list)
                .height(Length::Fill)
                .style(style::scrollable)
        ]
        .into()
    }

    /// A room with its unread messages (`catch_up`: the room fills the view
    /// and its own actions are in the bottom bar).
    fn group<'a>(&'a self, c: &Ctx<'_>, r: &UnreadRoom, catch_up: bool) -> Element<'a, Msg> {
        let rid = r.rid.clone();
        let mut head = row![
            c.room_avatar(&rid, 24.0),
            ellipsis(c.room_name(&rid))
                .size(TEXT_BODY)
                .font(font_semibold()),
        ]
        .spacing(SPACE_2)
        .align_y(Alignment::Center);
        // The badge sits with the actions: the name (`Fill`) takes the
        // row up to them (a badge after a short name floated mid-row).
        let b = if c.rc() {
            room_badge_rc(c.workspace, &r.rid)
        } else {
            room_badge(r)
        };
        if let Some(b) = b {
            head = head.push(b);
        }
        if !catch_up {
            head = head
                .push(icon_button(
                    Icon::Check,
                    c.online.then(|| Msg::MarkRead(rid.clone())),
                    t!("inbox.mark_read"),
                ))
                .push(icon_button(
                    Icon::SkipForward,
                    Some(Msg::Skip(rid.clone())),
                    t!("inbox.skip"),
                ));
        }
        head = head.push(icon_button(
            Icon::ArrowRight,
            Some(Msg::OpenRoom(rid.clone())),
            t!("inbox.open_room"),
        ));
        let mut col: Column<'a, Msg> =
            column![container(head).padding([0.0, SPACE_4])].spacing(SPACE_1);
        for e in self.feed_rows(c, &rid) {
            col = col.push(e);
        }
        col.into()
    }

    /// The loaded unread messages of a room, oldest first, with the
    /// "earlier" row above them.
    fn feed_rows(&self, c: &Ctx<'_>, rid: &str) -> Vec<Element<'static, Msg>> {
        let mut out = Vec::new();
        let feed = c.workspace.feeds().feed(rid);
        let pad = |e: Element<'static, Msg>| -> Element<'static, Msg> {
            container(e).padding([SPACE_1, SPACE_4 + 32.0]).into()
        };
        let Some(f) = feed.filter(|f| !f.messages().is_empty() || f.is_loaded()) else {
            match feed.map(|f| f.state()) {
                Some(FeedState::Failed(e)) => out.push(pad(row![
                    caption(t!("inbox.load_failed", error = e.as_str()).into_owned()),
                    link(t!("inbox.retry").into_owned(), Msg::Retry(rid.to_owned())),
                ]
                .spacing(SPACE_2)
                .align_y(Alignment::Center)
                .into())),
                _ => out.push(pad(spinner(16.0))),
            }
            return out;
        };
        // The newest messages only: a busy room's feed grows to hundreds.
        let hidden = f.messages().len().saturating_sub(self.shown(rid));
        if hidden > 0 {
            let n = hidden as u64 + f.earlier();
            let label = t!(format!("inbox.earlier_unread_{}", plural(n)), count = n).into_owned();
            out.push(pad(row![
                caption(label),
                link(
                    t!("inbox.show_earlier").into_owned(),
                    Msg::ShowEarlier(rid.to_owned()),
                ),
            ]
            .spacing(SPACE_2)
            .align_y(Alignment::Center)
            .into()));
        } else if f.is_full() {
            out.push(pad(link(
                t!("inbox.more_in_room").into_owned(),
                Msg::OpenRoom(rid.to_owned()),
            )));
        } else if f.earlier() > 0 {
            let n = f.earlier();
            let label = t!(format!("inbox.earlier_unread_{}", plural(n)), count = n).into_owned();
            let action: Element<'static, Msg> = if f.is_loading_earlier() {
                spinner(14.0)
            } else {
                link(
                    t!("inbox.load_earlier").into_owned(),
                    Msg::LoadEarlier(rid.to_owned()),
                )
            };
            out.push(pad(row![caption(label), action]
                .spacing(SPACE_2)
                .align_y(Alignment::Center)
                .into()));
        }
        if f.messages().is_empty() {
            out.push(pad(link(
                t!("inbox.nothing_loaded").into_owned(),
                Msg::OpenRoom(rid.to_owned()),
            )));
        }
        let vc = c.room_ctx(rid);
        for m in &f.messages()[hidden..] {
            // Quoting needs the message shown decrypted, as in the timeline.
            let quotable = !crate::e2e::shown::undecrypted(m);
            let toolbar = vec![
                icon_button_with(
                    Icon::Reply,
                    quotable.then(|| Msg::Reply {
                        rid: rid.to_owned(),
                        mid: m.id.clone(),
                    }),
                    t!("inbox.reply").into_owned(),
                    false,
                    28.0,
                ),
                icon_button_with(
                    Icon::MessageCircle,
                    Some(Msg::ReplyInThread {
                        rid: rid.to_owned(),
                        tmid: m.tmid.clone().unwrap_or_else(|| m.id.clone()),
                    }),
                    t!("inbox.reply_in_thread").into_owned(),
                    false,
                    28.0,
                ),
                icon_button_with(
                    Icon::Eye,
                    Some(Msg::Reveal {
                        rid: rid.to_owned(),
                        mid: m.id.clone(),
                    }),
                    t!("inbox.show_in_room").into_owned(),
                    false,
                    28.0,
                ),
            ];
            out.push(self.message(c, &vc, m, toolbar));
        }
        out
    }

    /// One message: the timeline's row layout with the shared body.
    fn message(
        &self,
        c: &Ctx<'_>,
        vc: &ViewCtx<'_>,
        m: &RcMessage,
        toolbar: Vec<Element<'static, Msg>>,
    ) -> Element<'static, Msg> {
        let ws = c.workspace;
        let use_real = c.use_real();
        let username = m.u.username.clone().unwrap_or_default();
        let time = common::time_label(m.ts, vc);
        if m.is_system() {
            return container(
                row![
                    icon::sized(Icon::Info, 13.0).style(style::text_hint),
                    text(crate::room::panel::messages::system_text(ws, m, use_real))
                        .size(TEXT_SMALL)
                        .style(style::text_secondary)
                        .width(Length::Fill),
                    text(time).size(TEXT_CAPTION).style(style::text_hint),
                ]
                .spacing(SPACE_2)
                .align_y(Alignment::Center),
            )
            .padding([SPACE_1, SPACE_4])
            .into();
        }
        let name = m
            .alias
            .clone()
            .filter(|a| !a.is_empty())
            .unwrap_or_else(|| common::user_display(ws, use_real, &username, m.u.name.as_deref()));
        let rid = m.rid.clone();
        let body = self.viewer.body(m, vc).map(move |v| Msg::View {
            rid: rid.clone(),
            m: v,
        });
        let mut p = MessageRowProps::new(c.user_avatar(&username, &name, 36.0), name, time, body);
        p.is_mention_highlight = mention_tint(c.prefs.indicator_scheme, ws, m);
        p.edited = m.edited_at.is_some();
        p.edited_label = t!("room_timeline.edited").into_owned();
        p.actions_toolbar = Some(message_toolbar(toolbar));
        message_row(p)
    }

    // --- Catch up --------------------------------------------------------------------

    fn catch_up_view<'a>(&'a self, c: &Ctx<'_>) -> Element<'a, Msg> {
        let Some(cu) = self.catch_up() else {
            return column![].into();
        };
        let (pos, total) = cu.progress();
        let Some(rid) = cu.current().map(str::to_owned) else {
            let header = header_bar(
                Some(icon::sized(Icon::ListChecks, 18.0).into()),
                t!("inbox.catch_up").into_owned(),
                None,
                vec![Self::close_button()],
            );
            let mut done = column![empty_state(
                Icon::CheckCheck,
                t!("inbox.caught_up_title"),
                String::new()
            )]
            .spacing(SPACE_3)
            .align_x(Alignment::Center);
            let skipped = cu.skipped_count();
            if skipped > 0 {
                done = done.push(primary_button(
                    t!(
                        format!("inbox.review_skipped_{}", plural(skipped as u64)),
                        count = skipped
                    ),
                    Some(Msg::ReviewSkipped),
                ));
            }
            done = done.push(secondary_button(
                t!("inbox.all_unreads"),
                Some(Msg::Open(InboxKind::Unreads)),
            ));
            return column![header, container(done).center(Length::Fill)].into();
        };
        let room = self
            .rooms(c.workspace, c.state, Self::include_muted(c))
            .into_iter()
            .find(|r| r.rid == rid)
            .unwrap_or_else(|| UnreadRoom {
                rid: rid.clone(),
                tier: Tier::Unread,
                ls: None,
                lm: None,
                badge: c
                    .workspace
                    .room(&rid)
                    .map(room_list::badge)
                    .unwrap_or_default(),
            });
        let header = header_bar(
            Some(icon::sized(Icon::ListChecks, 18.0).into()),
            t!("inbox.catch_up_progress", pos = pos, total = total).into_owned(),
            Some(c.room_name(&rid)),
            vec![
                secondary_button(t!("inbox.all_unreads"), Some(Msg::Open(InboxKind::Unreads)))
                    .size(ButtonSize::Small)
                    .into(),
                Self::close_button(),
            ],
        );
        let body = scrollable(container(self.group(c, &room, true)).padding([SPACE_3, 0.0]))
            .height(Length::Fill)
            .style(style::scrollable);
        let bar = container(
            column![
                row![
                    secondary_button(t!("inbox.back"), (pos > 1).then_some(Msg::Back))
                        .icon(Icon::ChevronLeft),
                    secondary_button(t!("inbox.skip"), Some(Msg::SkipCurrent))
                        .icon(Icon::SkipForward),
                    space().width(Length::Fill),
                    tip(
                        primary_button(
                            t!("inbox.mark_read_next"),
                            c.online.then_some(Msg::ReadNext)
                        )
                        .icon(Icon::Check)
                        .into(),
                        if c.online {
                            t!("inbox.mark_read_room_tip").into_owned()
                        } else {
                            t!("inbox.offline").into_owned()
                        }
                    ),
                ]
                .spacing(SPACE_2)
                .align_y(Alignment::Center),
                text(t!("inbox.catch_up_keys").into_owned())
                    .size(TEXT_CAPTION)
                    .style(style::text_hint),
            ]
            .spacing(SPACE_2),
        )
        .padding([SPACE_3, SPACE_4])
        .style(style::container_header);
        column![header, body, divider(), bar].into()
    }

    // --- Activity --------------------------------------------------------------------

    fn activity_view<'a>(&'a self, c: &Ctx<'_>) -> Element<'a, Msg> {
        let items = self.activity(c.workspace, c.use_real());
        let any = !items.is_empty();
        let mark_all: Element<'static, Msg> = secondary_button(
            t!("inbox.mark_all_read"),
            (any && c.online).then_some(Msg::MarkAll),
        )
        .icon(Icon::CheckCheck)
        .size(ButtonSize::Small)
        .into();
        let header = header_bar(
            Some(icon::sized(Icon::AtSign, 18.0).into()),
            t!("inbox.activity").into_owned(),
            None,
            vec![
                tip(
                    mark_all,
                    if c.online {
                        t!("inbox.mark_all_activity_tip").into_owned()
                    } else {
                        t!("inbox.offline").into_owned()
                    },
                ),
                Self::close_button(),
            ],
        );
        let pills = container(common::filter_pills(
            &ActivityFilter::ALL.map(|f| {
                (
                    f,
                    match f {
                        ActivityFilter::All => t!("inbox.filter_all"),
                        ActivityFilter::Mentions => t!("inbox.filter_mentions"),
                        ActivityFilter::Threads => t!("inbox.filter_threads"),
                    }
                    .into_owned(),
                )
            }),
            self.filter(),
            Msg::Filter,
        ))
        .padding([SPACE_3, SPACE_4]);
        if !any {
            return column![
                header,
                pills,
                empty_state(
                    Icon::AtSign,
                    t!("inbox.no_activity_title"),
                    t!("inbox.no_activity_subtitle")
                )
            ]
            .into();
        }
        let mut list: Column<'a, Msg> = column![]
            .spacing(0)
            .padding(iced::Padding::ZERO.bottom(SPACE_3));
        let mut in_threads = false;
        let mut first = true;
        for item in items.iter().take(self.activity_shown()) {
            let thread = !item.is_mention();
            if first || thread != in_threads {
                let title = if thread {
                    t!("inbox.section_threads")
                } else {
                    t!("inbox.section_mentions")
                };
                list = list.push(
                    container(
                        text(title.into_owned())
                            .size(TEXT_CAPTION)
                            .font(font_semibold())
                            .style(style::text_hint),
                    )
                    .padding([SPACE_2, SPACE_4]),
                );
                in_threads = thread;
                first = false;
            }
            list = list.push(self.activity_row(c, item));
        }
        let left = items.len().saturating_sub(self.activity_shown());
        if left > 0 {
            list = list.push(
                container(secondary_button(
                    t!("inbox.show_more", count = left.min(super::ACTIVITY_PAGE)),
                    Some(Msg::ShowMoreActivity),
                ))
                .center_x(Length::Fill)
                .padding(SPACE_2),
            );
        }
        column![
            header,
            pills,
            scrollable(list)
                .height(Length::Fill)
                .style(style::scrollable)
        ]
        .into()
    }

    fn activity_row(&self, c: &Ctx<'_>, item: &ActivityItem) -> Element<'static, Msg> {
        let ws = c.workspace;
        match item {
            ActivityItem::RoomMentions {
                rid,
                group,
                pending,
            } => self.room_line(c, rid, *group, pending),
            ActivityItem::MentionedThread(t) | ActivityItem::Thread(t) => self.thread_row(c, t),
            ActivityItem::Mention {
                rid, mid, group, ..
            } => match self.mention_message(ws, item) {
                Some(m) => self.mention_row(c, rid, mid, m, None, *group),
                None => self.room_line(c, rid, *group, &PendingMentions::Loading),
            },
            ActivityItem::ThreadMention { rid, tmid, mid, .. } => {
                match self.mention_message(ws, item) {
                    Some(m) => self.mention_row(c, rid, mid, m, Some(tmid), false),
                    None => self.room_line(c, rid, false, &PendingMentions::Loading),
                }
            }
        }
    }

    /// A mention: where it is, then the message; a click shows it in its
    /// room (its thread for a thread mention).
    fn mention_row(
        &self,
        c: &Ctx<'_>,
        rid: &str,
        mid: &str,
        m: &RcMessage,
        tmid: Option<&String>,
        group: bool,
    ) -> Element<'static, Msg> {
        let ws = c.workspace;
        let mut place = c.room_name(rid);
        if let Some(t) = tmid {
            let title = thread_title(ws, t);
            place = format!("{place} · {}", t!("inbox.in_thread", title = title));
        } else if group {
            place = format!("{place} · {}", t!("inbox.group_mention"));
        }
        let kind = if group {
            BadgeKind::GroupMention
        } else {
            BadgeKind::Mention
        };
        let place = container(
            row![kind_dot(kind), caption(place)]
                .spacing(SPACE_2)
                .align_y(Alignment::Center),
        )
        .padding([SPACE_1, SPACE_4]);
        let (mark, mark_tip, reply_tmid) = match tmid {
            Some(t) => (
                Msg::MarkThreadRead {
                    rid: rid.to_owned(),
                    tmid: t.clone(),
                },
                t!("inbox.mark_read").into_owned(),
                t.clone(),
            ),
            None => (
                Msg::MarkRead(rid.to_owned()),
                t!("inbox.mark_read_room_tip").into_owned(),
                m.tmid.clone().unwrap_or_else(|| m.id.clone()),
            ),
        };
        let toolbar = vec![
            icon_button_with(Icon::Check, c.online.then_some(mark), mark_tip, false, 28.0),
            icon_button_with(
                Icon::MessageCircle,
                Some(Msg::ReplyInThread {
                    rid: rid.to_owned(),
                    tmid: reply_tmid,
                }),
                t!("inbox.reply_in_thread").into_owned(),
                false,
                28.0,
            ),
        ];
        let vc = c.room_ctx(rid);
        let row_ = self.message(c, &vc, m, toolbar);
        rustket_ui::widget::selectable::click_area(
            column![place, row_],
            Msg::Reveal {
                rid: rid.to_owned(),
                mid: mid.to_owned(),
            },
        )
    }

    fn mention_message<'w>(&self, ws: &'w Workspace, item: &ActivityItem) -> Option<&'w RcMessage> {
        let feeds = ws.feeds();
        match item {
            ActivityItem::Mention { rid, mid, .. } => {
                feeds.feed(rid)?.messages().iter().find(|m| &m.id == mid)
            }
            ActivityItem::ThreadMention { rid, tmid, .. } => feeds.thread_mention(rid, tmid),
            _ => None,
        }
    }

    /// A room with mentions none of which is loaded.
    fn room_line(
        &self,
        c: &Ctx<'_>,
        rid: &str,
        group: bool,
        pending: &PendingMentions,
    ) -> Element<'static, Msg> {
        let kind = if group {
            BadgeKind::GroupMention
        } else {
            BadgeKind::Mention
        };
        let note: Element<'static, Msg> = match pending {
            PendingMentions::Loading => row![
                spinner(12.0),
                caption(t!("sidebar.thread_loading").into_owned())
            ]
            .spacing(SPACE_1)
            .align_y(Alignment::Center)
            .into(),
            PendingMentions::Failed(e) => row![
                caption(t!("inbox.load_failed", error = e.as_str()).into_owned()),
                link(t!("inbox.retry").into_owned(), Msg::Retry(rid.to_owned())),
            ]
            .spacing(SPACE_2)
            .align_y(Alignment::Center)
            .into(),
            PendingMentions::Older => caption(t!("inbox.older_mentions").into_owned()),
        };
        let line = row![
            kind_dot(kind),
            c.room_avatar(rid, 20.0),
            ellipsis(c.room_name(rid))
                .size(TEXT_BODY)
                .font(font_semibold()),
            note,
        ]
        .spacing(SPACE_2)
        .align_y(Alignment::Center);
        common::list_row(line, Some(Msg::OpenRoom(rid.to_owned())))
    }

    fn thread_row(&self, c: &Ctx<'_>, t: &ThreadListItem) -> Element<'static, Msg> {
        let title = match t.title.text() {
            Some(s) if !s.is_empty() => s.to_owned(),
            _ => t
                .title
                .i18n_key()
                .map(|k| t!(k).into_owned())
                .unwrap_or_default(),
        };
        let kind =
            crate::workspace::sidebar::thread_kind_in(c.workspace, t, c.prefs.indicator_scheme)
                .unwrap_or(BadgeKind::Thread);
        let vc = c.room_ctx(&t.rid);
        let mut meta = format!(
            "{} · {}",
            t.room_name,
            t!("room_panel.replies", count = t.tcount)
        );
        if t.tlm.is_some() {
            meta = format!(
                "{meta} · {}",
                t!(
                    "room_panel.last_reply",
                    time = common::time_label(t.tlm, &vc)
                )
            );
        }
        let main = row![
            kind_dot(kind),
            icon::sized(Icon::MessageCircle, 16.0).style(style::text_secondary),
            column![
                ellipsis(title).size(TEXT_BODY).font(font_semibold()),
                caption(meta),
            ]
            .spacing(2)
            .width(Length::Fill),
            icon_button_with(
                Icon::Check,
                c.online.then(|| Msg::MarkThreadRead {
                    rid: t.rid.clone(),
                    tmid: t.tmid.clone(),
                }),
                t!("inbox.mark_read").into_owned(),
                false,
                28.0,
            ),
            icon_button_with(
                Icon::Reply,
                Some(Msg::ReplyInThread {
                    rid: t.rid.clone(),
                    tmid: t.tmid.clone(),
                }),
                t!("inbox.reply_in_thread").into_owned(),
                false,
                28.0,
            ),
        ]
        .spacing(SPACE_2)
        .align_y(Alignment::Center);
        common::list_row(
            main,
            Some(Msg::OpenThread {
                rid: t.rid.clone(),
                tmid: t.tmid.clone(),
            }),
        )
    }
}

/// The first line of a thread's parent, if loaded.
fn thread_title(ws: &Workspace, tmid: &str) -> String {
    ws.thread_index()
        .parent(tmid)
        .map(|p| {
            // As the sidebar names it: never the server text of an
            // encrypted parent this process has not decrypted.
            if crate::e2e::shown::undecrypted(p) {
                return t!("preview.encrypted").into_owned();
            }
            p.plain_text()
                .lines()
                .map(str::trim)
                .find(|l| !l.is_empty())
                .unwrap_or_default()
                .chars()
                .take(60)
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod rc_tests {
    use super::*;
    use rocketchat::models::{Mention, Room, RoomType, Subscription, User, UserRef};
    use rustket_core::Change;

    fn ws() -> Workspace {
        let mut ws = Workspace::new();
        ws.apply_me(User {
            id: "me".into(),
            username: Some("me".into()),
            ..Default::default()
        });
        for (rid, unread) in [("hot", 2), ("read", 0)] {
            ws.apply_room(
                Change::Inserted,
                Room {
                    id: rid.into(),
                    t: RoomType::Channel,
                    name: Some(rid.into()),
                    ..Default::default()
                },
            );
            ws.apply_subscription(
                Change::Inserted,
                Subscription {
                    id: format!("s-{rid}"),
                    rid: rid.into(),
                    name: rid.into(),
                    t: RoomType::Channel,
                    open: true,
                    unread,
                    user_mentions: unread,
                    alert: unread > 0,
                    u: UserRef {
                        id: "me".into(),
                        username: Some("me".into()),
                        name: None,
                    },
                    ..Default::default()
                },
            );
        }
        ws
    }

    /// The views' room header draws the RC counter of an unread room only,
    /// and a mention of the user tints its row under Rustket only.
    #[test]
    fn views_follow_the_scheme() {
        let ws = ws();
        assert!(room_badge_rc(&ws, "hot").is_some());
        assert!(room_badge_rc(&ws, "read").is_none());
        assert!(room_badge_rc(&ws, "missing").is_none());
        let m = RcMessage {
            id: "m".into(),
            rid: "hot".into(),
            msg: "@me hi".into(),
            u: UserRef {
                id: "bob".into(),
                username: Some("bob".into()),
                name: None,
            },
            mentions: Some(vec![Mention {
                id: "me".into(),
                username: Some("me".into()),
                ..Default::default()
            }]),
            ..Default::default()
        };
        assert!(mention_tint(IndicatorScheme::Rustket, &ws, &m));
        assert!(!mention_tint(IndicatorScheme::RocketChat, &ws, &m));
    }
}
