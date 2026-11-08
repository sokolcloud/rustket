//! App-level wiring of the omnichannel controller: hooks called from
//! `App::update` (after every update, for every session event, for
//! `Message::Omni`, for the sidebar's omnichannel rows) and the contact
//! center in place of the room area.

use std::time::Duration;

use iced::{Element, Task};
use rocketchat::models::{AgentStatus, RoomType, UserStatus};
use rocketchat::session::{ChangeKind, OmnichannelEvent, SessionEvent};
use rust_i18n::t;
use rustket_core::{Change, ServerId};
use rustket_ui::widget::BannerKind;

use super::{Message as OMsg, contact_center, sidebar};
use crate::app::App;
use crate::message::Message;
use crate::screens::directory::open;
use crate::session::{ServerEvent, SessionHandle};
use crate::workspace::ServerState;

/// Before signing out: an available agent becomes unavailable, like the web
/// client's DDP `logout` does (Rustket's REST logout does not, §3.4).
pub async fn sign_out_agent(h: &SessionHandle) {
    let call = h
        .rest()
        .livechat_agent_status(Some(AgentStatus::NotAvailable));
    match tokio::time::timeout(Duration::from_secs(5), call).await {
        Ok(Ok(_)) => {}
        Ok(Err(e)) => tracing::debug!(error = %e, "agent not-available on sign-out"),
        Err(_) => tracing::debug!("agent not-available on sign-out: timeout"),
    }
}

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
        if let Some(name) = super::new_room_sound(&s.workspace) {
            self.desktop.sound.play(Some(&name));
        }
    }

    /// After every update: per logged-in server, loads for a new session,
    /// recompute after workspace changes, stream watches, snapshot.
    pub(crate) fn omni_after_update(&mut self) -> Task<Message> {
        let mut tasks = Vec::new();
        for s in self.servers.values_mut() {
            if !s.is_logged_in() {
                continue;
            }
            s.omni.follow_active_room(s.active_room.as_deref());
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
            super::shared::publish(&s.id, &s.omni.state);
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
                        if s.rooms.contains_key(&sub.rid)
                            && super::dialogs::hide_after_closing(&s.workspace)
                            && let Some(rv) = s.rooms.remove(&sub.rid)
                        {
                            let close = rv.close(&mut s.workspace, s.session.as_ref()).discard();
                            tasks.push(close);
                            if s.active_room.as_deref() == Some(sub.rid.as_str()) {
                                s.active_room = None;
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
                s.omni.open_contact_center(s.active_room.as_deref());
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
            cc.view()
                .map(move |m| Message::Omni(sid.clone(), epoch, OMsg::ContactCenter(m))),
        )
    }
}
