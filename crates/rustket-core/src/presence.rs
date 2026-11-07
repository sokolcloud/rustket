//! Presence cache: status and status text by user id and by username.

use std::collections::HashMap;

use rocketchat::models::{UserStatus, UserSummary};

#[derive(Debug, Clone, PartialEq, Default)]
pub struct PresenceEntry {
    pub uid: String,
    pub username: Option<String>,
    /// Real name, when known (presence lists and `users.info` carry it).
    pub name: Option<String>,
    pub status: UserStatus,
    pub status_text: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct Presence {
    by_uid: HashMap<String, PresenceEntry>,
    by_username: HashMap<String, String>,
}

impl Presence {
    pub fn new() -> Self {
        Self::default()
    }

    /// Records a status change. `None` fields keep their previous values, except
    /// that an explicit empty `status_text` clears it. Returns whether anything changed.
    pub fn update(
        &mut self,
        uid: &str,
        username: Option<&str>,
        status: UserStatus,
        status_text: Option<&str>,
    ) -> bool {
        let entry = self
            .by_uid
            .entry(uid.to_owned())
            .or_insert_with(|| PresenceEntry {
                uid: uid.to_owned(),
                ..Default::default()
            });
        let before = entry.clone();
        entry.status = status;
        if let Some(t) = status_text {
            entry.status_text = (!t.is_empty()).then(|| t.to_owned());
        }
        if let Some(u) = username.filter(|u| !u.is_empty()) {
            if entry.username.as_deref() != Some(u) {
                if let Some(old) = entry.username.take() {
                    self.by_username.remove(&old);
                }
                entry.username = Some(u.to_owned());
            }
            self.by_username.insert(u.to_owned(), uid.to_owned());
        }
        *entry != before
    }

    pub fn update_from_summary(&mut self, u: &UserSummary) -> bool {
        let changed = self.update(
            &u.id,
            u.username.as_deref(),
            u.status.unwrap_or_default(),
            Some(u.status_text.as_deref().unwrap_or("")),
        );
        let name_changed = match (&u.name, self.by_uid.get_mut(&u.id)) {
            (Some(n), Some(e)) if e.name.as_ref() != Some(n) => {
                e.name = Some(n.clone());
                true
            }
            _ => false,
        };
        changed || name_changed
    }

    /// Records a user's real name without touching status (e.g. from messages).
    pub fn remember_name(&mut self, uid: &str, username: Option<&str>, name: Option<&str>) {
        let Some(e) = self.by_uid.get_mut(uid) else {
            if let Some(un) = username {
                self.by_uid.insert(
                    uid.to_owned(),
                    PresenceEntry {
                        uid: uid.to_owned(),
                        username: Some(un.to_owned()),
                        name: name.map(str::to_owned),
                        status: UserStatus::Offline,
                        status_text: None,
                    },
                );
                self.by_username.insert(un.to_owned(), uid.to_owned());
            }
            return;
        };
        if let Some(n) = name {
            e.name = Some(n.to_owned());
        }
    }

    pub fn get(&self, uid: &str) -> Option<&PresenceEntry> {
        self.by_uid.get(uid)
    }

    pub fn get_by_username(&self, username: &str) -> Option<&PresenceEntry> {
        self.by_uid.get(self.by_username.get(username)?)
    }

    /// Unknown users are offline.
    pub fn status(&self, uid: &str) -> UserStatus {
        self.get(uid)
            .map(|e| e.status)
            .unwrap_or(UserStatus::Offline)
    }

    pub fn status_by_username(&self, username: &str) -> UserStatus {
        self.get_by_username(username)
            .map(|e| e.status)
            .unwrap_or(UserStatus::Offline)
    }

    /// Every user id with an entry.
    pub fn user_ids(&self) -> impl Iterator<Item = &str> + '_ {
        self.by_uid.keys().map(String::as_str)
    }

    pub fn len(&self) -> usize {
        self.by_uid.len()
    }

    pub fn is_empty(&self) -> bool {
        self.by_uid.is_empty()
    }

    pub fn clear(&mut self) {
        self.by_uid.clear();
        self.by_username.clear();
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[test]
    fn update_and_lookup() {
        let mut p = Presence::new();
        assert_eq!(p.status("u1"), UserStatus::Offline);
        assert!(p.update("u1", Some("alice"), UserStatus::Online, Some("hi")));
        assert!(!p.update("u1", Some("alice"), UserStatus::Online, None));
        assert_eq!(p.status_by_username("alice"), UserStatus::Online);
        assert_eq!(p.get("u1").unwrap().status_text.as_deref(), Some("hi"));
        assert!(p.update("u1", None, UserStatus::Busy, Some("")));
        assert_eq!(p.get("u1").unwrap().status_text, None);
        // Username change re-indexes.
        p.update("u1", Some("alice2"), UserStatus::Busy, None);
        assert!(p.get_by_username("alice").is_none());
        assert_eq!(p.status_by_username("alice2"), UserStatus::Busy);
    }

    #[test]
    fn from_fixture() {
        let v = crate::test_util::fixture("users_presence.json");
        let pr: rocketchat::models::Presence = serde_json::from_value(v).unwrap();
        let mut p = Presence::new();
        for u in &pr.users {
            p.update_from_summary(u);
        }
        assert_eq!(p.len(), pr.users.len());
    }
}
