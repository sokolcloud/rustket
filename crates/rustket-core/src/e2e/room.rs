//! Per-room E2E state (`docs/e2e.md` §7.2): the last known room/subscription
//! fields, the current room key and old keys. Network steps live in the manager;
//! this module is pure data and decisions.

use rocketchat::RcDate;
use rocketchat::e2e::RoomKey;
use rocketchat::models::{OldRoomKey, Room, RoomType, Subscription};

/// Room crypto state (web `E2ERoomState`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum RoomE2eState {
    #[default]
    NotStarted,
    Establishing,
    CreatingKeys,
    /// Another member must provide the key (or a room key reset is needed).
    WaitingKeys,
    KeysReceived,
    /// The current room key is loaded: messages can be encrypted.
    Ready,
    /// Not an encrypted room (or a type that cannot be encrypted).
    Disabled,
    Error(String),
}

/// What the manager knows about a room from the server.
#[derive(Debug, Clone, Default)]
pub(crate) struct RoomInfo {
    pub have_room: bool,
    pub have_sub: bool,
    pub t: Option<RoomType>,
    pub room_encrypted: bool,
    pub sub_encrypted: bool,
    pub e2e_key_id: Option<String>,
    pub waiting: Option<Vec<String>>,
    pub sub_e2e_key: Option<String>,
    pub sub_suggested: Option<String>,
    pub old_room_keys: Option<Vec<OldRoomKey>>,
    pub suggested_old: Option<Vec<OldRoomKey>>,
}

impl RoomInfo {
    pub fn encrypted(&self) -> bool {
        if self.have_room {
            self.room_encrypted
        } else {
            self.sub_encrypted
        }
    }

    pub fn encryptable_type(&self) -> bool {
        matches!(self.t, Some(RoomType::Private | RoomType::Direct))
    }
}

/// An old room key (from `oldRoomKeys` or kept locally after a reset).
#[derive(Debug, Clone)]
pub(crate) struct OldKey {
    pub kid: String,
    pub ts: Option<RcDate>,
    /// `None` when it could not be unwrapped (user key reset since).
    pub key: Option<RoomKey>,
}

/// Mutable per-room state, guarded by one async mutex per room.
#[derive(Debug, Default)]
pub(crate) struct RoomCrypto {
    pub info: RoomInfo,
    pub state: RoomE2eState,
    pub current: Option<RoomKey>,
    /// The wrapped string `current` came from.
    pub current_source: Option<String>,
    /// Old keys from `oldRoomKeys` (re-derived when the list changes).
    pub server_old: Vec<OldKey>,
    pub server_old_fingerprint: Vec<(String, String)>,
    /// Keys replaced locally (room key reset seen or initiated).
    pub local_old: Vec<OldKey>,
    /// `E2EKey` that was current before a detected reset: never reuse it.
    pub stale_e2e_key: Option<String>,
    /// `E2EKey` that failed to unwrap: don't retry until it changes.
    pub failed_e2e_key: Option<String>,
    /// Suggested key already rejected: don't loop on it.
    pub rejected_suggested: Option<String>,
}

/// What an update changed that needs action.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct UpdateEffects {
    /// The room key was reset by someone: drop the current key.
    pub reset: bool,
}

impl RoomCrypto {
    pub fn apply_room(&mut self, room: &Room) -> UpdateEffects {
        let mut fx = UpdateEffects::default();
        let new_kid = room.e2e_key_id.clone().filter(|k| !k.is_empty());
        let prev_kid = self.info.e2e_key_id.clone();
        if let (Some(new), Some(prev)) = (&new_kid, &prev_kid)
            && new != prev
            && self.current.as_ref().is_none_or(|c| &c.kid != new)
        {
            fx.reset = true;
        }
        self.info.have_room = true;
        self.info.t = Some(room.t);
        self.info.room_encrypted = room.encrypted == Some(true);
        if new_kid.is_some() || !fx.reset {
            self.info.e2e_key_id = new_kid.or(prev_kid);
        }
        self.info.waiting = room
            .users_waiting_for_e2e_keys
            .as_ref()
            .map(|w| w.iter().map(|u| u.user_id.clone()).collect());
        if fx.reset {
            self.on_reset();
        }
        fx
    }

