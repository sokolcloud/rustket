//! Top-level application message. Components have their own message types,
//! wrapped here together with the server (and room) they belong to.

use std::sync::Arc;

use rocketchat::Credentials;
use rocketchat::models::User;
use rocketchat::rest::LoginResult;
use rustket_core::ServerId;
use rustket_core::secrets::{SecretBackend, SwitchableStore};
use rustket_core::sync::{DeltaData, InitialData, RoomSync};

use crate::error::Res;
use crate::images::FetchResult;
use crate::room;
use crate::screens::{add_server, login};
use crate::session::ServerEvent;
use crate::workspace::{rail, sidebar, titlebar};

/// The secret store opened at startup plus the tokens read from it.
#[derive(Clone)]
pub struct SecretsReady {
    /// The one store instance everyone gets (its backend can change at runtime).
    pub store: Arc<SwitchableStore>,
    /// `(server, token)` for every server with a stored user id.
    pub tokens: Vec<(ServerId, Option<String>)>,
    /// Localised notices from opening the store (fallbacks, migrations).
    pub notices: Vec<String>,
}

impl std::fmt::Debug for SecretsReady {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SecretsReady")
            .field("backend", &self.store.backend())
            .field("tokens", &self.tokens.len())
            .finish()
    }
}

/// A navigation away from the current screen that the admin screen's
/// unsaved-changes dialog may hold back; replayed after "Discard".
#[derive(Debug, Clone)]
pub enum Go {
    Nav(crate::component::Nav),
    /// Activate server `sid` and open room `rid` (and thread `tmid`).
    Room {
        sid: ServerId,
        rid: String,
        tmid: Option<String>,
    },
    /// The add-server screen, probing `url` (an offered unknown-host link).
    AddServer(rocketchat::ServerUrl),
    Logout {
        sid: ServerId,
        everywhere: bool,
    },
    /// The "create discussion" dialog of room `rid` on server `sid`.
    Discussion {
        sid: ServerId,
        rid: String,
        pmid: Option<String>,
    },
    /// Quit the application (tray menu).
    Quit,
}

#[derive(Debug, Clone)]
pub enum ModalMessage {
    ConfirmToken,
    AddOfferedServer,
    RegisterHandler,
    ConfirmRemove(ServerId),
    OpenRegisterHandler,
    /// Keep secrets in memory only (`UiPrefs::secrets_memory_only`): confirmed.
    ConfirmSecretsMemoryOnly,
    /// "Mark all as read" (Shift+Esc): confirmed.
    ConfirmReadAll,
}

