//! Transient UI state of UIKit surfaces that is not part of a view's
//! `state` (docs/uikit.md §10.3): the open popover and its filter text,
//! half-typed date/time/text fields, local values of message and banner
//! widgets (those surfaces have no view state), "show more" and tab
//! selections, per-element loading flags, autocomplete results and a pending
//! confirmation. Plain data, string-keyed by [`SurfaceKey::element`].

use std::collections::{HashMap, HashSet};
use std::hash::{DefaultHasher, Hash, Hasher};

use rocketchat::uikit::Confirm;
use serde_json::Value;

use crate::room_list::Avatar;

/// The surface an element lives on.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum SurfaceKey {
    /// Blocks of message `mid` in room `rid` (thread reply: `tmid`).
    Message {
        rid: String,
        mid: String,
        tmid: Option<String>,
    },
    /// The open modal with this view id.
    Modal(String),
    /// The contextual bar with this view id.
    Bar(String),
    /// The banner with this view id.
    Banner(String),
}

impl SurfaceKey {
    /// Prefix of every scratch key of this surface.
    pub fn prefix(&self) -> String {
        match self {
            Self::Message { mid, .. } => format!("m:{mid}|"),
            Self::Modal(id) => format!("v:{id}|"),
            Self::Bar(id) => format!("c:{id}|"),
            Self::Banner(id) => format!("b:{id}|"),
        }
    }

    /// Scratch key of element `action_id` on this surface.
    pub fn element(&self, action_id: &str) -> String {
        format!("{}{action_id}", self.prefix())
    }

    /// The modal or bar this surface is, with its view id.
    pub fn view_target(&self) -> Option<(super::ViewTarget, &str)> {
        match self {
            Self::Modal(id) => Some((super::ViewTarget::Modal, id)),
            Self::Bar(id) => Some((super::ViewTarget::Bar, id)),
            Self::Message { .. } | Self::Banner(_) => None,
        }
    }

    /// View id of a modal, bar or banner.
    pub fn view_id(&self) -> Option<&str> {
        match self {
            Self::Message { .. } => None,
            Self::Modal(id) | Self::Bar(id) | Self::Banner(id) => Some(id),
        }
    }
}

/// A block action the user triggered, before it is emitted.
#[derive(Debug, Clone, PartialEq)]
pub struct ActionRequest {
    pub surface: SurfaceKey,
    /// Inherited `appId` of the element (`None`: no app, §2.2).
    pub app_id: Option<String>,
    pub block_id: String,
    pub action_id: String,
    pub value: Option<Value>,
    /// A link button / overflow option URL to open as well (§3.2).
    pub url: Option<String>,
    /// The element is part of a `video_conf` block (§3.2 trust rule).
    pub in_video_conf: bool,
}

/// One autocomplete result of a users / channels select.
#[derive(Debug, Clone, PartialEq)]
pub struct Suggestion {
    /// The value sent: a username or a room id (§2.4).
    pub value: String,
    pub label: String,
    /// Secondary text (`(username)` of a user).
    pub detail: Option<String>,
    pub avatar: Option<Avatar>,
}

/// See the module documentation.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Scratch {
    /// Key of the open popover (select list, overflow menu, date calendar,
    /// composer app menu).
    pub open: Option<String>,
    /// Filter text of the open select.
    pub filter: String,
    /// Field text that is not (yet) a value: date/time typing, autocomplete
    /// queries, text inputs of messages and banners.
    pub texts: HashMap<String, String>,
    /// Local widget values of surfaces without view state (messages, banners).
    pub values: HashMap<String, Value>,
    /// Actions blocks showing all their elements ("Show more...").
    pub expanded: HashSet<String>,
    /// Selected tab per tab-navigation block key.
    pub tabs: HashMap<String, String>,
    /// Element key → trigger id of the request it is waiting for.
    pub loading: HashMap<String, String>,
    /// Autocomplete results per element key.
    pub suggestions: HashMap<String, Vec<Suggestion>>,
    /// Labels of chosen users/channels (`element key|value` → label).
    pub labels: HashMap<String, String>,
    /// An action waiting for its confirmation dialog (§2.2).
    pub confirm: Option<(Confirm, ActionRequest)>,
    /// Latest debounce timer per key (element key, or a timer name); a timer
    /// that fires with an older stamp was superseded.
    pub stamps: HashMap<String, u64>,
    /// Source of [`Self::stamps`] values.
    pub next_stamp: u64,
}

