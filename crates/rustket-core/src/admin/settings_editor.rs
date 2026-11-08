//! Generic settings editor state (docs/admin.md §14): the private-settings store,
//! the group/tab/section structure, and an edit session evaluated against the
//! edited (unsaved) state like the web's `EditableSettingsContext`.
//!
//! Setting values are never logged (ids only).

use std::cmp::Ordering;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::fmt;

use rocketchat::RcDate;
use rocketchat::models::UpdatedSince;
use rocketchat::models::admin::{
    LicenseInfo, SettingChange, SettingDef, SettingEventAction, SettingPatch, SettingQuery,
    SettingType,
};
use serde_json::Value;

use super::mongo_filter;
use crate::permissions::has_permission;
use crate::workspace::Workspace;

/// Settings the web never shows (`defaultOmit` in `EditableSettingsProvider.tsx`).
pub const OMITTED: &[&str] = &["Cloud_Workspace_AirGapped_Restrictions_Remaining_Days"];

/// Group whose sections have no "Restore defaults" button.
pub const GROUP_WITHOUT_SECTION_RESET: &str = "Assets";

/// All private settings the user may manage, kept current from
/// `private-settings/get` (full and delta) and `private-settings-changed` events.
#[derive(Clone, Default)]
pub struct SettingsStore {
    defs: BTreeMap<String, SettingDef>,
    last_updated: Option<RcDate>,
    delta_needed: bool,
    /// Bumped by every stream event; a delta clears `delta_needed` only if no event
    /// arrived after its request.
    event_generation: u64,
    revision: u64,
}

impl fmt::Debug for SettingsStore {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SettingsStore")
            .field("len", &self.defs.len())
            .field("last_updated", &self.last_updated)
            .field("delta_needed", &self.delta_needed)
            .field("revision", &self.revision)
            .finish_non_exhaustive()
    }
}

/// A delta fetch to run: `private-settings/get` with `since` (`None` = full load),
/// then [`SettingsStore::apply_delta`] with this request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DeltaRequest {
    pub since: Option<RcDate>,
    pub generation: u64,
}

impl SettingsStore {
    pub fn new(defs: Vec<SettingDef>) -> Self {
        let mut s = Self::default();
        s.replace_all(defs);
        s
    }

    /// Replaces everything with a full `private-settings/get` result.
    pub fn replace_all(&mut self, defs: Vec<SettingDef>) {
        self.defs.clear();
        self.last_updated = None;
        for d in defs {
            self.insert(d);
        }
        self.delta_needed = false;
        self.revision += 1;
    }

    fn insert(&mut self, d: SettingDef) {
        if d.hidden || d.id.is_empty() {
            return;
        }
        if let Some(t) = d.updated_at
            && self.last_updated.is_none_or(|l| t > l)
        {
            self.last_updated = Some(t);
        }
        self.defs.insert(d.id.clone(), d);
    }

    /// Applies the answer to `request` (`private-settings/get([{"$date": since}])`).
    /// Events that arrived while it was in flight keep the delta flag set, since the
    /// server may have run the query before them.
    pub fn apply_delta(&mut self, request: DeltaRequest, delta: UpdatedSince<SettingDef>) {
        for d in delta.update {
            self.insert(d);
        }
        for r in delta.remove {
            self.defs.remove(&r.id);
        }
        if request.generation == self.event_generation {
            self.delta_needed = false;
        }
        self.revision += 1;
    }

    /// Merges a partial `private-settings-changed` event and flags that a delta fetch
    /// is needed (the event lacks metadata and carries license-masked values of EE
    /// settings, so it is only merged into non-EE settings). Debounce the fetch.
    pub fn apply_stream_event(&mut self, action: SettingEventAction, patch: SettingPatch) {
        if action == SettingEventAction::Updated
            && let Some(d) = self.defs.get_mut(&patch.id)
            && !d.enterprise
            && patch.enterprise != Some(true)
        {
            if let Some(v) = patch.value {
                d.value = Some(v);
            }
            if patch.editor.is_some() {
                d.editor = patch.editor;
            }
            self.revision += 1;
        }
        self.delta_needed = true;
        self.event_generation += 1;
    }

    /// The delta fetch to run, if any.
    pub fn delta_request(&self) -> Option<DeltaRequest> {
        self.delta_needed.then_some(DeltaRequest {
            since: self.last_updated,
            generation: self.event_generation,
        })
    }

    /// Optimistically applies a successful save (the stream confirms it later).
    pub fn apply_saved(&mut self, changes: &[SettingChange]) {
        for c in changes {
            if let Some(d) = self.defs.get_mut(&c.id) {
                d.value = Some(c.value.clone());
                if c.editor.is_some() {
                    d.editor = c.editor.clone();
                }
            }
        }
        self.revision += 1;
    }

    pub fn get(&self, id: &str) -> Option<&SettingDef> {
        self.defs.get(id)
    }

    pub fn iter(&self) -> impl Iterator<Item = &SettingDef> {
        self.defs.values()
    }

    pub fn len(&self) -> usize {
        self.defs.len()
    }

    pub fn is_empty(&self) -> bool {
        self.defs.is_empty()
    }

    /// Bumped on every change; use it to rebuild a [`GroupIndex`].
    pub fn revision(&self) -> u64 {
        self.revision
    }

    pub fn last_updated(&self) -> Option<RcDate> {
        self.last_updated
    }

    /// The persisted value of `id`.
    pub fn value(&self, id: &str) -> Option<&Value> {
        self.defs.get(id).and_then(|d| d.value.as_ref())
    }
}

/// A group card of the settings start page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GroupEntry {
    pub id: String,
    pub i18n_label: Option<String>,
    pub i18n_description: Option<String>,
}

/// A sub-tab of a group page (`tab`; `""` = untabbed, titled with the group).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tab {
    pub id: String,
    pub sections: Vec<Section>,
}

