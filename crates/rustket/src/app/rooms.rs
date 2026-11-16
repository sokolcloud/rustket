//! Rooms: opening and closing room views, sidebar actions, threads, flags,
//! room effects and calls.

use std::time::Instant;

use iced::Task;
use rust_i18n::t;
use rustket_core::ServerId;
use rustket_ui::widget::BannerKind;

use super::{App, Screen, components};
use crate::component;
use crate::error::Failure;
use crate::message::Message;
use crate::room::{self, RoomView, UpdateCtx};
use crate::workspace::sidebar;

impl App {
    /// Tells every room view whether it is on screen: the window is
    /// focused, its server is active and the server view is shown (not a
    /// page such as Settings or Administration, a view in place of the room
    /// area (`ServerState::room_area_taken`) or a dialog over it) and it is
    /// the server's active room. A room that
    /// becomes readable catches up on its read mark (`RoomView::set_visible`).
    pub(super) fn sync_room_visibility(&mut self) -> Task<Message> {
        let shown = self.focused && matches!(self.screen, Screen::Server) && self.modals.is_empty();
        let mut tasks = Vec::new();
        for (sid, s) in self.servers.iter_mut() {
            let server_shown = shown && self.active.as_ref() == Some(sid) && !s.room_area_taken();
            let epoch = s.generation;
            for (rid, rv) in s.rooms.iter_mut() {
                let visible = server_shown && s.active_room.as_deref() == Some(rid.as_str());
                let mut ctx = UpdateCtx {
                    server_id: &s.id,
                    server: &s.url,
                    rid,
                    workspace: &mut s.workspace,
                    session: s.session.as_ref(),
                    images: &self.images,
                    prefs: &self.config.ui,
                };
                let task = rv.set_visible(visible, &mut ctx);
                tasks.push(Self::room_task(sid, epoch, rid, task));
            }
        }
        Task::batch(tasks)
    }

    /// A result of the sidebar Threads loader; drops kept threads whose
    /// parent no longer exists from the config.
    pub(super) fn on_threads(
        &mut self,
        sid: ServerId,
        epoch: u64,
        m: crate::workspace::threads::Msg,
    ) -> Task<Message> {
        let Some(s) = self.servers.get_mut(&sid) else {
            return Task::none();
        };
        if !s.is_current(epoch) {
            return Task::none();
        }
        let mut save = false;
        for out in s.threads.update(m, &mut s.workspace, Instant::now()) {
            match out {
                crate::workspace::threads::Out::Unkeep { rid, tmid } => {
                    tracing::debug!(%rid, %tmid, "kept thread not found: removed");
                    if let Some(e) = self.config.server_mut(&sid) {
                        save |= e.set_thread_kept(&rid, &tmid, false);
                    }
                }
            }
        }
        if save {
            self.save_config()
        } else {
            Task::none()
        }
    }

    /// A result of the flag sync; tells the user when it gave up.
    pub(super) fn on_flags(
        &mut self,
        sid: ServerId,
        epoch: u64,
        m: crate::workspace::flags::Msg,
    ) -> Task<Message> {
        let Some(s) = self.servers.get_mut(&sid) else {
            return Task::none();
        };
        if !s.is_current(epoch) {
            return Task::none();
        }
        let outs = s.flags.update(m, Instant::now());
        let tasks: Vec<_> = outs
            .into_iter()
            .map(|out| match out {
                crate::workspace::flags::Out::Failed(f) => self.toast(
                    BannerKind::Error,
                    t!("sidebar.flags_sync_failed"),
                    f.user_text(),
                ),
            })
            .collect();
        Task::batch(tasks)
    }

    /// A push of the synced keys finished: each key takes its part.
    pub(super) fn on_service_pushed(
        &mut self,
        sid: ServerId,
        epoch: u64,
        o: crate::workspace::service_sync::Outcome,
    ) -> Task<Message> {
        let Some(s) = self.servers.get_mut(&sid) else {
            return Task::none();
        };
        if !s.is_current(epoch) {
            return Task::none();
        }
        let parts = s.service_pushed(&o, Instant::now());
        let mut tasks = Vec::new();
        if let Some(m) = parts.flags {
            tasks.push(self.on_flags(sid.clone(), epoch, m));
        }
        if let Some(m) = parts.mention_groups {
            tasks.push(self.on_mention_groups(sid.clone(), epoch, m));
        }
        if let Some(m) = parts.later {
            tasks.push(self.on_later_sync(sid.clone(), epoch, m));
        }
        if let Some(m) = parts.reminders {
            tasks.push(self.on_reminder_sync(sid, epoch, m));
        }
        Task::batch(tasks)
    }

