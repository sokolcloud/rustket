//! Message composer: multi-line editor (Enter sends, Shift+Enter breaks the
//! line), formatting toolbar, `@`/`#`/`:`/`/` autocomplete, emoji picker,
//! edit/quote modes, slash commands, uploads, drafts and typing notifications.
//! Messages go through the workspace outbox (`new_local_message` /
//! `add_pending`, failed sends can be retried or discarded).
//! Strings: `locales/room_composer.yml` (`room_composer.*`).

pub mod autocomplete;
pub mod text;

use std::collections::HashMap;
use std::ops::Range;
use std::path::PathBuf;
use std::sync::{Arc, LazyLock, Mutex};
use std::time::{Duration, Instant};

use iced::keyboard::{self, key};
use iced::widget::text_editor::{self, Binding, KeyPress};
use iced::widget::{button, column, container, row, space, text as label, text_input};
use iced::{Alignment, Background, Border, Color, Element, Length, Subscription, Task, Theme};
use rocketchat::models::{OutgoingMessage, RoomSummary, RoomType, SlashCommand, UserSummary};
use rocketchat::rest::RoomMembersOptions;
use rust_i18n::t;
use rustket_core::permissions::{self, PostBlock};
use rustket_core::typing::{TypingAction, TypingThrottle, USER_TYPING};
use rustket_core::workspace::Changes;
use rustket_core::{ServerId, Workspace};
use rustket_ui::theme::{
    FONT_SEMIBOLD, SPACE_1, SPACE_2, SPACE_3, SPACE_4, TEXT_CAPTION, TEXT_SMALL,
};
use rustket_ui::widget::{
    BannerKind, Placement, TooltipPosition, above, danger_button, icon_button_with, modal, popover,
    primary_button, secondary_button, tooltip,
};
use rustket_ui::{Icon, icon, style};

use self::autocomplete::{Item, Popup};
use self::text::{Format, Trigger, TriggerKind};
use super::emoji_picker::{self, EmojiPicker};
use super::upload::{self, Uploads};
use super::{Action, ComposeIntent, Effect, UpdateCtx, ViewCtx};
use crate::error::Res;
use crate::session::SessionHandle;

/// A pending message with its delivery state.
type PendingItem = (
    rocketchat::models::Message,
    rustket_core::timeline::DeliveryState,
);

/// Delay before a changed text is stored as the draft.
const DRAFT_DELAY: Duration = Duration::from_millis(800);
/// Delay before an autocomplete query goes to the server.
const QUERY_DELAY: Duration = Duration::from_millis(250);
/// Room members are refetched after this long (also after a failure).
const MEMBERS_TTL: Duration = Duration::from_secs(300);
/// Room members fetched for `@` completion.
const MEMBERS_COUNT: u64 = 100;
/// The editor grows up to about ten lines, then scrolls.
const MAX_EDITOR_HEIGHT: f32 = 204.0;

/// At most this many rooms keep cached members.
const MEMBERS_MAX_ROOMS: usize = 64;
/// `commands.list` is refetched after this long.
const COMMANDS_TTL: Duration = Duration::from_secs(600);
/// After a failed `commands.list`, wait this long before asking again.
const COMMANDS_RETRY: Duration = Duration::from_secs(15);

type MemberCache = HashMap<(String, String), (Instant, Arc<Vec<UserSummary>>)>;

/// Room members per (server, room) for `@` completion.
static MEMBERS: LazyLock<Mutex<MemberCache>> = LazyLock::new(|| Mutex::new(HashMap::new()));

#[derive(Default)]
struct CommandCache {
    lists: HashMap<String, (Instant, Arc<Vec<SlashCommand>>)>,
    failed: HashMap<String, Instant>,
}

/// `commands.list` per server.
static COMMANDS: LazyLock<Mutex<CommandCache>> =
    LazyLock::new(|| Mutex::new(CommandCache::default()));

fn cached_members(server: &ServerId, rid: &str) -> Option<Arc<Vec<UserSummary>>> {
    let key = (server.as_str().to_owned(), rid.to_owned());
    let map = MEMBERS.lock().ok()?;
    map.get(&key)
        .filter(|(at, _)| at.elapsed() < MEMBERS_TTL)
        .map(|(_, m)| m.clone())
}

fn store_members(server: &ServerId, rid: &str, list: Vec<UserSummary>) {
    if let Ok(mut map) = MEMBERS.lock() {
        map.retain(|_, (at, _)| at.elapsed() < MEMBERS_TTL);
        if map.len() >= MEMBERS_MAX_ROOMS
            && let Some(oldest) = map
                .iter()
                .min_by_key(|(_, (at, _))| *at)
                .map(|(k, _)| k.clone())
        {
            map.remove(&oldest);
        }
        map.insert(
            (server.as_str().to_owned(), rid.to_owned()),
            (Instant::now(), Arc::new(list)),
        );
    }
}

fn cached_commands(server: &ServerId) -> Option<Arc<Vec<SlashCommand>>> {
    COMMANDS
        .lock()
        .ok()?
        .lists
        .get(server.as_str())
        .filter(|(at, _)| at.elapsed() < COMMANDS_TTL)
        .map(|(_, l)| l.clone())
}

/// Caches a non-empty list. The server answers an empty list while its
/// Apps-Engine is still loading: that counts as a failure.
fn store_commands(server: &ServerId, list: Vec<SlashCommand>) -> bool {
    let Ok(mut c) = COMMANDS.lock() else {
        return false;
    };
    if list.is_empty() {
        c.failed.insert(server.as_str().to_owned(), Instant::now());
        return false;
    }
    c.failed.remove(server.as_str());
    c.lists
        .insert(server.as_str().to_owned(), (Instant::now(), Arc::new(list)));
    true
}

fn commands_failed(server: &ServerId) {
    if let Ok(mut c) = COMMANDS.lock() {
        c.failed.insert(server.as_str().to_owned(), Instant::now());
    }
}

/// Drops the cached `commands.list` of `server` (apps added or removed
/// commands, `crate::uikit`).
pub fn forget_commands(server: &ServerId) {
    if let Ok(mut c) = COMMANDS.lock() {
        c.lists.remove(server.as_str());
        c.failed.remove(server.as_str());
    }
}

/// `commands.list` failed a moment ago: do not ask again yet.
fn commands_recently_failed(server: &ServerId) -> bool {
    COMMANDS.lock().ok().is_some_and(|c| {
        c.failed
            .get(server.as_str())
            .is_some_and(|at| at.elapsed() < COMMANDS_RETRY)
    })
}

/// Outbox sends by (server, message id). Results are recorded here as soon as
/// the request finishes, so a result that arrives while the room is closed
/// (its message is then dropped) is applied when the room opens again.
enum Outcome {
    InFlight,
    Done(Res<Box<rocketchat::models::Message>>),
}

struct OutboxEntry {
    rid: String,
    outcome: Outcome,
}

static OUTBOX: LazyLock<Mutex<HashMap<(String, String), OutboxEntry>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

fn outbox_start(server: &ServerId, rid: &str, id: &str) {
    if let Ok(mut map) = OUTBOX.lock() {
        map.insert(
            (server.as_str().to_owned(), id.to_owned()),
            OutboxEntry {
                rid: rid.to_owned(),
                outcome: Outcome::InFlight,
            },
        );
    }
}

fn outbox_finish(server: &str, id: &str, r: &Res<Box<rocketchat::models::Message>>) {
    if let Ok(mut map) = OUTBOX.lock()
        && let Some(e) = map.get_mut(&(server.to_owned(), id.to_owned()))
    {
        e.outcome = Outcome::Done(r.clone());
    }
}

fn outbox_forget(server: &ServerId, id: &str) {
    if let Ok(mut map) = OUTBOX.lock() {
        map.remove(&(server.as_str().to_owned(), id.to_owned()));
    }
}

fn outbox_in_flight(server: &ServerId, id: &str) -> bool {
    OUTBOX.lock().ok().is_some_and(|m| {
        m.get(&(server.as_str().to_owned(), id.to_owned()))
            .is_some_and(|e| matches!(e.outcome, Outcome::InFlight))
    })
}

/// Takes the finished sends of room `rid`.
fn outbox_take_done(
    server: &ServerId,
    rid: &str,
) -> Vec<(String, Res<Box<rocketchat::models::Message>>)> {
    let Ok(mut map) = OUTBOX.lock() else {
        return Vec::new();
    };
    let keys: Vec<(String, String)> = map
        .iter()
        .filter(|((s, _), e)| {
            s == server.as_str() && e.rid == rid && matches!(e.outcome, Outcome::Done(_))
        })
        .map(|(k, _)| k.clone())
        .collect();
    keys.into_iter()
        .filter_map(|k| match map.remove(&k)?.outcome {
            Outcome::Done(r) => Some((k.1, r)),
            Outcome::InFlight => None,
        })
        .collect()
}

