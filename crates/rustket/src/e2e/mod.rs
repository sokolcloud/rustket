//! End-to-end encryption in the app (`docs/e2e.md` §7–§11, §14.4).
//!
//! One `rustket_core::e2e::E2eManager` per logged-in server, kept in the
//! server's `Workspace` (`Workspace::e2e`) so every component can reach it.
//! [`ServerE2e`] (in `ServerState`) owns its lifecycle: it is created once the
//! initial load is in, fed with every changed room and subscription, setting
//! changes and room key requests through a worker that runs inside the
//! server's E2E subscription, and stopped on logout. Its events trigger the
//! in-place decryption of shown messages ([`shown`]) and the account banner
//! and modals ([`view`]). Room-level UI lives in [`room`] (header key icon,
//! room menu, body states), sending in [`outgoing`], files in [`files`].
//!
//! Strings: `locales/e2e.yml` (`e2e.*`).

pub mod files;
pub mod markdown;
pub mod notify;
pub mod outgoing;
pub mod room;
pub mod shown;
pub mod view;

use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use iced::futures::SinkExt;
use iced::{Subscription, Task};
use rocketchat::SessionEvent;
use rocketchat::e2e::{RoomE2eKeyRequest, Zeroizing};
use rocketchat::models::{Message as RcMessage, Room, RoomType, Subscription as RcSub};
use rocketchat::session::ChangeKind;
use rust_i18n::t;
use rustket_core::ServerId;
use rustket_core::e2e::{AccountE2eState, E2eConfig, E2eEvent, E2eManager, E2eSettings};
use rustket_core::secrets::SecretStore;
use rustket_ui::widget::BannerKind;
use tokio::sync::mpsc;

use crate::error::Res;
use crate::workspace::ServerState;

/// Work for the manager, processed in order by the worker.
#[derive(Debug)]
enum Input {
    Load(Vec<Room>, Vec<RcSub>),
    Start,
    Room(Box<Room>),
    Sub(Box<RcSub>),
    Forget(String),
    Event(Box<SessionEvent>),
    Settings(E2eSettings),
}

/// The modal of the account flow.
pub enum Modal {
    /// "Enter your E2E password".
    EnterPassword {
        input: Zeroizing<String>,
        busy: bool,
        error: Option<String>,
    },
    /// "Save your new E2EE password": the generated passphrase.
    SavePassword {
        passphrase: Zeroizing<String>,
        busy: bool,
    },
}

impl std::fmt::Debug for Modal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Modal::EnterPassword { busy, .. } => f
                .debug_struct("EnterPassword")
                .field("busy", busy)
                .finish_non_exhaustive(),
            Modal::SavePassword { busy, .. } => f
                .debug_struct("SavePassword")
                .field("busy", busy)
                .finish_non_exhaustive(),
        }
    }
}

struct Worker {
    mgr: E2eManager,
    tx: mpsc::UnboundedSender<Input>,
    rx: Arc<Mutex<Option<mpsc::UnboundedReceiver<Input>>>>,
}

static INSTANCES: AtomicU64 = AtomicU64::new(1);

/// E2E state of one server.
#[derive(Default)]
pub struct ServerE2e {
    instance: u64,
    worker: Option<Worker>,
    /// The settings last given to the manager.
    settings: Option<E2eSettings>,
    pub modal: Option<Modal>,
    /// "Do it later" hid the password banner for this session.
    pub banner_hidden: bool,
    /// Quoted messages fetched from the server (decrypted when possible).
    quotes: HashMap<String, RcMessage>,
    requested_quotes: HashSet<String>,
    /// [`shown`] epoch of the last pass.
    last_epoch: u64,
}

impl std::fmt::Debug for ServerE2e {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ServerE2e")
            .field("running", &self.worker.is_some())
            .field("modal", &self.modal)
            .finish_non_exhaustive()
    }
}

impl ServerE2e {
    /// The running manager.
    pub fn manager(&self) -> Option<&E2eManager> {
        self.worker.as_ref().map(|w| &w.mgr)
    }

    /// Stops the manager and forgets everything (logout, session reset). The
    /// secrets stay; see [`forget_secrets`].
    pub fn reset(&mut self) {
        if let Some(w) = self.worker.take() {
            w.mgr.stop();
        }
        *self = Self::default();
    }
}

