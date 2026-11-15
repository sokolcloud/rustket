//! Server events and the initial load / resync of a server.

use std::time::Duration;

use iced::Task;
use rust_i18n::t;
use rustket_core::workspace::ConnectionState;
use rustket_core::{ServerId, sync};
use rustket_ui::widget::BannerKind;

use super::{App, Screen};
use crate::error::Failure;
use crate::message::Message;
use crate::room;
use crate::session::{self, Followup, ServerEvent};

impl App {
    pub(super) fn on_server_event(&mut self, sid: ServerId, ev: ServerEvent) -> Task<Message> {
        self.admin_event(&sid, &ev);
        let calls = match &ev {
            ServerEvent::Event(e) => {
                let (task, effects) = crate::videoconf::on_session_event(self, &sid, e);
                self.call_effects(task, effects)
            }
            _ => Task::none(),
        };
        Task::batch([calls, self.on_server_event_inner(sid, ev)])
    }

    fn on_server_event_inner(&mut self, sid: ServerId, ev: ServerEvent) -> Task<Message> {
        let Some(s) = self.servers.get_mut(&sid) else {
            return Task::none();
        };
        if let ServerEvent::Event(ev) = &ev {
            crate::e2e::on_event(s, ev);
        }
        match ev {
            ServerEvent::Ready(handle) => {
                s.session = Some(handle);
                s.start_error = None;
                // Room views opened before the session existed: watch + history.
                self.for_rooms(&sid, None, |rv, ctx| rv.on_reconnect(ctx))
            }
            ServerEvent::Event(ev) => match session::apply_event_for(
                &mut s.workspace,
                &ev,
                s.session
                    .as_ref()
                    .and_then(session::SessionHandle::ddp_session_id)
                    .as_deref(),
            ) {
                Followup::None => Task::none(),
                Followup::Notification(n) => {
                    let n = crate::e2e::notify::prepare(&s.workspace, n);
                    let name = s.name.clone();
                    let server_active =
                        self.active.as_ref() == Some(&sid) && matches!(self.screen, Screen::Server);
                    // The contact center replaces the room area: no room is shown.
                    let focused_rid = s
                        .active_room
                        .as_deref()
                        .filter(|_| s.omni.contact_center.is_none());
                    let focused_tmid =
                        focused_rid.and_then(|rid| s.rooms.get(rid)).and_then(|rv| {
                            match rv.panel.kind() {
                                Some(room::panel::Kind::Thread(tmid)) => Some(tmid.as_str()),
                                _ => None,
                            }
                        });
                    let env = crate::desktop::NotifyEnv {
                        images: &self.images,
                        server: &s.url,
                        server_active,
                        focused_rid,
                        focused_tmid,
                        window_active: self.focused,
                        multi_server: self.config.servers.len() > 1,
                    };
                    self.desktop
                        .on_notification(&sid, &name, &n, &s.workspace, &self.config.ui, &env)
                        .map(Message::Desktop)
                }
                Followup::InitialLoad => {
                    let boot = self.uikit_bootstrap(&sid);
                    Task::batch([self.initial_load(&sid), boot])
                }
                Followup::Resync { reconnected } => {
                    // Events were dropped (or the link was down): the Threads
                    // loader re-checks rooms with replies once the delta is in.
                    s.threads.note_missed_live(&s.workspace);
                    let sync = self.resync(&sid);
                    let rooms = if reconnected {
                        let boot = self.uikit_bootstrap(&sid);
                        let rooms = self.for_rooms(&sid, None, |rv, ctx| rv.on_reconnect(ctx));
                        Task::batch([rooms, boot])
                    } else {
                        Task::none()
                    };
                    Task::batch([sync, rooms])
                }
                Followup::RoomReset(rid) => {
                    self.for_rooms(&sid, Some(&rid), |rv, ctx| rv.reload(ctx))
                }
                Followup::LoggedOut(reason) => self.logged_out(&sid, Some(reason)),
                Followup::UiKit(hook) => self.on_uikit_session(&sid, hook),
                Followup::AvatarChanged { username, rid } => {
                    let url = s.url.clone();
                    self.images
                        .invalidate_avatar(&sid, &url, username.as_deref(), rid.as_deref());
                    Task::none()
                }
            },
            ServerEvent::StartFailed(f) => {
                if f.error().is_unauthorized() {
                    return self.logged_out(&sid, Some(String::from("rejected")));
                }
                let text = f.user_text();
                s.start_error = Some(text.clone());
                s.workspace.apply_connection(ConnectionState::Disconnected);
                let name = s.name.clone();
                self.toast(
                    BannerKind::Error,
                    t!("app.connect_failed", server = name),
                    text,
                )
            }
            ServerEvent::Ended => {
                s.session = None;
                if s.credentials.is_some()
                    && *s.workspace.connection() != ConnectionState::LoggedOut
                {
                    s.workspace.apply_connection(ConnectionState::Disconnected);
                }
                Task::none()
            }
        }
    }

