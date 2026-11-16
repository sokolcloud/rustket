//! Sending: messages, edits, slash commands, retries and the outbox requests.

use iced::Task;
use iced::widget::text_editor;
use rocketchat::models::{OutgoingMessage, RoomType};
use rust_i18n::t;
use rustket_core::permissions;
use rustket_core::{ServerId, Workspace};
use rustket_ui::widget::BannerKind;

use super::cache::{
    cached_commands, cached_members, commands_recently_failed, outbox_finish, outbox_forget,
    outbox_start,
};
use super::text;
use super::{Composer, FetchFor, Message, PendingItem};
use crate::error::Res;
use crate::room::{Action, Effect, UpdateCtx, upload};

impl Composer {
    /// Enter: saves an edit, runs a slash command or sends the message.
    pub(super) fn send(&mut self, ctx: &mut UpdateCtx<'_>) -> Action<Message> {
        let raw = self.content.text();
        let typed = text::close_code_block(raw.trim());
        // Mention groups expand before anything else looks at the text
        // (length, `@all` confirmation, encryption); slash commands keep
        // their parameters as typed. The editor keeps `@group`, and
        // expanding an expanded text changes nothing, so a send run again
        // (after a confirmation) gives the same text.
        let command = self.editing.is_none()
            && self.quotes.is_empty()
            && text::parse_slash(&typed).is_some()
            && !crate::e2e::outgoing::slash_commands_blocked(ctx.workspace, &self.rid);
        let expanded = if command {
            None
        } else {
            expand_groups(ctx.workspace, ctx.server_id, &self.rid, &typed)
        };
        let added = expanded.as_ref().is_some_and(|e| !e.added.is_empty());
        let text = expanded.map_or_else(|| typed.clone(), |e| e.text);
        let max = ctx.workspace.settings().max_message_size();
        if let Some(ed) = self.editing.clone() {
            if text.is_empty() {
                // Like the web client: an emptied edit asks to delete the message.
                self.confirm_delete = Some(ed.mid);
                return Action::none();
            }
            // Unchanged as typed: no edit, even where a group would expand
            // now (a message sent elsewhere, or before the group existed).
            if typed == ed.original.trim() {
                self.cancel_edit();
                return Action::task(self.focus());
            }
            if text::too_long(&text, max) {
                return Action::effect(too_long(max));
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
            let task = Task::batch([
                ctx.rest_task(
                    move |rest| crate::e2e::outgoing::update_message(rest, rid, ed.mid, text, enc),
                    |r| Message::Updated(r.map(Box::new)),
                ),
                self.focus(),
            ]);
            // The server notifies nobody of an edit.
            let effects = added
                .then(|| {
                    Effect::Toast(
                        BannerKind::Info,
                        t!("mention_groups.edit_no_notify").into_owned(),
                        String::new(),
                    )
                })
                .into_iter()
                .collect();
            return Action { task, effects };
        }
        // Quotes need some text, as in the web client; forwarded messages
        // may go without a comment.
        if text.is_empty() && !self.quotes.iter().any(|q| q.forwarded) {
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
        if !is_command && let Some(ask) = self.ask_broad(&text, ctx) {
            return ask;
        }
        let permalinks: Vec<String> = self
            .quotes
            .iter()
            .map(|q| permalink(ctx.workspace, ctx.server.as_str(), &q.rid, &q.mid))
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
        let capped = added && encrypted_mentions_capped(ctx.workspace, &self.rid, &full);
        let mut action = if is_command {
            self.dispatch_text(full, ctx)
        } else {
            Action::task(self.send_text(full, ctx))
        };
        if capped {
            action.effects.push(Effect::Toast(
                BannerKind::Warning,
                t!("mention_groups.e2e_cap", max = MAX_ENCRYPTED_MENTIONS).into_owned(),
                String::new(),
            ));
        }
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

    /// Sends a poll from the "Create poll" dialog (no mention-group
    /// expansion or `@all` check: the text is generated); `reactions` are
    /// added once it is sent.
    pub(super) fn send_poll(
        &mut self,
        text: String,
        reactions: Vec<&'static str>,
        ctx: &mut UpdateCtx<'_>,
    ) -> Action<Message> {
        if matches!(
            crate::e2e::outgoing::mode(ctx.workspace, &self.rid),
            crate::e2e::outgoing::Mode::Blocked
        ) {
            return Action::effect(error_toast(crate::e2e::outgoing::blocked_text(
                ctx.workspace,
                &self.rid,
            )));
        }
        let max = ctx.workspace.settings().max_message_size();
        if text::too_long(&text, max) {
            return Action::effect(too_long(max));
        }
        self.poll = None;
        let m = ctx
            .workspace
            .new_local_message(&self.rid, &text, self.tmid.as_deref(), self.tshow);
        if !reactions.is_empty() {
            self.poll_reactions.insert(m.id.clone(), reactions);
        }
        ctx.workspace.add_pending(m.clone());
        Action::task(Task::batch([send_task(ctx, m), self.focus()]))
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

    pub(super) fn retry(&mut self, id: &str, ctx: &mut UpdateCtx<'_>) -> Action<Message> {
        let Some((m, rustket_core::timeline::DeliveryState::Failed { .. })) =
            self.owned_pending(ctx.workspace, id)
        else {
            return Action::none();
        };
        ctx.workspace
            .mark_pending_sending(&self.rid, m.tmid.as_deref(), id);
        Action::task(send_task(ctx, m))
    }

    pub(super) fn pending_tmid(&self, ws: &Workspace, id: &str) -> Option<String> {
        ws.pending_messages(&self.rid)
            .into_iter()
            .find(|i| i.message.id == id)
            .and_then(|i| i.message.tmid.clone())
            .or_else(|| self.tmid.clone())
    }

    /// Drops a pending (failed) message (see [`Self::owned_pending`]).
    pub(super) fn discard(&mut self, id: &str, ctx: &mut UpdateCtx<'_>) -> Action<Message> {
        if let Some((m, _)) = self.owned_pending(ctx.workspace, id) {
            ctx.workspace
                .discard_pending(&self.rid, m.tmid.as_deref(), id);
        }
        Action::none()
    }
}

/// Mentions an encrypted message lists in plaintext (the rest notify nobody).
const MAX_ENCRYPTED_MENTIONS: usize = rocketchat::e2e::MAX_MENTIONS;

/// Whether `text` sent to `rid` goes out encrypted with more user mentions
/// than its plaintext mention list holds.
fn encrypted_mentions_capped(ws: &Workspace, rid: &str, text: &str) -> bool {
    let crate::e2e::outgoing::Mode::Encrypt(_) = crate::e2e::outgoing::mode(ws, rid) else {
        return false;
    };
    let settings = crate::e2e::outgoing::settings(ws);
    settings.mentions
        && rocketchat::e2e::e2e_user_mention_count(
            text,
            &settings.username_pattern,
            &crate::e2e::outgoing::is_group(ws),
        ) > MAX_ENCRYPTED_MENTIONS
}

/// `text` with the account's mention groups expanded for room `rid`
/// ([`rustket_core::mention_groups::expand`]); `None` when it has none. A
/// token that names a known user of the room (a member, the DM partner) is
/// left alone.
pub(crate) fn expand_groups(
    ws: &Workspace,
    server: &ServerId,
    rid: &str,
    text: &str,
) -> Option<rustket_core::mention_groups::Expanded> {
    let groups = ws.mention_groups();
    if groups.is_empty() {
        return None;
    }
    let members = cached_members(server, rid);
    let is_user = |name: &str| {
        members
            .as_ref()
            .is_some_and(|list| list.iter().any(|m| m.username.as_deref() == Some(name)))
            || ws
                .room(rid)
                .is_some_and(|e| e.room_type() == RoomType::Direct && e.name() == name)
    };
    let opts = rustket_core::mention_groups::ExpandOptions {
        me: ws.username(),
        is_user: &is_user,
    };
    rustket_core::mention_groups::expand(text, groups, &opts)
}

/// Permalink of message `mid` of room `rid` (quote links).
fn permalink(ws: &Workspace, server: &str, rid: &str, mid: &str) -> String {
    let site = ws
        .settings()
        .str("Site_Url")
        .filter(|s| !s.trim().is_empty())
        .unwrap_or(server);
    let (t, name) = ws
        .room(rid)
        .map(|e| (e.room_type(), e.name().to_owned()))
        .unwrap_or((RoomType::Channel, String::new()));
    text::permalink(site, t, &name, rid, mid)
}

pub(super) fn error_toast(body: String) -> Effect {
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

pub(super) fn commands_unavailable() -> Effect {
    Effect::Toast(
        BannerKind::Error,
        t!("room_composer.command_failed").into_owned(),
        t!("room_composer.commands_unavailable").into_owned(),
    )
}

/// A previewed room the user may join: a channel, or a discussion of any
/// type (members of the parent may join it, like in the web client).
pub(super) fn can_join(ws: &Workspace, rid: &str) -> bool {
    ws.room(rid)
        .is_some_and(|e| e.room_type() == RoomType::Channel || e.is_discussion())
}

/// Joining needs a code: the room requires one and the user may not skip it.
pub(super) fn needs_join_code(ws: &Workspace, rid: &str) -> bool {
    ws.room(rid)
        .and_then(|e| e.room.as_ref())
        .is_some_and(|r| r.join_code_required == Some(true))
        && !permissions::has_permission(ws, "join-without-join-code", Some(rid))
}

pub(super) fn load_commands(ctx: &UpdateCtx<'_>, pending: Option<String>) -> Task<Message> {
    ctx.rest_task(
        |rest| async move { rest.commands_list().await },
        move |r| Message::CommandsLoaded(r, pending),
    )
}

pub(super) fn fetch(ctx: &UpdateCtx<'_>, mid: String, purpose: FetchFor) -> Task<Message> {
    ctx.rest_task(
        move |rest| async move { rest.get_message(&mid).await },
        move |r| Message::Fetched(purpose, r.map(Box::new)),
    )
}

/// Applies a `chat.sendMessage` result to the outbox message `id` (reply
/// in thread `tmid`).
pub(super) fn apply_send_result(
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

/// Adds `reactions` to message `mid` one by one (a poll's options), waiting
/// out rate limits (`chat.react` allows 10 a minute by default).
pub(super) async fn add_reactions(
    rest: rocketchat::RestClient,
    mid: String,
    reactions: Vec<&'static str>,
) -> rocketchat::Result<()> {
    for emoji in reactions {
        let mut waits = 0;
        loop {
            match rest.react(&mid, emoji, Some(true)).await {
                Ok(()) => break,
                Err(rocketchat::Error::RateLimited { retry_after, .. }) if waits < 3 => {
                    waits += 1;
                    let d = retry_after
                        .unwrap_or(std::time::Duration::from_secs(10))
                        .min(std::time::Duration::from_secs(60));
                    tokio::time::sleep(d).await;
                }
                Err(e) => return Err(e),
            }
        }
    }
    Ok(())
}
