//! App-level wiring of the omnichannel controller: hooks called from
//! `App::update` (after every update, for every session event, for
//! `Message::Omni`, for the sidebar's omnichannel rows) and the contact
//! center in place of the room area.

use std::time::Duration;

use iced::{Element, Task};
use rocketchat::models::{AgentStatus, Room, RoomType, UserStatus};
use rocketchat::session::{ChangeKind, OmnichannelEvent, SessionEvent};
use rust_i18n::t;
use rustket_core::{Change, ServerId, Workspace};
use rustket_ui::widget::BannerKind;

use super::{Message as OMsg, contact_center, sidebar};
use crate::app::App;
use crate::message::Message;
use crate::room::UpdateCtx;
use crate::screens::directory::open;
use crate::session::{ServerEvent, SessionHandle};
use crate::workspace::ServerState;

/// The omnichannel side of a sign-out (`docs/omnichannel.md` §22.3a),
/// captured before the server state is reset.
#[derive(Debug, Clone, Copy, Default)]
pub struct AgentSignOut {
    agent: bool,
    available: bool,
}

impl AgentSignOut {
    pub fn of(ws: &Workspace) -> Self {
        Self {
            agent: ws.me().is_some_and(|m| m.is_livechat_agent()),
            available: super::is_available_agent(ws),
        }
    }

    /// Token-revoking sign-out of an agent: the DDP `logout` method, like the
    /// web client. It destroys the login token and runs the server's
    /// `onLogout` hook, which sets the agent not-available with
    /// `livechatStatusSystemModified`, so the next login restores the
    /// availability. Returns whether it succeeded (no REST logout needed then).
    pub async fn revoke(self, h: &SessionHandle) -> bool {
        if !self.agent {
            return false;
        }
        let call = h.session().call("logout", Vec::new());
        match tokio::time::timeout(SIGN_OUT_TIMEOUT, call).await {
            Ok(Ok(_)) => true,
            Ok(Err(e)) => {
                tracing::debug!(error = %e, "DDP logout");
                false
            }
            Err(_) => {
                tracing::debug!("DDP logout: timeout");
                false
            }
        }
    }

    /// Sign-out that keeps the token (personal access tokens): an available
    /// agent is set not-available through REST. Unlike the DDP `logout`, the
    /// server then keeps the agent not-available at the next login.
    pub async fn keep_token(self, h: &SessionHandle) {
        if !self.available {
            return;
        }
        let call = h
            .rest()
            .livechat_agent_status(Some(AgentStatus::NotAvailable));
        match tokio::time::timeout(SIGN_OUT_TIMEOUT, call).await {
            Ok(Ok(_)) => {}
            Ok(Err(e)) => tracing::debug!(error = %e, "agent not-available on sign-out"),
            Err(_) => tracing::debug!("agent not-available on sign-out: timeout"),
        }
    }
}

const SIGN_OUT_TIMEOUT: Duration = Duration::from_secs(5);

fn wrap(sid: &ServerId, epoch: u64, t: Task<OMsg>) -> Task<Message> {
    let sid = sid.clone();
    t.map(move |m| Message::Omni(sid.clone(), epoch, m))
}

impl App {
    fn play_new_room_sound(&self, sid: &ServerId) {
        let Some(s) = self.servers.get(sid) else {
            return;
        };
        // Busy users get no new-room sound (`useNotifyUser`).
        if s.workspace.my_status() == UserStatus::Busy {
            return;
        }
        if let Some(name) = super::new_room_sound(&s.workspace, self.config.ui.sound) {
            self.desktop.sound.play(Some(&name));
        }
    }

    /// After every update: per logged-in server, parked chat views back,
    /// loads for a new session, recompute after workspace changes, stream
    /// watches, snapshot.
    pub(crate) fn omni_after_update(&mut self) -> Task<Message> {
        let mut tasks = Vec::new();
        for s in self.servers.values_mut() {
            if !s.is_logged_in() {
                continue;
            }
            for (room, view) in s.omni.take_parked() {
                tasks.push(unpark(s, room, *view, &self.images, &self.config.ui));
            }
            let t = s.omni.tick(&s.id, &s.workspace, s.session.as_ref());
            tasks.push(wrap(&s.id, s.generation, t));
        }
        Task::batch(tasks)
    }

