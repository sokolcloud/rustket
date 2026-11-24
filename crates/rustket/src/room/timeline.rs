//! Message timeline of a room: grouped message rows with day/unread
//! separators, rich bodies (`room::render`), reactions, thread/discussion
//! summaries, hover toolbar and context menu actions, downloads, dialogs.
//! Scrolling is anchored to the bottom; older history loads near the top
//! (`Effect::LoadOlder`), new messages wait below a "jump to latest" button
//! while the user reads older ones. Only a window of the newest loaded
//! messages is built, and every row is a `lazy` widget.
//!
//! The row building ([`list`], [`row`]), the row interactions ([`interact`])
//! and the body interactions ([`viewer`]) are shared with the thread view.
//! Strings: `locales/room_timeline.yml` (`room_timeline.*`).

mod actions;
mod dialogs;
pub mod interact;
pub(crate) mod list;
mod poll_card;
pub(crate) mod row;
pub(crate) mod select;
pub(crate) mod transfer;
pub mod viewer;

use std::collections::BTreeSet;
use std::time::Duration;

use chrono::Local;
use iced::widget::scrollable::{RelativeOffset, Viewport};
use iced::widget::{button, column, container, row as hrow, scrollable, sensor, space, text};
use iced::{Alignment, Element, Length, Padding, Subscription, Task};
use rocketchat::RcDate;
use rust_i18n::t;
use rustket_core::config::IndicatorScheme;
use rustket_core::workspace::Changes;
use rustket_ui::icon::{self, Icon};
use rustket_ui::style;
use rustket_ui::theme::{SPACE_1, SPACE_2, SPACE_3, SPACE_4, TEXT_CAPTION, TEXT_SMALL};
use rustket_ui::widget::{
    BadgeKind, BannerKind, empty_state, keyed_list, loading_placeholder, spinner,
};

use super::render::system;
use super::{Action, Effect, UpdateCtx, ViewCtx};

pub use actions::{
    Act, available as available_actions, channel_url, keep_offer, kept_thread_of, permalink,
    star_thread_offer,
};
pub use interact::{Interactions, Msg as ItemMsg, Source};
pub use viewer::{Viewer, ViewerMsg, opens_image as viewer_opens_image};

/// History pages loaded at most while looking for a jump target.
const SEEK_PAGES: u32 = 20;
/// How long a jumped-to message stays highlighted: 4 s, Rocket.Chat 2 s
/// (`useTryToJumpToMessage`).
pub(crate) fn selected_for(scheme: IndicatorScheme) -> Duration {
    match scheme {
        IndicatorScheme::Rustket => Duration::from_secs(4),
        IndicatorScheme::RocketChat => Duration::from_secs(2),
    }
}

#[derive(Debug, Clone)]
pub enum Message {
    /// Scrolled near the top: request the next older page.
    ReachedTop,
    Scrolled(Viewport),
    JumpToLatest,
    /// Retry a failed history page.
    LoadOlder,
    Item(ItemMsg),
    /// Scroll the jumped-to message into view (once a frame shows it).
    Reveal(String),
    /// The highlight with this stamp ends.
    Unselect(u64),
    /// Leave selection mode (Cancel, Escape).
    ClearPicks,
    /// Forward the selected messages (asks for the target room).
    ForwardPicked,
    /// Copy the permalinks of the selected messages.
    CopyPickedLinks,
}

/// A message being looked for in older history.
#[derive(Debug, Clone)]
struct Seek {
    mid: String,
    ts: i64,
    pages: u32,
}

pub struct Timeline {
    rid: String,
    items: Interactions,
    /// Built window, bottom following and the frozen view while scrolled up.
    follow: list::Follow,
    scroll_id: iced::widget::Id,
    /// Jumped-to message and the stamp of its highlight.
    selected: Option<(String, u64)>,
    stamp: u64,
    seek: Option<Seek>,
    /// Selected messages (`None`: not selecting).
    picked: Option<BTreeSet<String>>,
}

/// Id of the container around a jumped-to row.
pub(crate) fn row_id(mid: &str) -> iced::widget::Id {
    iced::widget::Id::from(format!("message-row-{mid}"))
}

impl Timeline {
    pub fn new(rid: &str) -> Self {
        Self {
            rid: rid.to_owned(),
            items: Interactions::new(Source::Room),
            follow: list::Follow::default(),
            scroll_id: iced::widget::Id::unique(),
            selected: None,
            stamp: 0,
            seek: None,
            picked: None,
        }
    }

