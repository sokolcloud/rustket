//! Message composer: multi-line editor (Enter sends, Shift+Enter breaks the
//! line), formatting toolbar, `@`/`#`/`:`/`/` autocomplete, emoji picker,
//! edit/quote modes, slash commands, uploads, drafts and typing notifications.
//! Messages go through the workspace outbox (`new_local_message` /
//! `add_pending`, failed sends can be retried or discarded).
//! Strings: `locales/room_composer.yml` (`room_composer.*`).

pub mod autocomplete;
pub(crate) mod broad;
mod cache;
pub(crate) mod poll;
mod popup;
mod send;
pub mod text;
mod view;

use std::ops::Range;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use iced::widget::text_editor;
use iced::{Subscription, Task};
use rocketchat::models::{RoomSummary, SlashCommand, UserSummary};
use rust_i18n::t;
use rustket_core::Workspace;
use rustket_core::permissions;
use rustket_core::typing::{TypingAction, TypingThrottle, USER_TYPING};
use rustket_core::workspace::Changes;
use rustket_ui::widget::BannerKind;

use self::autocomplete::{Item, Popup};
pub use self::cache::forget_commands;
use self::cache::{
    commands_failed, outbox_in_flight, outbox_take_done, store_commands, store_members,
};
pub(crate) use self::cache::{
    outbox_forget, outbox_start, poll_reactions_peek, poll_reactions_restore, poll_reactions_take,
};
pub(crate) use self::send::add_reactions;
pub(crate) use self::send::expand_groups;
pub use self::send::orphan_result;
use self::send::{apply_send_result, commands_unavailable, error_toast, fetch, needs_join_code};
use self::text::{Format, TriggerKind};
use super::emoji_picker::{self, EmojiPicker};
use super::upload::{self, Uploads};
use super::{Action, ComposeIntent, Effect, UpdateCtx};
use crate::error::Res;
use crate::session::SessionHandle;
#[cfg(test)]
use {
    self::cache::{cached_commands, commands_recently_failed},
    self::send::can_join,
    self::view::{Keys, key_binding},
    super::ViewCtx,
};

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
    /// Escape with nothing to cancel: the editor gives up the focus (an
    /// empty thread composer closes the thread, like the web).
    Leave,
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
    /// Ctrl/Cmd+Shift+\: the emoji picker for the newest message here (the
    /// room timeline without thread replies, or this thread).
    ReactLast,
    /// Shift+Esc in the focused composer: the app's "Mark all as read?"
    /// (the app listener skips keys a widget took).
    AskReadAll,
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
    /// The `@all`/`@here` confirmation: send anyway, or keep the text.
    BroadConfirmed,
    BroadCancelled,
    /// Members online for the `@here` confirmation (its `seq`).
    OnlineCount(u64, Res<u64>),
    /// `users.info` of the DM partner (local time hint).
    PeerLoaded(String, Res<Box<rocketchat::models::User>>),
    /// Redraws the DM partner's local time.
    ClockTick,
    /// Ctrl/Cmd+Shift+Enter or the clock button: pick a time to send the
    /// message at instead of now.
    Schedule,
    SchedulePicker(crate::later::when::Msg),
    ScheduleCancelled,
    /// The "N scheduled" hint: open the Drafts & scheduled view.
    OpenScheduled,
    /// Toolbar: open the "Create poll" dialog.
    OpenPoll,
    /// The "Create poll" dialog.
    Poll(poll::Msg),
    /// The option reactions of a sent poll were added (or failed).
    PollReacted(Res<()>),
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
    /// Room of the quoted message (the permalink is built for it).
    rid: String,
    mid: String,
    author: String,
    excerpt: String,
    /// Forwarded from elsewhere: may be sent without a comment.
    forwarded: bool,
}

