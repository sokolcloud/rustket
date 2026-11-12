//! Top-level application message. Components have their own message types,
//! wrapped here together with the server (and room) they belong to.

use std::sync::Arc;

use rocketchat::Credentials;
use rocketchat::models::User;
use rocketchat::rest::LoginResult;
use rustket_core::ServerId;
use rustket_core::secrets::{SecretBackend, SecretStore};
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
    pub store: Arc<dyn SecretStore>,
    pub backend: SecretBackend,
    /// `(server, token)` for every server with a stored user id.
    pub tokens: Vec<(ServerId, Option<String>)>,
    /// Localised notices from opening the store (fallbacks, migrations).
    pub notices: Vec<String>,
}

impl std::fmt::Debug for SecretsReady {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SecretsReady")
            .field("backend", &self.backend)
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
    /// Ctrl+K.
    OpenQuickSwitcher,
    /// Tab / Shift+Tab that no widget handled (modal dialogs cycle their own
    /// focus): move focus to the next (`true`) or previous input.
    FocusNext(bool),
    /// F6 / Shift+F6: focus the next (`true`) or previous region
    /// (`crate::focus`).
    FocusRegion(bool),
    /// Escape that no widget handled: close the right panel / settings page.
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
    /// Current handler, whether it is Rustket, and the executable check
    /// (warnings to confirm, or why registration is refused).
    HandlerQueried(Option<String>, bool, Result<Vec<String>, String>),
    HandlerRegistered(Result<String, String>),
    Modal(ModalMessage),
    CloseModal,
    CloseToast(u64),
    /// Offline cache (`crate::cache`).
    Cache(crate::cache::Msg),
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
