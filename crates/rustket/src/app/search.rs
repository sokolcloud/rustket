//! App wiring of the search view (`crate::search`): opening it in place of
//! the room area, the provider lookup, starting runs with their local hits
//! (loaded timelines, offline cache), the paced server requests and
//! opening a result.

use std::sync::Arc;
use std::time::Instant;

use chrono::Offset;
use iced::{Element, Task};
use rocketchat::models::Message as RcMessage;
use rustket_core::ServerId;
use rustket_core::search::{
    Backend, GLOBAL_LIMIT, LocalMatcher, MatchCtx, Next, PER_ROOM_LIMIT, SearchRun, Target,
    resolve_rooms, rooms_to_search,
};
use rustket_core::workspace::ConnectionState;

use super::{App, Screen};
use crate::error::Failure;
use crate::message::Message;
use crate::search::{LOCAL_LIMIT, Msg, Out, Provider, QUERY_ID, SearchView};
use crate::workspace::{MainView, ServerState};

fn online(s: &ServerState) -> bool {
    s.session.is_some() && *s.workspace.connection() == ConnectionState::Connected
}

/// The backend for the provider the server reported (`None`: not known
/// yet).
fn backend(p: &Provider) -> Option<Backend> {
    match p {
        Provider::Known(Some(p)) if p.global_search() => Some(Backend::Global),
        Provider::Known(Some(p)) if p.is_default() => Some(Backend::Provider),
        // Another provider, none, or the lookup failed: `chat.search`
        // parses the default operators whatever is active.
        Provider::Known(_) | Provider::Failed => Some(Backend::ChatSearch),
        Provider::Unknown | Provider::Loading => None,
    }
}

impl App {
    /// Opens the search view of server `sid` (with the query searched
    /// last). `toggle`: an open search view closes instead.
    pub(super) fn open_search(&mut self, sid: &ServerId, toggle: bool) -> Task<Message> {
        let Some(s) = self.servers.get_mut(sid) else {
            return Task::none();
        };
        if s.login.is_some() || !s.is_logged_in() || s.drafts_dialog_open() {
            return Task::none();
        }
        if toggle && s.search_view().is_some() {
            s.close_main_view();
            return Task::none();
        }
        s.omni.close_contact_center();
        s.status_menu.open = false;
        let focus = iced::widget::operation::focus(QUERY_ID);
        if s.search_view().is_some() {
            return focus;
        }
        s.close_main_view();
        let query = s.search.last_query.clone();
        s.main_view = Some(MainView::Search(Box::new(SearchView::new(query))));
        let provider = self.ensure_provider(sid);
        let start = self.start_search(sid);
        Task::batch([focus, provider, start])
    }

    /// Ctrl/Cmd+Shift+F: the active server's search view, toggled.
    pub(super) fn search_shortcut(&mut self) -> Task<Message> {
        if !matches!(self.screen, Screen::Server) || self.overlay().0.is_some() {
            return Task::none();
        }
        let Some(sid) = self.active.clone() else {
            return Task::none();
        };
        self.open_search(&sid, true)
    }

    /// Asks the server for its search provider once per session.
    fn ensure_provider(&mut self, sid: &ServerId) -> Task<Message> {
        let Some(s) = self.servers.get_mut(sid) else {
            return Task::none();
        };
        let Some(h) = s.session.as_ref() else {
            return Task::none();
        };
        if s.search.provider != Provider::Unknown {
            return Task::none();
        }
        s.search.provider = Provider::Loading;
        let rest = h.rest().clone();
        let (tsid, epoch) = (sid.clone(), s.generation);
        Task::perform(
            async move { rest.search_provider().await.map_err(Failure::from) },
            move |r| Message::Search(tsid.clone(), epoch, Msg::Provider(r)),
        )
    }