    /// Applies a flag edit of the user: shown at once, stored on the server
    /// by the flag sync (`after_update` persists it until then).
    pub(super) fn set_flag(
        &mut self,
        sid: &ServerId,
        edit: rustket_core::flags::FlagEdit,
    ) -> Task<Message> {
        let (device, created) = self.config.ensure_device_id();
        let save = if created {
            self.save_config()
        } else {
            Task::none()
        };
        let Some(s) = self.servers.get_mut(sid) else {
            return save;
        };
        match s
            .flags
            .edit(&mut s.workspace, &edit, &device, Instant::now())
        {
            Ok(_) => save,
            Err(e) => {
                tracing::warn!(server = %sid, error = %e, "flag edit refused");
                let title = match e {
                    rustket_core::flags::FlagError::Full => t!("sidebar.flags_full"),
                    rustket_core::flags::FlagError::InvalidId => t!("sidebar.flag_failed"),
                };
                Task::batch([save, self.toast(BannerKind::Error, title, String::new())])
            }
        }
    }

    pub(super) fn on_sidebar(&mut self, sid: ServerId, m: sidebar::Message) -> Task<Message> {
        let Some(s) = self.servers.get_mut(&sid) else {
            return Task::none();
        };
        match s.sidebar.update(m) {
            sidebar::Out::None => Task::none(),
            sidebar::Out::OpenRoom(rid) => self.open_room(&sid, &rid),
            sidebar::Out::ToggleSection(key) => {
                if !self.ui_state.collapsed_sections.remove(&key) {
                    self.ui_state.collapsed_sections.insert(key);
                }
                self.save_config()
            }
            sidebar::Out::Status(m) => self.on_status_menu(sid, m),
            sidebar::Out::Omni(m) => self.on_omni_sidebar(sid, m),
            sidebar::Out::Inbox(kind) => self.open_inbox(&sid, kind, false),
            sidebar::Out::Later => self.open_later(&sid, None, false),
            sidebar::Out::Drafts => self.open_drafts(&sid, false),
            sidebar::Out::Search => self.open_search(&sid, false),
            sidebar::Out::Directory => self.navigate(component::Nav::Directory),
            sidebar::Out::Create => self.navigate(component::Nav::CreateRoom(
                crate::dialogs::create_room::Kind::Channel,
            )),
            sidebar::Out::OpenThread { rid, tmid } => self.go(crate::message::Go::Room {
                sid,
                rid,
                tmid: Some(tmid),
            }),
            sidebar::Out::KeepThread { rid, tmid, keep } => {
                self.keep_thread(&sid, &rid, &tmid, keep)
            }
            sidebar::Out::StarThread { rid, tmid, on } => self.star_thread(&sid, rid, tmid, on),
            sidebar::Out::ThreadCall { rid, tmid, call } => {
                let Some(rest) = s.session.as_ref().map(|h| h.rest().clone()) else {
                    return Task::none();
                };
                Task::perform(
                    async move {
                        match call {
                            sidebar::ThreadCall::MarkRead => rest.read_thread(&tmid).await,
                            sidebar::ThreadCall::MarkRoomRead => {
                                rest.subscriptions_read_with(&rid, true).await
                            }
                            sidebar::ThreadCall::Unfollow => rest.unfollow_message(&tmid).await,
                        }
                        .map_err(Failure::from)
                    },
                    move |r| Message::SidebarThreadDone(sid.clone(), call, r),
                )
            }
            sidebar::Out::Favorite { rid, on } => {
                let Some(rest) = s.session.as_ref().map(|h| h.rest().clone()) else {
                    return Task::none();
                };
                Task::perform(
                    async move { rest.rooms_favorite(&rid, on).await.map_err(Failure::from) },
                    move |r| Message::SidebarFavoriteDone(sid.clone(), on, r),
                )
            }
            sidebar::Out::Flag(edit) => self.set_flag(&sid, edit),
            sidebar::Out::Layout(e) => {
                let applied = rustket_core::sidebar_layout::apply(&mut self.config.ui, e);
                let mut changed = applied.changed;
                for key in &applied.dropped_collapse_keys {
                    changed |= self.ui_state.collapsed_sections.remove(key);
                }
                if changed {
                    self.save_config()
                } else {
                    Task::none()
                }
            }
            sidebar::Out::RefreshThreads => {
                s.threads.refresh(&s.workspace);
                Task::none()
            }
            sidebar::Out::RefreshStarred => {
                s.threads.refresh_starred(&s.workspace);
                Task::none()
            }
            sidebar::Out::OpenSettings { ungroup, flag } => {
                self.navigate(component::Nav::SidebarSettings { ungroup, flag })
            }
            sidebar::Out::ScrollBy(dy) => iced::widget::operation::scroll_by(
                sidebar::LIST_ID,
                iced::widget::scrollable::AbsoluteOffset { x: 0.0, y: dy },
            ),
        }
    }

