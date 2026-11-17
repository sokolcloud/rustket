//! App wiring of the inbox views (`crate::inbox`): opening and closing them
//! in place of the room area, the paced feed loader, read marks through the
//! read queue, and navigation into rooms.

use std::sync::Arc;
use std::time::Instant;

use iced::{Element, Task};
use rocketchat::rest::HistoryOptions;
use rust_i18n::t;
use rustket_core::inbox::feeds::FEED_PAGE;
use rustket_core::inbox::load::{self, Fetch, LoadFailure, Next};
use rustket_core::reads::ReadReq;
use rustket_core::{Dirty, ServerId};
use rustket_ui::widget::BannerKind;

use super::{App, Screen};
use crate::error::Failure;
use crate::inbox::{CatchUpKey, InboxKind, InboxScreen, Msg, Out};
use crate::message::Message;
use crate::reads::Origin;
use crate::room::{self, UpdateCtx};
use crate::workspace::threads::{Class, classify};
use crate::workspace::{MainView, ServerState};

/// Mentions fetched per room (`chat.getMentionedMessages`).
const MENTIONS_COUNT: u64 = 50;

/// How a failed fetch counts for the loader's pacing.
pub(super) fn load_failure(f: &Failure) -> LoadFailure {
    match classify(f) {
        Class::RateLimited(d) => LoadFailure::RateLimited(d),
        Class::Transient => LoadFailure::Transient,
        Class::NotAllowed => LoadFailure::NotAllowed,
        Class::NotFound | Class::Other => LoadFailure::Other,
    }
}

/// Effects a room view handles itself (they need the room open).
pub(super) fn room_local(e: &room::Effect) -> bool {
    matches!(
        e,
        room::Effect::LoadOlder
            | room::Effect::MarkRead
            | room::Effect::MarkAllRead
            | room::Effect::OpenPanel(_)
            | room::Effect::ClosePanel
            | room::Effect::ShowMessage(_)
            | room::Effect::Compose(_)
            | room::Effect::Omni(_)
    )
}

impl App {
    /// Opens inbox view `kind` of server `sid` in place of the room area
    /// (or switches the open one to it). `toggle`: the view already shown
    /// closes instead (its shortcut pressed again).
    pub(super) fn open_inbox(
        &mut self,
        sid: &ServerId,
        kind: InboxKind,
        toggle: bool,
    ) -> Task<Message> {
        let include_muted = self.config.ui.inbox_include_muted;
        let Some(s) = self.servers.get_mut(sid) else {
            return Task::none();
        };
        if s.login.is_some() || !s.is_logged_in() || s.drafts_dialog_open() {
            return Task::none();
        }
        if toggle && s.inbox_view().is_some_and(|i| i.shows(kind)) {
            s.close_main_view();
            return Task::none();
        }
        s.omni.close_contact_center();
        s.status_menu.open = false;
        if s.inbox_view().is_some_and(|i| i.kind() == kind) {
            return Task::none();
        }
        let online = s.session.is_some();
        if !matches!(s.main_view, Some(MainView::Inbox(_))) {
            s.show_main_view(MainView::Inbox(InboxScreen::new(InboxKind::Unreads)));
        }
        // What the loader wants changes with the view: plan afresh.
        s.inbox.plan_key = None;
        if let Some(screen) = s.main_view.as_mut().and_then(MainView::inbox_mut) {
            let _ = screen.update(
                Msg::Open(kind),
                &s.workspace,
                &mut s.inbox,
                include_muted,
                online,
            );
        }
        Task::none()
    }

    /// Ctrl/Cmd+Shift+A / M: the active server's view, toggled.
    pub(super) fn inbox_shortcut(&mut self, kind: InboxKind) -> Task<Message> {
        if !matches!(self.screen, Screen::Server) || self.overlay().0.is_some() {
            return Task::none();
        }
        let Some(sid) = self.active.clone() else {
            return Task::none();
        };
        self.open_inbox(&sid, kind, true)
    }