    /// A session event of `sid` (called before it is applied to the workspace).
    pub(crate) fn omni_on_event(&mut self, sid: &ServerId, ev: &ServerEvent) -> Task<Message> {
        let active = self.active.as_ref() == Some(sid);
        let Some(s) = self.servers.get_mut(sid) else {
            return Task::none();
        };
        let ev = match ev {
            ServerEvent::Event(ev) => ev,
            ServerEvent::Ended => {
                let _ = s.omni.tick(&s.id, &s.workspace, None);
                return Task::none();
            }
            _ => return Task::none(),
        };
        let mut tasks = Vec::new();
        let mut sound = false;
        let u = s.omni.state.apply_event(ev);
        sound |= u.queue_grew;
        if let Some(h) = s.session.clone() {
            let t = s.omni.handle(u, &s.workspace, &h);
            tasks.push(wrap(&s.id, s.generation, t));
        }
        if u.changed {
            s.omni.touch();
        }
        match &**ev {
            SessionEvent::Resync { .. } => s.omni.on_resync(),
            SessionEvent::SubscriptionChanged { kind, sub } if sub.t == RoomType::Livechat => {
                match kind {
                    ChangeKind::Inserted => {
                        // A new chat assigned to the user (§5.6).
                        let focused = active && s.active_room.as_deref() == Some(sub.rid.as_str());
                        if s.omni.first_seen(&sub.rid) && sub.alert && sub.ls.is_none() && !focused
                        {
                            sound = true;
                        }
                    }
                    ChangeKind::Removed => {
                        // Closed, forwarded or returned to the queue (§7.4).
                        s.omni.forget_seen(&sub.rid);
                        if let Some(rv) = s.rooms.remove(&sub.rid) {
                            // Kept open (as a closed preview) when the user
                            // keeps closed conversations, or until the result
                            // of the dialog's own request arrives (its toast).
                            let keep = !super::dialogs::hide_after_closing(&s.workspace)
                                || rv.omni.request_in_flight();
                            let room = s.workspace.room(&sub.rid).and_then(|e| e.room.clone());
                            match room {
                                Some(room) if keep => s.omni.park(room, rv),
                                _ => {
                                    let close =
                                        rv.close(&mut s.workspace, s.session.as_ref()).discard();
                                    tasks.push(close);
                                    // Its app panels go with it (the
                                    // after-update sweep only sees s.rooms).
                                    crate::uikit::controller::room_closed(
                                        &mut s.workspace,
                                        &sub.rid,
                                    );
                                    if s.active_room.as_deref() == Some(sub.rid.as_str()) {
                                        s.active_room = None;
                                    }
                                }
                            }
                        }
                    }
                    ChangeKind::Updated => {}
                }
            }
            SessionEvent::SubscriptionChanged { .. } => {}
            _ => {
                if let Some(OmnichannelEvent::RoomData(room)) = ev.omnichannel()
                    && s.workspace.room(&room.id).is_some()
                {
                    s.workspace.apply_room(Change::Updated, *room);
                }
            }
        }
        if sound {
            self.play_new_room_sound(sid);
        }
        Task::batch(tasks)
    }

    pub(crate) fn on_omni(&mut self, sid: ServerId, epoch: u64, m: OMsg) -> Task<Message> {
        let Some(s) = self.servers.get_mut(&sid) else {
            return Task::none();
        };
        if !s.is_current(epoch) {
            return Task::none();
        }
        match m {
            OMsg::Refreshed(r) => {
                match *r {
                    Ok(o) if s.rooms.contains_key(&o.room.id) => {
                        open::apply(&mut s.workspace, o);
                    }
                    Ok(_) => {}
                    Err(e) => tracing::debug!(error = %e, "closed chat refresh"),
                }
                Task::none()
            }
            OMsg::Opened(r) => match *r {
                Ok(o) => {
                    let rid = open::apply(&mut s.workspace, o);
                    self.open_room(&sid, &rid)
                }
                Err(f) => self.toast(
                    BannerKind::Error,
                    t!("omnichannel.open_failed"),
                    super::error_text(&f),
                ),
            },
            OMsg::ContactCenter(m) => {
                let rest = s.session.as_ref().map(|h| h.rest().clone());
                let Some(cc) = s.omni.contact_center.as_mut() else {
                    return Task::none();
                };
                let (task, out) = cc.update(m, rest, &s.workspace);
                let task = wrap(&sid, epoch, task.map(OMsg::ContactCenter));
                match out {
                    None => task,
                    Some(contact_center::Out::Close) => {
                        s.omni.close_contact_center();
                        task
                    }
                    Some(contact_center::Out::OpenChat(rid)) => {
                        s.omni.close_contact_center();
                        Task::batch([task, self.omni_open(&sid, &rid)])
                    }
                }
            }
            other => {
                let t = s.omni.apply(other, &s.id, &s.workspace, s.session.as_ref());
                wrap(&sid, epoch, t)
            }
        }
    }

