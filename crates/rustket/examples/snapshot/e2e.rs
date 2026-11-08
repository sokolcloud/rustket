//! End-to-end encryption shots and live checks (`RUSTKET_TEST_E2E=1` only:
//! they turn the shared server's `E2E_Enable` on and restore it afterwards,
//! also after a failure). `RUSTKET_SNAPSHOT_ONLY=e2e` stops after them.
//!
//! The snapshot user gets keys at runtime (save-password banner and modal),
//! creates an encrypted group with a throwaway peer whose keys live in a
//! `rustket_core` E2eManager in this process, exchanges encrypted messages
//! and an encrypted image, shows the room menu dialogs, a room that waits for
//! its key, and the enter-password banner and modal after the local key is
//! forgotten.

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use rocketchat::auth::TwoFactorCode;
use rocketchat::rest::CreateRoom;
use rocketchat::{RestClient, ServerUrl};
use rustket::e2e::{self, Modal};
use rustket::message::Message;
use rustket::room;
use rustket_core::ServerId;
use rustket_core::config::Theme;
use rustket_core::e2e::{AccountE2eState, E2eConfig, E2eManager, E2eSettings, RoomE2eState};
use rustket_core::secrets::MemoryStore;
use serde_json::{Value, json};

use super::{Driver, Ledger, admin, rest};

fn both(d: &mut Driver, out: &Path, name: &str) {
    d.app.config.ui.theme = Theme::Light;
    d.shoot(out, &format!("{name}-light"));
    d.app.config.ui.theme = Theme::Dark;
    d.shoot(out, &format!("{name}-dark"));
}

fn room_msg(d: &Driver, sid: &ServerId, rid: &str, m: room::Message) -> Message {
    Message::Room(
        sid.clone(),
        d.app.servers[sid].generation,
        rid.to_owned(),
        m,
    )
}

fn e2e_msg(d: &Driver, sid: &ServerId, m: e2e::Message) -> Message {
    Message::E2e(sid.clone(), d.app.servers[sid].generation, m)
}

fn account(d: &Driver, sid: &ServerId) -> AccountE2eState {
    d.app.servers[sid]
        .workspace
        .e2e()
        .map(|m| m.account_state())
        .unwrap_or_default()
}

/// Restores `E2E_Enable`; also on drop (panics, timeouts).
struct EnableGuard {
    server: ServerUrl,
    previous: Value,
    restored: AtomicBool,
}

async fn set_enable(admin: &RestClient, value: &Value) -> rocketchat::Result<()> {
    let _: Value = admin
        .post_with(
            "settings/E2E_Enable",
            &json!({ "value": value }),
            Some(&TwoFactorCode::password("adminpass")),
        )
        .await?;
    Ok(())
}

impl EnableGuard {
    async fn enable(server: &ServerUrl) -> Self {
        let admin = admin(server).await;
        let v: Value = admin
            .get("settings/E2E_Enable", &Default::default())
            .await
            .expect("read E2E_Enable");
        let guard = Self {
            server: server.clone(),
            previous: v["value"].clone(),
            restored: AtomicBool::new(false),
        };
        set_enable(&admin, &json!(true)).await.expect("enable E2E");
        let _ = admin.logout().await;
        guard
    }

    async fn restore(&self) {
        let admin = admin(&self.server).await;
        for attempt in 0..5 {
            match set_enable(&admin, &self.previous).await {
                Ok(()) => break,
                Err(e) => {
                    eprintln!("restoring E2E_Enable failed (attempt {attempt}): {e}");
                    tokio::time::sleep(Duration::from_secs(2)).await;
                }
            }
        }
        let v: Value = admin
            .get("settings/E2E_Enable", &Default::default())
            .await
            .expect("read E2E_Enable");
        assert_eq!(v["value"], self.previous, "E2E_Enable not restored");
        let _ = admin.logout().await;
        self.restored.store(true, Ordering::SeqCst);
        println!("E2E_Enable restored to {}", self.previous);
    }
}

