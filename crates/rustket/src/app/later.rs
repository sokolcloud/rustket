//! App wiring of Later (`crate::later`): opening the view in place of the
//! room area, the user's Later and reminder edits through the synced keys,
//! revealing messages, the paced fetch of reminder copies, and the view.

use std::time::{Duration, Instant};

use iced::{Element, Task};
use rust_i18n::t;
use rustket_core::ServerId;
use rustket_core::later::{LaterEdit, LaterError, LaterStatus, ts_ms};
use rustket_core::reminders::{ReminderEdit, ReminderError};
use rustket_core::workspace::{ConnectionState, LaterTab};
use rustket_ui::widget::BannerKind;

use super::{App, Screen};
use crate::error::Failure;
use crate::later::{LaterKey, LaterState, LaterView, Msg, Out};
use crate::message::Message;
use crate::room::UpdateCtx;
use crate::workspace::service_sync::Status;
use crate::workspace::threads::{Class, classify};
use crate::workspace::{MainView, ServerState};

/// `chat.getMessage` calls per session for reminder copies (the route
/// allows 10 a minute per IP, shared with the thread loader and links).
pub const COPY_FETCH_BUDGET: u32 = 10;
/// Minimum time between two of them.
pub const COPY_FETCH_INTERVAL: Duration = Duration::from_secs(20);

/// The worse of two sync states.
fn worse(a: Status, b: Status) -> Status {
    match (a, b) {
        (Status::Failed(e), _) | (_, Status::Failed(e)) => Status::Failed(e),
        (Status::Pending, _) | (_, Status::Pending) => Status::Pending,
        _ => Status::Synced,
    }
}

fn online(s: &ServerState) -> bool {
    s.session.is_some() && *s.workspace.connection() == ConnectionState::Connected
}

impl App {
    /// A result of the Later sync; tells the user when it gave up.
    pub(super) fn on_later_sync(
        &mut self,
        sid: ServerId,
        epoch: u64,
        m: crate::workspace::later_sync::LaterMsg,
    ) -> Task<Message> {
        let Some(s) = self.servers.get_mut(&sid).filter(|s| s.is_current(epoch)) else {
            return Task::none();
        };
        let outs = s.later.update(m, Instant::now());
        self.sync_failed(outs)
    }

    /// A result of the reminder sync; tells the user when it gave up.
    pub(super) fn on_reminder_sync(
        &mut self,
        sid: ServerId,
        epoch: u64,
        m: crate::workspace::later_sync::ReminderMsg,
    ) -> Task<Message> {
        let Some(s) = self.servers.get_mut(&sid).filter(|s| s.is_current(epoch)) else {
            return Task::none();
        };
        let outs = s.reminders.update(m, Instant::now());
        self.sync_failed(outs)
    }

    fn sync_failed(&mut self, outs: Vec<crate::workspace::service_sync::Out>) -> Task<Message> {
        let tasks: Vec<_> = outs
            .into_iter()
            .map(|out| match out {
                crate::workspace::service_sync::Out::Failed(f) => {
                    let body = f.user_text();
                    self.toast(BannerKind::Error, t!("later.sync_failed_toast"), body)
                }
            })
            .collect();
        Task::batch(tasks)
    }

    /// Opens the Later view of server `sid` (on `tab`, else the one shown
    /// last). `toggle`: an open Later view closes instead.
    pub(super) fn open_later(
        &mut self,
        sid: &ServerId,
        tab: Option<LaterTab>,
        toggle: bool,
    ) -> Task<Message> {
        let Some(s) = self.servers.get_mut(sid) else {
            return Task::none();
        };
        if s.login.is_some() || !s.is_logged_in() || s.drafts_dialog_open() {
            return Task::none();
        }
        if toggle && s.later_view().is_some() && tab.is_none() {
            s.close_main_view();
            return Task::none();
        }
        s.omni.close_contact_center();
        s.status_menu.open = false;
        match s.main_view.as_mut().and_then(MainView::later_mut) {
            Some(v) => {
                if let Some(t) = tab {
                    v.tab = t;
                }
            }
            None => {
                // Due reminders (the sidebar row's badge) first, else the
                // tab shown last.
                let now_ms = chrono::Utc::now().timestamp_millis();
                let tab = tab.unwrap_or_else(|| {
                    if s.workspace.overdue_reminders(now_ms) > 0 {
                        LaterTab::Reminders
                    } else {
                        s.later_state.last_tab.unwrap_or_default()
                    }
                });
                s.close_main_view();
                s.main_view = Some(MainView::Later(LaterView::new(tab)));
            }
        }
        Task::none()
    }

