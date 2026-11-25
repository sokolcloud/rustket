//! Desktop notifications honouring server and local preferences
//! (`rustket_core::notify::evaluate`).
//!
//! One notification per room: a new message replaces the room's previous
//! notification instead of stacking up, and the notification is closed once
//! the room was read: it was seen unread after the notification and no
//! longer is, or its last-seen (`ls`) reached the notified message (read on
//! another device before a state pass saw it unread). Activating it raises
//! the window and opens the room (or thread). Clicks, actions and dismissals
//! come back from one app-wide subscription and are routed by id. The
//! backend is a [`Notifier`] so tests use a mock: freedesktop
//! (`org.freedesktop.Notifications` over D-Bus) on Linux and the BSDs, toasts
//! on Windows, `UNUserNotificationCenter` on macOS. Every backend replaces a
//! room's notification in place and withdraws closed ones; the Windows and
//! macOS ones also clear what earlier launches left behind, whose ids would
//! collide with this launch's.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use futures::FutureExt;
use futures::future::BoxFuture;
use iced::{Subscription, Task};
use rocketchat::session::DesktopNotification;
use rocketchat::{RcDate, ServerUrl};
use rust_i18n::t;
use rustket_core::config::UiPrefs;
use rustket_core::notify::{
    IncomingNotification, NotificationMessage, NotificationPayload, NotificationPrefs,
    NotifyContext, evaluate,
};
use rustket_core::room_list::Avatar;
use rustket_core::{ServerId, Workspace};

use super::Request;
use crate::images::Images;

#[cfg(all(unix, not(target_os = "macos")))]
mod freedesktop;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(any(windows, target_os = "macos", test))]
mod native;
#[cfg(any(windows, target_os = "macos", test))]
mod text;
#[cfg(windows)]
mod toast;

#[cfg(all(unix, not(target_os = "macos")))]
pub use freedesktop::{ZbusNotifier, escape_markup};
#[cfg(target_os = "macos")]
pub use macos::install;

/// Key of the default action (clicking the notification).
pub const ACTION_DEFAULT: &str = "default";
/// Key of the "join" action of a video-conference ring.
pub const ACTION_JOIN: &str = "join";
/// `expire_timeout`: the server's default.
pub const TIMEOUT_DEFAULT: i32 = -1;
/// `expire_timeout`: stays until dismissed.
pub const TIMEOUT_NEVER: i32 = 0;
pub const URGENCY_NORMAL: u8 = 1;
pub const URGENCY_CRITICAL: u8 = 2;

/// A notification to show.
#[derive(Debug, Clone, PartialEq)]
pub struct Outgoing {
    /// Id of the notification this one replaces.
    pub replaces: Option<u32>,
    /// Plain text (escaped by the backend when the server parses markup).
    pub title: String,
    pub body: String,
    /// RGBA pixels of the sender's avatar: `(width, height, pixels)`.
    pub image: Option<(u32, u32, Vec<u8>)>,
    /// The sound that comes with the notification: a backend that can
    /// play sounds plays it ([`super::sound::route`]), so the system's Do
    /// Not Disturb applies; otherwise the app plays it after
    /// [`Event::Shown`] with `server_sound: false` or [`Event::Failed`].
    pub sound: Option<super::sound::RcSound>,
    /// `(key, label)`; the first is the default action.
    pub actions: Vec<(String, String)>,
    /// Milliseconds, or [`TIMEOUT_DEFAULT`] / [`TIMEOUT_NEVER`].
    pub timeout: i32,
    pub urgency: u8,
}

/// Result of showing a notification.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// Shown with this id; `server_sound`: the notification server plays
    /// [`Outgoing::sound`] itself.
    Shown {
        id: u32,
        server_sound: bool,
    },
    Failed(String),
    /// Not shown: the system does not allow the app's notifications (macOS,
    /// System Settings). Otherwise like [`Event::Failed`].
    NotAllowed,
}

/// A notification server signal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Signal {
    /// `ActionInvoked` with the action key.
    Action(String),
    /// `NotificationClosed` (expired, dismissed or closed by us).
    Closed,
}

/// The notification backend.
pub trait Notifier: Send + Sync + 'static {
    fn show(&self, n: Outgoing) -> BoxFuture<'static, Event>;
    fn close(&self, id: u32) -> BoxFuture<'static, ()>;
    /// Whether the app-wide signal subscription runs (real backend only).
    fn listens(&self) -> bool {
        false
    }
}

/// The platform's notification backend.
pub fn system_backend() -> Arc<dyn Notifier> {
    #[cfg(all(unix, not(target_os = "macos")))]
    return Arc::new(freedesktop::ZbusNotifier);
    #[cfg(windows)]
    return Arc::new(toast::ToastNotifier::new());
    #[cfg(target_os = "macos")]
    return Arc::new(macos::MacNotifier::new());
    #[cfg(not(any(unix, windows)))]
    return Arc::new(NullNotifier);
}

/// Clicks, actions and dismissals of every notification the platform
/// backend showed.
fn signals() -> impl futures::Stream<Item = Message> {
    #[cfg(all(unix, not(target_os = "macos")))]
    return freedesktop::signals();
    #[cfg(any(windows, target_os = "macos"))]
    return native::signals();
    #[cfg(not(any(unix, windows)))]
    return futures::stream::empty();
}

/// Shows nothing (tests, snapshots).
#[derive(Debug, Default)]
pub struct NullNotifier;

impl Notifier for NullNotifier {
    fn show(&self, _: Outgoing) -> BoxFuture<'static, Event> {
        async { Event::Failed("disabled".into()) }.boxed()
    }

    fn close(&self, _: u32) -> BoxFuture<'static, ()> {
        async {}.boxed()
    }
}

/// Records what would be shown (tests). Ids are 1, 2, … unless a
/// notification replaces one.
#[derive(Debug, Default, Clone)]
pub struct MockNotifier {
    pub shown: Arc<Mutex<Vec<Outgoing>>>,
    pub closed: Arc<Mutex<Vec<u32>>>,
    /// Plays the notifications' sounds (a server with the `sound`
    /// capability).
    pub plays_sound: bool,
}

impl Notifier for MockNotifier {
    fn show(&self, n: Outgoing) -> BoxFuture<'static, Event> {
        let n_sound = n.sound.is_some();
        let id = match self.shown.lock() {
            Ok(mut g) => {
                let id = n.replaces.unwrap_or(g.len() as u32 + 1);
                g.push(n);
                id
            }
            Err(_) => 0,
        };
        let server_sound = self.plays_sound && n_sound;
        async move { Event::Shown { id, server_sound } }.boxed()
    }

    fn close(&self, id: u32) -> BoxFuture<'static, ()> {
        if let Ok(mut g) = self.closed.lock() {
            g.push(id);
        }
        async {}.boxed()
    }
}

/// Where a notification leads.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Target {
    pub server_id: ServerId,
    pub rid: String,
}

#[derive(Debug, Clone)]
pub enum Message {
    /// A showing finished. `target`: `None` for the test notification; `seq`
    /// identifies the showing (a replaced notification's late result is
    /// ignored); `sound`: its [`Outgoing::sound`].
    Shown {
        target: Option<Target>,
        seq: u64,
        sound: Option<super::sound::RcSound>,
        event: Event,
    },
    /// A notification server signal for notification `id`.
    Signal { id: u32, signal: Signal },
}

