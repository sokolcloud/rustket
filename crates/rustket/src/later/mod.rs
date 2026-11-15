//! "Later": starred messages as a to-do list and message reminders
//! (`rustket_core::later`, `rustket_core::reminders`), a view shown in place
//! of the room area (`workspace::MainView::Later`). Strings:
//! `locales/later.yml`.
//!
//! The component holds what the open view shows (tab, selection, dialog)
//! and turns user input into [`Out`] requests; the app wiring
//! (`app/later.rs`) applies edits through the synced keys, unstars through
//! the paced star queue and opens messages. Rows come from the workspace,
//! so live updates, E2E decryption and renames reach them like any
//! timeline.

pub mod notify;
pub mod view;
pub mod when;

use std::cell::RefCell;
use std::sync::Arc;

use rustket_core::Workspace;
use rustket_core::later::{LaterEdit, LaterStatus};
use rustket_core::reminders::ReminderEdit;
use rustket_core::workspace::Dirty;
use rustket_core::workspace::{LaterRow, LaterTab};

use crate::room::Effect;
use crate::room::timeline::{Viewer, ViewerMsg};
use when::{Kind, Picked, WhenPicker};

/// Per account, kept while the view is closed: the paced fetch of
/// reminder copies (`chat.getMessage`).
#[derive(Debug, Default)]
pub struct LaterState {
    /// The session the state belongs to (its pointer).
    pub session: Option<usize>,
    /// Messages asked for this session (found or not).
    pub tried: std::collections::HashSet<String>,
    pub budget_used: u32,
    pub last_fetch: Option<std::time::Instant>,
    pub in_flight: bool,
    pub timer: Option<std::time::Instant>,
    /// The tab the view showed last (it opens there again).
    pub last_tab: Option<LaterTab>,
}

/// A key of the Later view (uncaptured, no modifiers; Enter comes through
/// the catch-up key).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LaterKey {
    Up,
    Down,
    Enter,
    /// Toggle done.
    E,
}

impl LaterKey {
    /// Up, Down and E (Enter is `crate::inbox::CatchUpKey::Enter`). E is
    /// the key's Latin letter, so it works on other layouts too.
    pub fn of(
        key: &iced::keyboard::Key,
        physical: iced::keyboard::key::Physical,
        m: iced::keyboard::Modifiers,
    ) -> Option<Self> {
        use iced::keyboard::Key;
        use iced::keyboard::key::Named;
        if !m.is_empty() {
            return None;
        }
        match key {
            Key::Named(Named::ArrowUp) => Some(Self::Up),
            Key::Named(Named::ArrowDown) => Some(Self::Down),
            Key::Character(_) if crate::keymap::is_char(key, physical, 'e') => Some(Self::E),
            _ => None,
        }
    }
}

/// A dialog of the view.
#[derive(Debug, Clone, PartialEq)]
pub enum Dialog {
    Remind {
        mid: String,
        rid: String,
        tmid: Option<String>,
        picker: WhenPicker,
    },
    Due {
        mid: String,
        rid: String,
        picker: WhenPicker,
        also_remind: bool,
    },
    /// "Remove done items from Later?" for these `(rid, mid)`.
    ClearDone(Vec<(String, String)>),
}

#[derive(Debug, Clone)]
pub enum Msg {
    Tab(LaterTab),
    Close,
    Select(String),
    Key(LaterKey),
    Open(String),
    SetStatus(String, LaterStatus),
    Remind(String),
    Due(String),
    RemoveReminder(String),
    /// Snooze a reminder by this many minutes.
    Snooze(String, i64),
    /// Remove from Later (unstar).
    Unstar(String),
    ClearDone,
    ConfirmClearDone,
    CloseDialog,
    Picker(when::Msg),
    AlsoRemind(bool),
    /// Scan every room for starred messages again.
    Refresh,
    /// Show the next [`PAGE`] rows of the tab.
    ShowMore,
    /// A message body's interaction (the shared viewer).
    View {
        rid: String,
        m: ViewerMsg,
    },
    /// The user info beside the view (a name clicked in a body).
    User(crate::room::panel::user_info::HostedMsg),
}