    /// Ctrl/Cmd+Shift+S: the active server's Later view, toggled.
    pub(super) fn later_shortcut(&mut self) -> Task<Message> {
        if !matches!(self.screen, Screen::Server) || self.overlay().0.is_some() {
            return Task::none();
        }
        let Some(sid) = self.active.clone() else {
            return Task::none();
        };
        self.open_later(&sid, None, true)
    }

    /// Up / Down / E nothing else took: the Later view's keys.
    pub(super) fn on_later_key(&mut self, k: LaterKey) -> Task<Message> {
        if !matches!(self.screen, Screen::Server) || self.overlay().0.is_some() {
            return Task::none();
        }
        let Some(s) = self.active.as_ref().and_then(|sid| self.servers.get(sid)) else {
            return Task::none();
        };
        if s.later_view().is_none() {
            return Task::none();
        }
        let (sid, epoch) = (s.id.clone(), s.generation);
        self.on_later(sid, epoch, Msg::Key(k))
    }

    pub(super) fn on_later(&mut self, sid: ServerId, epoch: u64, m: Msg) -> Task<Message> {
        let morning = self.config.ui.morning_hour();
        let Some(s) = self.servers.get_mut(&sid).filter(|s| s.is_current(epoch)) else {
            return Task::none();
        };
        let m = match m {
            Msg::View { rid, m } => return self.later_viewer(&sid, rid, m),
            Msg::User(m) => {
                return self.hosted_user(&sid, super::hosted_user::Host::Later, m);
            }
            m => m,
        };
        let is_online = online(s);
        let Some(view) = s.main_view.as_mut().and_then(MainView::later_mut) else {
            return Task::none();
        };
        let ctx = crate::later::UpdateCtx {
            workspace: &s.workspace,
            now: chrono::Local::now(),
            morning,
            online: is_online,
        };
        let outs = view.update(m, &ctx);
        self.later_outs(&sid, outs)
    }

    /// A message body's interaction in the Later view.
    fn later_viewer(
        &mut self,
        sid: &ServerId,
        rid: String,
        m: crate::room::timeline::ViewerMsg,
    ) -> Task<Message> {
        let Some(s) = self.servers.get_mut(sid) else {
            return Task::none();
        };
        let epoch = s.generation;
        let Some(view) = s.main_view.as_mut().and_then(MainView::later_mut) else {
            return Task::none();
        };
        let mut ctx = UpdateCtx {
            server_id: &s.id,
            server: &s.url,
            rid: &rid,
            workspace: &mut s.workspace,
            session: s.session.as_ref(),
            images: &self.images,
            prefs: &self.config.ui,
        };
        let opens = crate::room::timeline::viewer_opens_image(&m);
        let action = view.viewer.update(m, &mut ctx);
        view.image_rid = view
            .viewer
            .image_open()
            .then(|| {
                if opens {
                    Some(rid.clone())
                } else {
                    view.image_rid.take()
                }
            })
            .flatten();
        let (tsid, trid) = (sid.clone(), rid.clone());
        let task = action.task.map(move |m| {
            Message::Later(
                tsid.clone(),
                epoch,
                Msg::View {
                    rid: trid.clone(),
                    m,
                },
            )
        });
        let effects = self.later_outs(
            sid,
            vec![Out::Effects {
                rid,
                effects: action.effects,
            }],
        );
        Task::batch([task, effects])
    }

