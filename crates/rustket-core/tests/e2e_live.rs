//! Live end-to-end encryption tests (see CLAUDE.md, `docs/e2e.md` §15).
//!
//! - `e2e_keys_and_messages_with_e2e_disabled` needs only `RUSTKET_TEST_SERVER`:
//!   everything that works while `E2E_Enable` is off (key upload, room key
//!   creation, `updateGroupKey`, accept, encrypted messages, uploads, pins).
//! - `e2e_full_flow_with_e2e_enabled` additionally needs `RUSTKET_TEST_E2E=1`: it
//!   turns the global `E2E_Enable` on for about a minute (queue-based key
//!   distribution, plaintext rejection, room key reset) and always restores the
//!   previous value, also when the test fails.
//!
//! Every test creates uniquely named users and rooms and deletes them.
//! `RUSTKET_CAPTURE_E2E_FIXTURES=1` writes the server payloads it sees to
//! `crates/rocketchat/tests/fixtures/e2e/`.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::future::Future;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use rocketchat::auth::TwoFactorCode;
use rocketchat::e2e::RoomKey;
use rocketchat::models::{Message, OutgoingMessage, Subscription};
use rocketchat::rest::{CreateRoom, EncryptedMediaConfirm, UploadSource};
use rocketchat::{RestClient, ServerUrl, TlsConfig};
use rustket_core::config::ServerId;
use rustket_core::e2e::{
    AccountE2eState, DecryptResult, E2eConfig, E2eManager, E2eSettings, RoomE2eState,
    decrypt_download,
};
use rustket_core::secrets::MemoryStore;
use serde_json::{Value, json};

const BODY_TIMEOUT: Duration = Duration::from_secs(300);

fn server() -> Option<ServerUrl> {
    match std::env::var("RUSTKET_TEST_SERVER") {
        Ok(s) if !s.is_empty() => Some(ServerUrl::parse(&s).expect("RUSTKET_TEST_SERVER")),
        _ => {
            eprintln!("\n*** SKIPPED: live e2e test (set RUSTKET_TEST_SERVER) ***\n");
            None
        }
    }
}

fn client(server: &ServerUrl) -> RestClient {
    let tls = TlsConfig::new().build().unwrap();
    RestClient::new(server.clone(), &tls).unwrap()
}

fn unique(prefix: &str) -> String {
    format!(
        "{prefix}-{}-{}",
        chrono::Utc::now().timestamp_millis(),
        rocketchat::ids::random_id_len(4).to_lowercase()
    )
}

async fn login(server: &ServerUrl, user: &str, password: &str) -> RestClient {
    let c = client(server);
    c.login_password(user, password, None).await.unwrap();
    c
}

fn capture(name: &str, v: &impl serde::Serialize) {
    if std::env::var_os("RUSTKET_CAPTURE_E2E_FIXTURES").is_none() {
        return;
    }
    let path = format!(
        "{}/../rocketchat/tests/fixtures/e2e/{name}",
        env!("CARGO_MANIFEST_DIR")
    );
    let text = serde_json::to_string_pretty(v).unwrap();
    std::fs::write(&path, text + "\n").unwrap();
}

/// Captures the raw `chat.getMessage` payload of `id`.
async fn capture_message(rest: &RestClient, name: &str, id: &str) {
    if std::env::var_os("RUSTKET_CAPTURE_E2E_FIXTURES").is_none() {
        return;
    }
    let v: Value = rest
        .get(
            "chat.getMessage",
            &rocketchat::rest::Query::new().push("msgId", id),
        )
        .await
        .unwrap();
    capture(name, &v);
}

struct TempUser {
    id: String,
    username: String,
    rest: RestClient,
}

#[derive(Clone)]
struct Fixture {
    admin: RestClient,
    users: Arc<Mutex<Vec<String>>>,
    rooms: Arc<Mutex<Vec<String>>>,
}

impl Fixture {
    async fn new(server: &ServerUrl) -> Self {
        Self {
            admin: login(server, "admin", "adminpass").await,
            users: Arc::default(),
            rooms: Arc::default(),
        }
    }

