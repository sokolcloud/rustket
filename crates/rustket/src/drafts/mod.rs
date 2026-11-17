//! "Drafts & scheduled": the account's scheduled messages
//! (`rustket_core::scheduled`) and the rooms and threads with an unsent
//! composer draft, a view shown in place of the room area
//! (`workspace::MainView::Drafts`, a pinned sidebar row, Ctrl/Cmd+Shift+D).
//! Also the dialog of scheduled messages that were not sent on time
//! ([`missed`]). Strings: `locales/scheduled.yml` (`scheduled.*`).
//!
//! The component holds the open dialog and turns input into [`Out`]
//! requests; the app (`app/drafts.rs`) applies them to the workspace and
//! the outbox.

pub mod missed;
pub mod view;

use iced::widget::text_editor;
use rustket_core::Workspace;

use crate::later::when::{self, Kind, Picked, WhenPicker};

/// Longest text of a scheduled message accepted in the edit dialog
/// (the server's own limit is checked when it goes out).
pub const MAX_EDIT_CHARS: usize = 20_000;

/// A dialog of the view.
#[derive(Debug)]
pub enum Dialog {
    Reschedule {
        id: String,
        picker: WhenPicker,
    },
    Edit {
        id: String,
        content: text_editor::Content,
    },
    /// "Delete this scheduled message?"
    Delete(String),
}

#[derive(Debug, Clone)]
pub enum Msg {
    Close,
    SendNow(String),
    Reschedule(String),
    Edit(String),
    Delete(String),
    ConfirmDelete,
    Picker(when::Msg),
    EditAction(text_editor::Action),
    SaveEdit,
    CloseDialog,
    /// Open a room (or its thread) with its draft or scheduled message.
    Open {
        rid: String,
        tmid: Option<String>,
    },
}

/// Requests to the app.
#[derive(Debug, Clone, PartialEq)]
pub enum Out {
    Close,
    SendNow(String),
    Reschedule(String, i64),
    SetText(String, String),
    Delete(String),
    Open { rid: String, tmid: Option<String> },
}

/// What [`DraftsView::update`] needs besides the view.
pub struct UpdateCtx<'a> {
    pub workspace: &'a Workspace,
    pub now: chrono::DateTime<chrono::Local>,
    pub morning: u32,
}

/// The open Drafts & scheduled view.
#[derive(Debug, Default)]
pub struct DraftsView {
    pub dialog: Option<Dialog>,
}

impl DraftsView {
    pub fn new() -> Self {
        Self::default()
    }

    /// The scheduled message an edit or reschedule dialog is about (it must
    /// not go out meanwhile).
    pub fn held_id(&self) -> Option<&str> {
        match self.dialog.as_ref()? {
            Dialog::Reschedule { id, .. } | Dialog::Edit { id, .. } => Some(id),
            Dialog::Delete(_) => None,
        }
    }

    /// A dialog is open (Esc closes it first).
    pub fn has_dialog(&self) -> bool {
        self.dialog.is_some()
    }

    pub fn update(&mut self, msg: Msg, c: &UpdateCtx<'_>) -> Vec<Out> {
        let ws = c.workspace;
        match msg {
            Msg::Close => vec![Out::Close],
            Msg::SendNow(id) => vec![Out::SendNow(id)],
            Msg::Open { rid, tmid } => vec![Out::Open { rid, tmid }],
            Msg::Reschedule(id) => {
                if let Some(s) = ws.scheduled().get(&id) {
                    self.dialog = Some(Dialog::Reschedule {
                        picker: WhenPicker::new(Kind::Schedule, Some(s.send_at)),
                        id,
                    });
                }
                Vec::new()
            }
            Msg::Edit(id) => {
                if let Some(s) = ws.scheduled().get(&id) {
                    self.dialog = Some(Dialog::Edit {
                        content: text_editor::Content::with_text(&s.message.msg),
                        id,
                    });
                }
                Vec::new()
            }
            Msg::Delete(id) => {
                if ws.scheduled().get(&id).is_some() {
                    self.dialog = Some(Dialog::Delete(id));
                }
                Vec::new()
            }
            Msg::ConfirmDelete => match self.dialog.take() {
                Some(Dialog::Delete(id)) => vec![Out::Delete(id)],
                other => {
                    self.dialog = other;
                    Vec::new()
                }
            },
            Msg::Picker(m) => {
                let Some(Dialog::Reschedule { picker, .. }) = &mut self.dialog else {
                    return Vec::new();
                };
                match picker.update(m, c.now, c.morning) {
                    Some(Picked::At(at)) => match self.dialog.take() {
                        Some(Dialog::Reschedule { id, .. }) => vec![Out::Reschedule(id, at)],
                        _ => Vec::new(),
                    },
                    _ => Vec::new(),
                }
            }
            Msg::EditAction(a) => {
                if let Some(Dialog::Edit { content, .. }) = &mut self.dialog {
                    let grows = a.is_edit();
                    if !grows || content.text().chars().count() < MAX_EDIT_CHARS {
                        content.perform(a);
                    }
                }
                Vec::new()
            }
            Msg::SaveEdit => {
                let text = match &self.dialog {
                    Some(Dialog::Edit { content, .. }) => content.text(),
                    _ => return Vec::new(),
                };
                if text.trim().is_empty() {
                    return Vec::new();
                }
                match self.dialog.take() {
                    Some(Dialog::Edit { id, .. }) => vec![Out::SetText(id, text)],
                    _ => Vec::new(),
                }
            }
            Msg::CloseDialog => {
                self.dialog = None;
                Vec::new()
            }
        }
    }
}

