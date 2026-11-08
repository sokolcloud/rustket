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
    /// "Save your new E2E password": the generated passphrase.
    SavePassword {
        passphrase: Zeroizing<String>,
        busy: bool,
    },
    /// Signing out would delete the generated passphrase the user has not
    /// saved yet: show it once more before `action` runs.
    SignOutWarning {
        passphrase: Zeroizing<String>,
        action: SignOut,
    },
}

/// A sign-out the user asked for that deletes the E2E secrets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SignOut {
    Logout { everywhere: bool },
    RemoveServer,
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
            Modal::SignOutWarning { action, .. } => f
                .debug_struct("SignOutWarning")
                .field("action", action)
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
    /// Quote fetches that failed, retried after [`QUOTE_RETRY`].
    failed_quotes: HashMap<String, std::time::Instant>,
    quotes_in_flight: usize,
    /// [`shown`] epoch of the last pass.
    last_epoch: u64,
    /// Failed starts in a row (backoff of the automatic retry).
    start_failures: u32,
    /// An automatic retry of the start is scheduled.
    retry_scheduled: bool,
    /// `Workspace::last_sync` seen by the last update (a resync retries a
    /// failed start).
    seen_sync: Option<rocketchat::RcDate>,
    /// Failed fetches of the E2E settings in a row; whether one runs.
    settings_failures: u32,
    settings_fetching: bool,
    /// The last start failed on the secret store (locked keyring, dismissed
    /// unlock prompt): only the manual Retry tries again, since every
    /// attempt may show the system unlock prompt.
    manual_retry_only: bool,
    /// A secret store read for [`check_sign_out`] runs (since then).
    sign_out_checking: Option<std::time::Instant>,
    /// That read failed: the next sign-out keeps a pending passphrase.
    keep_pending: bool,
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
    /// `start()` finished (the failure, if any).
    Started(Option<StartFailure>),
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
    /// The backoff of an automatic retry ran out (manager instance).
    AutoRetry(u64),
    QuoteFetched(String, Res<Box<RcMessage>>),
    /// The E2E settings were fetched again (they were missing).
    SettingsFetched(Res<Vec<rocketchat::models::Setting>>),
    /// "Sign out anyway" in [`Modal::SignOutWarning`].
    ConfirmSignOut,
    /// The secret store was asked for a pending passphrase before `SignOut`
    /// (see [`check_sign_out`]).
    SignOutChecked(SignOut, Result<Option<Passphrase>, String>),
}

/// A failed start of the manager.
#[derive(Debug, Clone)]
pub struct StartFailure {
    pub text: String,
    /// Retried automatically (a secret store error is not: another attempt
    /// may show the system unlock prompt again).
    pub auto_retry: bool,
}

/// A passphrase in a message (never printed by `Debug`).
#[derive(Clone)]
pub struct Passphrase(Zeroizing<String>);

impl std::fmt::Debug for Passphrase {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Passphrase(..)")
    }
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
    /// The user saw the unsaved passphrase and still wants to sign out (or
    /// there is none).
    SignOut(SignOut),
    /// [`Modal::SignOutWarning`] opened: show the server screen.
    ShowModal,
}

/// What the room components ask the app for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RoomRequest {
    /// The account modal (enter or save the password).
    OpenModal,
}

