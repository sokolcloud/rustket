//! Application state, update and subscriptions (`view` is in `app/view.rs`).

mod after_update;
mod components;
mod hosted_user;
mod inbox;
mod keys;
mod later;
mod links;
mod mention_groups;
mod reminders;
mod rooms;
mod secrets;
mod session;
mod star_kept;
mod subscription;
mod sync;
mod uikit;
mod view;
#[cfg(test)]
pub(crate) use view::layered;

use std::collections::{BTreeMap, VecDeque};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use iced::{Task, Theme};
use rocketchat::models::User;
use rocketchat::rest::LoginResult;
use rocketchat::{Credentials, ServerUrl};
use rust_i18n::t;
use rustket_core::ServerId;
use rustket_core::config::{AppConfig, Theme as ThemePref, TokenKind};
use rustket_core::room_list;
use rustket_core::secrets::{MemoryStore, SecretBackend, SecretStore, SwitchableStore};
use rustket_ui::Mode;
use rustket_ui::widget::{BannerKind, Toast};

use crate::component;
use crate::config_writer::ConfigWriter;
use crate::deeplink::PendingLogin;
use crate::images::{self, Images};
use crate::message::{Message, ModalMessage, SecretsReady};
use crate::room;
use crate::screens::add_server::{self, AddServer};
use crate::single_instance::ListenerSlot;
use crate::ui_state::UiState;
use crate::url_handler;
use crate::workspace::sidebar;
use crate::workspace::{ServerState, rail};
use secrets::open_secrets;
pub use star_kept::StarJob;
use subscription::{query_handler_task, query_maximized};

/// How long a toast stays.
const TOAST_TTL: Duration = Duration::from_secs(6);
/// How long a toast with an action button stays (it may need a decision).
const TOAST_ACTION_TTL: Duration = Duration::from_secs(20);
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
    /// Whether the window was opened with system decorations (`main.rs`);
    /// `None`: as `UiPrefs::system_title_bar` says.
    pub window_decorated: Option<bool>,
    /// Base directory of the offline cache (`crate::cache`); `None`: off.
    pub offline_cache: Option<PathBuf>,
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
    /// Turning on `UiPrefs::secrets_memory_only`: the consequences, and the
    /// servers whose generated E2E password is not saved yet.
    SecretsMemoryOnly {
        pending: Vec<ServerId>,
    },
    CreateRoom(ServerId, Box<crate::dialogs::create_room::CreateRoom>),
    QuickSwitcher(ServerId, Box<crate::dialogs::quick_switcher::QuickSwitcher>),
    /// The keyboard shortcuts cheat sheet (Ctrl/Cmd+/).
    Shortcuts,
    /// "Mark all as read" (Shift+Esc) of a server: the rooms fixed when it
    /// opened, and the progress once confirmed.
    ReadAll {
        server_id: ServerId,
        rids: Vec<String>,
        /// Confirmed and running: the dialog cannot be dismissed.
        busy: bool,
        done: usize,
        failed: usize,
        /// Identifies this run in the read queue (`reads::Origin::ReadAll`).
        stamp: u64,
    },
}