/// Text of a list row: one line, cut.
pub fn preview(text: &str, max: usize) -> String {
    let line = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut out: String = line.chars().take(max).collect();
    if line.chars().count() > max {
        out.push('…');
    }
    out
}

/// Drafts of the account: `(rid, tmid, text)`, rooms the workspace knows,
/// sorted by room name then thread.
pub fn drafts(ws: &Workspace) -> Vec<(String, Option<String>, String)> {
    let mut v: Vec<(String, Option<String>, String)> = ws
        .drafts()
        .iter()
        .filter(|(_, text)| !text.trim().is_empty())
        .filter(|((rid, _), _)| ws.room(rid).is_some())
        .map(|((rid, tmid), text)| (rid.clone(), tmid.clone(), text.clone()))
        .collect();
    v.sort_by(|a, b| {
        let name = |rid: &str| ws.room(rid).map(|e| e.name().to_lowercase());
        (name(&a.0), &a.1).cmp(&(name(&b.0), &b.1))
    });
    v
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use rocketchat::models::Message;

    fn ws_with_scheduled() -> Workspace {
        let mut ws = Workspace::new();
        ws.schedule_message(
            Message {
                id: "s1".into(),
                rid: "R1".into(),
                msg: "hello later".into(),
                ..Default::default()
            },
            5_000,
        );
        ws
    }

    fn ctx(ws: &Workspace) -> UpdateCtx<'_> {
        UpdateCtx {
            workspace: ws,
            now: chrono::Local::now(),
            morning: 9,
        }
    }

    #[test]
    fn places_drop_bidi_controls() {
        use rocketchat::models::{Room, RoomType, Subscription};
        let mut ws = Workspace::new();
        ws.apply_subscription(
            rustket_core::workspace::Change::Inserted,
            Subscription {
                id: "s-d1".into(),
                rid: "D1".into(),
                name: "mallory".into(),
                fname: Some("\u{202E}fdp.eciovni".into()),
                t: RoomType::Direct,
                open: true,
                ..Default::default()
            },
        );
        ws.apply_room(
            rustket_core::workspace::Change::Inserted,
            Room {
                id: "D1".into(),
                t: RoomType::Direct,
                ..Default::default()
            },
        );
        let prefs = rustket_core::config::UiPrefs {
            use_real_names: Some(true),
            ..Default::default()
        };
        let p = view::place(&ws, &prefs, "D1", None);
        assert!(!p.contains('\u{202E}'), "{p:?}");
    }

    #[test]
    fn dialogs_turn_into_requests() {
        let ws = ws_with_scheduled();
        let mut v = DraftsView::new();
        assert_eq!(
            v.update(Msg::SendNow("s1".into()), &ctx(&ws)),
            [Out::SendNow("s1".into())]
        );
        // Delete asks first.
        assert!(v.update(Msg::Delete("s1".into()), &ctx(&ws)).is_empty());
        assert!(v.has_dialog());
        assert_eq!(
            v.update(Msg::ConfirmDelete, &ctx(&ws)),
            [Out::Delete("s1".into())]
        );
        assert!(!v.has_dialog());
        // Unknown ids open nothing.
        v.update(Msg::Edit("nope".into()), &ctx(&ws));
        assert!(!v.has_dialog());
        // Editing: an emptied text is not saved.
        v.update(Msg::Edit("s1".into()), &ctx(&ws));
        v.update(Msg::EditAction(text_editor::Action::SelectAll), &ctx(&ws));
        v.update(
            Msg::EditAction(text_editor::Action::Edit(text_editor::Edit::Backspace)),
            &ctx(&ws),
        );
        assert!(v.update(Msg::SaveEdit, &ctx(&ws)).is_empty());
        v.update(
            Msg::EditAction(text_editor::Action::Edit(text_editor::Edit::Insert('x'))),
            &ctx(&ws),
        );
        assert_eq!(
            v.update(Msg::SaveEdit, &ctx(&ws)),
            [Out::SetText("s1".into(), "x".into())]
        );
        // Rescheduling with a preset.
        v.update(Msg::Reschedule("s1".into()), &ctx(&ws));
        let outs = v.update(
            Msg::Picker(when::Msg::Preset(rustket_core::reminders::Preset::In1Hour)),
            &ctx(&ws),
        );
        assert!(matches!(outs.as_slice(), [Out::Reschedule(id, _)] if id == "s1"));
        assert!(!v.has_dialog());
    }

    #[test]
    fn previews_are_one_cut_line() {
        assert_eq!(preview("a\n\n b   c", 10), "a b c");
        assert_eq!(preview("abcdef", 3), "abc…");
    }
}
