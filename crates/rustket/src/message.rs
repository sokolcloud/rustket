//! Top-level application message. Components have their own message types,
//! wrapped here together with the server (and room) they belong to.

use std::sync::Arc;

use rocketchat::Credentials;
use rocketchat::models::User;
use rustket_core::ServerId;
use rustket_core::secrets::{SecretBackend, SecretStore};
use rustket_core::sync::{DeltaData, InitialData, RoomSync};

use crate::error::Res;
use crate::images::FetchResult;
use crate::room;
use crate::screens::{add_server, login};
use crate::session::ServerEvent;
use crate::workspace::{rail, sidebar};

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
    Sidebar(ServerId, sidebar::Message),
    Settings(crate::screens::settings::Message),
    Account(ServerId, crate::screens::account::Message),
    Directory(ServerId, crate::screens::directory::Message),
    CreateRoom(ServerId, crate::dialogs::create_room::Message),
    QuickSwitcher(ServerId, crate::dialogs::quick_switcher::Message),
    StatusMenu(ServerId, crate::workspace::status_menu::Message),
    Desktop(crate::desktop::Message),
    /// Ctrl+K.
    OpenQuickSwitcher,
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
    /// Image fetch result: server, its cache generation at request time, URL.
    ImageLoaded(ServerId, u64, String, FetchResult),
    DeepLinks(Vec<String>),
    TokenUser(ServerId, Credentials, Res<User>),
    TokenStored(ServerId, Result<(), String>),
    RetryConnection(ServerId),
    LoggedOut(ServerId, Result<(), String>),
    /// `subscriptions.unread` finished (the room was left before).
    MarkedUnread(ServerId, Res<()>),
    /// Current handler, whether it is Rustket, and the executable check
    /// (warnings to confirm, or why registration is refused).
    HandlerQueried(Option<String>, bool, Result<Vec<String>, String>),
    HandlerRegistered(Result<String, String>),
    Modal(ModalMessage),
    CloseModal,
    CloseToast(u64),
    /// A file was dropped onto the window (routed to the active composer).
    FileDropped(std::path::PathBuf),
    Noop,
}
