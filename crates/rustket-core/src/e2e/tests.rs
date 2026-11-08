//! State machine tests with a mock [`E2eApi`].
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use rocketchat::auth::TwoFactorCode;
use rocketchat::e2e::{PrivateKeyEnvelope, RoomKey, UserPrivateKey, UserPublicKey};
use rocketchat::models::{
    Message, OldRoomKey, Room, RoomType, Setting, Subscription, UploadedFile, WaitingUser,
};
use rocketchat::rest::{MyKeys, SetKeyIdOutcome, SuggestedKey, UserPublicKeyEntry};
use rocketchat::session::{ChangeKind, SessionEvent};
use rocketchat::{Error, RcDate};
use serde_json::{Value, json};

use super::*;
use crate::config::ServerId;
use crate::secrets::{MemoryStore, SecretStore};

const ME: &str = "BFqA4FAHKX4qEywfJ";
const OTHER: &str = "userE2EE";
const RID: &str = "room1";

fn fixture(name: &str) -> Value {
    let path = format!(
        "{}/../rocketchat/tests/fixtures/e2e/{name}",
        env!("CARGO_MANIFEST_DIR")
    );
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

/// My keys (pwdv2web) and the other member's keys (pwdv1web).
struct Keys {
    my_public: String,
    my_private: String,
    my_envelope: String,
    my_passphrase: String,
    other_public: String,
    other_private: String,
}

fn keys() -> Keys {
    let me = fixture("keychain_pwdv2web.json");
    let other = fixture("keychain_pwdv1web.json");
    let s = |v: &Value, k: &str| v[k].as_str().unwrap().to_owned();
    Keys {
        my_public: s(&me, "public_key"),
        my_private: s(&me, "private_key"),
        my_envelope: s(&me, "persisted"),
        my_passphrase: s(&me, "passphrase"),
        other_public: s(&other, "public_key"),
        other_private: s(&other, "private_key"),
    }
}

fn err(code: &str) -> Error {
    Error::Api {
        status: 400,
        error: Some(code.into()),
        error_type: Some(code.into()),
        message: None,
        details: None,
    }
}

#[derive(Default)]
struct MockState {
    my_keys: MyKeys,
    fail_fetch: bool,
    keys_already_set: bool,
    room_key_ids: HashMap<String, String>,
    without_key: HashMap<String, Vec<UserPublicKeyEntry>>,
    waiting: HashMap<String, Vec<UserPublicKeyEntry>>,
    accept_error: Option<String>,
    calls: Vec<String>,
    uploaded: Vec<(String, String, bool)>,
    group_keys: Vec<(String, String, String)>,
    provided: Vec<HashMap<String, Vec<SuggestedKey>>>,
    resets: Vec<(String, String, String)>,
    fail_update_group_key: bool,
    waiting_calls: usize,
    /// `fetch_my_keys` waits for this before answering.
    fetch_gate: Option<Arc<tokio::sync::Notify>>,
}

#[derive(Clone, Default)]
struct MockApi(Arc<Mutex<MockState>>);

impl MockApi {
    fn st(&self) -> std::sync::MutexGuard<'_, MockState> {
        self.0.lock().unwrap()
    }
    fn called(&self, name: &str) -> bool {
        self.st().calls.iter().any(|c| c == name)
    }
}

impl E2eApi for MockApi {
    fn user_id(&self) -> Option<String> {
        Some(ME.into())
    }
    async fn fetch_my_keys(&self) -> rocketchat::Result<MyKeys> {
        let gate = self.st().fetch_gate.clone();
        if let Some(g) = gate {
            g.notified().await;
        }
        let mut s = self.st();
        s.calls.push("fetch_my_keys".into());
        if s.fail_fetch {
            return Err(Error::Timeout("mock"));
        }
        Ok(s.my_keys.clone())
    }
    async fn set_user_keys(
        &self,
        public_key: &str,
        private_key: &str,
        force: bool,
    ) -> rocketchat::Result<()> {
        let mut s = self.st();
        s.calls.push("set_user_keys".into());
        if s.keys_already_set && !force {
            return Err(err("error-keys-already-set"));
        }
        s.uploaded
            .push((public_key.into(), private_key.into(), force));
        Ok(())
    }
    async fn request_subscription_keys(&self) -> rocketchat::Result<()> {
        self.st().calls.push("request_subscription_keys".into());
        Ok(())
    }
    async fn set_room_key_id(
        &self,
        rid: &str,
        key_id: &str,
    ) -> rocketchat::Result<SetKeyIdOutcome> {
        let mut s = self.st();
        s.calls.push("set_room_key_id".into());
        if s.room_key_ids.contains_key(rid) {
            return Ok(SetKeyIdOutcome::AlreadyExists);
        }
        s.room_key_ids.insert(rid.into(), key_id.into());
        Ok(SetKeyIdOutcome::Set)
    }
    async fn update_group_key(&self, rid: &str, uid: &str, key: &str) -> rocketchat::Result<()> {
        let mut s = self.st();
        s.calls.push("update_group_key".into());
        if s.fail_update_group_key {
            return Err(Error::Timeout("mock"));
        }
        s.group_keys.push((rid.into(), uid.into(), key.into()));
        Ok(())
    }
    async fn users_of_room_without_key(
        &self,
        rid: &str,
    ) -> rocketchat::Result<Vec<UserPublicKeyEntry>> {
        Ok(self.st().without_key.get(rid).cloned().unwrap_or_default())
    }
    async fn accept_suggested_group_key(&self, _rid: &str) -> rocketchat::Result<()> {
        let mut s = self.st();
        s.calls.push("accept".into());
        match s.accept_error.clone() {
            Some(code) => Err(err(&code)),
            None => Ok(()),
        }
    }
    async fn reject_suggested_group_key(&self, _rid: &str) -> rocketchat::Result<()> {
        self.st().calls.push("reject".into());
        Ok(())
    }
    async fn users_waiting_for_group_key(
        &self,
        room_ids: &[String],
    ) -> rocketchat::Result<HashMap<String, Vec<UserPublicKeyEntry>>> {
        let mut s = self.st();
        s.waiting_calls += 1;
        Ok(room_ids
            .iter()
            .filter_map(|r| s.waiting.get(r).map(|u| (r.clone(), u.clone())))
            .collect())
    }
    async fn provide_users_suggested_group_keys(
        &self,
        keys: &HashMap<String, Vec<SuggestedKey>>,
    ) -> rocketchat::Result<()> {
        self.st().provided.push(keys.clone());
        Ok(())
    }
    async fn reset_room_key(
        &self,
        rid: &str,
        e2e_key: &str,
        e2e_key_id: &str,
    ) -> rocketchat::Result<()> {
        self.st()
            .resets
            .push((rid.into(), e2e_key.into(), e2e_key_id.into()));
        Ok(())
    }
    async fn reset_own_key(&self, two_factor: Option<&TwoFactorCode>) -> rocketchat::Result<()> {
        self.st()
            .calls
            .push(format!("reset_own_key:{}", two_factor.is_some()));
        Ok(())
    }
    async fn set_room_encrypted(&self, _rid: &str, encrypted: bool) -> rocketchat::Result<()> {
        self.st()
            .calls
            .push(format!("set_room_encrypted:{encrypted}"));
        Ok(())
    }
}