    fn room(&self, rid: &str) {
        self.rooms.lock().unwrap().push(rid.to_owned());
    }

    async fn user(&self, server: &ServerUrl, prefix: &str) -> TempUser {
        let username = unique(prefix);
        let password = format!("{username}-pw");
        let r: Value = self
            .admin
            .post(
                "users.create",
                &json!({
                    "username": username, "email": format!("{username}@rustket.local"),
                    "password": password, "name": format!("Name {username}"),
                    "verified": true, "requirePasswordChange": false,
                }),
            )
            .await
            .unwrap();
        let id = r["user"]["_id"].as_str().unwrap().to_owned();
        self.users.lock().unwrap().push(id.clone());
        let rest = login(server, &username, &password).await;
        TempUser { id, username, rest }
    }

    async fn cleanup(&self) {
        let rooms = std::mem::take(&mut *self.rooms.lock().unwrap());
        for rid in rooms {
            if let Err(e) = self.admin.rooms_delete(&rid).await {
                eprintln!("cleanup: deleting room {rid} failed: {e}");
            }
        }
        let users = std::mem::take(&mut *self.users.lock().unwrap());
        for id in users {
            let r: Result<Value, _> = self
                .admin
                .post(
                    "users.delete",
                    &json!({ "userId": id, "confirmRelinquish": true }),
                )
                .await;
            if let Err(e) = r {
                eprintln!("cleanup: deleting user {id} failed: {e}");
            }
        }
    }
}

/// Runs `body`, then `cleanup` even if it panicked or timed out.
async fn guarded<Fut>(body: Fut, cleanup: impl Future<Output = ()>)
where
    Fut: Future<Output = ()> + Send + 'static,
{
    let task = tokio::spawn(body);
    let abort = task.abort_handle();
    let result = tokio::time::timeout(BODY_TIMEOUT, task).await;
    cleanup.await;
    match result {
        Ok(Ok(())) => {}
        Ok(Err(e)) => std::panic::resume_unwind(e.into_panic()),
        Err(_) => {
            abort.abort();
            panic!("test body timed out after {BODY_TIMEOUT:?}");
        }
    }
}

fn manager(user: &TempUser, enabled: bool, server: &ServerUrl) -> E2eManager {
    E2eManager::new(
        user.rest.clone(),
        Arc::new(MemoryStore::new()),
        &ServerId("live".into()),
        &user.id,
        E2eSettings {
            enabled,
            ..Default::default()
        },
        E2eConfig {
            auto_distribution: false,
            server_url: Some(server.clone()),
            ..Default::default()
        },
    )
}

async fn refresh(m: &E2eManager, rest: &RestClient, rid: &str) -> Subscription {
    let room = rest.rooms_info(rid).await.unwrap();
    m.update_room(&room).await;
    let sub = rest.subscriptions_get_one(rid).await.unwrap();
    m.update_subscription(&sub).await;
    sub
}

fn decrypted_text(r: DecryptResult) -> String {
    match r {
        DecryptResult::Decrypted(d) => d.message.text().to_owned(),
        other => panic!("not decrypted: {other:?}"),
    }
}

async fn create_encrypted_group(fx: &Fixture, owner: &TempUser, members: &[&TempUser]) -> String {
    let room = owner
        .rest
        .groups_create(&CreateRoom {
            name: unique("e2e-live"),
            members: members.iter().map(|u| u.username.clone()).collect(),
            extra_data: Some(json!({ "encrypted": true })),
            ..Default::default()
        })
        .await
        .unwrap();
    fx.room(&room.id);
    room.id
}

async fn send_text(m: &E2eManager, rest: &RestClient, rid: &str, text: &str) -> Message {
    let out = m.encrypt_outgoing(rid, text).unwrap();
    rest.e2e_send_message(
        rid,
        &rocketchat::ids::random_id(),
        None,
        false,
        &out.content,
        out.mentions.as_ref(),
    )
    .await
    .unwrap()
}

