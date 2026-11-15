//! Settings, session events and per-room key handling (handshake, suggested,
//! created and reset room keys).

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Instant;

use rocketchat::e2e::{RoomE2eKeyRequest, RoomKey, UserPrivateKey, new_key_id};
use rocketchat::models::{OldRoomKey, Room, Subscription};
use rocketchat::rest::SetKeyIdOutcome;
use rocketchat::session::{ChangeKind, SessionEvent};

use super::room::{OldKey, RoomCrypto, prefix_kid};
use super::{
    E2eApi, E2eEvent, E2eManager, E2eManagerError, E2eSettings, KEY_REQUEST_DEBOUNCE, Result,
    RoomE2eState, RoomKeySet, RoomSummary, blocking, lock, read, room_state_name, wrap_for_users,
    write,
};

impl<A: E2eApi> E2eManager<A> {
    // ----- settings and events ------------------------------------------------

    /// Replaces the settings. Turning `E2E_Enable` off keeps decrypting with the
    /// loaded keys but refuses to encrypt and pauses distribution.
    pub fn set_settings(&self, settings: E2eSettings) {
        *write(&self.inner.settings) = settings;
    }

    /// Applies a realtime event (rooms, subscriptions, settings, room key
    /// requests). Returns `true` if it was relevant. `force_logout` is not
    /// handled here: it may name another session, which only the caller can
    /// tell; it calls [`Self::forced_logout`] when the session was logged out.
    pub async fn handle_event(&self, ev: &SessionEvent) -> bool {
        match ev {
            SessionEvent::RoomChanged { kind, room } => {
                if *kind == ChangeKind::Removed {
                    self.forget_room(&room.id);
                } else {
                    self.update_room(room).await;
                }
                true
            }
            SessionEvent::SubscriptionChanged { kind, sub } => {
                if *kind == ChangeKind::Removed {
                    self.forget_room(&sub.rid);
                } else {
                    self.update_subscription(sub).await;
                }
                true
            }
            SessionEvent::SettingChanged { setting, .. } => {
                write(&self.inner.settings).apply(&setting.id, &setting.value)
            }
            other => match RoomE2eKeyRequest::from_event(other) {
                Some(req) => {
                    if !self.should_answer_key_request(&req.rid) {
                        return true;
                    }
                    if let Err(e) = self.distribute_rooms(vec![req.rid]).await {
                        tracing::debug!(error = %e, "key distribution after a request failed");
                    }
                    true
                }
                None => false,
            },
        }
    }

    /// Only rooms whose key this client holds, at most once per
    /// [`KEY_REQUEST_DEBOUNCE`] (the server emits a request on every join).
    fn should_answer_key_request(&self, rid: &str) -> bool {
        let ready = read(&self.inner.summaries)
            .get(rid)
            .is_some_and(|s| s.has_key && s.state == RoomE2eState::Ready);
        if !ready {
            return false;
        }
        let now = Instant::now();
        let mut last = lock(&self.inner.key_requests);
        if last
            .get(rid)
            .is_some_and(|t| now.duration_since(*t) < KEY_REQUEST_DEBOUNCE)
        {
            return false;
        }
        last.insert(rid.to_owned(), now);
        true
    }

    fn forget_room(&self, rid: &str) {
        lock(&self.inner.rooms).remove(rid);
        write(&self.inner.summaries).remove(rid);
        write(&self.inner.keys).remove(rid);
    }

    pub(super) fn room_cell(&self, rid: &str) -> Arc<tokio::sync::Mutex<RoomCrypto>> {
        lock(&self.inner.rooms)
            .entry(rid.to_owned())
            .or_insert_with(|| {
                let id = self.inner.next_cell.fetch_add(1, Ordering::SeqCst) + 1;
                let rc = RoomCrypto {
                    cell_id: id,
                    ..Default::default()
                };
                (id, Arc::new(tokio::sync::Mutex::new(rc)))
            })
            .1
            .clone()
    }

    /// `rc` is still the manager's cell for `rid` (not dropped by a logout,
    /// `stop` or room removal while it was being worked on).
    pub(super) fn attached(&self, rid: &str, rc: &RoomCrypto) -> bool {
        lock(&self.inner.rooms)
            .get(rid)
            .is_some_and(|(id, _)| *id == rc.cell_id)
    }

