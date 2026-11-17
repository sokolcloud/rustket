//! Per-server state and the workspace layout (rail + sidebar + room area).

pub mod flag_menu;
pub mod flags;
pub mod later_sync;
pub mod mention_groups;
pub mod rail;
pub mod service_sync;
pub mod sidebar;
pub mod status_menu;
pub mod threads;
pub mod titlebar;

use std::collections::HashMap;

use rocketchat::tls::{Fingerprint, parse_fingerprint};
use rocketchat::{Credentials, ServerUrl, TlsContext};
use rustket_core::config::ServerEntry;
use rustket_core::{ServerId, Workspace};

use crate::room::RoomView;
use crate::screens::login::Login;
use crate::session::{SessionHandle, SessionSpec, tls_for};

/// Each synced key's part of a combined push ([`ServerState::service_pushed`]).
pub struct ServicePushed {
    pub flags: Option<flags::Msg>,
    pub mention_groups: Option<mention_groups::Msg>,
    pub later: Option<later_sync::LaterMsg>,
    pub reminders: Option<later_sync::ReminderMsg>,
}

/// A view shown in place of the room area (the active room stays open
/// behind it). Opening one closes the other and the contact center;
/// opening a room closes it.
#[derive(Debug)]
pub enum MainView {
    /// All unreads, Catch up or Activity (`crate::inbox`).
    Inbox(crate::inbox::InboxScreen),
    /// Starred messages and reminders (`crate::later`).
    Later(crate::later::LaterView),
    /// Scheduled messages and drafts (`crate::drafts`).
    Drafts(crate::drafts::DraftsView),
    /// Message search across the rooms (`crate::search`).
    Search(Box<crate::search::SearchView>),
}

impl MainView {
    pub fn inbox(&self) -> Option<&crate::inbox::InboxScreen> {
        match self {
            MainView::Inbox(i) => Some(i),
            _ => None,
        }
    }

    pub fn inbox_mut(&mut self) -> Option<&mut crate::inbox::InboxScreen> {
        match self {
            MainView::Inbox(i) => Some(i),
            _ => None,
        }
    }

    pub fn later(&self) -> Option<&crate::later::LaterView> {
        match self {
            MainView::Later(l) => Some(l),
            _ => None,
        }
    }

    pub fn later_mut(&mut self) -> Option<&mut crate::later::LaterView> {
        match self {
            MainView::Later(l) => Some(l),
            _ => None,
        }
    }

    pub fn drafts(&self) -> Option<&crate::drafts::DraftsView> {
        match self {
            MainView::Drafts(d) => Some(d),
            _ => None,
        }
    }

    pub fn search(&self) -> Option<&crate::search::SearchView> {
        match self {
            MainView::Search(v) => Some(v),
            _ => None,
        }
    }

    pub fn drafts_mut(&mut self) -> Option<&mut crate::drafts::DraftsView> {
        match self {
            MainView::Drafts(d) => Some(d),
            _ => None,
        }
    }

    pub fn search_mut(&mut self) -> Option<&mut crate::search::SearchView> {
        match self {
            MainView::Search(v) => Some(v),
            _ => None,
        }
    }
}

/// Everything the app keeps for one configured server.
pub struct ServerState {
    pub id: ServerId,
    pub url: ServerUrl,
    pub name: String,
    pub pins: Vec<(String, Fingerprint)>,
    pub tls: TlsContext,
    /// Set while logged in: the session subscription runs for these.
    pub credentials: Option<Credentials>,
    /// Bumped to force a session restart with the same credentials.
    pub generation: u64,
    pub session: Option<SessionHandle>,
    pub workspace: Workspace,
    /// Shown instead of the workspace while logged out.
    pub login: Option<Login>,
    pub sidebar: sidebar::Sidebar,
    pub status_menu: status_menu::StatusMenu,
    /// Omnichannel state and loads (`crate::omnichannel`).
    pub omni: crate::omnichannel::Omni,
    /// Loader of the sidebar's Threads section.
    pub threads: threads::SidebarThreads,
    /// Sync of the account's flags (`workspace::flags`).
    pub flags: flags::FlagSync,
    /// Sync of the account's mention groups (`workspace::mention_groups`).
    pub mention_groups: mention_groups::MentionGroupSync,
    /// Sync of the account's Later state (`workspace::later_sync`).
    pub later: later_sync::LaterSync,
    /// Sync of the account's reminders (`workspace::later_sync`).
    pub reminders: later_sync::ReminderSync,
    /// What the synced keys of the account share: the write lock
    /// (`workspace::service_sync`).
    pub service: service_sync::Shared,
    /// Open room views (currently only the active one is kept open).
    pub rooms: HashMap<String, RoomView>,
    pub active_room: Option<String>,
    pub syncing: bool,
    /// A resync was requested while a sync was running: run once more after it.
    pub resync_pending: bool,
    /// Consecutive failed syncs (backoff) and the last error (banner).
    pub sync_attempt: u32,
    pub sync_error: Option<String>,
    /// A room deep link waiting for the initial load: (rid, path, stamp).
    pub pending_room: Option<(Option<String>, Option<String>, u64)>,
    /// The last session start failed with a non-auth error.
    pub start_error: Option<String>,
    /// `true` until the token was looked up at startup.
    pub restoring: bool,
    /// UIKit widget state that holds iced types (modal editors).
    pub uikit: crate::uikit::Local,
    /// End-to-end encryption of this account.
    pub e2e: crate::e2e::ServerE2e,
    /// Offline cache of this account (`crate::cache`).
    pub cache: crate::cache::ServerCache,
    /// Read marks waiting to be sent (`crate::reads`).
    pub reads: crate::reads::Queue,
    /// The view in place of the room area, if any.
    pub main_view: Option<MainView>,
    /// What the inbox views keep while closed (skipped rooms, loader).
    pub inbox: crate::inbox::InboxState,
    /// What Later keeps while closed (the fetch of reminder copies).
    pub later_state: crate::later::LaterState,
    /// What the search keeps while closed (provider, last query).
    pub search: crate::search::SearchState,
}

