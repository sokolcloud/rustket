//! Application state, update and subscriptions (`view` is in `app/view.rs`).

mod components;
mod uikit;
mod view;

use std::collections::{BTreeMap, VecDeque};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use iced::{Subscription, Task, Theme, window};
use rocketchat::auth::parse_deep_link;
use rocketchat::models::User;
use rocketchat::rest::{LoginRequest, LoginResult};
use rocketchat::{Credentials, RestClient, ServerUrl};
use rust_i18n::t;
use rustket_core::config::{
    AppConfig, PinnedCertificate, ServerEntry, Theme as ThemePref, TokenKind,
};
use rustket_core::room_list;
use rustket_core::secrets::{
    self, MemoryStore, SecretBackend, SecretNotice, SecretStore, secret_key,
};
use rustket_core::workspace::ConnectionState;
use rustket_core::{ServerId, sync};
use rustket_ui::Mode;
use rustket_ui::widget::{BannerKind, Toast};

use crate::component;
use crate::config_writer::ConfigWriter;
use crate::deeplink::{self, Decision, PendingLogin};
use crate::error::{Failure, Res};
use crate::images::{self, Images};
use crate::message::{Message, ModalMessage, SecretsReady};
use crate::room::{self, RoomView, UpdateCtx};
use crate::screens::add_server::{self, AddServer};
use crate::screens::directory::open;
use crate::screens::login::{self, Login};
use crate::session::{self, Followup, ServerEvent};
use crate::single_instance::{self, ListenerSlot};
use crate::ui_state::UiState;
use crate::url_handler::{self, XdgEnv};
use crate::workspace::sidebar;
use crate::workspace::{ServerState, rail};

/// How long a toast stays.
const TOAST_TTL: Duration = Duration::from_secs(6);
/// How long a room deep link waits for its server to load.
pub const ROOM_LINK_TTL: Duration = Duration::from_secs(120);
/// Delay before the confirm button of a token confirmation is enabled.
pub const CONFIRM_ARM_DELAY: Duration = Duration::from_millis(800);
/// Longest wait for the server-side logout.
pub const LOGOUT_TIMEOUT: Duration = Duration::from_secs(10);
/// Sidebar rows whose avatars are prefetched (the rest load when shown).
pub const AVATAR_PREFETCH_ROWS: usize = 200;

/// Where secrets come from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SecretsMode {
    /// OS keyring, file fallback, memory fallback.
    #[default]
    System,
    /// In-memory only (tests, snapshot).
    Memory,
}

/// Startup options.
#[derive(Clone, Default)]
pub struct Flags {
    /// Config directory; `None` = `AppConfig::default_dir()`.
    pub config_dir: Option<PathBuf>,
    /// Do not read or write any config file (tests).
    pub ephemeral: bool,
    pub cache_dir: Option<PathBuf>,
    pub secrets: SecretsMode,
    /// URLs from argv (deep links).
    pub urls: Vec<String>,
    pub listener: Option<ListenerSlot>,
    /// Open URLs in the external browser (off for tests/snapshots).
    pub open_urls: bool,
    /// Real desktop integration: tray, notifications, sounds, idle → away,
    /// window hiding (off for tests/snapshots).
    pub desktop: bool,
}

pub enum Screen {
    Loading,
    AddServer(Box<AddServer>),
    /// The active server: login or workspace.
    Server,
    Settings(Box<crate::screens::settings::Settings>),
    Account(ServerId, Box<crate::screens::account::Account>),
    Directory(ServerId, Box<crate::screens::directory::Directory>),
    Admin(ServerId, Box<crate::screens::admin::Admin>),
}

pub enum Modal {
    /// Login-CSRF protection for Token links: shows the account and server and
    /// signs in only after explicit confirmation (enabled after a short delay).
    ConfirmToken {
        server_id: ServerId,
        credentials: Credentials,
        user: Box<User>,
        armed: bool,
        /// Identifies this showing; a stale `ArmConfirm` never arms it.
        stamp: u64,
    },
    OfferAddServer {
        url: ServerUrl,
    },
    RegisterHandler {
        current: Option<String>,
        loaded: bool,
        busy: bool,
        error: Option<String>,
        /// Paths writable by a shared group (registration needs confirmation).
        warnings: Vec<String>,
    },
    ConfirmRemove(ServerId),
    CreateRoom(ServerId, Box<crate::dialogs::create_room::CreateRoom>),
    QuickSwitcher(ServerId, Box<crate::dialogs::quick_switcher::QuickSwitcher>),
}

pub struct App {
    pub config: AppConfig,
    pub config_dir: Option<PathBuf>,
    pub ui_state: UiState,
    writer: Option<ConfigWriter>,
    pub secrets: Option<Arc<dyn SecretStore>>,
    pub secret_backend: Option<SecretBackend>,
    pub servers: BTreeMap<ServerId, ServerState>,
    pub active: Option<ServerId>,
    pub screen: Screen,
    pub toasts: Vec<Toast>,
    next_toast: u64,
    pub modals: Vec<Modal>,
    pub images: Images,
    pub system_mode: Option<Mode>,
    pub focused: bool,
    pub handler_registered: bool,
    listener: Option<ListenerSlot>,
    queued_links: Vec<String>,
    booted: bool,
    open_urls: bool,
    secrets_mode: SecretsMode,
    stamp: u64,
    /// Token confirmations waiting for the single confirmation slot.
    confirm_queue: VecDeque<(ServerId, Credentials, Box<User>)>,
    save_stamp: u64,
    pub desktop: crate::desktop::Desktop,
    /// (unread rooms, mentions) over all servers, last given to `desktop`.
    last_unread: (u64, u64),
    /// Video calls: ringing, popups, call dialogs (`crate::videoconf`).
    pub calls: crate::videoconf::Calls,
}

fn notice_text(n: &SecretNotice) -> String {
    match n {
        SecretNotice::KeyringUnavailable(why) => {
            t!("app.keyring_unavailable", reason = why).into_owned()
        }
        SecretNotice::KeyringTokensUnreachable => t!("app.keyring_unreachable").into_owned(),
        SecretNotice::MigratedToKeyring { count } => {
            t!("app.keyring_migrated", count = count).into_owned()
        }
        SecretNotice::PlaintextTokensLeft { count } => {
            t!("app.plaintext_left", count = count).into_owned()
        }
        SecretNotice::FileRecovered { moved_to } => {
            t!("app.secret_file_recovered", path = moved_to.display()).into_owned()
        }
    }
}

/// Opens the secret store and reads the tokens (blocking).
fn open_secrets(mode: SecretsMode, wanted: Vec<(ServerId, String)>) -> SecretsReady {
    let (store, backend, notices): (Box<dyn SecretStore>, SecretBackend, Vec<String>) = match mode {
        SecretsMode::Memory => (
            Box::new(MemoryStore::new()),
            SecretBackend::Memory,
            Vec::new(),
        ),
        SecretsMode::System => {
            let o = secrets::open_default_report();
            let notices = o.notices.iter().map(notice_text).collect();
            (o.store, o.backend, notices)
        }
    };
    let tokens = wanted
        .into_iter()
        .map(|(sid, uid)| {
            let token = store.get(&secret_key(&sid, &uid)).unwrap_or_else(|e| {
                tracing::warn!(error = %e, "cannot read token");
                None
            });
            (sid, token)
        })
        .collect();
    SecretsReady {
        store: Arc::from(store),
        backend,
        tokens,
        notices,
    }
}

/// Room of a link segment: a room id, or a room name / display name.
fn find_room(ws: &rustket_core::Workspace, name: &str) -> Option<String> {
    if ws.room(name).is_some() {
        return Some(name.to_owned());
    }
    ws.rooms()
        .values()
        .find(|e| e.name() == name || e.fname() == Some(name))
        .map(|e| e.rid().to_owned())
}

fn sync_backoff(attempt: u32) -> Duration {
    Duration::from_secs(2u64.saturating_pow(attempt.clamp(1, 6)).min(60))
}

