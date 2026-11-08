//! Dialogs of an omnichannel room, opened from the header quick actions
//! (`docs/omnichannel.md` §11–§15): close, forward, return to queue, place on
//! hold and the e-mail transcript. They are drawn over the whole room by
//! [`super::room::OmniRoom::overlay`]; errors are shown inside the dialog.

pub mod close_chat;
pub mod confirm;
pub mod forward_chat;
pub mod transcript;

use iced::Element;
use rocketchat::models::Room;
use rustket_core::Workspace;

use crate::room::{UpdateCtx, ViewCtx};

/// The open dialog.
pub enum Dialog {
    Close(Box<close_chat::CloseChatDialog>),
    Forward(Box<forward_chat::ForwardDialog>),
    Confirm(confirm::Confirm),
    Transcript(Box<transcript::TranscriptDialog>),
}

#[derive(Debug, Clone)]
pub enum Msg {
    Close(close_chat::Msg),
    Forward(forward_chat::Msg),
    Confirm(confirm::Msg),
    Transcript(transcript::Msg),
}

/// What a dialog asks its owner to do after an update.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    Stay,
    /// Closed without doing anything.
    Dismiss,
    /// The action succeeded: close the dialog, toast `toast`, and close the
    /// room view when `close_room` (the chat left the agent).
    Finished {
        toast: Option<String>,
        close_room: bool,
    },
}

/// The room document of `rid`, if the workspace has it.
pub fn room_of(ws: &Workspace, rid: &str) -> Option<Room> {
    ws.room(rid).and_then(|e| e.room.clone())
}

/// `omnichannelHideConversationAfterClosing` (default `true`, client-side
/// only; §7.4).
pub fn hide_after_closing(ws: &Workspace) -> bool {
    crate::desktop::user_pref(ws, "omnichannelHideConversationAfterClosing")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(true)
}

impl Dialog {
    pub fn update(&mut self, msg: Msg, ctx: &mut UpdateCtx<'_>) -> (iced::Task<Msg>, Outcome) {
        match (self, msg) {
            (Dialog::Close(d), Msg::Close(m)) => {
                let (t, o) = d.update(m, ctx);
                (t.map(Msg::Close), o)
            }
            (Dialog::Forward(d), Msg::Forward(m)) => {
                let (t, o) = d.update(m, ctx);
                (t.map(Msg::Forward), o)
            }
            (Dialog::Confirm(d), Msg::Confirm(m)) => {
                let (t, o) = d.update(m, ctx);
                (t.map(Msg::Confirm), o)
            }
            (Dialog::Transcript(d), Msg::Transcript(m)) => {
                let (t, o) = d.update(m, ctx);
                (t.map(Msg::Transcript), o)
            }
            _ => (iced::Task::none(), Outcome::Stay),
        }
    }

    /// The message that dismisses the dialog (Escape / backdrop), unless busy.
    pub fn dismiss(&self) -> Msg {
        match self {
            Dialog::Close(_) => Msg::Close(close_chat::Msg::Cancel),
            Dialog::Forward(_) => Msg::Forward(forward_chat::Msg::Cancel),
            Dialog::Confirm(_) => Msg::Confirm(confirm::Msg::Cancel),
            Dialog::Transcript(_) => Msg::Transcript(transcript::Msg::Cancel),
        }
    }

    pub fn view<'a>(&'a self, ctx: &ViewCtx<'a>) -> Element<'a, Msg> {
        match self {
            Dialog::Close(d) => d.view(ctx).map(Msg::Close),
            Dialog::Forward(d) => d.view(ctx).map(Msg::Forward),
            Dialog::Confirm(d) => d.view().map(Msg::Confirm),
            Dialog::Transcript(d) => d.view().map(Msg::Transcript),
        }
    }
}