    pub fn apply_subscription(&mut self, sub: &Subscription) {
        self.info.have_sub = true;
        if self.info.t.is_none() {
            self.info.t = Some(sub.t);
        }
        let key = sub.e2e_key.clone().filter(|k| !k.is_empty());
        if key.is_some() && key != self.current_source {
            // A different own key (accepted elsewhere, or after a reset).
            self.current = None;
            self.current_source = None;
        }
        if key != self.info.sub_e2e_key {
            self.failed_e2e_key = None;
        }
        self.info.sub_e2e_key = key;
        let suggested = sub.e2e_suggested_key.clone().filter(|k| !k.is_empty());
        if suggested != self.rejected_suggested {
            self.rejected_suggested = None;
        }
        self.info.sub_suggested = suggested;
        self.info.old_room_keys = sub.old_room_keys.clone();
        if sub.suggested_old_room_keys.is_some() {
            self.info.suggested_old = sub.suggested_old_room_keys.clone();
        }
    }

    /// Someone reset the room key: keep the current key as an old key, wait for
    /// the new one, and never reuse the current `E2EKey` string.
    pub fn on_reset(&mut self) {
        if let Some(cur) = self.current.take() {
            self.push_local_old(cur);
        }
        self.stale_e2e_key = self.current_source.take().or(self.info.sub_e2e_key.clone());
        self.state = RoomE2eState::WaitingKeys;
    }

    pub fn push_local_old(&mut self, key: RoomKey) {
        self.local_old.retain(|o| o.kid != key.kid);
        self.local_old.insert(
            0,
            OldKey {
                kid: key.kid.clone(),
                ts: Some(RcDate::now()),
                key: Some(key),
            },
        );
        self.local_old.truncate(10);
    }

    pub fn old_fingerprint(&self) -> Vec<(String, String)> {
        self.info
            .old_room_keys
            .iter()
            .flatten()
            .map(|o| (o.e2e_key_id.clone(), o.e2e_key.clone()))
            .collect()
    }

    /// All usable old keys, server ones first, without duplicates by kid.
    pub fn old_keys(&self) -> Vec<&OldKey> {
        let mut seen = std::collections::HashSet::new();
        self.server_old
            .iter()
            .chain(self.local_old.iter())
            .filter(|o| o.key.is_some())
            .filter(|o| self.current.as_ref().is_none_or(|c| c.kid != o.kid))
            .filter(|o| seen.insert(o.kid.clone()))
            .collect()
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use rocketchat::models::WaitingUser;

    fn room(kid: Option<&str>) -> Room {
        Room {
            id: "r".into(),
            t: RoomType::Private,
            encrypted: Some(true),
            e2e_key_id: kid.map(Into::into),
            users_waiting_for_e2e_keys: Some(vec![WaitingUser {
                user_id: "u2".into(),
                ts: None,
            }]),
            ..Default::default()
        }
    }

    #[test]
    fn reset_detection() {
        let mut rc = RoomCrypto::default();
        assert_eq!(rc.apply_room(&room(Some("k1"))), UpdateEffects::default());
        assert_eq!(rc.info.waiting.as_deref(), Some(&["u2".to_owned()][..]));
        rc.current = Some(RoomKey::generate().with_kid("k1"));
        rc.current_source = Some("k1wrapped".into());
        assert!(!rc.apply_room(&room(Some("k1"))).reset);
        assert!(rc.apply_room(&room(Some("k2"))).reset);
        assert!(rc.current.is_none());
        assert_eq!(rc.stale_e2e_key.as_deref(), Some("k1wrapped"));
        assert_eq!(rc.state, RoomE2eState::WaitingKeys);
        assert_eq!(rc.old_keys().len(), 1);
        assert_eq!(rc.info.e2e_key_id.as_deref(), Some("k2"));
        // First sight of a kid is not a reset.
        let mut fresh = RoomCrypto::default();
        assert!(!fresh.apply_room(&room(None)).reset);
        assert!(!fresh.apply_room(&room(Some("k"))).reset);
    }

    #[test]
    fn subscription_key_change_drops_current() {
        let mut rc = RoomCrypto {
            current: Some(RoomKey::generate()),
            current_source: Some("a".into()),
            ..Default::default()
        };
        let mut sub = Subscription {
            rid: "r".into(),
            t: RoomType::Direct,
            e2e_key: Some("a".into()),
            ..Default::default()
        };
        rc.apply_subscription(&sub);
        assert!(rc.current.is_some());
        sub.e2e_key = Some("b".into());
        rc.apply_subscription(&sub);
        assert!(rc.current.is_none());
        assert_eq!(rc.info.t, Some(RoomType::Direct));
    }
}