/// What the app shows right now (decides suppression).
pub struct NotifyEnv<'a> {
    pub images: &'a Images,
    pub server: &'a ServerUrl,
    /// The notification's server is the one shown.
    pub server_active: bool,
    pub focused_rid: Option<&'a str>,
    pub focused_tmid: Option<&'a str>,
    /// The window is visible and focused.
    pub window_active: bool,
    /// More than one server is configured (the title names the server).
    pub multi_server: bool,
    /// Notifications are paused (`rustket_core::quiet`): none is shown and
    /// no sound plays; counters still update.
    pub paused: bool,
}

#[derive(Debug)]
struct Entry {
    id: Option<u32>,
    seq: u64,
    /// Thread of the latest message (opened on activation).
    tmid: Option<String>,
    /// The room was seen unread after the notification (so "read" means read).
    seen_unread: bool,
    /// Id of the notified message.
    mid: String,
    /// Server timestamp of the notified message, once the workspace knows
    /// it (looked up again on every read sync while unknown): a last-seen
    /// at or after it means read. Never compared with the local clock.
    about: Option<RcDate>,
    /// Call of a conference notification (its "Join" joins the call).
    conference_id: Option<String>,
    /// Shown for the conversation on screen (`muteFocusedConversations`
    /// off): reading it there is expected, so it is not closed on read.
    on_screen: bool,
}

pub struct Notifications {
    backend: Arc<dyn Notifier>,
    shown: HashMap<Target, Entry>,
    /// Id of the shown test notification.
    test_id: Option<u32>,
    seq: u64,
    /// The system blocks the app's notifications: the last showing ended
    /// [`Event::NotAllowed`] (cleared by the next shown one).
    os_blocked: bool,
}

impl std::fmt::Debug for Notifications {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Notifications")
            .field("shown", &self.shown)
            .field("seq", &self.seq)
            .finish()
    }
}

impl Default for Notifications {
    fn default() -> Self {
        Self::with_backend(Arc::new(NullNotifier))
    }
}

/// `{uid}/notification` payload in the core's shape.
pub fn incoming(n: &DesktopNotification) -> IncomingNotification {
    let p = &n.payload;
    IncomingNotification {
        title: n.title.clone(),
        text: n.text.clone(),
        duration: n.duration.map(|d| (d.max(0.0) * 1000.0) as u64),
        payload: NotificationPayload {
            id: p.id.clone(),
            rid: p.rid.clone(),
            tmid: p.tmid.clone(),
            sender: p.sender.clone(),
            room_type: p.room_type,
            name: p.name.clone(),
            message: Some(NotificationMessage {
                msg: p.message.msg.clone(),
                t: p.message.t.clone(),
            }),
        },
    }
}

/// Why the message sound of a shown notification stays silent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MessageSoundOff {
    /// Rustket's "Play a sound" is off.
    Setting,
    /// The room's sound (`audioNotificationValue`) is "None".
    Room,
    /// The account's new-message sound is "None".
    Account,
    /// The account's notification or master volume is zero.
    Volume,
}

/// The message sound, as Rocket.Chat web picks it
/// (`useNewMessageNotification`): `ui_sound` (`UiPrefs::sound`) must be on;
/// the room's own sound `room_sound` (`audioNotificationValue`) wins and
/// its "None" mutes the room; otherwise the account's new-message sound,
/// whose "None" mutes only rooms without a sound of their own. The volume
/// is the account's notification × master volume.
pub fn message_sound(
    ui_sound: bool,
    room_sound: Option<&str>,
    account: &super::AccountSound,
) -> Result<super::sound::RcSound, MessageSoundOff> {
    if !ui_sound {
        return Err(MessageSoundOff::Setting);
    }
    let name = match super::sound_choice(room_sound) {
        Some("none") => return Err(MessageSoundOff::Room),
        Some(own) => own,
        None => account.message_sound().ok_or(MessageSoundOff::Account)?,
    };
    match account.volume() {
        0 => Err(MessageSoundOff::Volume),
        volume => Ok(super::sound::RcSound {
            name: name.to_owned(),
            volume,
        }),
    }
}

/// The room's sound for `n`: the subscription's `audioNotificationValue`
/// (Rocket.Chat web reads it from the subscription), else the payload's; a
/// payload's "None" (conference rings) always wins.
fn room_sound<'a>(ws: &'a Workspace, n: &'a DesktopNotification) -> Option<&'a str> {
    let payload = n.payload.audio_notification_value.as_deref();
    if payload == Some("none") {
        return payload;
    }
    ws.subscription(&n.payload.rid)
        .and_then(|s| s.audio_notification_value.as_deref())
        .or(payload)
}

/// Why [`message_sound`] stays silent; `None` when it plays.
pub fn message_sound_off(
    ui_sound: bool,
    room_sound: Option<&str>,
    account: &super::AccountSound,
) -> Option<MessageSoundOff> {
    message_sound(ui_sound, room_sound, account).err()
}

/// The account's "require interaction" preference
/// (`desktopNotificationRequireInteraction`): notifications stay until
/// dismissed.
pub(crate) fn require_interaction_pref(ws: &Workspace) -> bool {
    super::user_pref(ws, "desktopNotificationRequireInteraction")
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
}

/// The account's "Mute focused conversations" preference
/// (`muteFocusedConversations`, Rocket.Chat's default on): no notification
/// for the conversation on screen.
pub(crate) fn mute_focused_pref(ws: &Workspace) -> bool {
    super::user_pref(ws, "muteFocusedConversations")
        .and_then(|v| v.as_bool())
        .unwrap_or(true)
}

/// Message `mid` of room `rid` is in the room's main timeline (a thread
/// reply sent to the channel too, `tshow`), as far as the workspace knows.
fn shown_in_room(ws: &Workspace, rid: &str, mid: &str) -> bool {
    ws.timeline(rid).is_some_and(|t| t.contains(mid))
        || ws
            .room(rid)
            .and_then(|e| e.room.as_ref())
            .and_then(|r| r.last_message.as_deref())
            .is_some_and(|m| m.id == mid && m.tshow == Some(true))
}

/// How long a thread reply's notification for the open room waits for the
/// reply itself: the server sends the notification before the message, and
/// only the message says whether it is also in the room (`tshow`).
pub(crate) const REPLY_WAIT: std::time::Duration = std::time::Duration::from_millis(500);

/// The notification of thread reply `n` must wait [`REPLY_WAIT`] before
/// Q4 is decided: the reply is in the open, focused room whose panel does
/// not show its thread, the account mutes the conversation on screen, and
/// the reply has not reached the workspace yet.
pub(crate) fn wait_for_reply(
    n: &DesktopNotification,
    ws: &Workspace,
    focused_rid: Option<&str>,
    focused_tmid: Option<&str>,
    on_screen: bool,
) -> bool {
    let p = &n.payload;
    let Some(tmid) = p.tmid.as_deref().filter(|t| !t.is_empty()) else {
        return false;
    };
    on_screen
        && focused_rid == Some(p.rid.as_str())
        && focused_tmid != Some(tmid)
        && mute_focused_pref(ws)
        && !shown_in_room(ws, &p.rid, &p.id)
}

/// `expire_timeout` and urgency: rings (`requireInteraction`) stay and are
/// critical; the "require interaction" preference keeps notifications;
/// otherwise the server's duration (ms) or the default.
fn timeout_and_urgency(n: &DesktopNotification, ws: &Workspace) -> (i32, u8) {
    if n.require_interaction == Some(true) {
        return (TIMEOUT_NEVER, URGENCY_CRITICAL);
    }
    if require_interaction_pref(ws) {
        return (TIMEOUT_NEVER, URGENCY_NORMAL);
    }
    let timeout = n
        .duration
        .filter(|d| d.is_finite() && *d > 0.0)
        .map_or(TIMEOUT_DEFAULT, |d| (d.min(3600.0) * 1000.0) as i32);
    (timeout, URGENCY_NORMAL)
}

