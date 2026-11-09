//! Freedesktop idle time sources: GNOME's `org.gnome.Mutter.IdleMonitor`,
//! the X11 screensaver extension (pure-Rust `x11rb`), then
//! `org.freedesktop.ScreenSaver` `GetSessionIdleTime` (KDE Wayland).

use std::sync::atomic::{AtomicU8, Ordering};
use std::time::Duration;

use super::QUERY_TIMEOUT;

const SOURCE_UNKNOWN: u8 = 0;
const SOURCE_MUTTER: u8 = 1;
const SOURCE_X11: u8 = 2;
const SOURCE_SCREENSAVER: u8 = 3;

/// The source that answered last (tried first next time).
static SOURCE: AtomicU8 = AtomicU8::new(SOURCE_UNKNOWN);

async fn call_u64(dest: &str, path: &str, iface: &str, method: &str) -> Option<u64> {
    let reply = super::super::dbus::call(dest, path, iface, method, &()).await?;
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
