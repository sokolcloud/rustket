//! Indicator-scheme comparison shots (`RUSTKET_SNAPSHOT_ONLY=schemes`), at
//! 1400x1000 to match the Rocket.Chat web shots. Signs in as the viewer
//! `schemeview` seeded by `dev/seed-indicators.sh` (nothing is created or
//! deleted) and renders the workspace under the Rustket scheme (light) and
//! the Rocket.Chat scheme (light, dark), then the rooms `<prefix>-mixed` and
//! `<prefix>-thread-and-all` under Rocket.Chat (light). The prefix is
//! `RUSTKET_SNAPSHOT_SCHEME_PREFIX` or the newest seeded one. `*-tall`
//! companions (1400x1500) show the rows below the fold. Opening the
//! rooms marks them read on the server: reseed afterwards.

use std::path::Path;
use std::time::Duration;

use iced::Size;
use rocketchat::ServerUrl;
use rustket::app::{App, Flags, Screen, SecretsMode};
use rustket::message::Message;
use rustket::screens::{add_server, login};
use rustket::workspace::{sidebar, status_menu};
use rustket_core::ServerId;
use rustket_core::config::{IndicatorScheme, Theme};

use super::{Driver, active_login};

const SIZE: Size = Size::new(1400.0, 1000.0);
/// Tall enough for every seeded row (`*-tall` companions of the sidebar shots).
const TALL: Size = Size::new(1400.0, 1500.0);
const USER: &str = "schemeview";
const PASSWORD: &str = "schemeviewpass";

async fn sign_in(server: &ServerUrl, cache: &Path, scheme: IndicatorScheme) -> (Driver, ServerId) {
    let mut d = Driver::new(Flags {
        ephemeral: true,
        cache_dir: Some(cache.to_owned()),
        secrets: SecretsMode::Memory,
        open_urls: false,
        indicator_scheme: Some(scheme),
        ..Flags::default()
    });
    d.pump_until("add-server screen", Duration::from_secs(10), |a| {
        matches!(a.screen, Screen::AddServer(_))
    })
    .await;
    d.send(Message::AddServer(add_server::Message::UrlChanged(
        server.as_str().to_owned(),
    )));
    d.send(Message::AddServer(add_server::Message::Submit));
    d.pump_until("login screen", Duration::from_secs(20), |a| {
        active_login(a).is_some_and(|l| l.meta.is_some())
    })
    .await;
    let sid = d.app.active.clone().expect("active server");
    let lm = |m| Message::Login(sid.clone(), m);
    d.send(lm(login::Message::User(USER.into())));
    d.send(lm(login::Message::Password(PASSWORD.into())));
    d.send(lm(login::Message::Submit));
    d.pump_until("workspace loaded", Duration::from_secs(40), |a| {
        a.servers.get(&sid).is_some_and(|s| {
            s.login.is_none() && s.workspace.last_sync().is_some() && s.sidebar.room_count() >= 15
        })
    })
    .await;
    d.settle(Duration::from_secs(3)).await;
    (d, sid)
}

/// Signs out through the app (the login token is revoked) and stops the
/// driver's subscriptions.
async fn sign_out(mut d: Driver, sid: &ServerId) {
    d.send(Message::Sidebar(
        sid.clone(),
        sidebar::Message::Status(status_menu::Message::Logout),
    ));
    d.pump_until("login after logout", Duration::from_secs(20), |a| {
        a.servers[sid]
            .login
            .as_ref()
            .is_some_and(|l| l.meta.is_some())
    })
    .await;
    for h in d.subs.values() {
        h.abort();
    }
}

fn prefix(app: &App, sid: &ServerId) -> String {
    if let Ok(p) = std::env::var("RUSTKET_SNAPSHOT_SCHEME_PREFIX") {
        return p;
    }
    app.servers[sid]
        .workspace
        .rooms()
        .values()
        .filter_map(|e| e.name().strip_suffix("-mixed"))
        .filter(|p| p.starts_with("ind"))
        .max()
        .expect("a seeded <prefix>-mixed room")
        .to_owned()
}

fn rid_named(app: &App, sid: &ServerId, name: &str) -> String {
    app.servers[sid]
        .workspace
        .rooms()
        .values()
        .find(|e| e.name() == name)
        .map(|e| e.rid().to_owned())
        .unwrap_or_else(|| panic!("room {name}"))
}

pub async fn scheme_shots(server: &ServerUrl, out: &Path) -> bool {
    if std::env::var("RUSTKET_SNAPSHOT_ONLY").as_deref() != Ok("schemes") {
        return false;
    }
    let cache = tempfile::tempdir().expect("cache dir");

    let (mut d, sid) = sign_in(server, cache.path(), IndicatorScheme::Rustket).await;
    d.app.config.ui.theme = Theme::Light;
    d.shoot_at(out, "rk-sidebar-rustket-light", SIZE);
    d.shoot_at(out, "rk-sidebar-rustket-light-tall", TALL);
    sign_out(d, &sid).await;

    let (mut d, sid) = sign_in(server, cache.path(), IndicatorScheme::RocketChat).await;
    d.app.config.ui.theme = Theme::Light;
    d.shoot_at(out, "rk-sidebar-rc-light", SIZE);
    d.shoot_at(out, "rk-sidebar-rc-light-tall", TALL);
    d.app.config.ui.theme = Theme::Dark;
    d.shoot_at(out, "rk-sidebar-rc-dark", SIZE);
    d.shoot_at(out, "rk-sidebar-rc-dark-tall", TALL);
    d.app.config.ui.theme = Theme::Light;

    let p = prefix(&d.app, &sid);
    println!("prefix {p}");
    for room in ["mixed", "thread-and-all"] {
        let rid = rid_named(&d.app, &sid, &format!("{p}-{room}"));
        d.send(Message::Sidebar(
            sid.clone(),
            sidebar::Message::Open(rid.clone()),
        ));
        d.pump_until("room open", Duration::from_secs(20), |a| {
            a.servers[&sid].rooms.get(&rid).is_some_and(|r| r.loaded)
        })
        .await;
        // Avatars and the thread metadata; the read mark may land meanwhile.
        d.settle(Duration::from_millis(1500)).await;
        d.shoot_at(out, &format!("rk-room-{room}-rc-light"), SIZE);
    }
    sign_out(d, &sid).await;
    true
}