fn avatar_rgba(
    images: &Images,
    sid: &ServerId,
    server: &ServerUrl,
    username: &str,
) -> Option<(u32, u32, Vec<u8>)> {
    let url = Images::avatar_url(
        server,
        &Avatar::User {
            username: username.to_owned(),
        },
    );
    match &images.get(sid, &url)?.handle {
        iced::widget::image::Handle::Rgba {
            width,
            height,
            pixels,
            ..
        } => Some((*width, *height, pixels.to_vec())),
        _ => None,
    }
}

/// Timestamp of message `mid` of room `rid`, if the workspace has it.
fn message_ts(ws: &Workspace, rid: &str, mid: &str) -> Option<RcDate> {
    if let Some(ts) = ws
        .timeline(rid)
        .and_then(|t| t.get(mid))
        .and_then(|i| i.message.ts)
    {
        return Some(ts);
    }
    ws.room(rid)
        .and_then(|e| e.room.as_ref())
        .and_then(|r| r.last_message.as_deref())
        .filter(|m| m.id == mid)
        .and_then(|m| m.ts)
}

/// The room's last-seen is at or after `about`.
fn seen_since(ws: &Workspace, rid: &str, about: RcDate) -> bool {
    ws.subscription(rid)
        .and_then(|s| s.ls)
        .is_some_and(|ls| ls >= about)
}

fn room_unread(ws: &Workspace, rid: &str) -> bool {
    ws.subscription(rid).is_some_and(|s| {
        s.unread > 0 || s.alert || s.tunread.as_ref().is_some_and(|t| !t.is_empty())
    })
}

impl Notifications {
    /// The notification backend (other components show their own
    /// notifications through it; their signals come back as
    /// [`Request::ForeignSignal`]).
    pub fn backend(&self) -> Arc<dyn Notifier> {
        Arc::clone(&self.backend)
    }

    pub fn with_backend(backend: Arc<dyn Notifier>) -> Self {
        Self {
            backend,
            shown: HashMap::new(),
            test_id: None,
            seq: 0,
            os_blocked: false,
        }
    }

    /// The system does not allow the app's notifications (as last seen).
    pub fn os_blocked(&self) -> bool {
        self.os_blocked
    }

    /// Shows `notification` unless a local rule suppresses it; returns the
    /// showing and the message sound to play now, without a notification
    /// (desktop notifications off or the toast suppressed: sound only). A
    /// shown notification carries its sound ([`Outgoing::sound`]).
    #[allow(clippy::too_many_arguments)]
    pub fn notify(
        &mut self,
        server_id: &ServerId,
        server_name: &str,
        notification: &DesktopNotification,
        workspace: &Workspace,
        prefs: &UiPrefs,
        env: &NotifyEnv<'_>,
    ) -> (Task<super::Message>, Option<super::sound::RcSound>) {
        // Not about a room: nothing to open (the web client skips these too).
        // Queued-inquiry notifications (manual routing) have no room: shown,
        // clicking only brings the server up (`docs/omnichannel.md` §5.6).
        if notification.payload.rid.is_empty() && !notification.payload.is_queued_inquiry() {
            return (Task::none(), None);
        }
        let n = incoming(notification);
        let ctx = NotifyContext {
            my_user_id: workspace.user_id().unwrap_or_default(),
            my_status: workspace.my_status(),
            focused_rid: env.focused_rid,
            focused_tmid: env.focused_tmid,
            window_active: env.window_active,
            server_active: env.server_active,
            prefs: NotificationPrefs::from(prefs),
            paused: env.paused,
            mute_focused: mute_focused_pref(workspace),
            reply_in_room: n.payload.tmid.is_some()
                && shown_in_room(workspace, &n.payload.rid, &n.payload.id),
        };
        let Some(spec) = evaluate(&n, &ctx) else {
            return (Task::none(), None);
        };
        let on_screen = !ctx.mute_focused
            && evaluate(
                &n,
                &NotifyContext {
                    mute_focused: true,
                    ..ctx
                },
            )
            .is_none();
        let sound = message_sound(
            spec.sound,
            room_sound(workspace, notification),
            &super::AccountSound::of(workspace),
        )
        .ok();
        if !spec.toast {
            return (Task::none(), sound);
        }
        let mut title = spec.title.clone();
        if env.multi_server && !server_name.is_empty() {
            title = format!("{title} · {server_name}");
        }
        let body = match spec.body {
            // The user's hidden words stay out of the preview too.
            Some(b) if prefs.hidden_words.matches(&b) => t!("desktop.hidden_body").into_owned(),
            Some(b) if !b.trim().is_empty() => b,
            Some(_) if notification.payload.message.content.is_some() => {
                t!("desktop.encrypted_body").into_owned()
            }
            _ => t!("desktop.hidden_body").into_owned(),
        };
        let image = notification
            .payload
            .sender
            .as_ref()
            .and_then(|s| s.username.as_deref())
            .and_then(|u| avatar_rgba(env.images, server_id, env.server, u));
        let mut actions = vec![(ACTION_DEFAULT.to_owned(), t!("desktop.open").into_owned())];
        let ring = notification.payload.conference_id.is_some()
            || notification
                .actions
                .iter()
                .flatten()
                .any(|a| a.action == ACTION_JOIN);
        if ring {
            actions.push((ACTION_JOIN.to_owned(), t!("desktop.join").into_owned()));
        }
        let (timeout, urgency) = timeout_and_urgency(notification, workspace);
        let target = Target {
            server_id: server_id.clone(),
            rid: spec.rid.clone(),
        };
        self.seq += 1;
        let seq = self.seq;
        let mid = notification.payload.id.clone();
        let about = message_ts(workspace, &spec.rid, &mid);
        let entry = self.shown.entry(target.clone()).or_insert(Entry {
            id: None,
            seq,
            tmid: None,
            seen_unread: false,
            mid: String::new(),
            about: None,
            conference_id: None,
            on_screen: false,
        });
        entry.seq = seq;
        entry.tmid = spec.tmid.clone();
        entry.seen_unread = false;
        entry.mid = mid;
        entry.about = about;
        entry.conference_id = notification.payload.conference_id.clone();
        entry.on_screen = on_screen;
        let out = Outgoing {
            replaces: entry.id,
            title,
            body,
            image,
            sound,
            actions,
            timeout,
            urgency,
        };
        (self.run(Some(target), seq, out), None)
    }

    /// "Send test notification" from the settings, with `sound`.
    pub fn test(&mut self, sound: Option<super::sound::RcSound>) -> Task<super::Message> {
        self.seq += 1;
        let out = Outgoing {
            replaces: self.test_id,
            title: t!("desktop.test_title").into_owned(),
            body: t!("desktop.test_body").into_owned(),
            image: None,
            sound,
            actions: vec![(ACTION_DEFAULT.to_owned(), t!("desktop.open").into_owned())],
            timeout: TIMEOUT_DEFAULT,
            urgency: URGENCY_NORMAL,
        };
        self.run(None, self.seq, out)
    }

    fn run(&self, target: Option<Target>, seq: u64, out: Outgoing) -> Task<super::Message> {
        let sound = out.sound.clone();
        Task::perform(self.backend.show(out), move |event| {
            super::Message::Notifications(Message::Shown {
                target: target.clone(),
                seq,
                sound: sound.clone(),
                event,
            })
        })
    }

    /// The app-wide signal subscription (real backend only).
    pub fn subscription(&self) -> Subscription<Message> {
        if self.backend.listens() {
            Subscription::run(signals)
        } else {
            Subscription::none()
        }
    }

