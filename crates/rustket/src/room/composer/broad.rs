//! Confirmation before a message notifies a whole room (`@all`, `@here`):
//! the check (permissions, room type, member count) and the dialog, shared
//! by the composer and the upload dialog's message.

use iced::Element;
use iced::widget::{column, text};
use rocketchat::models::RoomType;
use rust_i18n::t;
use rustket_core::Workspace;
use rustket_core::permissions;
use rustket_ui::style;
use rustket_ui::theme::{SPACE_2, TEXT_BODY, TEXT_SMALL};
use rustket_ui::widget::{primary_button, secondary_button};

use super::text::broad_mentions;

/// Rooms with fewer other members than this send without asking.
pub const MIN_MEMBERS: u64 = 5;

/// A send waiting for the confirmation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BroadConfirm {
    pub all: bool,
    pub here: bool,
    /// Room name as the composer shows it (`#general`).
    pub room: String,
    /// Members besides the sender (`usersCount - 1`), when known.
    pub members: Option<u64>,
    /// Members online (for `@here`), once fetched.
    pub online: Option<u64>,
    /// `Notifications_Max_Room_Members` when the room is bigger than it.
    pub big_room: Option<u64>,
    /// Matches the online-count answer to this dialog.
    pub seq: u64,
}

/// The confirmation `text` needs before it goes to room `rid`, if any:
/// `@all`/`@here` the user may use, in a room that is not a direct message,
/// with at least [`MIN_MEMBERS`] other members (an unknown count asks).
pub fn broad_check(ws: &Workspace, rid: &str, text: &str) -> Option<BroadConfirm> {
    let entry = ws.room(rid)?;
    if matches!(entry.room_type(), RoomType::Direct) {
        return None;
    }
    let found = broad_mentions(text);
    let all = found.all && permissions::can_mention_all(ws, rid);
    let here = found.here && permissions::can_mention_here(ws, rid);
    if !all && !here {
        return None;
    }
    let users = entry.room.as_ref().and_then(|r| r.users_count);
    let members = users.map(|n| n.saturating_sub(1));
    if members.is_some_and(|m| m < MIN_MEMBERS) {
        return None;
    }
    let max = ws.settings().notifications_max_room_members();
    Some(BroadConfirm {
        all,
        here,
        room: format!("#{}", entry.fname().unwrap_or(entry.name())),
        members,
        online: None,
        big_room: (max > 0 && users.is_some_and(|n| n > max)).then_some(max),
        seq: 0,
    })
}

/// "Notify everyone?" with Cancel and Send.
pub fn dialog<'a, M: Clone + 'a>(c: &BroadConfirm, cancel: M, send: M) -> Element<'a, M> {
    let plural = crate::room::render::plural;
    let room = c.room.as_str();
    let title = if c.all {
        t!("room_composer.broad.title_all")
    } else {
        t!("room_composer.broad.title_here")
    };
    let mut lines = Vec::new();
    if c.all {
        lines.push(match c.members {
            Some(n) => t!(
                format!("room_composer.broad.all_{}", plural(n)),
                count = n,
                room = room
            ),
            None => t!("room_composer.broad.all_unknown", room = room),
        });
    }
    if c.here {
        lines.push(match c.online {
            Some(n) => t!(
                format!("room_composer.broad.here_{}", plural(n)),
                count = n,
                room = room
            ),
            None => t!("room_composer.broad.here_unknown", room = room),
        });
    }
    let mut body = column![].spacing(SPACE_2);
    for l in lines {
        body = body.push(text(l.into_owned()).size(TEXT_BODY));
    }
    if let Some(max) = c.big_room {
        body = body.push(
            text(t!("room_composer.broad.big_room", max = max).into_owned())
                .size(TEXT_SMALL)
                .style(style::text_secondary),
        );
    }
    rustket_ui::widget::dialog(
        title.into_owned(),
        body,
        vec![
            secondary_button(t!("app.cancel"), Some(cancel.clone())).into(),
            primary_button(t!("room_composer.send"), Some(send)).into(),
        ],
        Some(cancel),
    )
}