/// A section of a group page (`""` = untitled).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Section {
    pub name: String,
    pub setting_ids: Vec<String>,
}

/// The structure of one group page.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct GroupPage {
    pub group: String,
    pub tabs: Vec<Tab>,
}

impl GroupPage {
    /// Show tabs only when there is more than one.
    pub fn has_tabs(&self) -> bool {
        self.tabs.len() > 1
    }

    /// A section is collapsible unless it is untitled or the only one of its tab.
    pub fn is_collapsible(tab: &Tab, section: &Section) -> bool {
        !section.name.is_empty() && tab.sections.len() > 1
    }

    pub fn setting_ids(&self) -> impl Iterator<Item = &str> {
        self.tabs
            .iter()
            .flat_map(|t| t.sections.iter())
            .flat_map(|s| s.setting_ids.iter().map(String::as_str))
    }
}

/// Group membership of the store (rebuild when [`SettingsStore::revision`] changes).
#[derive(Debug, Clone, Default)]
pub struct GroupIndex {
    groups: Vec<GroupEntry>,
    members: HashMap<String, Vec<String>>,
    revision: u64,
}

impl GroupIndex {
    pub fn build(store: &SettingsStore) -> Self {
        let mut groups = Vec::new();
        let mut members: HashMap<String, Vec<String>> = HashMap::new();
        for d in store.iter() {
            if d.is_group() {
                groups.push(GroupEntry {
                    id: d.id.clone(),
                    i18n_label: d.i18n_label.clone(),
                    i18n_description: d.i18n_description.clone(),
                });
            } else if let Some(g) = &d.group
                && !OMITTED.contains(&d.id.as_str())
            {
                members.entry(g.clone()).or_default().push(d.id.clone());
            }
        }
        Self {
            groups,
            members,
            revision: store.revision(),
        }
    }

    pub fn is_stale(&self, store: &SettingsStore) -> bool {
        self.revision != store.revision()
    }

    /// Group records sorted by `label` (pass the translated label), case-insensitive.
    /// Settings of groups without a record are unreachable and not listed.
    pub fn groups(&self, label: impl Fn(&GroupEntry) -> String) -> Vec<&GroupEntry> {
        let mut v: Vec<(String, &GroupEntry)> = self
            .groups
            .iter()
            .map(|g| (label(g).to_lowercase(), g))
            .collect();
        v.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.id.cmp(&b.1.id)));
        v.into_iter().map(|(_, g)| g).collect()
    }

    pub fn group(&self, id: &str) -> Option<&GroupEntry> {
        self.groups.iter().find(|g| g.id == id)
    }

    pub fn member_ids(&self, group: &str) -> &[String] {
        self.members.get(group).map_or(&[], Vec::as_slice)
    }

    /// Groups matching a search: a group matches by its own label/id, or when any
    /// of its settings matches by label, `t(_id)` or `_id` (case-insensitive
    /// substring; `label` returns the translated texts to test).
    pub fn search(
        &self,
        store: &SettingsStore,
        filter: &str,
        label: impl Fn(&SettingDef) -> Vec<String>,
    ) -> Vec<String> {
        let needle = filter.trim().to_lowercase();
        if needle.is_empty() {
            return self.groups.iter().map(|g| g.id.clone()).collect();
        }
        let hit = |d: &SettingDef| {
            d.id.to_lowercase().contains(&needle)
                || label(d).iter().any(|l| l.to_lowercase().contains(&needle))
        };
        self.groups
            .iter()
            .filter(|g| {
                store.get(&g.id).is_some_and(hit)
                    || self
                        .member_ids(&g.id)
                        .iter()
                        .filter_map(|id| store.get(id))
                        .any(&hit)
            })
            .map(|g| g.id.clone())
            .collect()
    }

    /// Tabs and sections of `group`, ordered by `(sorter, tab, i18nLabel)`.
    pub fn group_page(&self, store: &SettingsStore, group: &str) -> GroupPage {
        let mut defs: Vec<&SettingDef> = self
            .member_ids(group)
            .iter()
            .filter_map(|id| store.get(id))
            .collect();
        defs.sort_by(|a, b| compare_settings(a, b));
        let mut tabs: Vec<Tab> = Vec::new();
        for d in defs {
            let tab_id = d.tab.clone().unwrap_or_default();
            let section = d.section.clone().unwrap_or_default();
            let ti = match tabs.iter().position(|t| t.id == tab_id) {
                Some(i) => i,
                None => {
                    tabs.push(Tab {
                        id: tab_id,
                        sections: Vec::new(),
                    });
                    tabs.len() - 1
                }
            };
            let tab = &mut tabs[ti];
            match tab.sections.iter_mut().find(|s| s.name == section) {
                Some(s) => s.setting_ids.push(d.id.clone()),
                None => tab.sections.push(Section {
                    name: section,
                    setting_ids: vec![d.id.clone()],
                }),
            }
        }
        GroupPage {
            group: group.to_owned(),
            tabs,
        }
    }
}

fn compare_settings(a: &SettingDef, b: &SettingDef) -> Ordering {
    let key = |d: &SettingDef| {
        (
            d.tab.as_deref().unwrap_or_default().to_lowercase(),
            d.i18n_label.as_deref().unwrap_or_default().to_lowercase(),
        )
    };
    a.sorter
        .cmp(&b.sorter)
        .then_with(|| key(a).cmp(&key(b)))
        .then_with(|| a.id.cmp(&b.id))
}

/// Who may edit what, and which EE modules are active.
#[derive(Debug, Clone, Default)]
pub struct EditContext {
    /// `edit-privileged-setting`.
    pub can_edit_all: bool,
    /// Settings with a granted `change-setting-<id>` permission, filled only with
    /// `manage-selected-settings` (the server requires both, `saveSettingsBulk.ts`).
    pub editable_ids: HashSet<String>,
    pub has_license: bool,
    pub active_modules: Vec<String>,
}