/// Requests to the app.
#[derive(Debug, Clone)]
pub enum Out {
    Close,
    /// Open message `mid` of room `rid` (its thread for a reply).
    Open {
        rid: String,
        tmid: Option<String>,
        mid: String,
    },
    Later(LaterEdit),
    Reminder(ReminderEdit),
    /// Unstar these `(rid, mid)` (paced) and forget their Later entries.
    Unstar(Vec<(String, String)>),
    Refresh,
    /// Scroll the list so that the row of this message shows (keyboard
    /// selection, [`view::reveal`]).
    Scroll(String),
    /// Effects of a message body (links, images, user info).
    Effects {
        rid: String,
        effects: Vec<Effect>,
    },
}

/// What [`LaterView::update`] needs besides the view.
pub struct UpdateCtx<'a> {
    pub workspace: &'a Workspace,
    pub now: chrono::DateTime<chrono::Local>,
    pub morning: u32,
    /// The session is connected (unstarring needs it).
    pub online: bool,
}

/// Rows shown at first and added by "Show more" (like the timeline's
/// window: a tab can hold thousands of archived stars).
pub const PAGE: usize = 50;

/// What the rows depend on: the tab and the workspace areas they read.
type RowsKey = (LaterTab, u64, u64, u64);

/// The rows and counts of the last workspace state seen, so that a view
/// rebuilds them once per change instead of once per frame and lookup.
#[derive(Debug, Default)]
struct RowsCache {
    rows: Option<(RowsKey, Arc<[LaterRow]>)>,
    counts: Option<((u64, u64, u64), [usize; 4])>,
}

fn revs(ws: &Workspace) -> (u64, u64, u64) {
    (
        ws.revision(Dirty::LATER),
        ws.revision(Dirty::REMINDERS),
        ws.revision(Dirty::ROOMS),
    )
}

/// The open Later view.
#[derive(Debug)]
pub struct LaterView {
    pub tab: LaterTab,
    pub selected: Option<String>,
    pub dialog: Option<Dialog>,
    pub viewer: Viewer,
    /// The room of the image open in the viewer's lightbox.
    pub image_rid: Option<String>,
    /// Rows of the tab shown (a multiple of [`PAGE`]).
    pub shown: usize,
    /// User info beside the view.
    pub user: Option<crate::room::panel::user_info::Hosted>,
    cache: RefCell<RowsCache>,
}

impl LaterView {
    pub fn new(tab: LaterTab) -> Self {
        Self {
            tab,
            selected: None,
            dialog: None,
            viewer: Viewer::new(),
            image_rid: None,
            shown: PAGE,
            user: None,
            cache: RefCell::default(),
        }
    }

    /// A dialog of the view is open (Esc closes it first).
    pub fn has_dialog(&self) -> bool {
        self.dialog.is_some() || self.viewer.image_open()
    }

    /// The rows of the open tab (built once per workspace change).
    pub fn rows(&self, ws: &Workspace) -> Arc<[LaterRow]> {
        let (l, r, rooms) = revs(ws);
        let key = (self.tab, l, r, rooms);
        let mut cache = self.cache.borrow_mut();
        match &cache.rows {
            Some((k, rows)) if *k == key => rows.clone(),
            _ => {
                let rows: Arc<[LaterRow]> = ws.later_rows(self.tab).into();
                cache.rows = Some((key, rows.clone()));
                rows
            }
        }
    }

    /// Rows per tab (once per workspace change).
    pub fn counts(&self, ws: &Workspace) -> [usize; 4] {
        let key = revs(ws);
        let mut cache = self.cache.borrow_mut();
        match cache.counts {
            Some((k, counts)) if k == key => counts,
            _ => {
                let counts = ws.later_counts();
                cache.counts = Some((key, counts));
                counts
            }
        }
    }

    /// The row of `mid` (whichever tab lists it).
    fn find(&self, ws: &Workspace, mid: &str) -> Option<LaterRow> {
        ws.later_row_of(mid)
    }

    /// Shows rows up to index `i` (a keyboard selection below the window).
    fn show_through(&mut self, i: usize) {
        if i >= self.shown {
            self.shown = (i / PAGE + 1) * PAGE;
        }
    }