/// Sends an encrypted file; returns the confirmed message and the plaintext.
async fn send_file(m: &E2eManager, rest: &RestClient, rid: &str) -> (Message, Vec<u8>) {
    let data: Vec<u8> = (0..70_000u32).map(|i| (i % 251) as u8).collect();
    let up = m
        .prepare_upload(rid, "notes.txt", "text/plain", &data)
        .unwrap();
    let uploaded = rest
        .e2e_upload_media(
            rid,
            up.hashed_name.clone(),
            UploadSource::Bytes(up.encrypted.clone()),
            &up.file_content,
            None,
        )
        .await
        .unwrap();
    let content = m
        .upload_message_content(rid, &up, &uploaded, "file caption", None)
        .unwrap();
    let msg = rest
        .e2e_confirm_media(
            rid,
            &uploaded.id,
            &EncryptedMediaConfirm {
                tmid: None,
                content,
                file_content: up.file_content.clone(),
            },
        )
        .await
        .unwrap();
    (msg, data)
}

/// Downloads and decrypts the file of a decrypted upload message.
async fn download_plaintext(rest: &RestClient, r: DecryptResult) -> Vec<u8> {
    let DecryptResult::Decrypted(d) = r else {
        panic!("upload not decrypted: {r:?}")
    };
    assert_eq!(d.message.text(), "file caption");
    assert_eq!(d.message.file.as_ref().unwrap().name, "notes.txt");
    let att = &d.message.attachments.as_ref().unwrap()[0];
    let url = att.title_link.clone().unwrap();
    let cipher = rest.download(&url).await.unwrap().bytes().await.unwrap();
    decrypt_download(att, &cipher).unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn e2e_keys_and_messages_with_e2e_disabled() {
    let Some(s) = server() else { return };
    let fx = Fixture::new(&s).await;
    let body_fx = fx.clone();
    let body = async move {
        let fx = body_fx;
        let a = fx.user(&s, "e2e-a").await;
        let b = fx.user(&s, "e2e-b").await;
        // The client-side manager runs regardless of the server switch here.
        let ma = manager(&a, true, &s);
        let mb = manager(&b, true, &s);
        ma.start().await.unwrap();
        mb.start().await.unwrap();
        assert_eq!(ma.account_state(), AccountE2eState::SavePassword);
        let keys = a.rest.e2e_fetch_my_keys().await.unwrap();
        assert!(keys.has_keys());
        let raw: Value = a
            .rest
            .get("e2e.fetchMyKeys", &Default::default())
            .await
            .unwrap();
        capture("fetch_my_keys.json", &raw);
        // A second upload without force is refused.
        let err = a
            .rest
            .e2e_set_user_keys(
                keys.public_key.as_deref().unwrap(),
                keys.private_key.as_deref().unwrap(),
                false,
            )
            .await
            .unwrap_err();
        assert_eq!(err.api_error_code(), Some("error-keys-already-set"));

        let rid = create_encrypted_group(&fx, &a, &[&b]).await;
        refresh(&ma, &a.rest, &rid).await;
        assert_eq!(ma.room_state(&rid), RoomE2eState::Ready);
        let room = a.rest.rooms_info(&rid).await.unwrap();
        let kid = room.e2e_key_id.clone().unwrap();
        assert_eq!(kid.len(), 36);
        // The key id is claimed once.
        assert_eq!(
            a.rest.e2e_set_room_key_id(&rid, "another").await.unwrap(),
            rocketchat::rest::SetKeyIdOutcome::AlreadyExists
        );
        let without: Value = a
            .rest
            .get(
                "e2e.getUsersOfRoomWithoutKey",
                &rocketchat::rest::Query::new().push("rid", &rid),
            )
            .await
            .unwrap();
        capture("get_users_of_room_without_key.json", &without);
        let users = a.rest.e2e_users_of_room_without_key(&rid).await.unwrap();
        assert_eq!(
            users.iter().map(|u| u.user_id.as_str()).collect::<Vec<_>>(),
            [b.id.as_str()]
        );

        // provideUsersSuggestedGroupKeys is a no-op with E2E off: hand b the key
        // through updateGroupKey (suggested key) like old clients did.
        refresh(&mb, &b.rest, &rid).await;
        assert_eq!(mb.room_state(&rid), RoomE2eState::WaitingKeys);
        let a_sub = a.rest.subscriptions_get_one(&rid).await.unwrap();
        let a_private = rocketchat::e2e::UserPrivateKey::from_jwk_str(
            &rocketchat::e2e::PrivateKeyEnvelope::parse(
                keys.private_key.as_deref().unwrap(),
                &a.id,
            )
            .unwrap()
            .decrypt(&ma.pending_passphrase().unwrap())
            .unwrap(),
        )
        .unwrap();
        let room_key = RoomKey::unwrap(&a_private, a_sub.e2e_key.as_deref().unwrap()).unwrap();
        let b_public = rocketchat::e2e::UserPublicKey::from_jwk_str(&users[0].public_key).unwrap();
        a.rest
            .e2e_update_group_key(&rid, &b.id, &room_key.wrap_for(&b_public).unwrap())
            .await
            .unwrap();
        let b_sub: Value = b
            .rest
            .get(
                "subscriptions.getOne",
                &rocketchat::rest::Query::new().push("roomId", &rid),
            )
            .await
            .unwrap();
        capture("subscriptions_get_one_suggested.json", &b_sub);
        refresh(&mb, &b.rest, &rid).await;
        assert_eq!(mb.room_state(&rid), RoomE2eState::Ready);
        let b_sub = b.rest.subscriptions_get_one(&rid).await.unwrap();
        assert!(b_sub.e2e_key.is_some() && b_sub.e2e_suggested_key.is_none());

        // Messages both ways, an edit, a pin.
        let sent = send_text(&ma, &a.rest, &rid, &format!("Привет @{} 🔐", b.username)).await;
        capture_message(&b.rest, "chat_get_message_e2e.json", &sent.id).await;
        assert_eq!(sent.msg, "");
        assert!(sent.e2e_mentions.is_some());
        let got = b.rest.get_message(&sent.id).await.unwrap();
        assert_eq!(
            decrypted_text(mb.decrypt_message(&got)),
            format!("Привет @{} 🔐", b.username)
        );
        let reply = send_text(&mb, &b.rest, &rid, "reply").await;
        assert_eq!(
            decrypted_text(ma.decrypt_message(&a.rest.get_message(&reply.id).await.unwrap())),
            "reply"
        );
        let edit = ma.encrypt_edit(&rid, "edited", None).unwrap();
        a.rest
            .e2e_update_message(&rid, &sent.id, &edit.content, edit.mentions.as_ref())
            .await
            .unwrap();
        let edited = b.rest.get_message(&sent.id).await.unwrap();
        assert_eq!(decrypted_text(mb.decrypt_message(&edited)), "edited");
        a.rest.pin_message(&sent.id).await.unwrap();
        let pins = b.rest.get_pinned(&rid, 0, 10).await.unwrap();
        assert!(!pins.items.is_empty());
        let history = b.rest.load_history(&rid, None, 20, None).await.unwrap();
        let pinned = history
            .messages
            .iter()
            .find(|m| m.t.as_deref() == Some("message_pinned_e2e"))
            .expect("pin system message");
        capture_message(&b.rest, "message_pinned_e2e.json", &pinned.id).await;
        assert_eq!(decrypted_text(mb.decrypt_pinned(pinned)), "edited");

        // Encrypted upload and download.
        let (file_msg, data) = send_file(&ma, &a.rest, &rid).await;
        capture_message(&b.rest, "chat_get_message_e2e_upload.json", &file_msg.id).await;
        let fetched = b.rest.get_message(&file_msg.id).await.unwrap();
        assert_eq!(
            download_plaintext(&b.rest, mb.decrypt_message(&fetched)).await,
            data
        );

        let room: Value = a
            .rest
            .get(
                "rooms.info",
                &rocketchat::rest::Query::new().push("roomId", &rid),
            )
            .await
            .unwrap();
        capture("rooms_info_e2e.json", &room);
    };
    let cleanup_fx = fx.clone();
    guarded(body, async move { cleanup_fx.cleanup().await }).await;
}

// ----- E2E_Enable on -----------------------------------------------------------

/// Restores `E2E_Enable` to its previous value; also on drop (panics, timeouts).
struct E2eEnableGuard {
    server: ServerUrl,
    previous: Value,
    restored: AtomicBool,
}

async fn set_e2e_enable(admin: &RestClient, value: &Value) -> rocketchat::Result<()> {
    let _: Value = admin
        .post_with(
            "settings/E2E_Enable",
            &json!({ "value": value }),
            Some(&TwoFactorCode::password("adminpass")),
        )
        .await?;
    Ok(())
}

async fn read_e2e_enable(admin: &RestClient) -> Value {
    let v: Value = admin
        .get("settings/E2E_Enable", &Default::default())
        .await
        .unwrap();
    v["value"].clone()
}

impl E2eEnableGuard {
    async fn enable(server: &ServerUrl) -> Self {
        let admin = login(server, "admin", "adminpass").await;
        let previous = read_e2e_enable(&admin).await;
        let guard = Self {
            server: server.clone(),
            previous,
            restored: AtomicBool::new(false),
        };
        set_e2e_enable(&admin, &json!(true)).await.unwrap();
        assert_eq!(read_e2e_enable(&admin).await, json!(true));
        guard
    }

    async fn restore(&self) {
        let admin = login(&self.server, "admin", "adminpass").await;
        for attempt in 0..5 {
            match set_e2e_enable(&admin, &self.previous).await {
                Ok(()) => break,
                Err(e) => {
                    eprintln!("restoring E2E_Enable failed (attempt {attempt}): {e}");
                    tokio::time::sleep(Duration::from_secs(2)).await;
                }
            }
        }
        assert_eq!(
            read_e2e_enable(&admin).await,
            self.previous,
            "E2E_Enable not restored"
        );
        self.restored.store(true, Ordering::SeqCst);
    }
}

impl Drop for E2eEnableGuard {
    fn drop(&mut self) {
        if self.restored.load(Ordering::SeqCst) {
            return;
        }
        let (server, previous) = (self.server.clone(), self.previous.clone());
        let _ = std::thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            rt.block_on(async move {
                let admin = login(&server, "admin", "adminpass").await;
                if let Err(e) = set_e2e_enable(&admin, &previous).await {
                    eprintln!("E2E_Enable restore on drop failed: {e}");
                }
            });
        })
        .join();
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn e2e_full_flow_with_e2e_enabled() {
    let Some(s) = server() else { return };
    if std::env::var("RUSTKET_TEST_E2E").as_deref() != Ok("1") {
        eprintln!(
            "\n*** SKIPPED: set RUSTKET_TEST_E2E=1 to toggle E2E_Enable on the test server ***\n"
        );
        return;
    }
    let fx = Fixture::new(&s).await;
    let guard = Arc::new(E2eEnableGuard::enable(&s).await);
    let body_fx = fx.clone();
    let body = async move {
        let fx = body_fx;
        let a = fx.user(&s, "e2e-on-a").await;
        let b = fx.user(&s, "e2e-on-b").await;
        let c = fx.user(&s, "e2e-on-c").await;
        let ma = manager(&a, true, &s);
        let mb = manager(&b, true, &s);
        ma.start().await.unwrap();
        mb.start().await.unwrap();

        // a creates the room key and provides it to b through the queue endpoint.
        let rid = create_encrypted_group(&fx, &a, &[&b]).await;
        refresh(&ma, &a.rest, &rid).await;
        assert_eq!(ma.room_state(&rid), RoomE2eState::Ready);
        let b_sub = refresh(&mb, &b.rest, &rid).await;
        assert!(b_sub.e2e_suggested_key.is_some() || b_sub.e2e_key.is_some());
        assert_eq!(mb.room_state(&rid), RoomE2eState::Ready);

        // The server refuses plaintext in an encrypted room now.
        let plain = a
            .rest
            .send_message(&OutgoingMessage::new(&rid, "plaintext"))
            .await;
        assert_eq!(
            plain
                .err()
                .and_then(|e| e.api_error_code().map(str::to_owned))
                .as_deref(),
            Some("error-not-allowed"),
            "plaintext must be refused in an encrypted room"
        );

        let first = send_text(&ma, &a.rest, &rid, "secret one").await;
        assert_eq!(
            decrypted_text(mb.decrypt_message(&b.rest.get_message(&first.id).await.unwrap())),
            "secret one"
        );
        let (file_msg, data) = send_file(&ma, &a.rest, &rid).await;
        let fetched = b.rest.get_message(&file_msg.id).await.unwrap();
        assert_eq!(
            download_plaintext(&b.rest, mb.decrypt_message(&fetched)).await,
            data
        );

        // c joins later: waits, a's distribution loop provides the key.
        let _: Value = a
            .rest
            .post("groups.invite", &json!({ "roomId": rid, "userId": c.id }))
            .await
            .unwrap();
        let mc = manager(&c, true, &s);
        mc.start().await.unwrap();
        refresh(&mc, &c.rest, &rid).await;
        assert_eq!(mc.room_state(&rid), RoomE2eState::WaitingKeys);
        let waiting = a
            .rest
            .e2e_users_waiting_for_group_key(&[rid.as_str()])
            .await
            .unwrap();
        let raw: Value = a
            .rest
            .get(
                "e2e.fetchUsersWaitingForGroupKey",
                &rocketchat::rest::Query::new().push("roomIds[]", &rid),
            )
            .await
            .unwrap();
        capture("fetch_users_waiting_for_group_key.json", &raw);
        assert!(
            waiting
                .get(&rid)
                .is_some_and(|u| u.iter().any(|e| e.user_id == c.id))
        );
        refresh(&ma, &a.rest, &rid).await;
        assert_eq!(ma.run_distribution_once().await.unwrap(), 1);
        refresh(&mc, &c.rest, &rid).await;
        assert_eq!(mc.room_state(&rid), RoomE2eState::Ready);
        assert_eq!(
            decrypted_text(mc.decrypt_message(&c.rest.get_message(&first.id).await.unwrap())),
            "secret one"
        );

        // Room key reset by the owner: b and c get the new key through the
        // queue and still read the old message with the old key.
        ma.reset_room_key(&rid).await.unwrap();
        let new_kid = a.rest.rooms_info(&rid).await.unwrap().e2e_key_id.unwrap();
        refresh(&mb, &b.rest, &rid).await;
        refresh(&mc, &c.rest, &rid).await;
        assert_eq!(mb.room_state(&rid), RoomE2eState::WaitingKeys);
        refresh(&ma, &a.rest, &rid).await;
        assert_eq!(ma.run_distribution_once().await.unwrap(), 2);
        let b_sub = refresh(&mb, &b.rest, &rid).await;
        refresh(&mc, &c.rest, &rid).await;
        capture(
            "subscription_after_reset.json",
            &b.rest
                .get::<Value>(
                    "subscriptions.getOne",
                    &rocketchat::rest::Query::new().push("roomId", &rid),
                )
                .await
                .unwrap(),
        );
        assert!(b_sub.old_room_keys.as_ref().is_some_and(|o| !o.is_empty()));
        assert_eq!(mb.room_state(&rid), RoomE2eState::Ready);
        let second = send_text(&mb, &b.rest, &rid, "after reset").await;
        assert_eq!(second.content.as_ref().unwrap()["kid"], new_kid.as_str());
        assert_eq!(
            decrypted_text(mc.decrypt_message(&c.rest.get_message(&second.id).await.unwrap())),
            "after reset"
        );
        assert_eq!(
            decrypted_text(mb.decrypt_message(&b.rest.get_message(&first.id).await.unwrap())),
            "secret one"
        );
        assert_eq!(
            decrypted_text(mc.decrypt_message(&c.rest.get_message(&first.id).await.unwrap())),
            "secret one"
        );
    };
    let cleanup_fx = fx.clone();
    let cleanup_guard = guard.clone();
    guarded(body, async move {
        cleanup_fx.cleanup().await;
        cleanup_guard.restore().await;
    })
    .await;
    let admin = login(&server().unwrap(), "admin", "adminpass").await;
    assert_eq!(read_e2e_enable(&admin).await, guard.previous);
}