    fn later_outs(&mut self, sid: &ServerId, outs: Vec<Out>) -> Task<Message> {
        let mut tasks = Vec::new();
        for out in outs {
            tasks.push(match out {
                Out::Close => {
                    if let Some(s) = self.servers.get_mut(sid) {
                        s.close_main_view();
                    }
                    Task::none()
                }
                Out::Open { rid, tmid, mid } => self.reveal_message(sid, &rid, tmid, &mid),
                Out::Later(e) => self.set_later(sid, e),
                Out::Reminder(e) => self.set_reminder(sid, e),
                Out::Unstar(items) => self.queue_unstar(sid, items),
                Out::Scroll(mid) => crate::later::view::reveal(&mid),
                Out::Refresh => {
                    if let Some(s) = self.servers.get_mut(sid) {
                        s.threads.refresh_starred(&s.workspace);
                    }
                    Task::none()
                }
                Out::Effects { rid, effects } => {
                    // User info beside the view; a message's place opens
                    // the room, like the inbox's.
                    self.list_effects(sid, &rid, effects, super::hosted_user::Host::Later)
                }
            });
        }
        Task::batch(tasks)
    }

    /// Opens message `mid` of room `rid`: the room at the message from the
    /// local copy (its thread for a reply), else the room focused on it
    /// after a fetch. The server becomes active.
    pub(super) fn reveal_message(
        &mut self,
        sid: &ServerId,
        rid: &str,
        tmid: Option<String>,
        mid: &str,
    ) -> Task<Message> {
        let copy = self
            .servers
            .get(sid)
            .and_then(|s| s.workspace.later_message(mid).cloned());
        let activate =
            if self.active.as_ref() != Some(sid) || !matches!(self.screen, Screen::Server) {
                self.activate_for(sid.clone(), false)
            } else {
                Task::none()
            };
        let open = match copy {
            Some(m) => {
                let open = self.open_room(sid, rid);
                Task::batch([open, self.room_show(sid, rid, m)])
            }
            None => match tmid {
                Some(tmid) => self.go(crate::message::Go::Room {
                    sid: sid.clone(),
                    rid: rid.to_owned(),
                    tmid: Some(tmid),
                }),
                None => self.open_room_at(sid, rid, Some(mid)),
            },
        };
        Task::batch([activate, open])
    }

    /// Records a Later edit of the user: shown at once, stored on the
    /// server by the sync (`after_update` persists it until then).
    pub(super) fn set_later(&mut self, sid: &ServerId, edit: LaterEdit) -> Task<Message> {
        let (device, created) = self.config.ensure_device_id();
        let save = if created {
            self.save_config()
        } else {
            Task::none()
        };
        let Some(s) = self.servers.get_mut(sid) else {
            return save;
        };
        let now = Instant::now();
        let mut res = s.later.edit(&mut s.workspace, &edit, &device, now);
        if matches!(res, Err(LaterError::Full | LaterError::TooLarge)) {
            // Entries of messages unstarred elsewhere make room first.
            let orphans = s.workspace.later_orphans();
            if !orphans.is_empty() {
                tracing::debug!(
                    server = %sid,
                    entries = orphans.len(),
                    "Later full: forgetting entries of unstarred messages"
                );
                for mid in orphans {
                    let forget = LaterEdit::Forget { mid };
                    if let Err(e) = s.later.edit(&mut s.workspace, &forget, &device, now) {
                        tracing::debug!(server = %sid, error = %e, "Later orphan kept");
                    }
                }
                res = s.later.edit(&mut s.workspace, &edit, &device, now);
            }
        }
        match res {
            Ok(_) => save,
            Err(e) => {
                tracing::warn!(server = %sid, error = %e, "Later edit refused");
                let title = match e {
                    LaterError::Full | LaterError::TooLarge => t!("later.full"),
                    LaterError::InvalidId => t!("later.edit_failed"),
                };
                Task::batch([save, self.toast(BannerKind::Error, title, String::new())])
            }
        }
    }

    /// Records a reminder edit of the user (like [`Self::set_later`]); a
    /// new reminder keeps a local copy of its message for the notification.
    pub(super) fn set_reminder(&mut self, sid: &ServerId, edit: ReminderEdit) -> Task<Message> {
        self.set_reminder_with(sid, edit, true)
    }

