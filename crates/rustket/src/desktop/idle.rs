//! Idle detection → "away".
//!
//! The session idle time is polled every 30 s: GNOME's
//! `org.gnome.Mutter.IdleMonitor`, the X11 screensaver extension on X11
//! sessions (pure-Rust `x11rb`), then `org.freedesktop.ScreenSaver`
//! `GetSessionIdleTime` (KDE Wayland). When the user is idle longer than their
//! `idleTimeLimit` preference (default 300 s) and `enableAutoAway` is on, the
//! app calls `UserPresence:away` once on every connected server whose status
//! is `online`, and `UserPresence:online` once the user is back. These change
//! only the connection status (as the official clients do): a status the user
//! chose (busy, away, invisible) is never overridden and comes back on its own.
//! The server's status aggregates all of the user's sessions (another client
//! keeps it online), so "away" is re-sent only after a reconnect, which
//! resets this connection to online.

use std::collections::HashSet;
use std::sync::atomic::{AtomicU8, Ordering};
use std::time::Duration;

use iced::{Subscription, Task};
use rocketchat::models::UserStatus;
use rustket_core::{ServerId, Workspace};

use super::Request;

pub const POLL_INTERVAL: Duration = Duration::from_secs(30);
pub const DEFAULT_LIMIT: Duration = Duration::from_secs(300);
/// Shortest accepted `idleTimeLimit`.
const MIN_LIMIT: Duration = Duration::from_secs(30);
const QUERY_TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Debug, Clone)]
pub enum Message {
    Poll,
    /// Session idle time (`None`: no source works).
    Sampled(Option<Duration>),
}

/// The user's auto-away preferences on one server.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AwayPrefs {
    pub enabled: bool,
    pub limit: Duration,
}

impl Default for AwayPrefs {
    fn default() -> Self {
        Self {
            enabled: true,
            limit: DEFAULT_LIMIT,
        }
    }
}

impl AwayPrefs {
    /// From `me.settings.preferences` (`enableAutoAway`, `idleTimeLimit` in s).
    pub fn of(ws: &Workspace) -> Self {
        let mut p = Self::default();
        if let Some(b) = super::user_pref(ws, "enableAutoAway").and_then(|v| v.as_bool()) {
            p.enabled = b;
        }
        if let Some(secs) = super::user_pref(ws, "idleTimeLimit")
            .and_then(|v| v.as_f64().or_else(|| v.as_str()?.parse().ok()))
            .filter(|s| s.is_finite() && *s > 0.0)
        {
            p.limit = Duration::from_secs_f64(secs.min(7.0 * 86_400.0)).max(MIN_LIMIT);
        }
        p
    }
}

/// What to tell a server.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PresenceCall {
    Away,
    Online,
}

impl PresenceCall {
    pub fn method(self) -> &'static str {
        match self {
            Self::Away => "UserPresence:away",
            Self::Online => "UserPresence:online",
        }
    }
}

#[derive(Debug, Default)]
pub struct Idle {
    enabled: bool,
    /// Servers whose connection we marked away.
    away: HashSet<ServerId>,
}

impl Idle {
    pub fn system() -> Self {
        Self {
            enabled: true,
            away: HashSet::new(),
        }
    }

    /// The state machine: `idle` is the session idle time, `status` the
    /// user's current status on `sid`.
    pub fn decide(
        &mut self,
        sid: &ServerId,
        idle: Duration,
        prefs: AwayPrefs,
        status: UserStatus,
    ) -> Option<PresenceCall> {
        if prefs.enabled && idle >= prefs.limit {
            // Once per idle period (until a reconnect) and never over a
            // status the user chose.
            if status == UserStatus::Online && !self.away.contains(sid) {
                self.away.insert(sid.clone());
                return Some(PresenceCall::Away);
            }
            None
        } else if self.away.remove(sid) {
            Some(PresenceCall::Online)
        } else {
            None
        }
    }

    /// The server's connection was re-established (back to online): mark
    /// it away again on the next idle sample.
    pub fn reconnected(&mut self, sid: &ServerId) {
        self.away.remove(sid);
    }

    /// Forgets servers that are gone (logout, removal).
    pub fn retain(&mut self, mut keep: impl FnMut(&ServerId) -> bool) {
        self.away.retain(|s| keep(s));
    }

