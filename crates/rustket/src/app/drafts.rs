//! App wiring of "Drafts & scheduled" (`crate::drafts`): opening the view in
//! place of the room area, applying its requests to the workspace and the
//! outbox, and the dialog of scheduled messages not sent on time.

use iced::{Element, Task};
use rust_i18n::t;
use rustket_core::ServerId;
use rustket_ui::widget::BannerKind;

use super::{App, Modal, Screen};
use crate::drafts::missed::{Item, Missed, MissedMsg};
use crate::drafts::{DraftsView, Msg, Out, Refusal};
use crate::later::when::{Kind, Picked, WhenPicker};
use crate::message::{Go, Message};
use crate::workspace::{MainView, ServerState};

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

impl App {
    /// Opens the Drafts & scheduled view of server `sid`. `toggle`: an open
    /// one closes instead.
    pub(super) fn open_drafts(&mut self, sid: &ServerId, toggle: bool) -> Task<Message> {
        let Some(s) = self.servers.get_mut(sid) else {
            return Task::none();
        };
        if s.login.is_some() || !s.is_logged_in() || s.drafts_dialog_open() {
            return Task::none();
        }
        if s.drafts_view().is_some() {
            if toggle {
                s.close_main_view();
            }
            return Task::none();
        }
        // The open room's composer text counts as its draft right away.
        if let Some(rv) = s.active_room.as_ref().and_then(|r| s.rooms.get(r)) {
            rv.store_drafts(&mut s.workspace);
        }
        s.omni.close_contact_center();
        s.status_menu.open = false;
        s.close_main_view();
        s.main_view = Some(MainView::Drafts(DraftsView::new()));
        Task::none()
    }

    /// Ctrl/Cmd+Shift+D: the active server's Drafts & scheduled view, toggled.
    pub(super) fn drafts_shortcut(&mut self) -> Task<Message> {
        if !matches!(self.screen, Screen::Server) || self.overlay().0.is_some() {
            return Task::none();
        }
        let Some(sid) = self.active.clone() else {
            return Task::none();
        };
        self.open_drafts(&sid, true)
    }

    pub(super) fn on_drafts(&mut self, sid: ServerId, epoch: u64, m: Msg) -> Task<Message> {
        let morning = self.config.ui.morning_hour();
        let confirm_broad = self.config.ui.confirm_broad_mentions;
        let Some(s) = self.servers.get_mut(&sid).filter(|s| s.is_current(epoch)) else {
            return Task::none();
        };
        let Some(view) = s.main_view.as_mut().and_then(MainView::drafts_mut) else {
            return Task::none();
        };
        let ctx = crate::drafts::UpdateCtx {
            workspace: &s.workspace,
            server: &sid,
            now: chrono::Local::now(),
            morning,
            confirm_broad,
        };
        let outs = view.update(m, &ctx);
        let mut tasks = Vec::new();
        for out in outs {
            tasks.push(self.drafts_out(&sid, out));
        }
        Task::batch(tasks)
    }

    fn drafts_out(&mut self, sid: &ServerId, out: Out) -> Task<Message> {
        let Some(s) = self.servers.get_mut(sid) else {
            return Task::none();
        };
        match out {
            Out::Close => {
                s.close_main_view();
                Task::none()
            }
            Out::SendNow(id) => {
                send_now(s, &id);
                Task::none()
            }
            Out::Reschedule(id, at) => {
                s.workspace.reschedule(&id, at);
                Task::none()
            }
            Out::SetText(id, text) => {
                s.workspace.set_scheduled_text(&id, &text);
                Task::none()
            }
            Out::Refused(Refusal::TooLong(max)) => self.toast(
                BannerKind::Error,
                t!("room_composer.too_long_title"),
                t!("room_composer.too_long", max = max),
            ),
            Out::Refused(Refusal::Command) => self.toast(
                BannerKind::Warning,
                t!("scheduled.no_commands"),
                String::new(),
            ),
            Out::Delete(id) => {
                s.workspace.remove_scheduled(&id);
                Task::none()
            }
            Out::Open { rid, tmid } => self.go(Go::Room {
                sid: sid.clone(),
                rid,
                tmid,
            }),
        }
    }

    /// The open Drafts & scheduled view, shown instead of the room area.
    pub(super) fn drafts_screen<'a>(&'a self, s: &'a ServerState) -> Option<Element<'a, Message>> {
        let view = s.drafts_view()?;
        let ctx = crate::drafts::view::Ctx {
            workspace: &s.workspace,
            prefs: &self.config.ui,
            local_now: chrono::Local::now(),
        };
        let (sid, epoch) = (s.id.clone(), s.generation);
        Some(
            view.view(&ctx)
                .map(move |m| Message::Drafts(sid.clone(), epoch, m)),
        )
    }