#[derive(Debug, Clone)]
pub enum Message {
    Editor(text_editor::Action),
    /// Enter: send the message (or save the edit).
    Send,
    Format(Format),
    /// Up in an empty composer: edit the last own message.
    EditLast,
    /// Escape: close the popup, else cancel edit, else drop quotes.
    Cancel,
    CancelEdit,
    RemoveQuote(String),
    AcMove(i32),
    AcConfirm,
    AcSelect(usize),
    AcClose,
    AcDebounce(u64),
    AcUsers(u64, Res<Vec<UserSummary>>),
    AcRooms(u64, Res<Vec<RoomSummary>>),
    MembersLoaded(Res<Vec<UserSummary>>),
    /// `commands.list` result, with the text to send once it is known.
    CommandsLoaded(Res<Vec<SlashCommand>>, Option<String>),
    /// A message fetched for an edit or quote intent.
    Fetched(FetchFor, Res<Box<rocketchat::models::Message>>),
    Sent(String, Res<Box<rocketchat::models::Message>>),
    Updated(Res<Box<rocketchat::models::Message>>),
    /// The composer's app menu (`crate::uikit::buttons`).
    UiKit(Box<crate::uikit::UiKitMsg>),
    Typed(Res<()>),
    TypingTick,
    DraftTick(u64),
    TogglePicker,
    Picker(emoji_picker::Message),
    Reacted(Res<()>),
    Upload(upload::Message),
    /// Ctrl+V: look for files or an image on the clipboard first.
    PasteCheck,
    /// No file or image on the clipboard: paste its text.
    PasteText,
    Pasted(Option<String>),
    /// Files dropped onto the window.
    FilesDropped(Vec<PathBuf>),
    Join,
    JoinCode(String),
    Joined(Res<()>),
    /// The subscription fetched right after joining.
    JoinedSub(Res<Box<rocketchat::models::Subscription>>),
    /// Enter on an emptied edit: confirm before deleting the message.
    DeleteConfirmed,
    DeleteCancelled,
    Deleted(Res<()>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FetchFor {
    Edit,
    Quote,
}

#[derive(Debug, Clone, PartialEq)]
struct Editing {
    mid: String,
    original: String,
    /// The text that was in the composer before the edit started.
    stash: String,
    /// The message as shown when the edit started (encrypted edits need it).
    message: Box<rocketchat::models::Message>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Quote {
    mid: String,
    author: String,
    excerpt: String,
}

/// What the emoji picker does with the chosen emoji.
#[derive(Debug, Clone, PartialEq, Eq)]
enum PickTarget {
    Insert,
    React(String),
}

pub struct Composer {
    rid: String,
    tmid: Option<String>,
    content: text_editor::Content,
    editor_id: iced::widget::Id,
    editing: Option<Editing>,
    quotes: Vec<Quote>,
    typing: TypingThrottle,
    draft_stamp: u64,
    popup: Option<Popup>,
    ac_seq: u64,
    members_loading: bool,
    commands_loading: bool,
    picker: Option<(EmojiPicker, PickTarget)>,
    uploads: Uploads,
    joining: bool,
    /// Code for rooms that require one to join.
    join_code: String,
    /// Message waiting for the delete confirmation (emptied edit).
    confirm_delete: Option<String>,
    /// Thread composer: replies are also sent to the channel (`tshow`).
    tshow: bool,
}

impl Composer {
    pub fn new(rid: &str) -> Self {
        Self::build(rid, None)
    }

    /// A composer for replies in thread `tmid` of room `rid`.
    pub fn for_thread(rid: &str, tmid: &str) -> Self {
        Self::build(rid, Some(tmid.to_owned()))
    }

    fn build(rid: &str, tmid: Option<String>) -> Self {
        let editor_id =
            iced::widget::Id::from(format!("composer-{rid}-{}", tmid.as_deref().unwrap_or("")));
        Self {
            rid: rid.to_owned(),
            tmid,
            content: text_editor::Content::new(),
            editor_id,
            editing: None,
            quotes: Vec::new(),
            typing: TypingThrottle::new(),
            draft_stamp: 0,
            popup: None,
            ac_seq: 0,
            members_loading: false,
            commands_loading: false,
            picker: None,
            uploads: Uploads::default(),
            joining: false,
            join_code: String::new(),
            confirm_delete: None,
            tshow: false,
        }
    }

    /// Thread composer: send replies to the channel too ("Also send to
    /// channel", `tshow`).
    pub fn set_tshow(&mut self, on: bool) {
        self.tshow = on;
    }

    pub fn text(&self) -> String {
        self.content.text()
    }

    pub fn is_editing(&self) -> bool {
        self.editing.is_some()
    }

    pub fn editing_mid(&self) -> Option<&str> {
        self.editing.as_ref().map(|e| e.mid.as_str())
    }

    pub fn quoted(&self) -> Vec<&str> {
        self.quotes.iter().map(|q| q.mid.as_str()).collect()
    }

    pub fn popup_items(&self) -> &[Item] {
        self.popup.as_ref().map_or(&[], |p| p.items.as_slice())
    }

    pub fn popup_selected(&self) -> Option<usize> {
        self.popup.as_ref().map(|p| p.selected)
    }

    pub fn is_picker_open(&self) -> bool {
        self.picker.is_some()
    }

    pub fn is_upload_dialog_open(&self) -> bool {
        self.uploads.is_dialog_open()
    }

    /// Focuses the editor.
    pub fn focus(&self) -> Task<Message> {
        iced::widget::operation::focus(self.editor_id.clone())
    }

    /// Restores the draft when the room (or thread) opens, settles outbox
    /// sends that finished while it was closed, and focuses the editor.
    pub fn on_open(&mut self, ctx: &mut UpdateCtx<'_>) -> Action<Message> {
        self.settle_outbox(ctx);
        if self.content.is_empty()
            && let Some(d) = ctx.workspace.draft(&self.rid, self.tmid.as_deref())
        {
            let d = d.to_owned();
            let end = d.len();
            self.set_text(&d, end..end);
        }
        Action::task(self.focus())
    }

    /// Stores the current text as the draft right away (room closing).
    pub fn on_close(&self, ws: &mut Workspace) {
        let text = match &self.editing {
            Some(e) => e.stash.clone(),
            None => self.content.text(),
        };
        ws.set_draft(&self.rid, self.tmid.as_deref(), &text);
    }

    /// The room (or thread) is going away: stop the typing indicator right
    /// away instead of leaving it to the server's expiry.
    pub fn leave(&mut self, session: Option<&SessionHandle>) -> Task<Message> {
        let Some(TypingAction::Stop) = self.typing.on_leave() else {
            return Task::none();
        };
        let Some(session) = session.cloned() else {
            return Task::none();
        };
        let rid = self.rid.clone();
        let tmid = self.tmid.clone();
        Task::perform(
            async move {
                session
                    .session()
                    .set_user_activity(&rid, tmid.as_deref(), &[])
                    .await
                    .map_err(crate::error::Failure::from)
            },
            Message::Typed,
        )
    }

    /// Applies send results that arrived while the room was closed; a message
    /// still marked as sending without a request in flight (lost result, or
    /// an earlier run of the app) becomes failed so it can be retried.
    fn settle_outbox(&mut self, ctx: &mut UpdateCtx<'_>) {
        for (id, r) in outbox_take_done(ctx.server_id, &self.rid) {
            self.apply_sent(&id, r, ctx.workspace);
        }
        let stuck: Vec<(String, Option<String>)> = ctx
            .workspace
            .pending_messages(&self.rid)
            .into_iter()
            .filter(|i| i.message.tmid == self.tmid)
            .filter(|i| matches!(i.state, rustket_core::timeline::DeliveryState::Sending))
            .filter(|i| !outbox_in_flight(ctx.server_id, &i.message.id))
            .map(|i| (i.message.id.clone(), i.message.tmid.clone()))
            .collect();
        let error = t!("room_composer.not_delivered").into_owned();
        for (id, tmid) in stuck {
            ctx.workspace
                .mark_pending_failed(&self.rid, tmid.as_deref(), &id, &error);
        }
    }

    fn apply_sent(&self, id: &str, r: Res<Box<rocketchat::models::Message>>, ws: &mut Workspace) {
        let tmid = self.pending_tmid(ws, id);
        apply_send_result(ws, &self.rid, tmid.as_deref(), id, r);
    }

    // --- text helpers -----------------------------------------------------------

    /// The text (`\n` line breaks), the selection (sorted) and the cursor offset.
    fn snapshot(&self) -> (String, Range<usize>, usize) {
        let cursor = self.content.cursor();
        let mut text = String::new();
        let mut starts = Vec::new();
        for (i, line) in self.content.lines().enumerate() {
            if i > 0 {
                text.push('\n');
            }
            starts.push((text.len(), line.text.len()));
            text.push_str(&line.text);
        }
        let offset = |p: text_editor::Position| -> usize {
            starts
                .get(p.line)
                .map_or(text.len(), |(s, len)| s + p.column.min(*len))
        };
        let pos = offset(cursor.position);
        let anchor = cursor.selection.map_or(pos, offset);
        if anchor == pos
            && let Some(selected) = self.content.selection()
            && let Some(range) = text::selection_around(&text, &selected, pos)
        {
            // Double/triple click: a word/line selection has no anchor.
            return (text, range, pos);
        }
        (text, pos.min(anchor)..pos.max(anchor), pos)
    }

    /// Replaces the text; `sel.end` becomes the cursor, `sel.start` the anchor.
    fn set_text(&mut self, text: &str, sel: Range<usize>) {
        self.content = text_editor::Content::with_text(text);
        let to_pos = |off: usize| {
            let before = &text[..off.min(text.len())];
            let line = before.matches('\n').count();
            let column = before
                .rfind('\n')
                .map_or(before.len(), |i| before.len() - i - 1);
            text_editor::Position { line, column }
        };
        self.content.move_to(text_editor::Cursor {
            position: to_pos(sel.end),
            selection: (sel.start != sel.end).then(|| to_pos(sel.start)),
        });
    }

    fn apply_edit(&mut self, e: text::Edited) {
        self.set_text(&e.text, e.selection);
    }

    fn popup_visible(&self) -> bool {
        self.popup.as_ref().is_some_and(|p| !p.items.is_empty())
    }

    // --- update -------------------------------------------------------------------

    pub fn update(&mut self, msg: Message, ctx: &mut UpdateCtx<'_>) -> Action<Message> {
        match msg {
            Message::Editor(action) => {
                let edit = action.is_edit();
                self.content.perform(action);
                if edit {
                    Action::task(self.after_edit(ctx))
                } else {
                    Action::task(self.refresh_popup(ctx))
                }
            }
            Message::Send => self.send(ctx),
            Message::Format(f) => {
                let (text, sel, _) = self.snapshot();
                self.apply_edit(text::apply_format(&text, sel, f));
                Action::task(Task::batch([self.after_edit(ctx), self.focus()]))
            }
            Message::EditLast => {
                if !self.content.text().trim().is_empty() || self.editing.is_some() {
                    return Action::none();
                }
                match last_editable(ctx.workspace, &self.rid, self.tmid.as_deref()) {
                    Some(m) => {
                        let m = m.clone();
                        self.start_edit(&m, ctx.workspace)
                    }
                    None => Action::none(),
                }
            }
            Message::Cancel => {
                if self.confirm_delete.is_some() {
                    self.confirm_delete = None;
                } else if self.popup_visible() {
                    self.popup = None;
                } else if self.editing.is_some() {
                    self.cancel_edit();
                } else if !self.quotes.is_empty() {
                    self.quotes.clear();
                } else {
                    self.picker = None;
                }
                Action::none()
            }
            Message::CancelEdit => {
                self.cancel_edit();
                Action::task(self.focus())
            }
            Message::RemoveQuote(mid) => {
                self.quotes.retain(|q| q.mid != mid);
                Action::none()
            }
            Message::AcMove(d) => {
                if let Some(p) = &mut self.popup {
                    p.move_selection(d);
                }
                Action::none()
            }
            Message::AcConfirm => match self.popup.as_ref().map(|p| p.selected) {
                Some(i) => self.select_item(i, ctx),
                None => Action::none(),
            },
            Message::AcSelect(i) => self.select_item(i, ctx),
            Message::AcClose => {
                self.popup = None;
                Action::none()
            }
            Message::AcDebounce(seq) => {
                let Some(p) = self.popup.as_ref().filter(|p| p.seq == seq) else {
                    return Action::none();
                };
                let q = p.trigger.query.clone();
                Action::task(match p.trigger.kind {
                    TriggerKind::User => ctx.rest_task(
                        move |rest| async move { rest.users_autocomplete(&q).await },
                        move |r| Message::AcUsers(seq, r),
                    ),
                    TriggerKind::Room => {
                        ctx.rest_task(
                            move |rest| async move {
                                rest.rooms_autocomplete_channel_and_private(&q).await
                            },
                            move |r| Message::AcRooms(seq, r),
                        )
                    }
                    _ => Task::none(),
                })
            }
            Message::AcUsers(seq, r) => {
                match r {
                    Ok(list) => {
                        let remote = autocomplete::remote_users(ctx.workspace, list);
                        self.merge_remote(seq, remote, ctx);
                    }
                    Err(e) => tracing::debug!(error = %e, "users.autocomplete failed"),
                }
                Action::none()
            }
            Message::AcRooms(seq, r) => {
                match r {
                    Ok(list) => self.merge_remote(seq, autocomplete::remote_rooms(list), ctx),
                    Err(e) => tracing::debug!(error = %e, "rooms.autocomplete failed"),
                }
                Action::none()
            }
            Message::MembersLoaded(r) => {
                self.members_loading = false;
                match r {
                    Ok(list) => {
                        store_members(ctx.server_id, &self.rid, list);
                        self.recompute_local(ctx);
                    }
                    Err(e) => {
                        // Remembered as empty so every keystroke does not retry.
                        tracing::debug!(error = %e, "room members failed");
                        store_members(ctx.server_id, &self.rid, Vec::new());
                    }
                }
                Action::none()
            }
            Message::CommandsLoaded(r, pending) => {
                self.commands_loading = false;
                let loaded = match r {
                    Ok(list) => store_commands(ctx.server_id, list),
                    Err(e) => {
                        tracing::debug!(error = %e, "commands.list failed");
                        commands_failed(ctx.server_id);
                        false
                    }
                };
                self.recompute_local(ctx);
                // The text stayed in the editor while the list loaded: send it
                // now unless the user changed it meanwhile.
                let Some(text) = pending else {
                    return Action::none();
                };
                if self.editing.is_some() || self.content.text().trim() != text {
                    return Action::none();
                }
                if loaded {
                    self.send(ctx)
                } else {
                    Action::effect(commands_unavailable())
                }
            }
            Message::Fetched(purpose, r) => match r {
                Ok(m) => match purpose {
                    FetchFor::Edit => {
                        // Decrypted like every shown message before it is edited.
                        let mut m = m;
                        crate::e2e::shown::process_list(
                            ctx.workspace,
                            ctx.server,
                            std::slice::from_mut(&mut m),
                        );
                        self.start_edit(&m, ctx.workspace)
                    }
                    FetchFor::Quote => {
                        self.add_quote(&m);
                        Action::task(self.focus())
                    }
                },
                Err(e) => Action::effect(error_toast(e.user_text())),
            },
            Message::Sent(id, r) => {
                outbox_forget(ctx.server_id, &id);
                self.apply_sent(&id, r, ctx.workspace);
                Action::none()
            }
            Message::Updated(r) => match r {
                Ok(m) => {
                    ctx.workspace.apply_message(*m);
                    Action::none()
                }
                Err(e) => Action::effect(Effect::Toast(
                    BannerKind::Error,
                    t!("room_composer.edit_failed").into_owned(),
                    e.user_text(),
                )),
            },
            Message::UiKit(m) => Action::effect(Effect::UiKit(m)),
            Message::Typed(r) => {
                if let Err(e) = r {
                    tracing::debug!(error = %e, "user-activity failed");
                }
                Action::none()
            }
            Message::TypingTick => {
                let act = self.typing.tick(Instant::now());
                Action::task(self.typing_task(act, ctx))
            }
            Message::DraftTick(stamp) => {
                if stamp == self.draft_stamp && self.editing.is_none() {
                    let text = self.content.text();
                    ctx.workspace
                        .set_draft(&self.rid, self.tmid.as_deref(), &text);
                }
                Action::none()
            }
            Message::TogglePicker => {
                if self.picker.is_some() {
                    self.picker = None;
                    return Action::task(self.focus());
                }
                self.open_picker(PickTarget::Insert, ctx)
            }
            Message::Picker(m) => {
                let Some((picker, target)) = &mut self.picker else {
                    return Action::none();
                };
                match picker.update(m) {
                    None => Action::none(),
                    Some(emoji_picker::Event::Close) => {
                        self.picker = None;
                        Action::task(self.focus())
                    }
                    Some(emoji_picker::Event::Picked(key)) => {
                        let target = target.clone();
                        self.picker = None;
                        let used = Effect::RecentEmoji(key.clone());
                        match target {
                            PickTarget::Insert => {
                                let (text, sel, _) = self.snapshot();
                                self.apply_edit(text::insert_word(&text, sel, &key));
                                Action::task(Task::batch([self.after_edit(ctx), self.focus()]))
                                    .with(used)
                            }
                            PickTarget::React(mid) => Action::task(ctx.rest_task(
                                move |rest| async move { rest.react(&mid, &key, None).await },
                                Message::Reacted,
                            ))
                            .with(used),
                        }
                    }
                }
            }
            Message::Reacted(r) => match r {
                Ok(()) => Action::none(),
                Err(e) => Action::effect(Effect::Toast(
                    BannerKind::Error,
                    t!("room_composer.react_failed").into_owned(),
                    e.user_text(),
                )),
            },
            Message::Upload(m) => {
                let tmid = self.tmid.clone();
                self.uploads
                    .update(m, ctx, tmid.as_deref())
                    .map(Message::Upload)
            }
            Message::PasteCheck => {
                if permissions::can_upload(ctx.workspace, &self.rid) {
                    // Files or an image are uploaded instead of pasting text.
                    Action::task(upload::clipboard_contents().map(|m| match m {
                        upload::Message::ClipboardImage(None) => Message::PasteText,
                        other => Message::Upload(other),
                    }))
                } else {
                    Action::task(paste_text())
                }
            }
            Message::PasteText => Action::task(paste_text()),
            Message::Pasted(Some(s)) if !s.is_empty() => {
                self.content
                    .perform(text_editor::Action::Edit(text_editor::Edit::Paste(
                        Arc::new(s),
                    )));
                Action::task(self.after_edit(ctx))
            }
            Message::Pasted(_) => Action::none(),
            Message::FilesDropped(paths) => {
                if !permissions::can_upload(ctx.workspace, &self.rid) {
                    return Action::effect(error_toast(
                        t!("room_composer.upload.disabled").into_owned(),
                    ));
                }
                let tmid = self.tmid.clone();
                self.uploads
                    .update(upload::Message::Paths(paths), ctx, tmid.as_deref())
                    .map(Message::Upload)
            }
            Message::Join => {
                if self.joining {
                    return Action::none();
                }
                let rid = self.rid.clone();
                let code = if needs_join_code(ctx.workspace, &self.rid) {
                    let code = self.join_code.trim().to_owned();
                    if code.is_empty() {
                        return Action::none();
                    }
                    Some(code)
                } else {
                    None
                };
                // `rooms.join` also covers discussions of private groups,
                // like the web client.
                let task =
                    ctx.rest_task(
                        move |rest| async move {
                            rest.rooms_join(&rid, code.as_deref()).await.map(|_| ())
                        },
                        Message::Joined,
                    );
                self.joining = true;
                Action::task(task)
            }
            Message::JoinCode(code) => {
                self.join_code = code;
                Action::none()
            }
            Message::Joined(r) => {
                self.joining = false;
                match r {
                    Ok(_) => {
                        self.join_code.clear();
                        // The sidebar entry and the normal composer need the
                        // subscription; do not wait for the stream.
                        let rid = self.rid.clone();
                        let sub = ctx.rest_task(
                            move |rest| async move { rest.subscriptions_get_one(&rid).await },
                            |r| Message::JoinedSub(r.map(Box::new)),
                        );
                        Action::task(Task::batch([sub, self.focus()]))
                    }
                    Err(e) => Action::effect(Effect::Toast(
                        BannerKind::Error,
                        t!("room_composer.join_failed").into_owned(),
                        e.user_text(),
                    )),
                }
            }
            Message::JoinedSub(r) => {
                match r {
                    Ok(sub) => ctx
                        .workspace
                        .apply_subscription(rustket_core::Change::Updated, *sub),
                    Err(e) => tracing::debug!(error = %e, "subscription after join"),
                }
                Action::none()
            }
            Message::DeleteConfirmed => {
                let Some(mid) = self.confirm_delete.take() else {
                    return Action::none();
                };
                if self.editing.as_ref().is_some_and(|e| e.mid == mid) {
                    self.cancel_edit();
                }
                let rid = self.rid.clone();
                Action::task(Task::batch([
                    ctx.rest_task(
                        move |rest| async move { rest.delete_message(&rid, &mid).await },
                        Message::Deleted,
                    ),
                    self.focus(),
                ]))
            }
            Message::DeleteCancelled => {
                self.confirm_delete = None;
                Action::task(self.focus())
            }
            Message::Deleted(r) => match r {
                Ok(()) => Action::none(),
                Err(e) => Action::effect(Effect::Toast(
                    BannerKind::Error,
                    t!("room_composer.delete_failed").into_owned(),
                    e.user_text(),
                )),
            },
        }
    }

    /// A request from another room component.
    pub fn intent(&mut self, intent: ComposeIntent, ctx: &mut UpdateCtx<'_>) -> Action<Message> {
        match intent {
            ComposeIntent::InsertText(s) => {
                let (text, sel, _) = self.snapshot();
                self.apply_edit(text::insert(&text, sel, &s));
                Action::task(Task::batch([self.after_edit(ctx), self.focus()]))
            }
            ComposeIntent::Mention(u) => {
                let (text, sel, _) = self.snapshot();
                let u = u.trim_start_matches('@');
                self.apply_edit(text::insert_word(&text, sel, &format!("@{u}")));
                Action::task(Task::batch([self.after_edit(ctx), self.focus()]))
            }
            ComposeIntent::Edit(mid) => {
                match find_message(ctx.workspace, &self.rid, self.tmid.as_deref(), &mid) {
                    Some(m) => {
                        let m = m.clone();
                        self.start_edit(&m, ctx.workspace)
                    }
                    None => Action::task(fetch(ctx, mid, FetchFor::Edit)),
                }
            }
            ComposeIntent::Quote(mid) => {
                if self.quotes.iter().any(|q| q.mid == mid) {
                    return Action::task(self.focus());
                }
                match find_message(ctx.workspace, &self.rid, self.tmid.as_deref(), &mid) {
                    Some(m) => {
                        let m = m.clone();
                        self.add_quote(&m);
                        Action::task(self.focus())
                    }
                    None => Action::task(fetch(ctx, mid, FetchFor::Quote)),
                }
            }
            ComposeIntent::React(mid) => {
                if !permissions::can_react(ctx.workspace, &self.rid) {
                    return Action::none();
                }
                self.open_picker(PickTarget::React(mid), ctx)
            }
            // The thread panel owns its own composer (`Composer::for_thread`).
            ComposeIntent::ReplyInThread(_) => Action::none(),
            // Any pending message of the room (thread replies included).
            ComposeIntent::Retry(mid) => self.retry(&mid, ctx),
            ComposeIntent::Discard(mid) => self.discard(&mid, ctx),
        }
    }

    pub fn on_changes(&mut self, changes: &Changes, ctx: &mut UpdateCtx<'_>) -> Action<Message> {
        let _ = (changes, ctx);
        Action::none()
    }

    pub fn subscription(&self) -> Subscription<Message> {
        if self.typing.is_active() {
            iced::time::every(Duration::from_secs(1)).map(|_| Message::TypingTick)
        } else {
            Subscription::none()
        }
    }

    // --- flows --------------------------------------------------------------------

    /// Typing notification, draft debounce and autocomplete after a text change.
    fn after_edit(&mut self, ctx: &mut UpdateCtx<'_>) -> Task<Message> {
        let mut tasks = Vec::new();
        if self.editing.is_none() {
            let act = if self.content.text().trim().is_empty() {
                self.typing.on_cleared()
            } else {
                self.typing.on_input(Instant::now())
            };
            tasks.push(self.typing_task(act, ctx));
            self.draft_stamp += 1;
            let stamp = self.draft_stamp;
            tasks.push(Task::perform(
                async { tokio::time::sleep(DRAFT_DELAY).await },
                move |_| Message::DraftTick(stamp),
            ));
        }
        tasks.push(self.refresh_popup(ctx));
        Task::batch(tasks)
    }

    fn typing_task(&self, act: Option<TypingAction>, ctx: &UpdateCtx<'_>) -> Task<Message> {
        let Some(act) = act else {
            return Task::none();
        };
        if ctx.session.is_none() {
            return Task::none();
        }
        let rid = self.rid.clone();
        let tmid = self.tmid.clone();
        ctx.session_task(
            move |s| async move {
                let acts: &[&str] = match act {
                    TypingAction::Start => &[USER_TYPING],
                    TypingAction::Stop => &[],
                };
                s.session()
                    .set_user_activity(&rid, tmid.as_deref(), acts)
                    .await
            },
            Message::Typed,
        )
    }

    /// Opens, updates or closes the autocomplete popup for the cursor position.
    fn refresh_popup(&mut self, ctx: &mut UpdateCtx<'_>) -> Task<Message> {
        let (text, sel, cursor) = self.snapshot();
        let trigger = if sel.is_empty() {
            text::trigger_at(&text, cursor)
        } else {
            None
        };
        let Some(trigger) = trigger.filter(|t| {
            t.kind != TriggerKind::Command
                || !crate::e2e::outgoing::slash_commands_blocked(ctx.workspace, &self.rid)
        }) else {
            self.popup = None;
            return Task::none();
        };
        if self.popup.as_ref().is_some_and(|p| p.trigger == trigger) {
            return Task::none();
        }
        self.ac_seq += 1;
        let seq = self.ac_seq;
        let mut popup = Popup::new(trigger.clone(), seq);
        if let Some(old) = &self.popup
            && old.trigger.kind == trigger.kind
            && old.trigger.start == trigger.start
        {
            popup.selected = 0;
        }
        popup.set_items(self.local_items(&trigger, ctx.workspace, ctx.server_id));
        self.popup = Some(popup);
        let mut tasks = Vec::new();
        match trigger.kind {
            TriggerKind::User
                if cached_members(ctx.server_id, &self.rid).is_none() && !self.members_loading =>
            {
                self.members_loading = true;
                let rid = self.rid.clone();
                let room_type = ctx.workspace.room(&self.rid).map(|e| e.room_type());
                tasks.push(ctx.rest_task(
                    move |rest| async move {
                        // `rooms.membersOrderedByRole` refuses DMs.
                        if room_type == Some(RoomType::Direct) {
                            return rest
                                .room_members_by_type(&rid, RoomType::Direct, 0, MEMBERS_COUNT)
                                .await
                                .map(|p| p.items);
                        }
                        let opts = RoomMembersOptions {
                            count: Some(MEMBERS_COUNT),
                            ..Default::default()
                        };
                        rest.room_members(&rid, &opts).await.map(|p| p.items)
                    },
                    Message::MembersLoaded,
                ));
            }
            TriggerKind::Command
                if ctx.session.is_some()
                    && cached_commands(ctx.server_id).is_none()
                    && !self.commands_loading
                    && !commands_recently_failed(ctx.server_id) =>
            {
                self.commands_loading = true;
                tasks.push(load_commands(ctx, None));
            }
            _ => {}
        }
        if autocomplete::wants_remote(trigger.kind) && ctx.session.is_some() {
            tasks.push(Task::perform(
                async { tokio::time::sleep(QUERY_DELAY).await },
                move |_| Message::AcDebounce(seq),
            ));
        }
        Task::batch(tasks)
    }

    fn local_items(&self, trigger: &Trigger, ws: &Workspace, server: &ServerId) -> Vec<Item> {
        match trigger.kind {
            TriggerKind::User => {
                let members = cached_members(server, &self.rid).unwrap_or_default();
                autocomplete::match_users(ws, &self.rid, &members, &trigger.query)
            }
            TriggerKind::Room => autocomplete::match_rooms(ws, &trigger.query),
            TriggerKind::Emoji => autocomplete::match_emoji(ws, &trigger.query),
            TriggerKind::Command => {
                let cmds = cached_commands(server).unwrap_or_default();
                autocomplete::match_commands(ws, &self.rid, &cmds, &trigger.query)
            }
            TriggerKind::Canned => {
                crate::omnichannel::canned::suggestions(server, ws, &self.rid, &trigger.query)
                    .into_iter()
                    .map(|(shortcut, text)| Item::Canned { shortcut, text })
                    .collect()
            }
        }
    }

    fn recompute_local(&mut self, ctx: &UpdateCtx<'_>) {
        let Some(trigger) = self.popup.as_ref().map(|p| p.trigger.clone()) else {
            return;
        };
        let local = self.local_items(&trigger, ctx.workspace, ctx.server_id);
        if let Some(p) = &mut self.popup {
            p.set_items(local);
        }
    }

    /// Server results for query `seq`; results of an older query are dropped.
    fn merge_remote(&mut self, seq: u64, remote: Vec<Item>, ctx: &UpdateCtx<'_>) {
        if self.popup.as_ref().is_none_or(|p| p.seq != seq) {
            return;
        }
        if let Some(p) = &mut self.popup {
            p.remote = remote;
        }
        self.recompute_local(ctx);
    }

    fn select_item(&mut self, i: usize, ctx: &mut UpdateCtx<'_>) -> Action<Message> {
        let Some(popup) = self.popup.take() else {
            return Action::none();
        };
        let Some(item) = popup.items.get(i) else {
            return Action::none();
        };
        let (text, _, _) = self.snapshot();
        self.apply_edit(text::complete(&text, &popup.trigger, &item.replacement()));
        Action::task(Task::batch([self.after_edit(ctx), self.focus()]))
    }

    fn start_edit(&mut self, m: &rocketchat::models::Message, ws: &Workspace) -> Action<Message> {
        if !permissions::can_edit_message(ws, m, chrono::Utc::now()) {
            return Action::effect(error_toast(t!("room_composer.cannot_edit").into_owned()));
        }
        if let Err(e) = crate::e2e::room::check_edit(ws, m) {
            return Action::effect(error_toast(e));
        }
        let stash = match self.editing.take() {
            Some(e) => e.stash,
            None => self.content.text(),
        };
        self.editing = Some(Editing {
            mid: m.id.clone(),
            original: m.msg.clone(),
            stash,
            message: Box::new(m.clone()),
        });
        self.popup = None;
        self.picker = None;
        let end = m.msg.len();
        self.set_text(&m.msg.clone(), end..end);
        Action::task(self.focus())
    }

    fn cancel_edit(&mut self) {
        if let Some(e) = self.editing.take() {
            let end = e.stash.len();
            self.set_text(&e.stash, end..end);
        }
    }

    fn add_quote(&mut self, m: &rocketchat::models::Message) {
        if self.quotes.iter().any(|q| q.mid == m.id) {
            return;
        }
        let author =
            m.u.username
                .clone()
                .or_else(|| m.u.name.clone())
                .unwrap_or_default();
        let excerpt: String = m
            .msg
            .lines()
            .next()
            .unwrap_or_default()
            .chars()
            .take(80)
            .collect();
        self.quotes.push(Quote {
            mid: m.id.clone(),
            author,
            excerpt,
        });
    }

    fn open_picker(&mut self, target: PickTarget, ctx: &mut UpdateCtx<'_>) -> Action<Message> {
        let picker = EmojiPicker::new();
        picker.want_images(ctx.workspace, ctx.server_id, ctx.server, ctx.images);
        self.picker = Some((picker, target));
        self.popup = None;
        Action::task(EmojiPicker::focus())
    }

    /// Enter: saves an edit, runs a slash command or sends the message.
    fn send(&mut self, ctx: &mut UpdateCtx<'_>) -> Action<Message> {
        let raw = self.content.text();
        let text = text::close_code_block(raw.trim());
        let max = ctx.workspace.settings().max_message_size();
        if let Some(ed) = self.editing.clone() {
            if text.is_empty() {
                // Like the web client: an emptied edit asks to delete the message.
                self.confirm_delete = Some(ed.mid);
                return Action::none();
            }
            if text::too_long(&text, max) {
                return Action::effect(too_long(max));
            }
            if text == ed.original.trim() {
                self.cancel_edit();
                return Action::task(self.focus());
            }
            let rid = self.rid.clone();
            // Encrypted messages are edited encrypted (never with plaintext);
            // on failure the edit stays open with the text.
            let enc =
                match crate::e2e::outgoing::prepare_edit(ctx.workspace, &rid, &ed.message, &text) {
                    Some(Err(e)) => return Action::effect(error_toast(e)),
                    Some(Ok(enc)) => Some(enc),
                    None => None,
                };
            self.cancel_edit();
            return Action::task(Task::batch([
                ctx.rest_task(
                    move |rest| crate::e2e::outgoing::update_message(rest, rid, ed.mid, text, enc),
                    |r| Message::Updated(r.map(Box::new)),
                ),
                self.focus(),
            ]));
        }
        // Quotes need some text, as in the web client.
        if text.is_empty() {
            return Action::none();
        }
        // An encrypted room without its key: nothing goes out (§7.6).
        if matches!(
            crate::e2e::outgoing::mode(ctx.workspace, &self.rid),
            crate::e2e::outgoing::Mode::Blocked
        ) {
            return Action::effect(error_toast(crate::e2e::outgoing::blocked_text(
                ctx.workspace,
                &self.rid,
            )));
        }
        // Slash commands do not run in rooms that must stay encrypted (§2.1).
        let is_command = self.quotes.is_empty()
            && text::parse_slash(&text).is_some()
            && !crate::e2e::outgoing::slash_commands_blocked(ctx.workspace, &self.rid);
        if is_command
            && let Some((name, _)) = text::parse_slash(&text)
            && let Some(list) = cached_commands(ctx.server_id)
            && !list.iter().any(|c| c.command == name)
            && !ctx
                .workspace
                .settings()
                .bool_or("Message_AllowUnrecognizedSlashCommand", false)
        {
            // Like the web: an unknown command is not posted (it may carry
            // private parameters); the text stays for a fix.
            return Action::effect(Effect::Toast(
                BannerKind::Warning,
                t!("room_composer.no_such_command", command = name).into_owned(),
                String::new(),
            ));
        }
        if is_command && cached_commands(ctx.server_id).is_none() {
            // The text stays in the editor until the command list is known.
            if self.commands_loading {
                return Action::none();
            }
            if ctx.session.is_none() || commands_recently_failed(ctx.server_id) {
                return Action::effect(commands_unavailable());
            }
            self.commands_loading = true;
            return Action::task(load_commands(ctx, Some(text)));
        }
        let permalinks: Vec<String> = self
            .quotes
            .iter()
            .map(|q| self.permalink(ctx.workspace, ctx.server.as_str(), &q.mid))
            .collect();
        let full = if permalinks.is_empty() {
            text.clone()
        } else {
            text::with_quotes(&text, &permalinks)
        };
        if text::too_long(&full, max) {
            return Action::effect(too_long(max));
        }
        self.quotes.clear();
        self.content = text_editor::Content::new();
        self.popup = None;
        self.draft_stamp += 1;
        ctx.workspace.take_draft(&self.rid, self.tmid.as_deref());
        let stop = self.typing.on_send();
        let typing = self.typing_task(stop, ctx);
        let action = if is_command {
            self.dispatch_text(full, ctx)
        } else {
            Action::task(self.send_text(full, ctx))
        };
        Action {
            task: Task::batch([action.task, typing]),
            effects: action.effects,
        }
    }

    /// Runs `/command` when the server knows it, otherwise sends the text.
    fn dispatch_text(&mut self, text: String, ctx: &mut UpdateCtx<'_>) -> Action<Message> {
        if let Some((name, params)) = text::parse_slash(&text)
            && cached_commands(ctx.server_id).is_some_and(|c| c.iter().any(|c| c.command == name))
        {
            return Action::effect(self.run_command(name, params, ctx));
        }
        Action::task(self.send_text(text, ctx))
    }

    /// `commands.run`, run by the UIKit controller at app level (§7) so its
    /// result does not depend on this view staying open.
    fn run_command(&self, name: String, params: String, ctx: &UpdateCtx<'_>) -> Effect {
        let app_id = cached_commands(ctx.server_id).and_then(|c| {
            c.iter()
                .find(|c| c.command == name)
                .and_then(|c| c.app_id.clone())
        });
        Effect::UiKit(Box::new(crate::uikit::commands::request(
            &self.rid,
            self.tmid.as_deref(),
            name,
            params,
            app_id,
        )))
    }

    /// Adds a pending message to the outbox and sends it with `chat.sendMessage`.
    fn send_text(&mut self, text: String, ctx: &mut UpdateCtx<'_>) -> Task<Message> {
        let m = ctx
            .workspace
            .new_local_message(&self.rid, &text, self.tmid.as_deref(), self.tshow);
        ctx.workspace.add_pending(m.clone());
        send_task(ctx, m)
    }

    /// Pending message `id` this composer owns: the thread composer only
    /// replies of its thread, the room composer any message of the room (a
    /// thread reply shown in the room while its thread is closed).
    fn owned_pending(&self, ws: &Workspace, id: &str) -> Option<PendingItem> {
        ws.pending_messages(&self.rid)
            .into_iter()
            .find(|i| i.message.id == id)
            .filter(|i| self.tmid.is_none() || i.message.tmid == self.tmid)
            .map(|i| (i.message.clone(), i.state.clone()))
    }

    fn retry(&mut self, id: &str, ctx: &mut UpdateCtx<'_>) -> Action<Message> {
        let Some((m, rustket_core::timeline::DeliveryState::Failed { .. })) =
            self.owned_pending(ctx.workspace, id)
        else {
            return Action::none();
        };
        ctx.workspace
            .mark_pending_sending(&self.rid, m.tmid.as_deref(), id);
        Action::task(send_task(ctx, m))
    }

    fn pending_tmid(&self, ws: &Workspace, id: &str) -> Option<String> {
        ws.pending_messages(&self.rid)
            .into_iter()
            .find(|i| i.message.id == id)
            .and_then(|i| i.message.tmid.clone())
            .or_else(|| self.tmid.clone())
    }

    /// Drops a pending (failed) message (see [`Self::owned_pending`]).
    fn discard(&mut self, id: &str, ctx: &mut UpdateCtx<'_>) -> Action<Message> {
        if let Some((m, _)) = self.owned_pending(ctx.workspace, id) {
            ctx.workspace
                .discard_pending(&self.rid, m.tmid.as_deref(), id);
        }
        Action::none()
    }

    fn permalink(&self, ws: &Workspace, server: &str, mid: &str) -> String {
        let site = ws
            .settings()
            .str("Site_Url")
            .filter(|s| !s.trim().is_empty())
            .unwrap_or(server);
        let (t, name) = ws
            .room(&self.rid)
            .map(|e| (e.room_type(), e.name().to_owned()))
            .unwrap_or((RoomType::Channel, String::new()));
        text::permalink(site, t, &name, &self.rid, mid)
    }

    // --- view ---------------------------------------------------------------------

    pub fn view<'a>(&'a self, ctx: &ViewCtx<'a>) -> Element<'a, Message> {
        if let Some(block) = permissions::post_block(ctx.workspace, &self.rid) {
            return self.notice_view(block, ctx);
        }
        if let Some(n) = crate::e2e::room::composer_block(ctx.workspace, &self.rid) {
            return container(n).padding([SPACE_2, SPACE_4]).into();
        }
        // Bars above the editor, in one slot so the editor keeps its tree
        // position (and focus) when they come and go.
        let mut bars: Vec<Element<'a, Message>> = Vec::new();
        if let Some(h) = crate::e2e::room::composer_hint(ctx.workspace, &self.rid) {
            bars.push(h);
        }
        let slash = self
            .content
            .line(0)
            .is_some_and(|l| l.text.starts_with('/'));
        if let Some(h) = crate::e2e::room::slash_hint(ctx.workspace, &self.rid, slash) {
            bars.push(h);
        }
        if let Some(l) = ctx
            .workspace
            .typing_label(&self.rid, self.tmid.as_deref(), ctx.now)
        {
            bars.push(
                label(crate::workspace::sidebar::typing_text(&l))
                    .size(TEXT_CAPTION)
                    .style(style::text_secondary)
                    .into(),
            );
        }
        if let Some(t) =
            self.uploads
                .transfers_view(ctx.server_id.as_str(), &self.rid, self.tmid.as_deref())
        {
            bars.push(t.map(Message::Upload));
        }
        if let Some(e) = &self.editing {
            bars.push(bar(
                Icon::Pencil,
                t!("room_composer.editing").into_owned(),
                e.original.lines().next().unwrap_or_default().to_owned(),
                Message::CancelEdit,
            ));
        }
        for q in &self.quotes {
            bars.push(bar(
                Icon::Quote,
                t!("room_composer.quoting", user = q.author.as_str()).into_owned(),
                q.excerpt.clone(),
                Message::RemoveQuote(q.mid.clone()),
            ));
        }
        let frame = container(column![self.editor_view(ctx), self.toolbar_view(ctx)])
            .width(Length::Fill)
            .style(style::container_composer(false));
        let popup: Element<'a, Message> = match &self.popup {
            Some(p) if !p.items.is_empty() => autocomplete::view(p, ctx, Message::AcSelect),
            _ => space().into(),
        };
        let framed = popover(
            frame,
            popup,
            self.popup_visible(),
            Placement::AboveStart,
            Message::AcClose,
        );
        let bars = (!bars.is_empty()).then(|| column(bars).spacing(SPACE_1).into());
        // The emoji picker opens above the whole composer frame.
        let col = above(bars, SPACE_1, self.with_picker(framed, ctx));
        let body = container(col)
            .padding([SPACE_2, SPACE_4])
            .width(Length::Fill);
        if self.confirm_delete.is_some() {
            return modal(
                body,
                Some(self.delete_dialog()),
                Some(Message::DeleteCancelled),
            );
        }
        let dialog = self.uploads.dialog_view().map(|d| d.map(Message::Upload));
        modal(body, dialog, Some(Message::Upload(upload::Message::Close)))
    }

    /// `base` with the emoji picker popover above it (while open).
    fn with_picker<'a>(
        &'a self,
        base: impl Into<Element<'a, Message>>,
        ctx: &ViewCtx<'a>,
    ) -> Element<'a, Message> {
        let picker_view: Element<'a, Message> = match &self.picker {
            Some((p, target)) => {
                let title = match target {
                    PickTarget::React(_) => Some(t!("room_composer.add_reaction").into_owned()),
                    PickTarget::Insert => None,
                };
                p.view(ctx.workspace, ctx.server_id, ctx.server, ctx.images, title)
                    .map(Message::Picker)
            }
            None => space().into(),
        };
        popover(
            base,
            picker_view,
            self.picker.is_some(),
            Placement::AboveStart,
            Message::Picker(emoji_picker::Message::Close),
        )
    }

    fn delete_dialog<'a>(&'a self) -> Element<'a, Message> {
        rustket_ui::widget::dialog(
            t!("room_composer.delete.title").into_owned(),
            label(t!("room_composer.delete.body")).size(TEXT_SMALL),
            vec![
                secondary_button(t!("room_composer.cancel"), Some(Message::DeleteCancelled)).into(),
                danger_button(
                    t!("room_composer.delete.confirm"),
                    Some(Message::DeleteConfirmed),
                )
                .into(),
            ],
            Some(Message::DeleteCancelled),
        )
    }

    fn placeholder(&self, ws: &Workspace) -> String {
        if self.tmid.is_some() {
            return t!("room_composer.placeholder_thread").into_owned();
        }
        match ws.room(&self.rid) {
            Some(e) => {
                let name = e.fname().unwrap_or(e.name());
                let room = match e.room_type() {
                    RoomType::Direct => format!("@{name}"),
                    _ => format!("#{name}"),
                };
                t!("room_composer.placeholder_room", room = room).into_owned()
            }
            None => t!("room_composer.placeholder").into_owned(),
        }
    }

    fn editor_view<'a>(&'a self, ctx: &ViewCtx<'a>) -> Element<'a, Message> {
        let keys = Keys {
            popup: self.popup_visible(),
            empty: self.content.is_empty(),
            cancellable: self.editing.is_some() || !self.quotes.is_empty(),
        };
        iced::widget::text_editor(&self.content)
            .id(self.editor_id.clone())
            .placeholder(self.placeholder(ctx.workspace))
            .on_action(Message::Editor)
            .key_binding(move |kp| key_binding(kp, keys))
            .padding([10, 12])
            .size(rustket_ui::theme::TEXT_BODY)
            .max_height(MAX_EDITOR_HEIGHT)
            .style(editor_style)
            .into()
    }

    /// The app actions menu (`messageBoxAction` buttons), if there are any.
    fn apps_menu<'a>(&'a self, ctx: &ViewCtx<'a>) -> Option<Element<'a, Message>> {
        use crate::uikit::{UiKitMsg, buttons};
        let ws = ctx.workspace;
        if !buttons::composer_has_buttons(ws, &self.rid) {
            return None;
        }
        let tmid = self.tmid.as_deref();
        let open = buttons::composer_menu_open(ws, &self.rid, tmid);
        let key = buttons::composer_menu_key(&self.rid, tmid);
        let wrap = |m: UiKitMsg| Message::UiKit(Box::new(m));
        let entries = if open {
            buttons::composer_entries(ws, &self.rid, tmid, &self.content.text(), wrap)
        } else {
            Vec::new()
        };
        let trigger = icon_button_with(
            buttons::COMPOSER_ICON,
            Some(wrap(UiKitMsg::Popover((!open).then_some(key)))),
            t!("uikit.apps"),
            open,
            30.0,
        );
        Some(rustket_ui::widget::popover_menu(
            trigger,
            &entries,
            open,
            wrap(UiKitMsg::Popover(None)),
            Placement::AboveEnd,
        ))
    }

    fn toolbar_view<'a>(&'a self, ctx: &ViewCtx<'a>) -> Element<'a, Message> {
        let emoji_btn = icon_button_with(
            Icon::Smile,
            Some(Message::TogglePicker),
            t!("room_composer.emoji"),
            self.picker.is_some(),
            30.0,
        );
        let mut bar = row![emoji_btn].spacing(SPACE_1).align_y(Alignment::Center);
        bar = bar.push(
            container(space())
                .width(Length::Fixed(1.0))
                .height(18)
                .style(style::container_divider),
        );
        for f in Format::ALL {
            let (ic, tip) = format_icon(f);
            bar = bar.push(icon_button_with(
                ic,
                Some(Message::Format(f)),
                tip,
                false,
                28.0,
            ));
        }
        bar = bar.push(space().width(Length::Fill));
        if let Some(apps) = self.apps_menu(ctx) {
            bar = bar.push(apps);
        }
        if permissions::can_upload(ctx.workspace, &self.rid) {
            // Files cannot be added to an edited message.
            bar = bar.push(icon_button_with(
                Icon::Paperclip,
                self.editing
                    .is_none()
                    .then_some(Message::Upload(upload::Message::Attach)),
                t!("room_composer.attach"),
                false,
                30.0,
            ));
        }
        let can_send = !self.content.is_empty() || self.editing.is_some();
        let send_tip = if self.editing.is_some() {
            t!("room_composer.save")
        } else {
            t!("room_composer.send")
        };
        let send = button(
            container(
                icon(if self.editing.is_some() {
                    Icon::Check
                } else {
                    Icon::Send
                })
                .size(16.0),
            )
            .center(Length::Fill),
        )
        .width(Length::Fixed(32.0))
        .height(Length::Fixed(30.0))
        .padding(0)
        .style(style::button_primary)
        .on_press_maybe(can_send.then_some(Message::Send));
        bar = bar.push(tooltip(send, send_tip, TooltipPosition::Top));
        container(bar).padding([SPACE_1, SPACE_2]).into()
    }

    fn notice_view<'a>(&'a self, block: PostBlock, ctx: &ViewCtx<'a>) -> Element<'a, Message> {
        let channel = can_join(ctx.workspace, &self.rid);
        let text = match block {
            // A previewed channel or discussion: the Join bar.
            PostBlock::NotSubscribed if channel => t!("room_composer.preview_notice"),
            PostBlock::NotSubscribed => t!("room_composer.block.not_subscribed"),
            PostBlock::Archived => t!("room_composer.block.archived"),
            PostBlock::Muted => t!("room_composer.block.muted"),
            PostBlock::ReadOnly => t!("room_composer.block.read_only"),
            PostBlock::Blocker => t!("room_composer.block.you_blocked"),
            PostBlock::Blocked => t!("room_composer.block.blocked"),
        };
        let ic = match block {
            PostBlock::Archived => Icon::Archive,
            PostBlock::NotSubscribed => Icon::Info,
            _ => Icon::Lock,
        };
        let mut r = row![
            icon(ic).size(16.0).style(style::text_secondary),
            label(text).size(TEXT_SMALL).style(style::text_secondary),
        ]
        .spacing(SPACE_2)
        .align_y(Alignment::Center);
        let joinable = block == PostBlock::NotSubscribed && channel;
        if joinable {
            r = r.push(space().width(Length::Fill));
            let with_code = needs_join_code(ctx.workspace, &self.rid);
            if with_code {
                r = r.push(
                    text_input(&t!("room_composer.join_code"), &self.join_code)
                        .on_input(Message::JoinCode)
                        .on_submit(Message::Join)
                        .secure(true)
                        .width(Length::Fixed(180.0))
                        .padding([6, 10])
                        .style(style::text_input),
                );
            }
            let ready = !self.joining && (!with_code || !self.join_code.trim().is_empty());
            r = r.push(
                primary_button(t!("room_composer.join"), ready.then_some(Message::Join))
                    .loading(self.joining),
            );
        }
        let card = container(r)
            .padding([SPACE_3, SPACE_4])
            .width(Length::Fill)
            .style(style::container_card);
        // Reactions stay possible in archived rooms and with reactWhenReadOnly.
        container(self.with_picker(card, ctx))
            .padding([SPACE_2, SPACE_4])
            .width(Length::Fill)
            .into()
    }
}