    /// Selection mode is on.
    pub fn picking(&self) -> bool {
        self.picked.is_some()
    }

    /// Selects or deselects `mid` (starting selection mode); a full
    /// selection refuses more with a warning.
    fn pick(&mut self, mid: &str, ctx: &UpdateCtx<'_>) -> Action<Message> {
        let Some(item) = interact::find_item(ctx.workspace, ctx.rid, self.items.source(), mid)
        else {
            return Action::none();
        };
        let set = self.picked.get_or_insert_with(BTreeSet::new);
        if !set.contains(mid)
            && !select::pickable(
                ctx.workspace,
                self.items.source(),
                &item,
                chrono::Utc::now(),
            )
        {
            return Action::none();
        }
        self.items.menu = None;
        match select::toggle(set, mid, select::MAX_PICKED) {
            Ok(()) => Action::none(),
            Err(select::Full) => Action::effect(Effect::Toast(
                BannerKind::Warning,
                t!("room_timeline.pick_limit", max = select::MAX_PICKED).into_owned(),
                String::new(),
            )),
        }
    }

    /// Whether a read mark would clear anything (unread threads stay).
    fn unread(ctx: &UpdateCtx<'_>) -> bool {
        ctx.workspace
            .subscription(ctx.rid)
            .is_some_and(super::mark_clears)
    }

    fn snap_to_bottom(&self) -> Task<Message> {
        iced::widget::operation::snap_to(self.scroll_id.clone(), RelativeOffset::START)
    }

    /// The view follows the newest messages (not scrolled up, frozen or
    /// held at a jumped-to message).
    pub fn at_bottom(&self) -> bool {
        self.follow.at_bottom
    }

    /// The jumped-to message, if one is highlighted.
    pub fn selected(&self) -> Option<&str> {
        self.selected.as_ref().map(|(m, _)| m.as_str())
    }

    pub fn update(&mut self, msg: Message, ctx: &mut UpdateCtx<'_>) -> Action<Message> {
        match msg {
            Message::ReachedTop => self.reach_top(ctx),
            Message::LoadOlder => Action::effect(Effect::LoadOlder),
            Message::Scrolled(vp) => self.scrolled(vp, ctx),
            Message::JumpToLatest => {
                self.follow.follow_bottom();
                // A click: the window is focused and this room is visible.
                let mut a = Action::task(self.snap_to_bottom());
                if Self::unread(ctx) {
                    a = a.with(Effect::MarkRead);
                }
                a
            }
            Message::Item(ItemMsg::Act(mid, Act::Select)) => {
                self.items.menu = None;
                self.picked = Some(BTreeSet::new());
                self.pick(&mid, ctx)
            }
            Message::Item(ItemMsg::Pick(mid)) => self.pick(&mid, ctx),
            Message::Item(m) => self.items.update(m, ctx).map(Message::Item),
            Message::ClearPicks => {
                self.picked = None;
                Action::none()
            }
            Message::ForwardPicked => {
                let Some(set) = self.picked.take() else {
                    return Action::none();
                };
                let refs = select::ordered_refs(ctx.workspace, ctx.rid, &set);
                if refs.is_empty() {
                    return Action::none();
                }
                Action::effect(Effect::Forward(refs))
            }
            Message::CopyPickedLinks => {
                let Some(set) = self.picked.as_ref().filter(|s| !s.is_empty()) else {
                    return Action::none();
                };
                let links = select::links(ctx.server, ctx.workspace, ctx.rid, set);
                Action::task(iced::clipboard::write(links)).with(Effect::Toast(
                    BannerKind::Success,
                    t!("room_timeline.links_copied").into_owned(),
                    String::new(),
                ))
            }
            Message::Reveal(mid) => {
                Action::task(list::reveal(self.scroll_id.clone(), row_id(&mid), true))
            }
            Message::Unselect(stamp) => {
                if self.selected.as_ref().is_some_and(|(_, s)| *s == stamp) {
                    self.selected = None;
                }
                Action::none()
            }
        }
    }