    pub fn update(&mut self, msg: Message) -> (Task<super::Message>, Vec<Request>) {
        match msg {
            Message::Poll if self.enabled => (
                Task::perform(query_idle(), |d| super::Message::Idle(Message::Sampled(d))),
                Vec::new(),
            ),
            Message::Poll => (Task::none(), Vec::new()),
            Message::Sampled(Some(d)) => (Task::none(), vec![Request::IdleTime(d)]),
            Message::Sampled(None) => (Task::none(), Vec::new()),
        }
    }

    pub fn subscription(&self) -> Subscription<Message> {
        if self.enabled {
            iced::time::every(POLL_INTERVAL).map(|_| Message::Poll)
        } else {
            Subscription::none()
        }
    }
}

// --- idle time sources ------------------------------------------------------

const SOURCE_UNKNOWN: u8 = 0;
const SOURCE_MUTTER: u8 = 1;
const SOURCE_X11: u8 = 2;
const SOURCE_SCREENSAVER: u8 = 3;

/// The source that answered last (tried first next time).
static SOURCE: AtomicU8 = AtomicU8::new(SOURCE_UNKNOWN);
async fn call_u64(dest: &str, path: &str, iface: &str, method: &str) -> Option<u64> {
    let reply = super::dbus::call(dest, path, iface, method, &()).await?;
    let body = reply.body();
    body.deserialize::<u64>()
        .ok()
        .or_else(|| body.deserialize::<u32>().ok().map(u64::from))
}

async fn mutter() -> Option<Duration> {
    call_u64(
        "org.gnome.Mutter.IdleMonitor",
        "/org/gnome/Mutter/IdleMonitor/Core",
        "org.gnome.Mutter.IdleMonitor",
        "GetIdletime",
    )
    .await
    .map(Duration::from_millis)
}

/// `GetSessionIdleTime`: seconds per the freedesktop interface, but KDE's
/// implementation (kscreenlocker) answers milliseconds.
async fn screensaver() -> Option<Duration> {
    let v = call_u64(
        "org.freedesktop.ScreenSaver",
        "/org/freedesktop/ScreenSaver",
        "org.freedesktop.ScreenSaver",
        "GetSessionIdleTime",
    )
    .await?;
    let kde = std::env::var("XDG_CURRENT_DESKTOP").is_ok_and(|d| d.contains("KDE"));
    Some(if kde {
        Duration::from_millis(v)
    } else {
        Duration::from_secs(v)
    })
}

fn x11_session() -> bool {
    std::env::var_os("DISPLAY").is_some_and(|d| !d.is_empty())
        && std::env::var("XDG_SESSION_TYPE").map_or(true, |t| t != "wayland")
}

fn x11_blocking() -> Option<Duration> {
    use x11rb::connection::Connection as _;
    use x11rb::protocol::screensaver::ConnectionExt as _;
    let (conn, screen) = x11rb::connect(None).ok()?;
    let root = conn.setup().roots.get(screen)?.root;
    let reply = conn.screensaver_query_info(root).ok()?.reply().ok()?;
    Some(Duration::from_millis(u64::from(reply.ms_since_user_input)))
}

async fn x11() -> Option<Duration> {
    if !x11_session() {
        return None;
    }
    tokio::time::timeout(QUERY_TIMEOUT, tokio::task::spawn_blocking(x11_blocking))
        .await
        .ok()?
        .ok()?
}

async fn query(source: u8) -> Option<Duration> {
    match source {
        SOURCE_MUTTER => mutter().await,
        SOURCE_X11 => x11().await,
        SOURCE_SCREENSAVER => screensaver().await,
        _ => None,
    }
}

