//! Freedesktop notifications (notify-rust over zbus) honouring server and
//! local preferences (`rustket_core::notify::evaluate`).
//!
//! One notification per room: a new message replaces the room's previous
//! notification (`replaces_id`) instead of stacking up, and the notification
//! is closed once the room was read. Activating it raises the window and
//! opens the room (or thread). The backend is a [`Notifier`] so tests use a
//! mock; nothing here blocks the UI (every D-Bus call has a timeout).

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures::future::BoxFuture;
use futures::stream::BoxStream;
use futures::{FutureExt, StreamExt};
use iced::Task;
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

/// How long an action on a shown notification is waited for.
pub const ACTION_WAIT: Duration = Duration::from_secs(6 * 3600);
const SHOW_TIMEOUT: Duration = Duration::from_secs(5);

/// A notification to show.
#[derive(Debug, Clone, PartialEq)]
pub struct Outgoing {
    /// Id of the notification this one replaces.
    pub replaces: Option<u32>,
    pub title: String,
    pub body: String,
    /// RGBA pixels of the sender's avatar: `(width, height, pixels)`.
    pub image: Option<(u32, u32, Vec<u8>)>,
    /// Ask for the message sound.
    pub sound: bool,
    /// Label of the default action ("Open").
    pub action_label: String,
}

/// What happened to a notification.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// Shown with this id; `server_sound`: the notification server plays the
    /// sound hint itself.
    Shown {
        id: u32,
        server_sound: bool,
    },
    Activated,
    Closed,
    Failed(String),
}

/// The notification backend.
pub trait Notifier: Send + Sync + 'static {
    /// Shows `n`; the stream yields `Shown` (or `Failed`), then `Activated`
    /// or `Closed`.
    fn show(&self, n: Outgoing) -> BoxStream<'static, Event>;
    fn close(&self, id: u32) -> BoxFuture<'static, ()>;
}

/// Shows nothing (tests, snapshots).
#[derive(Debug, Default)]
pub struct NullNotifier;

impl Notifier for NullNotifier {
    fn show(&self, _: Outgoing) -> BoxStream<'static, Event> {
        futures::stream::empty().boxed()
    }

    fn close(&self, _: u32) -> BoxFuture<'static, ()> {
        async {}.boxed()
    }
}

/// Records what would be shown (tests).
#[derive(Debug, Default, Clone)]
pub struct MockNotifier {
    pub shown: Arc<Mutex<Vec<Outgoing>>>,
    pub closed: Arc<Mutex<Vec<u32>>>,
}

impl Notifier for MockNotifier {
    fn show(&self, n: Outgoing) -> BoxStream<'static, Event> {
        let id = match self.shown.lock() {
            Ok(mut g) => {
                g.push(n);
                g.len() as u32
            }
            Err(_) => 0,
        };
        futures::stream::iter([Event::Shown {
            id,
            server_sound: false,
        }])
        .boxed()
    }

    fn close(&self, id: u32) -> BoxFuture<'static, ()> {
        if let Ok(mut g) = self.closed.lock() {
            g.push(id);
        }
        async {}.boxed()
    }
}

/// The real backend: `org.freedesktop.Notifications` through notify-rust.
#[derive(Debug, Default)]
pub struct ZbusNotifier;

const NOTIFICATIONS: &str = "org.freedesktop.Notifications";
const NOTIFICATIONS_PATH: &str = "/org/freedesktop/Notifications";

static CAPABILITIES: tokio::sync::OnceCell<Vec<String>> = tokio::sync::OnceCell::const_new();

async fn capabilities() -> &'static [String] {
    CAPABILITIES
        .get_or_init(|| async {
            match super::dbus::call(
                NOTIFICATIONS,
                NOTIFICATIONS_PATH,
                NOTIFICATIONS,
                "GetCapabilities",
                &(),
            )
            .await
            {
                Some(m) => m.body().deserialize::<Vec<String>>().unwrap_or_default(),
                None => Vec::new(),
            }
        })
        .await
}