impl Drop for EnableGuard {
    fn drop(&mut self) {
        if self.restored.load(Ordering::SeqCst) {
            return;
        }
        let (server, previous) = (self.server.clone(), self.previous.clone());
        let _ = std::thread::spawn(move || {
            let Ok(rt) = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            else {
                return;
            };
            rt.block_on(async move {
                let admin = admin(&server).await;
                if let Err(e) = set_enable(&admin, &previous).await {
                    eprintln!("E2E_Enable restore on drop failed: {e}");
                }
            });
        })
        .join();
    }
}

async fn login(server: &ServerUrl, user: &str, password: &str) -> RestClient {
    let r = rest(server).await;
    r.login_password(user, password, None).await.expect("login");
    r
}

/// Rooms created here, deleted at the end (by the admin).
#[derive(Default)]
struct Rooms(Vec<String>);

async fn delete_rooms(server: &ServerUrl, rooms: &Rooms) {
    let admin = admin(server).await;
    for rid in &rooms.0 {
        let r: Result<Value, _> = admin.post("rooms.delete", &json!({"roomId": rid})).await;
        match r {
            Ok(_) => println!("deleted room {rid}"),
            Err(e) => eprintln!("rooms.delete {rid}: {e}"),
        }
    }
    let _ = admin.logout().await;
}

pub async fn e2e_shots(
    d: &mut Driver,
    server: &ServerUrl,
    out: &Path,
    sid: &ServerId,
    user: &(String, String, String),
    ledger: &Ledger,
) {
    let only = std::env::var("RUSTKET_SNAPSHOT_ONLY").as_deref() == Ok("e2e");
    if std::env::var("RUSTKET_TEST_E2E").as_deref() != Ok("1") {
        println!("e2e shots skipped: set RUSTKET_TEST_E2E=1 (toggles E2E_Enable on the server)");
        return;
    }
    let guard = EnableGuard::enable(server).await;
    let mut rooms = Rooms::default();
    let peer_mgr = std::sync::Mutex::new(None::<E2eManager>);
    let r = {
        use iced::futures::FutureExt;
        let fut = shots(d, server, out, sid, user, &mut rooms, &peer_mgr);
        let fut = tokio::time::timeout(Duration::from_secs(600), fut);
        std::panic::AssertUnwindSafe(fut).catch_unwind().await
    };
    if let Some(m) = peer_mgr.lock().expect("peer").take() {
        m.stop();
    }
    delete_rooms(server, &rooms).await;
    guard.restore().await;
    match r {
        Err(panic) => std::panic::resume_unwind(panic),
        Ok(Err(_)) => panic!("e2e shots timed out"),
        Ok(Ok(())) => {}
    }
    println!("e2e checks passed");
    if only {
        super::teardown(server, ledger).await;
        std::process::exit(0);
    }
}