    /// Enter / Left / Right nothing else took: the catch-up view's keys.
    pub(super) fn on_catch_up_key(&mut self, k: CatchUpKey) -> Task<Message> {
        if !matches!(self.screen, Screen::Server) || self.overlay().0.is_some() {
            return Task::none();
        }
        let Some(s) = self.active.as_ref().and_then(|sid| self.servers.get(sid)) else {
            return Task::none();
        };
        if s.later_view().is_some() && k == CatchUpKey::Enter {
            return self.on_later_key(crate::later::LaterKey::Enter);
        }
        if !s.inbox_view().is_some_and(InboxScreen::is_catch_up) {
            return Task::none();
        }
        let (sid, epoch) = (s.id.clone(), s.generation);
        self.on_inbox(sid, epoch, Msg::Key(k))
    }

    pub(super) fn on_inbox(&mut self, sid: ServerId, epoch: u64, m: Msg) -> Task<Message> {
        let include_muted = self.config.ui.inbox_include_muted;
        let Some(s) = self.servers.get_mut(&sid).filter(|s| s.is_current(epoch)) else {
            return Task::none();
        };
        let now = Instant::now();
        match m {
            Msg::FeedDone { fetch, req, res } => {
                s.inbox.plan_key = None;
                let failure = res.as_ref().err().map(load_failure);
                s.inbox
                    .load
                    .finish(&fetch, failure.map_or(Ok(()), Err), now);
                match res {
                    Ok(h) => s.workspace.finish_feed(&req, Ok(h)),
                    Err(f) => {
                        tracing::debug!(error = %f, "inbox feed page failed");
                        let given_up = s.inbox.load.is_given_up(&fetch);
                        s.workspace
                            .finish_feed(&req, Err(given_up.then(|| f.user_text())));
                    }
                }
                if let (Fetch::Feed { rid, earlier: true }, Some(screen)) =
                    (&fetch, s.main_view.as_mut().and_then(MainView::inbox_mut))
                {
                    screen.earlier_done(rid);
                }
                Task::none()
            }
            Msg::MentionsDone { fetch, res } => {
                s.inbox.plan_key = None;
                let failure = res.as_ref().err().map(load_failure);
                s.inbox
                    .load
                    .finish(&fetch, failure.map_or(Ok(()), Err), now);
                let res = match res {
                    Ok(page) => Ok(page.items),
                    Err(f) => {
                        tracing::debug!(error = %f, "inbox thread mentions failed");
                        Err(s.inbox.load.is_given_up(&fetch).then(|| f.user_text()))
                    }
                };
                s.workspace.finish_thread_mentions(fetch.rid(), res);
                Task::none()
            }
            Msg::Pace => {
                s.inbox.timer = None;
                s.inbox.plan_key = None;
                Task::none()
            }
            Msg::Retry(rid) => {
                s.inbox.load.forgive(&rid);
                s.workspace.reset_feed(&rid);
                Task::none()
            }
            Msg::View { rid, m } => self.inbox_viewer(&sid, rid, m),
            Msg::User(m) => self.hosted_user(&sid, super::hosted_user::Host::Inbox, m),
            m => {
                let online = s.session.is_some()
                    && *s.workspace.connection()
                        == rustket_core::workspace::ConnectionState::Connected;
                let Some(screen) = s.main_view.as_mut().and_then(MainView::inbox_mut) else {
                    return Task::none();
                };
                let outs = screen.update(m, &s.workspace, &mut s.inbox, include_muted, online);
                self.inbox_outs(&sid, outs)
            }
        }
    }

