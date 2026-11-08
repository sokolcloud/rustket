//! Offline cache screenshots: a first app instance signs in as the
//! throwaway user (offline cache in a temporary directory), opens the two
//! throwaway channels and quits; a second instance starts with the server
//! unreachable and renders the cached workspace (offline strip), another
//! cached room with a message waiting to be sent, and Settings → Storage.
//!
//! Runs only with `RUSTKET_SNAPSHOT_ONLY=cache`.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use rocketchat::ServerUrl;
use rustket::app::{App, Flags, SecretsMode};
use rustket::message::{Message, SecretsReady};
use rustket::workspace::rail;
use rustket_core::ServerId;
use rustket_core::config::{AppConfig, ServerEntry, Theme};
use rustket_core::secrets::{MemoryStore, SecretBackend};
use tokio::sync::mpsc;

use super::{Driver, rest};

/// A driver whose secret store is fed by the test (the app's own lookup in
/// an empty memory store would sign the server out).
fn driver(flags: Flags, sid: &ServerId, token: &str) -> Driver {
    let (tx, rx) = mpsc::unbounded_channel();
    let (app, _boot) = App::new(flags);
    let mut d = Driver {
        app,
        tx,
        rx,
        subs: Default::default(),
    };
    d.send(Message::SecretsReady(SecretsReady {
        store: Arc::new(MemoryStore::new()),
        backend: SecretBackend::Memory,
        tokens: vec![(sid.clone(), Some(token.to_owned()))],
        notices: vec![],
    }));
    d
}

fn stop(d: Driver) {
    for h in d.subs.values() {
        h.abort();
    }
}

fn rid_named(app: &App, sid: &ServerId, prefix: &str) -> String {
    app.servers[sid]
        .workspace
        .rooms()
        .values()
        .find(|e| e.name().starts_with(prefix))
        .map(|e| e.rid().to_owned())
        .unwrap_or_else(|| panic!("room {prefix}*"))
}

fn both(d: &mut Driver, out: &Path, name: &str) {
    d.app.config.ui.theme = Theme::Light;
    d.shoot(out, &format!("{name}-light"));
    d.app.config.ui.theme = Theme::Dark;
    d.shoot(out, &format!("{name}-dark"));
    d.app.config.ui.theme = Theme::Light;
}

pub async fn cache_shots(server: &ServerUrl, out: &Path, user: &(String, String, String)) -> bool {
    if std::env::var("RUSTKET_SNAPSHOT_ONLY").as_deref() != Ok("cache") {
        return false;
    }
    let tmp = tempfile::tempdir().expect("tmp");
    let cfg_dir = tmp.path().join("config");
    let mut cfg = AppConfig::default();
    let mut entry = ServerEntry::new(server.as_str(), "Rustket dev");
    entry.user_id = Some(user.0.clone());
    let sid = cfg.add_server(entry);
    cfg.active_server = Some(sid.clone());
    cfg.save(&cfg_dir).expect("config");
    let login = rest(server).await;
    let token = login
        .login_password(&user.1, &user.2, None)
        .await
        .expect("login")
        .auth_token;
    let flags = Flags {
        config_dir: Some(cfg_dir.clone()),
        cache_dir: Some(tmp.path().join("images")),
        offline_cache: Some(tmp.path().join("offline")),
        secrets: SecretsMode::Memory,
        open_urls: false,
        ..Flags::default()
    };

    // Online: load, open both channels (the dev one last), let it write.
    let mut d = driver(flags.clone(), &sid, &token);
    d.pump_until("initial load", Duration::from_secs(30), |a| {
        a.servers[&sid].workspace.last_sync().is_some() && a.servers[&sid].cache.is_open()
    })
    .await;
    let chat = rid_named(&d.app, &sid, "rk-snap-chat-");
    let dev = rid_named(&d.app, &sid, "rk-snap-dev-");
    for rid in [&chat, &dev] {
        let t = d.app.open_room(&sid, rid);
        d.spawn(t);
        d.pump_until("first page", Duration::from_secs(20), |a| {
            a.servers[&sid]
                .workspace
                .timeline(rid)
                .is_some_and(|t| t.pagination.loaded)
        })
        .await;
        d.settle(Duration::from_secs(1)).await;
    }
    rustket::cache::on_quit(&d.app);
    d.app.flush_config();
    stop(d);

    // Offline: the server address now points nowhere.
    let mut cfg = AppConfig::load(&cfg_dir).expect("config");
    cfg.servers[0].url = "http://127.0.0.1:9/".into();
    cfg.save(&cfg_dir).expect("config");
    let mut d = driver(flags, &sid, &token);
    d.pump_until("cached workspace", Duration::from_secs(20), |a| {
        let s = &a.servers[&sid];
        s.workspace.from_cache()
            && s.active_room.as_deref() == Some(dev.as_str())
            && s.workspace.timeline(&dev).is_some_and(|t| t.has_seeded())
    })
    .await;
    d.settle(Duration::from_secs(2)).await;
    both(&mut d, out, "cache-01-startup-offline");

    let t = d.app.open_room(&sid, &chat);
    d.spawn(t);
    d.pump_until("cached room", Duration::from_secs(10), |a| {
        a.servers[&sid]
            .workspace
            .timeline(&chat)
            .is_some_and(|t| t.has_seeded())
    })
    .await;
    // A message written offline (what the composer's failed send leaves).
    {
        let ws = &mut d.app.servers.get_mut(&sid).expect("server").workspace;
        let m = ws.new_local_message(&chat, "Written while offline", None, false);
        let id = m.id.clone();
        ws.add_pending(m);
        let waiting = "Waiting for the connection; sent automatically";
        ws.mark_pending_failed(&chat, None, &id, waiting);
    }
    d.send(Message::Noop);
    d.settle(Duration::from_secs(1)).await;
    both(&mut d, out, "cache-02-offline-room");

    d.send(Message::Rail(rail::Message::Settings));
    d.settle(Duration::from_secs(1)).await;
    d.shoot_at(
        out,
        "cache-03-settings-storage",
        iced::Size::new(1280.0, 1900.0),
    );
    stop(d);
    let _ = login.logout().await;
    true
}