/// A localized text for a manager error (the raw text is logged at debug).
pub fn error_text(e: &rustket_core::e2e::E2eManagerError) -> String {
    use rustket_core::e2e::E2eManagerError as E;
    tracing::debug!(error = %e, "end-to-end encryption error");
    match e {
        E::Disabled => t!("e2e.err_disabled"),
        E::NotReady => t!("e2e.err_not_ready"),
        E::RoomNotReady => t!("e2e.err_room_not_ready"),
        E::RoomNotEncrypted => t!("e2e.err_room_not_encrypted"),
        E::Cancelled => t!("e2e.err_cancelled"),
        E::WrongPassword | E::Crypto(rocketchat::e2e::E2eError::WrongPassword) => {
            t!("e2e.wrong_password")
        }
        E::NoRoomKeyId => t!("e2e.reset_no_key"),
        E::Crypto(_) => t!("e2e.err_crypto"),
        E::Rest(r) => return crate::error::user_text(r),
        E::Secret(_) => t!("e2e.err_secret"),
        E::Task(_) => t!("e2e.err_internal"),
    }
    .into_owned()
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
    let mut purge = Task::none();
    if ready && s.e2e.worker.is_none() {
        start(s, secrets);
        // Decrypted media left behind by an earlier run.
        purge = Task::perform(files::purge_private_media(true), |_| {
            crate::message::Message::Noop
        });
    } else if let Some(w) = &s.e2e.worker {
        // A resync after a failed start (e.g. `e2e.fetchMyKeys` timed out
        // while the connection dropped) tries again.
        let sync = s.workspace.last_sync();
        if sync != s.e2e.seen_sync
            && !s.e2e.manual_retry_only
            && matches!(w.mgr.account_state(), AccountE2eState::Error(_))
        {
            let _ = w.tx.send(Input::Start);
        }
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
    s.e2e.seen_sync = s.workspace.last_sync();
    let settings = if ready {
        fetch_settings(s)
    } else {
        Task::none()
    };
    Task::batch([purge, settings, decrypt_pass(s)])
}

/// Delay before the next automatic attempt after `failures` failures in a
/// row: 5 s, doubling, at most 5 min (the default REST rate limit window is
/// one minute).
fn backoff(failures: u32) -> std::time::Duration {
    let secs = 5u64.saturating_mul(1 << failures.min(6));
    std::time::Duration::from_secs(secs.min(300))
}

/// While the E2E settings are unknown (`settings.public` failed during the
/// load), encrypted rooms are blocked (`outgoing::mode`); fetch just those
/// settings again, with backoff, instead of waiting for the next reconnect.
fn fetch_settings(s: &mut ServerState) -> Task<crate::message::Message> {
    if s.e2e.settings_fetching || E2eSettings::loaded(s.workspace.settings()) {
        return Task::none();
    }
    let Some(rest) = s.session.as_ref().map(|h| h.rest().clone()) else {
        return Task::none();
    };
    s.e2e.settings_fetching = true;
    let delay = backoff(s.e2e.settings_failures);
    let (sid, generation) = (s.id.clone(), s.generation);
    Task::perform(
        async move {
            tokio::time::sleep(delay).await;
            rest.settings_public_by_ids(&rustket_core::e2e::E2E_SETTING_IDS)
                .await
                .map_err(crate::error::Failure::from)
        },
        move |r| crate::message::Message::E2e(sid.clone(), generation, Message::SettingsFetched(r)),
    )
}

/// Schedules an automatic retry of a failed start.
fn schedule_retry(s: &mut ServerState) -> Task<Message> {
    if s.e2e.retry_scheduled {
        return Task::none();
    }
    s.e2e.retry_scheduled = true;
    let delay = backoff(s.e2e.start_failures);
    s.e2e.start_failures = s.e2e.start_failures.saturating_add(1);
    let instance = s.e2e.instance;
    Task::perform(tokio::time::sleep(delay), move |_| {
        Message::AutoRetry(instance)
    })
}

/// What the caller of [`check_sign_out`] does.
pub enum SignOutCheck {
    /// Nothing would be lost: sign out now.
    Now,
    /// [`Modal::SignOutWarning`] is open; [`Request::SignOut`] follows if
    /// the user confirms.
    Warned,
    /// The secret store is asked first (the manager does not hold the
    /// passphrase in every state, and none runs offline); the result comes
    /// back as [`Message::SignOutChecked`], then [`Request::SignOut`] or
    /// [`Request::ShowModal`].
    Pending(Task<crate::message::Message>),
}

/// Before `action` deletes the E2E secrets: a generated passphrase the user
/// has not saved yet is shown once more ([`Modal::SignOutWarning`]). The
/// secret store decides, whatever state the manager is in; when it cannot be
/// read, the sign-out goes ahead and keeps the passphrase.
pub fn check_sign_out(
    s: &mut ServerState,
    store: Option<&Arc<dyn SecretStore>>,
    user_id: Option<&str>,
    action: SignOut,
) -> SignOutCheck {
    if let Some(passphrase) = s.e2e.manager().and_then(E2eManager::pending_passphrase) {
        s.e2e.modal = Some(Modal::SignOutWarning { passphrase, action });
        return SignOutCheck::Warned;
    }
    let uid = s
        .credentials
        .as_ref()
        .map(|c| c.user_id.clone())
        .or_else(|| user_id.map(str::to_owned));
    let (Some(store), Some(uid)) = (store, uid) else {
        return SignOutCheck::Now;
    };
    // One read at a time (a keyring read may wait for the unlock prompt);
    // a lost answer does not block signing out for good.
    if s.e2e
        .sign_out_checking
        .is_some_and(|t| t.elapsed() < SIGN_OUT_CHECK_TIMEOUT)
    {
        return SignOutCheck::Pending(Task::none());
    }
    s.e2e.sign_out_checking = Some(std::time::Instant::now());
    let secrets = rustket_core::e2e::E2eSecrets::new(store.clone(), &s.id, &uid);
    let (sid, generation) = (s.id.clone(), s.generation);
    SignOutCheck::Pending(Task::perform(
        async move {
            secrets
                .pending_passphrase()
                .await
                .map(|p| p.map(Passphrase))
                .map_err(|e| e.to_string())
        },
        move |r| {
            crate::message::Message::E2e(
                sid.clone(),
                generation,
                Message::SignOutChecked(action, r),
            )
        },
    ))
}

/// Longer than a keyring read may take (it waits up to a minute for the
/// unlock prompt).
const SIGN_OUT_CHECK_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(90);

fn sign_out_checked(
    s: &mut ServerState,
    action: SignOut,
    r: Result<Option<Passphrase>, String>,
) -> (Task<Message>, Vec<Request>) {
    if s.e2e.sign_out_checking.take().is_none() {
        return (Task::none(), Vec::new());
    }
    match r {
        Ok(Some(Passphrase(passphrase))) => {
            s.e2e.modal = Some(Modal::SignOutWarning { passphrase, action });
            (Task::none(), vec![Request::ShowModal])
        }
        Ok(None) => (Task::none(), vec![Request::SignOut(action)]),
        Err(e) => {
            tracing::warn!(error = %e, "cannot read the E2E secrets before signing out");
            s.e2e.keep_pending = true;
            (Task::none(), vec![Request::SignOut(action)])
        }
    }
}

/// Decrypts shown messages when something changed; fetches quoted messages
/// that are not loaded.
/// Quote fetches running at once.
const MAX_QUOTE_FETCHES: usize = 6;
/// Delay before a failed quote fetch is tried again.
const QUOTE_RETRY: std::time::Duration = std::time::Duration::from_secs(60);
/// Fetched quotes kept (the cache starts over beyond that).
const MAX_QUOTE_CACHE: usize = 1024;

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
        if s.e2e.quotes_in_flight >= MAX_QUOTE_FETCHES {
            break;
        }
        if s.e2e.requested_quotes.contains(&mid)
            || s.e2e
                .failed_quotes
                .get(&mid)
                .is_some_and(|t| t.elapsed() < QUOTE_RETRY)
        {
            continue;
        }
        s.e2e.requested_quotes.insert(mid.clone());
        s.e2e.quotes_in_flight += 1;
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

/// Explicit logout or server removal (after [`warn_before_sign_out`]):
/// stops the manager and deletes this account's E2E secrets (the private key
/// and a pending generated passphrase). Call before the server state is reset.
/// Without a running manager (offline start, logout before the initial load)
/// the secrets are deleted directly, for the logged-in user or `user_id`.
pub fn forget_secrets(
    s: &mut ServerState,
    store: Option<&Arc<dyn SecretStore>>,
    user_id: Option<&str>,
) -> Task<crate::message::Message> {
    forget(s, store, user_id, false)
}

/// `force_logout` (key reset, this session logged out elsewhere): like
/// [`forget_secrets`], but a generated passphrase the user has not saved yet
/// is kept, since nobody could warn them; the next login unlocks the key
/// backup with it and shows it again.
pub fn forget_keys(
    s: &mut ServerState,
    store: Option<&Arc<dyn SecretStore>>,
    user_id: Option<&str>,
) -> Task<crate::message::Message> {
    forget(s, store, user_id, true)
}

fn forget(
    s: &mut ServerState,
    store: Option<&Arc<dyn SecretStore>>,
    user_id: Option<&str>,
    keep_pending: bool,
) -> Task<crate::message::Message> {
    let keep_pending = keep_pending || s.e2e.keep_pending;
    let purge = Task::perform(files::purge_private_media(false), |_| {
        crate::message::Message::Noop
    });
    if let Some(mgr) = s.e2e.manager().cloned() {
        s.e2e.reset();
        let logout = Task::perform(
            async move {
                if keep_pending {
                    mgr.forced_logout().await;
                } else {
                    mgr.logout().await;
                }
            },
            |_| crate::message::Message::Noop,
        );
        return Task::batch([purge, logout]);
    }
    s.e2e.reset();
    let uid = s
        .credentials
        .as_ref()
        .map(|c| c.user_id.clone())
        .or_else(|| user_id.map(str::to_owned));
    let (Some(store), Some(uid)) = (store, uid) else {
        return purge;
    };
    let secrets = rustket_core::e2e::E2eSecrets::new(store.clone(), &s.id, &uid);
    let clear = Task::perform(
        async move {
            let r = if keep_pending {
                secrets.delete_private_key().await
            } else {
                secrets.clear().await
            };
            if let Err(e) = r {
                tracing::warn!(error = %e, "could not delete the E2E secrets");
            }
        },
        |_| crate::message::Message::Noop,
    );
    Task::batch([purge, clear])
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
                        let err = r.err().map(|e| StartFailure {
                            auto_retry: !matches!(e, rustket_core::e2e::E2eManagerError::Secret(_)),
                            text: error_text(&e),
                        });
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

fn settings_fetched(s: &mut ServerState, r: Res<Vec<rocketchat::models::Setting>>) {
    s.e2e.settings_fetching = false;
    match r {
        Ok(list) => {
            for setting in list {
                s.workspace
                    .apply_setting(rustket_core::Change::Updated, setting);
            }
            s.e2e.settings_failures = if E2eSettings::loaded(s.workspace.settings()) {
                0
            } else {
                s.e2e.settings_failures.saturating_add(1)
            };
        }
        Err(e) => {
            tracing::debug!(error = %e, "E2E settings not available");
            s.e2e.settings_failures = s.e2e.settings_failures.saturating_add(1);
        }
    }
    // The next update schedules another attempt while still unknown.
    shown::keys_changed();
}

/// Handles an E2E message of server `s`.
pub fn update(s: &mut ServerState, msg: Message) -> (Task<Message>, Vec<Request>) {
    let none = (Task::none(), Vec::new());
    // The sign-out warning works without a manager (offline, logged out).
    match msg {
        Message::SettingsFetched(r) => {
            settings_fetched(s, r);
            return none;
        }
        Message::SignOutChecked(action, r) => return sign_out_checked(s, action, r),
        Message::ConfirmSignOut => {
            let Some(Modal::SignOutWarning { action, .. }) = &s.e2e.modal else {
                return none;
            };
            let action = *action;
            s.e2e.modal = None;
            return (Task::none(), vec![Request::SignOut(action)]);
        }
        Message::CloseModal | Message::CopyPassphrase => return modal_update(s, msg),
        _ => {}
    }
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
            if let Some(f) = &err
                && matches!(mgr.account_state(), AccountE2eState::Error(_))
            {
                tracing::warn!(server = %s.id, "end-to-end encryption failed to start");
                // Transient failures (fetchMyKeys timeout, 5xx, 429) are
                // retried with backoff; a secret store error waits for the
                // banner's Retry.
                s.e2e.manual_retry_only = !f.auto_retry;
                if f.auto_retry {
                    return (schedule_retry(s), Vec::new());
                }
                return none;
            }
            if err.is_none() {
                s.e2e.start_failures = 0;
                s.e2e.manual_retry_only = false;
            }
            none
        }
        Message::AutoRetry(instance) => {
            if instance == s.e2e.instance {
                s.e2e.retry_scheduled = false;
                if !s.e2e.manual_retry_only
                    && matches!(mgr.account_state(), AccountE2eState::Error(_))
                    && let Some(w) = &s.e2e.worker
                {
                    let _ = w.tx.send(Input::Start);
                }
            }
            none
        }
        // Handled above.
        Message::SettingsFetched(_)
        | Message::SignOutChecked(..)
        | Message::ConfirmSignOut
        | Message::CloseModal
        | Message::CopyPassphrase => none,
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
                    other => PasswordError::Other(error_text(&other)),
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
        Message::ConfirmSaved => {
            let Some(Modal::SavePassword { busy, .. }) = &mut s.e2e.modal else {
                return none;
            };
            *busy = true;
            let task = Task::perform(async move { mgr.confirm_passphrase_saved().await }, |r| {
                Message::Saved(r.map_err(|e| error_text(&e)))
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
            s.e2e.start_failures = 0;
            s.e2e.manual_retry_only = false;
            if let Some(w) = &s.e2e.worker {
                let _ = w.tx.send(Input::Start);
            }
            none
        }
        Message::QuoteFetched(mid, r) => {
            s.e2e.quotes_in_flight = s.e2e.quotes_in_flight.saturating_sub(1);
            s.e2e.requested_quotes.remove(&mid);
            match r {
                Ok(m) => {
                    if s.e2e.quotes.len() >= MAX_QUOTE_CACHE {
                        s.e2e.quotes.clear();
                    }
                    s.e2e.failed_quotes.remove(&mid);
                    s.e2e.quotes.insert(mid, *m);
                }
                Err(e) => {
                    tracing::debug!(error = %e, "quoted message not available");
                    if s.e2e.failed_quotes.len() >= MAX_QUOTE_CACHE {
                        s.e2e.failed_quotes.clear();
                    }
                    s.e2e.failed_quotes.insert(mid, std::time::Instant::now());
                }
            }
            // Another pass: resolved quotes are attached, waiting ones fetched.
            shown::keys_changed();
            none
        }
    }
}

/// Closing the modal and copying the passphrase (with or without a manager).
fn modal_update(s: &mut ServerState, msg: Message) -> (Task<Message>, Vec<Request>) {
    let none = (Task::none(), Vec::new());
    match msg {
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
        Message::CopyPassphrase => {
            let Some(
                Modal::SavePassword { passphrase, .. } | Modal::SignOutWarning { passphrase, .. },
            ) = &s.e2e.modal
            else {
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
        _ => none,
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
        let _ = forget_secrets(&mut s, Some(&store), None);
    }

    const INSTANCE: u64 = 77;

    /// Installs `mgr` as the running manager; returns the worker's inbox.
    fn with_manager(s: &mut ServerState, mgr: E2eManager) -> mpsc::UnboundedReceiver<Input> {
        let (tx, rx) = mpsc::unbounded_channel();
        s.workspace.set_e2e(Some(mgr.clone()));
        s.e2e.instance = INSTANCE;
        s.e2e.worker = Some(Worker {
            mgr,
            tx,
            rx: Arc::new(Mutex::new(None)),
        });
        rx
    }

    fn manager(url: &rocketchat::ServerUrl, store: Arc<dyn SecretStore>) -> E2eManager {
        let tls = crate::session::tls_for(&[]).unwrap();
        let rest = rocketchat::RestClient::new(url.clone(), &tls).unwrap();
        E2eManager::new(
            rest,
            store,
            &ServerId("s-e2e".into()),
            "me",
            E2eSettings {
                enabled: true,
                ..Default::default()
            },
            E2eConfig {
                auto_distribution: false,
                ..Default::default()
            },
        )
    }

    async fn run(task: Task<crate::message::Message>) {
        use iced::futures::StreamExt;
        if let Some(stream) = iced_runtime::task::into_stream(task) {
            stream.collect::<Vec<_>>().await;
        }
    }

    fn starts(rx: &mut mpsc::UnboundedReceiver<Input>) -> usize {
        std::iter::from_fn(|| rx.try_recv().ok())
            .filter(|i| matches!(i, Input::Start))
            .count()
    }

    fn failure(auto_retry: bool) -> StartFailure {
        StartFailure {
            text: "failed".into(),
            auto_retry,
        }
    }

    #[tokio::test]
    async fn secret_store_failures_wait_for_the_manual_retry() {
        let url = rocketchat::ServerUrl::parse("http://127.0.0.1:9").unwrap();
        let mgr = manager(&url, Arc::new(rustket_core::secrets::MemoryStore::new()));
        assert!(mgr.start().await.is_err());
        let mut s = server();
        let mut rx = with_manager(&mut s, mgr);
        // A locked keyring: no timer, no retry on resync (each attempt may
        // show the unlock prompt again).
        let _ = update(&mut s, Message::Started(Some(failure(false))));
        assert!(!s.e2e.retry_scheduled);
        s.workspace.apply_initial(rustket_core::sync::InitialData {
            next_since: Some(rocketchat::RcDate::now()),
            ..Default::default()
        });
        let _ = after_update(&mut s, None);
        assert_eq!(starts(&mut rx), 0);
        let _ = update(&mut s, Message::AutoRetry(INSTANCE));
        assert_eq!(starts(&mut rx), 0);
        // The banner's Retry does.
        let _ = update(&mut s, Message::Retry);
        assert_eq!(starts(&mut rx), 1);
        assert!(!s.e2e.manual_retry_only);
    }

    #[tokio::test]
    async fn failed_start_is_retried_with_backoff_and_on_resync() {
        // `e2e.fetchMyKeys` fails (server unreachable): Error.
        let url = rocketchat::ServerUrl::parse("http://127.0.0.1:9").unwrap();
        let mgr = manager(&url, Arc::new(rustket_core::secrets::MemoryStore::new()));
        assert!(mgr.start().await.is_err());
        assert!(matches!(mgr.account_state(), AccountE2eState::Error(_)));
        let mut s = server();
        let mut rx = with_manager(&mut s, mgr);
        let _ = update(&mut s, Message::Started(Some(failure(true))));
        assert!(s.e2e.retry_scheduled);
        assert_eq!(s.e2e.start_failures, 1);
        // One timer at a time.
        let _ = update(&mut s, Message::Started(Some(failure(true))));
        assert_eq!(s.e2e.start_failures, 1);
        assert_eq!(backoff(0), std::time::Duration::from_secs(5));
        assert_eq!(backoff(1), std::time::Duration::from_secs(10));
        assert_eq!(backoff(30), std::time::Duration::from_secs(300));
        // The timer of an old manager does nothing.
        let _ = update(&mut s, Message::AutoRetry(INSTANCE + 1));
        assert_eq!(starts(&mut rx), 0);
        let _ = update(&mut s, Message::AutoRetry(INSTANCE));
        assert_eq!(starts(&mut rx), 1);
        assert!(!s.e2e.retry_scheduled);
        // A resync (reconnect) while in Error tries again at once.
        s.workspace.apply_initial(rustket_core::sync::InitialData {
            next_since: Some(rocketchat::RcDate::now()),
            ..Default::default()
        });
        let _ = after_update(&mut s, None);
        assert_eq!(starts(&mut rx), 1);
        let _ = after_update(&mut s, None);
        assert_eq!(starts(&mut rx), 0, "only when the sync changed");
    }

    /// Runs `task`; returns the E2E messages it produced.
    async fn messages(task: Task<crate::message::Message>) -> Vec<Message> {
        use iced::futures::StreamExt;
        let Some(stream) = iced_runtime::task::into_stream(task) else {
            return Vec::new();
        };
        stream
            .filter_map(|a| async move {
                match a {
                    iced_runtime::Action::Output(crate::message::Message::E2e(_, _, m)) => Some(m),
                    _ => None,
                }
            })
            .collect()
            .await
    }

    /// [`check_sign_out`] up to the requests to the app.
    async fn sign_out(
        s: &mut ServerState,
        store: &Arc<dyn SecretStore>,
        action: SignOut,
    ) -> Vec<Request> {
        match check_sign_out(s, Some(store), Some("me"), action) {
            SignOutCheck::Now => vec![Request::SignOut(action)],
            SignOutCheck::Warned => vec![Request::ShowModal],
            SignOutCheck::Pending(task) => {
                let mut out = Vec::new();
                for m in messages(task).await {
                    out.extend(update(s, m).1);
                }
                out
            }
        }
    }

    #[tokio::test]
    async fn sign_out_warns_while_the_generated_passphrase_is_unsaved() {
        // First use: keys are created and backed up with a generated passphrase.
        let url = super::shown::tests::stub_server(r#"{"success":true}"#.into()).await;
        let mem = Arc::new(rustket_core::secrets::MemoryStore::new());
        let store: Arc<dyn SecretStore> = mem.clone();
        let mgr = manager(&url, store.clone());
        mgr.start().await.unwrap();
        assert_eq!(mgr.account_state(), AccountE2eState::SavePassword);
        let mut s = server();
        let _rx = with_manager(&mut s, mgr.clone());
        let logout = SignOut::Logout { everywhere: false };
        assert!(matches!(
            check_sign_out(&mut s, Some(&store), None, logout),
            SignOutCheck::Warned
        ));
        assert!(matches!(s.e2e.modal, Some(Modal::SignOutWarning { .. })));
        assert!(view::modal(&s).is_some());
        let (_, req) = update(&mut s, Message::CopyPassphrase);
        assert!(matches!(req.as_slice(), [Request::Toast(..)]));
        // Cancel keeps everything.
        let _ = update(&mut s, Message::CloseModal);
        assert!(s.e2e.modal.is_none());
        assert_eq!(
            sign_out(&mut s, &store, SignOut::RemoveServer).await.len(),
            1
        );
        let (_, req) = update(&mut s, Message::ConfirmSignOut);
        assert!(matches!(
            req.as_slice(),
            [Request::SignOut(SignOut::RemoveServer)]
        ));
        assert!(s.e2e.modal.is_none());
        // Saved: nothing to warn about.
        mgr.confirm_passphrase_saved().await.unwrap();
        assert!(matches!(
            sign_out(&mut s, &store, logout).await.as_slice(),
            [Request::SignOut(_)]
        ));
        assert!(s.e2e.modal.is_none());
    }

    #[tokio::test]
    async fn sign_out_asks_the_secret_store_without_a_manager() {
        use rustket_core::e2e::pending_passphrase_secret;
        let sid = ServerId("s-e2e".into());
        let mem = Arc::new(rustket_core::secrets::MemoryStore::new());
        let store: Arc<dyn SecretStore> = mem.clone();
        mem.set(&pending_passphrase_secret(&sid, "me"), "twelve words")
            .unwrap();
        // Offline start, fetchMyKeys failed, read-only start: the manager
        // does not hold the passphrase, the store does.
        let mut s = server();
        let logout = SignOut::Logout { everywhere: true };
        let req = sign_out(&mut s, &store, logout).await;
        assert!(matches!(req.as_slice(), [Request::ShowModal]));
        let Some(Modal::SignOutWarning { passphrase, action }) = &s.e2e.modal else {
            panic!("no warning");
        };
        assert_eq!(passphrase.as_str(), "twelve words");
        assert_eq!(*action, logout);
        assert!(view::modal(&s).is_some());
        let (_, req) = update(&mut s, Message::CopyPassphrase);
        assert!(matches!(req.as_slice(), [Request::Toast(..)]));
        let (_, req) = update(&mut s, Message::ConfirmSignOut);
        assert!(matches!(req.as_slice(), [Request::SignOut(a)] if *a == logout));
        // Nothing stored: sign out at once.
        mem.delete(&pending_passphrase_secret(&sid, "me")).unwrap();
        let req = sign_out(&mut s, &store, SignOut::RemoveServer).await;
        assert!(matches!(
            req.as_slice(),
            [Request::SignOut(SignOut::RemoveServer)]
        ));
        assert!(s.e2e.modal.is_none());
    }

    /// A keyring that cannot be read (locked); deletes are recorded.
    #[derive(Default)]
    struct LockedStore {
        deleted: Mutex<Vec<String>>,
    }

    impl SecretStore for LockedStore {
        fn get(&self, _: &str) -> rustket_core::secrets::Result<Option<String>> {
            Err(rustket_core::secrets::SecretError::NoStorageAccess(
                "locked".into(),
            ))
        }
        fn set(&self, _: &str, _: &str) -> rustket_core::secrets::Result<()> {
            Ok(())
        }
        fn delete(&self, key: &str) -> rustket_core::secrets::Result<()> {
            self.deleted.lock().unwrap().push(key.to_owned());
            Ok(())
        }
    }

    #[tokio::test]
    async fn unreadable_store_keeps_the_passphrase_on_sign_out() {
        use rustket_core::e2e::{pending_passphrase_secret, private_key_secret};
        let sid = ServerId("s-e2e".into());
        let locked = Arc::new(LockedStore::default());
        let store: Arc<dyn SecretStore> = locked.clone();
        let mut s = server();
        let logout = SignOut::Logout { everywhere: false };
        let req = sign_out(&mut s, &store, logout).await;
        assert!(matches!(req.as_slice(), [Request::SignOut(_)]));
        run(forget_secrets(&mut s, Some(&store), Some("me"))).await;
        let deleted = locked.deleted.lock().unwrap().clone();
        assert!(deleted.contains(&private_key_secret(&sid, "me")));
        assert!(!deleted.contains(&pending_passphrase_secret(&sid, "me")));
    }

    #[tokio::test]
    async fn forced_logout_keeps_the_unsaved_passphrase() {
        use rustket_core::e2e::{pending_passphrase_secret, private_key_secret};
        let sid = ServerId("s-e2e".into());
        let fill = |store: &rustket_core::secrets::MemoryStore| {
            store.set(&private_key_secret(&sid, "me"), "jwk").unwrap();
            store
                .set(&pending_passphrase_secret(&sid, "me"), "twelve words")
                .unwrap();
        };
        let mem = Arc::new(rustket_core::secrets::MemoryStore::new());
        let store: Arc<dyn SecretStore> = mem.clone();
        fill(&mem);
        let mut s = server();
        run(forget_keys(&mut s, Some(&store), Some("me"))).await;
        assert!(mem.get(&private_key_secret(&sid, "me")).unwrap().is_none());
        assert!(
            mem.get(&pending_passphrase_secret(&sid, "me"))
                .unwrap()
                .is_some()
        );
        // An explicit sign-out (after the warning) deletes both.
        run(forget_secrets(&mut s, Some(&store), Some("me"))).await;
        assert!(
            mem.get(&pending_passphrase_secret(&sid, "me"))
                .unwrap()
                .is_none()
        );
    }
}