    /// Starts the initial load unless a sync is already running (then a sync
    /// runs once more when it finishes).
    fn initial_load(&mut self, sid: &ServerId) -> Task<Message> {
        let Some(s) = self.servers.get_mut(sid) else {
            return Task::none();
        };
        if s.syncing {
            s.resync_pending = true;
            return Task::none();
        }
        let Some(rest) = s.session.as_ref().map(|h| h.rest().clone()) else {
            return Task::none();
        };
        s.syncing = true;
        s.threads.sync_started(&s.workspace);
        let sid = sid.clone();
        let epoch = s.generation;
        Task::perform(
            async move {
                sync::initial_load(&rest)
                    .await
                    .map(Box::new)
                    .map_err(Failure::from)
            },
            move |r| Message::InitialLoaded(sid.clone(), epoch, r),
        )
    }

    /// Delta sync (initial load when nothing was loaded yet); coalesced while
    /// a sync runs.
    pub(super) fn resync(&mut self, sid: &ServerId) -> Task<Message> {
        let Some(s) = self.servers.get_mut(sid) else {
            return Task::none();
        };
        if s.syncing {
            s.resync_pending = true;
            return Task::none();
        }
        let Some(since) = s.workspace.last_sync() else {
            return self.initial_load(sid);
        };
        let Some(rest) = s.session.as_ref().map(|h| h.rest().clone()) else {
            return Task::none();
        };
        s.syncing = true;
        s.threads.sync_started(&s.workspace);
        let rooms: Vec<(String, rocketchat::RcDate)> = s
            .rooms
            .keys()
            .filter_map(|rid| Some((rid.clone(), s.workspace.timeline(rid)?.last_updated_at()?)))
            .collect();
        let sid = sid.clone();
        let epoch = s.generation;
        Task::perform(
            async move {
                let delta = sync::delta_sync(&rest, since)
                    .await
                    .map(Box::new)
                    .map_err(Failure::from);
                let rooms = iced::futures::future::join_all(rooms.into_iter().map(|(rid, ts)| {
                    let rest = rest.clone();
                    async move {
                        sync::sync_room(&rest, &rid, ts)
                            .await
                            .map_err(Failure::from)
                    }
                }))
                .await;
                (delta, rooms)
            },
            move |(d, r)| Message::Resynced(sid.clone(), epoch, d, r),
        )
    }

    /// Common tail of a finished sync: retry on failure, run a pending resync.
    fn sync_done(&mut self, sid: &ServerId, error: Option<Failure>) -> Task<Message> {
        let Some(s) = self.servers.get_mut(sid) else {
            return Task::none();
        };
        s.syncing = false;
        match error {
            None => {
                s.sync_attempt = 0;
                s.sync_error = None;
                if std::mem::take(&mut s.resync_pending) {
                    return self.resync(sid);
                }
                Task::none()
            }
            Some(e) if e.error().is_unauthorized() => {
                self.logged_out(sid, Some(String::from("rejected")))
            }
            Some(e) => {
                s.resync_pending = false;
                s.sync_attempt += 1;
                s.sync_error = Some(e.user_text());
                let delay = sync_backoff(s.sync_attempt);
                tracing::warn!(server = %sid, attempt = s.sync_attempt, error = %e, "sync failed; retrying");
                let (sid, epoch) = (sid.clone(), s.generation);
                Task::perform(tokio::time::sleep(delay), move |_| {
                    Message::RetrySync(sid.clone(), epoch)
                })
            }
        }
    }

