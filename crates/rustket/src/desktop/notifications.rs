//! Freedesktop notifications (`org.freedesktop.Notifications` over the
//! shared zbus connection) honouring server and local preferences
//! (`rustket_core::notify::evaluate`).
//!
//! One notification per room: a new message replaces the room's previous
//! notification (`replaces_id`) instead of stacking up, and the notification
//! is closed once the room was read. Activating it raises the window and
//! opens the room (or thread). `ActionInvoked` / `NotificationClosed` come
//! from one app-wide signal subscription and are routed by id. The backend is
//! a [`Notifier`] so tests use a mock; every D-Bus call has a timeout.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use futures::future::BoxFuture;
use futures::{FutureExt, StreamExt};
use iced::{Subscription, Task};
use rocketchat::ServerUrl;
use rocketchat::session::DesktopNotification;
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
    /// Ask for the message sound.
    pub sound: bool,
    /// `(key, label)`; the first is the default action.
    pub actions: Vec<(String, String)>,
    /// Milliseconds, or [`TIMEOUT_DEFAULT`] / [`TIMEOUT_NEVER`].
    pub timeout: i32,
    pub urgency: u8,
}

/// Result of showing a notification.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// Shown with this id; `server_sound`: the notification server plays the
    /// sound hint itself.
    Shown {
        id: u32,
        server_sound: bool,
    },
    Failed(String),
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
}

impl Notifier for MockNotifier {
    fn show(&self, n: Outgoing) -> BoxFuture<'static, Event> {
        let id = match self.shown.lock() {
            Ok(mut g) => {
                let id = n.replaces.unwrap_or(g.len() as u32 + 1);
                g.push(n);
                id
            }
            Err(_) => 0,
        };
        async move {
            Event::Shown {
                id,
                server_sound: false,
            }
        }
        .boxed()
    }

    fn close(&self, id: u32) -> BoxFuture<'static, ()> {
        if let Ok(mut g) = self.closed.lock() {
            g.push(id);
        }
        async {}.boxed()
    }
}

/// The real backend: `org.freedesktop.Notifications` on the shared session
/// bus connection.
#[derive(Debug, Default)]
pub struct ZbusNotifier;

const NOTIFICATIONS: &str = "org.freedesktop.Notifications";
const NOTIFICATIONS_PATH: &str = "/org/freedesktop/Notifications";

static CAPABILITIES: tokio::sync::OnceCell<Vec<String>> = tokio::sync::OnceCell::const_new();

/// The server's capabilities (cached once a server answered).
async fn capabilities() -> Vec<String> {
    if let Some(c) = CAPABILITIES.get() {
        return c.clone();
    }
    let caps = match super::dbus::call(
        NOTIFICATIONS,
        NOTIFICATIONS_PATH,
        NOTIFICATIONS,
        "GetCapabilities",
        &(),
    )
    .await
    {
        Some(m) => m.body().deserialize::<Vec<String>>().unwrap_or_default(),
        None => return Vec::new(),
    };
    let _ = CAPABILITIES.set(caps.clone());
    caps
}

/// Escapes text for servers that parse the body as markup.
pub fn escape_markup(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            c => out.push(c),
        }
    }
    out
}

type Hints = HashMap<&'static str, zbus::zvariant::Value<'static>>;

/// Hints of `n` (`server_sound`: the server plays sound hints).
fn hints(n: &Outgoing, server_sound: bool) -> Hints {
    use zbus::zvariant::{Structure, Value};
    let mut h = Hints::new();
    h.insert("category", Value::from("im.received"));
    h.insert("urgency", Value::U8(n.urgency));
    if n.sound && server_sound {
        h.insert("sound-name", Value::from(super::sound::MESSAGE_SOUND));
    } else {
        h.insert("suppress-sound", Value::Bool(true));
    }
    if let Some((w, height, px)) = &n.image
        && let (Ok(w), Ok(height)) = (i32::try_from(*w), i32::try_from(*height))
        && i64::from(w) * i64::from(height) * 4 == px.len() as i64
    {
        let s = Structure::from((w, height, w * 4, true, 8i32, 4i32, px.clone()));
        h.insert("image-data", Value::Structure(s));
    }
    h
}

type NotifyArgs<'a> = (
    &'a str,
    u32,
    &'a str,
    &'a str,
    String,
    Vec<&'a str>,
    Hints,
    i32,
);