impl App {
    pub fn new(flags: Flags) -> (Self, Task<Message>) {
        let config_dir = if flags.ephemeral {
            None
        } else {
            flags
                .config_dir
                .clone()
                .or_else(|| AppConfig::default_dir().ok())
        };
        let config = match &config_dir {
            Some(dir) => AppConfig::load(dir).unwrap_or_else(|e| {
                tracing::warn!(error = %e, "config unreadable; using defaults");
                AppConfig::default()
            }),
            None => AppConfig::default(),
        };
        crate::i18n::apply(config.ui.language);
        let desktop = if flags.desktop {
            crate::desktop::Desktop::system(&config.ui)
        } else {
            crate::desktop::Desktop::new()
        };
        crate::room::emoji_picker::set_recent(&config.recent_emoji);
        let ui_state = config_dir.as_deref().map(UiState::load).unwrap_or_default();
        let writer = config_dir.clone().and_then(ConfigWriter::spawn);
        let mut servers = BTreeMap::new();
        for entry in &config.servers {
            match ServerState::from_entry(entry) {
                Ok(s) => {
                    servers.insert(entry.id.clone(), s);
                }
                Err(e) => tracing::warn!(url = %entry.url, error = %e, "skipping server"),
            }
        }
        let active = config
            .active_server
            .clone()
            .filter(|id| servers.contains_key(id))
            .or_else(|| config.sorted_servers().first().map(|s| s.id.clone()));
        let cache_dir = flags
            .cache_dir
            .clone()
            .map(|d| d.join("images"))
            .or_else(images::default_cache_dir);
        let app = Self {
            config,
            config_dir,
            ui_state,
            writer,
            secrets: None,
            secret_backend: None,
            servers,
            active,
            screen: Screen::Loading,
            toasts: Vec::new(),
            next_toast: 0,
            modals: Vec::new(),
            images: Images::new(cache_dir),
            system_mode: None,
            focused: true,
            handler_registered: false,
            listener: flags.listener.clone(),
            queued_links: flags.urls.clone(),
            booted: false,
            open_urls: flags.open_urls,
            secrets_mode: flags.secrets,
            stamp: 0,
            confirm_queue: VecDeque::new(),
            save_stamp: 0,
            desktop,
            last_unread: (0, 0),
            calls: crate::videoconf::Calls::default(),
        };
        let wanted: Vec<(ServerId, String)> = app
            .config
            .servers
            .iter()
            .filter_map(|s| Some((s.id.clone(), s.user_id.clone()?)))
            .collect();
        let mode = flags.secrets;
        let open = Task::perform(
            async move { tokio::task::spawn_blocking(move || open_secrets(mode, wanted)).await },
            |r| match r {
                Ok(s) => Message::SecretsReady(s),
                Err(e) => {
                    tracing::error!(error = %e, "secret store task failed");
                    Message::SecretsReady(SecretsReady {
                        store: Arc::new(MemoryStore::new()),
                        backend: SecretBackend::Memory,
                        tokens: Vec::new(),
                        notices: Vec::new(),
                    })
                }
            },
        );
        let theme = iced::system::theme().map(Message::SystemTheme);
        let handler = query_handler_task();
        (app, Task::batch([open, theme, handler]))
    }

    // --- helpers ---------------------------------------------------------------

    pub fn mode(&self) -> Mode {
        match self.config.ui.theme {
            ThemePref::Light => Mode::Light,
            ThemePref::Dark => Mode::Dark,
            ThemePref::System => self.system_mode.unwrap_or(Mode::Light),
        }
    }

    pub fn theme(&self) -> Theme {
        rustket_ui::theme(self.mode())
    }

    /// UI zoom (`UiPrefs::zoom`).
    pub fn scale_factor(&self) -> f32 {
        self.config.ui.zoom
    }

    pub fn title(&self) -> String {
        let unread: u64 = self
            .servers
            .values()
            .map(|s| room_list::unread_totals(&s.workspace).0)
            .sum();
        if unread > 0 {
            format!("{} ({unread})", t!("app.title"))
        } else {
            t!("app.title").into_owned()
        }
    }

    fn next_stamp(&mut self) -> u64 {
        self.stamp += 1;
        self.stamp
    }

    pub fn toast(
        &mut self,
        kind: BannerKind,
        title: impl Into<String>,
        body: impl Into<String>,
    ) -> Task<Message> {
        self.next_toast += 1;
        let id = self.next_toast;
        self.toasts.push(Toast {
            id,
            kind,
            title: title.into(),
            body: body.into(),
        });
        while self.toasts.len() > 4 {
            self.toasts.remove(0);
        }
        Task::perform(tokio::time::sleep(TOAST_TTL), move |_| {
            Message::CloseToast(id)
        })
    }

    /// Queues the current config + UI state for the single writer thread.
    fn save_config(&self) -> Task<Message> {
        if let Some(w) = &self.writer {
            w.save(self.config.clone(), self.ui_state.clone());
        }
        Task::none()
    }

    /// Blocks until queued config writes are on disk (tests, shutdown).
    pub fn flush_config(&self) {
        if let Some(w) = &self.writer {
            w.flush();
        }
    }

    fn secret_task(
        &self,
        op: impl FnOnce(&dyn SecretStore) -> Result<(), String> + Send + 'static,
    ) -> Task<Result<(), String>> {
        let Some(store) = self.secrets.clone() else {
            return Task::done(Err("secret store not ready".into()));
        };
        Task::perform(
            async move {
                tokio::task::spawn_blocking(move || op(store.as_ref()))
                    .await
                    .unwrap_or_else(|e| Err(e.to_string()))
            },
            |r| r,
        )
    }

    /// Opens an absolute `http`/`https`/`mailto` URL in the browser; anything
    /// else (other schemes, paths) is refused.
    fn open_url(&mut self, url: String) -> Task<Message> {
        let Some(url) = rustket_core::uikit::openable_url(&url).map(str::to_owned) else {
            tracing::warn!("refusing to open a non-http URL");
            return self.toast(BannerKind::Error, t!("app.url_refused"), String::new());
        };
        if !self.open_urls {
            tracing::info!("not opening browser (disabled)");
            return Task::none();
        }
        Task::perform(
            async move {
                tokio::task::spawn_blocking(move || {
                    webbrowser::open(&url).map_err(|e| e.to_string())
                })
                .await
                .unwrap_or_else(|e| Err(e.to_string()))
            },
            |r| {
                if let Err(e) = r {
                    tracing::warn!(error = %e, "cannot open browser");
                }
                Message::Noop
            },
        )
    }

    /// A URL clicked in a room: permalinks to messages of this server open
    /// in the app (a channel the user has not joined as a preview, like the
    /// directory does), everything else in the browser, as does a permalink
    /// whose room the server does not show.
    fn open_room_url(&mut self, sid: &ServerId, url: String) -> Task<Message> {
        let Some(s) = self.servers.get(sid) else {
            return self.open_url(url);
        };
        let mut bases = vec![s.url.join("")];
        if let Some(site) = s.workspace.settings().str("Site_Url") {
            bases.push(site.trim().to_owned());
        }
        let Some(path) = deeplink::permalink_path(&url, &bases) else {
            return self.open_url(url);
        };
        let local = deeplink::room_name_from_path(&path)
            .and_then(|(_, name)| find_room(&s.workspace, &deeplink::percent_decode(name)));
        match local {
            Some(rid) => self.open_room_link(sid, Some(rid), Some(path)),
            None => match self.resolve_room_link(sid, &path, Some(url.clone())) {
                Some(task) => task,
                None => self.open_url(url),
            },
        }
    }

    /// Fetches the room of link `path` that the workspace does not have (by
    /// name; a channel or private group the server lets the user see) to
    /// open it as a preview. `None` when the path names no such room or the
    /// session is down.
    fn resolve_room_link(
        &self,
        sid: &ServerId,
        path: &str,
        fallback: Option<String>,
    ) -> Option<Task<Message>> {
        let (kind, name) = deeplink::room_name_from_path(path)?;
        if !matches!(kind, "channel" | "group") {
            return None;
        }
        let rest = self.servers.get(sid)?.session.as_ref()?.rest().clone();
        let target = open::Target::Name(deeplink::percent_decode(name));
        let mid = deeplink::message_id_from_path(path);
        let sid = sid.clone();
        Some(Task::perform(open::resolve(rest, target), move |r| {
            Message::RoomLinkResolved(
                sid.clone(),
                mid.clone(),
                fallback.clone(),
                Box::new(r.map_err(Failure::from)),
            )
        }))
    }

    fn server_urls(&self) -> Vec<ServerUrl> {
        self.servers.values().map(|s| s.url.clone()).collect()
    }

    fn pending_logins(&self) -> Vec<PendingLogin> {
        self.servers
            .values()
            .filter_map(|s| s.login.as_ref()?.pending_login().cloned())
            .collect()
    }

    fn show_login(&mut self, sid: &ServerId, reason: Option<String>) -> Task<Message> {
        let handler_registered = self.handler_registered;
        let Some(s) = self.servers.get_mut(sid) else {
            return Task::none();
        };
        s.restoring = false;
        match Login::new(sid.clone(), s.url.clone(), s.tls.clone(), None, reason) {
            Ok((mut login, task)) => {
                login.handler_registered = handler_registered;
                s.login = Some(login);
                let sid = sid.clone();
                task.map(move |m| Message::Login(sid.clone(), m))
            }
            Err(e) => {
                let body = crate::error::user_text(&e);
                self.toast(BannerKind::Error, t!("app.error"), body)
            }
        }
    }