    /// Opens a room of a server: closes the previous room view (dropping its
    /// watch), creates the new one (timeline, watch, history, read mark).
    pub fn open_room(&mut self, sid: &ServerId, rid: &str) -> Task<Message> {
        let Some(s) = self.servers.get_mut(sid) else {
            return Task::none();
        };
        if s.workspace.room(rid).is_none() {
            return Task::none();
        }
        s.omni.close_contact_center();
        s.close_main_view();
        if s.active_room.as_deref() == Some(rid) && s.rooms.contains_key(rid) {
            return Task::none();
        }
        let old: Vec<String> = s
            .rooms
            .keys()
            .filter(|r| r.as_str() != rid)
            .cloned()
            .collect();
        let mut closed = Vec::new();
        for r in old {
            if let Some(rv) = s.rooms.remove(&r) {
                closed.push(rv.close(&mut s.workspace, s.session.as_ref()).discard());
                crate::uikit::controller::room_closed(&mut s.workspace, &r);
            }
        }
        s.active_room = Some(rid.to_owned());
        let epoch = s.generation;
        let mut ctx = UpdateCtx {
            server_id: &s.id,
            server: &s.url,
            rid,
            workspace: &mut s.workspace,
            session: s.session.as_ref(),
            images: &self.images,
            prefs: &self.config.ui,
        };
        let (rv, action) = RoomView::open(&mut ctx);
        s.rooms.insert(rid.to_owned(), rv);
        let task = Self::room_task(sid, epoch, rid, action.task);
        if let Some(e) = self.config.server_mut(sid) {
            e.last_room = Some(rid.to_owned());
        }
        closed.extend([
            task,
            self.handle_effects(sid, rid, action.effects),
            self.save_config(),
        ]);
        Task::batch(closed)
    }

