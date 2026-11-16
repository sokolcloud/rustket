//! Autocomplete popup state of the composer: opening, local matches and
//! merging server results.

use iced::Task;
use rocketchat::models::RoomType;
use rocketchat::rest::RoomMembersOptions;
use rustket_core::{ServerId, Workspace};

use super::autocomplete::{self, Item, Popup};
use super::cache::{cached_commands, cached_members, commands_recently_failed};
use super::send::load_commands;
use super::text::{self, Trigger, TriggerKind};
use super::{Composer, MEMBERS_COUNT, Message, QUERY_DELAY};
use crate::room::{Action, Effect, UpdateCtx};

impl Composer {
    /// Opens, updates or closes the autocomplete popup for the cursor position.
    pub(super) fn refresh_popup(&mut self, ctx: &mut UpdateCtx<'_>) -> Task<Message> {
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
                let mut items = autocomplete::match_users(ws, &self.rid, &members, &trigger.query);
                items.extend(autocomplete::group_footer(ws, &trigger.query, &items));
                items
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

    pub(super) fn recompute_local(&mut self, ctx: &UpdateCtx<'_>) {
        let Some(trigger) = self.popup.as_ref().map(|p| p.trigger.clone()) else {
            return;
        };
        let local = self.local_items(&trigger, ctx.workspace, ctx.server_id);
        if let Some(p) = &mut self.popup {
            p.set_items(local);
        }
    }

    /// Server results for query `seq`; results of an older query are dropped.
    pub(super) fn merge_remote(&mut self, seq: u64, remote: Vec<Item>, ctx: &UpdateCtx<'_>) {
        if self.popup.as_ref().is_none_or(|p| p.seq != seq) {
            return;
        }
        if let Some(p) = &mut self.popup {
            p.remote = remote;
        }
        self.recompute_local(ctx);
    }

    pub(super) fn select_item(&mut self, i: usize, ctx: &mut UpdateCtx<'_>) -> Action<Message> {
        let Some(popup) = self.popup.take() else {
            return Action::none();
        };
        let Some(item) = popup.items.get(i) else {
            return Action::none();
        };
        if matches!(item, Item::ManageGroups { .. }) {
            return Action::effect(Effect::ManageMentionGroups);
        }
        let (text, _, _) = self.snapshot();
        self.apply_edit(text::complete(&text, &popup.trigger, &item.replacement()));
        Action::task(Task::batch([self.after_edit(ctx), self.focus()]))
    }
}