    /// Shows message `mid` (sent at `ts`): highlighted and scrolled into
    /// view when it is loaded, after loading older pages (up to
    /// [`SEEK_PAGES`]) when it is older than the loaded history.
    pub fn jump_to(
        &mut self,
        mid: &str,
        ts: Option<RcDate>,
        ctx: &mut UpdateCtx<'_>,
    ) -> Action<Message> {
        let Some(tl) = ctx.workspace.timeline(ctx.rid) else {
            return Action::none();
        };
        if tl.contains(mid) {
            self.seek = None;
            return self.select(mid, ctx);
        }
        let older = match (ts, tl.oldest()) {
            (Some(ts), Some(oldest)) => oldest
                .ts
                .is_none_or(|o| ts.timestamp_millis() < o.timestamp_millis()),
            (Some(_), None) => true,
            (None, _) => false,
        };
        let p = &tl.pagination;
        match ts {
            Some(ts) if older && (p.has_more_before || p.loading_before) => {
                self.seek = Some(Seek {
                    mid: mid.to_owned(),
                    ts: ts.timestamp_millis(),
                    pages: 0,
                });
                self.continue_seek(ctx)
            }
            _ => Action::effect(Effect::Toast(
                BannerKind::Info,
                t!("room_timeline.jump_not_loaded").into_owned(),
                String::new(),
            )),
        }
    }

    /// One more page towards the sought message, or the end of the search.
    fn continue_seek(&mut self, ctx: &mut UpdateCtx<'_>) -> Action<Message> {
        let Some(seek) = self.seek.clone() else {
            return Action::none();
        };
        let Some(tl) = ctx.workspace.timeline(ctx.rid) else {
            self.seek = None;
            return Action::none();
        };
        if tl.contains(&seek.mid) {
            self.seek = None;
            return self.select(&seek.mid, ctx);
        }
        let p = &tl.pagination;
        if p.loading_before {
            return Action::none();
        }
        let reached = tl
            .oldest()
            .and_then(|m| m.ts)
            .is_some_and(|o| o.timestamp_millis() <= seek.ts);
        if reached || !p.has_more_before || p.error.is_some() || seek.pages >= SEEK_PAGES {
            self.seek = None;
            return Action::effect(Effect::Toast(
                BannerKind::Info,
                t!("room_timeline.jump_not_loaded").into_owned(),
                String::new(),
            ));
        }
        if let Some(s) = &mut self.seek {
            s.pages += 1;
        }
        Action::effect(Effect::LoadOlder)
    }

    fn select(&mut self, mid: &str, ctx: &UpdateCtx<'_>) -> Action<Message> {
        let Some(tl) = ctx.workspace.timeline(ctx.rid) else {
            return Action::none();
        };
        // Rows down from the target, kept still while the user looks at it.
        if !self.follow.hold_at(tl, mid) {
            return Action::none();
        }
        self.stamp += 1;
        let stamp = self.stamp;
        self.selected = Some((mid.to_owned(), stamp));
        let reveal = mid.to_owned();
        Action::task(Task::batch([
            // After the frame that builds the row.
            Task::perform(tokio::time::sleep(Duration::from_millis(60)), move |_| {
                Message::Reveal(reveal.clone())
            }),
            Task::perform(
                tokio::time::sleep(selected_for(ctx.prefs.indicator_scheme)),
                move |_| Message::Unselect(stamp),
            ),
        ]))
    }

    fn scrolled(&mut self, vp: Viewport, ctx: &mut UpdateCtx<'_>) -> Action<Message> {
        let s = self.follow.scrolled(&vp, ctx.workspace.timeline(ctx.rid));
        let mut action = Action::none();
        // Back at the bottom; scrolling happens in the visible room of a
        // focused window.
        if s.back_at_bottom && Self::unread(ctx) {
            action = action.with(Effect::MarkRead);
        }
        if s.near_top {
            let more = self.reach_top(ctx);
            action.task = more.task;
            action.effects.extend(more.effects);
        }
        action
    }

    /// Near the top: build more of the loaded rows, else load an older page.
    fn reach_top(&mut self, ctx: &mut UpdateCtx<'_>) -> Action<Message> {
        let Some(tl) = ctx.workspace.timeline(ctx.rid) else {
            return Action::none();
        };
        if !self.follow.grow(tl) {
            return Action::none();
        }
        let p = &tl.pagination;
        if p.has_more_before && !p.loading_before && p.error.is_none() {
            Action::effect(Effect::LoadOlder)
        } else {
            Action::none()
        }
    }