impl ServerState {
    pub fn from_entry(entry: &ServerEntry) -> Result<Self, rocketchat::Error> {
        let url = ServerUrl::parse(&entry.url)?;
        let pins: Vec<(String, Fingerprint)> = entry
            .pinned_certificates
            .iter()
            .filter_map(|p| match parse_fingerprint(&p.sha256) {
                Ok(fp) => Some((p.host.clone(), fp)),
                Err(e) => {
                    tracing::warn!(host = %p.host, error = %e, "ignoring invalid certificate pin");
                    None
                }
            })
            .collect();
        let tls = tls_for(&pins)?;
        Ok(Self {
            id: entry.id.clone(),
            name: if entry.display_name.is_empty() {
                url.host()
            } else {
                entry.display_name.clone()
            },
            url,
            pins,
            tls,
            credentials: None,
            generation: 0,
            session: None,
            workspace: Workspace::new(),
            login: None,
            sidebar: sidebar::Sidebar::default(),
            status_menu: status_menu::StatusMenu::new(),
            omni: crate::omnichannel::Omni::default(),
            threads: threads::SidebarThreads::default(),
            flags: flags::FlagSync::default(),
            mention_groups: mention_groups::MentionGroupSync::default(),
            later: later_sync::LaterSync::default(),
            reminders: later_sync::ReminderSync::default(),
            service: service_sync::Shared::default(),
            rooms: HashMap::new(),
            active_room: None,
            syncing: false,
            resync_pending: false,
            sync_attempt: 0,
            sync_error: None,
            pending_room: None,
            start_error: None,
            restoring: true,
            uikit: crate::uikit::Local::default(),
            e2e: Default::default(),
            cache: Default::default(),
            reads: Default::default(),
            main_view: None,
            inbox: Default::default(),
            later_state: Default::default(),
            search: Default::default(),
        })
    }

    pub fn spec(&self) -> Option<SessionSpec> {
        Some(SessionSpec {
            server_id: self.id.clone(),
            url: self.url.clone(),
            credentials: self.credentials.clone()?,
            pins: self.pins.clone(),
            generation: self.generation,
        })
    }

    /// Closes every room view; the returned task stops their typing
    /// indicators (sent through the current session; results are not needed).
    pub fn close_rooms(&mut self) -> iced::Task<crate::room::Message> {
        let rooms: Vec<_> = self.rooms.drain().collect();
        let mut tasks = Vec::new();
        for (_, rv) in rooms {
            tasks.push(rv.close(&mut self.workspace, self.session.as_ref()));
        }
        self.active_room = None;
        iced::Task::batch(tasks)
    }

    /// Drops the session, room views and workspace state (logout). Bumps the
    /// generation so late results of the old session are discarded. Call
    /// [`Self::close_rooms`] first to stop the typing indicators.
    pub fn reset(&mut self) {
        self.generation += 1;
        crate::room::upload::cancel_server(self.id.as_str());
        self.credentials = None;
        let _ = self.close_rooms();
        self.session = None;
        self.e2e.reset();
        self.workspace = Workspace::new();
        self.uikit = crate::uikit::Local::default();
        self.sidebar = sidebar::Sidebar::default();
        self.omni.reset(&self.id);
        self.threads.reset();
        self.flags.reset();
        self.mention_groups.reset();
        self.later.reset();
        self.reminders.reset();
        // `service` stays: a push of the old session may still hold its lock.
        self.reads.clear();
        self.main_view = None;
        self.inbox.reset();
        self.later_state = Default::default();
        // The last query and results belong to the account signed out.
        self.search = Default::default();
        self.syncing = false;
        self.resync_pending = false;
        self.sync_attempt = 0;
        self.sync_error = None;
        self.start_error = None;
    }