#[allow(clippy::too_many_lines)]
async fn shots(
    d: &mut Driver,
    server: &ServerUrl,
    out: &Path,
    sid: &ServerId,
    user: &(String, String, String),
    rooms: &mut Rooms,
    peer_slot: &std::sync::Mutex<Option<E2eManager>>,
) {
    let (_, username, password) = user;
    let peer_name = username.replacen("rk-snap-", "rk-peer-", 1);
    let suffix = username.trim_start_matches("rk-snap-").to_owned();

    // E2E turned on at runtime: the app creates keys and asks to save the
    // generated passphrase.
    let s2 = sid.clone();
    d.pump_until("save password state", Duration::from_secs(240), move |a| {
        a.servers[&s2]
            .workspace
            .e2e()
            .is_some_and(|m| m.account_state() == AccountE2eState::SavePassword)
    })
    .await;
    d.settle(Duration::from_millis(500)).await;
    both(d, out, "e1-banner-save");
    d.send(e2e_msg(d, sid, e2e::Message::OpenModal));
    let passphrase = match &d.app.servers[sid].e2e.modal {
        Some(Modal::SavePassword { passphrase, .. }) => passphrase.to_string(),
        other => panic!("save modal expected, got {other:?}"),
    };
    assert_eq!(passphrase.split(' ').count(), 12, "12-word passphrase");
    both(d, out, "e2-save-modal");
    d.send(e2e_msg(d, sid, e2e::Message::ConfirmSaved));
    let s2 = sid.clone();
    d.pump_until("ready", Duration::from_secs(30), move |a| {
        a.servers[&s2]
            .workspace
            .e2e()
            .is_some_and(|m| m.account_state() == AccountE2eState::Ready)
    })
    .await;

    // The peer: keys in a core manager of this process.
    let peer = login(server, &peer_name, password).await;
    let peer_id = peer.credentials().expect("peer creds").user_id;
    let pm = E2eManager::new(
        peer.clone(),
        Arc::new(MemoryStore::new()),
        &ServerId("snap-peer".into()),
        &peer_id,
        E2eSettings {
            enabled: true,
            ..Default::default()
        },
        E2eConfig {
            server_url: Some(server.clone()),
            ..Default::default()
        },
    );
    pm.start().await.expect("peer keys");
    *peer_slot.lock().expect("peer") = Some(pm.clone());

    // An encrypted group owned by the snapshot user.
    let me = login(server, username, password).await;
    let room = me
        .groups_create(&CreateRoom {
            name: format!("rk-e2e-{suffix}"),
            members: vec![peer_name.clone()],
            extra_data: Some(json!({"encrypted": true})),
            ..Default::default()
        })
        .await
        .expect("encrypted group");
    rooms.0.push(room.id.clone());
    let rid = room.id.clone();
    let (s2, r2) = (sid.clone(), rid.clone());
    d.pump_until("group known", Duration::from_secs(20), move |a| {
        a.servers[&s2].workspace.subscription(&r2).is_some()
    })
    .await;
    let (s2, r2) = (sid.clone(), rid.clone());
    d.pump_until("room key created", Duration::from_secs(60), move |a| {
        a.servers[&s2]
            .workspace
            .e2e()
            .is_some_and(|m| m.room_state(&r2) == RoomE2eState::Ready)
    })
    .await;
    d.send(Message::Sidebar(
        sid.clone(),
        rustket::workspace::sidebar::Message::Open(rid.clone()),
    ));
    let (s2, r2) = (sid.clone(), rid.clone());
    d.pump_until("encrypted room open", Duration::from_secs(20), move |a| {
        a.servers[&s2].rooms.get(&r2).is_some_and(|r| r.loaded)
    })
    .await;

    // Own message through the composer (encrypted on send).
    {
        use iced::widget::text_editor::{Action as EditorAction, Edit};
        use rustket::room::composer;
        for ch in "The *release notes* are in the shared folder.".chars() {
            let m = room_msg(
                d,
                sid,
                &rid,
                room::Message::Composer(composer::Message::Editor(EditorAction::Edit(
                    Edit::Insert(ch),
                ))),
            );
            d.send(m);
        }
        let m = room_msg(
            d,
            sid,
            &rid,
            room::Message::Composer(composer::Message::Send),
        );
        d.send(m);
    }
    let (s2, r2) = (sid.clone(), rid.clone());
    d.pump_until("own encrypted message", Duration::from_secs(20), move |a| {
        let ws = &a.servers[&s2].workspace;
        ws.pending_messages(&r2).is_empty()
            && ws.timeline(&r2).is_some_and(|t| {
                t.messages().any(|m| {
                    m.t.as_deref() == Some("e2e")
                        && m.msg.contains("release notes")
                        && rustket::e2e::shown::is_decrypted(m)
                })
            })
    })
    .await;
    // On the server it is ciphertext only.
    let raw: Value = me
        .get(
            "groups.history",
            &rocketchat::rest::Query::new()
                .push("roomId", rid.as_str())
                .push("count", "5"),
        )
        .await
        .expect("history");
    let first = &raw["messages"][0];
    assert_eq!(first["t"], "e2e", "stored encrypted");
    assert_eq!(first["msg"], "", "no plaintext on the server");
    assert!(first.get("md").is_none(), "no md sent");

    // The peer gets the room key from the app's distribution loop.
    let deadline = std::time::Instant::now() + Duration::from_secs(90);
    loop {
        let r = peer.rooms_info(&rid).await.expect("room");
        pm.update_room(&r).await;
        let sub = peer.subscriptions_get_one(&rid).await.expect("sub");
        pm.update_subscription(&sub).await;
        if pm.can_encrypt(&rid) {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "peer never got the key"
        );
        d.settle(Duration::from_secs(2)).await;
    }
    let text = format!("@{username} the build is green, deploying to staging 🚀");
    let enc = pm.encrypt_outgoing(&rid, &text).expect("peer encrypt");
    peer.e2e_send_message(
        &rid,
        &rocketchat::ids::random_id(),
        None,
        false,
        &enc.content,
        enc.mentions.as_ref(),
    )
    .await
    .expect("peer send");
    let (s2, r2) = (sid.clone(), rid.clone());
    d.pump_until(
        "peer message decrypted",
        Duration::from_secs(20),
        move |a| {
            a.servers[&s2].workspace.timeline(&r2).is_some_and(|t| {
                t.messages().any(|m| {
                    m.msg.contains("deploying to staging") && rustket::e2e::shown::is_decrypted(m)
                })
            })
        },
    )
    .await;

    // An encrypted image through the upload dialog.
    let tmp = tempfile::tempdir().expect("tmp");
    let png = tmp.path().join("architecture.png");
    std::fs::write(&png, super::image_bytes(480, 300)).expect("png");
    d.send(Message::FileDropped(png));
    let (s2, r2) = (sid.clone(), rid.clone());
    d.pump_until("upload dialog", Duration::from_secs(10), move |a| {
        a.servers[&s2]
            .rooms
            .get(&r2)
            .is_some_and(|r| r.composer.is_upload_dialog_open())
    })
    .await;
    let m = room_msg(
        d,
        sid,
        &rid,
        room::Message::Composer(room::composer::Message::Upload(
            room::upload::Message::Text("Architecture overview".into()),
        )),
    );
    d.send(m);
    let m = room_msg(
        d,
        sid,
        &rid,
        room::Message::Composer(room::composer::Message::Upload(room::upload::Message::Send)),
    );
    d.send(m);
    fn image_url(a: &rustket::App, sid: &ServerId, rid: &str) -> Option<String> {
        a.servers[sid].workspace.timeline(rid).and_then(|t| {
            t.messages()
                .filter(|m| rustket::e2e::shown::is_decrypted(m))
                .flat_map(|m| m.attachments.iter().flatten())
                .find_map(|att| att.image_url.clone())
        })
    }
    let (s2, r2) = (sid.clone(), rid.clone());
    d.pump_until(
        "encrypted image message",
        Duration::from_secs(60),
        move |a| image_url(a, &s2, &r2).is_some_and(|u| u.ends_with(rustket::e2e::files::FRAGMENT)),
    )
    .await;
    // Drawing requests the image; it is downloaded, decrypted and checked.
    d.shoot(out, "e3-encrypted-room-loading-light");
    let url = image_url(&d.app, sid, &rid).expect("image url");
    d.pump_until(
        "encrypted image decrypted",
        Duration::from_secs(60),
        move |a| a.images.contains(&url),
    )
    .await;
    // The file on the server is ciphertext with a hashed name.
    let files: Value = me
        .get(
            "groups.files",
            &rocketchat::rest::Query::new().push("roomId", rid.as_str()),
        )
        .await
        .expect("files");
    let f = &files["files"][0];
    assert_eq!(f["type"], "application/octet-stream");
    assert_ne!(f["name"], "architecture.png", "file name not leaked");
    assert!(f.get("description").is_none_or(Value::is_null));
    d.settle(Duration::from_secs(1)).await;
    both(d, out, "e3-encrypted-room");

    // Room menu: encryption entries, the disable dialog.
    use room::header::Message as Hm;
    d.send(room_msg(
        d,
        sid,
        &rid,
        room::Message::Header(Hm::ToggleMenu),
    ));
    both(d, out, "e4-room-menu");
    d.send(room_msg(
        d,
        sid,
        &rid,
        room::Message::Header(Hm::E2e(e2e::room::Msg::Open(e2e::room::Dialog::Disable))),
    ));
    both(d, out, "e5-disable-dialog");
    d.send(room_msg(
        d,
        sid,
        &rid,
        room::Message::Header(Hm::E2e(e2e::room::Msg::Cancel)),
    ));

    // A room whose key exists but was never shared with the snapshot user.
    let waiting = peer
        .groups_create(&CreateRoom {
            name: format!("rk-e2e-wait-{suffix}"),
            members: vec![],
            extra_data: Some(json!({"encrypted": true})),
            ..Default::default()
        })
        .await
        .expect("waiting group");
    rooms.0.push(waiting.id.clone());
    peer.e2e_set_room_key_id(&waiting.id, &rocketchat::e2e::new_key_id())
        .await
        .expect("key id");
    let me_id = me.credentials().expect("creds").user_id;
    let _: Value = peer
        .post(
            "groups.invite",
            &json!({"roomId": waiting.id, "userId": me_id}),
        )
        .await
        .expect("invite");
    let _: Value = peer
        .post(
            "groups.addOwner",
            &json!({"roomId": waiting.id, "userId": me_id}),
        )
        .await
        .expect("owner");
    let (s2, w2) = (sid.clone(), waiting.id.clone());
    d.pump_until("waiting for the key", Duration::from_secs(30), move |a| {
        a.servers[&s2]
            .workspace
            .e2e()
            .is_some_and(|m| m.room_state(&w2) == RoomE2eState::WaitingKeys)
    })
    .await;
    d.send(Message::Sidebar(
        sid.clone(),
        rustket::workspace::sidebar::Message::Open(waiting.id.clone()),
    ));
    let (s2, w2) = (sid.clone(), waiting.id.clone());
    d.pump_until("waiting room open", Duration::from_secs(20), move |a| {
        a.servers[&s2].rooms.contains_key(&w2)
            && rustket::e2e::room::can_reset(&a.servers[&s2].workspace, &w2)
    })
    .await;
    d.settle(Duration::from_millis(500)).await;
    both(d, out, "e6-waiting-for-key");
    d.send(room_msg(
        d,
        sid,
        &waiting.id,
        room::Message::Header(Hm::E2e(e2e::room::Msg::Open(e2e::room::Dialog::Reset))),
    ));
    both(d, out, "e7-reset-dialog");
    d.send(room_msg(
        d,
        sid,
        &waiting.id,
        room::Message::Header(Hm::E2e(e2e::room::Msg::Cancel)),
    ));

    // A plain private group: the enable dialog.
    let plain = me
        .groups_create(&CreateRoom {
            name: format!("rk-e2e-plain-{suffix}"),
            members: vec![],
            ..Default::default()
        })
        .await
        .expect("plain group");
    rooms.0.push(plain.id.clone());
    let (s2, p2) = (sid.clone(), plain.id.clone());
    d.pump_until("plain group known", Duration::from_secs(20), move |a| {
        a.servers[&s2].workspace.subscription(&p2).is_some()
    })
    .await;
    d.send(Message::Sidebar(
        sid.clone(),
        rustket::workspace::sidebar::Message::Open(plain.id.clone()),
    ));
    let (s2, p2) = (sid.clone(), plain.id.clone());
    d.pump_until("plain group open", Duration::from_secs(20), move |a| {
        a.servers[&s2].rooms.get(&p2).is_some_and(|r| r.loaded)
    })
    .await;
    d.send(room_msg(
        d,
        sid,
        &plain.id,
        room::Message::Header(Hm::E2e(e2e::room::Msg::Open(e2e::room::Dialog::Enable))),
    ));
    both(d, out, "e8-enable-dialog");
    d.send(room_msg(
        d,
        sid,
        &plain.id,
        room::Message::Header(Hm::E2e(e2e::room::Msg::Cancel)),
    ));

    // The local key is forgotten (another device, a restart without the
    // secret): enter the passphrase again.
    let mgr = d.app.servers[sid]
        .workspace
        .e2e()
        .cloned()
        .expect("manager");
    mgr.logout().await;
    d.send(e2e_msg(d, sid, e2e::Message::Retry));
    let s2 = sid.clone();
    d.pump_until("enter password state", Duration::from_secs(30), move |a| {
        matches!(
            a.servers[&s2].workspace.e2e().map(|m| m.account_state()),
            Some(AccountE2eState::EnterPassword { .. })
        )
    })
    .await;
    d.send(Message::Sidebar(
        sid.clone(),
        rustket::workspace::sidebar::Message::Open(rid.clone()),
    ));
    d.settle(Duration::from_millis(800)).await;
    both(d, out, "e9-banner-enter-password");
    d.send(e2e_msg(d, sid, e2e::Message::OpenModal));
    both(d, out, "e10-enter-password-modal");
    d.send(e2e_msg(
        d,
        sid,
        e2e::Message::PasswordInput("not my passphrase".into()),
    ));
    d.send(e2e_msg(d, sid, e2e::Message::SubmitPassword));
    let s2 = sid.clone();
    d.pump_until("wrong password", Duration::from_secs(60), move |a| {
        matches!(
            &a.servers[&s2].e2e.modal,
            Some(Modal::EnterPassword {
                error: Some(_),
                busy: false,
                ..
            })
        )
    })
    .await;
    both(d, out, "e11-enter-password-failed");
    d.send(e2e_msg(d, sid, e2e::Message::PasswordInput(passphrase)));
    d.send(e2e_msg(d, sid, e2e::Message::SubmitPassword));
    let s2 = sid.clone();
    d.pump_until("unlocked", Duration::from_secs(60), move |a| {
        a.servers[&s2].e2e.modal.is_none()
            && a.servers[&s2]
                .workspace
                .e2e()
                .is_some_and(|m| m.account_state() == AccountE2eState::Ready)
    })
    .await;
    let (s2, r2) = (sid.clone(), rid.clone());
    d.pump_until(
        "history decrypted again",
        Duration::from_secs(30),
        move |a| {
            a.servers[&s2].workspace.timeline(&r2).is_some_and(|t| {
                t.messages().any(|m| {
                    m.msg.contains("deploying to staging") && rustket::e2e::shown::is_decrypted(m)
                })
            })
        },
    )
    .await;
    assert_eq!(account(d, sid), AccountE2eState::Ready);

    // Account → Security: change passphrase and reset.
    d.app.screen = rustket::app::Screen::Account(
        sid.clone(),
        Box::new(rustket::screens::account::Account::with_tab(
            rustket::screens::account::Tab::Security,
        )),
    );
    d.send(Message::Account(
        sid.clone(),
        rustket::screens::account::Message::Security(
            rustket::screens::account::security::Msg::E2e(
                rustket::screens::account::e2e::Msg::New("Correct-Horse-Battery".into()),
            ),
        ),
    ));
    d.settle(Duration::from_secs(1)).await;
    let tall = iced::Size::new(1280.0, 1300.0);
    d.app.config.ui.theme = Theme::Light;
    d.shoot_at(out, "e12-account-security-light", tall);
    d.app.config.ui.theme = Theme::Dark;
    d.shoot_at(out, "e12-account-security-dark", tall);
    d.app.screen = rustket::app::Screen::Server;
    let _ = me.logout().await;
    let _ = peer.logout().await;
    d.app.config.ui.theme = Theme::Light;
}