    /// Records rooms and subscriptions without handshaking (initial load before
    /// [`Self::start`]).
    pub async fn load(&self, rooms: &[Room], subscriptions: &[Subscription]) {
        for r in rooms {
            let cell = self.room_cell(&r.id);
            let mut rc = cell.lock().await;
            rc.apply_room(r);
            self.publish(&r.id, &rc);
        }
        for s in subscriptions {
            let cell = self.room_cell(&s.rid);
            let mut rc = cell.lock().await;
            rc.apply_subscription(s);
            self.publish(&s.rid, &rc);
        }
    }

    /// A room arrived or changed (`rooms.get`, `rooms-changed`).
    pub async fn update_room(&self, room: &Room) {
        let cell = self.room_cell(&room.id);
        let mut rc = cell.lock().await;
        let fx = rc.apply_room(room);
        if fx.reset {
            tracing::debug!(rid = %room.id, kid = ?rc.info.e2e_key_id, "room key reset detected");
        }
        self.handshake(&room.id, &mut rc).await;
    }

    /// A subscription arrived or changed (`subscriptions.get`,
    /// `subscriptions-changed`).
    pub async fn update_subscription(&self, sub: &Subscription) {
        let cell = self.room_cell(&sub.rid);
        let mut rc = cell.lock().await;
        rc.apply_subscription(sub);
        self.handshake(&sub.rid, &mut rc).await;
    }

    pub(super) async fn handshake_all(&self) {
        let rids: Vec<String> = lock(&self.inner.rooms).keys().cloned().collect();
        for rid in rids {
            let cell = self.room_cell(&rid);
            let mut rc = cell.lock().await;
            self.handshake(&rid, &mut rc).await;
        }
    }

    /// Publishes the room's state and keys for the synchronous getters.
    fn publish(&self, rid: &str, rc: &RoomCrypto) {
        if !self.attached(rid, rc) {
            return;
        }
        let set = RoomKeySet {
            current: rc.current.clone(),
            old: rc
                .old_keys()
                .into_iter()
                .filter_map(|o| o.key.clone())
                .collect(),
        };
        let kids = |s: &RoomKeySet| -> Vec<String> {
            s.current
                .iter()
                .chain(s.old.iter())
                .map(|k| k.kid.clone())
                .collect()
        };
        let keys_changed = {
            let mut keys = write(&self.inner.keys);
            let before = keys.get(rid).map(|s| kids(s)).unwrap_or_default();
            let changed = before != kids(&set)
                || match (
                    keys.get(rid).and_then(|s| s.current.as_ref()),
                    set.current.as_ref(),
                ) {
                    (Some(a), Some(b)) => !a.same_key(b),
                    _ => false,
                };
            keys.insert(rid.to_owned(), Arc::new(set.clone()));
            changed
        };
        let state_changed = {
            let mut sums = write(&self.inner.summaries);
            let entry = sums.entry(rid.to_owned()).or_default();
            let changed = entry.state != rc.state;
            *entry = RoomSummary {
                state: rc.state.clone(),
                waiting: rc.info.waiting.clone(),
                has_key: set.current.is_some(),
                encrypted: rc.info.encrypted(),
            };
            changed
        };
        if state_changed {
            tracing::debug!(%rid, state = ?room_state_name(&rc.state), "e2e room state");
            self.emit(E2eEvent::RoomState {
                rid: rid.to_owned(),
                state: rc.state.clone(),
            });
        }
        if keys_changed && (set.current.is_some() || !set.old.is_empty()) {
            let pending = lock(&self.inner.cache).take_pending(rid);
            self.emit(E2eEvent::KeysChanged {
                rid: rid.to_owned(),
                pending,
            });
        }
    }

    fn set_room_state(&self, rid: &str, rc: &mut RoomCrypto, state: RoomE2eState) {
        rc.state = state;
        self.publish(rid, rc);
    }