    /// [`Self::set_reminder`]; `confirm`: a toast confirms the change (not
    /// for a notification's actions, which need none).
    pub(super) fn set_reminder_with(
        &mut self,
        sid: &ServerId,
        edit: ReminderEdit,
        confirm: bool,
    ) -> Task<Message> {
        let (device, created) = self.config.ensure_device_id();
        let save = if created {
            self.save_config()
        } else {
            Task::none()
        };
        let clock_24h = self.config.ui.clock_24h;
        let Some(s) = self.servers.get_mut(sid) else {
            return save;
        };
        match s
            .reminders
            .edit(&mut s.workspace, &edit, &device, Instant::now())
        {
            Ok(changed) => {
                let toast = match &edit {
                    ReminderEdit::Set {
                        rid,
                        tmid,
                        mid,
                        at_ms,
                    } if changed => {
                        if let Some(m) = find_message(&s.workspace, rid, tmid.as_deref(), mid) {
                            s.workspace.keep_reminder_copy(&m);
                        }
                        let time = crate::format::moment(*at_ms, chrono::Local::now(), clock_24h);
                        Some((
                            BannerKind::Success,
                            t!("room_timeline.remind_set", time = time.as_str()).into_owned(),
                        ))
                    }
                    ReminderEdit::Clear { .. } if changed => Some((
                        BannerKind::Success,
                        t!("room_timeline.remind_removed").into_owned(),
                    )),
                    _ => None,
                };
                match toast.filter(|_| confirm) {
                    Some((kind, title)) => {
                        Task::batch([save, self.toast(kind, title, String::new())])
                    }
                    None => save,
                }
            }
            Err(e) => {
                tracing::warn!(server = %sid, error = %e, "reminder edit refused");
                let title = match e {
                    ReminderError::Full | ReminderError::TooLarge => t!("later.reminders_full"),
                    ReminderError::Past => t!("later.custom_past"),
                    ReminderError::InvalidId => t!("later.edit_failed"),
                };
                Task::batch([save, self.toast(BannerKind::Error, title, String::new())])
            }
        }
    }

    /// The user starred (`on`) or unstarred message `m` from its menu:
    /// a star of an older message than Later's seed goes to In progress
    /// explicitly; an unstar forgets its Later entries.
    pub(super) fn later_starred(
        &mut self,
        sid: &ServerId,
        rid: &str,
        mid: &str,
        ts: Option<i64>,
        on: bool,
    ) -> Task<Message> {
        let edit = if on {
            LaterEdit::Status {
                rid: rid.to_owned(),
                mid: mid.to_owned(),
                ts_ms: ts,
                status: LaterStatus::InProgress,
            }
        } else {
            LaterEdit::Forget {
                mid: mid.to_owned(),
            }
        };
        self.set_later(sid, edit)
    }

    /// Per update: the Later view follows the workspace; reminder copies
    /// missing in the Reminders tab are fetched, paced.
    pub(super) fn tick_later(&mut self, sid: &ServerId) -> Task<Message> {
        let Some(s) = self.servers.get_mut(sid) else {
            return Task::none();
        };
        let ptr = s
            .session
            .as_ref()
            .map(|h| std::sync::Arc::as_ptr(&h.0) as usize);
        if s.later_state.session != ptr {
            s.later_state = LaterState {
                session: ptr,
                last_tab: s.later_state.last_tab,
                ..LaterState::default()
            };
        }
        let Some(view) = s.main_view.as_mut().and_then(MainView::later_mut) else {
            return Task::none();
        };
        s.later_state.last_tab = Some(view.tab);
        view.prune(&s.workspace);
        if view.tab != LaterTab::Reminders || !online(s) || s.workspace.from_cache() {
            return Task::none();
        }
        let st = &mut s.later_state;
        if st.in_flight || st.budget_used >= COPY_FETCH_BUDGET {
            return Task::none();
        }
        let Some(mid) = s
            .workspace
            .active_reminders()
            .iter()
            .find(|r| s.workspace.later_message(&r.mid).is_none() && !st.tried.contains(&r.mid))
            .map(|r| r.mid.clone())
        else {
            return Task::none();
        };
        let now = Instant::now();
        let epoch = s.generation;
        if let Some(at) = st
            .last_fetch
            .map(|t| t + COPY_FETCH_INTERVAL)
            .filter(|at| *at > now)
        {
            if st.timer.is_some_and(|t| t <= at && t > now) {
                return Task::none();
            }
            st.timer = Some(at);
            let sid = sid.clone();
            let wait = at - now;
            return Task::perform(async move { tokio::time::sleep(wait).await }, move |()| {
                Message::LaterCopy(sid.clone(), epoch, None)
            });
        }
        let Some(rest) = s.session.as_ref().map(|h| h.rest().clone()) else {
            return Task::none();
        };
        st.in_flight = true;
        st.budget_used += 1;
        st.last_fetch = Some(now);
        st.tried.insert(mid.clone());
        let sid = sid.clone();
        Task::perform(
            async move {
                rest.get_message(&mid)
                    .await
                    .map(|m| (mid, m))
                    .map_err(Failure::from)
            },
            move |r| Message::LaterCopy(sid.clone(), epoch, Some(r.map(|(_, m)| Box::new(m)))),
        )
    }