    pub fn on_changes(&mut self, changes: &Changes, ctx: &mut UpdateCtx<'_>) -> Action<Message> {
        if !changes.timelines.contains(ctx.rid) {
            return Action::none();
        }
        let Some(tl) = ctx.workspace.timeline(ctx.rid) else {
            return Action::none();
        };
        // Forget state of deleted messages.
        self.items.prune(|mid| tl.contains(mid));
        if let Some(set) = &mut self.picked {
            set.retain(|mid| tl.contains(mid));
        }
        // An own message being sent while scrolled up: jump down to it.
        if self.follow.on_changes(tl) {
            self.seek = None;
            return Action::task(self.snap_to_bottom());
        }
        if self.seek.is_some() {
            return self.continue_seek(ctx);
        }
        Action::none()
    }

    /// Window focus is handled by the app (`RoomView::set_visible`), which
    /// knows which server and room are visible.
    pub fn subscription(&self) -> Subscription<Message> {
        Subscription::none()
    }

    /// The open dialog or lightbox (shown over the whole room by the room
    /// view), with its key and the message that dismisses it.
    pub fn overlay<'a>(&'a self, ctx: &ViewCtx<'a>) -> Option<(u8, Element<'a, Message>, Message)> {
        self.items
            .overlay(ctx)
            .map(|(k, e, d)| (k, e.map(Message::Item), Message::Item(d)))
    }

    pub fn view<'a>(&'a self, ctx: &ViewCtx<'a>) -> Element<'a, Message> {
        let ws = ctx.workspace;
        let Some(tl) = ws.timeline(&self.rid) else {
            return loading_placeholder(Some(t!("room_timeline.loading").into_owned()));
        };
        if tl.is_empty() && !tl.pagination.loaded {
            if tl.pagination.error.is_some() && !tl.pagination.loading_before {
                return first_page_failed();
            }
            return loading_placeholder(Some(t!("room_timeline.loading").into_owned()));
        }
        let env = row::Env {
            server_id: ctx.server_id,
            server: ctx.server,
            ws,
            images: ctx.images,
            render: &self.items.viewer.render,
            prefs: ctx.prefs,
            rid: &self.rid,
            source: self.items.source(),
        };
        let me = ws.user_id().unwrap_or_default();
        let plan = list::plan(
            tl,
            list::visible_in(ws, &self.rid),
            self.follow.frozen(),
            self.follow.window,
            me,
            ws.settings().grouping_period_secs(),
            &Local,
        );
        if plan.rows.is_empty() && plan.new_below == 0 {
            return empty_state(
                Icon::MessagesSquare,
                t!("room_timeline.empty_title"),
                t!("room_timeline.empty_subtitle"),
            );
        }

        let sub = ws.subscription(&self.rid);
        let common = list::Common::new(ctx, &self.rid);
        let mut col = keyed_list::<u64, Message, _, _>()
            .width(Length::Fill)
            .padding(Padding::from([SPACE_2, 0.0]));
        // Near the top (also when nothing scrolls yet): build more rows or
        // load the next page; the key re-arms it after every change.
        let top: Element<'a, Message> = if plan.more_above {
            space().height(1).into()
        } else {
            // Cached messages shown while the server is unreachable: the
            // offline strip explains the failed first page.
            let offline = tl.has_seeded()
                && *ws.connection() != rustket_core::workspace::ConnectionState::Connected;
            top_indicator(tl, offline)
        };
        col = col.push(
            list::TOP_KEY,
            sensor(top)
                .key((tl.len(), self.follow.window, tl.pagination.loading_before))
                .anticipate(list::TOP_THRESHOLD)
                .on_show(|_| Message::ReachedTop),
        );
        let selected = self.selected();
        for r in &plan.rows {
            let m = &r.item.message;
            let flags = row::RowFlags {
                sequential: r.sequential,
                menu_open: self.items.menu.as_deref() == Some(m.id.as_str()),
                thread_unread: parent_thread_unread(sub, &m.id),
                thread_kind: parent_thread_kind(sub, &m.id, ctx.prefs.indicator_scheme),
                selected: selected == Some(m.id.as_str()),
                picking: self.picked.is_some(),
                picked: self.picked.as_ref().is_some_and(|p| p.contains(&m.id)),
            };
            let el = list::row(env, &common, r, flags);
            let el = if flags.selected {
                list::anchored(el, &m.id)
            } else {
                el
            };
            col = col.push(list::id_key(&m.id), el.map(Message::Item));
        }
        let new_below = plan.new_below;
        let list = scrollable(col)
            .id(self.scroll_id.clone())
            .anchor_bottom()
            .on_scroll(Message::Scrolled)
            .width(Length::Fill)
            .height(Length::Fill)
            .style(style::scrollable);
        let jump = (!self.follow.at_bottom || new_below > 0).then(|| {
            let label = if new_below > 0 {
                t!("room_timeline.new_messages", count = new_below).into_owned()
            } else {
                t!("room_timeline.jump_to_latest").into_owned()
            };
            (label, Message::JumpToLatest)
        });
        // The bar slot always exists, so the list keeps its tree position
        // (and scroll state) when selection mode starts or ends.
        let bar: Element<'a, Message> = match &self.picked {
            Some(set) => selection_bar(set.len()),
            None => space().height(0).into(),
        };
        column![list::with_jump_button(list.into(), jump), bar].into()
    }
}