impl EditContext {
    pub fn from_workspace(ws: &Workspace, license: Option<&LicenseInfo>) -> Self {
        let can_edit_all = has_permission(ws, "edit-privileged-setting", None);
        let editable_ids = if has_permission(ws, "manage-selected-settings", None) {
            ws.permissions()
                .keys()
                .filter_map(|p| p.strip_prefix("change-setting-"))
                .filter(|id| has_permission(ws, &format!("change-setting-{id}"), None))
                .map(str::to_owned)
                .collect()
        } else {
            HashSet::new()
        };
        Self {
            can_edit_all,
            editable_ids,
            has_license: license.is_some_and(|l| l.has_valid_license),
            active_modules: license
                .map(|l| l.active_modules.clone())
                .unwrap_or_default(),
        }
    }

    /// Everything editable, no license.
    pub fn admin() -> Self {
        Self {
            can_edit_all: true,
            ..Self::default()
        }
    }

    pub fn can_edit(&self, id: &str) -> bool {
        self.can_edit_all || self.editable_ids.contains(id)
    }

    /// Read-only mode: no setting editable at all (no save bar).
    pub fn is_read_only(&self) -> bool {
        !self.can_edit_all && self.editable_ids.is_empty()
    }

    /// An EE setting is locked unless licensed and all its modules are active (an EE
    /// setting without modules stays locked, like the web).
    pub fn is_ee_locked(&self, def: &SettingDef) -> bool {
        def.enterprise
            && !(self.has_license
                && !def.modules.is_empty()
                && def
                    .modules
                    .iter()
                    .all(|m| self.active_modules.iter().any(|a| a == m)))
    }
}

/// A client-side validation failure; `i18n_key` is a Rustket (rust-i18n) key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SettingError {
    /// `code` setting with `application/json` that does not parse.
    InvalidJson,
    /// `int`/`range`/`timespan` that is not an integer.
    NotInteger,
    /// `int`/`range` outside `minValue`/`maxValue`.
    OutOfRange { min: Option<i64>, max: Option<i64> },
    /// The JSON type does not match what the server checks.
    WrongType,
}

impl SettingError {
    /// Rustket translation key (en + ru in the UI locale files); `OutOfRange`
    /// takes the `min`/`max` arguments, use `..._min`/`..._max` variants when only
    /// one bound is set.
    pub fn i18n_key(&self) -> &'static str {
        match self {
            Self::InvalidJson => "admin.settings.error.invalid_json",
            Self::NotInteger => "admin.settings.error.not_integer",
            Self::OutOfRange {
                min: Some(_),
                max: Some(_),
            } => "admin.settings.error.out_of_range",
            Self::OutOfRange { min: Some(_), .. } => "admin.settings.error.out_of_range_min",
            Self::OutOfRange { max: Some(_), .. } => "admin.settings.error.out_of_range_max",
            Self::OutOfRange { .. } | Self::WrongType => "admin.settings.error.wrong_type",
        }
    }
}

#[derive(Clone, PartialEq)]
struct Edit {
    value: Value,
    editor: Option<String>,
    /// Persisted state when the edit started (to detect remote changes).
    base_value: Option<Value>,
    base_editor: Option<String>,
}

/// Unsaved edits of one group page. `Debug` prints ids only (values may be secrets).
#[derive(Clone, PartialEq, Default)]
pub struct EditSession {
    pub group: String,
    edits: HashMap<String, Edit>,
}

impl fmt::Debug for EditSession {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut ids: Vec<&String> = self.edits.keys().collect();
        ids.sort();
        f.debug_struct("EditSession")
            .field("group", &self.group)
            .field("edited_ids", &ids)
            .finish()
    }
}

impl EditSession {
    pub fn new(group: &str) -> Self {
        Self {
            group: group.to_owned(),
            edits: HashMap::new(),
        }
    }

    /// Sets the edited value; an edit equal to the persisted state is dropped.
    pub fn set_value(&mut self, store: &SettingsStore, id: &str, value: Value) {
        let editor = self.effective_editor(store, id).map(str::to_owned);
        self.set(store, id, value, editor);
    }

    /// Sets the editor of a `color` setting (`color` or `expression`).
    pub fn set_editor(&mut self, store: &SettingsStore, id: &str, editor: &str) {
        let value = self
            .effective(store, id)
            .cloned()
            .unwrap_or(Value::String(String::new()));
        self.set(store, id, value, Some(editor.to_owned()));
    }

    fn set(&mut self, store: &SettingsStore, id: &str, value: Value, editor: Option<String>) {
        let Some(def) = store.get(id) else {
            return;
        };
        let same = json_eq(def.value.as_ref(), Some(&value)) && editor == def.editor;
        if same {
            self.edits.remove(id);
            return;
        }
        let (base_value, base_editor) = match self.edits.get(id) {
            Some(e) => (e.base_value.clone(), e.base_editor.clone()),
            None => (def.value.clone(), def.editor.clone()),
        };
        self.edits.insert(
            id.to_owned(),
            Edit {
                value,
                editor,
                base_value,
                base_editor,
            },
        );
    }

    /// The edited value, else the persisted one.
    pub fn effective<'a>(&'a self, store: &'a SettingsStore, id: &str) -> Option<&'a Value> {
        match self.edits.get(id) {
            Some(e) => Some(&e.value),
            None => store.value(id),
        }
    }

    pub fn effective_editor<'a>(&'a self, store: &'a SettingsStore, id: &str) -> Option<&'a str> {
        match self.edits.get(id) {
            Some(e) => e.editor.as_deref(),
            None => store.get(id).and_then(|d| d.editor.as_deref()),
        }
    }