    pub(super) fn on_initial_loaded(
        &mut self,
        sid: ServerId,
        epoch: u64,
        r: Result<Box<sync::InitialData>, Failure>,
    ) -> Task<Message> {
        let last_room = self.config.server(&sid).and_then(|e| e.last_room.clone());
        let Some(s) = self.servers.get_mut(&sid) else {
            return Task::none();
        };
        if !s.is_current(epoch) {
            tracing::debug!(server = %sid, "dropping initial load of an old session");
            return Task::none();
        }
        let data = match r {
            Ok(d) => d,
            Err(e) => return self.sync_done(&sid, Some(e)),
        };
        for w in &data.warnings {
            tracing::warn!(what = w.what, error = %w.error, "initial load warning");
        }
        // Realtime events received while the snapshot loaded are kept: the
        // workspace merges by `_updatedAt` and drops only older entries.
        s.workspace.apply_initial(*data);
        if let Some(username) = s.workspace.username().map(str::to_owned)
            && let Some(entry) = self.config.server_mut(&sid)
            && entry.username.as_deref() != Some(&username)
        {
            entry.username = Some(username);
        }
        let mut tasks = vec![self.sync_done(&sid, None)];
        let Some(s) = self.servers.get_mut(&sid) else {
            return Task::batch(tasks);
        };
        // A queued room link wins over the last room. The last room of a
        // server in the background opens when that server is shown
        // (`activate`).
        if let Some((rid, path, _)) = s.pending_room.take() {
            tasks.push(self.open_room_link(&sid, rid, path));
        } else if s.active_room.is_none()
            && self.active.as_ref() == Some(&sid)
            && let Some(rid) = last_room.filter(|r| s.workspace.room(r).is_some())
        {
            tasks.push(self.open_room(&sid, &rid));
        } else {
            tasks.push(self.for_rooms(&sid, None, |rv, ctx| rv.on_reconnect(ctx)));
        }
        Task::batch(tasks)
    }

    pub(super) fn on_resynced(
        &mut self,
        sid: ServerId,
        epoch: u64,
        delta: Result<Box<sync::DeltaData>, Failure>,
        rooms: Vec<Result<sync::RoomSync, Failure>>,
    ) -> Task<Message> {
        let Some(s) = self.servers.get_mut(&sid) else {
            return Task::none();
        };
        if !s.is_current(epoch) {
            return Task::none();
        }
        let error = match delta {
            Ok(d) => {
                s.workspace.apply_delta(*d);
                None
            }
            Err(e) => Some(e),
        };
        for r in rooms {
            match r {
                Ok(rs) => s.workspace.apply_room_sync(rs),
                Err(e) => tracing::warn!(error = %e, "room sync failed"),
            }
        }
        let done = self.sync_done(&sid, error);
        // Timelines whose first page never arrived are reloaded.
        let rooms = self.for_rooms(&sid, None, |rv, ctx| rv.on_reconnect(ctx));
        Task::batch([done, rooms])
    }
}

fn sync_backoff(attempt: u32) -> Duration {
    Duration::from_secs(2u64.saturating_pow(attempt.clamp(1, 6)).min(60))
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn sync_backoff_grows_and_caps() {
        assert_eq!(sync_backoff(1), Duration::from_secs(2));
        assert_eq!(sync_backoff(2), Duration::from_secs(4));
        assert_eq!(sync_backoff(30), Duration::from_secs(60));
    }
}