fn enabled() -> E2eSettings {
    E2eSettings {
        enabled: true,
        ..Default::default()
    }
}

fn config() -> E2eConfig {
    E2eConfig {
        auto_distribution: false,
        ..Default::default()
    }
}

fn manager(api: &MockApi, store: &Arc<MemoryStore>) -> E2eManager<MockApi> {
    E2eManager::new(
        api.clone(),
        store.clone(),
        &ServerId("srv".into()),
        ME,
        enabled(),
        config(),
    )
}

/// A manager whose account is ready with my fixture key (cached + on server).
async fn ready_manager() -> (E2eManager<MockApi>, MockApi, Arc<MemoryStore>) {
    let k = keys();
    let api = MockApi::default();
    api.st().my_keys = MyKeys {
        public_key: Some(k.my_public.clone()),
        private_key: Some(k.my_envelope.clone()),
    };
    let store = Arc::new(MemoryStore::new());
    store
        .set(
            &private_key_secret(&ServerId("srv".into()), ME),
            &k.my_private,
        )
        .unwrap();
    let m = manager(&api, &store);
    m.start().await.unwrap();
    assert_eq!(m.account_state(), AccountE2eState::Ready);
    (m, api, store)
}

fn my_public() -> UserPublicKey {
    UserPublicKey::from_jwk_str(&keys().my_public).unwrap()
}

fn room(kid: Option<&str>) -> Room {
    Room {
        id: RID.into(),
        t: RoomType::Private,
        encrypted: Some(true),
        e2e_key_id: kid.map(Into::into),
        ..Default::default()
    }
}

fn sub(e2e_key: Option<String>, suggested: Option<String>) -> Subscription {
    Subscription {
        id: "s1".into(),
        rid: RID.into(),
        t: RoomType::Private,
        e2e_key,
        e2e_suggested_key: suggested,
        ..Default::default()
    }
}

fn e2e_msg(id: &str, content: Value, updated: i64) -> Message {
    Message {
        id: id.into(),
        rid: RID.into(),
        t: Some("e2e".into()),
        content: Some(content),
        updated_at: RcDate::from_millis(updated),
        ..Default::default()
    }
}

fn text(r: &DecryptResult) -> String {
    match r {
        DecryptResult::Decrypted(d) => d.message.text().to_owned(),
        other => panic!("not decrypted: {other:?}"),
    }
}

#[tokio::test]
async fn first_start_creates_and_uploads_keys() {
    let api = MockApi::default();
    let store = Arc::new(MemoryStore::new());
    let m = manager(&api, &store);
    let mut events = m.subscribe();
    m.start().await.unwrap();
    assert_eq!(m.account_state(), AccountE2eState::SavePassword);
    let passphrase = m.pending_passphrase().unwrap();
    assert_eq!(passphrase.split(' ').count(), 12);
    let (public, envelope, force) = api.st().uploaded[0].clone();
    assert!(!force);
    // The uploaded backup decrypts with the generated passphrase to the cached key.
    let jwk = PrivateKeyEnvelope::parse(&envelope, ME)
        .unwrap()
        .decrypt(&passphrase)
        .unwrap();
    let private = UserPrivateKey::from_jwk_str(&jwk).unwrap();
    assert_eq!(
        private.public_key(),
        UserPublicKey::from_jwk_str(&public).unwrap()
    );
    let server = ServerId("srv".into());
    assert_eq!(
        store
            .get(&private_key_secret(&server, ME))
            .unwrap()
            .as_deref(),
        Some(jwk.as_str())
    );
    assert_eq!(
        store
            .get(&pending_passphrase_secret(&server, ME))
            .unwrap()
            .as_deref(),
        Some(passphrase.as_str())
    );
    assert!(api.called("request_subscription_keys"));
    m.confirm_passphrase_saved().await.unwrap();
    assert_eq!(m.account_state(), AccountE2eState::Ready);
    assert!(
        store
            .get(&pending_passphrase_secret(&server, ME))
            .unwrap()
            .is_none()
    );
    let mut states = Vec::new();
    while let Ok(ev) = events.try_recv() {
        if let E2eEvent::AccountState(s) = ev {
            states.push(s);
        }
    }
    assert_eq!(
        states,
        [
            AccountE2eState::LoadingKeys,
            AccountE2eState::SavePassword,
            AccountE2eState::Ready
        ]
    );
}

#[tokio::test]
async fn fetch_failure_never_generates_keys() {
    let api = MockApi::default();
    api.st().fail_fetch = true;
    let m = manager(&api, &Arc::new(MemoryStore::new()));
    assert!(m.start().await.is_err());
    assert!(matches!(m.account_state(), AccountE2eState::Error(_)));
    assert!(!api.called("set_user_keys"));
}

#[tokio::test]
async fn disabled_setting_keeps_manager_off() {
    let api = MockApi::default();
    let m = E2eManager::new(
        api.clone(),
        Arc::new(MemoryStore::new()),
        &ServerId("srv".into()),
        ME,
        E2eSettings::default(),
        config(),
    );
    m.start().await.unwrap();
    assert_eq!(m.account_state(), AccountE2eState::Disabled);
    assert!(api.st().calls.is_empty());
}