/// Edit/quote bar above the editor.
fn bar<'a>(ic: Icon, title: String, detail: String, on_close: Message) -> Element<'a, Message> {
    container(
        row![
            icon(ic).size(14.0).style(style::text_link),
            label(title).size(TEXT_CAPTION).font(FONT_SEMIBOLD),
            label(detail)
                .size(TEXT_CAPTION)
                .style(style::text_secondary)
                .width(Length::Fill)
                .wrapping(iced::widget::text::Wrapping::None),
            icon_button_with(
                Icon::X,
                Some(on_close),
                t!("room_composer.cancel"),
                false,
                22.0
            ),
        ]
        .spacing(SPACE_2)
        .align_y(Alignment::Center),
    )
    .padding([SPACE_1, SPACE_2])
    .style(style::container_quote)
    .into()
}

fn format_icon(f: Format) -> (Icon, String) {
    let (ic, key) = match f {
        Format::Bold => (Icon::Bold, t!("room_composer.format.bold")),
        Format::Italic => (Icon::Italic, t!("room_composer.format.italic")),
        Format::Strike => (Icon::Strikethrough, t!("room_composer.format.strike")),
        Format::Code => (Icon::Code, t!("room_composer.format.code")),
        Format::CodeBlock => (Icon::SquareCode, t!("room_composer.format.code_block")),
        Format::Link => (Icon::Link, t!("room_composer.format.link")),
        Format::Quote => (Icon::TextQuote, t!("room_composer.format.quote")),
    };
    (ic, key.into_owned())
}