pub struct App {
    pub config: AppConfig,
    pub config_dir: Option<PathBuf>,
    pub ui_state: UiState,
    writer: Option<ConfigWriter>,
    /// The secret store: `secret_switch` as handed to sessions and E2E.
    pub secrets: Option<Arc<dyn SecretStore>>,
    /// The same instance, for switching its backend at runtime.
    pub secret_switch: Option<Arc<SwitchableStore>>,
    pub secret_backend: Option<SecretBackend>,
    /// `SwitchableStore::apply_policy` is running.
    pub secrets_switching: bool,
    /// The settings tab shown last this session (not saved).
    pub settings_tab: crate::screens::settings::Tab,
    pub servers: BTreeMap<ServerId, ServerState>,
    pub active: Option<ServerId>,
    pub screen: Screen,
    pub toasts: Vec<Toast<Message>>,
    next_toast: u64,
    pub modals: Vec<Modal>,
    pub images: Images,
    pub system_mode: Option<Mode>,
    /// External colour themes (`crate::themes`).
    pub themes: crate::themes::Themes,
    /// External translations (`crate::locales`).
    pub locales: crate::locales::Locales,
    pub focused: bool,
    pub handler_registered: bool,
    listener: Option<ListenerSlot>,
    queued_links: Vec<String>,
    booted: bool,
    open_urls: bool,
    pub secrets_mode: SecretsMode,
    stamp: u64,
    /// Token confirmations waiting for the single confirmation slot.
    confirm_queue: VecDeque<(ServerId, Credentials, Box<User>)>,
    save_stamp: u64,
    pub desktop: crate::desktop::Desktop,
    /// (unread rooms, mentions) over all servers, last given to `desktop`.
    last_unread: (u64, u64),
    /// Video calls: ringing, popups, call dialogs (`crate::videoconf`).
    pub calls: crate::videoconf::Calls,
    /// Instance of the create-room / quick-switcher dialog opened last: task
    /// results of a closed dialog never reach the next one.
    dialog_instances: (u64, u64),
    /// Custom title bar pointer and maximize state.
    pub titlebar: crate::workspace::titlebar::TitleBar,
    /// Whether the window has system decorations now (kept in step with
    /// `UiPrefs::system_title_bar` by `sync_decorations`).
    pub decorated: bool,
    /// Offline cache (`crate::cache`).
    pub offline: crate::cache::Caches,
    /// The paced star queue ("Move kept threads to Favorites", Later).
    star_kept: star_kept::StarKept,
    /// Reminder firing and notifications (`app::reminders`).
    reminder_timer: reminders::ReminderTimer,
    /// In-app updates (`crate::updater`).
    #[cfg(feature = "self-update")]
    pub updater: crate::updater::Updater,
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
        let mut locales =
            crate::locales::Locales::new(config_dir.as_deref().map(crate::locales::locales_dir));
        locales.reload(&config.ui.language);
        let desktop = if flags.desktop {
            crate::desktop::Desktop::system(&config.ui)
        } else {
            crate::desktop::Desktop::new()
        };
        // A window that starts hidden may never report losing the focus.
        let focused = !(flags.desktop && config.ui.start_minimized);
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
        let decorated = flags.window_decorated.unwrap_or(config.ui.system_title_bar);
        let mut themes =
            crate::themes::Themes::new(config_dir.as_deref().map(crate::themes::themes_dir));
        themes.rescan();
        themes.apply(config.ui.theme_name.as_deref());
        let app = Self {
            config,
            config_dir,
            ui_state,
            writer,
            secrets: None,
            secret_switch: None,
            secret_backend: None,
            secrets_switching: false,
            settings_tab: Default::default(),
            servers,
            active,
            screen: Screen::Loading,
            toasts: Vec::new(),
            next_toast: 0,
            modals: Vec::new(),
            images: Images::new(cache_dir),
            system_mode: None,
            themes,
            locales,
            focused,
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
            dialog_instances: (0, 0),
            titlebar: Default::default(),
            decorated,
            offline: crate::cache::Caches::new(flags.offline_cache.clone()),
            star_kept: Default::default(),
            reminder_timer: Default::default(),
            #[cfg(feature = "self-update")]
            updater: crate::updater::Updater::new(flags.desktop),
        };
        let wanted: Vec<(ServerId, String)> = app
            .config
            .servers
            .iter()
            .filter_map(|s| Some((s.id.clone(), s.user_id.clone()?)))
            .collect();
        let mode = flags.secrets;
        let policy =
            rustket_core::secrets::FilePolicy::memory_only(app.config.ui.secrets_memory_only);
        let open = Task::perform(
            async move { tokio::task::spawn_blocking(move || open_secrets(mode, policy, wanted)).await },
            |r| match r {
                Ok(s) => Message::SecretsReady(s),
                Err(e) => {
                    tracing::error!(error = %e, "secret store task failed");
                    Message::SecretsReady(SecretsReady {
                        store: Arc::new(SwitchableStore::new(
                            Box::new(MemoryStore::new()),
                            SecretBackend::Memory,
                            None,
                        )),
                        tokens: Vec::new(),
                        notices: Vec::new(),
                    })
                }
            },
        );
        let theme = iced::system::theme().map(Message::SystemTheme);
        let handler = query_handler_task();
        // Media opened with the system player in earlier runs.
        let media = crate::room::timeline::transfer::purge_media_task(
            app.images.disk_dir(),
            true,
            Message::Noop,
        );
        // The window manager may map the window maximized.
        let maximized = if app.custom_chrome() {
            query_maximized()
        } else {
            Task::none()
        };
        // Image disk cache cap (Settings → Storage).
        images::set_disk_cap_mb(app.config.ui.image_cache_mb);
        let trim = images::trim_task(app.images.disk_dir(), Message::Noop);
        (
            app,
            Task::batch([open, theme, handler, media, maximized, trim]),
        )
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
        self.push_toast(Toast::new(id, kind, title, body))
    }

    /// [`Self::toast`] with an action button (`label`, then `action` runs
    /// and the toast closes).
    pub fn toast_with_action(
        &mut self,
        kind: BannerKind,
        title: impl Into<String>,
        body: impl Into<String>,
        label: impl Into<String>,
        action: Message,
    ) -> Task<Message> {
        self.next_toast += 1;
        let id = self.next_toast;
        let toast = Toast::new(id, kind, title, body)
            .with_action(label, Message::ToastAction(id, Box::new(action)));
        self.push_toast(toast)
    }

    fn push_toast(&mut self, toast: Toast<Message>) -> Task<Message> {
        let id = toast.id;
        let ttl = if toast.action.is_some() {
            TOAST_ACTION_TTL
        } else {
            TOAST_TTL
        };
        self.toasts.push(toast);
        while self.toasts.len() > 4 {
            self.toasts.remove(0);
        }
        Task::perform(tokio::time::sleep(ttl), move |_| Message::CloseToast(id))
    }

    /// Queues the current config + UI state for the single writer thread.
    pub(crate) fn save_config(&self) -> Task<Message> {
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

    /// Opens `<config dir>/themes` in the file manager, creating it first.
    pub(crate) fn open_themes_folder(&mut self) -> Task<Message> {
        let Some(dir) = self.themes.dir.clone() else {
            return Task::none();
        };
        if let Err(e) = std::fs::create_dir_all(&dir) {
            return self.toast(
                BannerKind::Error,
                t!("settings.themes_folder_failed"),
                e.to_string(),
            );
        }
        if !self.open_urls {
            return Task::none();
        }
        let path = dir.display().to_string();
        Task::perform(crate::room::timeline::transfer::open_path(dir), move |r| {
            if r.is_err() {
                tracing::warn!("cannot open the themes folder");
                Message::ThemesFolderNotOpened(path.clone())
            } else {
                Message::Noop
            }
        })
    }

    pub(crate) fn open_locales_folder(&mut self) -> Task<Message> {
        let Some(dir) = self.locales.dir.clone() else {
            return Task::none();
        };
        if let Err(e) = std::fs::create_dir_all(&dir) {
            return self.toast(
                BannerKind::Error,
                t!("settings.locales_folder_failed"),
                e.to_string(),
            );
        }
        if !self.open_urls {
            return Task::none();
        }
        let path = dir.display().to_string();
        Task::perform(crate::room::timeline::transfer::open_path(dir), move |r| {
            if r.is_err() {
                tracing::warn!("cannot open the locales folder");
                Message::LocalesFolderNotOpened(path.clone())
            } else {
                Message::Noop
            }
        })
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
        self.drop_orphaned_read_all();
        Task::batch([task, after, confirm, unread])
    }

    fn update_inner(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::Noop => Task::none(),
            Message::FileDropped(path) => self.route_file_drop(path),
            Message::Go(go) => self.go(go),
            Message::Dialog(instance, m) => self.on_dialog_result(instance, *m),
            Message::E2e(sid, epoch, m) => self.on_e2e(sid, epoch, m),
            Message::Cache(m) => crate::cache::update(self, m),
            #[cfg(feature = "self-update")]
            Message::Updater(m) => crate::updater::app::update(self, m),
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
                // The room read mark follows in `sync_room_visibility`.
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
            Message::ToastAction(id, action) => {
                self.toasts.retain(|t| t.id != id);
                self.update(*action)
            }
            Message::SecretsPolicyApplied(on, backend, error) => {
                self.on_secrets_policy_applied(on, backend, error)
            }
            Message::SecretsPurged(cleared, error) => self.on_secrets_purged(&cleared, error),
            Message::SecretsPendingFound(found) => {
                if let Some(Modal::SecretsMemoryOnly { pending }) = self.modals.last_mut() {
                    for sid in found {
                        if !pending.contains(&sid) {
                            pending.push(sid);
                        }
                    }
                }
                Task::none()
            }
            Message::ThemesFolderNotOpened(path) => self.toast(
                BannerKind::Error,
                t!("settings.themes_folder_open_failed"),
                path,
            ),
            Message::LocalesFolderNotOpened(path) => self.toast(
                BannerKind::Error,
                t!("settings.locales_folder_open_failed"),
                path,
            ),
            Message::ShowAddServer if !self.may_leave(Message::ShowAddServer) => Task::none(),
            Message::ShowAddServer => {
                self.screen = Screen::AddServer(Box::new(AddServer::new(!self.servers.is_empty())));
                iced::widget::operation::focus(add_server::URL_INPUT_ID)
            }
            Message::AddServer(m) => self.on_add_server(m),
            Message::Login(sid, m) => self.on_login(sid, m),
            Message::Rail(m) if !self.may_leave(Message::Rail(m.clone())) => Task::none(),
            Message::Rail(m) => match m {
                rail::Message::Select(sid) => self.activate(sid),
                rail::Message::Add => Task::done(Message::ShowAddServer),
                rail::Message::Settings => self.navigate(component::Nav::Settings),
            },
            Message::Sidebar(sid, m) => self.on_sidebar(sid, m),
            Message::Settings(m) => {
                let task = self.on_settings(m);
                Task::batch([task, self.sync_decorations()])
            }
            Message::TitleBar(m) => self.on_title_bar(m),
            Message::Account(sid, m) => self.on_account(sid, m),
            Message::Directory(sid, m) => self.on_directory(sid, m),
            Message::Admin(sid, m) => self.on_admin(sid, m),
            Message::CreateRoom(sid, m) => self.on_create_room(sid, m),
            Message::QuickSwitcher(sid, m) => self.on_quick_switcher(sid, m),
            Message::StatusMenu(sid, m) => self.on_status_menu(sid, m),
            Message::UiKit(sid, epoch, m) => self.on_uikit_in(sid, epoch, m),
            Message::Desktop(m) => self.on_desktop(m),
            Message::Calls(m) => self.on_calls(m),
            Message::Shortcut(s) => self.on_shortcut(s),
            Message::Reads(sid, epoch, m) => self.on_reads(sid, epoch, m),
            Message::Inbox(sid, epoch, m) => self.on_inbox(sid, epoch, m),
            Message::CatchUpKey(k) => self.on_catch_up_key(k),
            Message::Later(sid, epoch, m) => self.on_later(sid, epoch, m),
            Message::LaterKey(k) => self.on_later_key(k),
            Message::LaterCopy(sid, epoch, r) => self.on_later_copy(sid, epoch, r),
            Message::LaterSync(sid, epoch, m) => self.on_later_sync(sid, epoch, m),
            Message::ReminderSync(sid, epoch, m) => self.on_reminder_sync(sid, epoch, m),
            Message::RemindersDue(at) => self.on_reminders_due(at),
            Message::ReminderNotify(m) => self.on_reminder_notify(m),
            Message::ReminderAction(a) => self.on_reminder_action(a),
            Message::ReminderFetched(sid, epoch, mid, at, r) => {
                self.on_reminder_fetched(sid, epoch, mid, at, r)
            }
            Message::MessageStarred {
                sid,
                epoch,
                m,
                on,
                job,
                result,
            } => self.on_message_starred(sid, epoch, *m, on, job, result),
            Message::FocusNext(forward) => rustket_ui::widget::focus_cycle(forward),
            Message::FocusRegion(forward) => self.focus_region(forward),
            Message::Escape => self.on_escape(),
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
            Message::Threads(sid, epoch, m) => self.on_threads(sid, epoch, m),
            Message::Flags(sid, epoch, m) => self.on_flags(sid, epoch, m),
            Message::ServicePushed(sid, epoch, o) => self.on_service_pushed(sid, epoch, o),
            Message::MentionGroups(sid, epoch, m) => self.on_mention_groups(sid, epoch, m),
            Message::MentionGroupChecked(sid, check, report) => {
                self.mention_group_checked(sid, *check, report)
            }
            Message::SetFlag(sid, edit) => self.set_flag(&sid, edit),
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
                if !self.may_leave(Message::CloseRequested(id)) {
                    return Task::none();
                }
                self.flush_config();
                crate::cache::on_quit(self);
                iced::window::close(id)
            }
            Message::FlushImages => Task::none(),
            Message::PurgeStaleMedia => Task::batch([
                crate::room::timeline::transfer::purge_media_task(
                    self.images.disk_dir(),
                    true,
                    Message::Noop,
                ),
                Task::perform(crate::e2e::files::purge_private_media(true), |()| {
                    Message::Noop
                }),
                images::trim_task(self.images.disk_dir(), Message::Noop),
            ]),
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
            Message::SamlLinkLogin(sid, token, r) => self.on_saml_link_login(sid, token, r),
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
            Message::StarKeptNext => self.on_star_kept_next(),
            Message::ThreadStarred {
                sid,
                epoch,
                rid,
                tmid,
                on,
                queued,
                result,
            } => match result {
                // The parent's realtime update confirms it; a queued star
                // of a kept thread is archived in Later.
                Ok(()) if queued => self.star_kept_done(&sid, &rid, &tmid),
                Ok(()) => Task::none(),
                Err(e) => {
                    tracing::warn!(server = %sid, %rid, %tmid, on, error = %e, "thread star failed");
                    if let Some(s) = self.servers.get_mut(&sid)
                        && s.is_current(epoch)
                    {
                        s.workspace.set_thread_starred(&rid, &tmid, !on);
                    }
                    if queued {
                        self.star_kept_failed(sid, rid, tmid, &e);
                        return Task::none();
                    }
                    let title = if on {
                        t!("sidebar.star_thread_failed")
                    } else {
                        t!("sidebar.unstar_thread_failed")
                    };
                    self.toast(BannerKind::Error, title, e.user_text())
                }
            },
            Message::SidebarFavoriteDone(sid, on, r) => match r {
                // The subscription update moves the row.
                Ok(()) => Task::none(),
                Err(e) => {
                    tracing::warn!(server = %sid, error = %e, "rooms.favorite failed");
                    let title = crate::room::panel::common::RoomAction::Favorite(on).label();
                    self.toast(
                        BannerKind::Error,
                        title,
                        crate::room::panel::common::failure_text(&e),
                    )
                }
            },
            Message::SidebarThreadDone(sid, call, r) => match r {
                // The subscription and parent updates bring the change.
                Ok(()) => Task::none(),
                Err(e) => {
                    tracing::warn!(server = %sid, ?call, error = %e, "thread call failed");
                    let title = match call {
                        sidebar::ThreadCall::MarkRead => t!("sidebar.thread_read_failed"),
                        sidebar::ThreadCall::MarkRoomRead => t!("sidebar.room_read_failed"),
                        sidebar::ThreadCall::Unfollow => t!("sidebar.thread_unfollow_failed"),
                    };
                    self.toast(BannerKind::Error, title, e.user_text())
                }
            },
            Message::Modal(m) => self.on_modal(m),
            // A running "Mark all as read" finishes first.
            Message::CloseModal
                if matches!(self.modals.last(), Some(Modal::ReadAll { busy: true, .. })) =>
            {
                Task::none()
            }
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
        // Keep the newly opened room's row in view.
        let reveal = s
            .sidebar
            .reveal(&rid, &self.config.ui, &self.ui_state.collapsed_sections);
        Task::batch([self.open_room(&sid, &rid), reveal])
    }

    /// F6 / Shift+F6 (`crate::focus`).
    fn focus_region(&self, forward: bool) -> Task<Message> {
        let room = self
            .active
            .as_ref()
            .and_then(|sid| self.servers.get(sid))
            .filter(|_| matches!(self.screen, Screen::Server))
            .and_then(|s| {
                let rid = s.active_room.as_deref()?;
                let rv = s.rooms.get(rid)?;
                let tmid = match rv.panel.kind() {
                    Some(room::panel::Kind::Thread(t)) => Some(t.as_str()),
                    _ => None,
                };
                Some((rid, tmid))
            });
        rustket_ui::widget::focus_region(crate::focus::regions(room), forward)
    }

    /// Escape no widget took (popovers, dialogs and focused inputs handle
    /// their own): on the server view the chain of `escape_on_server`
    /// (`app/keys.rs`); else close the settings, account, directory or
    /// administration page.
    fn on_escape(&mut self) -> Task<Message> {
        if !self.modals.is_empty() {
            return Task::none();
        }
        match &self.screen {
            Screen::Settings(_) => self.on_settings(crate::screens::settings::Message::Close),
            // Admin asks first when settings edits are unsaved.
            Screen::Account(sid, _) => Task::done(Message::Account(
                sid.clone(),
                crate::screens::account::Message::Close,
            )),
            Screen::Directory(sid, _) => Task::done(Message::Directory(
                sid.clone(),
                crate::screens::directory::Message::Close,
            )),
            Screen::Admin(sid, _) => Task::done(Message::Admin(
                sid.clone(),
                crate::screens::admin::Message::Close,
            )),
            Screen::Server => self.escape_on_server(),
            Screen::Loading | Screen::AddServer(_) => Task::none(),
        }
    }

    // --- modals ----------------------------------------------------------------

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
                let a = self.activate_for_login(&server_id);
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
                self.go(crate::message::Go::AddServer(url))
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
                        tokio::task::spawn_blocking(url_handler::register_current)
                            .await
                            .unwrap_or_else(|e| Err(e.to_string()))
                    },
                    Message::HandlerRegistered,
                )
            }
            ModalMessage::ConfirmRemove(sid) => {
                self.modals.pop();
                self.sign_out(&sid, crate::e2e::SignOut::RemoveServer)
            }
            ModalMessage::ConfirmSecretsMemoryOnly => {
                self.modals
                    .retain(|m| !matches!(m, Modal::SecretsMemoryOnly { .. }));
                self.set_secrets_memory_only(true)
            }
            ModalMessage::ConfirmReadAll => self.confirm_read_all(),
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
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
}
