//! Custom title bar shots, offline (`RUSTKET_SNAPSHOT_ONLY=titlebar`, no
//! server needed): one and three workspaces with badges and a signed-out
//! marker, light and dark, English and Russian, maximized, a modal open,
//! 1024 px, the settings toggle and the system-title-bar fallback (rail).

use std::path::Path;

use iced::Size;
use rocketchat::RcDate;
use rocketchat::models::{Message as RcMessage, Room, RoomType, Subscription, User, UserRef};
use rustket::app::{Flags, Modal, Screen, SecretsMode};
use rustket::message::Message;
use rustket::workspace::titlebar;
use rustket_core::config::{ServerEntry, Theme};
use rustket_core::sync::InitialData;
use rustket_core::{ServerId, Workspace};

use super::Driver;

fn sub(rid: &str, name: &str, t: RoomType, unread: u64, mentions: u64) -> Subscription {
    Subscription {
        id: format!("sub-{rid}"),
        rid: rid.into(),
        name: name.into(),
        t,
        u: UserRef {
            id: "me".into(),
            username: Some("me".into()),
            name: None,
        },
        open: true,
        unread,
        user_mentions: mentions,
        ..Default::default()
    }
}

fn workspace(rooms: &[(&str, &str, u64, u64)]) -> Workspace {
    let mut ws = Workspace::new();
    ws.apply_initial(InitialData {
        me: User {
            id: "me".into(),
            username: Some("me".into()),
            name: Some("Me".into()),
            ..Default::default()
        },
        subscriptions: rooms
            .iter()
            .map(|(rid, name, u, m)| sub(rid, name, RoomType::Channel, *u, *m))
            .collect(),
        rooms: rooms
            .iter()
            .map(|(rid, name, _, _)| Room {
                id: (*rid).into(),
                t: RoomType::Channel,
                name: Some((*name).into()),
                ..Default::default()
            })
            .collect(),
        ..Default::default()
    });
    ws
}

fn add(
    d: &mut Driver,
    name: &str,
    port: u16,
    rooms: &[(&str, &str, u64, u64)],
    signed_in: bool,
) -> ServerId {
    let entry = ServerEntry::new(format!("http://localhost:{port}"), name);
    let sid = d.app.config.add_server(entry.clone());
    let mut state = rustket::workspace::ServerState::from_entry(&entry).expect("state");
    state.workspace = workspace(rooms);
    state.restoring = false;
    if signed_in {
        state.credentials = Some(rocketchat::Credentials {
            user_id: "me".into(),
            auth_token: "tok".into(),
        });
    }
    d.app.servers.insert(sid.clone(), state);
    sid
}

/// Updates the app without running tasks or subscriptions: the fake
/// servers must not start sessions.
fn upd(d: &mut Driver, m: Message) {
    let _ = d.app.update(m);
}

fn both(d: &mut Driver, out: &Path, name: &str) {
    d.app.config.ui.theme = Theme::Light;
    d.shoot(out, &format!("{name}-light"));
    d.app.config.ui.theme = Theme::Dark;
    d.shoot(out, &format!("{name}-dark"));
}

pub async fn titlebar_shots(out: &Path) -> bool {
    if std::env::var("RUSTKET_SNAPSHOT_ONLY").as_deref() != Ok("titlebar") {
        return false;
    }
    let out = out.join("titlebar");
    std::fs::create_dir_all(&out).expect("out dir");
    let cache = tempfile::tempdir().expect("cache dir");
    let mut d = Driver::new(Flags {
        ephemeral: true,
        cache_dir: Some(cache.path().to_owned()),
        secrets: SecretsMode::Memory,
        open_urls: false,
        ..Flags::default()
    });
    let rooms = [
        ("R1", "general", 0, 0),
        ("R2", "design", 3, 0),
        ("R3", "releases", 0, 0),
        ("R4", "support", 7, 2),
    ];
    let first = add(&mut d, "Acme", 3901, &rooms, true);
    {
        let s = d.app.servers.get_mut(&first).expect("server");
        let now = chrono::Utc::now().timestamp_millis();
        for (i, text) in [
            "The custom title bar is in.",
            "Workspaces moved from the left column to the window title.",
        ]
        .iter()
        .enumerate()
        {
            s.workspace.apply_message(RcMessage {
                id: format!("m{i}"),
                rid: "R1".into(),
                msg: (*text).into(),
                ts: RcDate::from_millis(now - 60_000 + i as i64 * 1000),
                u: UserRef {
                    id: "alice".into(),
                    username: Some("alice".into()),
                    name: Some("Alice".into()),
                },
                ..Default::default()
            });
        }
    }
    d.app.active = Some(first.clone());
    d.app.screen = Screen::Server;
    upd(
        &mut d,
        Message::Sidebar(
            first.clone(),
            rustket::workspace::sidebar::Message::Open("R1".into()),
        ),
    );

    rust_i18n::set_locale("en");
    both(&mut d, &out, "01-one-server");

    add(&mut d, "Rocket Team", 3902, &[("T1", "random", 5, 0)], true);
    add(&mut d, "Old Corp", 3903, &[("O1", "lobby", 0, 0)], false);
    both(&mut d, &out, "02-three-servers");
    rust_i18n::set_locale("ru");
    both(&mut d, &out, "03-three-servers-ru");
    rust_i18n::set_locale("en");

    upd(
        &mut d,
        Message::TitleBar(titlebar::Message::Maximized(true)),
    );
    d.app.config.ui.theme = Theme::Light;
    d.shoot(&out, "04-maximized-light");
    upd(
        &mut d,
        Message::TitleBar(titlebar::Message::Maximized(false)),
    );

    d.app.modals.push(Modal::ConfirmRemove(first.clone()));
    d.app.config.ui.theme = Theme::Dark;
    d.shoot(&out, "05-modal-dark");
    d.app.modals.clear();

    d.app.config.ui.theme = Theme::Light;
    d.shoot_at(&out, "06-1024-light", Size::new(1024.0, 700.0));
    d.app.config.ui.theme = Theme::Dark;
    d.shoot_at(&out, "06-1024-dark", Size::new(1024.0, 700.0));

    upd(
        &mut d,
        Message::Rail(rustket::workspace::rail::Message::Settings),
    );
    d.app.config.ui.theme = Theme::Light;
    d.shoot_at(&out, "07-settings-light", Size::new(1280.0, 1300.0));
    rust_i18n::set_locale("ru");
    d.shoot_at(&out, "07-settings-ru-light", Size::new(1280.0, 1300.0));
    rust_i18n::set_locale("en");
    upd(
        &mut d,
        Message::Settings(rustket::screens::settings::Message::SetSystemTitleBar(true)),
    );
    upd(
        &mut d,
        Message::Settings(rustket::screens::settings::Message::Close),
    );
    assert!(!d.app.custom_chrome());
    both(&mut d, &out, "08-system-title-bar");
    true
}