    fn open(row: &LaterRow) -> Out {
        Out::Open {
            rid: row.rid.clone(),
            tmid: row.tmid.clone(),
            mid: row.mid.clone(),
        }
    }

    fn status_edit(row: &LaterRow, status: LaterStatus) -> Out {
        Out::Later(LaterEdit::Status {
            rid: row.rid.clone(),
            mid: row.mid.clone(),
            ts_ms: row.ts_ms,
            status,
        })
    }

    pub fn update(&mut self, msg: Msg, c: &UpdateCtx<'_>) -> Vec<Out> {
        let ws = c.workspace;
        match msg {
            Msg::Tab(t) => {
                self.tab = t;
                self.selected = None;
                self.shown = PAGE;
                Vec::new()
            }
            Msg::ShowMore => {
                self.shown = self.shown.saturating_add(PAGE);
                Vec::new()
            }
            Msg::Close => vec![Out::Close],
            Msg::Select(mid) => {
                self.selected = Some(mid);
                Vec::new()
            }
            Msg::Key(k) => self.key(k, c),
            Msg::Open(mid) => self
                .find(ws, &mid)
                .map(|r| Self::open(&r))
                .into_iter()
                .collect(),
            Msg::SetStatus(mid, status) => self
                .find(ws, &mid)
                .filter(|r| r.starred)
                .map(|r| Self::status_edit(&r, status))
                .into_iter()
                .collect(),
            Msg::Remind(mid) => {
                if let Some(r) = self.find(ws, &mid) {
                    self.dialog = Some(Dialog::Remind {
                        picker: WhenPicker::new(Kind::Reminder, r.reminder_ms),
                        mid: r.mid,
                        rid: r.rid,
                        tmid: r.tmid,
                    });
                }
                Vec::new()
            }
            Msg::Due(mid) => {
                if let Some(r) = self.find(ws, &mid).filter(|r| r.starred) {
                    self.dialog = Some(Dialog::Due {
                        picker: WhenPicker::new(Kind::Due, r.due_ms),
                        mid: r.mid,
                        rid: r.rid,
                        also_remind: false,
                    });
                }
                Vec::new()
            }
            Msg::RemoveReminder(mid) => vec![Out::Reminder(ReminderEdit::Clear { mid })],
            Msg::Snooze(mid, minutes) => {
                let Some(r) = ws.reminders().get(&mid) else {
                    return Vec::new();
                };
                vec![Out::Reminder(ReminderEdit::Set {
                    rid: r.rid.clone(),
                    tmid: r.tmid.clone(),
                    mid,
                    at_ms: c.now.timestamp_millis() + minutes * 60_000,
                })]
            }
            Msg::Unstar(mid) => {
                if !c.online {
                    return Vec::new();
                }
                self.find(ws, &mid)
                    .filter(|r| r.starred)
                    .map(|r| Out::Unstar(vec![(r.rid, r.mid)]))
                    .into_iter()
                    .collect()
            }
            Msg::ClearDone => {
                let items: Vec<(String, String)> = ws
                    .later_rows(LaterTab::Done)
                    .into_iter()
                    .map(|r| (r.rid, r.mid))
                    .collect();
                if !items.is_empty() && c.online {
                    self.dialog = Some(Dialog::ClearDone(items));
                }
                Vec::new()
            }
            Msg::ConfirmClearDone => match self.dialog.take() {
                Some(Dialog::ClearDone(items)) if c.online => vec![Out::Unstar(items)],
                other => {
                    self.dialog = other;
                    Vec::new()
                }
            },
            Msg::CloseDialog => {
                self.dialog = None;
                Vec::new()
            }
            Msg::AlsoRemind(on) => {
                if let Some(Dialog::Due { also_remind, .. }) = &mut self.dialog {
                    *also_remind = on;
                }
                Vec::new()
            }
            Msg::Picker(m) => self.picked(m, c),
            Msg::Refresh => vec![Out::Refresh],
            // Routed by the app (they need the room context).
            Msg::View { .. } | Msg::User(_) => Vec::new(),
        }
    }