    pub(super) fn on_search(&mut self, sid: ServerId, epoch: u64, m: Msg) -> Task<Message> {
        let Some(s) = self.servers.get_mut(&sid).filter(|s| s.is_current(epoch)) else {
            return Task::none();
        };
        match m {
            Msg::Provider(r) => {
                s.search.provider = match r {
                    Ok(p) => Provider::Known(p),
                    Err(e) => {
                        tracing::debug!(error = %e, "search provider lookup failed");
                        Provider::Failed
                    }
                };
                Task::none()
            }
            Msg::Fetched { run, target, res } => {
                let Some(r) = s
                    .main_view
                    .as_mut()
                    .and_then(MainView::search_mut)
                    .and_then(|v| v.run.as_mut())
                    .filter(|r| r.id == run)
                else {
                    return Task::none();
                };
                let res = res.map_err(|f| {
                    tracing::debug!(error = %f, "search request failed");
                    super::inbox::load_failure(&f)
                });
                r.finish(&target, res, Instant::now());
                Task::none()
            }
            Msg::Cached { run, found } => {
                if let Some(v) = s.main_view.as_mut().and_then(MainView::search_mut)
                    && let Some(r) = v.run.as_mut().filter(|r| r.id == run)
                {
                    r.merge_local(found);
                    v.cache_pending = false;
                }
                Task::none()
            }
            Msg::Pace => {
                s.search.timer = None;
                Task::none()
            }
            m => {
                let Some(view) = s.main_view.as_mut().and_then(MainView::search_mut) else {
                    return Task::none();
                };
                let outs = view.update(m, &s.workspace);
                self.search_outs(&sid, epoch, outs)
            }
        }
    }

    fn search_outs(&mut self, sid: &ServerId, epoch: u64, outs: Vec<Out>) -> Task<Message> {
        let mut tasks = Vec::new();
        for out in outs {
            tasks.push(match out {
                Out::Close => {
                    if let Some(s) = self.servers.get_mut(sid) {
                        s.close_main_view();
                    }
                    Task::none()
                }
                Out::Start => self.start_search(sid),
                Out::Debounce(seq) => {
                    let sid = sid.clone();
                    Task::perform(tokio::time::sleep(crate::search::DEBOUNCE), move |()| {
                        Message::Search(sid.clone(), epoch, Msg::Debounced(seq))
                    })
                }
                Out::Focus(id) => iced::widget::operation::focus(id),
                Out::Open(m) => self.open_search_hit(sid, *m),
            });
        }
        Task::batch(tasks)
    }

    /// Opens a result: its thread for a reply, else the room at the
    /// message.
    fn open_search_hit(&mut self, sid: &ServerId, m: RcMessage) -> Task<Message> {
        let rid = m.rid.clone();
        match m.tmid.clone() {
            Some(tmid) => self.go(crate::message::Go::Room {
                sid: sid.clone(),
                rid,
                tmid: Some(tmid),
            }),
            None => {
                let open = self.open_room(sid, &rid);
                let show = self.room_show(sid, &rid, m);
                Task::batch([open, show])
            }
        }
    }

    /// Starts a run for the view's query (the previous run's answers are
    /// dropped): local hits at once, the offline cache's when read, the
    /// server's as the paced requests come back. Waits for the provider
    /// when it is not known yet.
    fn start_search(&mut self, sid: &ServerId) -> Task<Message> {
        let Some(s) = self.servers.get_mut(sid) else {
            return Task::none();
        };
        let session = s.session.is_some();
        let known = backend(&s.search.provider);
        let Some(view) = s.main_view.as_mut().and_then(MainView::search_mut) else {
            return Task::none();
        };
        let q = view.parsed();
        if q.is_empty() {
            view.set_run(None);
            view.cache_pending = false;
            return Task::none();
        }
        let backend = match known {
            Some(b) => b,
            // Offline: the local hits now, the server once connected.
            None if !session => Backend::Provider,
            None => {
                view.set_run(None);
                view.pending_start = true;
                return Task::none();
            }
        };
        let id = s.search.next_run;
        s.search.next_run += 1;
        s.search.timer = None;
        let ws = &s.workspace;
        let only = (!q.rooms.is_empty()).then(|| resolve_rooms(ws, &q.rooms));
        let (plain, encrypted) = rooms_to_search(ws, only.as_ref());
        let mut run = SearchRun::new(
            id,
            q.server_query(),
            backend,
            plain,
            encrypted.len(),
            only.is_some(),
        );
        let ctx = MatchCtx {
            user_id: ws.user_id().unwrap_or_default().to_owned(),
            username: ws.username().unwrap_or_default().to_owned(),
            offset: chrono::Local::now().offset().fix(),
        };
        let matcher = LocalMatcher::new(&q, &ctx);
        let mut cache_task = Task::none();
        let mut cache_pending = false;
        if let Some(matcher) = matcher {
            // Loaded timelines: decrypted messages of encrypted rooms are
            // searched here, in memory only.
            let found: Vec<RcMessage> = ws
                .loaded_messages()
                .filter(|m| only.as_ref().is_none_or(|o| o.contains(&m.rid)))
                .filter(|m| !crate::e2e::shown::undecrypted(m))
                .filter(|m| matcher.matches(m))
                .cloned()
                .collect();
            run.merge_local(found);
            if let Some(h) = s.cache.handle().cloned() {
                cache_pending = true;
                let (tsid, epoch) = (sid.clone(), s.generation);
                cache_task = Task::perform(
                    async move { h.search(matcher, only, LOCAL_LIMIT).await },
                    move |found| {
                        Message::Search(tsid.clone(), epoch, Msg::Cached { run: id, found })
                    },
                );
            }
        }
        let Some(view) = s.main_view.as_mut().and_then(MainView::search_mut) else {
            return Task::none();
        };
        view.set_run(Some(run));
        view.cache_pending = cache_pending;
        Task::batch([cache_task, self.tick_search(sid)])
    }