    /// A reminder copy fetch ended (`None`: the pacing timer).
    pub(super) fn on_later_copy(
        &mut self,
        sid: ServerId,
        epoch: u64,
        r: Option<Result<Box<rocketchat::models::Message>, Failure>>,
    ) -> Task<Message> {
        let Some(s) = self.servers.get_mut(&sid).filter(|s| s.is_current(epoch)) else {
            return Task::none();
        };
        let Some(r) = r else {
            s.later_state.timer = None;
            return Task::none();
        };
        s.later_state.in_flight = false;
        match r {
            Ok(m) => s.workspace.apply_later_messages(vec![*m]),
            Err(f) => {
                if let Class::RateLimited(d) = classify(&f) {
                    s.later_state.last_fetch = Some(Instant::now() + d);
                }
                tracing::debug!(error = %f, "reminder copy not fetched");
            }
        }
        Task::none()
    }

    /// The open Later view, shown instead of the room area.
    pub(super) fn later_screen<'a>(&'a self, s: &'a ServerState) -> Option<Element<'a, Message>> {
        let view = s.later_view()?;
        let ctx = crate::later::view::Ctx {
            server_id: &s.id,
            server: &s.url,
            workspace: &s.workspace,
            images: &self.images,
            prefs: &self.config.ui,
            online: online(s),
            sync: worse(s.later.status(), s.reminders.status()),
            scanning: s
                .threads
                .starred_progress(&s.workspace)
                .filter(|_| !s.workspace.from_cache()),
            now: Instant::now(),
            local_now: chrono::Local::now(),
        };
        let (sid, epoch) = (s.id.clone(), s.generation);
        crate::room::render::set_mode(self.mode());
        Some(
            view.view(&ctx)
                .map(move |m| Message::Later(sid.clone(), epoch, m)),
        )
    }
}

/// Message `mid` as the workspace shows it (timelines, Later copies).
pub(super) fn find_message(
    ws: &rustket_core::Workspace,
    rid: &str,
    tmid: Option<&str>,
    mid: &str,
) -> Option<rocketchat::models::Message> {
    ws.timeline(rid)
        .and_then(|t| t.get(mid))
        .map(|i| i.message.clone())
        .or_else(|| {
            let t = ws.thread(tmid.unwrap_or(mid))?;
            if t.parent().is_some_and(|p| p.id == mid) {
                return t.parent().cloned();
            }
            t.get(mid).map(|i| i.message.clone())
        })
        .or_else(|| ws.later_message(mid).cloned())
}

/// The time of a message as the workspace knows it.
pub(super) fn message_ts(ws: &rustket_core::Workspace, rid: &str, mid: &str) -> Option<i64> {
    find_message(ws, rid, None, mid)
        .as_ref()
        .and_then(ts_ms)
        .or_else(|| ws.thread_index().parent(mid).and_then(ts_ms))
}