fn editor_style(theme: &Theme, status: text_editor::Status) -> text_editor::Style {
    let t = rustket_ui::tokens(theme);
    text_editor::Style {
        background: Background::Color(Color::TRANSPARENT),
        border: Border::default(),
        placeholder: t.text_hint,
        value: if status == text_editor::Status::Disabled {
            t.text_disabled
        } else {
            t.text_primary
        },
        selection: t.selection,
    }
}

fn error_toast(body: String) -> Effect {
    Effect::Toast(
        BannerKind::Error,
        t!("room_composer.error").into_owned(),
        body,
    )
}

fn too_long(max: usize) -> Effect {
    Effect::Toast(
        BannerKind::Error,
        t!("room_composer.too_long_title").into_owned(),
        t!("room_composer.too_long", max = max).into_owned(),
    )
}

fn commands_unavailable() -> Effect {
    Effect::Toast(
        BannerKind::Error,
        t!("room_composer.command_failed").into_owned(),
        t!("room_composer.commands_unavailable").into_owned(),
    )
}

fn paste_text() -> Task<Message> {
    iced::clipboard::read().map(Message::Pasted)
}

/// A previewed room the user may join: a channel, or a discussion of any
/// type (members of the parent may join it, like in the web client).
fn can_join(ws: &Workspace, rid: &str) -> bool {
    ws.room(rid)
        .is_some_and(|e| e.room_type() == RoomType::Channel || e.is_discussion())
}

