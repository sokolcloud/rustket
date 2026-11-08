//! Transient UI state of UIKit surfaces that is not part of a view's
//! `state` (docs/uikit.md §10.3): the open popover and its filter text,
//! half-typed date/time/text fields, local values of message and banner
//! widgets (those surfaces have no view state), "show more" and tab
//! selections, per-element loading flags, autocomplete results and a pending
//! confirmation. Plain data, string-keyed by [`SurfaceKey::element`].

use std::collections::{HashMap, HashSet};

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
    /// Counter for debounce timers.
    pub stamp: u64,
}

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
        if self
            .confirm
            .as_ref()
            .is_some_and(|(_, a)| a.surface.prefix() == prefix)
        {
            self.confirm = None;
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
}