/// The session idle time from the first source that answers.
pub async fn query_idle() -> Option<Duration> {
    let last = SOURCE.load(Ordering::Relaxed);
    if last != SOURCE_UNKNOWN
        && let Some(d) = query(last).await
    {
        return Some(d);
    }
    for source in [SOURCE_MUTTER, SOURCE_X11, SOURCE_SCREENSAVER] {
        if source == last {
            continue;
        }
        if let Some(d) = query(source).await {
            SOURCE.store(source, Ordering::Relaxed);
            return Some(d);
        }
    }
    SOURCE.store(SOURCE_UNKNOWN, Ordering::Relaxed);
    None
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use serde_json::json;

    const MIN: Duration = Duration::from_secs(60);

    fn sid(s: &str) -> ServerId {
        ServerId(s.into())
    }

    #[test]
    fn away_after_the_limit_and_back_when_active() {
        let mut idle = Idle::default();
        let p = AwayPrefs::default();
        let a = sid("a");
        assert_eq!(idle.decide(&a, MIN, p, UserStatus::Online), None);
        assert_eq!(
            idle.decide(&a, 5 * MIN, p, UserStatus::Online),
            Some(PresenceCall::Away)
        );
        // The server reports away now: nothing more to do while idle.
        assert_eq!(idle.decide(&a, 6 * MIN, p, UserStatus::Away), None);
        // Another client keeps the aggregate online: not re-sent.
        assert_eq!(idle.decide(&a, 6 * MIN, p, UserStatus::Online), None);
        // A reconnect reset the connection to online while still idle.
        idle.reconnected(&a);
        assert_eq!(
            idle.decide(&a, 7 * MIN, p, UserStatus::Online),
            Some(PresenceCall::Away)
        );
        // Back.
        assert_eq!(
            idle.decide(&a, Duration::from_secs(3), p, UserStatus::Away),
            Some(PresenceCall::Online)
        );
        assert_eq!(
            idle.decide(&a, Duration::from_secs(3), p, UserStatus::Online),
            None
        );
    }

    #[test]
    fn manual_status_is_never_touched() {
        let mut idle = Idle::default();
        let p = AwayPrefs::default();
        let a = sid("a");
        for status in [UserStatus::Busy, UserStatus::Away, UserStatus::Offline] {
            assert_eq!(idle.decide(&a, 10 * MIN, p, status), None, "{status:?}");
            assert_eq!(idle.decide(&a, Duration::ZERO, p, status), None);
        }
    }

    #[test]
    fn disabled_auto_away_restores_and_stays_quiet() {
        let mut idle = Idle::default();
        let a = sid("a");
        assert_eq!(
            idle.decide(&a, 10 * MIN, AwayPrefs::default(), UserStatus::Online),
            Some(PresenceCall::Away)
        );
        let off = AwayPrefs {
            enabled: false,
            limit: DEFAULT_LIMIT,
        };
        assert_eq!(
            idle.decide(&a, 10 * MIN, off, UserStatus::Away),
            Some(PresenceCall::Online)
        );
        assert_eq!(idle.decide(&a, 10 * MIN, off, UserStatus::Online), None);
    }

    #[test]
    fn servers_are_independent() {
        let mut idle = Idle::default();
        let (a, b) = (sid("a"), sid("b"));
        let short = AwayPrefs {
            enabled: true,
            limit: MIN,
        };
        assert_eq!(
            idle.decide(&a, 2 * MIN, short, UserStatus::Online),
            Some(PresenceCall::Away)
        );
        assert_eq!(
            idle.decide(&b, 2 * MIN, AwayPrefs::default(), UserStatus::Online),
            None
        );
        idle.retain(|s| s != &a);
        assert_eq!(
            idle.decide(&a, Duration::ZERO, short, UserStatus::Away),
            None
        );
    }

    #[test]
    fn prefs_from_the_user_document() {
        let mut ws = Workspace::new();
        assert_eq!(AwayPrefs::of(&ws), AwayPrefs::default());
        let me: rocketchat::models::User = serde_json::from_value(json!({
            "_id": "u1", "username": "me",
            "settings": {"preferences": {"enableAutoAway": false, "idleTimeLimit": 600}}
        }))
        .unwrap();
        ws.apply_me(me);
        assert_eq!(
            AwayPrefs::of(&ws),
            AwayPrefs {
                enabled: false,
                limit: Duration::from_secs(600)
            }
        );
        let me: rocketchat::models::User = serde_json::from_value(json!({
            "_id": "u1", "username": "me",
            "settings": {"preferences": {"idleTimeLimit": 1}}
        }))
        .unwrap();
        ws.apply_me(me);
        assert_eq!(AwayPrefs::of(&ws).limit, MIN_LIMIT);
    }

    #[test]
    fn disabled_idle_does_not_poll() {
        let mut idle = Idle::default();
        let (_, req) = idle.update(Message::Poll);
        assert!(req.is_empty());
        let (_, req) = idle.update(Message::Sampled(Some(MIN)));
        assert!(matches!(req.as_slice(), [Request::IdleTime(d)] if *d == MIN));
    }
}