/// Message surfaces with scratch entries kept before the oldest are dropped
/// (messages are never closed, so their entries would pile up).
pub const MESSAGE_SURFACES_MAX: usize = 64;

impl Scratch {
    /// Drops everything of the surface with key `prefix`.
    pub fn forget(&mut self, prefix: &str) {
        if self.open.as_deref().is_some_and(|o| o.starts_with(prefix)) {
            self.open = None;
            self.filter.clear();
        }
        self.texts.retain(|k, _| !k.starts_with(prefix));
        self.values.retain(|k, _| !k.starts_with(prefix));
        self.expanded.retain(|k| !k.starts_with(prefix));
        self.tabs.retain(|k, _| !k.starts_with(prefix));
        self.loading.retain(|k, _| !k.starts_with(prefix));
        self.suggestions.retain(|k, _| !k.starts_with(prefix));
        self.labels.retain(|k, _| !k.starts_with(prefix));
        // Pending debounce timers of the surface: a view replaced under the
        // same id must not receive the old instance's input.
        self.stamps.retain(|k, _| !k.starts_with(prefix));
        if self
            .confirm
            .as_ref()
            .is_some_and(|(_, a)| a.surface.prefix() == prefix)
        {
            self.confirm = None;
        }
    }

    /// Starts a debounce timer for `key`, superseding the previous one of
    /// that key only; returns its stamp.
    pub fn stamp(&mut self, key: &str) -> u64 {
        self.next_stamp += 1;
        self.stamps.insert(key.to_owned(), self.next_stamp);
        self.next_stamp
    }

    /// Whether `stamp` is the latest timer of `key` (consumes it then).
    pub fn take_stamp(&mut self, key: &str, stamp: u64) -> bool {
        if self.stamps.get(key) == Some(&stamp) {
            self.stamps.remove(key);
            return true;
        }
        false
    }

    /// The latest timer of `key`, if any.
    pub fn current_stamp(&self, key: &str) -> Option<u64> {
        self.stamps.get(key).copied()
    }

    /// A hash of everything stored for the surface with key `prefix` (row
    /// caches key on it, so a change elsewhere does not rebuild the row).
    pub fn digest(&self, prefix: &str) -> u64 {
        fn sorted<'a, V>(
            m: impl Iterator<Item = (&'a String, V)>,
            prefix: &str,
        ) -> Vec<(&'a String, V)> {
            let mut v: Vec<_> = m.filter(|(k, _)| k.starts_with(prefix)).collect();
            v.sort_by(|a, b| a.0.cmp(b.0));
            v
        }
        let mut h = DefaultHasher::new();
        let open = self.open.as_deref().filter(|o| o.starts_with(prefix));
        open.hash(&mut h);
        if open.is_some() {
            self.filter.hash(&mut h);
        }
        sorted(self.texts.iter(), prefix).hash(&mut h);
        for (k, v) in sorted(self.values.iter(), prefix) {
            k.hash(&mut h);
            v.to_string().hash(&mut h);
        }
        let mut expanded: Vec<_> = self
            .expanded
            .iter()
            .filter(|k| k.starts_with(prefix))
            .collect();
        expanded.sort();
        expanded.hash(&mut h);
        sorted(self.tabs.iter(), prefix).hash(&mut h);
        sorted(self.loading.iter(), prefix).hash(&mut h);
        for (k, list) in sorted(self.suggestions.iter(), prefix) {
            k.hash(&mut h);
            for s in list {
                (&s.value, &s.label, &s.detail).hash(&mut h);
            }
        }
        sorted(self.labels.iter(), prefix).hash(&mut h);
        h.finish()
    }