    /// §7.2 handshake, re-run after every update. Acts only when the account
    /// has keys; creating, accepting and rejecting keys only when it is writable
    /// (E2E on, not read-only).
    async fn handshake(&self, rid: &str, rc: &mut RoomCrypto) {
        let Some(private) = self.private_key() else {
            self.publish(rid, rc);
            return;
        };
        if !self.attached(rid, rc) || (!rc.info.have_room && !rc.info.have_sub) {
            return;
        }
        if !rc.info.encryptable_type() {
            self.set_room_state(rid, rc, RoomE2eState::Disabled);
            return;
        }
        self.refresh_old_keys(rid, rc, &private).await;
        self.load_own_key(rid, rc, &private).await;
        if !self.attached(rid, rc) {
            return;
        }
        let writable = self.writable();

        if writable
            && let Some(suggested) = rc.info.sub_suggested.clone()
            && rc.rejected_suggested.as_ref() != Some(&suggested)
        {
            self.process_suggested(rid, rc, &private, suggested).await;
        }

        if let Some((pending, _)) = &rc.pending_created
            && (rc.current.is_some() || rc.info.e2e_key_id.as_ref() != Some(&pending.kid))
        {
            rc.pending_created = None;
        }
        if writable
            && rc.info.encrypted()
            && let Some((key, wrapped)) = rc.pending_created.take()
        {
            self.store_created_key(rid, rc, key, wrapped).await;
            return;
        }

        let next = if !rc.info.encrypted() {
            // Keys stay loaded for decrypting the history; nothing is encrypted.
            RoomE2eState::Disabled
        } else if rc.current.is_some() {
            RoomE2eState::Ready
        } else if writable && rc.info.have_room && rc.info.e2e_key_id.is_none() {
            self.create_room_key(rid, rc).await;
            return;
        } else {
            RoomE2eState::WaitingKeys
        };
        self.set_room_state(rid, rc, next);
    }

    /// Unwraps the subscription's `E2EKey` into the current key. Its kid is the
    /// wrapped prefix (the room's `e2eKeyId` for legacy keys without one, else a
    /// random id like the web). A prefix that differs from the room's kid is a
    /// reset whose subscription update came first: the key is kept as an old key
    /// until the room update arrives.
    async fn load_own_key(&self, rid: &str, rc: &mut RoomCrypto, private: &Arc<UserPrivateKey>) {
        if rc.current.is_some() {
            return;
        }
        let Some(wrapped) = rc.info.sub_e2e_key.clone() else {
            return;
        };
        if rc.stale_e2e_key.as_ref() == Some(&wrapped)
            || rc.failed_e2e_key.as_ref() == Some(&wrapped)
        {
            return;
        }
        let prefix = prefix_kid(&wrapped).unwrap_or_default();
        let room_kid = rc.info.e2e_key_id.clone();
        let mismatch = !prefix.is_empty() && room_kid.as_ref().is_some_and(|k| *k != prefix);
        if mismatch && rc.deferred_e2e_key.as_ref() == Some(&wrapped) {
            return;
        }
        if rc.state != RoomE2eState::Ready {
            self.set_room_state(rid, rc, RoomE2eState::Establishing);
        }
        let (p, w) = (private.clone(), wrapped.clone());
        match blocking(move || RoomKey::unwrap(&p, &w)).await {
            Ok(Ok(key)) if mismatch => {
                tracing::debug!(%rid, kid = %prefix, "own room key does not match the room key id yet");
                rc.push_local_old(key);
                rc.deferred_e2e_key = Some(wrapped);
            }
            Ok(Ok(key)) => {
                let kid = if prefix.is_empty() {
                    room_kid.unwrap_or_else(new_key_id)
                } else {
                    prefix
                };
                rc.current = Some(key.with_kid(kid));
                rc.current_source = Some(wrapped);
                rc.deferred_e2e_key = None;
            }
            Ok(Err(e)) => {
                tracing::warn!(%rid, error = %e, "cannot decrypt the own room key; waiting for a new one");
                rc.failed_e2e_key = Some(wrapped);
            }
            Err(e) => tracing::warn!(%rid, error = %e, "room key task failed"),
        }
    }