    /// Edited value differs from the persisted one (or the colour editor differs).
    pub fn is_changed(&self, store: &SettingsStore, id: &str) -> bool {
        let (Some(e), Some(def)) = (self.edits.get(id), store.get(id)) else {
            return false;
        };
        !json_eq(Some(&e.value), def.value.as_ref()) || e.editor != def.editor
    }

    pub fn is_dirty(&self, store: &SettingsStore) -> bool {
        self.edits.keys().any(|id| self.is_changed(store, id))
    }

    /// Any of `ids` (e.g. a section) changed.
    pub fn any_changed<'a>(
        &self,
        store: &SettingsStore,
        ids: impl IntoIterator<Item = &'a String>,
    ) -> bool {
        ids.into_iter().any(|id| self.is_changed(store, id))
    }

    /// `query` satisfied against the edited state of the whole store.
    pub fn query_satisfied(&self, store: &SettingsStore, query: &SettingQuery) -> bool {
        query
            .filters
            .iter()
            .all(|f| match f.get("_id").and_then(Value::as_str) {
                Some(id) => store
                    .get(id)
                    .is_some_and(|d| mongo_filter::matches(f, &self.document(store, d))),
                None => store
                    .iter()
                    .any(|d| mongo_filter::matches(f, &self.document(store, d))),
            })
    }

    fn document(&self, store: &SettingsStore, d: &SettingDef) -> Value {
        d.query_document(
            self.effective(store, &d.id),
            self.effective_editor(store, &d.id),
        )
    }

    /// `displayQuery` satisfied (edited state).
    pub fn is_visible(&self, store: &SettingsStore, id: &str) -> bool {
        let Some(def) = store.get(id) else {
            return false;
        };
        def.display_query
            .as_ref()
            .is_none_or(|q| self.query_satisfied(store, q))
    }

    /// `enableQuery` satisfied (edited state).
    pub fn is_enabled_by_query(&self, store: &SettingsStore, id: &str) -> bool {
        store.get(id).is_some_and(|d| {
            d.enable_query
                .as_ref()
                .is_none_or(|q| self.query_satisfied(store, q))
        })
    }

    /// Disabled: blocked, EE without license, not editable for this user, or
    /// `enableQuery` not satisfied. `readonly` is separate ([`Self::is_readonly`]).
    pub fn is_disabled(&self, store: &SettingsStore, ctx: &EditContext, id: &str) -> bool {
        let Some(def) = store.get(id) else {
            return true;
        };
        def.blocked
            || ctx.is_ee_locked(def)
            || !ctx.can_edit(id)
            || !self.is_enabled_by_query(store, id)
    }

    pub fn is_readonly(&self, store: &SettingsStore, id: &str) -> bool {
        store.get(id).is_none_or(|d| d.readonly)
    }

    /// Editable widget: neither disabled nor read-only, and a type that is edited
    /// through Save (not `action`/`asset`/`group`).
    pub fn is_editable(&self, store: &SettingsStore, ctx: &EditContext, id: &str) -> bool {
        !self.is_disabled(store, ctx, id)
            && !self.is_readonly(store, id)
            && store.get(id).is_some_and(|d| {
                !matches!(
                    d.kind,
                    SettingType::Action
                        | SettingType::Asset
                        | SettingType::Group
                        | SettingType::Unknown(_)
                )
            })
    }

    fn differs_from_package(&self, store: &SettingsStore, def: &SettingDef) -> bool {
        !json_eq(self.effective(store, &def.id), def.package_value.as_ref())
            || (def.kind == SettingType::Color
                && self.effective_editor(store, &def.id) != def.package_editor.as_deref())
    }

    /// Per-setting reset button.
    pub fn can_reset(&self, store: &SettingsStore, ctx: &EditContext, id: &str) -> bool {
        let Some(def) = store.get(id) else {
            return false;
        };
        def.package_value.is_some()
            && self.is_editable(store, ctx, id)
            && self.differs_from_package(store, def)
    }

    /// Sets the edited value back to `packageValue` (does not save).
    pub fn reset(&mut self, store: &SettingsStore, id: &str) {
        let Some(def) = store.get(id) else {
            return;
        };
        let Some(pv) = def.package_value.clone() else {
            return;
        };
        let editor = if def.kind == SettingType::Color {
            def.package_editor.clone().or_else(|| def.editor.clone())
        } else {
            def.editor.clone()
        };
        self.set(store, id, pv, editor);
    }

    /// Section "Restore defaults" button (not for the Assets group).
    pub fn section_can_reset(
        &self,
        store: &SettingsStore,
        ctx: &EditContext,
        section: &Section,
    ) -> bool {
        self.group != GROUP_WITHOUT_SECTION_RESET
            && section
                .setting_ids
                .iter()
                .any(|id| self.can_reset(store, ctx, id))
    }

    /// Resets every editable setting of `section` to its default.
    pub fn reset_section(&mut self, store: &SettingsStore, ctx: &EditContext, section: &Section) {
        for id in &section.setting_ids {
            if self.can_reset(store, ctx, id) {
                self.reset(store, id);
            }
        }
    }

    /// Drops all edits.
    pub fn cancel(&mut self) {
        self.edits.clear();
    }

    /// Drops the edit of `id` (e.g. "take the server's value" after a conflict).
    pub fn discard(&mut self, id: &str) {
        self.edits.remove(id);
    }

    /// Edits whose persisted value changed on the server after editing started.
    pub fn remote_conflicts(&self, store: &SettingsStore) -> Vec<String> {
        let mut v: Vec<String> = self
            .edits
            .iter()
            .filter(|(id, e)| {
                store.get(id).is_some_and(|d| {
                    (!json_eq(d.value.as_ref(), e.base_value.as_ref()) || d.editor != e.base_editor)
                        && self.is_changed(store, id)
                })
            })
            .map(|(id, _)| id.clone())
            .collect();
        v.sort();
        v
    }

    /// After a successful save of `changes` (call after
    /// [`SettingsStore::apply_saved`]): edits made while the save was in flight are
    /// rebased onto the saved values, so the own save is not a remote conflict; edits
    /// equal to the saved state are dropped.
    pub fn apply_saved(&mut self, store: &SettingsStore, changes: &[SettingChange]) {
        for c in changes {
            if let (Some(e), Some(d)) = (self.edits.get_mut(&c.id), store.get(&c.id)) {
                e.base_value = d.value.clone();
                e.base_editor = d.editor.clone();
            }
        }
        self.prune(store);
    }

    /// Keeps the local edit of `id` over a remote change.
    pub fn keep_mine(&mut self, store: &SettingsStore, id: &str) {
        if let (Some(e), Some(d)) = (self.edits.get_mut(id), store.get(id)) {
            e.base_value = d.value.clone();
            e.base_editor = d.editor.clone();
        }
    }

    /// Removes edits that now equal the persisted state (after a save or a remote
    /// change) and edits of settings that disappeared.
    pub fn prune(&mut self, store: &SettingsStore) {
        let gone: Vec<String> = self
            .edits
            .keys()
            .filter(|id| !self.is_changed(store, id))
            .cloned()
            .collect();
        for id in gone {
            self.edits.remove(&id);
        }
    }

    /// Changed settings that fail client-side checks (Save must be disabled).
    pub fn validate(&self, store: &SettingsStore) -> Vec<(String, SettingError)> {
        let mut out: Vec<(String, SettingError)> = self
            .edits
            .iter()
            .filter(|(id, _)| self.is_changed(store, id))
            .filter_map(|(id, e)| {
                let def = store.get(id)?;
                validate_value(def, &e.value)
                    .err()
                    .map(|err| (id.clone(), err))
            })
            .collect();
        out.sort_by(|a, b| a.0.cmp(&b.0));
        out
    }

    /// Changed settings of the session as a bulk-save body, in display order.
    /// `editor` is sent only for colour settings.
    pub fn change_set(&self, store: &SettingsStore) -> Vec<SettingChange> {
        let mut defs: Vec<&SettingDef> = self
            .edits
            .keys()
            .filter(|id| self.is_changed(store, id))
            .filter_map(|id| store.get(id))
            .collect();
        defs.sort_by(|a, b| compare_settings(a, b));
        defs.into_iter()
            .filter_map(|d| {
                let e = self.edits.get(&d.id)?;
                Some(SettingChange {
                    id: d.id.clone(),
                    value: e.value.clone(),
                    editor: (d.kind == SettingType::Color)
                        .then(|| e.editor.clone())
                        .flatten(),
                })
            })
            .collect()
    }

    /// Changed settings with an `alert`: confirm before saving.
    pub fn alerts(&self, store: &SettingsStore) -> Vec<String> {
        self.change_set(store)
            .into_iter()
            .filter(|c| {
                store
                    .get(&c.id)
                    .is_some_and(|d| d.alert.as_deref().is_some_and(|a| !a.is_empty()))
            })
            .map(|c| c.id)
            .collect()
    }
}