fn build(n: &Outgoing, server_sound: bool) -> notify_rust::Notification {
    use notify_rust::{Hint, Notification, Timeout};
    let mut x = Notification::new();
    x.appname(&t!("desktop.app_name"))
        .summary(&n.title)
        .body(&n.body)
        .hint(Hint::Category("im.received".into()))
        .action("default", &n.action_label)
        .timeout(Timeout::Default);
    if n.sound && server_sound {
        x.sound_name(super::sound::MESSAGE_SOUND);
    } else {
        x.hint(Hint::SuppressSound(true));
    }
    if let Some(id) = n.replaces {
        x.id(id);
    }
    if let Some((w, h, px)) = &n.image
        && let (Ok(w), Ok(h)) = (i32::try_from(*w), i32::try_from(*h))
        && let Ok(img) = notify_rust::Image::from_rgba(w, h, px.clone())
    {
        x.image_data(img);
    }
    x
}

impl Notifier for ZbusNotifier {
    fn show(&self, n: Outgoing) -> BoxStream<'static, Event> {
        iced::stream::channel(
            4,
            async move |mut out: futures::channel::mpsc::Sender<Event>| {
                use futures::SinkExt;
                let server_sound = capabilities().await.iter().any(|c| c == "sound");
                let notification = build(&n, server_sound);
                let handle =
                    match tokio::time::timeout(SHOW_TIMEOUT, notification.show_async()).await {
                        Ok(Ok(h)) => h,
                        Ok(Err(e)) => {
                            let _ = out.send(Event::Failed(e.to_string())).await;
                            return;
                        }
                        Err(_) => {
                            let _ = out.send(Event::Failed("timed out".into())).await;
                            return;
                        }
                    };
                let _ = out
                    .send(Event::Shown {
                        id: handle.id(),
                        server_sound,
                    })
                    .await;
                let mut result = Event::Closed;
                let wait = handle.wait_for_action_async(|r| {
                    use notify_rust::NotificationResponse as R;
                    result = match r {
                        R::Default => Event::Activated,
                        R::Action(a) if a == "default" => Event::Activated,
                        _ => Event::Closed,
                    };
                });
                let _ = tokio::time::timeout(ACTION_WAIT, wait).await;
                let _ = out.send(result).await;
            },
        )
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
}

/// Where a notification leads.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Target {
    pub server_id: ServerId,
    pub rid: String,
}

#[derive(Debug, Clone)]
pub enum Message {
    /// `target`: `None` for the test notification; `seq` identifies the
    /// showing (a replaced notification's late events are ignored).
    Event {
        target: Option<Target>,
        tmid: Option<String>,
        seq: u64,
        sound: bool,
        event: Event,
    },
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
    /// The room was seen unread after the notification (so "read" means read).
    seen_unread: bool,
}