/// Arguments of `Notify` (`caps`: the server's capabilities).
fn notify_args<'a>(n: &'a Outgoing, app_name: &'a str, caps: &[String]) -> NotifyArgs<'a> {
    let server_sound = caps.iter().any(|c| c == "sound");
    let body = if caps.iter().any(|c| c == "body-markup") {
        escape_markup(&n.body)
    } else {
        n.body.clone()
    };
    let actions = n
        .actions
        .iter()
        .flat_map(|(k, l)| [k.as_str(), l.as_str()])
        .collect();
    (
        app_name,
        n.replaces.unwrap_or(0),
        "",
        n.title.as_str(),
        body,
        actions,
        hints(n, server_sound),
        n.timeout,
    )
}

impl Notifier for ZbusNotifier {
    fn show(&self, n: Outgoing) -> BoxFuture<'static, Event> {
        async move {
            let caps = capabilities().await;
            let server_sound = caps.iter().any(|c| c == "sound");
            let app_name = t!("desktop.app_name");
            let args = notify_args(&n, &app_name, &caps);
            match super::dbus::call(
                NOTIFICATIONS,
                NOTIFICATIONS_PATH,
                NOTIFICATIONS,
                "Notify",
                &args,
            )
            .await
            {
                Some(reply) => match reply.body().deserialize::<u32>() {
                    Ok(id) => Event::Shown { id, server_sound },
                    Err(e) => Event::Failed(e.to_string()),
                },
                None => Event::Failed("Notify failed".into()),
            }
        }
        .boxed()
    }

    fn close(&self, id: u32) -> BoxFuture<'static, ()> {
        async move {
            let _ = super::dbus::call(
                NOTIFICATIONS,
                NOTIFICATIONS_PATH,
                NOTIFICATIONS,
                "CloseNotification",
                &(id,),
            )
            .await;
        }
        .boxed()
    }

    fn listens(&self) -> bool {
        true
    }
}