/// Joining needs a code: the room requires one and the user may not skip it.
fn needs_join_code(ws: &Workspace, rid: &str) -> bool {
    ws.room(rid)
        .and_then(|e| e.room.as_ref())
        .is_some_and(|r| r.join_code_required == Some(true))
        && !permissions::has_permission(ws, "join-without-join-code", Some(rid))
}

fn load_commands(ctx: &UpdateCtx<'_>, pending: Option<String>) -> Task<Message> {
    ctx.rest_task(
        |rest| async move { rest.commands_list().await },
        move |r| Message::CommandsLoaded(r, pending),
    )
}

fn fetch(ctx: &UpdateCtx<'_>, mid: String, purpose: FetchFor) -> Task<Message> {
    ctx.rest_task(
        move |rest| async move { rest.get_message(&mid).await },
        move |r| Message::Fetched(purpose, r.map(Box::new)),
    )
}

/// Applies a `chat.sendMessage` result to the outbox message `id` (reply
/// in thread `tmid`).
fn apply_send_result(
    ws: &mut Workspace,
    rid: &str,
    tmid: Option<&str>,
    id: &str,
    r: Res<Box<rocketchat::models::Message>>,
) {
    match r {
        Ok(m) => ws.apply_message(*m),
        Err(e) => {
            tracing::warn!(error = %e, "chat.sendMessage failed");
            ws.mark_pending_failed(rid, tmid, id, &e.user_text());
        }
    }
}