#[derive(Debug, Clone)]
pub enum Message {
    SecretsReady(SecretsReady),
    SystemTheme(iced::theme::Mode),
    WindowFocused(bool),
    Tick,
    AddServer(add_server::Message),
    ShowAddServer,
    Login(ServerId, login::Message),
    Rail(rail::Message),
    TitleBar(titlebar::Message),
    Sidebar(ServerId, sidebar::Message),
    Settings(crate::screens::settings::Message),
    Account(ServerId, crate::screens::account::Message),
    Directory(ServerId, crate::screens::directory::Message),
    Admin(ServerId, crate::screens::admin::Message),
    CreateRoom(ServerId, crate::dialogs::create_room::Message),
    QuickSwitcher(ServerId, crate::dialogs::quick_switcher::Message),
    StatusMenu(ServerId, crate::workspace::status_menu::Message),
    /// UIKit controller (`crate::uikit`) of a server, in session epoch
    /// `ServerState::generation`.
    UiKit(ServerId, u64, crate::uikit::Msg),
    Desktop(crate::desktop::Message),
    /// Video conferences (`crate::videoconf`).
    Calls(crate::videoconf::Message),
    /// Omnichannel controller message: server, session epoch, message.
    Omni(ServerId, u64, crate::omnichannel::Message),
    /// Sidebar Threads loader message: server, session epoch, message.
    Threads(ServerId, u64, crate::workspace::threads::Msg),
    /// Flag sync message: server, session epoch, message.
    Flags(ServerId, u64, crate::workspace::flags::Msg),
    /// A push of the synced keys finished: server, session epoch, result.
    ServicePushed(ServerId, u64, crate::workspace::service_sync::Outcome),
    /// Mention group sync message: server, session epoch, message.
    MentionGroups(ServerId, u64, crate::workspace::mention_groups::Msg),
    /// The server's checks of a mention group the Settings asked for.
    MentionGroupChecked(
        ServerId,
        Box<crate::screens::settings::MentionGroupCheck>,
        crate::screens::settings::MentionCheckReport,
    ),
    /// A flag edit of the user (sidebar, room header, thread panel).
    SetFlag(ServerId, rustket_core::flags::FlagEdit),
    /// An app-wide keyboard shortcut (`crate::keymap`).
    Shortcut(crate::keymap::Shortcut),
    /// Read queue of a server (`crate::reads`): server, session epoch, message.
    Reads(ServerId, u64, crate::reads::Msg),
    /// The inbox views (`crate::inbox`): server, session epoch, message.
    Inbox(ServerId, u64, crate::inbox::Msg),
    /// Enter / Left / Right that no widget took (the catch-up view).
    CatchUpKey(crate::inbox::CatchUpKey),
    /// The Later view (`crate::later`): server, session epoch, message.
    Later(ServerId, u64, crate::later::Msg),
    /// The Drafts & scheduled view (`crate::drafts`): server, session
    /// epoch, message.
    Drafts(ServerId, u64, crate::drafts::Msg),
    /// The dialog of scheduled messages not sent on time.
    Missed(crate::drafts::missed::MissedMsg),
    /// Notification pauses and expiring statuses (`crate::quiet`).
    Quiet(crate::quiet::Msg),
    /// Up / Down / E that no widget took (the Later view).
    LaterKey(crate::later::LaterKey),
    /// The search view (`crate::search`): server, session epoch, message.
    Search(ServerId, u64, crate::search::Msg),
    /// A reminder copy fetched for the Later view (`None`: the pacing
    /// timer): server, session epoch, result.
    LaterCopy(ServerId, u64, Option<Res<Box<rocketchat::models::Message>>>),
    /// Later sync message: server, session epoch, message.
    LaterSync(ServerId, u64, crate::workspace::later_sync::LaterMsg),
    /// Reminder sync message: server, session epoch, message.
    ReminderSync(ServerId, u64, crate::workspace::later_sync::ReminderMsg),
    /// The reminder timer armed for this time fired.
    RemindersDue(std::time::Instant),
    /// A large-room mention check is due (`App::on_large_room_due`).
    LargeRoomDue,
    /// A thread reply's notification held for the reply itself
    /// (`desktop::notifications::wait_for_reply`), decided now.
    NotificationRecheck(ServerId, Box<rocketchat::session::DesktopNotification>),
    /// Reminder notifications (`crate::later::notify`).
    ReminderNotify(crate::later::notify::Msg),
    /// A reminder notification action, replayed after the admin screen's
    /// leave question.
    ReminderAction(crate::later::notify::ReminderAction),
    /// A due reminder's message was fetched for its notification: server,
    /// session epoch, message id, due time, result.
    ReminderFetched(
        ServerId,
        u64,
        String,
        i64,
        Res<Box<rocketchat::models::Message>>,
    ),
    /// Tab / Shift+Tab that no widget handled (modal dialogs cycle their own
    /// focus): move focus to the next (`true`) or previous input.
    FocusNext(bool),
    /// F6 / Shift+F6: focus the next (`true`) or previous region
    /// (`crate::focus`).
    FocusRegion(bool),
    /// Escape that no widget handled: close the main view or the right
    /// panel, mark the room read, or close the settings page
    /// (`App::on_escape`).
    Escape,
    SidebarResized(f32),
    /// Room component message: server, session epoch, room id.
    Room(ServerId, u64, String, room::Message),
    /// Session subscription output: server, session generation, event.
    Server(ServerId, u64, ServerEvent),
    InitialLoaded(ServerId, u64, Res<Box<InitialData>>),
    Resynced(ServerId, u64, Res<Box<DeltaData>>, Vec<Res<RoomSync>>),
    /// Retry a failed initial load / delta sync.
    RetrySync(ServerId, u64),
    /// A queued room link was not resolved in time.
    RoomLinkExpired(ServerId, u64),
    /// The room of a link the workspace did not have (a channel the user
    /// has not joined opens as a preview): message to show, URL to open in
    /// the browser if it cannot be resolved, and the fetched room.
    RoomLinkResolved(
        ServerId,
        Option<String>,
        Option<String>,
        Box<Res<crate::screens::directory::open::Opened>>,
    ),
    /// Enables the confirm button of the token confirmation shown with this stamp.
    ArmConfirm(u64),
    /// Debounced config save (sidebar resize) with this stamp.
    SaveDebounced(u64),
    /// The window asked to close: flush the config, then close.
    CloseRequested(iced::window::Id),
    /// Flush image misses recorded by the last frame.
    FlushImages,
    /// Hourly: delete media opened with the system player that is older
    /// than `transfer::MEDIA_TTL`.
    PurgeStaleMedia,
    /// Image fetch result: server, its cache generation at request time, URL.
    ImageLoaded(ServerId, u64, String, FetchResult),
    DeepLinks(Vec<String>),
    TokenUser(ServerId, Credentials, Res<User>),
    /// Login with a server-generated SAML token (`Decision::ConfirmSaml`).
    SamlLinkLogin(ServerId, String, Res<LoginResult>),
    TokenStored(ServerId, Result<(), String>),
    RetryConnection(ServerId),
    LoggedOut(ServerId, Result<(), String>),
    /// `subscriptions.unread` finished (the room was left before).
    MarkedUnread(ServerId, Res<()>),
    /// A thread row's server call (mark read, unfollow) finished.
    SidebarThreadDone(ServerId, crate::workspace::sidebar::ThreadCall, Res<()>),
    /// A room row's "Add to/Remove from favorites" (`rooms.favorite`) ended.
    SidebarFavoriteDone(ServerId, bool, Res<()>),
    /// `chat.starMessage`/`chat.unstarMessage` of a thread parent ended
    /// (reverted in the sidebar on failure).
    ThreadStarred {
        sid: ServerId,
        /// The session generation the call was made in.
        epoch: u64,
        rid: String,
        tmid: String,
        /// Part of "Move kept threads to Favorites".
        queued: bool,
        on: bool,
        result: Res<()>,
    },
    /// The star queue ("Move kept threads to Favorites", Later's "Remove
    /// done items"): time for the next change.
    StarKeptNext,
    /// `chat.starMessage`/`chat.unStarMessage` of a message ended (Later
    /// reverts it on failure).
    MessageStarred {
        sid: ServerId,
        epoch: u64,
        m: Box<rocketchat::models::Message>,
        on: bool,
        /// Queued (failures are reported together).
        job: Option<crate::app::StarJob>,
        result: Res<()>,
    },
    /// Current handler, whether it is Rustket, and the executable check
    /// (warnings to confirm, or why registration is refused).
    HandlerQueried(Option<String>, bool, Result<Vec<String>, String>),
    HandlerRegistered(Result<String, String>),
    Modal(ModalMessage),
    CloseModal,
    CloseToast(u64),
    /// The action button of toast `id`: closes it, then runs the message.
    ToastAction(u64, Box<Message>),
    /// `SwitchableStore::apply_policy` finished: the memory-only preference
    /// it applied, the backend afterwards, what failed.
    SecretsPolicyApplied(bool, SecretBackend, Option<String>),
    /// Servers whose stored E2E password has not been confirmed as saved
    /// (for the memory-only confirmation).
    SecretsPendingFound(Vec<ServerId>),
    /// Secrets of signed-out accounts were deleted from the store: the keys
    /// that are gone for good, what failed.
    SecretsPurged(Vec<String>, Option<String>),
    /// The file manager could not open the themes folder (its path).
    ThemesFolderNotOpened(String),
    /// The file manager could not open the locales folder (its path).
    LocalesFolderNotOpened(String),
    /// Offline cache (`crate::cache`).
    Cache(crate::cache::Msg),
    /// In-app updates (`crate::updater`).
    #[cfg(feature = "self-update")]
    Updater(crate::updater::app::Msg),
    /// End-to-end encryption of a server: server, session generation, message.
    E2e(ServerId, u64, crate::e2e::Message),
    /// A file was dropped onto the window (routed to the active composer).
    FileDropped(std::path::PathBuf),
    /// A navigation checked against unsaved admin settings first.
    Go(Go),
    /// A task result of the create-room or quick-switcher dialog instance
    /// (`Message::CreateRoom` / `Message::QuickSwitcher` inside).
    Dialog(u64, Box<Message>),
    Noop,
}
