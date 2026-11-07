//! Message rendering shared by the timeline and the right panel (threads,
//! pinned, search results …). Owner: timeline agent; other components only
//! call [`message_body`] and handle [`RenderMsg`].
//! Strings: `locales/room_render.yml` (`room_render.*`).
//! STUB: the plain text of the message.

use iced::Element;
use iced::widget::text;
use rocketchat::models::Message;

use super::ViewCtx;

/// Interactions inside a rendered message body.
#[derive(Debug, Clone)]
pub enum RenderMsg {
    OpenUrl(String),
    /// A `#channel` mention (room name).
    OpenRoom(String),
    /// A `@user` mention (username).
    OpenUser(String),
    /// Reveal/hide a spoiler: message id and spoiler index.
    ToggleSpoiler(String, usize),
    /// Copy text (code block) to the clipboard.
    Copy(String),
}

/// The body of `msg` (markdown, attachments, …) without author/time.
pub fn message_body<'a>(msg: &'a Message, ctx: &ViewCtx<'a>) -> Element<'a, RenderMsg> {
    let _ = ctx;
    text(msg.plain_text()).into()
}