/// `ActionInvoked` / `NotificationClosed` of every notification (one match
/// rule on the shared connection).
fn signals() -> impl futures::Stream<Item = Message> {
    iced::stream::channel(
        32,
        async |mut out: futures::channel::mpsc::Sender<Message>| {
            use futures::SinkExt;
            let Some(bus) = super::dbus::session_bus().await else {
                return;
            };
            let rule = zbus::MatchRule::builder()
                .msg_type(zbus::message::Type::Signal)
                .sender(NOTIFICATIONS)
                .and_then(|b| b.interface(NOTIFICATIONS))
                .and_then(|b| b.path(NOTIFICATIONS_PATH))
                .map(|b| b.build());
            let rule = match rule {
                Ok(r) => r,
                Err(e) => {
                    tracing::debug!(error = %e, "notification match rule");
                    return;
                }
            };
            let mut stream = match zbus::MessageStream::for_match_rule(rule, &bus, Some(64)).await {
                Ok(s) => s,
                Err(e) => {
                    tracing::debug!(error = %e, "cannot watch notification signals");
                    return;
                }
            };
            while let Some(m) = stream.next().await {
                let Ok(m) = m else { continue };
                let header = m.header();
                let msg = match header.member().map(|m| m.as_str()) {
                    Some("ActionInvoked") => {
                        m.body()
                            .deserialize::<(u32, String)>()
                            .ok()
                            .map(|(id, key)| Message::Signal {
                                id,
                                signal: Signal::Action(key),
                            })
                    }
                    Some("NotificationClosed") => {
                        m.body()
                            .deserialize::<(u32, u32)>()
                            .ok()
                            .map(|(id, _)| Message::Signal {
                                id,
                                signal: Signal::Closed,
                            })
                    }
                    _ => None,
                };
                if let Some(msg) = msg
                    && out.send(msg).await.is_err()
                {
                    break;
                }
            }
        },
    )
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
    /// ignored); `sound`: the message sound was asked for.
    Shown {
        target: Option<Target>,
        seq: u64,
        sound: bool,
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
}

#[derive(Debug)]
struct Entry {
    id: Option<u32>,
    seq: u64,
    /// Thread of the latest message (opened on activation).
    tmid: Option<String>,
    /// The room was seen unread after the notification (so "read" means read).
    seen_unread: bool,
    /// Call of a conference notification (its "Join" joins the call).
    conference_id: Option<String>,
}

pub struct Notifications {
    backend: Arc<dyn Notifier>,
    shown: HashMap<Target, Entry>,
    /// Id of the shown test notification.
    test_id: Option<u32>,
    seq: u64,
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

/// Whether the message sound may play: the room's sound is not "None", the
/// account's new-message sound is not "None" and its volumes are not zero.
pub fn sound_allowed(n: &DesktopNotification, ws: &Workspace) -> bool {
    if n.payload.audio_notification_value.as_deref() == Some("none") {
        return false;
    }
    if super::user_pref(ws, "newMessageNotification").and_then(|v| v.as_str()) == Some("none") {
        return false;
    }
    !["notificationsSoundVolume", "masterVolume"]
        .iter()
        .any(|k| {
            super::user_pref(ws, k)
                .and_then(|v| v.as_f64())
                .is_some_and(|v| v <= 0.0)
        })
}

/// `expire_timeout` and urgency: rings (`requireInteraction`) stay and are
/// critical; the "require interaction" preference keeps notifications;
/// otherwise the server's duration (ms) or the default.
fn timeout_and_urgency(n: &DesktopNotification, ws: &Workspace) -> (i32, u8) {
    if n.require_interaction == Some(true) {
        return (TIMEOUT_NEVER, URGENCY_CRITICAL);
    }
    let pref = super::user_pref(ws, "desktopNotificationRequireInteraction")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    if pref {
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
        }
    }

    /// Shows `notification` unless a local rule suppresses it.
    #[allow(clippy::too_many_arguments)]
    pub fn notify(
        &mut self,
        server_id: &ServerId,
        server_name: &str,
        notification: &DesktopNotification,
        workspace: &Workspace,
        prefs: &UiPrefs,
        env: &NotifyEnv<'_>,
    ) -> Task<super::Message> {
        // Not about a room: nothing to open (the web client skips these too).
        if notification.payload.rid.is_empty() {
            return Task::none();
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
        };
        let Some(spec) = evaluate(&n, &ctx) else {
            return Task::none();
        };
        let mut title = spec.title.clone();
        if env.multi_server && !server_name.is_empty() {
            title = format!("{title} · {server_name}");
        }
        let body = match spec.body {
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
        let entry = self.shown.entry(target.clone()).or_insert(Entry {
            id: None,
            seq,
            tmid: None,
            seen_unread: false,
            conference_id: None,
        });
        entry.seq = seq;
        entry.tmid = spec.tmid.clone();
        entry.seen_unread = false;
        entry.conference_id = notification.payload.conference_id.clone();
        let out = Outgoing {
            replaces: entry.id,
            title,
            body,
            image,
            sound: spec.sound && sound_allowed(notification, workspace),
            actions,
            timeout,
            urgency,
        };
        self.run(Some(target), seq, out)
    }

    /// "Send test notification" from the settings.
    pub fn test(&mut self, prefs: &UiPrefs) -> Task<super::Message> {
        self.seq += 1;
        let out = Outgoing {
            replaces: self.test_id,
            title: t!("desktop.test_title").into_owned(),
            body: t!("desktop.test_body").into_owned(),
            image: None,
            sound: prefs.sound,
            actions: vec![(ACTION_DEFAULT.to_owned(), t!("desktop.open").into_owned())],
            timeout: TIMEOUT_DEFAULT,
            urgency: URGENCY_NORMAL,
        };
        self.run(None, self.seq, out)
    }

    fn run(&self, target: Option<Target>, seq: u64, out: Outgoing) -> Task<super::Message> {
        let sound = out.sound;
        Task::perform(self.backend.show(out), move |event| {
            super::Message::Notifications(Message::Shown {
                target: target.clone(),
                seq,
                sound,
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

    /// Returns `(task, requests, play_sound)`.
    pub fn update(&mut self, msg: Message) -> (Task<super::Message>, Vec<Request>, bool) {
        match msg {
            Message::Shown {
                target,
                seq,
                sound,
                event,
            } => match event {
                Event::Shown { id, server_sound } => {
                    let play = sound && !server_sound;
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
                        Some(e) if e.id != Some(id) => {
                            (self.close_ids(vec![id]), Vec::new(), false)
                        }
                        Some(_) => (Task::none(), Vec::new(), false),
                        None => (self.close_ids(vec![id]), Vec::new(), false),
                    }
                }
                Event::Failed(e) => {
                    tracing::debug!(error = %e, "notification not shown");
                    if let Some(t) = target
                        && self.shown.get(&t).is_some_and(|e| e.seq == seq)
                    {
                        self.shown.remove(&t);
                    }
                    (Task::none(), Vec::new(), false)
                }
            },
            Message::Signal { id, signal } => {
                let target = self
                    .shown
                    .iter()
                    .find(|(_, e)| e.id == Some(id))
                    .map(|(t, _)| t.clone());
                let activated =
                    matches!(&signal, Signal::Action(k) if k == ACTION_DEFAULT || k == ACTION_JOIN);
                if let Some(t) = target {
                    if !activated && signal != Signal::Closed {
                        return (Task::none(), Vec::new(), false);
                    }
                    let Some(entry) = self.shown.remove(&t) else {
                        return (Task::none(), Vec::new(), false);
                    };
                    if !activated {
                        return (Task::none(), Vec::new(), false);
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
                            false,
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
                    return (Task::none(), vec![Request::ShowWindow, open], false);
                }
                if self.test_id != Some(id) {
                    // Not ours (e.g. a call notification of `videoconf`).
                    return (
                        Task::none(),
                        vec![Request::ForeignSignal(id, signal)],
                        false,
                    );
                }
                if self.test_id == Some(id) {
                    if activated {
                        self.test_id = None;
                        return (Task::none(), vec![Request::ShowWindow], false);
                    }
                    if signal == Signal::Closed {
                        self.test_id = None;
                    }
                }
                (Task::none(), Vec::new(), false)
            }
        }
    }

    /// Closes the notifications of `sid`'s rooms that were read since.
    pub fn sync_read(&mut self, sid: &ServerId, ws: &Workspace) -> Task<super::Message> {
        let mut close = Vec::new();
        self.shown.retain(|t, e| {
            if &t.server_id != sid {
                return true;
            }
            if room_unread(ws, &t.rid) {
                e.seen_unread = true;
                return true;
            }
            if e.seen_unread {
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
        let t = n.notify(
            &sid,
            "Dev",
            &notification("r1", "m0", None),
            &ws,
            &prefs,
            &fx.env(true, Some("r1")),
        );
        drain(&mut n, t);
        assert!(mock.shown.lock().unwrap().is_empty());

        let t = n.notify(
            &sid,
            "Dev",
            &notification("r1", "m1", None),
            &ws,
            &prefs,
            &fx.env(false, Some("r1")),
        );
        drain(&mut n, t);
        let t = n.notify(
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
        assert!(shown[1].sound);
        assert_eq!(n.len(), 1);
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
        let t = n.notify(
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
        assert!(!shown[0].sound);

        ws.apply_user_status("me", Some("me"), UserStatus::Busy, None);
        let t = n.notify(
            &sid,
            "Dev",
            &notification("r2", "m2", None),
            &ws,
            &UiPrefs::default(),
            &fx.env(false, None),
        );
        drain(&mut n, t);
        assert_eq!(mock.shown.lock().unwrap().len(), 1, "busy: do not disturb");
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
                conference_id: None,
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
                conference_id: None,
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
                conference_id: None,
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
                conference_id: Some("c1".into()),
            },
        );
        let (_, req, _) = n.update(signal(10, Signal::Action("join".into())));
        assert!(matches!(&req[2], Request::JoinCall { call_id, .. } if call_id == "c1"));
    }

    #[test]
    fn local_sound_only_when_the_server_does_not_play_it() {
        let mut n = Notifications::default();
        let ev = |server_sound, sound| Message::Shown {
            target: None,
            seq: 1,
            sound,
            event: Event::Shown {
                id: 1,
                server_sound,
            },
        };
        assert!(n.update(ev(false, true)).2);
        assert!(!n.update(ev(true, true)).2);
        assert!(!n.update(ev(false, false)).2);
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
                conference_id: None,
            },
        );
        // An older showing finished late with a different id: closed.
        let (t, _, play) = n.update(Message::Shown {
            target: Some(target.clone()),
            seq: 1,
            sound: true,
            event: Event::Shown {
                id: 4,
                server_sound: false,
            },
        });
        assert!(!play);
        drain(&mut n, t);
        assert_eq!(*mock.closed.lock().unwrap(), vec![4]);
        assert_eq!(n.shown[&target].id, Some(3));
    }

    #[test]
    fn room_and_account_sound_none_mute_the_sound() {
        let mock = MockNotifier::default();
        let mut n = Notifications::with_backend(Arc::new(mock.clone()));
        let fx = Fixture::new();
        let mut ws = workspace();
        let sid = ServerId("s1".into());
        let mut quiet = notification("r1", "m1", None);
        quiet.payload.audio_notification_value = Some("none".into());
        assert!(!sound_allowed(&quiet, &ws));
        assert!(sound_allowed(&notification("r1", "m1", None), &ws));
        let t = n.notify(
            &sid,
            "Dev",
            &quiet,
            &ws,
            &UiPrefs::default(),
            &fx.env(false, None),
        );
        drain(&mut n, t);
        assert!(!mock.shown.lock().unwrap()[0].sound);
        ws.apply_me(
            serde_json::from_value(json!({
                "_id": "me", "username": "me", "status": "online",
                "settings": {"preferences": {"newMessageNotification": "none"}}
            }))
            .unwrap(),
        );
        assert!(!sound_allowed(&notification("r1", "m1", None), &ws));
        ws.apply_me(
            serde_json::from_value(json!({
                "_id": "me", "username": "me", "status": "online",
                "settings": {"preferences": {"notificationsSoundVolume": 0}}
            }))
            .unwrap(),
        );
        assert!(!sound_allowed(&notification("r1", "m1", None), &ws));
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
        let t = n.notify(&sid, "Dev", &x, &ws, &prefs, &fx.env(false, None));
        drain(&mut n, t);
        assert!(mock.shown.lock().unwrap().is_empty(), "no rid: skipped");

        x.payload.rid = "r1".into();
        x.duration = Some(10.0);
        let t = n.notify(&sid, "Dev", &x, &ws, &prefs, &fx.env(false, None));
        drain(&mut n, t);
        let mut ring = notification("r2", "m2", None);
        ring.require_interaction = Some(true);
        ring.payload.conference_id = Some("c1".into());
        let t = n.notify(&sid, "Dev", &ring, &ws, &prefs, &fx.env(false, None));
        drain(&mut n, t);
        ws.apply_me(
            serde_json::from_value(json!({
                "_id": "me", "username": "me", "status": "online",
                "settings": {"preferences": {"desktopNotificationRequireInteraction": true}}
            }))
            .unwrap(),
        );
        let t = n.notify(
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

    #[test]
    fn markup_is_escaped_and_hints_are_built() {
        assert_eq!(
            escape_markup(r#"x <b>bold</b> & <a href="u">y</a>"#),
            r#"x &lt;b&gt;bold&lt;/b&gt; &amp; &lt;a href="u"&gt;y&lt;/a&gt;"#
        );
        let out = Outgoing {
            replaces: None,
            title: "t".into(),
            body: "b".into(),
            image: Some((2, 1, vec![0; 8])),
            sound: true,
            actions: Vec::new(),
            timeout: TIMEOUT_DEFAULT,
            urgency: URGENCY_NORMAL,
        };
        let h = hints(&out, true);
        assert!(h.contains_key("sound-name") && h.contains_key("image-data"));
        let h = hints(&out, false);
        assert!(h.contains_key("suppress-sound") && !h.contains_key("sound-name"));
        let bad = Outgoing {
            image: Some((2, 2, vec![0; 8])),
            ..out
        };
        assert!(!hints(&bad, false).contains_key("image-data"));
    }

    #[test]
    fn notify_call_signature() {
        use zbus::zvariant::DynamicType;
        let out = Outgoing {
            replaces: Some(3),
            title: "t".into(),
            body: "a < b".into(),
            image: Some((1, 1, vec![0; 4])),
            sound: false,
            actions: vec![("default".into(), "Open".into())],
            timeout: TIMEOUT_DEFAULT,
            urgency: URGENCY_NORMAL,
        };
        let caps = vec!["body-markup".to_owned()];
        let args = notify_args(&out, "Rustket", &caps);
        assert_eq!(args.signature().to_string(), "(susssasa{sv}i)");
        assert_eq!(args.4, "a &lt; b");
        assert_eq!(args.5, ["default", "Open"]);
        assert_eq!(
            args.6["image-data"].value_signature().to_string(),
            "(iiibiiay)"
        );
        assert_eq!(notify_args(&out, "Rustket", &[]).4, "a < b");
    }

    #[test]
    fn closes_when_the_room_is_read() {
        let mock = MockNotifier::default();
        let mut n = Notifications::with_backend(Arc::new(mock.clone()));
        let fx = Fixture::new();
        let mut ws = workspace();
        let sid = ServerId("s1".into());
        let t = n.notify(
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
                conference_id: None,
            },
        );
        let keep = ServerId("s1".into());
        let t = n.retain_servers(&[&keep]);
        drain(&mut n, t);
        assert!(n.is_empty());
        assert_eq!(*mock.closed.lock().unwrap(), vec![4]);
    }
}