    /// Opens `rid`, fetching it into the workspace first when needed (queued
    /// chats, closed chats from the contact center).
    fn omni_open(&mut self, sid: &ServerId, rid: &str) -> Task<Message> {
        let Some(s) = self.servers.get(sid) else {
            return Task::none();
        };
        if s.workspace.room(rid).is_some() {
            return self.open_room(sid, rid);
        }
        let Some(rest) = s.session.as_ref().map(|h| h.rest().clone()) else {
            return Task::none();
        };
        let target = open::Target::Room(rid.to_owned());
        let t = Task::perform(
            async move {
                open::resolve(rest, target)
                    .await
                    .map_err(crate::error::Failure::from)
            },
            |r| OMsg::Opened(Box::new(r)),
        );
        wrap(sid, s.generation, t)
    }

    /// The sidebar's omnichannel rows and buttons.
    pub(crate) fn on_omni_sidebar(&mut self, sid: ServerId, m: sidebar::Msg) -> Task<Message> {
        match m {
            sidebar::Msg::OpenQueued { rid, .. } => self.omni_open(&sid, &rid),
            sidebar::Msg::ContactCenter => {
                let Some(s) = self.servers.get_mut(&sid) else {
                    return Task::none();
                };
                s.status_menu.open = false;
                s.omni.open_contact_center();
                let rest = s.session.as_ref().map(|h| h.rest().clone());
                let epoch = s.generation;
                let task = match s.omni.contact_center.as_mut() {
                    Some(cc) => cc.load(rest, &s.workspace, 0),
                    None => Task::none(),
                };
                wrap(&sid, epoch, task.map(OMsg::ContactCenter))
            }
            // Handled by the sidebar itself (persisted collapse state).
            sidebar::Msg::Toggle(_) => Task::none(),
        }
    }

    /// The contact center, shown instead of the room area while open.
    pub(crate) fn omni_screen<'a>(&'a self, s: &'a ServerState) -> Option<Element<'a, Message>> {
        let cc = s.omni.contact_center.as_ref()?;
        let (sid, epoch) = (s.id.clone(), s.generation);
        Some(
            cc.view(self.config.ui.clock_24h)
                .map(move |m| Message::Omni(sid.clone(), epoch, OMsg::ContactCenter(m))),
        )
    }
}

/// Puts a parked chat view back after its subscription was removed: the
/// room as a preview (the server's copy is re-read for `open: false`), the
/// history reloaded (the removal dropped the timeline).
fn unpark(
    s: &mut ServerState,
    room: Room,
    mut view: crate::room::RoomView,
    images: &crate::images::Images,
    prefs: &rustket_core::config::UiPrefs,
) -> Task<Message> {
    let rid = room.id.clone();
    if s.workspace.room(&rid).is_none() {
        s.workspace.apply_room(Change::Updated, room);
    }
    let epoch = s.generation;
    let mut ctx = UpdateCtx {
        server_id: &s.id,
        server: &s.url,
        rid: &rid,
        workspace: &mut s.workspace,
        session: s.session.as_ref(),
        images,
        prefs,
    };
    let history = view.on_reconnect(&mut ctx).task;
    s.rooms.insert(rid.clone(), view);
    let (sid, r) = (s.id.clone(), rid.clone());
    let history = history.map(move |m| Message::Room(sid.clone(), epoch, r.clone(), m));
    let refresh = match s.session.as_ref().map(|h| h.rest().clone()) {
        Some(rest) => wrap(
            &s.id,
            epoch,
            Task::perform(
                async move {
                    open::resolve(rest, open::Target::Room(rid))
                        .await
                        .map_err(crate::error::Failure::from)
                },
                |r| OMsg::Refreshed(Box::new(r)),
            ),
        ),
        None => Task::none(),
    };
    Task::batch([history, refresh])
}