    /// Re-derives old keys when `oldRoomKeys` changed.
    async fn refresh_old_keys(
        &self,
        rid: &str,
        rc: &mut RoomCrypto,
        private: &Arc<UserPrivateKey>,
    ) {
        let fp = rc.old_fingerprint();
        if fp == rc.server_old_fingerprint {
            return;
        }
        let entries: Vec<OldRoomKey> = rc.info.old_room_keys.clone().unwrap_or_default();
        let p = private.clone();
        let derived = blocking(move || {
            entries
                .into_iter()
                .map(|o| OldKey {
                    key: RoomKey::unwrap(&p, &o.e2e_key)
                        .ok()
                        .map(|k| k.with_kid(o.e2e_key_id.clone())),
                    kid: o.e2e_key_id,
                    ts: o.ts,
                })
                .collect::<Vec<_>>()
        })
        .await;
        match derived {
            Ok(old) => {
                let failed = old.iter().filter(|o| o.key.is_none()).count();
                if failed > 0 {
                    tracing::debug!(%rid, failed, "some old room keys cannot be decrypted");
                }
                rc.server_old = old;
                rc.server_old_fingerprint = fp;
            }
            Err(e) => tracing::warn!(%rid, error = %e, "old room key task failed"),
        }
    }

    /// Accepts a suggested key that decrypts and matches the room's key id,
    /// otherwise rejects it (§7.2, §11.1, §11.2).
    async fn process_suggested(
        &self,
        rid: &str,
        rc: &mut RoomCrypto,
        private: &Arc<UserPrivateKey>,
        suggested: String,
    ) {
        let (p, s) = (private.clone(), suggested.clone());
        let unwrapped = match blocking(move || RoomKey::unwrap(&p, &s)).await {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!(%rid, error = %e, "suggested key task failed");
                return;
            }
        };
        let accept = match &unwrapped {
            Err(_) => {
                tracing::debug!(%rid, "suggested room key does not decrypt");
                None
            }
            Ok(key) => {
                if rc.info.e2e_key_id.as_ref().is_some_and(|k| *k != key.kid) {
                    tracing::debug!(%rid, kid = %key.kid, "suggested room key has a stale kid");
                    None
                } else if rc
                    .current
                    .as_ref()
                    .is_some_and(|c| c.kid == key.kid && !c.same_key(key))
                {
                    tracing::warn!(%rid, kid = %key.kid, "suggested room key differs from the working key");
                    None
                } else {
                    Some(key.clone())
                }
            }
        };
        if !self.attached(rid, rc) {
            return;
        }
        match accept {
            Some(key) => match self.inner.api.accept_suggested_group_key(rid).await {
                Ok(()) => {
                    tracing::debug!(%rid, kid = %key.kid, "accepted suggested room key");
                    rc.current = Some(key);
                    rc.current_source = Some(suggested.clone());
                    rc.info.sub_e2e_key = Some(suggested);
                    rc.info.sub_suggested = None;
                    rc.stale_e2e_key = None;
                    if rc.info.old_room_keys.is_none() && rc.info.suggested_old.is_some() {
                        rc.info.old_room_keys = rc.info.suggested_old.take();
                        self.refresh_old_keys(rid, rc, private).await;
                    }
                    if rc.state == RoomE2eState::WaitingKeys {
                        self.set_room_state(rid, rc, RoomE2eState::KeysReceived);
                    }
                }
                Err(e) if e.api_error_code() == Some("error-no-suggested-key-available") => {
                    rc.info.sub_suggested = None;
                }
                Err(e) => tracing::debug!(%rid, error = %e, "accepting the suggested key failed"),
            },
            None => {
                rc.rejected_suggested = Some(suggested);
                rc.info.sub_suggested = None;
                if let Err(e) = self.inner.api.reject_suggested_group_key(rid).await {
                    tracing::debug!(%rid, error = %e, "rejecting the suggested key failed");
                }
            }
        }
    }

    /// First room key (§7.3): claim the kid, then publish wrapped keys.
    async fn create_room_key(&self, rid: &str, rc: &mut RoomCrypto) {
        let Some(public) = self.public_key() else {
            return;
        };
        self.set_room_state(rid, rc, RoomE2eState::CreatingKeys);
        let key = RoomKey::generate();
        let wrapped = match key.wrap_for(&public) {
            Ok(w) => w,
            Err(e) => {
                self.set_room_state(rid, rc, RoomE2eState::Error(e.to_string()));
                return;
            }
        };
        if !self.attached(rid, rc) {
            return;
        }
        match self.inner.api.set_room_key_id(rid, &key.kid).await {
            Ok(SetKeyIdOutcome::Set) => {}
            Ok(SetKeyIdOutcome::AlreadyExists) => {
                tracing::debug!(%rid, "another member created the room key");
                self.set_room_state(rid, rc, RoomE2eState::WaitingKeys);
                return;
            }
            Err(e) => {
                self.set_room_state(rid, rc, RoomE2eState::Error(e.to_string()));
                return;
            }
        }
        rc.info.e2e_key_id = Some(key.kid.clone());
        self.store_created_key(rid, rc, key, wrapped).await;
    }

    /// Stores a created key in the own subscription, then makes it current and
    /// provides it to the other members. Until the own copy is stored the room
    /// stays in `Error` and nothing is encrypted (a key only in RAM would make
    /// every message unreadable after a restart); the next handshake retries.
    async fn store_created_key(
        &self,
        rid: &str,
        rc: &mut RoomCrypto,
        key: RoomKey,
        wrapped: String,
    ) {
        if !self.attached(rid, rc) {
            return;
        }
        let me = self.inner.user_id.clone();
        if let Err(e) = self.inner.api.update_group_key(rid, &me, &wrapped).await {
            tracing::warn!(%rid, error = %e, "storing the own room key failed");
            rc.pending_created = Some((key, wrapped));
            self.set_room_state(rid, rc, RoomE2eState::Error(e.to_string()));
            return;
        }
        rc.pending_created = None;
        rc.current_source = Some(wrapped);
        rc.current = Some(key.clone());
        self.set_room_state(rid, rc, RoomE2eState::Ready);
        if !self.attached(rid, rc) {
            return;
        }
        match self.inner.api.users_of_room_without_key(rid).await {
            Ok(users) => {
                let users: Vec<_> = users.into_iter().filter(|u| u.user_id != me).collect();
                let entries = wrap_for_users(key, Vec::new(), users).await;
                if !entries.is_empty() && self.attached(rid, rc) {
                    let map = HashMap::from([(rid.to_owned(), entries)]);
                    if let Err(e) = self
                        .inner
                        .api
                        .provide_users_suggested_group_keys(&map)
                        .await
                    {
                        tracing::debug!(%rid, error = %e, "providing room keys failed");
                    }
                }
            }
            Err(e) => tracing::debug!(%rid, error = %e, "listing members without key failed"),
        }
    }

    // ----- room operations ------------------------------------------------------

    /// `rooms.saveRoomSettings {encrypted}`.
    pub async fn toggle_room(&self, rid: &str, encrypted: bool) -> Result<()> {
        Ok(self.inner.api.set_room_encrypted(rid, encrypted).await?)
    }

    /// Resets the room key (§7.5): the server call first, then the local switch;
    /// the previous key stays usable as an old key.
    pub async fn reset_room_key(&self, rid: &str) -> Result<()> {
        if !self.settings().enabled {
            return Err(E2eManagerError::Disabled);
        }
        let public = self.public_key().ok_or(E2eManagerError::NotReady)?;
        if !self.account_state().is_ready() {
            return Err(E2eManagerError::NotReady);
        }
        let cell = self.room_cell(rid);
        let mut rc = cell.lock().await;
        if rc.info.e2e_key_id.is_none() {
            return Err(E2eManagerError::NoRoomKeyId);
        }
        let key = RoomKey::generate();
        let wrapped = key.wrap_for(&public)?;
        self.inner
            .api
            .reset_room_key(rid, &wrapped, &key.kid)
            .await?;
        if let Some(old) = rc.current.take() {
            let old_kid = rc
                .info
                .e2e_key_id
                .clone()
                .unwrap_or_else(|| old.kid.clone());
            rc.push_local_old(old.with_kid(old_kid));
        }
        rc.info.e2e_key_id = Some(key.kid.clone());
        rc.info.sub_e2e_key = Some(wrapped.clone());
        rc.current = Some(key);
        rc.current_source = Some(wrapped);
        rc.stale_e2e_key = None;
        rc.deferred_e2e_key = None;
        rc.pending_created = None;
        self.set_room_state(rid, &mut rc, RoomE2eState::Ready);
        Ok(())
    }
}