    /// The workspace and every synced key of the account. Keys added later
    /// join here (and in [`Self::service_pushed`]'s `finish`es).
    fn service_syncs(
        &mut self,
    ) -> (
        &rustket_core::Workspace,
        [&mut dyn service_sync::Syncing; 4],
    ) {
        (
            &self.workspace,
            [
                &mut self.flags.sync,
                &mut self.mention_groups,
                &mut self.later,
                &mut self.reminders,
            ],
        )
    }

    /// Starts one push of the account's synced keys when one of them is
    /// due (after their `tick`s).
    pub fn start_service_push(
        &mut self,
        now: std::time::Instant,
    ) -> Option<iced::Task<service_sync::Outcome>> {
        self.session.as_ref()?;
        let parts = self.service_parts(now)?;
        let h = self.session.as_ref()?;
        Some(service_sync::push_task(
            &self.workspace,
            h,
            &self.service,
            parts,
        ))
    }

    /// The parts of a push, when one key's time has come.
    pub(crate) fn service_parts(
        &mut self,
        now: std::time::Instant,
    ) -> Option<Vec<service_sync::Outgoing>> {
        let (ws, mut syncs) = self.service_syncs();
        service_sync::take_parts(ws, &mut syncs, now)
    }

    /// Hands a combined push's result to the keys: each key's part, for
    /// its `update`. Every key learns the room the push used and keeps a
    /// rate limit it hit.
    pub fn service_pushed(
        &mut self,
        o: &service_sync::Outcome,
        now: std::time::Instant,
    ) -> ServicePushed {
        let (_, mut syncs) = self.service_syncs();
        service_sync::pause_on_rate_limit(o, &mut syncs, now);
        let flags = self.flags.finish(o);
        let groups = self.mention_groups.finish(o);
        let later = self.later.finish(o);
        let reminders = self.reminders.finish(o);
        if let Some(rid) = o.rid() {
            self.flags.adopt_created(rid);
            self.mention_groups.adopt_created(rid);
            self.later.adopt_created(rid);
            self.reminders.adopt_created(rid);
        }
        ServicePushed {
            flags: flags.map(|(epoch, res)| flags::Msg::Pushed { epoch, res }),
            mention_groups: groups.map(|(epoch, res)| service_sync::Msg::Pushed { epoch, res }),
            later: later.map(|(epoch, res)| service_sync::Msg::Pushed { epoch, res }),
            reminders: reminders.map(|(epoch, res)| service_sync::Msg::Pushed { epoch, res }),
        }
    }

    /// Whether a result tagged with `epoch` belongs to the current session.
    pub fn is_current(&self, epoch: u64) -> bool {
        self.generation == epoch && self.credentials.is_some()
    }

    pub fn is_logged_in(&self) -> bool {
        self.credentials.is_some()
    }

    /// Another view replaces the room area (a [`MainView`] or the
    /// omnichannel contact center): the active room is not on screen, so it
    /// is not marked read, takes no dropped files and its notifications
    /// still show.
    pub fn room_area_taken(&self) -> bool {
        self.main_view.is_some() || self.omni.contact_center.is_some()
    }

    /// The open inbox view, if any.
    pub fn inbox_view(&self) -> Option<&crate::inbox::InboxScreen> {
        self.main_view.as_ref().and_then(MainView::inbox)
    }

    /// The open Later view, if any.
    pub fn later_view(&self) -> Option<&crate::later::LaterView> {
        self.main_view.as_ref().and_then(MainView::later)
    }

    /// The open Drafts & scheduled view, if any.
    pub fn drafts_view(&self) -> Option<&crate::drafts::DraftsView> {
        self.main_view.as_ref().and_then(MainView::drafts)
    }

    /// The open search view, if any.
    pub fn search_view(&self) -> Option<&crate::search::SearchView> {
        self.main_view.as_ref().and_then(MainView::search)
    }

    /// Shows `v` in place of the room area, closing the view there.
    pub fn show_main_view(&mut self, v: MainView) {
        self.close_main_view();
        self.main_view = Some(v);
    }

    /// Closes the view in place of the room area (a room opens, Esc). The
    /// search view is kept to show again.
    pub fn close_main_view(&mut self) {
        if let Some(MainView::Search(v)) = self.main_view.take() {
            self.search.stash = Some(v.stashed());
        }
        self.main_view = None;
        self.workspace.keep_feeds(&[]);
        // A view opened next starts at the same revision: the loader must
        // plan for it rather than take the old view's key as current.
        self.inbox.plan_key = None;
    }
}