    /// Keeps the entries of at most [`MESSAGE_SURFACES_MAX`] message surfaces
    /// besides `keep`: the others (oldest touched first is not tracked, so
    /// arbitrary ones) are forgotten.
    pub fn trim_messages(&mut self, keep: &str) {
        let mut prefixes: HashSet<String> = HashSet::new();
        let keys = self
            .texts
            .keys()
            .chain(self.values.keys())
            .chain(self.expanded.iter())
            .chain(self.tabs.keys())
            .chain(self.labels.keys());
        for k in keys {
            if let Some(rest) = k.strip_prefix("m:")
                && let Some(end) = rest.find('|')
            {
                prefixes.insert(k[..end + 3].to_owned());
            }
        }
        if prefixes.len() <= MESSAGE_SURFACES_MAX {
            return;
        }
        let excess = prefixes.len() - MESSAGE_SURFACES_MAX;
        let drop: Vec<String> = prefixes
            .into_iter()
            .filter(|p| p != keep && !self.surface_loading(p))
            .take(excess)
            .collect();
        for p in drop {
            self.forget(&p);
        }
    }

    /// The request `trigger_id` finished: its elements stop loading.
    pub fn settle(&mut self, trigger_id: &str) {
        self.loading.retain(|_, t| t != trigger_id);
    }

    pub fn is_loading(&self, key: &str) -> bool {
        self.loading.contains_key(key)
    }

    /// Whether any element of the surface with key `prefix` is loading.
    pub fn surface_loading(&self, prefix: &str) -> bool {
        self.loading.keys().any(|k| k.starts_with(prefix))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_and_forget() {
        let m = SurfaceKey::Message {
            rid: "r".into(),
            mid: "m1".into(),
            tmid: None,
        };
        let v = SurfaceKey::Modal("view".into());
        assert_eq!(m.element("a"), "m:m1|a");
        assert_eq!(v.element("a"), "v:view|a");
        assert_eq!(v.view_id(), Some("view"));
        assert_eq!(m.view_id(), None);
        let mut s = Scratch {
            open: Some(v.element("sel")),
            ..Default::default()
        };
        s.texts.insert(v.element("date"), "2026-".into());
        s.texts.insert(m.element("date"), "x".into());
        s.loading.insert(m.element("btn"), "T1".into());
        s.loading.insert(v.element("btn"), "T2".into());
        s.forget(&v.prefix());
        assert!(s.open.is_none());
        assert_eq!(s.texts.len(), 1);
        assert!(s.surface_loading(&m.prefix()));
        assert!(!s.surface_loading(&v.prefix()));
        s.settle("T1");
        assert!(!s.is_loading(&m.element("btn")));
    }

    #[test]
    fn stamps_are_per_key() {
        let mut s = Scratch::default();
        let a = s.stamp("v:x|a");
        let b = s.stamp("v:x|b");
        // B's timer does not supersede A's.
        assert!(s.take_stamp("v:x|a", a));
        let b2 = s.stamp("v:x|b");
        assert!(!s.take_stamp("v:x|b", b));
        assert!(s.take_stamp("v:x|b", b2));
        assert!(!s.take_stamp("v:x|b", b2));
    }

    #[test]
    fn digest_covers_only_its_surface() {
        let m1 = SurfaceKey::Message {
            rid: "r".into(),
            mid: "m1".into(),
            tmid: None,
        };
        let mut s = Scratch::default();
        let d = s.digest(&m1.prefix());
        s.texts.insert("v:view|a".into(), "typed".into());
        s.open = Some("v:view|sel".into());
        assert_eq!(s.digest(&m1.prefix()), d);
        s.values.insert(m1.element("sel"), Value::from("x"));
        assert_ne!(s.digest(&m1.prefix()), d);
    }

    #[test]
    fn message_entries_are_capped() {
        let mut s = Scratch::default();
        for i in 0..(MESSAGE_SURFACES_MAX + 10) {
            s.values.insert(format!("m:{i}|a"), Value::from(1));
        }
        s.values.insert("v:view|a".into(), Value::from(1));
        s.trim_messages("m:3|");
        assert_eq!(s.values.len(), MESSAGE_SURFACES_MAX + 1);
        assert!(s.values.contains_key("m:3|a"));
        assert!(s.values.contains_key("v:view|a"));
    }
}
