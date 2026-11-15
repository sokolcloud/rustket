//! The user info beside the inbox and Later views
//! (`room::panel::user_info::Hosted`): a name clicked in a body there shows
//! who it is without opening the room (which would mark it read and close
//! the view).

use iced::Task;
use rustket_core::ServerId;

use super::App;
use crate::message::Message;
use crate::room::panel::user_info::{Hosted, HostedMsg};
use crate::room::{self, UpdateCtx};
use crate::workspace::ServerState;

/// The view hosting the panel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Host {
    Inbox,
    Later,
}

fn slot(s: &mut ServerState, host: Host) -> Option<&mut Option<Hosted>> {
    let v = s.main_view.as_mut()?;
    match host {
        Host::Inbox => v.inbox_mut().map(|v| &mut v.user),
        Host::Later => v.later_mut().map(|v| &mut v.user),
    }
}

fn wrap(host: Host, sid: ServerId, epoch: u64) -> impl Fn(HostedMsg) -> Message + Clone {
    move |m| match host {
        Host::Inbox => Message::Inbox(sid.clone(), epoch, crate::inbox::Msg::User(m)),
        Host::Later => Message::Later(sid.clone(), epoch, crate::later::Msg::User(m)),
    }
}

/// The user a body effect asks to show, if that is what it is.
fn user_of(e: &room::Effect) -> Option<&str> {
    match e {
        room::Effect::OpenPanel(room::panel::Kind::UserInfo(u)) => Some(u),
        _ => None,
    }
}

impl App {
    /// Effects of a body of room `rid` in a list of several rooms: user
    /// info opens beside the list; effects that need the room (a message's
    /// place, the composer) open it; the rest are the app's.
    pub(super) fn list_effects(
        &mut self,
        sid: &ServerId,
        rid: &str,
        effects: Vec<room::Effect>,
        host: Host,
    ) -> Task<Message> {
        let mut tasks = Vec::new();
        let mut rest = Vec::new();
        for e in effects {
            match user_of(&e) {
                Some(u) => tasks.push(self.show_user(sid, rid, u.to_owned(), host)),
                None => rest.push(e),
            }
        }
        let (local, app): (Vec<_>, Vec<_>) = rest.into_iter().partition(super::inbox::room_local);
        tasks.push(self.handle_effects(sid, rid, app));
        if !local.is_empty() {
            tasks.push(self.open_room(sid, rid));
            tasks.push(self.room_run_effects(sid, rid, local));
        }
        Task::batch(tasks)
    }

    fn show_user(
        &mut self,
        sid: &ServerId,
        rid: &str,
        username: String,
        host: Host,
    ) -> Task<Message> {
        let Some(s) = self.servers.get_mut(sid) else {
            return Task::none();
        };
        let epoch = s.generation;
        match slot(s, host) {
            None => return Task::none(),
            Some(Some(h)) if h.rid == rid && h.username() == username => return Task::none(),
            Some(_) => {}
        }
        let mut hosted = Hosted::new(rid, &username);
        let mut ctx = UpdateCtx {
            server_id: &s.id,
            server: &s.url,
            rid,
            workspace: &mut s.workspace,
            session: s.session.as_ref(),
            images: &self.images,
            prefs: &self.config.ui,
        };
        let action = hosted.info.activate(&mut ctx);
        let stamp = hosted.stamp;
        if let Some(slot) = slot(s, host) {
            *slot = Some(hosted);
        }
        let w = wrap(host, sid.clone(), epoch);
        let task = action.task.map(move |m| w(HostedMsg::Info(stamp, m)));
        let effects = self.list_effects(sid, rid, action.effects, host);
        Task::batch([task, effects])
    }

    /// A message of the user panel beside `host`'s view.
    pub(super) fn hosted_user(
        &mut self,
        sid: &ServerId,
        host: Host,
        m: HostedMsg,
    ) -> Task<Message> {
        let Some(s) = self.servers.get_mut(sid) else {
            return Task::none();
        };
        let epoch = s.generation;
        let Some(open) = slot(s, host) else {
            return Task::none();
        };
        let m = match m {
            HostedMsg::Close => {
                *open = None;
                return Task::none();
            }
            HostedMsg::Info(stamp, m) => {
                // A result for a panel since replaced or closed.
                if open.as_ref().is_none_or(|h| h.stamp != stamp) {
                    return Task::none();
                }
                m
            }
        };
        let Some(mut hosted) = open.take() else {
            return Task::none();
        };
        let stamp = hosted.stamp;
        let rid = hosted.rid.clone();
        let mut ctx = UpdateCtx {
            server_id: &s.id,
            server: &s.url,
            rid: &rid,
            workspace: &mut s.workspace,
            session: s.session.as_ref(),
            images: &self.images,
            prefs: &self.config.ui,
        };
        let action = hosted.info.update(m, &mut ctx);
        if let Some(slot) = slot(s, host) {
            *slot = Some(hosted);
        }
        let w = wrap(host, sid.clone(), epoch);
        let task = action.task.map(move |m| w(HostedMsg::Info(stamp, m)));
        let effects = self.list_effects(sid, &rid, action.effects, host);
        Task::batch([task, effects])
    }

    /// Per update: a direct message the panel created opens once its room
    /// arrived.
    pub(super) fn tick_hosted_user(&mut self, sid: &ServerId, host: Host) -> Task<Message> {
        let Some(s) = self.servers.get_mut(sid) else {
            return Task::none();
        };
        let ready = {
            let Some(view) = s.main_view.as_mut() else {
                return Task::none();
            };
            let hosted = match host {
                Host::Inbox => view.inbox_mut().and_then(|v| v.user.as_mut()),
                Host::Later => view.later_mut().and_then(|v| v.user.as_mut()),
            };
            match hosted {
                Some(h) => h.info.dm_ready(&s.workspace).map(|r| (h.rid.clone(), r)),
                None => None,
            }
        };
        match ready {
            Some((rid, dm)) => self.handle_effects(sid, &rid, vec![room::Effect::OpenRoom(dm)]),
            None => Task::none(),
        }
    }
}