/// Results for a composer that is gone (a thread view closed or replaced,
/// or the room closed, while a reply or an upload was running): the reply
/// settles in the workspace, the upload reports its outcome.
pub fn orphan_result(m: Message, server: &ServerId, rid: &str, ws: &mut Workspace) -> Vec<Effect> {
    match m {
        Message::Sent(id, r) => {
            outbox_forget(server, &id);
            let tmid = ws
                .pending_messages(rid)
                .into_iter()
                .find(|i| i.message.id == id)
                .and_then(|i| i.message.tmid.clone());
            apply_send_result(ws, rid, tmid.as_deref(), &id, r);
            Vec::new()
        }
        Message::Upload(u) => upload::orphan_result(u, ws).into_iter().collect(),
        _ => Vec::new(),
    }
}

/// Sends an outbox message. The result is recorded in `OUTBOX` when the
/// request finishes, even if the room was closed meanwhile.
fn send_task(ctx: &UpdateCtx<'_>, m: rocketchat::models::Message) -> Task<Message> {
    let id = m.id.clone();
    outbox_start(ctx.server_id, &m.rid, &id);
    let server = ctx.server_id.as_str().to_owned();
    // Encrypted rooms: encrypted at send time with the current room key.
    let enc = crate::e2e::outgoing::prepare_send(ctx.workspace, &m);
    let out = OutgoingMessage {
        id: Some(m.id),
        rid: m.rid,
        msg: m.msg,
        tmid: m.tmid,
        tshow: m.tshow,
        ..Default::default()
    };
    ctx.rest_task(
        move |rest| crate::e2e::outgoing::send_outgoing(rest, out, enc),
        move |r| {
            let r = r.map(Box::new);
            outbox_finish(&server, &id, &r);
            Message::Sent(id, r)
        },
    )
}