    /// Removes token confirmations of `sid` (login cancelled, finished, …) and
    /// shows the next queued one.
    fn drop_token_confirmations(&mut self, sid: &ServerId) {
        self.modals
            .retain(|m| !matches!(m, Modal::ConfirmToken { server_id, .. } if server_id == sid));
        self.confirm_queue.retain(|(s, _, _)| s != sid);
    }

    /// One confirmation at a time: shows the next queued one when the slot is
    /// free; a confirmation covered by another dialog is disarmed, and the one
    /// on top arms after a delay. Runs after every update.
    fn show_next_confirmation(&mut self) -> Task<Message> {
        let shown = self
            .modals
            .iter()
            .any(|m| matches!(m, Modal::ConfirmToken { .. }));
        if !shown && let Some((server_id, credentials, user)) = self.confirm_queue.pop_front() {
            self.modals.push(Modal::ConfirmToken {
                server_id,
                credentials,
                user,
                armed: false,
                stamp: 0,
            });
        }
        self.disarm_covered_confirmations();
        self.rearm_top_confirmation()
    }

    /// When a confirmation is (or became) the top dialog it starts disarmed and
    /// arms after [`CONFIRM_ARM_DELAY`].
    fn rearm_top_confirmation(&mut self) -> Task<Message> {
        let stamp = self.next_stamp();
        let Some(Modal::ConfirmToken {
            armed, stamp: s, ..
        }) = self.modals.last_mut()
        else {
            return Task::none();
        };
        if *s != 0 {
            // Armed, or counting down.
            return Task::none();
        }
        *armed = false;
        *s = stamp;
        Task::perform(tokio::time::sleep(CONFIRM_ARM_DELAY), move |_| {
            Message::ArmConfirm(stamp)
        })
    }

    /// Disarms a confirmation that was covered by another dialog.
    fn disarm_covered_confirmations(&mut self) {
        let n = self.modals.len();
        for (i, m) in self.modals.iter_mut().enumerate() {
            if i + 1 < n
                && let Modal::ConfirmToken { armed, stamp, .. } = m
            {
                *armed = false;
                *stamp = 0;
            }
        }
    }

    fn room_task(
        sid: &ServerId,
        epoch: u64,
        rid: &str,
        task: Task<room::Message>,
    ) -> Task<Message> {
        let (sid, rid) = (sid.clone(), rid.to_owned());
        task.map(move |m| Message::Room(sid.clone(), epoch, rid.clone(), m))
    }

    // --- update ----------------------------------------------------------------

    pub fn update(&mut self, message: Message) -> Task<Message> {
        let task = self.update_inner(message);
        let after = self.after_update();
        let confirm = self.show_next_confirmation();
        let unread = self.unread_hook();
        let unread = Task::batch([unread, self.omni_after_update()]);
        Task::batch([task, after, confirm, unread])
    }

    /// Applies pending workspace changes to derived views and fetches wanted images.
    fn after_update(&mut self) -> Task<Message> {
        let mut tasks = Vec::new();
        let focused = self.focused;
        let active = self.active.clone();
        let sids: Vec<ServerId> = self.servers.keys().cloned().collect();
        let mut effects = Vec::new();
        for sid in sids {
            let Some(s) = self.servers.get_mut(&sid) else {
                continue;
            };
            // E2E: start/feed the manager and decrypt what is shown.
            tasks.push(crate::e2e::after_update(s, self.secrets.as_ref()));
            s.sidebar.refresh(
                &s.workspace,
                &self.config.ui,
                &self.images,
                &s.id,
                &s.url,
                AVATAR_PREFETCH_ROWS,
            );
            if s.workspace.pending_changes().is_empty() {
                continue;
            }
            let changes = s.workspace.take_changes();
            // Close views of rooms that disappeared (kicked, left, deleted).
            let gone: Vec<String> = s
                .rooms
                .keys()
                .filter(|rid| s.workspace.room(rid).is_none())
                .cloned()
                .collect();
            for rid in gone {
                if let Some(rv) = s.rooms.remove(&rid) {
                    tasks.push(rv.close(&mut s.workspace, s.session.as_ref()).discard());
                }
                if s.active_room.as_deref() == Some(rid.as_str()) {
                    s.active_room = None;
                }
                if let Some(e) = self.config.server_mut(&sid)
                    && e.last_room.as_deref() == Some(rid.as_str())
                {
                    e.last_room = None;
                }
            }
            let is_active_server = active.as_ref() == Some(&sid);
            let active_room = s.active_room.clone();
            let epoch = s.generation;
            for (rid, rv) in s.rooms.iter_mut() {
                let mut ctx = UpdateCtx {
                    server_id: &s.id,
                    server: &s.url,
                    rid,
                    workspace: &mut s.workspace,
                    session: s.session.as_ref(),
                    images: &self.images,
                    prefs: &self.config.ui,
                };
                let mut action = rv.on_changes(&changes, &mut ctx);
                // Mark the focused room read when new messages arrive (not
                // when it was marked unread elsewhere).
                if is_active_server
                    && focused
                    && active_room.as_deref() == Some(rid.as_str())
                    && (changes.rooms.contains(rid) || changes.timelines.contains(rid))
                    && rv.wants_auto_read(ctx.workspace)
                {
                    action.task = Task::batch([action.task, rv.mark_read(&ctx)]);
                }
                // ... and its open thread (`tunread`) when new replies arrive.
                if is_active_server
                    && focused
                    && active_room.as_deref() == Some(rid.as_str())
                    && changes.rooms.contains(rid)
                {
                    action.task = Task::batch([action.task, rv.mark_thread_read(&ctx)]);
                }
                tasks.push(Self::room_task(&sid, epoch, rid, action.task));
                for e in action.effects {
                    effects.push((sid.clone(), rid.clone(), e));
                }
            }
        }
        for (sid, rid, e) in effects {
            tasks.push(self.handle_effects(&sid, &rid, vec![e]));
        }
        // Avatar working set of all servers: every row of the active server,
        // the prefetched rows of the others, plus room views.
        let working: usize = self
            .servers
            .iter()
            .map(|(id, s)| {
                let rooms = s.workspace.rooms().len();
                let rows = if active.as_ref() == Some(id) {
                    rooms
                } else {
                    rooms.min(AVATAR_PREFETCH_ROWS)
                };
                rows + 64
            })
            .sum();
        self.images.set_working_set(working);
        tasks.push(self.fetch_images());
        tasks.push(crate::videoconf::fetch_wanted(self));
        Task::batch(tasks)
    }

    fn fetch_images(&mut self) -> Task<Message> {
        let mut tasks = Vec::new();
        for req in self.images.take_wanted() {
            let generation = self.images.generation(&req.server_id);
            let Some(s) = self.servers.get(&req.server_id) else {
                self.images.finish(
                    &req.server_id,
                    generation,
                    req.url,
                    Err("server gone".into()),
                );
                continue;
            };
            let rest = match &s.session {
                Some(h) => h.rest().clone(),
                None => match RestClient::new(s.url.clone(), &s.tls) {
                    Ok(r) => r,
                    Err(e) => {
                        self.images
                            .finish(&req.server_id, generation, req.url, Err(e.to_string()));
                        continue;
                    }
                },
            };
            let dir = self.images.server_dir(&req.server_id);
            let (sid, url) = (req.server_id.clone(), req.url.clone());
            tasks.push(Task::perform(images::fetch(rest, dir, req.url), move |r| {
                Message::ImageLoaded(sid.clone(), generation, url.clone(), r)
            }));
        }
        Task::batch(tasks)
    }