#[tokio::test]
async fn upload_race_falls_back_to_enter_password() {
    let api = MockApi::default();
    api.st().keys_already_set = true;
    let store = Arc::new(MemoryStore::new());
    let m = manager(&api, &store);
    m.start().await.unwrap();
    assert_eq!(
        m.account_state(),
        AccountE2eState::EnterPassword { failed: false }
    );
    assert!(
        store
            .get(&private_key_secret(&ServerId("srv".into()), ME))
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn unlock_with_passphrase() {
    let k = keys();
    let api = MockApi::default();
    api.st().my_keys = MyKeys {
        public_key: Some(k.my_public.clone()),
        private_key: Some(k.my_envelope.clone()),
    };
    let store = Arc::new(MemoryStore::new());
    let m = manager(&api, &store);
    m.start().await.unwrap();
    assert_eq!(
        m.account_state(),
        AccountE2eState::EnterPassword { failed: false }
    );
    assert!(matches!(
        m.submit_password("wrong").await,
        Err(E2eManagerError::WrongPassword)
    ));
    assert_eq!(
        m.account_state(),
        AccountE2eState::EnterPassword { failed: true }
    );
    m.submit_password(&k.my_passphrase).await.unwrap();
    assert_eq!(m.account_state(), AccountE2eState::Ready);
    let cached = store
        .get(&private_key_secret(&ServerId("srv".into()), ME))
        .unwrap()
        .unwrap();
    assert_eq!(cached, k.my_private);
    assert!(matches!(
        m.submit_password("x").await,
        Err(E2eManagerError::NotReady)
    ));
}

#[tokio::test]
async fn stale_cache_is_dropped_when_server_key_differs() {
    let k = keys();
    let api = MockApi::default();
    api.st().my_keys = MyKeys {
        public_key: Some(k.my_public.clone()),
        private_key: Some(k.my_envelope.clone()),
    };
    let store = Arc::new(MemoryStore::new());
    let server = ServerId("srv".into());
    store
        .set(&private_key_secret(&server, ME), &k.other_private)
        .unwrap();
    store
        .set(&pending_passphrase_secret(&server, ME), "old words")
        .unwrap();
    let m = manager(&api, &store);
    m.start().await.unwrap();
    assert_eq!(
        m.account_state(),
        AccountE2eState::EnterPassword { failed: false }
    );
    assert!(
        store
            .get(&private_key_secret(&server, ME))
            .unwrap()
            .is_none()
    );
    assert!(
        store
            .get(&pending_passphrase_secret(&server, ME))
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn creates_the_first_room_key_and_provides_it() {
    let (m, api, _) = ready_manager().await;
    let k = keys();
    api.st().without_key.insert(
        RID.into(),
        vec![
            UserPublicKeyEntry {
                user_id: ME.into(),
                public_key: k.my_public.clone(),
            },
            UserPublicKeyEntry {
                user_id: OTHER.into(),
                public_key: k.other_public.clone(),
            },
            UserPublicKeyEntry {
                user_id: "broken".into(),
                public_key: "{}".into(),
            },
        ],
    );
    m.update_room(&room(None)).await;
    assert_eq!(m.room_state(RID), RoomE2eState::Ready);
    let kid = api.st().room_key_ids[RID].clone();
    let (rid, uid, wrapped) = api.st().group_keys[0].clone();
    assert_eq!((rid.as_str(), uid.as_str()), (RID, ME));
    assert!(wrapped.starts_with(&kid));
    let provided = api.st().provided[0][RID].clone();
    assert_eq!(provided.len(), 1);
    assert_eq!(provided[0].user_id, OTHER);
    // The other member can unwrap the key and read my message.
    let other = UserPrivateKey::from_jwk_str(&k.other_private).unwrap();
    let their_key = RoomKey::unwrap(&other, &provided[0].key).unwrap();
    let out = m.encrypt_outgoing(RID, "hello @alice").unwrap();
    assert_eq!(out.content["kid"], kid.as_str());
    assert_eq!(out.mentions.unwrap().e2e_user_mentions, ["@alice"]);
    let env = rocketchat::e2e::EncryptedContent::from_value(&out.content).unwrap();
    assert_eq!(
        their_key.decrypt_json(&env).unwrap(),
        json!({"msg": "hello @alice"})
    );
    assert_eq!(
        text(&m.decrypt_message(&e2e_msg("m1", out.content, 1))),
        "hello @alice"
    );
}

#[tokio::test]
async fn losing_the_key_id_race_waits() {
    let (m, api, _) = ready_manager().await;
    api.st().room_key_ids.insert(RID.into(), "theirs".into());
    m.update_room(&room(None)).await;
    assert_eq!(m.room_state(RID), RoomE2eState::WaitingKeys);
    assert!(api.st().group_keys.is_empty());
    assert!(matches!(
        m.encrypt_outgoing(RID, "x"),
        Err(E2eManagerError::RoomNotReady)
    ));
}

#[tokio::test]
async fn own_key_and_old_keys_decrypt() {
    let (m, _, _) = ready_manager().await;
    let current = RoomKey::generate();
    let old = RoomKey::generate();
    let me = my_public();
    let mut s = sub(Some(current.wrap_for(&me).unwrap()), None);
    s.old_room_keys = Some(vec![
        OldRoomKey {
            e2e_key_id: old.kid.clone(),
            ts: Some(RcDate::now()),
            e2e_key: old.wrap_for(&me).unwrap(),
        },
        OldRoomKey {
            e2e_key_id: "gone".into(),
            ts: None,
            e2e_key: "garbage".into(),
        },
    ]);
    m.update_room(&room(Some(&current.kid))).await;
    m.update_subscription(&s).await;
    assert_eq!(m.room_state(RID), RoomE2eState::Ready);
    let enc = |k: &RoomKey, t: &str| k.encrypt_json(&json!({"msg": t})).unwrap().to_json();
    assert_eq!(
        text(&m.decrypt_message(&e2e_msg("a", enc(&current, "now"), 1))),
        "now"
    );
    assert_eq!(
        text(&m.decrypt_message(&e2e_msg("b", enc(&old, "before"), 1))),
        "before"
    );
    // Unknown kid: current key as fallback (fails → indecipherable).
    let stranger = RoomKey::generate();
    assert_eq!(
        m.decrypt_message(&e2e_msg("c", enc(&stranger, "?"), 1)),
        DecryptResult::Failed(DecryptFailure::NoKey)
    );
    let fallback = current.clone().with_kid("other-kid");
    assert_eq!(
        text(&m.decrypt_message(&e2e_msg("d", enc(&fallback, "fb"), 1))),
        "fb"
    );
    // Right kid, wrong key material.
    let liar = stranger.with_kid(current.kid.clone());
    assert_eq!(
        m.decrypt_message(&e2e_msg("e", enc(&liar, "x"), 1)),
        DecryptResult::Failed(DecryptFailure::KeyError)
    );
}

#[tokio::test]
async fn undecryptable_own_key_is_not_ready() {
    let (m, _, _) = ready_manager().await;
    let other = UserPublicKey::from_jwk_str(&keys().other_public).unwrap();
    let key = RoomKey::generate();
    m.update_room(&room(Some(&key.kid))).await;
    m.update_subscription(&sub(Some(key.wrap_for(&other).unwrap()), None))
        .await;
    assert_eq!(m.room_state(RID), RoomE2eState::WaitingKeys);
    assert!(!m.can_encrypt(RID));
}

#[tokio::test]
async fn suggested_keys_accept_and_reject() {
    let (m, api, _) = ready_manager().await;
    let me = my_public();
    let key = RoomKey::generate();
    m.update_room(&room(Some(&key.kid))).await;
    assert_eq!(m.room_state(RID), RoomE2eState::WaitingKeys);
    let early = e2e_msg(
        "p1",
        key.encrypt_json(&json!({"msg": "early"}))
            .unwrap()
            .to_json(),
        1,
    );
    assert_eq!(m.decrypt_message(&early), DecryptResult::Pending);
    let mut events = m.subscribe();

    // Stale kid → reject.
    let stale = RoomKey::generate().wrap_for(&me).unwrap();
    m.update_subscription(&sub(None, Some(stale.clone()))).await;
    assert!(api.called("reject"));
    assert!(!api.called("accept"));
    // The same rejected suggestion is not processed twice.
    api.st().calls.clear();
    m.update_subscription(&sub(None, Some(stale))).await;
    assert!(!api.called("reject"));
    // Garbage → reject.
    m.update_subscription(&sub(
        None,
        Some(format!("{}{}", key.kid, "A".repeat(342) + "==")),
    ))
    .await;
    assert!(api.called("reject"));

    // Valid → accept, keys change, pending message named.
    let good = key.wrap_for(&me).unwrap();
    m.update_subscription(&sub(None, Some(good.clone()))).await;
    assert!(api.called("accept"));
    assert_eq!(m.room_state(RID), RoomE2eState::Ready);
    let mut pending = None;
    while let Ok(ev) = events.try_recv() {
        if let E2eEvent::KeysChanged { pending: p, .. } = ev {
            pending = Some(p);
        }
    }
    assert_eq!(pending.unwrap(), ["p1"]);
    assert_eq!(text(&m.decrypt_message(&early)), "early");

    // Same kid but different key material → reject, working key kept.
    api.st().calls.clear();
    let liar = RoomKey::generate()
        .with_kid(key.kid.clone())
        .wrap_for(&me)
        .unwrap();
    m.update_subscription(&sub(Some(good.clone()), Some(liar)))
        .await;
    assert!(api.called("reject"));
    assert_eq!(text(&m.decrypt_message(&early)), "early");
}

#[tokio::test]
async fn accept_uses_suggested_old_keys_when_none_exist() {
    let (m, _, _) = ready_manager().await;
    let me = my_public();
    let (key, old) = (RoomKey::generate(), RoomKey::generate());
    m.update_room(&room(Some(&key.kid))).await;
    let mut s = sub(None, Some(key.wrap_for(&me).unwrap()));
    s.suggested_old_room_keys = Some(vec![OldRoomKey {
        e2e_key_id: old.kid.clone(),
        ts: None,
        e2e_key: old.wrap_for(&me).unwrap(),
    }]);
    m.update_subscription(&s).await;
    let msg = e2e_msg(
        "o",
        old.encrypt_json(&json!({"msg": "old"})).unwrap().to_json(),
        1,
    );
    assert_eq!(text(&m.decrypt_message(&msg)), "old");
}

#[tokio::test]
async fn accept_error_no_suggested_key_is_harmless() {
    let (m, api, _) = ready_manager().await;
    api.st().accept_error = Some("error-no-suggested-key-available".into());
    let key = RoomKey::generate();
    m.update_room(&room(Some(&key.kid))).await;
    m.update_subscription(&sub(None, Some(key.wrap_for(&my_public()).unwrap())))
        .await;
    assert_eq!(m.room_state(RID), RoomE2eState::WaitingKeys);
    assert!(!api.called("reject"));
}

#[tokio::test]
async fn reset_by_someone_else_keeps_old_messages_readable() {
    let (m, _, _) = ready_manager().await;
    let me = my_public();
    let k1 = RoomKey::generate();
    let w1 = k1.wrap_for(&me).unwrap();
    m.update_room(&room(Some(&k1.kid))).await;
    m.update_subscription(&sub(Some(w1.clone()), None)).await;
    assert_eq!(m.room_state(RID), RoomE2eState::Ready);
    let before = e2e_msg(
        "b",
        k1.encrypt_json(&json!({"msg": "before"}))
            .unwrap()
            .to_json(),
        1,
    );

    let k2 = RoomKey::generate();
    m.update_room(&room(Some(&k2.kid))).await;
    assert_eq!(m.room_state(RID), RoomE2eState::WaitingKeys);
    // The stale subscription (still carrying the old E2EKey) must not revive k1.
    m.update_subscription(&sub(Some(w1), None)).await;
    assert_eq!(m.room_state(RID), RoomE2eState::WaitingKeys);
    assert_eq!(text(&m.decrypt_message(&before)), "before");
    m.update_subscription(&sub(None, Some(k2.wrap_for(&me).unwrap())))
        .await;
    assert_eq!(m.room_state(RID), RoomE2eState::Ready);
    assert_eq!(
        m.encrypt_outgoing(RID, "x").unwrap().content["kid"],
        k2.kid.as_str()
    );
    assert_eq!(text(&m.decrypt_message(&before)), "before");
}

#[tokio::test]
async fn reset_room_key_as_initiator() {
    let (m, api, _) = ready_manager().await;
    assert!(matches!(
        m.reset_room_key(RID).await,
        Err(E2eManagerError::NoRoomKeyId)
    ));
    let k1 = RoomKey::generate();
    m.update_room(&room(Some(&k1.kid))).await;
    m.update_subscription(&sub(Some(k1.wrap_for(&my_public()).unwrap()), None))
        .await;
    let before = e2e_msg(
        "b",
        k1.encrypt_json(&json!({"msg": "before"}))
            .unwrap()
            .to_json(),
        1,
    );
    m.reset_room_key(RID).await.unwrap();
    let (_, wrapped, new_kid) = api.st().resets[0].clone();
    assert!(wrapped.starts_with(&new_kid));
    assert_ne!(new_kid, k1.kid);
    assert_eq!(
        m.encrypt_outgoing(RID, "x").unwrap().content["kid"],
        new_kid.as_str()
    );
    assert_eq!(text(&m.decrypt_message(&before)), "before");
    // The echo of the reset (room with the new kid) is not a reset.
    m.update_room(&room(Some(&new_kid))).await;
    assert_eq!(m.room_state(RID), RoomE2eState::Ready);
}

#[tokio::test]
async fn distribution_provides_current_and_old_keys() {
    let (m, api, _) = ready_manager().await;
    let me = my_public();
    let (key, old) = (RoomKey::generate(), RoomKey::generate());
    let mut r = room(Some(&key.kid));
    r.users_waiting_for_e2e_keys = Some(vec![WaitingUser {
        user_id: OTHER.into(),
        ts: None,
    }]);
    let mut s = sub(Some(key.wrap_for(&me).unwrap()), None);
    s.old_room_keys = Some(vec![OldRoomKey {
        e2e_key_id: old.kid.clone(),
        ts: RcDate::from_millis(1_700_000_000_000),
        e2e_key: old.wrap_for(&me).unwrap(),
    }]);
    m.update_room(&r).await;
    m.update_subscription(&s).await;
    let k = keys();
    api.st().waiting.insert(
        RID.into(),
        vec![UserPublicKeyEntry {
            user_id: OTHER.into(),
            public_key: k.other_public.clone(),
        }],
    );
    assert_eq!(m.run_distribution_once().await.unwrap(), 1);
    let entry = api.st().provided[0][RID][0].clone();
    let other = UserPrivateKey::from_jwk_str(&k.other_private).unwrap();
    assert!(RoomKey::unwrap(&other, &entry.key).unwrap().same_key(&key));
    assert_eq!(entry.old_keys.len(), 1);
    assert_eq!(entry.old_keys[0].e2e_key_id, old.kid);
    assert_eq!(entry.old_keys[0].ts, "2023-11-14T22:13:20.000Z");
    assert!(
        RoomKey::unwrap(&other, &entry.old_keys[0].e2e_key)
            .unwrap()
            .same_key(&old)
    );

    // I am in the waiting list myself: not a candidate.
    r.users_waiting_for_e2e_keys = Some(vec![WaitingUser {
        user_id: ME.into(),
        ts: None,
    }]);
    m.update_room(&r).await;
    assert_eq!(m.run_distribution_once().await.unwrap(), 0);

    // A room key request event distributes immediately.
    let ev = SessionEvent::Unknown {
        collection: "stream-notify-room".into(),
        event_name: format!("{RID}/e2e.keyRequest"),
        args: json!([key.kid]),
    };
    assert!(m.handle_event(&ev).await);
    assert_eq!(api.st().provided.len(), 2);

    // E2E disabled: no distribution, no encryption, decryption still works.
    m.set_settings(E2eSettings::default());
    assert_eq!(m.run_distribution_once().await.unwrap(), 0);
    assert!(matches!(
        m.encrypt_outgoing(RID, "x"),
        Err(E2eManagerError::Disabled)
    ));
    let msg = e2e_msg(
        "z",
        key.encrypt_json(&json!({"msg": "still"}))
            .unwrap()
            .to_json(),
        1,
    );
    assert_eq!(text(&m.decrypt_message(&msg)), "still");
}

#[tokio::test]
async fn decrypt_edge_cases() {
    let (m, _, _) = ready_manager().await;
    let key = RoomKey::generate();
    m.update_room(&room(Some(&key.kid))).await;
    m.update_subscription(&sub(Some(key.wrap_for(&my_public()).unwrap()), None))
        .await;

    let plain = Message {
        id: "p".into(),
        rid: RID.into(),
        msg: "hi".into(),
        ..Default::default()
    };
    assert_eq!(m.decrypt_message(&plain), DecryptResult::NotEncrypted);
    let empty = Message {
        id: "e".into(),
        rid: RID.into(),
        t: Some("e2e".into()),
        ..Default::default()
    };
    assert_eq!(text(&m.decrypt_message(&empty)), "");
    let megolm = e2e_msg(
        "g",
        json!({"algorithm": "m.megolm.v1.aes-sha2", "ciphertext": "x"}),
        1,
    );
    assert_eq!(
        m.decrypt_message(&megolm),
        DecryptResult::Failed(DecryptFailure::Unsupported("m.megolm.v1.aes-sha2".into()))
    );
    let bad = e2e_msg("b", json!({"algorithm": "rc.v2.aes-sha2", "kid": "k"}), 1);
    assert_eq!(
        m.decrypt_message(&bad),
        DecryptResult::Failed(DecryptFailure::Malformed)
    );
    // Whitelist: spoofed fields in the plaintext are ignored; md/e2e from the server too.
    let spoof = key
        .encrypt_json(&json!({"msg": "real", "u": {"_id": "admin"}, "rid": "elsewhere", "t": "uj"}))
        .unwrap()
        .to_json();
    let mut msg = e2e_msg("s", spoof, 5);
    msg.e2e = Some("done".into());
    msg.msg = "leaked plaintext".into();
    let DecryptResult::Decrypted(d) = m.decrypt_message(&msg) else {
        panic!()
    };
    assert_eq!(d.message.text(), "real");
    assert!(d.plaintext_edit);
    // Cached by (id, _updatedAt); an edit (new _updatedAt) is decrypted again.
    let DecryptResult::Decrypted(again) = m.decrypt_message(&msg) else {
        panic!()
    };
    assert!(Arc::ptr_eq(&d, &again));
    let edited = e2e_msg(
        "s",
        key.encrypt_json(&json!({"msg": "edited"}))
            .unwrap()
            .to_json(),
        6,
    );
    assert_eq!(text(&m.decrypt_message(&edited)), "edited");
    // Other room: pending.
    let mut elsewhere = edited.clone();
    elsewhere.rid = "nope".into();
    assert_eq!(m.decrypt_message(&elsewhere), DecryptResult::Pending);

    // Pinned message and notification payloads.
    let content = key
        .encrypt_json(&json!({"msg": "pinned text"}))
        .unwrap()
        .to_json();
    let pinned: Message = serde_json::from_value(json!({
        "_id": "pin", "rid": RID, "t": "message_pinned_e2e", "msg": "",
        "attachments": [{"text": "", "content": content}]
    }))
    .unwrap();
    assert_eq!(text(&m.decrypt_pinned(&pinned)), "pinned text");
    assert_eq!(
        m.decrypt_notification(RID, &json!({"msg": "", "t": "e2e", "content": content}))
            .as_deref(),
        Some("pinned text")
    );
    assert!(m.decrypt_notification(RID, &json!({"msg": "x"})).is_none());
}

#[tokio::test]
async fn encrypted_upload_roundtrip() {
    let (m, _, _) = ready_manager().await;
    let key = RoomKey::generate();
    m.update_room(&room(Some(&key.kid))).await;
    m.update_subscription(&sub(Some(key.wrap_for(&my_public()).unwrap()), None))
        .await;
    let data = b"file bytes \x00\x01\x02 and more".to_vec();
    let up = m
        .prepare_upload(RID, "Отчёт.pdf", "application/pdf", &data)
        .unwrap();
    assert_eq!(
        up.hashed_name,
        "2e2bc3fdfa1c462abcadcd3ca0ac2065699f7aa10076e3f9a71c485224c84e78"
    );
    assert_ne!(&up.encrypted[..], &data[..]);
    let meta = m.decrypt_file_metadata(RID, &up.file_content).unwrap();
    assert_eq!(meta.name, "Отчёт.pdf");
    assert_eq!(meta.type_group, "application");
    let uploaded = UploadedFile {
        id: "f1".into(),
        url: "/file-upload/f1/hash".into(),
    };
    let content = m
        .upload_message_content(RID, &up, &uploaded, "caption", None)
        .unwrap();
    let DecryptResult::Decrypted(d) = m.decrypt_message(&e2e_msg("u", content, 1)) else {
        panic!()
    };
    assert_eq!(d.message.text(), "caption");
    assert_eq!(
        d.message.file.as_ref().unwrap().format.as_deref(),
        Some("PDF")
    );
    let att = &d.message.attachments.as_ref().unwrap()[0];
    assert_eq!(att.title_link.as_deref(), Some("/file-upload/f1/hash"));
    assert_eq!(decrypt_download(att, &up.encrypted).unwrap(), data);
    let mut tampered = up.encrypted.to_vec();
    tampered[0] ^= 1;
    assert!(decrypt_download(att, &tampered).is_err());
    // Images get image_* fields.
    let img = m.prepare_upload(RID, "a.png", "image/png", b"png").unwrap();
    let p = upload_payload(&img, &uploaded, "", Some((3, 4)));
    let a = &p.attachments.unwrap()[0];
    assert_eq!(a.image_url.as_deref(), Some("/file-upload/f1/hash"));
    assert_eq!(a.image_dimensions.unwrap().height, 4);
}

#[tokio::test]
async fn change_passphrase_and_reset() {
    let (m, api, store) = ready_manager().await;
    assert!(matches!(
        m.change_passphrase("short").await,
        Err(E2eManagerError::Crypto(
            rocketchat::e2e::E2eError::PasswordPolicy(_)
        ))
    ));
    let new = "Correct-Horse-Battery-Staple-2026!";
    m.change_passphrase(new).await.unwrap();
    let (public, envelope, force) = api.st().uploaded.last().cloned().unwrap();
    assert!(force);
    assert_eq!(UserPublicKey::from_jwk_str(&public).unwrap(), my_public());
    let jwk = PrivateKeyEnvelope::parse(&envelope, ME)
        .unwrap()
        .decrypt(new)
        .unwrap();
    assert_eq!(
        UserPrivateKey::from_jwk_str(&jwk).unwrap().public_key(),
        my_public()
    );

    m.reset_own_keys(None).await.unwrap();
    assert!(api.called("reset_own_key:false"));
    assert_eq!(m.account_state(), AccountE2eState::Disabled);
    assert!(
        store
            .get(&private_key_secret(&ServerId("srv".into()), ME))
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn events_update_settings_and_force_logout() {
    let (m, _, store) = ready_manager().await;
    let setting = Setting {
        id: "E2E_Enable".into(),
        value: json!(false),
        ..Default::default()
    };
    assert!(
        m.handle_event(&SessionEvent::SettingChanged {
            kind: ChangeKind::Updated,
            setting: Box::new(setting),
            public: true,
        })
        .await
    );
    assert!(!m.settings().enabled);
    let key = RoomKey::generate();
    let ev = SessionEvent::RoomChanged {
        kind: ChangeKind::Updated,
        room: Box::new(room(Some(&key.kid))),
    };
    assert!(m.handle_event(&ev).await);
    assert_eq!(m.room_state(RID), RoomE2eState::WaitingKeys);
    // `force_logout` may be meant for another session: ignored here.
    assert!(
        !m.handle_event(&SessionEvent::ForceLogout {
            session_id: Some("other".into())
        })
        .await
    );
    assert_eq!(m.room_state(RID), RoomE2eState::WaitingKeys);
    m.forced_logout().await;
    assert_eq!(m.account_state(), AccountE2eState::Disabled);
    assert!(
        store
            .get(&private_key_secret(&ServerId("srv".into()), ME))
            .unwrap()
            .is_none()
    );
    assert_eq!(m.room_state(RID), RoomE2eState::NotStarted);
}

#[tokio::test]
async fn public_rooms_and_unencrypted_rooms_are_disabled() {
    let (m, api, _) = ready_manager().await;
    let mut r = room(None);
    r.t = RoomType::Channel;
    m.update_room(&r).await;
    assert_eq!(m.room_state(RID), RoomE2eState::Disabled);
    r.t = RoomType::Private;
    r.encrypted = Some(false);
    m.update_room(&r).await;
    assert_eq!(m.room_state(RID), RoomE2eState::Disabled);
    assert!(!api.called("set_room_key_id"));
    m.toggle_room(RID, true).await.unwrap();
    assert!(api.called("set_room_encrypted:true"));
}

#[test]
fn settings_from_store() {
    let mut s = crate::settings::Settings::new();
    s.set_value("E2E_Enable", json!(true));
    s.set_value("E2E_Enabled_Mentions", json!(false));
    s.set_value("UTF8_User_Names_Validation", json!("[a-z]+"));
    let e = E2eSettings::from_settings(&s);
    assert!(e.enabled && !e.mentions && e.encrypt_files && !e.allow_unencrypted);
    assert_eq!(e.username_pattern, "[a-z]+");
    let mut e = E2eSettings::default();
    assert!(e.apply("E2E_Allow_Unencrypted_Messages", &json!(true)));
    assert!(e.allow_unencrypted);
    assert!(!e.apply("Site_Url", &json!("x")));
}

fn enc(k: &RoomKey, t: &str) -> Value {
    k.encrypt_json(&json!({"msg": t})).unwrap().to_json()
}

/// Reset from my other device: the server sends my subscription (new `E2EKey`)
/// before the room with the new `e2eKeyId` (resetRoomKey.ts).
#[tokio::test]
async fn reset_by_my_other_device_subscription_first() {
    let (m, _, _) = ready_manager().await;
    let me = my_public();
    let k1 = RoomKey::generate();
    m.update_room(&room(Some(&k1.kid))).await;
    m.update_subscription(&sub(Some(k1.wrap_for(&me).unwrap()), None))
        .await;
    assert_eq!(m.room_state(RID), RoomE2eState::Ready);
    let k2 = RoomKey::generate();
    let mut s = sub(Some(k2.wrap_for(&me).unwrap()), None);
    s.old_room_keys = Some(vec![OldRoomKey {
        e2e_key_id: k1.kid.clone(),
        ts: Some(RcDate::now()),
        e2e_key: k1.wrap_for_with_kid(&me, &k1.kid).unwrap(),
    }]);
    m.update_subscription(&s).await;
    // Gap: never send under the old kid; both keys decrypt.
    if let Ok(out) = m.encrypt_outgoing(RID, "gap") {
        assert_eq!(out.content["kid"], k2.kid.as_str());
    }
    assert_eq!(
        text(&m.decrypt_message(&e2e_msg("a", enc(&k1, "old"), 1))),
        "old"
    );
    assert_eq!(
        text(&m.decrypt_message(&e2e_msg("b", enc(&k2, "new"), 1))),
        "new"
    );
    m.update_room(&room(Some(&k2.kid))).await;
    assert_eq!(m.room_state(RID), RoomE2eState::Ready);
    assert!(m.can_encrypt(RID));
    assert_eq!(
        m.encrypt_outgoing(RID, "x").unwrap().content["kid"],
        k2.kid.as_str()
    );
    assert_eq!(
        text(&m.decrypt_message(&e2e_msg("c", enc(&k1, "old"), 1))),
        "old"
    );
}

#[tokio::test]
async fn own_key_without_prefix_gets_a_kid() {
    let (m, _, _) = ready_manager().await;
    let key = RoomKey::generate();
    let mut r = room(None);
    r.encrypted = Some(true);
    // A legacy room: the room has a kid, the wrapped key has no prefix.
    r.e2e_key_id = Some("legacy-kid".into());
    m.update_room(&r).await;
    m.update_subscription(&sub(
        Some(key.wrap_for_with_kid(&my_public(), "").unwrap()),
        None,
    ))
    .await;
    assert_eq!(
        m.encrypt_outgoing(RID, "x").unwrap().content["kid"],
        "legacy-kid"
    );
}

#[tokio::test]
async fn failed_own_key_store_blocks_encryption_and_retries() {
    let (m, api, _) = ready_manager().await;
    api.st().fail_update_group_key = true;
    api.st().without_key.insert(
        RID.into(),
        vec![UserPublicKeyEntry {
            user_id: OTHER.into(),
            public_key: keys().other_public,
        }],
    );
    m.update_room(&room(None)).await;
    assert!(matches!(m.room_state(RID), RoomE2eState::Error(_)));
    assert!(!m.can_encrypt(RID));
    assert!(m.encrypt_outgoing(RID, "x").is_err());
    assert!(api.st().provided.is_empty());
    let kid = api.st().room_key_ids[RID].clone();
    // The echo of setRoomKeyID retries the store.
    api.st().fail_update_group_key = false;
    m.update_room(&room(Some(&kid))).await;
    assert_eq!(m.room_state(RID), RoomE2eState::Ready);
    assert_eq!(api.st().group_keys.len(), 1);
    assert_eq!(api.st().provided.len(), 1);
    assert_eq!(
        m.encrypt_outgoing(RID, "x").unwrap().content["kid"],
        kid.as_str()
    );
}

#[tokio::test]
async fn room_encryption_turned_off_stops_encrypting() {
    let (m, _, _) = ready_manager().await;
    let key = RoomKey::generate();
    m.update_room(&room(Some(&key.kid))).await;
    m.update_subscription(&sub(Some(key.wrap_for(&my_public()).unwrap()), None))
        .await;
    assert!(m.can_encrypt(RID));
    let mut r = room(Some(&key.kid));
    r.encrypted = Some(false);
    m.update_room(&r).await;
    assert_eq!(m.room_state(RID), RoomE2eState::Disabled);
    assert!(!m.can_encrypt(RID));
    assert!(matches!(
        m.encrypt_outgoing(RID, "x"),
        Err(E2eManagerError::RoomNotEncrypted)
    ));
    assert_eq!(
        text(&m.decrypt_message(&e2e_msg("a", enc(&key, "h"), 1))),
        "h"
    );
    m.update_room(&room(Some(&key.kid))).await;
    assert!(m.can_encrypt(RID));
}

#[tokio::test]
async fn logout_during_start_leaves_no_secrets() {
    let api = MockApi::default();
    let gate = Arc::new(tokio::sync::Notify::new());
    api.st().fetch_gate = Some(gate.clone());
    let store = Arc::new(MemoryStore::new());
    let m = manager(&api, &store);
    let task = tokio::spawn({
        let m = m.clone();
        async move { m.start().await }
    });
    while m.account_state() != AccountE2eState::LoadingKeys {
        tokio::task::yield_now().await;
    }
    m.logout().await;
    gate.notify_one();
    assert!(matches!(
        task.await.unwrap(),
        Err(E2eManagerError::Cancelled)
    ));
    assert_eq!(m.account_state(), AccountE2eState::Disabled);
    let server = ServerId("srv".into());
    assert!(
        store
            .get(&private_key_secret(&server, ME))
            .unwrap()
            .is_none()
    );
    assert!(
        store
            .get(&pending_passphrase_secret(&server, ME))
            .unwrap()
            .is_none()
    );
    assert!(api.st().uploaded.is_empty());
}

struct ReadOnlyStore(MemoryStore);

impl SecretStore for ReadOnlyStore {
    fn get(&self, key: &str) -> crate::secrets::Result<Option<String>> {
        self.0.get(key)
    }
    fn set(&self, _key: &str, _value: &str) -> crate::secrets::Result<()> {
        Err(SecretError::Timeout)
    }
    fn delete(&self, key: &str) -> crate::secrets::Result<()> {
        self.0.delete(key)
    }
}

#[tokio::test]
async fn secret_store_failures_do_not_block_unlock() {
    let k = keys();
    let api = MockApi::default();
    api.st().my_keys = MyKeys {
        public_key: Some(k.my_public.clone()),
        private_key: Some(k.my_envelope.clone()),
    };
    let store: Arc<dyn SecretStore> = Arc::new(ReadOnlyStore(MemoryStore::new()));
    let m = E2eManager::new(
        api.clone(),
        store,
        &ServerId("srv".into()),
        ME,
        enabled(),
        config(),
    );
    m.start().await.unwrap();
    m.submit_password(&k.my_passphrase).await.unwrap();
    assert_eq!(m.account_state(), AccountE2eState::Ready);

    // First-time creation with a broken store still uploads and keeps the
    // passphrase in memory.
    let api = MockApi::default();
    let store: Arc<dyn SecretStore> = Arc::new(ReadOnlyStore(MemoryStore::new()));
    let m = E2eManager::new(
        api.clone(),
        store,
        &ServerId("srv".into()),
        ME,
        enabled(),
        config(),
    );
    m.start().await.unwrap();
    assert_eq!(m.account_state(), AccountE2eState::SavePassword);
    assert!(m.pending_passphrase().is_some());
}

#[tokio::test]
async fn disabled_start_with_cached_key_is_read_only() {
    let k = keys();
    let api = MockApi::default();
    api.st().my_keys = MyKeys {
        public_key: Some(k.my_public.clone()),
        private_key: Some(k.my_envelope.clone()),
    };
    let store = Arc::new(MemoryStore::new());
    store
        .set(
            &private_key_secret(&ServerId("srv".into()), ME),
            &k.my_private,
        )
        .unwrap();
    let m = E2eManager::new(
        api.clone(),
        store,
        &ServerId("srv".into()),
        ME,
        E2eSettings::default(),
        config(),
    );
    m.start().await.unwrap();
    assert_eq!(m.account_state(), AccountE2eState::DisabledReadOnly);
    let key = RoomKey::generate();
    m.update_room(&room(Some(&key.kid))).await;
    m.update_subscription(&sub(Some(key.wrap_for(&my_public()).unwrap()), None))
        .await;
    assert_eq!(
        text(&m.decrypt_message(&e2e_msg("a", enc(&key, "ro"), 1))),
        "ro"
    );
    assert!(!m.can_encrypt(RID));
    // No key creation and no suggestion handling while read-only.
    m.update_room(&Room {
        id: "r2".into(),
        ..room(None)
    })
    .await;
    m.update_subscription(&sub(None, Some(key.wrap_for(&my_public()).unwrap())))
        .await;
    assert!(!api.called("set_room_key_id"));
    assert!(!api.called("accept") && !api.called("reject"));
}

#[tokio::test]
async fn v1_ciphertext_in_msg_is_not_a_plaintext_edit() {
    let (m, _, _) = ready_manager().await;
    let key = RoomKey::generate();
    m.update_room(&room(Some(&key.kid))).await;
    m.update_subscription(&sub(Some(key.wrap_for(&my_public()).unwrap()), None))
        .await;
    let content = json!({"algorithm": "rc.v1.aes-sha2", "ciphertext": "AAAAAAAAAAAA"});
    let mut msg = e2e_msg("v1", content, 1);
    msg.msg = "AAAAAAAAAAAA".into();
    let r = m.decrypt_message(&msg);
    if let DecryptResult::Decrypted(d) = r {
        assert!(!d.plaintext_edit);
    }
    // The real v1 fixture (same ciphertext in msg and content).
    let mut v1 = e2e_msg("v1b", enc(&key, "v"), 2);
    v1.msg = v1.content.as_ref().unwrap()["ciphertext"]
        .as_str()
        .unwrap()
        .to_owned();
    let DecryptResult::Decrypted(d) = m.decrypt_message(&v1) else {
        panic!()
    };
    assert!(!d.plaintext_edit);
}

async fn upload_room() -> (E2eManager<MockApi>, PreparedUpload, UploadedFile, Vec<u8>) {
    let (m, _, _) = ready_manager().await;
    let key = RoomKey::generate();
    m.update_room(&room(Some(&key.kid))).await;
    m.update_subscription(&sub(Some(key.wrap_for(&my_public()).unwrap()), None))
        .await;
    let data: Vec<u8> = (0..200_000u32).map(|i| (i % 251) as u8).collect();
    let up = m
        .prepare_upload_async(
            RID,
            "big.bin".into(),
            "application/octet-stream".into(),
            data.clone(),
        )
        .await
        .unwrap();
    let uploaded = UploadedFile {
        id: "f1".into(),
        url: "/file-upload/f1/big.bin".into(),
    };
    (m, up, uploaded, data)
}

#[tokio::test]
async fn removed_files_replace_decrypted_attachments() {
    let (m, up, uploaded, _) = upload_room().await;
    let content = m
        .upload_message_content(RID, &up, &uploaded, "cap", None)
        .unwrap();
    let mut msg = e2e_msg("rf", content, 1);
    msg.attachments = Some(vec![MessageAttachment {
        kind: Some("removed-file".into()),
        file_id: Some("f1".into()),
        text: Some("_File removed_".into()),
        ..Default::default()
    }]);
    let DecryptResult::Decrypted(d) = m.decrypt_message(&msg) else {
        panic!()
    };
    let att = &d.message.attachments.as_ref().unwrap()[0];
    assert_eq!(att.kind.as_deref(), Some("removed-file"));
    assert!(att.title_link.is_none() && att.encryption.is_none());
    assert!(d.message.files.as_ref().unwrap().is_empty());
    assert!(d.message.file.is_none());
    assert_eq!(d.message.text(), "cap");
    // A placeholder without fileId removes every file attachment.
    let mut msg2 = msg.clone();
    msg2.id = "rf2".into();
    msg2.attachments.as_mut().unwrap()[0].file_id = None;
    let DecryptResult::Decrypted(d) = m.decrypt_message(&msg2) else {
        panic!()
    };
    assert_eq!(
        d.message.attachments.as_ref().unwrap()[0].kind.as_deref(),
        Some("removed-file")
    );
}

#[tokio::test]
async fn editing_an_upload_keeps_the_file_key() {
    let (m, up, uploaded, data) = upload_room().await;
    let content = m
        .upload_message_content(RID, &up, &uploaded, "cap", None)
        .unwrap();
    let DecryptResult::Decrypted(orig) = m.decrypt_message(&e2e_msg("u", content, 1)) else {
        panic!()
    };
    let edit = m
        .encrypt_edit(RID, "new caption", Some(&orig.message))
        .unwrap();
    let DecryptResult::Decrypted(d) = m.decrypt_message(&e2e_msg("u", edit.content, 2)) else {
        panic!()
    };
    assert_eq!(d.message.text(), "new caption");
    assert_eq!(d.message.file.as_ref().unwrap().id, "f1");
    let att = d.message.attachments.as_ref().unwrap()[0].clone();
    assert_eq!(
        decrypt_download_async(att, up.encrypted.to_vec())
            .await
            .unwrap(),
        data
    );
}

#[tokio::test]
async fn key_requests_skip_rooms_without_key_and_are_debounced() {
    let (m, api, _) = ready_manager().await;
    let ev = |rid: &str| SessionEvent::Unknown {
        collection: "stream-notify-room".into(),
        event_name: format!("{rid}/e2e.keyRequest"),
        args: json!(["kid"]),
    };
    let key = RoomKey::generate();
    m.update_room(&room(Some(&key.kid))).await;
    assert!(m.handle_event(&ev(RID)).await);
    assert_eq!(api.st().waiting_calls, 0, "no key: no REST call");
    m.update_subscription(&sub(Some(key.wrap_for(&my_public()).unwrap()), None))
        .await;
    assert!(m.handle_event(&ev(RID)).await);
    assert!(m.handle_event(&ev(RID)).await);
    assert_eq!(api.st().waiting_calls, 1);
}

/// A store whose reads of keys containing `failing` fail while `fail` is set.
struct FlakyStore {
    inner: MemoryStore,
    failing: &'static str,
    fail: std::sync::atomic::AtomicBool,
}

impl SecretStore for FlakyStore {
    fn get(&self, key: &str) -> crate::secrets::Result<Option<String>> {
        if key.contains(self.failing) && self.fail.load(std::sync::atomic::Ordering::SeqCst) {
            return Err(SecretError::Timeout);
        }
        self.inner.get(key)
    }
    fn set(&self, key: &str, value: &str) -> crate::secrets::Result<()> {
        self.inner.set(key, value)
    }
    fn delete(&self, key: &str) -> crate::secrets::Result<()> {
        self.inner.delete(key)
    }
}

#[tokio::test]
async fn secret_store_read_errors_delete_nothing() {
    let k = keys();
    let server = ServerId("srv".into());
    for failing in ["private_key", "pending_passphrase"] {
        let api = MockApi::default();
        api.st().my_keys = MyKeys {
            public_key: Some(k.my_public.clone()),
            private_key: Some(k.my_envelope.clone()),
        };
        let store = Arc::new(FlakyStore {
            inner: MemoryStore::new(),
            failing,
            fail: true.into(),
        });
        store
            .inner
            .set(&private_key_secret(&server, ME), &k.my_private)
            .unwrap();
        store
            .inner
            .set(&pending_passphrase_secret(&server, ME), "unsaved words")
            .unwrap();
        let m = E2eManager::new(api.clone(), store.clone(), &server, ME, enabled(), config());
        assert!(m.start().await.is_err(), "{failing}");
        assert!(matches!(m.account_state(), AccountE2eState::Error(_)));
        assert!(
            store
                .inner
                .get(&private_key_secret(&server, ME))
                .unwrap()
                .is_some()
        );
        assert!(
            store
                .inner
                .get(&pending_passphrase_secret(&server, ME))
                .unwrap()
                .is_some()
        );
        // The keyring answers again: the cached key and the passphrase are back.
        store.fail.store(false, std::sync::atomic::Ordering::SeqCst);
        m.start().await.unwrap();
        assert_eq!(m.account_state(), AccountE2eState::SavePassword);
        assert_eq!(
            m.pending_passphrase().as_deref().map(String::as_str),
            Some("unsaved words")
        );
    }
}

#[tokio::test]
async fn forced_logout_keeps_an_unsaved_passphrase_that_unlocks_again() {
    // First use: keys created, backup encrypted with a generated passphrase.
    let api = MockApi::default();
    let store = Arc::new(MemoryStore::new());
    let m = manager(&api, &store);
    m.start().await.unwrap();
    assert_eq!(m.account_state(), AccountE2eState::SavePassword);
    let passphrase = m.pending_passphrase().unwrap();
    let (public, envelope, _) = api.st().uploaded[0].clone();
    api.st().my_keys = MyKeys {
        public_key: Some(public),
        private_key: Some(envelope),
    };
    // `force_logout` (no chance to warn): the cached key goes, the
    // passphrase stays.
    m.forced_logout().await;
    let server = ServerId("srv".into());
    assert!(
        store
            .get(&private_key_secret(&server, ME))
            .unwrap()
            .is_none()
    );
    assert!(
        store
            .get(&pending_passphrase_secret(&server, ME))
            .unwrap()
            .is_some()
    );
    // Next login: the backup is unlocked with it and the user is asked to
    // save it again.
    let m = manager(&api, &store);
    m.start().await.unwrap();
    assert_eq!(m.account_state(), AccountE2eState::SavePassword);
    assert_eq!(m.pending_passphrase().unwrap(), passphrase);
    assert!(
        store
            .get(&private_key_secret(&server, ME))
            .unwrap()
            .is_some()
    );
    // An explicit logout deletes both.
    m.logout().await;
    assert!(
        store
            .get(&pending_passphrase_secret(&server, ME))
            .unwrap()
            .is_none()
    );
}