/// A message of the room or the thread by id.
fn find_message<'a>(
    ws: &'a Workspace,
    rid: &str,
    tmid: Option<&str>,
    mid: &str,
) -> Option<&'a rocketchat::models::Message> {
    let thread = tmid.and_then(|t| ws.thread(t));
    thread
        .and_then(|t| t.get(mid))
        .or_else(|| ws.timeline(rid).and_then(|t| t.get(mid)))
        .map(|i| &i.message)
        .or_else(|| thread.and_then(|t| t.parent()).filter(|p| p.id == mid))
}

/// The newest own message the user may edit (Up in an empty composer).
fn last_editable<'a>(
    ws: &'a Workspace,
    rid: &str,
    tmid: Option<&str>,
) -> Option<&'a rocketchat::models::Message> {
    let me = ws.user_id()?;
    let timeline = match tmid {
        Some(t) => ws.thread(t)?,
        None => ws.timeline(rid)?,
    };
    let now = chrono::Utc::now();
    use rustket_core::timeline::DeliveryState;
    timeline
        .iter()
        .rev()
        // A failed message (never delivered) cannot be edited and does not
        // block the older ones.
        .filter(|i| !matches!(i.state, DeliveryState::Failed { .. }))
        .filter(|i| i.message.u.id == me)
        // The room composer edits room messages, not thread replies shown in the channel.
        .filter(|i| tmid.is_some() || i.message.tmid.is_none())
        // A message still being sent stops the search: the user means the
        // one just sent, not an older one (editable once delivered).
        .find(|i| {
            i.state == DeliveryState::Sending || permissions::can_edit_message(ws, &i.message, now)
        })
        .filter(|i| i.state != DeliveryState::Sending)
        .map(|i| &i.message)
        // An encrypted message that cannot be edited stops the search (an
        // older message is not what the user meant).
        .filter(|m| crate::e2e::room::check_edit(ws, m).is_ok())
}