    /// Returns `(task, requests, sound)`; `sound`: the app plays it, as the
    /// current showing's backend did not (it cannot, or it failed).
    pub fn update(
        &mut self,
        msg: Message,
    ) -> (
        Task<super::Message>,
        Vec<Request>,
        Option<super::sound::RcSound>,
    ) {
        match msg {
            Message::Shown {
                target,
                seq,
                sound,
                event,
            } => match event {
                Event::Shown { id, server_sound } => {
                    self.os_blocked = false;
                    let play = sound.filter(|_| !server_sound);
                    let Some(t) = target else {
                        self.test_id = Some(id);
                        return (Task::none(), Vec::new(), play);
                    };
                    match self.shown.get_mut(&t) {
                        Some(e) if e.seq == seq => {
                            e.id = Some(id);
                            (Task::none(), Vec::new(), play)
                        }
                        // A replaced showing that got its own id (it raced
                        // with the newer one): close the duplicate.
                        Some(e) if e.id != Some(id) => (self.close_ids(vec![id]), Vec::new(), None),
                        Some(_) => (Task::none(), Vec::new(), None),
                        None => (self.close_ids(vec![id]), Vec::new(), None),
                    }
                }
                Event::Failed(_) | Event::NotAllowed => {
                    let mut requests = Vec::new();
                    if let Event::Failed(e) = &event {
                        tracing::debug!(error = %e, "notification not shown");
                    } else {
                        self.os_blocked = true;
                        // Only the test (asked for in the settings) says why.
                        if target.is_none() {
                            requests.push(Request::NotificationsBlocked);
                        }
                    }
                    let current = match &target {
                        Some(t) => self.shown.get(t).is_some_and(|e| e.seq == seq),
                        None => true,
                    };
                    if current && let Some(t) = &target {
                        self.shown.remove(t);
                    }
                    // Not shown: the sound plays alone.
                    (Task::none(), requests, sound.filter(|_| current))
                }
            },
            Message::Signal { id, signal } => {
                let (task, requests) = self.signal(id, signal);
                (task, requests, None)
            }
        }
    }

    /// A notification server signal for notification `id`.
    fn signal(&mut self, id: u32, signal: Signal) -> (Task<super::Message>, Vec<Request>) {
        let target = self
            .shown
            .iter()
            .find(|(_, e)| e.id == Some(id))
            .map(|(t, _)| t.clone());
        let activated =
            matches!(&signal, Signal::Action(k) if k == ACTION_DEFAULT || k == ACTION_JOIN);
        if let Some(t) = target {
            if !activated && signal != Signal::Closed {
                return (Task::none(), Vec::new());
            }
            let Some(entry) = self.shown.remove(&t) else {
                return (Task::none(), Vec::new());
            };
            if !activated {
                return (Task::none(), Vec::new());
            }
            if signal == Signal::Action(ACTION_JOIN.to_owned())
                && let Some(call_id) = entry.conference_id
            {
                let server_id = t.server_id.clone();
                return (
                    Task::none(),
                    vec![
                        Request::ShowWindow,
                        Request::OpenRoom {
                            server_id: t.server_id,
                            rid: t.rid,
                        },
                        Request::JoinCall { server_id, call_id },
                    ],
                );
            }
            let open = match entry.tmid {
                Some(tmid) => Request::OpenThread {
                    server_id: t.server_id,
                    rid: t.rid,
                    tmid,
                },
                None => Request::OpenRoom {
                    server_id: t.server_id,
                    rid: t.rid,
                },
            };
            return (Task::none(), vec![Request::ShowWindow, open]);
        }
        if self.test_id != Some(id) {
            // Not ours (e.g. a call notification of `videoconf`).
            return (Task::none(), vec![Request::ForeignSignal(id, signal)]);
        }
        if self.test_id == Some(id) {
            if activated {
                self.test_id = None;
                return (Task::none(), vec![Request::ShowWindow]);
            }
            if signal == Signal::Closed {
                self.test_id = None;
            }
        }
        (Task::none(), Vec::new())
    }

    /// Closes the notifications of `sid`'s rooms that were read since: the
    /// room is not unread and either was seen unread after the notification
    /// or its last-seen is at or after the notified message's server
    /// timestamp (read elsewhere before a state pass saw it unread). While
    /// the workspace does not know that timestamp, only the first rule
    /// applies.
    pub fn sync_read(&mut self, sid: &ServerId, ws: &Workspace) -> Task<super::Message> {
        let mut close = Vec::new();
        self.shown.retain(|t, e| {
            if &t.server_id != sid || e.on_screen {
                return true;
            }
            if room_unread(ws, &t.rid) {
                e.seen_unread = true;
                return true;
            }
            if e.about.is_none() {
                e.about = message_ts(ws, &t.rid, &e.mid);
            }
            if e.seen_unread || e.about.is_some_and(|about| seen_since(ws, &t.rid, about)) {
                close.extend(e.id);
                return false;
            }
            true
        });
        self.close_ids(close)
    }

    /// Closes every notification of servers not in `alive` (logged out, removed).
    pub fn retain_servers(&mut self, alive: &[&ServerId]) -> Task<super::Message> {
        let mut close = Vec::new();
        self.shown.retain(|t, e| {
            let keep = alive.contains(&&t.server_id);
            if !keep {
                close.extend(e.id);
            }
            keep
        });
        self.close_ids(close)
    }

    fn close_ids(&self, ids: Vec<u32>) -> Task<super::Message> {
        if ids.is_empty() {
            return Task::none();
        }
        let futs: Vec<_> = ids.into_iter().map(|id| self.backend.close(id)).collect();
        Task::future(futures::future::join_all(futs)).discard()
    }

    /// Number of rooms with a shown notification.
    pub fn len(&self) -> usize {
        self.shown.len()
    }