/// "3 messages selected" with Copy links, Forward and Cancel.
fn selection_bar<'a>(count: usize) -> Element<'a, Message> {
    use rustket_ui::widget::{ButtonSize, ghost_button, primary_button, secondary_button};
    let n = count as u64;
    let some = count > 0;
    let bar = container(
        hrow![
            text(
                t!(
                    format!("room_timeline.picked_{}", super::render::plural(n)),
                    count = n
                )
                .into_owned()
            )
            .size(TEXT_SMALL)
            .width(Length::Fill),
            ghost_button(t!("app.cancel").into_owned(), Some(Message::ClearPicks))
                .size(ButtonSize::Small),
            secondary_button(
                t!("room_timeline.copy_links").into_owned(),
                some.then_some(Message::CopyPickedLinks)
            )
            .icon(Icon::Link)
            .size(ButtonSize::Small),
            primary_button(
                t!("room_timeline.act_forward").into_owned(),
                some.then_some(Message::ForwardPicked)
            )
            .icon(Icon::Forward)
            .size(ButtonSize::Small),
        ]
        .spacing(SPACE_2)
        .align_y(Alignment::Center),
    )
    .padding([SPACE_2, SPACE_3])
    .width(Length::Fill)
    .style(style::container_composer(false));
    container(bar).padding([SPACE_1, SPACE_4]).into()
}

/// The first history page failed: error with a retry button.
fn first_page_failed<'a>() -> Element<'a, Message> {
    container(
        column![
            icon::sized(Icon::CircleAlert, 28.0).style(style::text_danger),
            text(t!("room_timeline.history_first_failed").into_owned())
                .size(TEXT_SMALL)
                .style(style::text_secondary),
            button(text(t!("room_timeline.retry").into_owned()).size(TEXT_SMALL))
                .padding([4, 12])
                .style(style::button_secondary)
                .on_press(Message::LoadOlder),
        ]
        .spacing(SPACE_2)
        .align_x(Alignment::Center),
    )
    .center(Length::Fill)
    .into()
}

fn top_indicator<'a>(tl: &rustket_core::timeline::Timeline, offline: bool) -> Element<'a, Message> {
    let p = &tl.pagination;
    let content: Element<'a, Message> = if p.loading_before {
        hrow![
            spinner(16.0),
            text(t!("room_timeline.loading_older").into_owned())
                .size(TEXT_CAPTION)
                .style(style::text_hint)
        ]
        .spacing(SPACE_2)
        .align_y(Alignment::Center)
        .into()
    } else if p.error.is_some() && !offline {
        hrow![
            text(t!("room_timeline.history_failed").into_owned())
                .size(TEXT_CAPTION)
                .style(style::text_danger),
            button(text(t!("room_timeline.retry").into_owned()).size(TEXT_CAPTION))
                .padding([0, 4])
                .style(style::button_link)
                .on_press(Message::LoadOlder)
        ]
        .spacing(SPACE_2)
        .align_y(Alignment::Center)
        .into()
    } else if !p.has_more_before && p.loaded {
        column![
            icon::sized(Icon::MessagesSquare, 28.0).style(style::text_hint),
            text(t!("room_timeline.start_of_conversation").into_owned())
                .size(TEXT_SMALL)
                .style(style::text_secondary),
        ]
        .spacing(SPACE_2)
        .align_x(Alignment::Center)
        .into()
    } else {
        space().height(24).into()
    };
    container(content)
        .center_x(Length::Fill)
        .padding([SPACE_4, 0.0])
        .into()
}

/// Whether the thread of parent message `mid` has unread replies (the dot of
/// its thread summary): `tunread` of the room's subscription lists it.
pub(crate) fn parent_thread_unread(
    sub: Option<&rocketchat::models::Subscription>,
    mid: &str,
) -> bool {
    sub.and_then(|s| s.tunread.as_ref())
        .is_some_and(|t| t.iter().any(|x| x == mid))
}