    /// Per update: a new session restarts the search; the open run starts
    /// its next requests (or arms the pacing timer).
    pub(super) fn tick_search(&mut self, sid: &ServerId) -> Task<Message> {
        let Some(s) = self.servers.get_mut(sid) else {
            return Task::none();
        };
        let ptr = s.session.as_ref().map(|h| Arc::as_ptr(&h.0) as usize);
        let renewed = s.search.session != ptr;
        if renewed {
            s.search.session = ptr;
            s.search.provider = Provider::Unknown;
            s.search.timer = None;
        }
        let Some(view) = s.main_view.as_ref().and_then(MainView::search) else {
            return Task::none();
        };
        let restart = (renewed && view.run.is_some())
            || (view.pending_start && backend(&s.search.provider).is_some());
        let provider = if s.search.provider == Provider::Unknown {
            self.ensure_provider(sid)
        } else {
            Task::none()
        };
        if restart {
            return Task::batch([provider, self.start_search(sid)]);
        }
        let Some(s) = self.servers.get_mut(sid) else {
            return provider;
        };
        if !online(s) {
            return provider;
        }
        let Some(h) = s.session.as_ref() else {
            return provider;
        };
        let Some(run) = s
            .main_view
            .as_mut()
            .and_then(MainView::search_mut)
            .and_then(|v| v.run.as_mut())
        else {
            return provider;
        };
        let now = Instant::now();
        let (epoch, mut tasks) = (s.generation, vec![provider]);
        loop {
            match run.next(now) {
                Next::Fetch(target) => {
                    let rest = h.rest().clone();
                    let (q, b, id) = (run.server_query.clone(), run.backend, run.id);
                    let tsid = sid.clone();
                    let t2 = target.clone();
                    tasks.push(Task::perform(
                        async move {
                            match (&t2, b) {
                                (Target::All, _) => {
                                    rest.provider_search(&q, None, GLOBAL_LIMIT).await
                                }
                                (Target::Room(r), Backend::ChatSearch) => {
                                    rest.search_messages(r, &q, 0, u64::from(PER_ROOM_LIMIT))
                                        .await
                                }
                                (Target::Room(r), _) => {
                                    rest.provider_search(&q, Some(r), PER_ROOM_LIMIT).await
                                }
                            }
                            .map_err(Failure::from)
                        },
                        move |res| {
                            Message::Search(
                                tsid.clone(),
                                epoch,
                                Msg::Fetched {
                                    run: id,
                                    target: target.clone(),
                                    res,
                                },
                            )
                        },
                    ));
                }
                Next::Wait(at) => {
                    if s.search.timer.is_none_or(|t| t <= now || t > at) {
                        s.search.timer = Some(at);
                        let tsid = sid.clone();
                        tasks.push(Task::perform(
                            tokio::time::sleep(at.saturating_duration_since(now)),
                            move |()| Message::Search(tsid.clone(), epoch, Msg::Pace),
                        ));
                    }
                    break;
                }
                Next::Idle => break,
            }
        }
        Task::batch(tasks)
    }

    /// The open search view, shown instead of the room area.
    pub(super) fn search_screen<'a>(&'a self, s: &'a ServerState) -> Option<Element<'a, Message>> {
        let view = s.search_view()?;
        let ctx = crate::search::view::Ctx {
            server_id: &s.id,
            server: &s.url,
            workspace: &s.workspace,
            images: &self.images,
            prefs: &self.config.ui,
            state: &s.search,
            online: online(s),
            now: Instant::now(),
            local_now: chrono::Local::now(),
        };
        let (sid, epoch) = (s.id.clone(), s.generation);
        crate::room::render::set_mode(self.mode());
        Some(
            view.view(&ctx)
                .map(move |m| Message::Search(sid.clone(), epoch, m)),
        )
    }
}