/// Client-side check of a value against the server's type checks
/// (`saveSettingsBulk`) and the JSON check of `application/json` code settings.
pub fn validate_value(def: &SettingDef, value: &Value) -> Result<(), SettingError> {
    match &def.kind {
        SettingType::Boolean => value
            .is_boolean()
            .then_some(())
            .ok_or(SettingError::WrongType),
        SettingType::Int | SettingType::Range | SettingType::Timespan => {
            let Some(n) = as_integer(value) else {
                return Err(if value.is_number() {
                    SettingError::NotInteger
                } else {
                    SettingError::WrongType
                });
            };
            let bounded = matches!(def.kind, SettingType::Int | SettingType::Range);
            if bounded && n != 0 {
                let (min, max) = match def.kind {
                    SettingType::Range => (
                        Some(def.min_value.unwrap_or(0)),
                        Some(def.max_value.unwrap_or(100)),
                    ),
                    _ => (def.min_value, def.max_value),
                };
                if min.is_some_and(|m| n < m) || max.is_some_and(|m| n > m) {
                    return Err(SettingError::OutOfRange { min, max });
                }
            }
            Ok(())
        }
        SettingType::MultiSelect => value
            .is_array()
            .then_some(())
            .ok_or(SettingError::WrongType),
        SettingType::RoomPick => (value.is_array() || value == "")
            .then_some(())
            .ok_or(SettingError::WrongType),
        SettingType::Code => {
            let Some(s) = value.as_str() else {
                return Err(SettingError::WrongType);
            };
            if def.code.as_deref() == Some("application/json")
                && !s.trim().is_empty()
                && serde_json::from_str::<Value>(s).is_err()
            {
                return Err(SettingError::InvalidJson);
            }
            Ok(())
        }
        SettingType::Action | SettingType::Asset | SettingType::Group | SettingType::Unknown(_) => {
            Ok(())
        }
        _ => value
            .is_string()
            .then_some(())
            .ok_or(SettingError::WrongType),
    }
}

fn as_integer(v: &Value) -> Option<i64> {
    let n = v.as_number()?;
    n.as_i64().or_else(|| {
        n.as_f64()
            .filter(|f| f.is_finite() && f.fract() == 0.0)
            .map(|f| f as i64)
    })
}