    pub fn is_empty(&self) -> bool {
        self.shown.is_empty()
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use futures::StreamExt;
    use rocketchat::models::UserStatus;
    use serde_json::json;

    fn notification(rid: &str, mid: &str, tmid: Option<&str>) -> DesktopNotification {
        serde_json::from_value(json!({
            "title": "bob",
            "text": "hello there",
            "payload": {
                "_id": mid, "rid": rid, "tmid": tmid,
                "sender": {"_id": "bobid", "username": "bob", "name": "Bob"},
                "type": "d",
                "message": {"msg": "hello there"}
            }
        }))
        .unwrap()
    }

    fn workspace() -> Workspace {
        let mut ws = Workspace::new();
        ws.apply_me(
            serde_json::from_value(json!({"_id": "me", "username": "me", "status": "online"}))
                .unwrap(),
        );
        ws
    }

    fn sub(rid: &str, unread: u64) -> rocketchat::models::Subscription {
        serde_json::from_value(json!({
            "_id": format!("s-{rid}"), "rid": rid, "name": "bob", "t": "d",
            "open": true, "alert": unread > 0, "unread": unread,
            "u": {"_id": "me", "username": "me"}
        }))
        .unwrap()
    }

    struct Fixture {
        images: Images,
        server: ServerUrl,
    }

    impl Fixture {
        fn new() -> Self {
            Self {
                images: Images::new(None),
                server: ServerUrl::parse("http://localhost:3000").unwrap(),
            }
        }

        fn env(&self, window_active: bool, focused_rid: Option<&'static str>) -> NotifyEnv<'_> {
            NotifyEnv {
                images: &self.images,
                server: &self.server,
                server_active: true,
                focused_rid,
                focused_tmid: None,
                window_active,
                multi_server: false,
                paused: false,
            }
        }
    }

    /// Runs a task to completion and feeds its messages back.
    fn drain(n: &mut Notifications, task: Task<super::super::Message>) -> Vec<Request> {
        let mut out = Vec::new();
        let Some(stream) = iced_runtime::task::into_stream(task) else {
            return out;
        };
        let actions: Vec<_> = futures::executor::block_on(stream.collect::<Vec<_>>());
        for a in actions {
            if let iced_runtime::Action::Output(super::super::Message::Notifications(m)) = a {
                let (t, r, _) = n.update(m);
                out.extend(r);
                out.extend(drain(n, t));
            }
        }
        out
    }

    #[test]
    fn shows_replaces_per_room_and_respects_focus() {
        let mock = MockNotifier::default();
        let mut n = Notifications::with_backend(Arc::new(mock.clone()));
        let fx = Fixture::new();
        let ws = workspace();
        let sid = ServerId("s1".into());
        let prefs = UiPrefs::default();

        // Room on screen in the focused window: nothing.
        let (t, _) = n.notify(
            &sid,
            "Dev",
            &notification("r1", "m0", None),
            &ws,
            &prefs,
            &fx.env(true, Some("r1")),
        );
        drain(&mut n, t);
        assert!(mock.shown.lock().unwrap().is_empty());

        let (t, _) = n.notify(
            &sid,
            "Dev",
            &notification("r1", "m1", None),
            &ws,
            &prefs,
            &fx.env(false, Some("r1")),
        );
        drain(&mut n, t);
        let (t, sound) = n.notify(
            &sid,
            "Dev",
            &notification("r1", "m2", None),
            &ws,
            &prefs,
            &fx.env(false, None),
        );
        drain(&mut n, t);
        let shown = mock.shown.lock().unwrap().clone();
        assert_eq!(shown.len(), 2);
        assert_eq!(shown[0].replaces, None);
        assert_eq!(shown[1].replaces, Some(1), "same room replaces");
        assert_eq!(shown[1].title, "bob");
        assert_eq!(shown[1].body, "hello there");
        // Rocket.Chat's default sound comes with the notification.
        assert_eq!(sound, None);
        assert_eq!(
            shown[1].sound,
            Some(crate::desktop::sound::RcSound {
                name: "chime".into(),
                volume: 100
            })
        );
        assert_eq!(n.len(), 1);
    }

    /// Q4: `muteFocusedConversations` (default on) and what counts as on
    /// screen for a thread reply.
    #[test]
    fn focused_conversation_follows_the_account_preference() {
        let mock = MockNotifier::default();
        let mut n = Notifications::with_backend(Arc::new(mock.clone()));
        let fx = Fixture::new();
        let mut ws = workspace();
        let sid = ServerId("s1".into());
        let prefs = UiPrefs::default();
        let show = |n: &mut Notifications, ws: &Workspace, mid: &str, tmid: Option<&str>| {
            let (t, _) = n.notify(
                &sid,
                "Dev",
                &notification("r1", mid, tmid),
                ws,
                &prefs,
                &fx.env(true, Some("r1")),
            );
            drain(n, t);
            mock.shown.lock().unwrap().len()
        };
        assert!(mute_focused_pref(&ws), "absent: Rocket.Chat's default");
        assert_eq!(show(&mut n, &ws, "m0", None), 0, "room on screen");
        // A reply in the open room without its thread panel is not on screen.
        assert_eq!(show(&mut n, &ws, "m1", Some("t1")), 1);
        // The same kind of reply also sent to the channel is.
        ws.apply_room(
            rustket_core::Change::Inserted,
            serde_json::from_value(json!({
                "_id": "r1", "t": "d",
                "lastMessage": {"_id": "m2", "rid": "r1", "msg": "x", "u": {"_id": "bobid"},
                    "tmid": "t1", "tshow": true}
            }))
            .unwrap(),
        );
        assert_eq!(show(&mut n, &ws, "m2", Some("t1")), 1);
        // Preference off: the conversation on screen notifies too.
        ws.apply_me(
            serde_json::from_value(json!({"_id": "me", "username": "me", "status": "online",
                "settings": {"preferences": {"muteFocusedConversations": false}}}))
            .unwrap(),
        );
        assert!(!mute_focused_pref(&ws));
        assert_eq!(show(&mut n, &ws, "m3", None), 2);
        assert_eq!(show(&mut n, &ws, "m4", Some("t1")), 3);
        // Read right away where it is on screen: the toast stays (it is not
        // closed on read like a toast of a conversation elsewhere; the
        // reply above, without its thread panel, was not on screen).
        assert_eq!(show(&mut n, &ws, "m5", None), 4);
        ws.apply_subscription(rustket_core::workspace::Change::Inserted, sub("r1", 1));
        let t = n.sync_read(&sid, &ws);
        drain(&mut n, t);
        ws.apply_subscription(rustket_core::workspace::Change::Updated, sub("r1", 0));
        let t = n.sync_read(&sid, &ws);
        drain(&mut n, t);
        assert_eq!(n.len(), 1);
        assert!(mock.closed.lock().unwrap().is_empty());
    }

    #[test]
    fn preview_sound_and_busy_prefs() {
        let mock = MockNotifier::default();
        let mut n = Notifications::with_backend(Arc::new(mock.clone()));
        let fx = Fixture::new();
        let mut ws = workspace();
        let sid = ServerId("s1".into());
        let prefs = UiPrefs {
            notification_preview: false,
            sound: false,
            ..UiPrefs::default()
        };
        let (t, sound) = n.notify(
            &sid,
            "Dev",
            &notification("r1", "m1", None),
            &ws,
            &prefs,
            &fx.env(false, None),
        );
        drain(&mut n, t);
        let shown = mock.shown.lock().unwrap().clone();
        assert_ne!(shown[0].body, "hello there");
        assert!(!shown[0].body.is_empty());
        assert_eq!(sound, None);
        assert_eq!(shown[0].sound, None);

        ws.apply_user_status("me", Some("me"), UserStatus::Busy, None);
        let (t, sound) = n.notify(
            &sid,
            "Dev",
            &notification("r2", "m2", None),
            &ws,
            &UiPrefs::default(),
            &fx.env(false, None),
        );
        drain(&mut n, t);
        assert_eq!(mock.shown.lock().unwrap().len(), 1, "busy: do not disturb");
        assert_eq!(sound, None);
    }

    /// A message with one of the hidden words shows no preview.
    #[test]
    fn hidden_words_keep_the_preview_out() {
        let mock = MockNotifier::default();
        let mut n = Notifications::with_backend(Arc::new(mock.clone()));
        let fx = Fixture::new();
        let ws = workspace();
        let sid = ServerId("s1".into());
        let mut prefs = UiPrefs::default();
        prefs.hidden_words.add("THERE").unwrap();
        let (t, _) = n.notify(
            &sid,
            "Dev",
            &notification("r1", "m1", None),
            &ws,
            &prefs,
            &fx.env(false, None),
        );
        drain(&mut n, t);
        let shown = mock.shown.lock().unwrap().clone();
        assert_eq!(shown.len(), 1, "still notified");
        assert!(!shown[0].body.contains("there"), "{}", shown[0].body);
        assert!(!shown[0].body.is_empty());
    }

    /// "Play a sound" without desktop notifications: the sound alone.
    #[test]
    fn sound_without_notifications() {
        let mock = MockNotifier::default();
        let mut n = Notifications::with_backend(Arc::new(mock.clone()));
        let fx = Fixture::new();
        let ws = workspace();
        let sid = ServerId("s1".into());
        let prefs = UiPrefs {
            desktop_notifications: false,
            ..UiPrefs::default()
        };
        let (t, sound) = n.notify(
            &sid,
            "Dev",
            &notification("r1", "m1", None),
            &ws,
            &prefs,
            &fx.env(false, None),
        );
        drain(&mut n, t);
        assert!(mock.shown.lock().unwrap().is_empty());
        assert_eq!(sound.map(|s| s.name).as_deref(), Some("chime"));
        assert_eq!(n.len(), 0);
        // The gates still apply: the room on screen stays silent.
        let (_, sound) = n.notify(
            &sid,
            "Dev",
            &notification("r1", "m2", None),
            &ws,
            &prefs,
            &fx.env(true, Some("r1")),
        );
        assert_eq!(sound, None);
        let off = UiPrefs {
            sound: false,
            ..prefs
        };
        let (_, sound) = n.notify(
            &sid,
            "Dev",
            &notification("r1", "m3", None),
            &ws,
            &off,
            &fx.env(false, None),
        );
        assert_eq!(sound, None);
    }

    #[test]
    fn activation_opens_room_or_thread() {
        let mut n = Notifications::default();
        let target = Target {
            server_id: ServerId("s1".into()),
            rid: "r1".into(),
        };
        n.shown.insert(
            target.clone(),
            Entry {
                id: Some(7),
                seq: 3,
                tmid: Some("t1".into()),
                seen_unread: false,
                mid: String::new(),
                about: None,
                conference_id: None,
                on_screen: false,
            },
        );
        let signal = |id, signal| Message::Signal { id, signal };
        // Another component's notification: handed on; an action we did not offer.
        let (_, req, _) = n.update(signal(8, Signal::Action("default".into())));
        assert!(matches!(req.as_slice(), [Request::ForeignSignal(8, _)]));
        let (_, req, _) = n.update(signal(7, Signal::Action("other".into())));
        assert!(req.is_empty() && n.len() == 1);
        let (_, req, _) = n.update(signal(7, Signal::Action("default".into())));
        assert!(matches!(req[0], Request::ShowWindow));
        assert!(
            matches!(&req[1], Request::OpenThread { rid, tmid, .. } if rid == "r1" && tmid == "t1")
        );
        assert!(n.is_empty());
        // The test notification only raises the window.
        n.test_id = Some(9);
        let (_, req, _) = n.update(signal(9, Signal::Action("default".into())));
        assert!(matches!(req.as_slice(), [Request::ShowWindow]));
        // Closing forgets the room.
        n.shown.insert(
            target.clone(),
            Entry {
                id: Some(5),
                seq: 4,
                tmid: None,
                seen_unread: false,
                mid: String::new(),
                about: None,
                conference_id: None,
                on_screen: false,
            },
        );
        let (_, req, _) = n.update(signal(5, Signal::Closed));
        assert!(req.is_empty() && n.is_empty());
        // A ring's "join" opens the room.
        n.shown.insert(
            target,
            Entry {
                id: Some(6),
                seq: 5,
                tmid: None,
                seen_unread: false,
                mid: String::new(),
                about: None,
                conference_id: None,
                on_screen: false,
            },
        );
        let (_, req, _) = n.update(signal(6, Signal::Action("join".into())));
        assert!(matches!(&req[1], Request::OpenRoom { rid, .. } if rid == "r1"));
        // A conference notification's "join" also joins the call.
        n.shown.insert(
            Target {
                server_id: ServerId("s1".into()),
                rid: "r1".into(),
            },
            Entry {
                id: Some(10),
                seq: 6,
                tmid: None,
                seen_unread: false,
                mid: String::new(),
                about: None,
                conference_id: Some("c1".into()),
                on_screen: false,
            },
        );
        let (_, req, _) = n.update(signal(10, Signal::Action("join".into())));
        assert!(matches!(&req[2], Request::JoinCall { call_id, .. } if call_id == "c1"));
    }

    /// Not allowed by the system: like a failure (the sound plays alone, the
    /// room's entry is dropped), remembered until a notification shows, and
    /// only the test asks the app to say why.
    #[test]
    fn not_allowed_is_remembered_and_explained_for_the_test() {
        use crate::desktop::sound::RcSound;
        let chime = Some(RcSound {
            name: "chime".into(),
            volume: 100,
        });
        let mut n = Notifications::default();
        let target = Target {
            server_id: ServerId("s1".into()),
            rid: "r1".into(),
        };
        let msg = |target: Option<Target>, seq, event| Message::Shown {
            target,
            seq,
            sound: chime.clone(),
            event,
        };
        assert!(!n.os_blocked());
        n.shown.insert(
            target.clone(),
            Entry {
                id: None,
                seq: 1,
                tmid: None,
                seen_unread: false,
                mid: String::new(),
                about: None,
                conference_id: None,
                on_screen: false,
            },
        );
        // A room's notification: the sound alone, no request.
        let (_, req, sound) = n.update(msg(Some(target.clone()), 1, Event::NotAllowed));
        assert!(req.is_empty());
        assert_eq!(sound, chime);
        assert!(n.is_empty());
        assert!(n.os_blocked());
        // The test notification: the sound alone and the explanation.
        let (_, req, sound) = n.update(msg(None, 2, Event::NotAllowed));
        assert!(matches!(req.as_slice(), [Request::NotificationsBlocked]));
        assert_eq!(sound, chime);
        assert!(n.os_blocked());
        // Another failure neither explains nor clears it.
        let (_, req, _) = n.update(msg(None, 3, Event::Failed("x".into())));
        assert!(req.is_empty());
        assert!(n.os_blocked());
        // A shown notification clears it.
        let shown = Event::Shown {
            id: 1,
            server_sound: true,
        };
        let (_, req, sound) = n.update(msg(None, 4, shown));
        assert!(req.is_empty());
        assert_eq!(sound, None);
        assert!(!n.os_blocked());
    }

    /// The app plays a notification's sound only when its backend did not:
    /// it cannot (KDE), or the notification failed.
    #[test]
    fn local_sound_only_when_the_notification_does_not_play_it() {
        use crate::desktop::sound::RcSound;
        let chime = Some(RcSound {
            name: "chime".into(),
            volume: 100,
        });
        let mut n = Notifications::default();
        let target = Target {
            server_id: ServerId("s1".into()),
            rid: "r1".into(),
        };
        let entry = || Entry {
            id: None,
            seq: 1,
            tmid: None,
            seen_unread: false,
            mid: String::new(),
            about: None,
            conference_id: None,
            on_screen: false,
        };
        let msg = |target: Option<Target>, seq, event| Message::Shown {
            target,
            seq,
            sound: chime.clone(),
            event,
        };
        let shown = |server_sound| Event::Shown {
            id: 1,
            server_sound,
        };
        n.shown.insert(target.clone(), entry());
        assert_eq!(n.update(msg(Some(target.clone()), 1, shown(true))).2, None);
        assert_eq!(
            n.update(msg(Some(target.clone()), 1, shown(false))).2,
            chime
        );
        // Failed: the sound alone, unless a newer showing replaced it.
        let failed = || Event::Failed("no server".into());
        assert_eq!(n.update(msg(Some(target.clone()), 2, failed())).2, None);
        assert_eq!(n.update(msg(Some(target.clone()), 1, failed())).2, chime);
        assert!(n.is_empty());
        // The test notification.
        assert_eq!(n.update(msg(None, 3, failed())).2, chime);
        assert_eq!(n.update(msg(None, 4, shown(true))).2, None);

        // Through a backend that plays sounds: nothing to play locally.
        let mock = MockNotifier {
            plays_sound: true,
            ..MockNotifier::default()
        };
        let mut n = Notifications::with_backend(Arc::new(mock.clone()));
        let fx = Fixture::new();
        let (t, now) = n.notify(
            &ServerId("s1".into()),
            "Dev",
            &notification("r1", "m1", None),
            &workspace(),
            &UiPrefs::default(),
            &fx.env(false, None),
        );
        assert_eq!(now, None);
        let stream = iced_runtime::task::into_stream(t).unwrap();
        let mut played = Vec::new();
        for a in futures::executor::block_on(stream.collect::<Vec<_>>()) {
            if let iced_runtime::Action::Output(super::super::Message::Notifications(m)) = a {
                played.push(n.update(m).2);
            }
        }
        assert_eq!(played, vec![None]);
        assert_eq!(mock.shown.lock().unwrap()[0].sound, chime);
    }

    #[test]
    fn stale_showings_do_not_take_over() {
        let mock = MockNotifier::default();
        let mut n = Notifications::with_backend(Arc::new(mock.clone()));
        let target = Target {
            server_id: ServerId("s1".into()),
            rid: "r1".into(),
        };
        n.shown.insert(
            target.clone(),
            Entry {
                id: Some(3),
                seq: 2,
                tmid: None,
                seen_unread: false,
                mid: String::new(),
                about: None,
                conference_id: None,
                on_screen: false,
            },
        );
        // An older showing finished late with a different id: closed.
        let (t, _, sound) = n.update(Message::Shown {
            target: Some(target.clone()),
            seq: 1,
            sound: Some(crate::desktop::sound::RcSound {
                name: "chime".into(),
                volume: 100,
            }),
            event: Event::Shown {
                id: 4,
                server_sound: false,
            },
        });
        // The newer showing has its own sound.
        assert_eq!(sound, None);
        drain(&mut n, t);
        assert_eq!(*mock.closed.lock().unwrap(), vec![4]);
        assert_eq!(n.shown[&target].id, Some(3));
    }

    #[test]
    fn message_sound_picks_room_then_account() {
        use crate::desktop::AccountSound;
        let name = |ui: bool, room: Option<&str>, acc: &AccountSound| {
            message_sound(ui, room, acc).map(|s| (s.name, s.volume))
        };
        let def = AccountSound::default();
        assert_eq!(name(true, None, &def), Ok(("chime".into(), 100)));
        assert_eq!(name(true, Some("default"), &def), Ok(("chime".into(), 100)));
        assert_eq!(name(true, Some(""), &def), Ok(("chime".into(), 100)));
        assert_eq!(name(true, Some("door"), &def), Ok(("door".into(), 100)));
        assert_eq!(name(true, Some("none"), &def), Err(MessageSoundOff::Room));
        assert_eq!(
            name(false, Some("door"), &def),
            Err(MessageSoundOff::Setting)
        );
        let ding = AccountSound {
            new_message: Some("ding".into()),
            ..AccountSound::default()
        };
        assert_eq!(name(true, None, &ding), Ok(("ding".into(), 100)));
        // The account's "None" mutes only rooms without their own sound.
        let none = AccountSound {
            new_message: Some("none".into()),
            ..AccountSound::default()
        };
        assert_eq!(name(true, None, &none), Err(MessageSoundOff::Account));
        assert_eq!(name(true, Some("beep"), &none), Ok(("beep".into(), 100)));
        // Volumes multiply; zero mutes.
        let half = AccountSound {
            notifications_volume: Some(50.0),
            master_volume: Some(50.0),
            ..AccountSound::default()
        };
        assert_eq!(name(true, None, &half), Ok(("chime".into(), 25)));
        let zero = AccountSound {
            master_volume: Some(0.0),
            ..AccountSound::default()
        };
        assert_eq!(
            name(true, Some("door"), &zero),
            Err(MessageSoundOff::Volume)
        );
        assert_eq!(
            message_sound_off(true, None, &none),
            Some(MessageSoundOff::Account)
        );
    }

    #[test]
    fn room_sound_comes_from_the_subscription() {
        let mock = MockNotifier::default();
        let mut n = Notifications::with_backend(Arc::new(mock.clone()));
        let fx = Fixture::new();
        let mut ws = workspace();
        let sid = ServerId("s1".into());
        let prefs = UiPrefs::default();
        let mut quiet = notification("r1", "m1", None);
        quiet.payload.audio_notification_value = Some("none".into());
        let last = || mock.shown.lock().unwrap().last().unwrap().sound.clone();
        let (t, sound) = n.notify(&sid, "Dev", &quiet, &ws, &prefs, &fx.env(false, None));
        drain(&mut n, t);
        assert_eq!(sound, None);
        assert_eq!(last(), None, "the payload's None");
        let mut s = sub("r2", 1);
        s.audio_notification_value = Some("highbell".into());
        ws.apply_subscription(rustket_core::Change::Inserted, s);
        let (_, now) = n.notify(
            &sid,
            "Dev",
            &notification("r2", "m2", None),
            &ws,
            &prefs,
            &fx.env(false, None),
        );
        assert_eq!(now, None);
        let sound = last();
        assert_eq!(sound.map(|s| s.name).as_deref(), Some("highbell"));
        let mut door = notification("r3", "m3", None);
        door.payload.audio_notification_value = Some("door".into());
        let (_, now) = n.notify(&sid, "Dev", &door, &ws, &prefs, &fx.env(false, None));
        assert_eq!(now, None);
        let sound = last();
        assert_eq!(sound.map(|s| s.name).as_deref(), Some("door"));
        ws.apply_me(
            serde_json::from_value(json!({
                "_id": "me", "username": "me", "status": "online",
                "settings": {"preferences": {"newMessageNotification": "none",
                    "notificationsSoundVolume": 40}}
            }))
            .unwrap(),
        );
        let (_, now) = n.notify(
            &sid,
            "Dev",
            &notification("r4", "m4", None),
            &ws,
            &prefs,
            &fx.env(false, None),
        );
        assert_eq!(now, None);
        let sound = last();
        assert_eq!(sound, None, "account None");
        let (_, now) = n.notify(
            &sid,
            "Dev",
            &notification("r2", "m5", None),
            &ws,
            &prefs,
            &fx.env(false, None),
        );
        assert_eq!(now, None);
        let sound = last();
        assert_eq!(
            sound,
            Some(crate::desktop::sound::RcSound {
                name: "highbell".into(),
                volume: 40
            })
        );
    }

    #[test]
    fn duration_interaction_rings_and_roomless_payloads() {
        let mock = MockNotifier::default();
        let mut n = Notifications::with_backend(Arc::new(mock.clone()));
        let fx = Fixture::new();
        let mut ws = workspace();
        let sid = ServerId("s1".into());
        let prefs = UiPrefs::default();
        let mut x = notification("", "m0", None);
        let (t, _) = n.notify(&sid, "Dev", &x, &ws, &prefs, &fx.env(false, None));
        drain(&mut n, t);
        assert!(mock.shown.lock().unwrap().is_empty(), "no rid: skipped");

        x.payload.rid = "r1".into();
        x.duration = Some(10.0);
        let (t, _) = n.notify(&sid, "Dev", &x, &ws, &prefs, &fx.env(false, None));
        drain(&mut n, t);
        let mut ring = notification("r2", "m2", None);
        ring.require_interaction = Some(true);
        ring.payload.conference_id = Some("c1".into());
        let (t, _) = n.notify(&sid, "Dev", &ring, &ws, &prefs, &fx.env(false, None));
        drain(&mut n, t);
        ws.apply_me(
            serde_json::from_value(json!({
                "_id": "me", "username": "me", "status": "online",
                "settings": {"preferences": {"desktopNotificationRequireInteraction": true}}
            }))
            .unwrap(),
        );
        let (t, _) = n.notify(
            &sid,
            "Dev",
            &notification("r3", "m3", None),
            &ws,
            &prefs,
            &fx.env(false, None),
        );
        drain(&mut n, t);
        let shown = mock.shown.lock().unwrap().clone();
        assert_eq!(
            (shown[0].timeout, shown[0].urgency),
            (10_000, URGENCY_NORMAL)
        );
        assert_eq!(shown[0].actions.len(), 1);
        assert_eq!(
            (shown[1].timeout, shown[1].urgency),
            (TIMEOUT_NEVER, URGENCY_CRITICAL)
        );
        assert_eq!(shown[1].actions[1].0, ACTION_JOIN);
        assert_eq!(
            (shown[2].timeout, shown[2].urgency),
            (TIMEOUT_NEVER, URGENCY_NORMAL)
        );
    }

    /// The Help page's "stays until dismissed" follows the same preference
    /// as the notification's timeout.
    #[test]
    fn help_sticky_agrees_with_the_timeout() {
        let mut ws = workspace();
        let n = notification("r1", "m1", None);
        for on in [false, true] {
            ws.apply_me(
                serde_json::from_value(json!({
                    "_id": "me", "username": "me", "status": "online",
                    "settings": {"preferences": {"desktopNotificationRequireInteraction": on}}
                }))
                .unwrap(),
            );
            let sticky = crate::help::model::AccountPrefs::of(&ws).require_interaction;
            assert_eq!(sticky, on);
            assert_eq!(timeout_and_urgency(&n, &ws).0 == TIMEOUT_NEVER, sticky);
        }
    }

    #[test]
    fn closes_when_the_room_is_read() {
        let mock = MockNotifier::default();
        let mut n = Notifications::with_backend(Arc::new(mock.clone()));
        let fx = Fixture::new();
        let mut ws = workspace();
        let sid = ServerId("s1".into());
        let (t, _) = n.notify(
            &sid,
            "Dev",
            &notification("r1", "m1", None),
            &ws,
            &UiPrefs::default(),
            &fx.env(false, None),
        );
        drain(&mut n, t);
        // The subscription update may arrive after the notification: an
        // unseen unread state does not close it.
        let t = n.sync_read(&sid, &ws);
        drain(&mut n, t);
        assert_eq!(n.len(), 1);
        ws.apply_subscription(rustket_core::workspace::Change::Inserted, sub("r1", 1));
        let t = n.sync_read(&sid, &ws);
        drain(&mut n, t);
        assert_eq!(n.len(), 1);
        ws.apply_subscription(rustket_core::workspace::Change::Updated, sub("r1", 0));
        let t = n.sync_read(&sid, &ws);
        drain(&mut n, t);
        assert!(n.is_empty());
        assert_eq!(*mock.closed.lock().unwrap(), vec![1]);
    }

    fn sub_seen(rid: &str, ls: &str) -> rocketchat::models::Subscription {
        let mut s = sub(rid, 0);
        s.ls = RcDate::parse_iso(ls);
        s
    }

    #[test]
    fn closes_when_read_elsewhere_before_seen_unread() {
        use rustket_core::workspace::Change;
        let mock = MockNotifier::default();
        let mut n = Notifications::with_backend(Arc::new(mock.clone()));
        let fx = Fixture::new();
        let mut ws = workspace();
        let sid = ServerId("s1".into());
        // The workspace knows the message: its timestamp is what counts.
        ws.apply_room(
            Change::Inserted,
            serde_json::from_value(json!({
                "_id": "r1", "t": "d",
                "lastMessage": {"_id": "m1", "rid": "r1", "msg": "hello there",
                    "ts": "2026-01-01T10:00:00.000Z", "u": {"_id": "bobid", "username": "bob"}}
            }))
            .unwrap(),
        );
        ws.apply_subscription(Change::Inserted, sub_seen("r1", "2026-01-01T09:00:00.000Z"));
        let (t, _) = n.notify(
            &sid,
            "Dev",
            &notification("r1", "m1", None),
            &ws,
            &UiPrefs::default(),
            &fx.env(false, None),
        );
        drain(&mut n, t);
        // Last seen before the message (the unread update is still on its way).
        let t = n.sync_read(&sid, &ws);
        drain(&mut n, t);
        assert_eq!(n.len(), 1);
        ws.apply_subscription(Change::Updated, sub_seen("r1", "2026-01-01T09:59:59.999Z"));
        let t = n.sync_read(&sid, &ws);
        drain(&mut n, t);
        assert_eq!(n.len(), 1);
        // Read on another device: never seen unread here, still closed.
        ws.apply_subscription(Change::Updated, sub_seen("r1", "2026-01-01T10:00:00.000Z"));
        let t = n.sync_read(&sid, &ws);
        drain(&mut n, t);
        assert!(n.is_empty());
        assert_eq!(*mock.closed.lock().unwrap(), vec![1]);
    }

    #[test]
    fn unknown_timestamp_never_closes_on_last_seen() {
        use rustket_core::workspace::Change;
        let mock = MockNotifier::default();
        let mut n = Notifications::with_backend(Arc::new(mock.clone()));
        let fx = Fixture::new();
        let mut ws = workspace();
        let sid = ServerId("s1".into());
        ws.apply_subscription(Change::Inserted, sub_seen("r1", "2026-01-01T09:00:00.000Z"));
        let (t, _) = n.notify(
            &sid,
            "Dev",
            &notification("r1", "m1", None),
            &ws,
            &UiPrefs::default(),
            &fx.env(false, None),
        );
        drain(&mut n, t);
        // The message is unknown: no last-seen closes it, however late.
        ws.apply_subscription(Change::Updated, sub_seen("r1", "2099-01-01T00:00:00.000Z"));
        let t = n.sync_read(&sid, &ws);
        drain(&mut n, t);
        assert_eq!(n.len(), 1, "an unknown timestamp keeps it");
        // The lastMessage update arrives later: now its timestamp counts.
        ws.apply_subscription(Change::Updated, sub_seen("r1", "2026-01-01T09:00:00.000Z"));
        ws.apply_room(
            Change::Inserted,
            serde_json::from_value(json!({
                "_id": "r1", "t": "d",
                "lastMessage": {"_id": "m1", "rid": "r1", "msg": "hello there",
                    "ts": "2026-01-01T10:00:00.000Z", "u": {"_id": "bobid", "username": "bob"}}
            }))
            .unwrap(),
        );
        let t = n.sync_read(&sid, &ws);
        drain(&mut n, t);
        assert_eq!(n.len(), 1, "last seen before the message");
        ws.apply_subscription(Change::Updated, sub_seen("r1", "2026-01-01T10:00:00.000Z"));
        let t = n.sync_read(&sid, &ws);
        drain(&mut n, t);
        assert!(n.is_empty());
        assert_eq!(*mock.closed.lock().unwrap(), vec![1]);
    }

    #[test]
    fn logged_out_servers_lose_their_notifications() {
        let mock = MockNotifier::default();
        let mut n = Notifications::with_backend(Arc::new(mock.clone()));
        n.shown.insert(
            Target {
                server_id: ServerId("gone".into()),
                rid: "r".into(),
            },
            Entry {
                id: Some(4),
                seq: 1,
                tmid: None,
                seen_unread: false,
                mid: String::new(),
                about: None,
                conference_id: None,
                on_screen: false,
            },
        );
        let keep = ServerId("s1".into());
        let t = n.retain_servers(&[&keep]);
        drain(&mut n, t);
        assert!(n.is_empty());
        assert_eq!(*mock.closed.lock().unwrap(), vec![4]);
    }
}
