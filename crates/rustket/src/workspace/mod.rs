//! Per-server state and the workspace layout (rail + sidebar + room area).

pub mod rail;
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
        self.syncing = false;
        self.resync_pending = false;
        self.sync_attempt = 0;
        self.sync_error = None;
        self.start_error = None;
    }

    /// Whether a result tagged with `epoch` belongs to the current session.
    pub fn is_current(&self, epoch: u64) -> bool {
        self.generation == epoch && self.credentials.is_some()
    }

    pub fn is_logged_in(&self) -> bool {
        self.credentials.is_some()
    }
}