/// The colour of that dot, `Some` iff [`parent_thread_unread`]: Thread, or
/// under Rocket.Chat its thread rule (`tunreadUser` Mention, `tunreadGroup`
/// GroupMention, else Thread).
pub(crate) fn parent_thread_kind(
    sub: Option<&rocketchat::models::Subscription>,
    mid: &str,
    scheme: IndicatorScheme,
) -> Option<BadgeKind> {
    if !parent_thread_unread(sub, mid) {
        return None;
    }
    Some(match scheme {
        IndicatorScheme::Rustket => BadgeKind::Thread,
        IndicatorScheme::RocketChat => sub
            .and_then(|s| rustket_core::indicators::rc_thread_kind(s, mid))
            .map_or(BadgeKind::Thread, crate::workspace::sidebar::widget_kind),
    })
}

/// Rocket.Chat's tooltip on an unread thread's dot ("Mentions you",
/// "mention-all", "Unread"); none under Rustket.
pub(crate) fn rc_thread_dot_tip(kind: BadgeKind, scheme: IndicatorScheme) -> Option<String> {
    if scheme != IndicatorScheme::RocketChat {
        return None;
    }
    let key = match kind {
        BadgeKind::Mention => "room_timeline.rc_thread_dot_mention",
        BadgeKind::GroupMention => "room_timeline.rc_thread_dot_all",
        _ => "room_timeline.rc_thread_dot_unread",
    };
    Some(t!(key).into_owned())
}

#[cfg(test)]
mod rc_tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    /// H2: the unread thread dot's tooltip, Rocket.Chat only.
    #[test]
    fn thread_dot_tip_follows_the_scheme() {
        let rk = IndicatorScheme::Rustket;
        let rc = IndicatorScheme::RocketChat;
        for kind in [
            BadgeKind::Mention,
            BadgeKind::GroupMention,
            BadgeKind::Thread,
        ] {
            assert_eq!(rc_thread_dot_tip(kind, rk), None);
        }
        for (kind, key, en) in [
            (
                BadgeKind::Mention,
                "room_timeline.rc_thread_dot_mention",
                "Mentions you",
            ),
            (
                BadgeKind::GroupMention,
                "room_timeline.rc_thread_dot_all",
                "Mention All",
            ),
            (
                BadgeKind::Thread,
                "room_timeline.rc_thread_dot_unread",
                "Unread",
            ),
        ] {
            assert_eq!(rc_thread_dot_tip(kind, rc), Some(t!(key).into_owned()));
            assert_eq!(t!(key, locale = "en"), en);
        }
    }

    /// H7: the jump highlight lasts 4 s, 2 s under Rocket.Chat.
    #[test]
    fn jump_highlight_duration_follows_the_scheme() {
        assert_eq!(
            selected_for(IndicatorScheme::Rustket),
            Duration::from_secs(4)
        );
        assert_eq!(
            selected_for(IndicatorScheme::RocketChat),
            Duration::from_secs(2)
        );
    }

    /// H2/H3: the summary dot shows iff `tunread` lists the parent in both
    /// schemes; Rocket.Chat colours it by `tunreadUser` / `tunreadGroup`.
    #[test]
    fn thread_dot_kind_follows_the_scheme() {
        let sub = rocketchat::models::Subscription {
            tunread: Some(vec!["a".into(), "b".into(), "c".into()]),
            tunread_user: Some(vec!["a".into()]),
            tunread_group: Some(vec!["b".into()]),
            // R1–R3 do not matter for the dot.
            disable_notifications: Some(true),
            hide_unread_status: Some(true),
            hide_mention_status: Some(true),
            ..Default::default()
        };
        let rc = IndicatorScheme::RocketChat;
        let rk = IndicatorScheme::Rustket;
        for (mid, kind) in [
            ("a", BadgeKind::Mention),
            ("b", BadgeKind::GroupMention),
            ("c", BadgeKind::Thread),
        ] {
            assert!(parent_thread_unread(Some(&sub), mid));
            assert_eq!(parent_thread_kind(Some(&sub), mid, rc), Some(kind), "{mid}");
            assert_eq!(
                parent_thread_kind(Some(&sub), mid, rk),
                Some(BadgeKind::Thread),
                "{mid}"
            );
        }
        for scheme in [rc, rk] {
            assert_eq!(parent_thread_kind(Some(&sub), "d", scheme), None);
            assert_eq!(parent_thread_kind(None, "a", scheme), None);
        }
    }
}