    fn update_inner(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::Noop => Task::none(),
            Message::FileDropped(path) => self.route_file_drop(path),
            Message::E2e(sid, epoch, m) => self.on_e2e(sid, epoch, m),
            Message::SecretsReady(s) => self.on_secrets_ready(s),
            Message::SystemTheme(mode) => {
                self.system_mode = match mode {
                    iced::theme::Mode::Dark => Some(Mode::Dark),
                    iced::theme::Mode::Light => Some(Mode::Light),
                    iced::theme::Mode::None => None,
                };
                Task::none()
            }
            Message::WindowFocused(f) => {
                self.focused = f;
                let desktop = self
                    .desktop
                    .on_focus_changed(f, &self.config.ui)
                    .map(Message::Desktop);
                if f {
                    return Task::batch([desktop, self.mark_active_room_read()]);
                }
                desktop
            }
            Message::Tick => {
                let now = Instant::now();
                for s in self.servers.values_mut() {
                    s.workspace.expire_typing(now);
                }
                Task::none()
            }
            Message::CloseToast(id) => {
                self.toasts.retain(|t| t.id != id);
                Task::none()
            }
            Message::ShowAddServer => {
                self.screen = Screen::AddServer(Box::new(AddServer::new(!self.servers.is_empty())));
                iced::widget::operation::focus(add_server::URL_INPUT_ID)
            }
            Message::AddServer(m) => self.on_add_server(m),
            Message::Login(sid, m) => self.on_login(sid, m),
            Message::Rail(m) => match m {
                rail::Message::Select(sid) => self.activate(sid),
                rail::Message::Add => Task::done(Message::ShowAddServer),
                rail::Message::Settings => self.navigate(component::Nav::Settings),
            },
            Message::Sidebar(sid, m) => self.on_sidebar(sid, m),
            Message::Settings(m) => self.on_settings(m),
            Message::Account(sid, m) => self.on_account(sid, m),
            Message::Directory(sid, m) => self.on_directory(sid, m),
            Message::Admin(sid, m) => self.on_admin(sid, m),
            Message::CreateRoom(sid, m) => self.on_create_room(sid, m),
            Message::QuickSwitcher(sid, m) => self.on_quick_switcher(sid, m),
            Message::StatusMenu(sid, m) => self.on_status_menu(sid, m),
            Message::UiKit(sid, epoch, m) => self.on_uikit_in(sid, epoch, m),
            Message::Desktop(m) => self.on_desktop(m),
            Message::Calls(m) => self.on_calls(m),
            Message::OpenQuickSwitcher => self.navigate(component::Nav::QuickSwitcher),
            Message::SidebarResized(w) => {
                self.ui_state.sidebar_width = w;
                self.save_stamp += 1;
                let stamp = self.save_stamp;
                Task::perform(tokio::time::sleep(Duration::from_millis(500)), move |_| {
                    Message::SaveDebounced(stamp)
                })
            }
            Message::Room(sid, epoch, rid, m) => self.on_room(sid, epoch, rid, m),
            Message::Server(sid, generation, ev) => {
                let current = self
                    .servers
                    .get(&sid)
                    .is_some_and(|s| s.generation == generation && s.credentials.is_some());
                if !current {
                    tracing::debug!(server = %sid, "dropping event of an old session");
                    return Task::none();
                }
                let omni = self.omni_on_event(&sid, &ev);
                Task::batch([omni, self.on_server_event(sid, ev)])
            }
            Message::Omni(sid, epoch, m) => self.on_omni(sid, epoch, m),
            Message::InitialLoaded(sid, epoch, r) => self.on_initial_loaded(sid, epoch, r),
            Message::Resynced(sid, epoch, delta, rooms) => {
                self.on_resynced(sid, epoch, delta, rooms)
            }
            Message::RetrySync(sid, epoch) => {
                if self.servers.get(&sid).is_some_and(|s| s.is_current(epoch)) {
                    return self.resync(&sid);
                }
                Task::none()
            }
            Message::RoomLinkResolved(sid, mid, fallback, r) => {
                self.room_link_resolved(&sid, mid, fallback, *r)
            }
            Message::RoomLinkExpired(sid, stamp) => {
                let expired = self
                    .servers
                    .get_mut(&sid)
                    .filter(|s| s.pending_room.as_ref().is_some_and(|p| p.2 == stamp))
                    .map(|s| s.pending_room.take());
                if expired.is_some() {
                    return self.toast(
                        BannerKind::Warning,
                        t!("deeplink.room_link_timeout"),
                        String::new(),
                    );
                }
                Task::none()
            }
            Message::ArmConfirm(stamp) => {
                if let Some(Modal::ConfirmToken {
                    armed, stamp: s, ..
                }) = self.modals.last_mut()
                    && *s == stamp
                {
                    *armed = true;
                }
                Task::none()
            }
            Message::SaveDebounced(stamp) => {
                if stamp == self.save_stamp {
                    return self.save_config();
                }
                Task::none()
            }
            Message::CloseRequested(id) => {
                if let Some(hide) = self.desktop.on_close_requested(&self.config.ui) {
                    return hide.map(Message::Desktop);
                }
                self.flush_config();
                iced::window::close(id)
            }
            Message::FlushImages => Task::none(),
            Message::ImageLoaded(sid, generation, url, r) => {
                // Results of a purged cache (logout, removal) are dropped and
                // never written back.
                let current = self.servers.contains_key(&sid)
                    && self.images.finish(&sid, generation, url, r.fetched);
                match r.write {
                    Some((path, bytes)) if current => Task::perform(
                        async move {
                            tokio::task::spawn_blocking(move || {
                                images::write_cache_entry(&path, &bytes)
                            })
                            .await
                        },
                        |r| {
                            if let Ok(Err(e)) = r {
                                tracing::debug!(error = %e, "image cache write failed");
                            }
                            Message::Noop
                        },
                    ),
                    _ => Task::none(),
                }
            }
            Message::DeepLinks(urls) => {
                // Another launch (with or without links): raise the window.
                let raise = self.desktop.show_window().map(Message::Desktop);
                if urls.is_empty() {
                    return raise;
                }
                if !self.booted {
                    self.queued_links.extend(urls);
                    return raise;
                }
                let mut tasks: Vec<_> = urls.into_iter().map(|u| self.handle_link(&u)).collect();
                tasks.push(raise);
                Task::batch(tasks)
            }
            Message::TokenUser(sid, creds, r) => self.on_token_user(sid, creds, r),
            Message::TokenStored(sid, r) => {
                if let Err(e) = r {
                    tracing::warn!(server = %sid, error = %e, "token not stored");
                    return self.toast(BannerKind::Warning, t!("app.token_not_saved"), e);
                }
                Task::none()
            }
            Message::RetryConnection(sid) => {
                if let Some(s) = self.servers.get_mut(&sid) {
                    match &s.session {
                        Some(h) => h.session().reconnect_now(),
                        None => {
                            s.start_error = None;
                            s.generation += 1;
                        }
                    }
                }
                Task::none()
            }
            Message::LoggedOut(sid, r) => {
                if let Err(e) = r {
                    tracing::warn!(server = %sid, error = %e, "logout call failed");
                    return self.toast(BannerKind::Warning, t!("app.revoke_failed"), e);
                }
                Task::none()
            }
            Message::HandlerQueried(current, registered, check) => {
                self.handler_registered = registered;
                for s in self.servers.values_mut() {
                    if let Some(l) = s.login.as_mut() {
                        l.handler_registered = registered;
                    }
                }
                for m in &mut self.modals {
                    if let Modal::RegisterHandler {
                        current: c,
                        loaded,
                        warnings,
                        error,
                        ..
                    } = m
                    {
                        *c = current.clone();
                        *loaded = true;
                        match &check {
                            Ok(w) => *warnings = w.clone(),
                            Err(e) => *error = Some(e.clone()),
                        }
                    }
                }
                Task::none()
            }
            Message::HandlerRegistered(r) => {
                for m in &mut self.modals {
                    if let Modal::RegisterHandler { busy, error, .. } = m {
                        *busy = false;
                        if let Err(e) = &r {
                            *error = Some(e.clone());
                        }
                    }
                }
                match r {
                    Ok(_) => {
                        self.modals
                            .retain(|m| !matches!(m, Modal::RegisterHandler { .. }));
                        let t = self.toast(
                            BannerKind::Success,
                            t!("handler.registered"),
                            String::new(),
                        );
                        Task::batch([t, query_handler_task()])
                    }
                    Err(_) => Task::none(),
                }
            }
            Message::MarkedUnread(sid, r) => match r {
                Ok(()) => self.toast(BannerKind::Success, t!("app.marked_unread"), String::new()),
                Err(e) => {
                    tracing::warn!(server = %sid, error = %e, "subscriptions.unread failed");
                    self.toast(
                        BannerKind::Error,
                        t!("app.mark_unread_failed"),
                        e.user_text(),
                    )
                }
            },
            Message::Modal(m) => self.on_modal(m),
            Message::CloseModal => {
                if let Some(Modal::ConfirmToken { server_id, .. }) = self.modals.pop() {
                    // Rejecting the account also ends the browser login.
                    if let Some(l) = self
                        .servers
                        .get_mut(&server_id)
                        .and_then(|s| s.login.as_mut())
                    {
                        l.cancel_pending();
                    }
                    self.drop_token_confirmations(&server_id);
                }
                Task::none()
            }
        }
    }

    fn on_secrets_ready(&mut self, s: SecretsReady) -> Task<Message> {
        self.secrets = Some(s.store);
        self.secret_backend = Some(s.backend);
        let mut tasks = Vec::new();
        for n in &s.notices {
            tasks.push(self.toast(BannerKind::Warning, t!("app.secrets_notice"), n.clone()));
        }
        if s.backend == SecretBackend::File && self.secrets_mode == SecretsMode::System {
            tasks.push(self.toast(
                BannerKind::Warning,
                t!("app.keyring_missing"),
                t!("app.keyring_file"),
            ));
        }
        let tokens: BTreeMap<ServerId, Option<String>> = s.tokens.into_iter().collect();
        let sids: Vec<ServerId> = self.servers.keys().cloned().collect();
        for sid in sids {
            let uid = self.config.server(&sid).and_then(|e| e.user_id.clone());
            let token = tokens.get(&sid).cloned().flatten();
            match (uid, token) {
                (Some(uid), Some(token)) => {
                    if let Some(st) = self.servers.get_mut(&sid) {
                        st.restoring = false;
                        st.credentials = Some(Credentials::new(uid, token));
                        st.workspace.apply_connection(ConnectionState::Connecting);
                    }
                }
                _ => tasks.push(self.show_login(&sid, None)),
            }
        }
        self.screen = if self.servers.is_empty() {
            Screen::AddServer(Box::new(AddServer::new(false)))
        } else {
            Screen::Server
        };
        self.booted = true;
        let links = std::mem::take(&mut self.queued_links);
        for l in links {
            tasks.push(self.handle_link(&l));
        }
        Task::batch(tasks)
    }

    fn activate(&mut self, sid: ServerId) -> Task<Message> {
        if !self.servers.contains_key(&sid) {
            return Task::none();
        }
        // The rooms of the server left behind stay open but hidden.
        let mut tasks = Vec::new();
        if let Some(prev) = self.active.clone().filter(|p| *p != sid)
            && let Some(s) = self.servers.get_mut(&prev)
        {
            for rv in s.rooms.values_mut() {
                tasks.push(rv.hide(s.session.as_ref()).discard());
            }
        }
        self.active = Some(sid.clone());
        self.screen = Screen::Server;
        if self.config.active_server.as_ref() != Some(&sid) {
            self.config.active_server = Some(sid);
            tasks.push(self.save_config());
        }
        Task::batch(tasks)
    }

    fn on_add_server(&mut self, m: add_server::Message) -> Task<Message> {
        let existing = self.server_urls();
        let Screen::AddServer(a) = &mut self.screen else {
            return Task::none();
        };
        let (task, out) = a.update(m, &existing);
        let task = task.map(Message::AddServer);
        match out {
            add_server::Out::None => task,
            add_server::Out::Cancel => {
                self.screen = if self.servers.is_empty() {
                    Screen::AddServer(Box::new(AddServer::new(false)))
                } else {
                    Screen::Server
                };
                task
            }
            add_server::Out::Added { probe, pins } => {
                let mut entry = ServerEntry::new(probe.url.as_str(), probe.meta.site_name.clone());
                entry.pinned_certificates = pins
                    .iter()
                    .map(|(host, fp)| PinnedCertificate {
                        host: host.clone(),
                        sha256: rocketchat::tls::fingerprint_hex(fp),
                    })
                    .collect();
                let mut state = match ServerState::from_entry(&entry) {
                    Ok(s) => s,
                    Err(e) => {
                        let body = crate::error::user_text(&e);
                        return self.toast(BannerKind::Error, t!("app.error"), body);
                    }
                };
                state.restoring = false;
                let (mut login, login_task) = match Login::new(
                    entry.id.clone(),
                    state.url.clone(),
                    state.tls.clone(),
                    Some(probe.meta.clone()),
                    None,
                ) {
                    Ok(pair) => pair,
                    Err(e) => {
                        let body = crate::error::user_text(&e);
                        return self.toast(BannerKind::Error, t!("app.error"), body);
                    }
                };
                login.handler_registered = self.handler_registered;
                state.login = Some(login);
                let sid = self.config.add_server(entry);
                self.config.active_server = Some(sid.clone());
                self.servers.insert(sid.clone(), state);
                self.active = Some(sid.clone());
                self.screen = Screen::Server;
                let sid2 = sid.clone();
                let login_task = login_task.map(move |m| Message::Login(sid2.clone(), m));
                Task::batch([
                    task,
                    login_task,
                    self.save_config(),
                    iced::widget::operation::focus("login-user"),
                ])
            }
        }
    }

    fn on_login(&mut self, sid: ServerId, m: login::Message) -> Task<Message> {
        let Some(s) = self.servers.get_mut(&sid) else {
            return Task::none();
        };
        let Some(l) = s.login.as_mut() else {
            return Task::none();
        };
        let (task, out) = l.update(m);
        if l.pending.is_none() {
            self.drop_token_confirmations(&sid);
        }
        let sid2 = sid.clone();
        let task = task.map(move |m| Message::Login(sid2.clone(), m));
        let out_task = match out {
            login::Out::None => Task::none(),
            login::Out::LoggedIn(r, kind) => self.logged_in(&sid, *r, kind),
            login::Out::OpenUrl(url) => self.open_url(url),
            login::Out::DeepLink(link) => self.handle_link(&link),
            login::Out::RegisterHandler => self.open_register_handler(),
        };
        Task::batch([task, out_task])
    }

    /// A successful login: persist the token and the server entry, start the session.
    fn logged_in(&mut self, sid: &ServerId, r: LoginResult, kind: TokenKind) -> Task<Message> {
        self.drop_token_confirmations(sid);
        let username = r.me.as_ref().and_then(|m| m.username.clone());
        let old_uid = self.config.server(sid).and_then(|e| e.user_id.clone());
        if let Some(entry) = self.config.server_mut(sid) {
            entry.user_id = Some(r.user_id.clone());
            entry.username = username;
            entry.token_kind = Some(kind);
        }
        let Some(s) = self.servers.get_mut(sid) else {
            return Task::none();
        };
        // A fresh login: no session of the old one is left to tell.
        s.reset();
        s.login = None;
        s.restoring = false;
        s.credentials = Some(r.credentials());
        s.workspace.apply_connection(ConnectionState::Connecting);
        if let Some(me) = r.me.clone() {
            s.workspace.apply_me(me);
        }
        let key = secret_key(sid, &r.user_id);
        let old_key = old_uid
            .filter(|u| u != &r.user_id)
            .map(|u| secret_key(sid, &u));
        let token = r.auth_token.clone();
        let sid2 = sid.clone();
        let store = self
            .secret_task(move |st| {
                if let Some(old) = old_key {
                    let _ = st.delete(&old);
                }
                st.set(&key, &token).map_err(|e| e.to_string())
            })
            .map(move |r| Message::TokenStored(sid2.clone(), r));
        Task::batch([store, self.save_config()])
    }

    fn purge_images(&mut self, sid: &ServerId) -> Task<Message> {
        let Some(url) = self.servers.get(sid).map(|s| s.url.clone()) else {
            return Task::none();
        };
        match self.images.purge_server(sid, &url) {
            Some(dir) => Task::perform(
                async move {
                    let _ = tokio::task::spawn_blocking(move || std::fs::remove_dir_all(dir)).await;
                },
                |_| Message::Noop,
            ),
            None => Task::none(),
        }
    }

    fn delete_token(&mut self, sid: &ServerId) -> Task<Message> {
        let Some(uid) = self.config.server(sid).and_then(|e| e.user_id.clone()) else {
            return Task::none();
        };
        let key = secret_key(sid, &uid);
        self.secret_task(move |st| st.delete(&key).map_err(|e| e.to_string()))
            .map(|_| Message::Noop)
    }

    /// The server rejected the token: drop it and show the login screen.
    fn logged_out(&mut self, sid: &ServerId, reason: Option<String>) -> Task<Message> {
        let mut e2e = Task::none();
        if let Some(s) = self.servers.get_mut(sid) {
            // `force_logout` (e.g. an E2E key reset) also drops the E2E secrets.
            if reason.as_deref() == Some("force_logout") {
                let uid = self.config.server(sid).and_then(|e| e.user_id.clone());
                e2e = crate::e2e::forget_secrets(s, self.secrets.as_ref(), uid.as_deref());
            }
            // The token was rejected: nothing can be sent with it any more.
            s.reset();
        }
        let delete = self.delete_token(sid);
        let purge = self.purge_images(sid);
        let reason = reason.map(|r| match r.as_str() {
            "force_logout" => t!("app.force_logout").into_owned(),
            _ => t!("app.session_expired").into_owned(),
        });
        Task::batch([e2e, delete, purge, self.show_login(sid, reason)])
    }

    /// User logout. A login token is revoked on the server (through the
    /// session, or a plain REST call when the session is not up); a personal
    /// access token or a token of unknown kind is only forgotten locally,
    /// unless `everywhere` (the user chose "sign out everywhere").
    fn logout(&mut self, sid: &ServerId, everywhere: bool) -> Task<Message> {
        let Some(s) = self.servers.get(sid) else {
            return Task::none();
        };
        let handle = s.session.clone();
        let credentials = s.credentials.clone();
        let (url, tls) = (s.url.clone(), s.tls.clone());
        let kind = self.config.server(sid).and_then(|e| e.token_kind);
        let revoke = everywhere || kind == Some(TokenKind::Login);
        let agent = crate::omnichannel::AgentSignOut::of(&s.workspace);
        // Stop-typing calls of the open rooms; not chained before the
        // logout, which must not wait for a DDP call while the socket is down
        // (the server drops the activity with the connection anyway).
        let uid = self.config.server(sid).and_then(|e| e.user_id.clone());
        let typing = match self.servers.get_mut(sid) {
            Some(s) => {
                let t = s.close_rooms().discard();
                let e2e = crate::e2e::forget_secrets(s, self.secrets.as_ref(), uid.as_deref());
                s.reset();
                Task::batch([t, e2e])
            }
            None => Task::none(),
        };
        let call = match (revoke, handle, credentials) {
            (true, Some(h), _) => {
                let sid = sid.clone();
                Task::perform(
                    async move {
                        if agent.revoke(&h).await {
                            h.session().close().await;
                            return Ok(());
                        }
                        tokio::time::timeout(LOGOUT_TIMEOUT, h.session().logout())
                            .await
                            .map_err(|_| "timeout".to_owned())?
                            .map_err(|e| e.to_string())
                    },
                    move |r| Message::LoggedOut(sid.clone(), r),
                )
            }
            (true, None, Some(creds)) => {
                let sid = sid.clone();
                Task::perform(
                    async move {
                        let rest = RestClient::new(url, &tls).map_err(|e| e.to_string())?;
                        rest.set_credentials(Some(creds));
                        tokio::time::timeout(LOGOUT_TIMEOUT, rest.logout())
                            .await
                            .map_err(|_| "timeout".to_owned())?
                            .map_err(|e| e.to_string())
                    },
                    move |r| Message::LoggedOut(sid.clone(), r),
                )
            }
            (false, Some(h), _) => Task::perform(
                async move {
                    agent.keep_token(&h).await;
                    h.session().close().await;
                },
                |_| Message::Noop,
            ),
            _ => Task::none(),
        };
        let delete = self.delete_token(sid);
        let purge = self.purge_images(sid);
        Task::batch([typing, call, delete, purge, self.show_login(sid, None)])
    }

    fn remove_server(&mut self, sid: &ServerId) -> Task<Message> {
        let t = self.logout(sid, false);
        self.servers.remove(sid);
        self.config.remove_server(sid);
        self.drop_token_confirmations(sid);
        self.active = self
            .config
            .active_server
            .clone()
            .filter(|id| self.servers.contains_key(id))
            .or_else(|| self.servers.keys().next().cloned());
        if self.servers.is_empty() {
            self.screen = Screen::AddServer(Box::new(AddServer::new(false)));
        }
        Task::batch([t, self.save_config()])
    }

    /// Alt(+Shift)+Up/Down: the previous/next (unread) room in sidebar order
    /// of the active server (collapsed sections are skipped).
    fn room_step(&mut self, step: crate::desktop::shortcuts::RoomStep) -> Task<Message> {
        if !matches!(self.screen, Screen::Server) || !self.modals.is_empty() {
            return Task::none();
        }
        let Some(sid) = self.active.clone() else {
            return Task::none();
        };
        let Some(s) = self.servers.get(&sid) else {
            return Task::none();
        };
        let rows: Vec<(&str, bool)> = crate::omnichannel::sidebar::step_order(
            &s.id,
            &s.workspace,
            &s.sidebar.sections,
            &self.ui_state.collapsed_sections,
        )
        .into_iter()
        .map(|i| (i.rid.as_str(), i.badge.count > 0 || i.badge.dot))
        .collect();
        let Some(rid) = crate::desktop::shortcuts::target(&rows, s.active_room.as_deref(), step)
            .map(str::to_owned)
        else {
            return Task::none();
        };
        self.open_room(&sid, &rid)
    }

    fn on_sidebar(&mut self, sid: ServerId, m: sidebar::Message) -> Task<Message> {
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
            sidebar::Out::Directory => self.navigate(component::Nav::Directory),
            sidebar::Out::Create => self.navigate(component::Nav::CreateRoom(
                crate::dialogs::create_room::Kind::Channel,
            )),
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

    fn mark_active_room_read(&mut self) -> Task<Message> {
        let Some(sid) = self.active.clone() else {
            return Task::none();
        };
        let Some(s) = self.servers.get_mut(&sid) else {
            return Task::none();
        };
        let Some(rid) = s.active_room.clone() else {
            return Task::none();
        };
        let epoch = s.generation;
        let Some(rv) = s.rooms.get_mut(&rid) else {
            return Task::none();
        };
        // The room read mark only when wanted (an unread mark made by the
        // user stays); the open thread is checked on its own (`tunread`).
        let unread = rv.wants_auto_read(&s.workspace);
        let ctx = UpdateCtx {
            server_id: &s.id,
            server: &s.url,
            rid: &rid,
            workspace: &mut s.workspace,
            session: s.session.as_ref(),
            images: &self.images,
            prefs: &self.config.ui,
        };
        let thread = rv.mark_thread_read(&ctx);
        let t = if unread {
            rv.mark_read(&ctx)
        } else {
            Task::none()
        };
        Self::room_task(&sid, epoch, &rid, Task::batch([t, thread]))
    }

    fn on_room(
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
        let action = rv.update(m, &mut ctx);
        let task = Self::room_task(&sid, epoch, &rid, action.task);
        Task::batch([task, self.handle_effects(&sid, &rid, action.effects)])
    }

    /// Runs `f` on the open room views of `sid` (all, or only `only`).
    fn for_rooms(
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

    fn handle_effects(
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
                | room::Effect::OpenPanel(_)
                | room::Effect::ClosePanel
                | room::Effect::ShowMessage(_)
                | room::Effect::Compose(_)
                | room::Effect::Omni(_) => Task::none(),
                room::Effect::MarkUnread(mid) => self.mark_unread(sid, rid, mid),
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
        let a = self.activate(sid.clone());
        let nav = self.navigate(component::Nav::CreateDiscussion {
            rid: rid.to_owned(),
            pmid,
        });
        Task::batch([a, nav])
    }

    /// Hands a dropped file to the composer of the active room.
    fn route_file_drop(&mut self, path: std::path::PathBuf) -> Task<Message> {
        let Some(sid) = self.active.clone() else {
            return Task::none();
        };
        let Some(s) = self.servers.get(&sid) else {
            return Task::none();
        };
        let Some(rid) = s.active_room.clone().filter(|r| s.rooms.contains_key(r)) else {
            return Task::none();
        };
        Task::done(Message::Room(
            sid,
            s.generation,
            rid,
            room::Message::Composer(room::composer::Message::FilesDropped(vec![path])),
        ))
    }

    /// Video-call messages (`crate::videoconf`) and their effects.
    fn on_calls(&mut self, m: crate::videoconf::Message) -> Task<Message> {
        let (task, effects) = crate::videoconf::update(self, m);
        self.call_effects(task, effects)
    }

    fn call_effects(
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

    fn on_server_event(&mut self, sid: ServerId, ev: ServerEvent) -> Task<Message> {
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
            ServerEvent::Event(ev) => match session::apply_event(&mut s.workspace, &ev) {
                Followup::None => Task::none(),
                Followup::Notification(n) => {
                    let n = crate::e2e::notify::prepare(&s.workspace, n);
                    let name = s.name.clone();
                    let server_active =
                        self.active.as_ref() == Some(&sid) && matches!(self.screen, Screen::Server);
                    let focused_rid = s.active_room.as_deref();
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
    fn resync(&mut self, sid: &ServerId) -> Task<Message> {
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

    fn on_initial_loaded(
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
        // A queued room link wins over the last room.
        if let Some((rid, path, _)) = s.pending_room.take() {
            tasks.push(self.open_room_link(&sid, rid, path));
        } else if s.active_room.is_none()
            && let Some(rid) = last_room.filter(|r| s.workspace.room(r).is_some())
        {
            tasks.push(self.open_room(&sid, &rid));
        } else {
            tasks.push(self.for_rooms(&sid, None, |rv, ctx| rv.on_reconnect(ctx)));
        }
        Task::batch(tasks)
    }

    fn on_resynced(
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

    // --- deep links ------------------------------------------------------------

    fn handle_link(&mut self, url: &str) -> Task<Message> {
        let Some(link) = parse_deep_link(url) else {
            tracing::info!("ignoring unrecognised argument");
            return Task::none();
        };
        let servers: Vec<(ServerId, ServerUrl)> = self
            .servers
            .iter()
            .map(|(id, s)| (id.clone(), s.url.clone()))
            .collect();
        let decision = deeplink::decide(&link, &self.pending_logins(), &servers, Instant::now());
        tracing::info!(?decision, "deep link");
        self.apply_decision(decision)
    }

    fn apply_decision(&mut self, decision: Decision) -> Task<Message> {
        match decision {
            Decision::LoginOAuth {
                server_id,
                credential_token,
                credential_secret,
            } => self.complete_login(
                &server_id,
                LoginRequest::OAuth {
                    credential_token,
                    credential_secret,
                },
            ),
            Decision::LoginSaml {
                server_id,
                credential_token,
            } => self.complete_login(&server_id, LoginRequest::Saml { credential_token }),
            Decision::ConfirmToken {
                server_id,
                user_id,
                token,
            } => {
                let creds = Credentials::new(user_id, token);
                let Some(s) = self.servers.get_mut(&server_id) else {
                    return Task::none();
                };
                let Some(l) = s.login.as_mut() else {
                    return Task::none();
                };
                // The pending login accepts this one link only.
                l.claim_token(&creds);
                let (url, tls) = (s.url.clone(), s.tls.clone());
                let c = creds.clone();
                Task::perform(
                    async move {
                        let rest = RestClient::new(url, &tls)?;
                        rest.set_credentials(Some(c));
                        rest.me().await
                    },
                    move |r| {
                        Message::TokenUser(
                            server_id.clone(),
                            creds.clone(),
                            r.map_err(Failure::from),
                        )
                    },
                )
            }
            Decision::DuplicateToken { server_id } => {
                // A second account link for one browser login: someone else
                // is sending links. Start over.
                self.drop_token_confirmations(&server_id);
                if let Some(l) = self
                    .servers
                    .get_mut(&server_id)
                    .and_then(|s| s.login.as_mut())
                {
                    l.cancel_pending();
                    l.error = Some(t!("deeplink.duplicate_token").into_owned());
                }
                Task::none()
            }
            Decision::AuthError { server_id, error } => {
                if let Some(l) = self
                    .servers
                    .get_mut(&server_id)
                    .and_then(|s| s.login.as_mut())
                {
                    l.show_link_error(&error);
                }
                Task::none()
            }
            Decision::OpenRoom {
                server_id,
                rid,
                path,
            } => {
                let a = self.activate(server_id.clone());
                Task::batch([a, self.open_room_link(&server_id, rid, path)])
            }
            Decision::OpenInvite { server_id, .. } => {
                let a = self.activate(server_id);
                Task::batch([
                    a,
                    self.toast(BannerKind::Info, t!("app.not_yet"), String::new()),
                ])
            }
            Decision::OfferAddServer { url } => {
                if self
                    .modals
                    .iter()
                    .any(|m| matches!(m, Modal::OfferAddServer { .. }))
                {
                    return Task::none();
                }
                self.modals.push(Modal::OfferAddServer { url });
                Task::none()
            }
            Decision::OtherOrigin { server_id, url } => {
                let name = self
                    .servers
                    .get(&server_id)
                    .map(|s| s.url.as_str().to_owned())
                    .unwrap_or_default();
                self.toast(
                    BannerKind::Warning,
                    t!("deeplink.other_origin_title"),
                    t!(
                        "deeplink.other_origin_body",
                        link = url.as_str(),
                        server = name
                    ),
                )
            }
            Decision::Ignore(reason) => {
                tracing::info!(reason, "deep link ignored");
                Task::none()
            }
        }
    }

    /// Opens the room of a link, or queues it until the server has loaded.
    fn open_room_link(
        &mut self,
        sid: &ServerId,
        rid: Option<String>,
        path: Option<String>,
    ) -> Task<Message> {
        let stamp = self.next_stamp();
        let Some(s) = self.servers.get_mut(sid) else {
            return Task::none();
        };
        if s.workspace.last_sync().is_none() {
            s.pending_room = Some((rid, path, stamp));
            let sid = sid.clone();
            return Task::perform(tokio::time::sleep(ROOM_LINK_TTL), move |_| {
                Message::RoomLinkExpired(sid.clone(), stamp)
            });
        }
        let target = rid.filter(|r| s.workspace.room(r).is_some()).or_else(|| {
            let (_, name) = deeplink::room_name_from_path(path.as_deref()?)?;
            find_room(&s.workspace, &deeplink::percent_decode(name))
        });
        match target {
            Some(rid) => {
                let mid = path.as_deref().and_then(deeplink::message_id_from_path);
                self.open_room_at(sid, &rid, mid.as_deref())
            }
            None => match path
                .as_deref()
                .and_then(|p| self.resolve_room_link(sid, p, None))
            {
                Some(task) => task,
                None => self.toast(
                    BannerKind::Warning,
                    t!("deeplink.room_not_found"),
                    String::new(),
                ),
            },
        }
    }

    /// Opens room `rid` and shows message `mid` in it, if given.
    fn open_room_at(&mut self, sid: &ServerId, rid: &str, mid: Option<&str>) -> Task<Message> {
        let open = self.open_room(sid, rid);
        let focus = match mid {
            Some(mid) => self.focus_message(sid, rid, mid),
            None => Task::none(),
        };
        Task::batch([open, focus])
    }

    /// The room of a link was fetched: put it into the workspace (a preview
    /// when not joined) and open it.
    fn room_link_resolved(
        &mut self,
        sid: &ServerId,
        mid: Option<String>,
        fallback: Option<String>,
        r: Res<open::Opened>,
    ) -> Task<Message> {
        match (r, self.servers.get_mut(sid)) {
            (Ok(o), Some(s)) => {
                let rid = open::apply(&mut s.workspace, o);
                self.open_room_at(sid, &rid, mid.as_deref())
            }
            (Ok(_), None) => Task::none(),
            (Err(e), _) => {
                tracing::debug!(server = %sid, error = %e, "room of a link not resolved");
                match fallback {
                    Some(url) => self.open_url(url),
                    None => self.toast(
                        BannerKind::Warning,
                        t!("deeplink.room_not_found"),
                        String::new(),
                    ),
                }
            }
        }
    }

    /// Shows message `mid` in the open view of `rid` (permalinks).
    fn focus_message(&mut self, sid: &ServerId, rid: &str, mid: &str) -> Task<Message> {
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
        let action = rv.focus_message(mid, &mut ctx);
        Task::batch([
            Self::room_task(sid, epoch, rid, action.task),
            self.handle_effects(sid, rid, action.effects),
        ])
    }

    fn complete_login(&mut self, sid: &ServerId, req: LoginRequest) -> Task<Message> {
        let a = self.activate(sid.clone());
        let Some(l) = self.servers.get_mut(sid).and_then(|s| s.login.as_mut()) else {
            return a;
        };
        let sid = sid.clone();
        let t = l
            .complete_with(req)
            .map(move |m| Message::Login(sid.clone(), m));
        Task::batch([a, t])
    }

    /// The `me` lookup of a Token link finished: ask for confirmation, but only
    /// while the browser login that accepted this exact link is still pending.
    fn on_token_user(
        &mut self,
        sid: ServerId,
        creds: Credentials,
        r: crate::error::Res<User>,
    ) -> Task<Message> {
        let live = self
            .servers
            .get(&sid)
            .and_then(|s| s.login.as_ref())
            .is_some_and(|l| l.token_claim_matches(&creds, Instant::now()));
        if !live {
            tracing::info!("token lookup finished without a matching pending login");
            return Task::none();
        }
        let user = match r {
            Ok(u) if u.id == creds.user_id => u,
            Ok(_) => {
                return self.toast(
                    BannerKind::Error,
                    t!("deeplink.token_rejected"),
                    String::new(),
                );
            }
            Err(e) => {
                if e.error().is_transient()
                    && let Some(l) = self.servers.get_mut(&sid).and_then(|s| s.login.as_mut())
                {
                    // The same link may be delivered again.
                    l.release_token(&creds);
                }
                return self.toast(
                    BannerKind::Error,
                    t!("deeplink.token_rejected"),
                    e.user_text(),
                );
            }
        };
        self.drop_token_confirmations(&sid);
        self.confirm_queue.push_back((sid, creds, Box::new(user)));
        Task::none()
    }

    // --- modals ----------------------------------------------------------------

    fn open_register_handler(&mut self) -> Task<Message> {
        self.modals.push(Modal::RegisterHandler {
            current: None,
            loaded: false,
            busy: false,
            error: None,
            warnings: Vec::new(),
        });
        query_handler_task()
    }

    fn on_modal(&mut self, m: ModalMessage) -> Task<Message> {
        match m {
            ModalMessage::ConfirmToken => {
                let Some(Modal::ConfirmToken {
                    server_id,
                    credentials,
                    user,
                    armed: true,
                    ..
                }) = self.modals.last()
                else {
                    return Task::none();
                };
                let (server_id, credentials, user) =
                    (server_id.clone(), credentials.clone(), user.clone());
                self.drop_token_confirmations(&server_id);
                let Some(l) = self
                    .servers
                    .get_mut(&server_id)
                    .and_then(|s| s.login.as_mut())
                else {
                    return Task::none();
                };
                // Re-check: the login that accepted this link must still be live.
                let live = l.token_claim_matches(&credentials, Instant::now());
                l.cancel_pending();
                if !live {
                    return Task::none();
                }
                let a = self.activate(server_id.clone());
                let r = LoginResult {
                    user_id: credentials.user_id,
                    auth_token: credentials.auth_token,
                    me: Some(*user),
                };
                Task::batch([a, self.logged_in(&server_id, r, TokenKind::Login)])
            }
            ModalMessage::AddOfferedServer => {
                let Some(Modal::OfferAddServer { url }) = self.modals.pop() else {
                    return Task::none();
                };
                self.screen = Screen::AddServer(Box::new(
                    AddServer::new(!self.servers.is_empty()).with_url(url.as_str()),
                ));
                Task::done(Message::AddServer(add_server::Message::Submit))
            }
            ModalMessage::OpenRegisterHandler => {
                self.modals.pop();
                self.open_register_handler()
            }
            ModalMessage::RegisterHandler => {
                for m in &mut self.modals {
                    if let Modal::RegisterHandler { busy, .. } = m {
                        *busy = true;
                    }
                }
                Task::perform(
                    async {
                        tokio::task::spawn_blocking(|| {
                            let env = XdgEnv::from_env().ok_or("no data directory")?;
                            let (exe, _) = handler_executable()?;
                            url_handler::register(&env, &exe)
                                .map(|p| p.display().to_string())
                                .map_err(|e| e.to_string())
                        })
                        .await
                        .unwrap_or_else(|e| Err(e.to_string()))
                    },
                    Message::HandlerRegistered,
                )
            }
            ModalMessage::ConfirmRemove(sid) => {
                self.modals.pop();
                self.remove_server(&sid)
            }
        }
    }

    // --- subscriptions -----------------------------------------------------------

    pub fn subscription(&self) -> Subscription<Message> {
        let mut subs = Vec::new();
        for s in self.servers.values() {
            if let Some(spec) = s.spec() {
                subs.push(
                    session::subscription(spec)
                        .map(|(sid, generation, ev)| Message::Server(sid, generation, ev)),
                );
            }
            subs.push(crate::e2e::subscription(s));
            for (rid, rv) in &s.rooms {
                subs.push(
                    rv.subscription()
                        .with((s.id.clone(), s.generation, rid.clone()))
                        .map(|((sid, epoch, rid), m)| Message::Room(sid, epoch, rid, m)),
                );
            }
        }
        if let Some(slot) = &self.listener {
            subs.push(single_instance::subscription(slot.clone()).map(Message::DeepLinks));
        }
        if self.config.ui.theme == ThemePref::System {
            subs.push(iced::system::theme_changes().map(Message::SystemTheme));
        }
        subs.push(iced::event::listen_with(
            |event, _status, _id| match event {
                iced::Event::Window(window::Event::Focused) => Some(Message::WindowFocused(true)),
                iced::Event::Window(window::Event::Unfocused) => {
                    Some(Message::WindowFocused(false))
                }
                _ => None,
            },
        ));
        if self
            .servers
            .values()
            .any(|s| s.workspace.typing().next_expiry().is_some())
        {
            subs.push(iced::time::every(Duration::from_secs(1)).map(|_| Message::Tick));
        }
        // Image misses recorded while drawing a frame are fetched right after it.
        subs.push(iced::event::listen_with(
            |event, _status, _id| match event {
                iced::Event::Window(window::Event::RedrawRequested(_))
                    if images::VIEW_MISSES.swap(false, std::sync::atomic::Ordering::Relaxed) =>
                {
                    Some(Message::FlushImages)
                }
                _ => None,
            },
        ));
        subs.push(window::close_requests().map(Message::CloseRequested));
        // Files dropped onto the window go to the active composer.
        subs.push(iced::event::listen_with(
            |event, _status, _id| match event {
                iced::Event::Window(window::Event::FileDropped(path)) => {
                    Some(Message::FileDropped(path))
                }
                _ => None,
            },
        ));
        // Ctrl+K: quick switcher.
        subs.push(iced::keyboard::listen().filter_map(|ev| match ev {
            iced::keyboard::Event::KeyPressed {
                key,
                modifiers,
                physical_key,
                ..
            } if modifiers.command() && key.to_latin(physical_key) == Some('k') => {
                Some(Message::OpenQuickSwitcher)
            }
            _ => None,
        }));
        subs.push(self.desktop.subscription().map(Message::Desktop));
        subs.push(self.calls.subscription().map(Message::Calls));
        match &self.screen {
            Screen::Settings(c) => subs.push(c.subscription().map(Message::Settings)),
            Screen::Account(sid, c) => subs.push(
                c.subscription()
                    .with(sid.clone())
                    .map(|(sid, m)| Message::Account(sid, m)),
            ),
            Screen::Directory(sid, c) => subs.push(
                c.subscription()
                    .with(sid.clone())
                    .map(|(sid, m)| Message::Directory(sid, m)),
            ),
            Screen::Admin(sid, c) => subs.push(
                c.subscription()
                    .with(sid.clone())
                    .map(|(sid, m)| Message::Admin(sid, m)),
            ),
            _ => {}
        }
        for m in &self.modals {
            match m {
                Modal::CreateRoom(sid, c) => subs.push(
                    c.subscription()
                        .with(sid.clone())
                        .map(|(sid, m)| Message::CreateRoom(sid, m)),
                ),
                Modal::QuickSwitcher(sid, c) => subs.push(
                    c.subscription()
                        .with(sid.clone())
                        .map(|(sid, m)| Message::QuickSwitcher(sid, m)),
                ),
                _ => {}
            }
        }
        for s in self.servers.values() {
            subs.push(
                s.status_menu
                    .subscription()
                    .with(s.id.clone())
                    .map(|(sid, m)| Message::StatusMenu(sid, m)),
            );
        }
        Subscription::batch(subs)
    }
}

/// The executable `rocketchat://` links would start (blocking).
fn handler_executable() -> Result<(PathBuf, Vec<String>), String> {
    let current = std::env::current_exe().map_err(|e| e.to_string())?;
    let appimage = std::env::var_os("APPIMAGE");
    let appdir = std::env::var_os("APPDIR");
    url_handler::handler_executable(&current, appimage.as_deref(), appdir.as_deref())
        .map_err(|e| e.to_string())
}

fn query_handler_task() -> Task<Message> {
    Task::perform(
        async {
            tokio::task::spawn_blocking(|| {
                let check = handler_executable().map(|(_, w)| w);
                let Some(env) = XdgEnv::from_env() else {
                    return (None, false, check);
                };
                let current = url_handler::current_handler(&env);
                let registered = current.as_deref() == Some(url_handler::DESKTOP_FILE);
                (current, registered, check)
            })
            .await
            .unwrap_or_else(|e| (None, false, Err(e.to_string())))
        },
        |(c, reg, check)| Message::HandlerQueried(c, reg, check),
    )
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn openable_urls() {
        use rustket_core::uikit::is_openable_url;
        assert!(is_openable_url("https://chat.example.com/oauth?x=1"));
        assert!(is_openable_url("http://localhost:3000/home"));
        assert!(is_openable_url("mailto:a@b.c"));
        for bad in [
            "smb://attacker.example/share",
            "file:///etc/passwd",
            "/usr/bin/xterm",
            "javascript:alert(1)",
            "x.desktop",
            "https://",
            "mailto:",
        ] {
            assert!(!is_openable_url(bad), "{bad}");
        }
    }

    #[test]
    fn sync_backoff_grows_and_caps() {
        assert_eq!(sync_backoff(1), Duration::from_secs(2));
        assert_eq!(sync_backoff(2), Duration::from_secs(4));
        assert_eq!(sync_backoff(30), Duration::from_secs(60));
    }
}