    /// A message body's interaction in the inbox: the shared viewer state,
    /// its effects routed like the room's.
    fn inbox_viewer(
        &mut self,
        sid: &ServerId,
        rid: String,
        m: crate::room::timeline::ViewerMsg,
    ) -> Task<Message> {
        let Some(s) = self.servers.get_mut(sid) else {
            return Task::none();
        };
        let epoch = s.generation;
        let Some(screen) = s.main_view.as_mut().and_then(MainView::inbox_mut) else {
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
        let action = screen.viewer.update(m, &mut ctx);
        screen.image_rid = screen
            .viewer
            .image_open()
            .then(|| {
                if opens {
                    Some(rid.clone())
                } else {
                    screen.image_rid.take()
                }
            })
            .flatten();
        let (tsid, trid) = (sid.clone(), rid.clone());
        let task = action.task.map(move |m| {
            Message::Inbox(
                tsid.clone(),
                epoch,
                Msg::View {
                    rid: trid.clone(),
                    m,
                },
            )
        });
        let effects = self.inbox_outs(
            sid,
            vec![Out::Effects {
                rid,
                effects: action.effects,
            }],
        );
        Task::batch([task, effects])
    }

    /// Carries out what the inbox asked for.
    fn inbox_outs(&mut self, sid: &ServerId, outs: Vec<Out>) -> Task<Message> {
        let mut tasks = Vec::new();
        let mut reads = false;
        for out in outs {
            tasks.push(match out {
                Out::Close => {
                    if let Some(s) = self.servers.get_mut(sid) {
                        s.close_main_view();
                    }
                    Task::none()
                }
                Out::OpenRoom(rid) => self.open_room(sid, &rid),
                Out::Reveal { rid, m } => {
                    let open = self.open_room(sid, &rid);
                    let show = self.room_show(sid, &rid, *m);
                    Task::batch([open, show])
                }
                Out::OpenThread { rid, tmid } => self.go(crate::message::Go::Room {
                    sid: sid.clone(),
                    rid,
                    tmid: Some(tmid),
                }),
                Out::Compose { rid, intent } => {
                    let open = self.open_room(sid, &rid);
                    let compose = self.room_compose(sid, &rid, intent);
                    Task::batch([open, compose])
                }
                Out::Read(req) => {
                    if let Some(s) = self.servers.get_mut(sid) {
                        s.reads.push(req, Origin::Inbox);
                        reads = true;
                    }
                    Task::none()
                }
                Out::SetIncludeMuted(on) => {
                    self.config.ui.inbox_include_muted = on;
                    self.save_config()
                }
                Out::Effects { rid, effects } => {
                    self.list_effects(sid, &rid, effects, super::hosted_user::Host::Inbox)
                }
            });
        }
        if reads {
            tasks.push(self.pump_reads(sid));
        }
        Task::batch(tasks)
    }

    /// Shows message `m` in the open view of `rid` from a local copy.
    pub(super) fn room_show(
        &mut self,
        sid: &ServerId,
        rid: &str,
        m: rocketchat::models::Message,
    ) -> Task<Message> {
        self.room_run(sid, rid, |rv, ctx| rv.show(m, ctx))
    }

    pub(super) fn room_run_effects(
        &mut self,
        sid: &ServerId,
        rid: &str,
        effects: Vec<room::Effect>,
    ) -> Task<Message> {
        self.room_run(sid, rid, |rv, ctx| rv.run_effects(effects, ctx))
    }

    /// Runs `f` on the open view of `rid` and handles its effects.
    fn room_run(
        &mut self,
        sid: &ServerId,
        rid: &str,
        f: impl FnOnce(&mut room::RoomView, &mut UpdateCtx<'_>) -> room::Action<room::Message>,
    ) -> Task<Message> {
        let Some(s) = self.servers.get_mut(sid) else {
            return Task::none();
        };
        let epoch = s.generation;
        let Some(rv) = s.rooms.get_mut(rid) else {
            return Task::none();
        };
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
        Task::batch([
            Self::room_task(sid, epoch, rid, action.task),
            self.handle_effects(sid, rid, action.effects),
        ])
    }

    /// A read mark the inbox queued ended: the item shows again on failure.
    pub(super) fn inbox_read_done(
        &mut self,
        sid: &ServerId,
        req: &ReadReq,
        ok: bool,
    ) -> Task<Message> {
        let Some(s) = self.servers.get_mut(sid) else {
            return Task::none();
        };
        if let Some(screen) = s.main_view.as_mut().and_then(MainView::inbox_mut) {
            screen.read_done(req, ok, Instant::now());
        }
        if ok {
            return Task::none();
        }
        let real = s.workspace.use_real_names(self.config.ui.use_real_names);
        let room = s
            .workspace
            .room(req.rid())
            .map(|e| rustket_core::room_list::display_name(&s.workspace, e, real))
            .unwrap_or_default();
        self.toast(
            BannerKind::Error,
            t!("inbox.mark_read_failed", room = room),
            String::new(),
        )
    }

    /// Per update: the open inbox view follows the workspace and its loader
    /// starts the next fetch (or arms the pacing timer).
    pub(super) fn tick_inbox(&mut self, sid: &ServerId) -> Task<Message> {
        let include_muted = self.config.ui.inbox_include_muted;
        let Some(s) = self.servers.get_mut(sid) else {
            return Task::none();
        };
        let Some(screen) = s.main_view.as_mut().and_then(MainView::inbox_mut) else {
            return Task::none();
        };
        let ptr = s.session.as_ref().map(|h| Arc::as_ptr(&h.0) as usize);
        if s.inbox.session != ptr {
            s.inbox.load.reset();
            s.inbox.timer = None;
            s.inbox.plan_key = None;
            s.inbox.session = ptr;
        }
        let now = Instant::now();
        screen.tick(&s.workspace, &s.inbox, include_muted, now);
        let Some(h) = s.session.as_ref() else {
            return Task::none();
        };
        if s.inbox.load.is_busy() {
            return Task::none();
        }
        let ws = &s.workspace;
        let key = [
            ws.revision(Dirty::ROOMS),
            ws.revision(Dirty::FEEDS),
            ws.revision(Dirty::CONNECTION),
            ws.revision(Dirty::SYNC),
            screen.revision,
        ];
        if s.inbox.plan_key == Some(key) && s.inbox.timer.is_none_or(|t| t > now) {
            return Task::none();
        }
        s.inbox.plan_key = Some(key);
        let wanted = screen.wanted(&s.workspace, &s.inbox, include_muted);
        s.workspace
            .keep_feeds(wanted.feeds.iter().chain(&wanted.earlier));
        let epoch = s.generation;
        let wrap = {
            let sid = sid.clone();
            move |m: Msg| Message::Inbox(sid.clone(), epoch, m)
        };
        match load::plan(&s.workspace, &wanted, &s.inbox.load, now) {
            Next::Idle => Task::none(),
            Next::Wait(at) => {
                if s.inbox.timer.is_some_and(|t| t <= at && t > now) {
                    return Task::none();
                }
                s.inbox.timer = Some(at);
                Task::perform(
                    tokio::time::sleep(at.saturating_duration_since(now)),
                    |()| Msg::Pace,
                )
                .map(wrap)
            }
            Next::Fetch(fetch) => {
                let rest = h.rest().clone();
                match &fetch {
                    Fetch::Feed { rid, earlier } => {
                        let Some(req) = s.workspace.begin_feed(rid, *earlier) else {
                            return Task::none();
                        };
                        s.inbox.load.begin(&fetch, now);
                        let opts = HistoryOptions {
                            before: req.before,
                            limit: FEED_PAGE,
                            last_seen: req.last_seen,
                            show_thread_messages: false,
                        };
                        let call_rid = req.rid.clone();
                        Task::perform(
                            async move {
                                rest.load_history_with(&call_rid, &opts)
                                    .await
                                    .map_err(Failure::from)
                            },
                            move |res| Msg::FeedDone {
                                fetch: fetch.clone(),
                                req: req.clone(),
                                res,
                            },
                        )
                        .map(wrap)
                    }
                    Fetch::ThreadMentions { rid } => {
                        if !s.workspace.begin_thread_mentions(rid) {
                            return Task::none();
                        }
                        s.inbox.load.begin(&fetch, now);
                        let call_rid = rid.clone();
                        Task::perform(
                            async move {
                                rest.get_mentioned(&call_rid, 0, MENTIONS_COUNT)
                                    .await
                                    .map_err(Failure::from)
                            },
                            move |res| Msg::MentionsDone {
                                fetch: fetch.clone(),
                                res,
                            },
                        )
                        .map(wrap)
                    }
                }
            }
        }
    }

    /// The open inbox view, shown instead of the room area.
    pub(super) fn inbox_screen<'a>(&'a self, s: &'a ServerState) -> Option<Element<'a, Message>> {
        let screen = s.inbox_view()?;
        let ctx = crate::inbox::view::Ctx {
            server_id: &s.id,
            server: &s.url,
            workspace: &s.workspace,
            images: &self.images,
            prefs: &self.config.ui,
            state: &s.inbox,
            online: s.session.is_some()
                && *s.workspace.connection() == rustket_core::workspace::ConnectionState::Connected,
            now: Instant::now(),
        };
        let (sid, epoch) = (s.id.clone(), s.generation);
        crate::room::render::set_mode(self.mode());
        Some(
            screen
                .view(&ctx)
                .map(move |m| Message::Inbox(sid.clone(), epoch, m)),
        )
    }
}