impl Quote {
    fn from_ref(r: super::ForwardRef, forwarded: bool) -> Self {
        Self {
            rid: r.rid,
            mid: r.mid,
            author: r.author,
            excerpt: r.excerpt,
            forwarded,
        }
    }
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
    /// A send waiting for the `@all`/`@here` confirmation.
    confirm_broad: Option<broad::BroadConfirm>,
    /// The confirmation was given: the next `send` skips the check.
    broad_ok: bool,
    broad_seq: u64,
    /// 1:1 DM: the partner's username and, once known, their offset.
    peer: Option<(String, Option<cache::PeerInfo>)>,
    /// Thread composer: replies are also sent to the channel (`tshow`).
    tshow: bool,
    /// The "Schedule message" picker is open.
    schedule: Option<crate::later::when::WhenPicker>,
    /// The send running now schedules the message for this time (UTC ms)
    /// instead (kept through the `@all` confirmation).
    schedule_at: Option<i64>,
    /// The open "Create poll" dialog.
    poll: Option<poll::PollDraft>,
    /// A poll waiting for the `@all` confirmation: its text and reactions.
    broad_poll: Option<(String, Vec<&'static str>)>,
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
            confirm_broad: None,
            broad_ok: false,
            broad_seq: 0,
            peer: None,
            tshow: false,
            schedule: None,
            schedule_at: None,
            poll: None,
            broad_poll: None,
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

    /// The message the open emoji picker reacts to.
    #[cfg(test)]
    fn react_target(&self) -> Option<&str> {
        match &self.picker {
            Some((_, PickTarget::React(mid))) => Some(mid),
            _ => None,
        }
    }

    pub fn is_upload_dialog_open(&self) -> bool {
        self.uploads.is_dialog_open()
    }

    /// A confirmation dialog of the composer is open (delete an emptied
    /// edit, `@all`/`@here`).
    pub fn is_confirm_open(&self) -> bool {
        self.confirm_delete.is_some()
            || self.confirm_broad.is_some()
            || self.schedule.is_some()
            || self.poll.is_some()
    }

    /// Whether the message may be scheduled: not an edit, the offline cache
    /// keeps scheduled messages, and a room whose messages go out
    /// unencrypted (a scheduled text waits on disk, and plaintext of
    /// encrypted rooms never goes there).
    pub(crate) fn can_schedule(&self, ws: &Workspace) -> bool {
        self.editing.is_none()
            && ws.scheduled_storage()
            && matches!(
                crate::e2e::outgoing::mode(ws, &self.rid),
                crate::e2e::outgoing::Mode::Plain
            )
    }

    /// Widget id of the editor (F6 target).
    pub fn editor_id(&self) -> &iced::widget::Id {
        &self.editor_id
    }

    /// Whether the Send button is enabled: there is text, an edit to save,
    /// or forwarded quotes (which may go without a comment).
    pub(crate) fn can_send(&self) -> bool {
        !self.content.is_empty()
            || self.editing.is_some()
            || self.quotes.iter().any(|q| q.forwarded)
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
        Action::task(Task::batch([self.focus(), self.load_peer(ctx)]))
    }

    /// The partner of a 1:1 DM (room composer only) and their UTC offset,
    /// cached or fetched, for the local time hint.
    fn load_peer(&mut self, ctx: &UpdateCtx<'_>) -> Task<Message> {
        if self.tmid.is_some() {
            return Task::none();
        }
        let Some(u) = super::header::dm_partner(ctx.workspace, &self.rid)
            .filter(|u| Some(u.as_str()) != ctx.workspace.username())
        else {
            return Task::none();
        };
        let cached = cache::cached_peer(ctx.server_id, &u);
        let fetch = cached.is_none();
        self.peer = Some((u.clone(), cached));
        if !fetch {
            return Task::none();
        }
        let name = u.clone();
        ctx.rest_task(
            move |rest| async move {
                rest.users_info(rocketchat::rest::UserLookup::Username(&name))
                    .await
            },
            move |r| Message::PeerLoaded(u.clone(), r.map(Box::new)),
        )
    }

    /// The DM partner's clock when it differs from ours.
    fn peer_clock(&self) -> Option<(&str, &cache::PeerInfo, rustket_core::clock::PeerClock)> {
        let (u, info) = self.peer.as_ref()?;
        let info = info.as_ref()?;
        let mine = chrono::Local::now().offset().local_minus_utc();
        let clock = rustket_core::clock::peer_clock(info.offset?, mine, chrono::Utc::now())?;
        Some((u.as_str(), info, clock))
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
            Message::Leave => {
                if self.tmid.is_some() && self.content.text().trim().is_empty() {
                    return Action::effect(Effect::ClosePanel);
                }
                Action::task(iced::advanced::widget::operate(
                    iced::advanced::widget::operation::focusable::unfocus(),
                ))
            }
            Message::Cancel => {
                if self.schedule.is_some() {
                    self.schedule = None;
                } else if self.confirm_broad.is_some() {
                    // Over the poll dialog when a poll asked: back to it.
                    self.confirm_broad = None;
                    self.broad_poll = None;
                    self.schedule_at = None;
                } else if self.poll.is_some() {
                    self.poll = None;
                } else if self.confirm_delete.is_some() {
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
                        // Decrypted like every shown message: an encrypted
                        // room's message fetched here (an inbox Reply in a
                        // room not open yet) is quotable once it is.
                        let mut m = m;
                        crate::e2e::shown::process_list(
                            ctx.workspace,
                            ctx.server,
                            std::slice::from_mut(&mut m),
                        );
                        if crate::e2e::shown::undecrypted(&m) {
                            return Action::effect(error_toast(
                                t!("e2e.quote_unreadable").into_owned(),
                            ));
                        }
                        self.add_quote(&m);
                        Action::task(self.focus())
                    }
                },
                Err(e) => Action::effect(error_toast(e.user_text())),
            },
            Message::Sent(id, r) => {
                // A poll's option reactions follow from `send_task`.
                outbox_forget(ctx.server_id, &id);
                self.apply_sent(&id, r, ctx.workspace);
                Action::none()
            }
            Message::OpenPoll => {
                if self.editing.is_some() {
                    return Action::none();
                }
                self.picker = None;
                self.popup = None;
                self.poll = Some(poll::PollDraft::default());
                Action::task(iced::widget::operation::focus(iced::widget::Id::new(
                    poll::QUESTION_ID,
                )))
            }
            Message::Poll(m) => {
                let Some(draft) = self.poll.as_mut() else {
                    return Action::none();
                };
                match draft.update(m) {
                    poll::Done::Keep => Action::none(),
                    poll::Done::Close => {
                        self.poll = None;
                        Action::task(self.focus())
                    }
                    poll::Done::Send { text, reactions } => self.send_poll(text, reactions, ctx),
                }
            }
            Message::PollReacted(r) => match r {
                Ok(()) => Action::none(),
                Err(e) => Action::effect(Effect::Toast(
                    BannerKind::Error,
                    t!("poll.reactions_failed").into_owned(),
                    e.user_text(),
                )),
            },
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
            Message::ReactLast => {
                match last_reactable(ctx.workspace, &self.rid, self.tmid.as_deref()) {
                    Some(mid) => self.intent(ComposeIntent::React(mid), ctx),
                    None => Action::none(),
                }
            }
            Message::AskReadAll => Action::effect(Effect::AskReadAll),
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
            Message::BroadConfirmed => {
                if self.confirm_broad.take().is_none() {
                    return Action::none();
                }
                self.broad_ok = true;
                let a = match self.broad_poll.take() {
                    Some((text, reactions)) => self.send_poll(text, reactions, ctx),
                    None => self.send(ctx),
                };
                self.broad_ok = false;
                self.schedule_at = None;
                a
            }
            Message::BroadCancelled => {
                self.confirm_broad = None;
                self.broad_poll = None;
                self.schedule_at = None;
                Action::task(self.focus())
            }
            Message::Schedule => {
                if !self.can_send() || !self.can_schedule(ctx.workspace) {
                    return Action::none();
                }
                self.popup = None;
                self.schedule = Some(crate::later::when::WhenPicker::new(
                    crate::later::when::Kind::Schedule,
                    None,
                ));
                Action::none()
            }
            Message::SchedulePicker(m) => {
                let Some(p) = self.schedule.as_mut() else {
                    return Action::none();
                };
                let picked = p.update(m, chrono::Local::now(), ctx.prefs.morning_hour());
                let Some(crate::later::when::Picked::At(at)) = picked else {
                    if p.is_custom() {
                        return Action::task(iced::widget::operation::focus(
                            crate::later::when::DATE_INPUT_ID,
                        ));
                    }
                    return Action::none();
                };
                self.schedule = None;
                if !self.can_schedule(ctx.workspace) {
                    return Action::none();
                }
                self.schedule_at = Some(at);
                let a = self.send(ctx);
                if self.confirm_broad.is_none() {
                    self.schedule_at = None;
                }
                a
            }
            Message::ScheduleCancelled => {
                self.schedule = None;
                Action::task(self.focus())
            }
            Message::OpenScheduled => Action::effect(Effect::OpenDrafts),
            Message::PeerLoaded(u, r) => {
                match r {
                    Ok(user) => {
                        let info = cache::PeerInfo {
                            offset: user.utc_offset,
                            name: user.name.clone(),
                        };
                        cache::store_peer(ctx.server_id, &u, info.clone());
                        if let Some((p, slot)) = &mut self.peer
                            && *p == u
                        {
                            *slot = Some(info);
                        }
                    }
                    Err(e) => tracing::debug!(error = %e, "users.info of the DM partner failed"),
                }
                Action::none()
            }
            Message::ClockTick => Action::none(),
            Message::OnlineCount(seq, r) => {
                match r {
                    Ok(n) => {
                        if let Some(c) = self.confirm_broad.as_mut().filter(|c| c.seq == seq) {
                            // The sender is online too.
                            c.online = Some(n.saturating_sub(1));
                        }
                    }
                    Err(e) => tracing::debug!(error = %e, "online member count failed"),
                }
                Action::none()
            }
        }
    }

    /// Asks before `text` notifies the whole room (see
    /// [`broad::broad_check`]); `None` when it may go out now.
    fn ask_broad(&mut self, text: &str, ctx: &UpdateCtx<'_>) -> Option<Action<Message>> {
        if self.broad_ok || !ctx.prefs.confirm_broad_mentions {
            return None;
        }
        let mut c = broad::broad_check(ctx.workspace, &self.rid, text)?;
        self.broad_seq += 1;
        c.seq = self.broad_seq;
        let task = if c.here {
            let (rid, seq) = (self.rid.clone(), c.seq);
            ctx.rest_task(
                move |rest| async move { rest.room_member_total(&rid, &["online"]).await },
                move |r| Message::OnlineCount(seq, r),
            )
        } else {
            Task::none()
        };
        self.confirm_broad = Some(c);
        self.popup = None;
        Some(Action::task(task))
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
                    Some(m) if crate::e2e::shown::undecrypted(m) => {
                        Action::effect(error_toast(t!("e2e.quote_unreadable").into_owned()))
                    }
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
            ComposeIntent::Forward(refs) => {
                for r in refs {
                    if !self.quotes.iter().any(|q| q.mid == r.mid) {
                        self.quotes.push(Quote::from_ref(r, true));
                    }
                }
                Action::task(self.focus())
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
        let typing = if self.typing.is_active() {
            iced::time::every(Duration::from_secs(1)).map(|_| Message::TypingTick)
        } else {
            Subscription::none()
        };
        // The DM partner's local time stays current while it is shown.
        let clock = if self.peer_clock().is_some() {
            iced::time::every(Duration::from_secs(30)).map(|_| Message::ClockTick)
        } else {
            Subscription::none()
        };
        Subscription::batch([typing, clock])
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
        // Only what is shown decrypted can be quoted (the server's text of
        // an encrypted message is unauthenticated), whoever asks.
        if self.quotes.iter().any(|q| q.mid == m.id) || crate::e2e::shown::undecrypted(m) {
            return;
        }
        self.quotes
            .push(Quote::from_ref(super::ForwardRef::of(m), false));
    }

    fn open_picker(&mut self, target: PickTarget, ctx: &mut UpdateCtx<'_>) -> Action<Message> {
        let picker = EmojiPicker::new();
        picker.want_images(ctx.workspace, ctx.server_id, ctx.server, ctx.images);
        self.picker = Some((picker, target));
        self.popup = None;
        Action::task(EmojiPicker::focus())
    }
}

fn paste_text() -> Task<Message> {
    iced::clipboard::read().map(Message::Pasted)
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

/// The newest message to react to (Ctrl/Cmd+Shift+\): of the thread, or of
/// the room without the thread replies shown there.
fn last_reactable(ws: &Workspace, rid: &str, tmid: Option<&str>) -> Option<String> {
    let timeline = match tmid {
        Some(t) => ws.thread(t)?,
        None => ws.timeline(rid)?,
    };
    timeline
        .last_reactable(tmid.is_some())
        .map(|m| m.id.clone())
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

#[cfg(test)]
mod tests;