pub struct Notifications {
    backend: Arc<dyn Notifier>,
    shown: HashMap<Target, Entry>,
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
    pub fn with_backend(backend: Arc<dyn Notifier>) -> Self {
        Self {
            backend,
            shown: HashMap::new(),
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
        let target = Target {
            server_id: server_id.clone(),
            rid: spec.rid.clone(),
        };
        self.seq += 1;
        let seq = self.seq;
        let entry = self.shown.entry(target.clone()).or_insert(Entry {
            id: None,
            seq,
            seen_unread: false,
        });
        entry.seq = seq;
        entry.seen_unread = false;
        let out = Outgoing {
            replaces: entry.id,
            title,
            body,
            image,
            sound: spec.sound,
            action_label: t!("desktop.open").into_owned(),
        };
        self.run(Some(target), spec.tmid, seq, out)
    }

    /// "Send test notification" from the settings.
    pub fn test(&mut self, prefs: &UiPrefs) -> Task<super::Message> {
        self.seq += 1;
        let out = Outgoing {
            replaces: None,
            title: t!("desktop.test_title").into_owned(),
            body: t!("desktop.test_body").into_owned(),
            image: None,
            sound: prefs.sound,
            action_label: t!("desktop.open").into_owned(),
        };
        self.run(None, None, self.seq, out)
    }

    fn run(
        &self,
        target: Option<Target>,
        tmid: Option<String>,
        seq: u64,
        out: Outgoing,
    ) -> Task<super::Message> {
        let sound = out.sound;
        Task::run(self.backend.show(out), move |event| {
            super::Message::Notifications(Message::Event {
                target: target.clone(),
                tmid: tmid.clone(),
                seq,
                sound,
                event,
            })
        })
    }

    /// Whether the event is the current showing of its target.
    fn current(&self, target: &Option<Target>, seq: u64) -> bool {
        match target {
            Some(t) => self.shown.get(t).is_some_and(|e| e.seq == seq),
            None => true,
        }
    }

    /// Returns `(task, requests, play_sound)`.
    pub fn update(&mut self, msg: Message) -> (Task<super::Message>, Vec<Request>, bool) {
        match msg {
            Message::Event {
                target,
                tmid,
                seq,
                sound,
                event,
            } => {
                if !self.current(&target, seq) {
                    return (Task::none(), Vec::new(), false);
                }
                match event {
                    Event::Shown { id, server_sound } => {
                        if let Some(e) = target.as_ref().and_then(|t| self.shown.get_mut(t)) {
                            e.id = Some(id);
                        }
                        (Task::none(), Vec::new(), sound && !server_sound)
                    }
                    Event::Activated => {
                        let mut req = vec![Request::ShowWindow];
                        if let Some(t) = target {
                            self.shown.remove(&t);
                            req.push(match tmid {
                                Some(tmid) => Request::OpenThread {
                                    server_id: t.server_id,
                                    rid: t.rid,
                                    tmid,
                                },
                                None => Request::OpenRoom {
                                    server_id: t.server_id,
                                    rid: t.rid,
                                },
                            });
                        }
                        (Task::none(), req, false)
                    }
                    Event::Closed => {
                        if let Some(t) = target {
                            self.shown.remove(&t);
                        }
                        (Task::none(), Vec::new(), false)
                    }
                    Event::Failed(e) => {
                        tracing::debug!(error = %e, "notification not shown");
                        if let Some(t) = target {
                            self.shown.remove(&t);
                        }
                        (Task::none(), Vec::new(), false)
                    }
                }
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
                seen_unread: false,
            },
        );
        // A stale showing is ignored.
        let (_, req, _) = n.update(Message::Event {
            target: Some(target.clone()),
            tmid: None,
            seq: 2,
            sound: false,
            event: Event::Activated,
        });
        assert!(req.is_empty());
        let (_, req, _) = n.update(Message::Event {
            target: Some(target.clone()),
            tmid: Some("t1".into()),
            seq: 3,
            sound: false,
            event: Event::Activated,
        });
        assert!(matches!(req[0], Request::ShowWindow));
        assert!(
            matches!(&req[1], Request::OpenThread { rid, tmid, .. } if rid == "r1" && tmid == "t1")
        );
        assert!(n.is_empty());
        // The test notification only raises the window.
        let (_, req, _) = n.update(Message::Event {
            target: None,
            tmid: None,
            seq: 9,
            sound: false,
            event: Event::Activated,
        });
        assert!(matches!(req.as_slice(), [Request::ShowWindow]));
    }

    #[test]
    fn local_sound_only_when_the_server_does_not_play_it() {
        let mut n = Notifications::default();
        let ev = |server_sound, sound| Message::Event {
            target: None,
            tmid: None,
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
                seen_unread: false,
            },
        );
        let keep = ServerId("s1".into());
        let t = n.retain_servers(&[&keep]);
        drain(&mut n, t);
        assert!(n.is_empty());
        assert_eq!(*mock.closed.lock().unwrap(), vec![4]);
    }
}