/// Messages of the E2E flow (tagged with server and session generation by
/// the app).
#[derive(Debug, Clone)]
pub enum Message {
    Event(E2eEvent),
    /// `start()` finished (error text on failure).
    Started(Option<String>),
    /// Open the modal for the current account state.
    OpenModal,
    CloseModal,
    /// "Do it later" (closes the modal and hides the password banner).
    Later,
    PasswordInput(String),
    SubmitPassword,
    PasswordResult(Result<(), PasswordError>),
    CopyPassphrase,
    ConfirmSaved,
    Saved(Result<(), String>),
    /// "Forgot your E2E password?": Account → Security.
    ForgotPassword,
    /// Retry loading the keys after an error.
    Retry,
    QuoteFetched(String, Res<Box<RcMessage>>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PasswordError {
    Wrong,
    Other(String),
}

/// Requests to the app.
#[derive(Debug, Clone)]
pub enum Request {
    Toast(BannerKind, String, String),
    /// Account → Security of this server.
    OpenAccountSecurity,
}

/// What the room components ask the app for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RoomRequest {
    /// The account modal (enter or save the password).
    OpenModal,
}

fn settings_of(s: &ServerState) -> E2eSettings {
    E2eSettings::from_settings(s.workspace.settings())
}

/// Starts the manager when the server is ready, keeps it informed about
/// changed rooms and settings, and decrypts what is shown. Call after every
/// update, before the workspace changes are taken.
pub fn after_update(
    s: &mut ServerState,
    secrets: Option<&Arc<dyn SecretStore>>,
) -> Task<crate::message::Message> {
    let ready = s.session.is_some() && s.credentials.is_some() && s.workspace.last_sync().is_some();
    if ready && s.e2e.worker.is_none() {
        start(s, secrets);
    } else if let Some(w) = &s.e2e.worker {
        let now = settings_of(s);
        if s.e2e.settings.as_ref() != Some(&now) {
            let was_enabled = s.e2e.settings.as_ref().is_some_and(|x| x.enabled);
            let _ = w.tx.send(Input::Settings(now.clone()));
            if now.enabled && !was_enabled {
                // E2E turned on at runtime (§11.12): load or create the keys.
                let _ = w.tx.send(Input::Start);
            }
            s.e2e.settings = Some(now);
            shown::keys_changed();
        }
        let changed: Vec<String> = s
            .workspace
            .pending_changes()
            .rooms
            .iter()
            .cloned()
            .collect();
        for rid in changed {
            match s.workspace.room(&rid) {
                Some(e) => {
                    if let Some(r) = &e.room {
                        let _ = w.tx.send(Input::Room(Box::new(r.clone())));
                    }
                    if let Some(sub) = &e.sub {
                        let _ = w.tx.send(Input::Sub(Box::new(sub.clone())));
                    }
                }
                None => {
                    let _ = w.tx.send(Input::Forget(rid));
                }
            }
        }
    }
    decrypt_pass(s)
}

/// Decrypts shown messages when something changed; fetches quoted messages
/// that are not loaded.
fn decrypt_pass(s: &mut ServerState) -> Task<crate::message::Message> {
    let epoch = shown::current_epoch();
    if s.workspace.pending_changes().is_empty() && epoch == s.e2e.last_epoch {
        return Task::none();
    }
    s.e2e.last_epoch = epoch;
    let (_, wants) = shown::process_workspace(&mut s.workspace, &s.url, &s.e2e.quotes);
    let Some(rest) = s.session.as_ref().map(|h| h.rest().clone()) else {
        return Task::none();
    };
    let mut tasks = Vec::new();
    for mid in wants {
        if !s.e2e.requested_quotes.insert(mid.clone()) {
            continue;
        }
        let (sid, generation, rest) = (s.id.clone(), s.generation, rest.clone());
        tasks.push(Task::perform(
            async move {
                rest.get_message(&mid)
                    .await
                    .map(Box::new)
                    .map_err(crate::error::Failure::from)
                    .map(|m| (mid.clone(), m))
                    .map_err(|e| (mid, e))
            },
            move |r| {
                let m = match r {
                    Ok((mid, msg)) => Message::QuoteFetched(mid, Ok(msg)),
                    Err((mid, e)) => Message::QuoteFetched(mid, Err(e)),
                };
                crate::message::Message::E2e(sid.clone(), generation, m)
            },
        ));
    }
    Task::batch(tasks)
}

fn start(s: &mut ServerState, secrets: Option<&Arc<dyn SecretStore>>) {
    let (Some(store), Some(creds), Some(handle)) = (secrets, &s.credentials, &s.session) else {
        return;
    };
    let settings = settings_of(s);
    let mgr = E2eManager::new(
        handle.rest().clone(),
        store.clone(),
        &s.id,
        &creds.user_id,
        settings.clone(),
        E2eConfig {
            server_url: Some(s.url.clone()),
            ..Default::default()
        },
    );
    s.workspace.set_e2e(Some(mgr.clone()));
    let (tx, rx) = mpsc::unbounded_channel();
    let rooms: Vec<Room> = s
        .workspace
        .rooms()
        .values()
        .filter_map(|e| e.room.clone())
        .collect();
    let subs: Vec<RcSub> = s
        .workspace
        .rooms()
        .values()
        .filter_map(|e| e.sub.clone())
        .collect();
    let _ = tx.send(Input::Load(rooms, subs));
    let _ = tx.send(Input::Start);
    s.e2e.instance = INSTANCES.fetch_add(1, Ordering::Relaxed);
    s.e2e.settings = Some(settings);
    s.e2e.worker = Some(Worker {
        mgr,
        tx,
        rx: Arc::new(Mutex::new(Some(rx))),
    });
    shown::keys_changed();
}

/// A realtime event the manager needs that the workspace does not keep:
/// `{rid}/e2e.keyRequest` (key distribution for that room now).
pub fn on_event(s: &ServerState, ev: &SessionEvent) {
    if let Some(w) = &s.e2e.worker
        && RoomE2eKeyRequest::from_event(ev).is_some()
    {
        let _ = w.tx.send(Input::Event(Box::new(ev.clone())));
    }
}

/// Explicit logout, `force_logout` or a key reset: stops the manager and
/// deletes this account's E2E secrets (the private key and a pending
/// generated passphrase). Call before the server state is reset.
pub fn forget_secrets(s: &mut ServerState) -> Task<crate::message::Message> {
    let Some(mgr) = s.e2e.manager().cloned() else {
        return Task::none();
    };
    s.e2e.reset();
    Task::perform(async move { mgr.logout().await }, |_| {
        crate::message::Message::Noop
    })
}

struct Runner {
    key: (ServerId, u64, u64),
    mgr: E2eManager,
    rx: Arc<Mutex<Option<mpsc::UnboundedReceiver<Input>>>>,
}

impl Hash for Runner {
    fn hash<H: Hasher>(&self, state: &mut H) {
        "rustket-e2e".hash(state);
        self.key.hash(state);
    }
}

type Tagged = (ServerId, u64, Message);

/// The E2E subscription of a server: runs the worker and forwards the
/// manager's events.
pub fn subscription(s: &ServerState) -> Subscription<crate::message::Message> {
    let Some(w) = &s.e2e.worker else {
        return Subscription::none();
    };
    let runner = Runner {
        key: (s.id.clone(), s.generation, s.e2e.instance),
        mgr: w.mgr.clone(),
        rx: w.rx.clone(),
    };
    Subscription::run_with(runner, run).map(|(sid, g, m)| crate::message::Message::E2e(sid, g, m))
}

fn run(r: &Runner) -> impl iced::futures::Stream<Item = Tagged> + use<> {
    let mgr = r.mgr.clone();
    let rx =
        r.rx.lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
    let (sid, generation) = (r.key.0.clone(), r.key.1);
    iced::stream::channel(64, async move |out| {
        let Some(mut rx) = rx else {
            return;
        };
        let mut events = mgr.subscribe();
        let mut out_events = out.clone();
        let (sid2, mgr2) = (sid.clone(), mgr.clone());
        let forward = async move {
            loop {
                match events.recv().await {
                    Ok(e) => {
                        if out_events
                            .send((sid2.clone(), generation, Message::Event(e)))
                            .await
                            .is_err()
                        {
                            return;
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                        let e = E2eEvent::AccountState(mgr2.account_state());
                        let _ = out_events
                            .send((sid2.clone(), generation, Message::Event(e)))
                            .await;
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => return,
                }
            }
        };
        let mut out_work = out;
        let work = async move {
            while let Some(input) = rx.recv().await {
                match input {
                    Input::Load(rooms, subs) => mgr.load(&rooms, &subs).await,
                    Input::Start => {
                        let r = mgr.start().await;
                        let err = r.err().map(|e| e.to_string());
                        let _ = out_work
                            .send((sid.clone(), generation, Message::Started(err)))
                            .await;
                    }
                    Input::Room(r) => mgr.update_room(&r).await,
                    Input::Sub(s) => mgr.update_subscription(&s).await,
                    Input::Forget(rid) => {
                        let room = Room {
                            id: rid,
                            t: RoomType::Private,
                            ..Default::default()
                        };
                        mgr.handle_event(&SessionEvent::RoomChanged {
                            kind: ChangeKind::Removed,
                            room: Box::new(room),
                        })
                        .await;
                    }
                    Input::Event(ev) => {
                        mgr.handle_event(&ev).await;
                    }
                    Input::Settings(s) => mgr.set_settings(s),
                }
            }
        };
        iced::futures::future::join(forward, work).await;
    })
}

/// Handles an E2E message of server `s`.
pub fn update(s: &mut ServerState, msg: Message) -> (Task<Message>, Vec<Request>) {
    let none = (Task::none(), Vec::new());
    let Some(mgr) = s.e2e.manager().cloned() else {
        return none;
    };
    match msg {
        Message::Event(e) => {
            match &e {
                E2eEvent::AccountState(state) => {
                    // The modal follows the account state.
                    match (state, &s.e2e.modal) {
                        (AccountE2eState::EnterPassword { .. }, _) => {}
                        (_, Some(Modal::EnterPassword { .. })) if state.is_ready() => {
                            s.e2e.modal = None;
                        }
                        _ => {}
                    }
                }
                E2eEvent::RoomState { .. } | E2eEvent::KeysChanged { .. } => {}
            }
            shown::keys_changed();
            none
        }
        Message::Started(err) => {
            shown::keys_changed();
            match err {
                Some(e) if !e.is_empty() => {
                    tracing::warn!(server = %s.id, "end-to-end encryption failed to start");
                    let _ = e;
                    none
                }
                _ => none,
            }
        }
        Message::OpenModal => {
            s.e2e.modal = match mgr.account_state() {
                AccountE2eState::EnterPassword { .. } => Some(Modal::EnterPassword {
                    input: Zeroizing::new(String::new()),
                    busy: false,
                    error: None,
                }),
                AccountE2eState::SavePassword => {
                    mgr.pending_passphrase()
                        .map(|passphrase| Modal::SavePassword {
                            passphrase,
                            busy: false,
                        })
                }
                _ => None,
            };
            let focus = if matches!(s.e2e.modal, Some(Modal::EnterPassword { .. })) {
                iced::widget::operation::focus(view::PASSWORD_INPUT_ID)
            } else {
                Task::none()
            };
            (focus, Vec::new())
        }
        Message::CloseModal => {
            if !matches!(
                s.e2e.modal,
                Some(
                    Modal::EnterPassword { busy: true, .. }
                        | Modal::SavePassword { busy: true, .. }
                )
            ) {
                s.e2e.modal = None;
            }
            none
        }
        Message::Later => {
            let was_password = matches!(s.e2e.modal, Some(Modal::EnterPassword { .. }))
                || matches!(mgr.account_state(), AccountE2eState::EnterPassword { .. });
            s.e2e.modal = None;
            if was_password {
                s.e2e.banner_hidden = true;
                return (
                    Task::none(),
                    vec![Request::Toast(
                        BannerKind::Info,
                        t!("e2e.not_enabled_title").into_owned(),
                        t!("e2e.not_enabled_body").into_owned(),
                    )],
                );
            }
            none
        }
        Message::PasswordInput(v) => {
            if let Some(Modal::EnterPassword { input, error, .. }) = &mut s.e2e.modal {
                *input = Zeroizing::new(v);
                *error = None;
            }
            none
        }
        Message::SubmitPassword => {
            let Some(Modal::EnterPassword { input, busy, error }) = &mut s.e2e.modal else {
                return none;
            };
            if *busy || input.is_empty() {
                return none;
            }
            *busy = true;
            *error = None;
            let pw = input.clone();
            let task = Task::perform(async move { mgr.submit_password(&pw).await }, |r| {
                Message::PasswordResult(r.map_err(|e| match e {
                    rustket_core::e2e::E2eManagerError::WrongPassword => PasswordError::Wrong,
                    rustket_core::e2e::E2eManagerError::Crypto(
                        rocketchat::e2e::E2eError::WrongPassword,
                    ) => PasswordError::Wrong,
                    other => PasswordError::Other(other.to_string()),
                }))
            });
            (task, Vec::new())
        }
        Message::PasswordResult(r) => match r {
            Ok(()) => {
                s.e2e.modal = None;
                shown::keys_changed();
                (
                    Task::none(),
                    vec![Request::Toast(
                        BannerKind::Success,
                        t!("e2e.enabled_toast").into_owned(),
                        String::new(),
                    )],
                )
            }
            Err(PasswordError::Wrong) => {
                if let Some(Modal::EnterPassword { busy, error, input }) = &mut s.e2e.modal {
                    *busy = false;
                    *error = Some(t!("e2e.wrong_password").into_owned());
                    *input = Zeroizing::new(String::new());
                }
                (
                    iced::widget::operation::focus(view::PASSWORD_INPUT_ID),
                    Vec::new(),
                )
            }
            Err(PasswordError::Other(e)) => {
                s.e2e.modal = None;
                (
                    Task::none(),
                    vec![Request::Toast(
                        BannerKind::Error,
                        t!("e2e.unlock_failed").into_owned(),
                        e,
                    )],
                )
            }
        },
        Message::CopyPassphrase => {
            let Some(Modal::SavePassword { passphrase, .. }) = &s.e2e.modal else {
                return none;
            };
            (
                iced::clipboard::write(passphrase.to_string()),
                vec![Request::Toast(
                    BannerKind::Success,
                    t!("e2e.copied").into_owned(),
                    String::new(),
                )],
            )
        }
        Message::ConfirmSaved => {
            let Some(Modal::SavePassword { busy, .. }) = &mut s.e2e.modal else {
                return none;
            };
            *busy = true;
            let task = Task::perform(async move { mgr.confirm_passphrase_saved().await }, |r| {
                Message::Saved(r.map_err(|e| e.to_string()))
            });
            (task, Vec::new())
        }
        Message::Saved(r) => {
            s.e2e.modal = None;
            match r {
                Ok(()) => (
                    Task::none(),
                    vec![Request::Toast(
                        BannerKind::Success,
                        t!("e2e.enabled_toast").into_owned(),
                        String::new(),
                    )],
                ),
                Err(e) => (
                    Task::none(),
                    vec![Request::Toast(
                        BannerKind::Error,
                        t!("e2e.save_failed").into_owned(),
                        e,
                    )],
                ),
            }
        }
        Message::ForgotPassword => {
            s.e2e.modal = None;
            (Task::none(), vec![Request::OpenAccountSecurity])
        }
        Message::Retry => {
            if let Some(w) = &s.e2e.worker {
                let _ = w.tx.send(Input::Start);
            }
            none
        }
        Message::QuoteFetched(mid, r) => {
            match r {
                Ok(m) => {
                    s.e2e.quotes.insert(mid, *m);
                    shown::keys_changed();
                }
                Err(e) => tracing::debug!(error = %e, "quoted message not available"),
            }
            none
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use rustket_core::config::ServerEntry;

    fn server() -> ServerState {
        let entry = ServerEntry {
            id: ServerId("s-e2e".into()),
            url: "http://localhost:3000".into(),
            ..Default::default()
        };
        ServerState::from_entry(&entry).unwrap()
    }

    #[test]
    fn nothing_runs_while_logged_out() {
        let mut s = server();
        let store: Arc<dyn SecretStore> = Arc::new(rustket_core::secrets::MemoryStore::new());
        let _ = after_update(&mut s, Some(&store));
        assert!(s.e2e.manager().is_none());
        assert!(s.workspace.e2e().is_none());
        assert!(view::banner(&s).is_none());
        assert!(view::modal(&s).is_none());
        let (_, req) = update(&mut s, Message::OpenModal);
        assert!(req.is_empty());
        assert!(s.e2e.modal.is_none());
        let _ = forget_secrets(&mut s);
    }
}
