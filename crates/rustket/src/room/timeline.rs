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
pub(crate) mod row;
mod transfer;
pub mod viewer;

use std::time::Duration;

use chrono::Local;
use iced::widget::scrollable::{RelativeOffset, Viewport};
use iced::widget::{
    button, column, container, keyed_column, row as hrow, scrollable, sensor, space, text,
};
use iced::{Alignment, Element, Length, Padding, Subscription, Task};
use rocketchat::RcDate;
use rust_i18n::t;
use rustket_core::workspace::Changes;
use rustket_ui::icon::{self, Icon};
use rustket_ui::style;
use rustket_ui::theme::{SPACE_2, SPACE_4, TEXT_CAPTION, TEXT_SMALL};
use rustket_ui::widget::{BannerKind, empty_state, loading_placeholder, spinner};

use super::render::system;
use super::{Action, Effect, UpdateCtx, ViewCtx};

pub use actions::{Act, available as available_actions, channel_url, permalink};
pub use interact::{Interactions, Msg as ItemMsg, Source};
pub use viewer::{Viewer, ViewerMsg};

/// History pages loaded at most while looking for a jump target.
const SEEK_PAGES: u32 = 20;
/// How long a jumped-to message stays highlighted.
pub(crate) const SELECTED_FOR: Duration = Duration::from_secs(4);

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
        }
    }

    fn unread(ctx: &UpdateCtx<'_>) -> bool {
        ctx.workspace
            .subscription(ctx.rid)
            .is_some_and(|s| s.unread > 0 || s.alert)
    }

    fn snap_to_bottom(&self) -> Task<Message> {
        iced::widget::operation::snap_to(self.scroll_id.clone(), RelativeOffset::START)
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
            Message::Item(m) => self.items.update(m, ctx).map(Message::Item),
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
            Task::perform(tokio::time::sleep(SELECTED_FOR), move |_| {
                Message::Unselect(stamp)
            }),
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

    /// Window focus is handled by the app (`mark_active_room_read`), which
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

        let tunread = ws.subscription(&self.rid).and_then(|s| s.tunread.as_ref());
        let common = list::Common::new(ctx, &self.rid);
        let mut col = keyed_column::<u64, Message, _, _>([])
            .width(Length::Fill)
            .padding(Padding::from([SPACE_2, 0.0]));
        // Near the top (also when nothing scrolls yet): build more rows or
        // load the next page; the key re-arms it after every change.
        let top: Element<'a, Message> = if plan.more_above {
            space().height(1).into()
        } else {
            top_indicator(tl)
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
                thread_unread: tunread.is_some_and(|t| t.contains(&m.id)),
                selected: selected == Some(m.id.as_str()),
            };
            let el = list::row(env, &common, r, flags);
            let el = if flags.selected {
                container(el).id(row_id(&m.id)).into()
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
        if self.follow.at_bottom && new_below == 0 {
            return list.into();
        }
        let label = if new_below > 0 {
            t!("room_timeline.new_messages", count = new_below).into_owned()
        } else {
            t!("room_timeline.jump_to_latest").into_owned()
        };
        list::with_jump_button(list.into(), label, Message::JumpToLatest)
    }
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

fn top_indicator<'a>(tl: &rustket_core::timeline::Timeline) -> Element<'a, Message> {
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
    } else if p.error.is_some() {
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
