//! Per-server state and the workspace layout (rail + sidebar + room area).

pub mod rail;
pub mod sidebar;
pub mod status_menu;

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
            rooms: HashMap::new(),
            active_room: None,
            syncing: false,
            resync_pending: false,
            sync_attempt: 0,
            sync_error: None,
            pending_room: None,
            start_error: None,
            restoring: true,
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

    /// Drops the session, room views and workspace state (logout). Bumps the
    /// generation so late results of the old session are discarded. The
    /// returned task stops the typing indicators of the closed rooms (sent
    /// through the old session; results are not needed).
    pub fn reset(&mut self) -> iced::Task<crate::room::Message> {
        self.generation += 1;
        self.credentials = None;
        let session = self.session.take();
        let rooms: Vec<_> = self.rooms.drain().collect();
        let mut tasks = Vec::new();
        for (_, rv) in rooms {
            tasks.push(rv.close(&mut self.workspace, session.as_ref()));
        }
        self.active_room = None;
        self.workspace = Workspace::new();
        self.sidebar = sidebar::Sidebar::default();
        self.syncing = false;
        self.resync_pending = false;
        self.sync_attempt = 0;
        self.sync_error = None;
        self.start_error = None;
        iced::Task::batch(tasks)
    }

    /// Whether a result tagged with `epoch` belongs to the current session.
    pub fn is_current(&self, epoch: u64) -> bool {
        self.generation == epoch && self.credentials.is_some()
    }

    pub fn is_logged_in(&self) -> bool {
        self.credentials.is_some()
    }
}