fn json_eq(a: Option<&Value>, b: Option<&Value>) -> bool {
    match (a, b) {
        (None, None) => true,
        (Some(Value::Number(x)), Some(Value::Number(y))) => x.as_f64() == y.as_f64(),
        (Some(x), Some(y)) => x == y,
        _ => false,
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::test_util::fixture;
    use rocketchat::models::admin::parse_private_settings;
    use serde_json::json;

    fn store() -> SettingsStore {
        SettingsStore::new(parse_private_settings(fixture(
            "admin/private_settings.json",
        )))
    }

    #[test]
    fn decodes_every_setting_and_query() {
        let raw = fixture("admin/private_settings.json");
        let raw = raw.as_array().unwrap();
        let s = store();
        assert_eq!(s.len(), raw.len());
        let mut queries = 0;
        for r in raw {
            let id = r["_id"].as_str().unwrap();
            let def = s.get(id).unwrap();
            for (field, parsed) in [
                ("enableQuery", &def.enable_query),
                ("displayQuery", &def.display_query),
            ] {
                if let Some(q) = r.get(field).and_then(Value::as_str) {
                    queries += 1;
                    let parsed = parsed.as_ref().unwrap_or_else(|| panic!("{id} {field}"));
                    let v: Value = serde_json::from_str(q).unwrap();
                    assert!(
                        mongo_filter::unknown_operators(&v).is_empty(),
                        "{id} {field}: {q}"
                    );
                    assert!(!parsed.filters.is_empty() || v == json!([]), "{id}");
                }
            }
            assert!(!matches!(def.kind, SettingType::Unknown(_)), "{id}");
        }
        assert!(queries >= 373, "{queries}");
    }

    #[test]
    fn every_live_query_evaluates_and_flips_with_edits() {
        let s = store();
        let session = EditSession::new("x");
        let mut flipped = 0;
        for def in s.iter() {
            for q in [&def.enable_query, &def.display_query]
                .into_iter()
                .flatten()
            {
                let before = session.query_satisfied(&s, q);
                // Make every filter that names a boolean setting satisfied by editing
                // that setting; the query result must follow the edits.
                let mut edit = EditSession::new("x");
                let mut all_bool = !q.filters.is_empty();
                for f in &q.filters {
                    match (f.get("_id").and_then(Value::as_str), f.get("value")) {
                        (Some(id), Some(Value::Bool(b)))
                            if s.get(id).is_some() && f.as_object().unwrap().len() == 2 =>
                        {
                            edit.set_value(&s, id, Value::Bool(*b));
                        }
                        _ => all_bool = false,
                    }
                }
                if all_bool {
                    assert!(edit.query_satisfied(&s, q), "{}", def.id);
                    if !before {
                        flipped += 1;
                    }
                }
            }
        }
        assert!(flipped > 50, "{flipped}");
    }

    #[test]
    fn ldap_enable_query_on_edited_state() {
        let s = store();
        let ctx = EditContext::admin();
        let mut e = EditSession::new("LDAP");
        assert_eq!(s.value("LDAP_Enable"), Some(&json!(false)));
        assert!(e.is_disabled(&s, &ctx, "LDAP_Host"));
        assert!(!e.is_disabled(&s, &ctx, "LDAP_Enable"));
        e.set_value(&s, "LDAP_Enable", json!(true));
        assert!(!e.is_disabled(&s, &ctx, "LDAP_Host"));
        assert!(e.is_changed(&s, "LDAP_Enable"));
        // displayQuery: AD-only vs OpenLDAP-only fields.
        let shown: Vec<&SettingDef> = s.iter().filter(|d| d.display_query.is_some()).collect();
        assert!(!shown.is_empty());
        // Back to the persisted value drops the edit.
        e.set_value(&s, "LDAP_Enable", json!(false));
        assert!(!e.is_dirty(&s));
        assert!(e.is_disabled(&s, &ctx, "LDAP_Host"));
    }

    #[test]
    fn group_pages() {
        let s = store();
        let idx = GroupIndex::build(&s);
        let groups = idx.groups(|g| g.id.clone());
        assert!(groups.len() >= 38, "{}", groups.len());
        assert!(
            groups
                .iter()
                .all(|g| !idx.member_ids(&g.id).is_empty() || g.id != "LDAP")
        );

        let ldap = idx.group_page(&s, "LDAP");
        assert!(ldap.has_tabs());
        let tab_ids: Vec<&str> = ldap.tabs.iter().map(|t| t.id.as_str()).collect();
        assert!(tab_ids.contains(&"LDAP_Connection"), "{tab_ids:?}");
        assert_eq!(ldap.setting_ids().count(), idx.member_ids("LDAP").len());
        // Ordered by sorter inside a section.
        for t in &ldap.tabs {
            for sec in &t.sections {
                let sorters: Vec<i64> = sec
                    .setting_ids
                    .iter()
                    .map(|id| s.get(id).unwrap().sorter)
                    .collect();
                assert!(sorters.windows(2).all(|w| w[0] <= w[1]));
            }
        }

        let oauth = idx.group_page(&s, "OAuth");
        assert!(!oauth.has_tabs());
        assert!(
            oauth.tabs[0]
                .sections
                .iter()
                .any(|sec| sec.name == "Custom OAuth: Keycloak"),
        );

        let accounts = idx.group_page(&s, "Accounts");
        assert!(!accounts.has_tabs());
        assert!(accounts.tabs[0].sections.len() > 5);
        let tab = &accounts.tabs[0];
        assert!(
            tab.sections
                .iter()
                .any(|sec| GroupPage::is_collapsible(tab, sec))
        );

        let found = idx.search(&s, "keycloak", |d| {
            vec![d.i18n_label.clone().unwrap_or_default()]
        });
        assert_eq!(found, vec!["OAuth".to_owned()]);
        assert_eq!(idx.search(&s, "", |_| vec![]).len(), groups.len());
        assert!(!idx.is_stale(&s));
    }

    #[test]
    fn edit_reset_validate_and_save_set() {
        let s = store();
        let ctx = EditContext::admin();
        let mut e = EditSession::new("Accounts");
        let id = "Accounts_Password_Policy_MinLength";
        let def = s.get(id).unwrap();
        assert_eq!(def.kind, SettingType::Int);
        assert!(!e.can_reset(&s, &ctx, id) || s.value(id) != def.package_value.as_ref());
        e.set_value(&s, id, json!(9));
        assert!(e.is_changed(&s, id));
        assert!(e.can_reset(&s, &ctx, id));
        assert_eq!(
            e.change_set(&s),
            vec![SettingChange {
                id: id.into(),
                value: json!(9),
                editor: None
            }]
        );
        e.set_value(&s, id, json!(1.5));
        assert_eq!(
            e.validate(&s),
            vec![(id.to_owned(), SettingError::NotInteger)]
        );
        e.set_value(&s, id, json!("7"));
        assert_eq!(e.validate(&s)[0].1, SettingError::WrongType);
        e.reset(&s, id);
        assert!(!e.is_changed(&s, id) || s.value(id) != def.package_value.as_ref());
        e.cancel();
        assert!(!e.is_dirty(&s));

        // A JSON code setting.
        let code = s
            .iter()
            .find(|d| d.kind == SettingType::Code && d.code.as_deref() == Some("application/json"))
            .map(|d| d.id.clone());
        if let Some(code) = code {
            e.set_value(&s, &code, json!("{not json"));
            assert!(
                e.validate(&s)
                    .iter()
                    .any(|(i, er)| *i == code && *er == SettingError::InvalidJson)
            );
            e.set_value(&s, &code, json!(""));
            assert!(e.validate(&s).is_empty());
            e.cancel();
        }

        // Alerts are reported for confirmation.
        let alerted = s
            .iter()
            .find(|d| d.alert.is_some() && d.kind == SettingType::Boolean && !d.enterprise)
            .unwrap();
        let cur = s
            .value(&alerted.id)
            .and_then(Value::as_bool)
            .unwrap_or(false);
        e.set_value(&s, &alerted.id, json!(!cur));
        assert_eq!(e.alerts(&s), vec![alerted.id.clone()]);
    }

    #[test]
    fn range_and_colour() {
        let mut def = SettingDef {
            id: "r".into(),
            kind: SettingType::Range,
            min_value: Some(5),
            max_value: Some(10),
            ..SettingDef::default()
        };
        assert!(validate_value(&def, &json!(7)).is_ok());
        assert!(validate_value(&def, &json!(0)).is_ok());
        assert!(matches!(
            validate_value(&def, &json!(11)),
            Err(SettingError::OutOfRange { .. })
        ));
        def.kind = SettingType::Timespan;
        assert!(validate_value(&def, &json!(99_000)).is_ok());

        let color = SettingDef {
            id: "c".into(),
            kind: SettingType::Color,
            value: Some(json!("#ff0000")),
            package_value: Some(json!("#ff0000")),
            editor: Some("color".into()),
            package_editor: Some("color".into()),
            ..SettingDef::default()
        };
        let s = SettingsStore::new(vec![color]);
        let ctx = EditContext::admin();
        let mut e = EditSession::new("g");
        assert!(!e.can_reset(&s, &ctx, "c"));
        e.set_editor(&s, "c", "expression");
        assert!(e.is_changed(&s, "c"));
        assert!(e.can_reset(&s, &ctx, "c"));
        assert_eq!(e.change_set(&s)[0].editor.as_deref(), Some("expression"));
        e.reset(&s, "c");
        assert!(!e.is_dirty(&s));
    }

    #[test]
    fn ee_blocked_readonly_and_permissions() {
        let s = store();
        let e = EditSession::new("x");
        let admin = EditContext::admin();
        let ee = s
            .iter()
            .find(|d| d.enterprise && !d.modules.is_empty())
            .unwrap();
        assert!(admin.is_ee_locked(ee));
        assert!(e.is_disabled(&s, &admin, &ee.id));
        let licensed = EditContext {
            can_edit_all: true,
            has_license: true,
            active_modules: ee.modules.clone(),
            ..EditContext::default()
        };
        assert!(!licensed.is_ee_locked(ee));

        let ro = s.iter().find(|d| d.readonly).unwrap();
        assert!(e.is_readonly(&s, &ro.id));
        assert!(!e.can_reset(&s, &admin, &ro.id));

        let mut blocked = s.get("LDAP_Enable").unwrap().clone();
        blocked.blocked = true;
        let bs = SettingsStore::new(vec![blocked]);
        assert!(e.is_disabled(&bs, &admin, "LDAP_Enable"));

        let viewer = EditContext::default();
        assert!(viewer.is_read_only());
        assert!(e.is_disabled(&s, &viewer, "LDAP_Enable"));
        let selected = EditContext {
            editable_ids: ["LDAP_Enable".to_owned()].into(),
            ..EditContext::default()
        };
        assert!(!e.is_disabled(&s, &selected, "LDAP_Enable"));
        assert!(e.is_disabled(&s, &selected, "Accounts_AllowAnonymousRead"));
    }

    #[test]
    fn remote_changes_keep_local_edits() {
        let mut s = store();
        let mut e = EditSession::new("LDAP");
        e.set_value(&s, "LDAP_Host", json!("ldap.example.org"));
        e.set_value(&s, "LDAP_Port", json!(1389));
        assert!(e.remote_conflicts(&s).is_empty());

        // The server changes LDAP_Port underneath; LDAP_Host is untouched remotely.
        let mut port = s.get("LDAP_Port").unwrap().clone();
        port.value = Some(json!(2389));
        port.updated_at = RcDate::from_millis(4_000_000_000_000);
        s.apply_delta(
            DeltaRequest {
                since: None,
                generation: 0,
            },
            UpdatedSince {
                update: vec![port],
                ..UpdatedSince::default()
            },
        );
        assert_eq!(e.remote_conflicts(&s), vec!["LDAP_Port".to_owned()]);
        assert_eq!(e.effective(&s, "LDAP_Port"), Some(&json!(1389)));
        e.keep_mine(&s, "LDAP_Port");
        assert!(e.remote_conflicts(&s).is_empty());
        assert_eq!(s.last_updated(), RcDate::from_millis(4_000_000_000_000));

        // A save that is echoed back prunes the edits.
        let changes = e.change_set(&s);
        s.apply_saved(&changes);
        e.apply_saved(&s, &changes);
        assert!(!e.is_dirty(&s));
    }

    #[test]
    fn edits_during_save_are_not_conflicts() {
        let mut s = store();
        let mut e = EditSession::new("LDAP");
        e.set_value(&s, "LDAP_Host", json!("one"));
        let changes = e.change_set(&s);
        // Edited again while the save is in flight.
        e.set_value(&s, "LDAP_Host", json!("two"));
        s.apply_saved(&changes);
        e.apply_saved(&s, &changes);
        assert!(e.remote_conflicts(&s).is_empty());
        assert!(e.is_changed(&s, "LDAP_Host"));
        assert_eq!(e.effective(&s, "LDAP_Host"), Some(&json!("two")));
    }

    #[test]
    fn unknown_types_are_read_only() {
        let mut d = store().get("LDAP_Host").unwrap().clone();
        d.kind = SettingType::Unknown("future".into());
        d.package_value = Some(json!("default"));
        d.value = Some(json!("other"));
        let s = SettingsStore::new(vec![d]);
        let e = EditSession::new("LDAP");
        let admin = EditContext::admin();
        assert!(!e.is_editable(&s, &admin, "LDAP_Host"));
        assert!(!e.can_reset(&s, &admin, "LDAP_Host"));
    }

    #[test]
    fn debug_prints_no_values() {
        let s = store();
        let mut e = EditSession::new("LDAP");
        e.set_value(&s, "LDAP_Authentication_Password", json!("hunter2-typed"));
        let dbg = format!("{e:?} {s:?} {:?}", s.get("LDAP_Authentication_Password"));
        assert!(!dbg.contains("hunter2-typed"));
        assert!(dbg.contains("LDAP_Authentication_Password"));
    }

    #[test]
    fn error_keys_are_rustket_keys() {
        assert_eq!(
            SettingError::OutOfRange {
                min: Some(1),
                max: Some(5)
            }
            .i18n_key(),
            "admin.settings.error.out_of_range"
        );
        assert!(
            [
                SettingError::InvalidJson,
                SettingError::NotInteger,
                SettingError::WrongType
            ]
            .iter()
            .all(|e| e.i18n_key().starts_with("admin.settings.error."))
        );
    }

    #[test]
    fn stream_events_trigger_delta() {
        let mut s = store();
        assert_eq!(s.delta_request(), None);
        let (action, patch) = SettingPatch::from_stream_args(&[
            json!("updated"),
            json!({"_id": "LDAP_Host", "value": "x.example"}),
        ])
        .unwrap();
        s.apply_stream_event(action, patch);
        assert_eq!(s.value("LDAP_Host"), Some(&json!("x.example")));
        let req = s.delta_request().unwrap();
        assert_eq!(req.since, s.last_updated());
        s.apply_delta(req, UpdatedSince::default());
        assert_eq!(s.delta_request(), None);

        // An event while the delta is in flight keeps the flag set.
        let (a, p) = SettingPatch::from_stream_args(&[
            json!("updated"),
            json!({"_id": "LDAP_Host", "value": "y.example"}),
        ])
        .unwrap();
        s.apply_stream_event(a, p);
        let d1 = s.delta_request().unwrap();
        let (a, p) = SettingPatch::from_stream_args(&[
            json!("inserted"),
            json!({"_id": "Accounts_OAuth_Custom-New", "value": ""}),
        ])
        .unwrap();
        s.apply_stream_event(a, p);
        s.apply_delta(d1, UpdatedSince::default());
        let d2 = s.delta_request().unwrap();
        s.apply_delta(d2, UpdatedSince::default());
        assert_eq!(s.delta_request(), None);
        // EE settings are not merged from the (license-masked) event.
        let ee = s.iter().find(|d| d.enterprise).unwrap().id.clone();
        let before = s.value(&ee).cloned();
        let (a, p) = SettingPatch::from_stream_args(&[
            json!("updated"),
            json!({"_id": ee, "value": "masked", "enterprise": true}),
        ])
        .unwrap();
        s.apply_stream_event(a, p);
        assert_eq!(s.value(&ee).cloned(), before);
    }

    #[test]
    fn context_from_workspace() {
        use crate::workspace::Change;
        use rocketchat::models::Permission;
        let mut ws = Workspace::new();
        ws.apply_me(crate::test_util::me("me", "me"));
        ws.apply_permission(
            Change::Inserted,
            Permission {
                id: "change-setting-LDAP_Enable".into(),
                roles: vec!["user".into()],
                updated_at: None,
            },
        );
        ws.apply_permission(
            Change::Inserted,
            Permission {
                id: "change-setting-LDAP_Host".into(),
                roles: vec!["admin".into()],
                updated_at: None,
            },
        );
        // `change-setting-*` alone is not enough: the server also requires
        // `manage-selected-settings`.
        let ctx = EditContext::from_workspace(&ws, None);
        assert!(!ctx.can_edit_all);
        assert!(!ctx.can_edit("LDAP_Enable"));
        assert!(ctx.is_read_only());
        ws.apply_permission(
            Change::Inserted,
            Permission {
                id: "manage-selected-settings".into(),
                roles: vec!["user".into()],
                updated_at: None,
            },
        );
        let ctx = EditContext::from_workspace(&ws, None);
        assert!(ctx.can_edit("LDAP_Enable"));
        assert!(!ctx.can_edit("LDAP_Host"));
    }
}