/// Editor state the key bindings depend on.
#[derive(Debug, Clone, Copy, Default)]
struct Keys {
    /// The autocomplete popup is visible.
    popup: bool,
    empty: bool,
    /// Escape has something to cancel (edit, quotes).
    cancellable: bool,
}

/// Composer keys on top of the default text editor bindings.
fn key_binding(kp: KeyPress, k: Keys) -> Option<Binding<Message>> {
    if !matches!(kp.status, text_editor::Status::Focused { .. }) {
        return None;
    }
    let m = kp.modifiers;
    if let keyboard::Key::Named(named) = kp.key.as_ref() {
        match named {
            key::Named::Enter if !m.shift() && !m.alt() => {
                return Some(Binding::Custom(if k.popup {
                    Message::AcConfirm
                } else {
                    Message::Send
                }));
            }
            key::Named::Tab if k.popup && !m.shift() => {
                return Some(Binding::Custom(Message::AcConfirm));
            }
            key::Named::ArrowUp if k.popup => return Some(Binding::Custom(Message::AcMove(-1))),
            key::Named::ArrowDown if k.popup => return Some(Binding::Custom(Message::AcMove(1))),
            key::Named::ArrowUp if k.empty && !m.shift() && !m.command() => {
                return Some(Binding::Custom(Message::EditLast));
            }
            key::Named::Escape if k.popup || k.cancellable => {
                return Some(Binding::Custom(Message::Cancel));
            }
            _ => {}
        }
    }
    if m.command() && !m.alt() {
        match kp.key.to_latin(kp.physical_key) {
            Some('v') if !m.shift() => {
                // Files/images are uploaded; text is pasted only without them.
                return Some(Binding::Custom(Message::PasteCheck));
            }
            Some('b') => return Some(Binding::Custom(Message::Format(Format::Bold))),
            Some('i') => return Some(Binding::Custom(Message::Format(Format::Italic))),
            _ => {}
        }
    }
    // Shift+Enter falls through to the default line break.
    Binding::from_key_press(kp)
}

#[cfg(test)]
mod tests;