    fn picked(&mut self, m: when::Msg, c: &UpdateCtx<'_>) -> Vec<Out> {
        let picked = match &mut self.dialog {
            Some(Dialog::Remind { picker, .. } | Dialog::Due { picker, .. }) => {
                picker.update(m, c.now, c.morning)
            }
            _ => None,
        };
        let Some(picked) = picked else {
            return Vec::new();
        };
        match self.dialog.take() {
            Some(Dialog::Remind { mid, rid, tmid, .. }) => vec![Out::Reminder(match picked {
                Picked::At(at_ms) => ReminderEdit::Set {
                    rid,
                    tmid,
                    mid,
                    at_ms,
                },
                Picked::Clear => ReminderEdit::Clear { mid },
            })],
            Some(Dialog::Due {
                mid,
                rid,
                also_remind,
                ..
            }) => {
                let at_ms = match picked {
                    Picked::At(at) => Some(at),
                    Picked::Clear => None,
                };
                let mut outs = vec![Out::Later(LaterEdit::Due {
                    rid: rid.clone(),
                    mid: mid.clone(),
                    at_ms,
                })];
                if let Some(at_ms) =
                    at_ms.filter(|at| also_remind && *at > c.now.timestamp_millis())
                {
                    let tmid = c.workspace.later_message(&mid).and_then(|m| m.tmid.clone());
                    outs.push(Out::Reminder(ReminderEdit::Set {
                        rid,
                        tmid,
                        mid,
                        at_ms,
                    }));
                }
                outs
            }
            other => {
                self.dialog = other;
                Vec::new()
            }
        }
    }

    fn key(&mut self, k: LaterKey, c: &UpdateCtx<'_>) -> Vec<Out> {
        if self.dialog.is_some() {
            return Vec::new();
        }
        let rows = self.rows(c.workspace);
        let pos = self
            .selected
            .as_ref()
            .and_then(|s| rows.iter().position(|r| r.mid == *s));
        match k {
            LaterKey::Up | LaterKey::Down => {
                if rows.is_empty() {
                    return Vec::new();
                }
                let next = match (pos, k) {
                    // The last row rendered, not the tab's last: a long
                    // tab stays windowed.
                    (None, LaterKey::Up) => rows.len().min(self.shown).saturating_sub(1),
                    (None, _) => 0,
                    (Some(p), LaterKey::Up) => p.saturating_sub(1),
                    (Some(p), _) => (p + 1).min(rows.len() - 1),
                };
                let mid = rows[next].mid.clone();
                self.selected = Some(mid.clone());
                self.show_through(next);
                vec![Out::Scroll(mid)]
            }
            LaterKey::Enter => pos.map(|p| Self::open(&rows[p])).into_iter().collect(),
            LaterKey::E => {
                let Some(r) = pos.map(|p| &rows[p]).filter(|r| r.starred) else {
                    return Vec::new();
                };
                let next = if r.status == LaterStatus::Done {
                    LaterStatus::InProgress
                } else {
                    LaterStatus::Done
                };
                vec![Self::status_edit(r, next)]
            }
        }
    }

    /// Forgets state of rows that are gone.
    pub fn prune(&mut self, ws: &Workspace) {
        if let Some(sel) = &self.selected
            && self.find(ws, sel).is_none()
        {
            self.selected = None;
        }
        let gone = match &self.dialog {
            Some(Dialog::Remind { mid, .. } | Dialog::Due { mid, .. }) => {
                ws.later_message(mid).is_none() && ws.reminders().get(mid).is_none()
            }
            _ => false,
        };
        if gone {
            self.dialog = None;
        }
        if self
            .user
            .as_ref()
            .is_some_and(|u| ws.room(&u.rid).is_none())
        {
            self.user = None;
        }
        if !self.viewer.image_open() {
            self.image_rid = None;
        } else if self
            .image_rid
            .as_deref()
            .is_none_or(|rid| ws.room(rid).is_none())
        {
            self.viewer.close_image();
            self.image_rid = None;
        }
    }
}

#[cfg(test)]
mod tests;
