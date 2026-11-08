//! Contract of app-level components (screens, dialogs, menus): contexts they
//! get, the effects they return. Room components use `room::{UpdateCtx,
//! ViewCtx, Action, Effect}` instead. See `crates/rustket/COMPONENTS.md`.

use std::time::Instant;

use iced::Task;
use rocketchat::{RestClient, ServerUrl};
use rustket_core::config::AppConfig;
use rustket_core::{ServerId, Workspace};
use rustket_ui::widget::BannerKind;

use crate::error::{Failure, Res};
use crate::images::Images;
use crate::session::SessionHandle;

/// Read-only context of a per-server component's `view`.
pub struct ViewCtx<'a> {
    pub server_id: &'a ServerId,
    pub server: &'a ServerUrl,
    pub workspace: &'a Workspace,
    pub images: &'a Images,
    pub config: &'a AppConfig,
    pub connected: bool,
    pub now: Instant,
}

/// Context of a per-server component's `update`.
pub struct Ctx<'a> {
    pub server_id: &'a ServerId,
    pub server: &'a ServerUrl,
    pub workspace: &'a mut Workspace,
    /// `None` while the session is (re)starting.
    pub session: Option<&'a SessionHandle>,
    pub images: &'a Images,
    pub config: &'a AppConfig,
}

impl Ctx<'_> {
    pub fn rest(&self) -> Option<RestClient> {
        self.session.map(|s| s.rest().clone())
    }

    /// Runs an authenticated REST call as a task (an `Unauthorized` failure
    /// without a session).
    pub fn rest_task<T, M, F, Fut>(
        &self,
        call: F,
        map: impl FnOnce(Res<T>) -> M + Send + 'static,
    ) -> Task<M>
    where
        T: Send + 'static,
        M: Send + 'static,
        F: FnOnce(RestClient) -> Fut + Send + 'static,
        Fut: Future<Output = rocketchat::Result<T>> + Send + 'static,
    {
        let rest = self.rest();
        Task::perform(
            async move {
                match rest {
                    Some(r) => call(r).await.map_err(Failure::from),
                    None => Err(Failure::from(no_session())),
                }
            },
            map,
        )
    }

    /// Runs a call on the DDP session.
    pub fn session_task<T, M, F, Fut>(
        &self,
        call: F,
        map: impl FnOnce(Res<T>) -> M + Send + 'static,
    ) -> Task<M>
    where
        T: Send + 'static,
        M: Send + 'static,
        F: FnOnce(SessionHandle) -> Fut + Send + 'static,
        Fut: Future<Output = rocketchat::Result<T>> + Send + 'static,
    {
        let session = self.session.cloned();
        Task::perform(
            async move {
                match session {
                    Some(s) => call(s).await.map_err(Failure::from),
                    None => Err(Failure::from(no_session())),
                }
            },
            map,
        )
    }
}

fn no_session() -> rocketchat::Error {
    rocketchat::Error::Unauthorized {
        message: Some("not connected".into()),
        code: None,
    }
}

/// App-level navigation targets.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Nav {
    /// App preferences (`screens/settings.rs`).
    Settings,
    /// Account of the active server (`screens/account.rs`).
    Account,
    /// Directory of the active server (`screens/directory.rs`).
    Directory,
    /// Create a room on the active server (`dialogs/create_room.rs`).
    CreateRoom(crate::dialogs::create_room::Kind),
    /// A discussion of room `rid` of the active server (optionally started
    /// from message `pmid`): the create dialog with the parent preset.
    CreateDiscussion { rid: String, pmid: Option<String> },
    /// Ctrl+K (`dialogs/quick_switcher.rs`).
    QuickSwitcher,
    /// Back to the workspace of the active server.
    Workspace,
}

/// Requests an app-level component makes to the app.
#[derive(Debug, Clone)]
pub enum Effect {
    /// Transient notification (title, body; already translated).
    Toast(BannerKind, String, String),
    Navigate(Nav),
    /// Open a room of the component's server.
    OpenRoom(String),
    /// Open a URL in the browser (absolute http/https/mailto only).
    OpenUrl(String),
    /// Close this screen/dialog.
    Close,
    /// The component changed `AppConfig`/`UiState`: persist them.
    SaveConfig,
    /// Log out of the component's server (`everywhere`: also revoke a PAT).
    Logout {
        everywhere: bool,
    },
    /// Remove the component's server (asks for confirmation).
    RemoveServer,
    /// Open the "register rocketchat:// handler" dialog.
    RegisterUrlHandler,
    /// Drop the image cache (memory and disk) of every server.
    ClearImageCache,
    /// Show a test desktop notification.
    TestNotification,
    /// The recently used custom status texts changed (most recent first):
    /// store them in `AppConfig::recent_status_texts` and save.
    RecentStatusTexts(Vec<String>),
    /// A UIKit interaction (app action button of the user menu).
    UiKit(Box<crate::uikit::UiKitMsg>),
}

/// Result of an app-level component `update`.
pub struct Action<M> {
    pub task: Task<M>,
    pub effects: Vec<Effect>,
}

impl<M> Default for Action<M> {
    fn default() -> Self {
        Self::none()
    }
}

impl<M> Action<M> {
    pub fn none() -> Self {
        Self {
            task: Task::none(),
            effects: Vec::new(),
        }
    }

    pub fn task(task: Task<M>) -> Self {
        Self {
            task,
            effects: Vec::new(),
        }
    }

    pub fn effect(effect: Effect) -> Self {
        Self {
            task: Task::none(),
            effects: vec![effect],
        }
    }

    pub fn with(mut self, effect: Effect) -> Self {
        self.effects.push(effect);
        self
    }

    pub fn map<N: Send + 'static>(self, f: impl Fn(M) -> N + Send + 'static) -> Action<N>
    where
        M: Send + 'static,
    {
        Action {
            task: self.task.map(f),
            effects: self.effects,
        }
    }
}