    /// Scheduled messages turned out missed: the dialog lists them (once
    /// per batch; closing it leaves them in the view).
    pub(crate) fn missed_scheduled(&mut self) {
        if self.modals.iter().any(|m| matches!(m, Modal::Missed(_))) {
            return;
        }
        self.modals.push(Modal::Missed(Missed::default()));
    }

    /// Every missed scheduled message, for the dialog.
    pub(super) fn missed_items(&self) -> Vec<Item> {
        let now = chrono::Local::now();
        let multi = self.servers.values().filter(|s| s.is_logged_in()).count() > 1;
        let mut items = Vec::new();
        for s in self.servers.values().filter(|s| s.is_logged_in()) {
            for e in s.workspace.scheduled().missed() {
                let m = &e.message;
                let mut place = crate::drafts::view::place(
                    &s.workspace,
                    &self.config.ui,
                    &m.rid,
                    m.tmid.as_deref(),
                );
                if multi {
                    place = format!("{place} · {}", s.name);
                }
                items.push(Item {
                    sid: s.id.clone(),
                    id: m.id.clone(),
                    place,
                    when: crate::drafts::view::missed_when(
                        crate::format::moment(e.send_at, now, self.config.ui.clock_24h),
                        e.error.as_deref(),
                    ),
                    preview: crate::drafts::preview(&m.msg, 160),
                    held: e.held,
                });
            }
        }
        items
    }

    pub(super) fn on_missed(&mut self, m: MissedMsg) -> Task<Message> {
        let morning = self.config.ui.morning_hour();
        let Some(Modal::Missed(state)) = self.modals.last_mut() else {
            return Task::none();
        };
        // An entry open in a dialog of Drafts & scheduled (being edited,
        // rescheduled or deleted there) is left to that dialog.
        let free =
            |s: &ServerState, id: &str| s.workspace.scheduled().get(id).is_some_and(|e| !e.held);
        match m {
            MissedMsg::SendNow(sid, id) => {
                if let Some(s) = self.servers.get_mut(&sid).filter(|s| free(s, &id)) {
                    send_now(s, &id);
                }
            }
            MissedMsg::Delete(sid, id) => {
                if let Some(s) = self.servers.get_mut(&sid).filter(|s| free(s, &id)) {
                    s.workspace.remove_scheduled(&id);
                }
            }
            MissedMsg::Reschedule(sid, id) => {
                let at = self
                    .servers
                    .get(&sid)
                    .and_then(|s| s.workspace.scheduled().get(&id))
                    .map(|e| e.send_at);
                state.reschedule = Some((sid, id, WhenPicker::new(Kind::Schedule, at)));
                return Task::none();
            }
            MissedMsg::Picker(pm) => {
                let Some((_, _, picker)) = state.reschedule.as_mut() else {
                    return Task::none();
                };
                let Some(Picked::At(at)) = picker.update(pm, chrono::Local::now(), morning) else {
                    return Task::none();
                };
                if let Some((sid, id, _)) = state.reschedule.take()
                    && let Some(s) = self.servers.get_mut(&sid)
                {
                    s.workspace.reschedule(&id, at);
                }
            }
            MissedMsg::CancelReschedule => {
                state.reschedule = None;
                return Task::none();
            }
            MissedMsg::DeleteAll => {
                state.confirm_delete_all = true;
                return Task::none();
            }
            MissedMsg::CancelDeleteAll => {
                state.confirm_delete_all = false;
                return Task::none();
            }
            MissedMsg::SendAll | MissedMsg::ConfirmDeleteAll => {
                state.confirm_delete_all = false;
                let send = matches!(m, MissedMsg::SendAll);
                for s in self.servers.values_mut() {
                    let ids: Vec<String> = s
                        .workspace
                        .scheduled()
                        .missed()
                        .iter()
                        .filter(|e| !e.held)
                        .map(|e| e.id().to_owned())
                        .collect();
                    for id in ids {
                        if send {
                            send_now(s, &id);
                        } else {
                            s.workspace.remove_scheduled(&id);
                        }
                    }
                }
            }
        }
        if self.missed_items().is_empty() {
            self.modals.retain(|m| !matches!(m, Modal::Missed(_)));
        }
        Task::none()
    }
}

/// Sends scheduled message `id` now: it becomes a queued outbox entry (sent
/// once the server answers).
fn send_now(s: &mut ServerState, id: &str) {
    if let Some(e) = s.workspace.take_scheduled_now(id, now_ms()) {
        crate::cache::enqueue_scheduled(s, e.message);
    }
}
