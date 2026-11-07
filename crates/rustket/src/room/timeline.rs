//! Message timeline of a room. STUB: shows a placeholder with the number of
//! loaded messages; to be completed (message rows, date/unread separators,
//! infinite scroll via `Effect::LoadOlder`, mark-as-read on scroll to bottom).
//! Strings: `locales/room_timeline.yml` (`room_timeline.*`).

use iced::{Element, Subscription};
use rust_i18n::t;
use rustket_core::workspace::Changes;
use rustket_ui::icon::Icon;
use rustket_ui::widget::{empty_state, loading_placeholder};

use super::{Action, Effect, UpdateCtx, ViewCtx};

#[derive(Debug, Clone)]
pub enum Message {
    /// Scrolled near the top: request the next older page.
    ReachedTop,
}

pub struct Timeline {
    rid: String,
}

impl Timeline {
    pub fn new(rid: &str) -> Self {
        Self {
            rid: rid.to_owned(),
        }
    }

    pub fn update(&mut self, msg: Message, ctx: &mut UpdateCtx<'_>) -> Action<Message> {
        let _ = ctx;
        match msg {
            Message::ReachedTop => Action::effect(Effect::LoadOlder),
        }
    }

    pub fn on_changes(&mut self, changes: &Changes, ctx: &mut UpdateCtx<'_>) -> Action<Message> {
        let _ = (changes, ctx);
        Action::none()
    }

    pub fn subscription(&self) -> Subscription<Message> {
        Subscription::none()
    }

    pub fn view<'a>(&'a self, ctx: &ViewCtx<'a>) -> Element<'a, Message> {
        match ctx.workspace.timeline(&self.rid) {
            Some(t) if !t.is_empty() => empty_state(
                Icon::MessagesSquare,
                t!("room_timeline.placeholder_title"),
                t!("room_timeline.placeholder_count", count = t.len()),
            ),
            Some(_) | None => loading_placeholder(Some(t!("room_timeline.loading").into_owned())),
        }
    }
}