    pub(super) fn on_room(
        &mut self,
        sid: ServerId,
        epoch: u64,
        rid: String,
        m: room::Message,
    ) -> Task<Message> {
        let Some(s) = self.servers.get_mut(&sid) else {
            return Task::none();
        };
        if !s.is_current(epoch) {
            return Task::none();
        }
        let Some(rv) = s.rooms.get_mut(&rid) else {
            // Uploads and thread replies outlive the room view: their
            // results still settle (a failed upload becomes a toast).
            let effects = room::orphan_result(m, &mut s.workspace);
            return self.handle_effects(&sid, &rid, effects);
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
        let action = rv.update(m, &mut ctx);
        let task = Self::room_task(&sid, epoch, &rid, action.task);
        Task::batch([task, self.handle_effects(&sid, &rid, action.effects)])
    }

    /// Runs `f` on the open room views of `sid` (all, or only `only`).
    pub(super) fn for_rooms(
        &mut self,
        sid: &ServerId,
        only: Option<&str>,
        f: impl Fn(&mut RoomView, &mut UpdateCtx<'_>) -> room::Action<room::Message>,
    ) -> Task<Message> {
        let Some(s) = self.servers.get_mut(sid) else {
            return Task::none();
        };
        let epoch = s.generation;
        let mut tasks = Vec::new();
        for (rid, rv) in s.rooms.iter_mut() {
            if only.is_some_and(|o| o != rid) {
                continue;
            }
            let mut ctx = UpdateCtx {
                server_id: &s.id,
                server: &s.url,
                rid,
                workspace: &mut s.workspace,
                session: s.session.as_ref(),
                images: &self.images,
                prefs: &self.config.ui,
            };
            let action = f(rv, &mut ctx);
            tasks.push(Self::room_task(sid, epoch, rid, action.task));
        }
        Task::batch(tasks)
    }

    /// Drops the view of `rid` (the room stays in the workspace).
    fn close_room_view(&mut self, sid: &ServerId, rid: &str) -> Task<Message> {
        let Some(s) = self.servers.get_mut(sid) else {
            return Task::none();
        };
        let typing = match s.rooms.remove(rid) {
            Some(rv) => rv.close(&mut s.workspace, s.session.as_ref()).discard(),
            None => Task::none(),
        };
        crate::uikit::controller::room_closed(&mut s.workspace, rid);
        if s.active_room.as_deref() == Some(rid) {
            s.active_room = None;
        }
        if let Some(e) = self.config.server_mut(sid)
            && e.last_room.as_deref() == Some(rid)
        {
            e.last_room = None;
            return Task::batch([typing, self.save_config()]);
        }
        typing
    }

    pub(super) fn handle_effects(
        &mut self,
        sid: &ServerId,
        rid: &str,
        effects: Vec<room::Effect>,
    ) -> Task<Message> {
        let mut tasks = Vec::new();
        for e in effects {
            tasks.push(match e {
                room::Effect::CloseRoom => self.close_room_view(sid, rid),
                room::Effect::Toast(kind, title, body) => self.toast(kind, title, body),
                room::Effect::OpenRoom(rid) => self.open_room(sid, &rid),
                room::Effect::OpenUrl(url) => self.open_room_url(sid, url),
                // Handled by RoomView.
                room::Effect::LoadOlder
                | room::Effect::MarkRead
                | room::Effect::MarkAllRead
                | room::Effect::OpenPanel(_)
                | room::Effect::ClosePanel
                | room::Effect::ShowMessage(_)
                | room::Effect::Compose(_)
                | room::Effect::Omni(_) => Task::none(),
                room::Effect::MarkUnread(mid) => self.mark_unread(sid, rid, mid),
                room::Effect::AskReadAll => self.ask_read_all(),
                room::Effect::Forward(refs) => self.navigate(component::Nav::Forward(refs)),
                room::Effect::ManageMentionGroups => {
                    self.navigate(component::Nav::MentionGroupSettings)
                }
                room::Effect::OpenDrafts => self.open_drafts(sid, false),
                room::Effect::Call(a) => {
                    self.on_calls(crate::videoconf::Message::Action(sid.clone(), a))
                }
                room::Effect::UiKit(m) => self.on_uikit(
                    sid.clone(),
                    crate::uikit::Msg::Ui {
                        rid: Some(rid.to_owned()),
                        msg: *m,
                    },
                ),
                room::Effect::StartDiscussion { pmid } => self.start_discussion(sid, rid, pmid),
                room::Effect::KeepThread { rid, tmid, keep } => {
                    self.keep_thread(sid, &rid, &tmid, keep)
                }
                room::Effect::StarThread { rid, tmid, on } => self.star_thread(sid, rid, tmid, on),
                room::Effect::SetFlag(edit) => self.set_flag(sid, edit),
                room::Effect::Later(edit) => self.set_later(sid, edit),
                room::Effect::Reminder(edit) => self.set_reminder(sid, edit),
                room::Effect::E2e(req) => self.on_e2e_room(sid, req),
                room::Effect::RecentEmoji(key) => {
                    self.config.push_recent_emoji(&key);
                    crate::room::emoji_picker::set_recent(&self.config.recent_emoji);
                    self.save_config()
                }
            });
        }
        Task::batch(tasks)
    }

    /// "Keep in sidebar" / "Remove from sidebar": edits the server's kept
    /// threads (deduped and capped) and saves only on a change; the next
    /// `after_update` mirrors the list into the workspace.
    fn keep_thread(&mut self, sid: &ServerId, rid: &str, tmid: &str, keep: bool) -> Task<Message> {
        let changed = self
            .config
            .server_mut(sid)
            .is_some_and(|e| e.set_thread_kept(rid, tmid, keep));
        if changed {
            self.save_config()
        } else {
            Task::none()
        }
    }

    /// Stars or unstars the parent of thread `tmid`: the sidebar shows the
    /// change right away, the realtime echo confirms it, a failure reverts it.
    fn star_thread(
        &mut self,
        sid: &ServerId,
        rid: String,
        tmid: String,
        on: bool,
    ) -> Task<Message> {
        let ts = self
            .servers
            .get(sid)
            .and_then(|s| super::later::message_ts(&s.workspace, &rid, &tmid));
        let later = self.later_starred(sid, &rid, &tmid, ts, on);
        Task::batch([later, self.star_thread_call(sid, rid, tmid, on, false)])
    }

    /// [`Self::star_thread`]; `queued`: for "Move kept threads to
    /// Favorites" (`star_kept`), whose failures are reported together.
    pub(super) fn star_thread_call(
        &mut self,
        sid: &ServerId,
        rid: String,
        tmid: String,
        on: bool,
        queued: bool,
    ) -> Task<Message> {
        let Some(s) = self.servers.get_mut(sid) else {
            return Task::none();
        };
        let Some(rest) = s.session.as_ref().map(|h| h.rest().clone()) else {
            return Task::none();
        };
        s.workspace.set_thread_starred(&rid, &tmid, on);
        let (sid, epoch) = (sid.clone(), s.generation);
        let call_tmid = tmid.clone();
        Task::perform(
            async move {
                if on {
                    rest.star_message(&call_tmid).await
                } else {
                    rest.unstar_message(&call_tmid).await
                }
                .map_err(Failure::from)
            },
            move |r| Message::ThreadStarred {
                sid: sid.clone(),
                epoch,
                rid: rid.clone(),
                tmid: tmid.clone(),
                on,
                queued,
                result: r,
            },
        )
    }

    /// "Mark unread": leaves the room first (like the web client), so the
    /// automatic read mark of the open room cannot undo it, then calls
    /// `subscriptions.unread`.
    fn mark_unread(&mut self, sid: &ServerId, rid: &str, mid: String) -> Task<Message> {
        let close = self.close_room_view(sid, rid);
        let Some(rest) = self
            .servers
            .get(sid)
            .and_then(|s| s.session.as_ref())
            .map(|h| h.rest().clone())
        else {
            return close;
        };
        let (sid, rid) = (sid.clone(), rid.to_owned());
        let call = Task::perform(
            async move {
                rest.subscriptions_unread(&rid, Some(&mid))
                    .await
                    .map_err(Failure::from)
            },
            move |r| Message::MarkedUnread(sid.clone(), r),
        );
        Task::batch([close, call])
    }

    /// The "create discussion" dialog with `rid` as the parent room
    /// (optionally from message `pmid`).
    fn start_discussion(
        &mut self,
        sid: &ServerId,
        rid: &str,
        pmid: Option<String>,
    ) -> Task<Message> {
        self.go(crate::message::Go::Discussion {
            sid: sid.clone(),
            rid: rid.to_owned(),
            pmid,
        })
    }

    /// Hands a dropped file to the shown room (its open thread, else the
    /// room composer). Nothing happens while no room is shown (another
    /// page, a view in place of the room area or a dialog is on screen).
    pub(super) fn route_file_drop(&mut self, path: std::path::PathBuf) -> Task<Message> {
        if !matches!(self.screen, Screen::Server) || !self.modals.is_empty() {
            return Task::none();
        }
        let Some(sid) = self.active.clone() else {
            return Task::none();
        };
        let Some(s) = self.servers.get(&sid) else {
            return Task::none();
        };
        if s.room_area_taken() {
            return Task::none();
        }
        let Some(rid) = s.active_room.clone().filter(|r| s.rooms.contains_key(r)) else {
            return Task::none();
        };
        Task::done(Message::Room(
            sid,
            s.generation,
            rid,
            room::Message::FilesDropped(vec![path]),
        ))
    }

    /// Video-call messages (`crate::videoconf`) and their effects.
    pub(super) fn on_calls(&mut self, m: crate::videoconf::Message) -> Task<Message> {
        let (task, effects) = crate::videoconf::update(self, m);
        self.call_effects(task, effects)
    }

    pub(super) fn call_effects(
        &mut self,
        task: Task<Message>,
        effects: Vec<(ServerId, component::Effect)>,
    ) -> Task<Message> {
        let mut tasks = vec![task];
        for (sid, e) in effects {
            tasks.push(self.component_effects(Some(&sid), components::Source::StatusMenu, vec![e]));
        }
        Task::batch(tasks)
    }
}
