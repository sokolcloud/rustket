//! Generic settings editor (docs/admin.md §14): the private settings store
//! (`private-settings/get`, kept current by `private-settings-changed` plus a
//! debounced delta fetch), the group list with search, a group page with
//! tabs/sections and one widget per setting type, evaluated against the
//! edited state; reset / reset section / cancel, validation, alerts
//! confirmation, the bulk save with its 2FA prompt, remote-change conflicts,
//! action settings, assets, and the LDAP/OAuth/SAML extras.

mod group;
mod widgets;

use std::collections::{HashMap, HashSet};
use std::time::Duration;

use iced::widget::text_editor;
use iced::{Element, Subscription, Task};
use rocketchat::models::Setting;
use rocketchat::models::UpdatedSince;
use rocketchat::models::admin::{
    ActionResult, AdminRoom, LookupOption, SamlMetadata, SettingChange, SettingDef,
    SettingEventAction, SettingPatch, SettingType,
};
use rocketchat::session::ChangeKind;
use rust_i18n::t;
use rustket_core::admin::AdminSection;
use rustket_core::admin::settings_editor::{
    DeltaRequest, EditContext, EditSession, GroupIndex, GroupPage, OMITTED, SettingsStore,
};
use rustket_core::admin::timespan::{self, TimeUnit};
use serde_json::{Value, json};

use super::common::{Confirm, Cx, Picked, err_text, pick_file, strip_tags};
use super::guard::{Done, Op};
use super::{Message, V};
use crate::error::Res;

/// Bursts of stream events are coalesced into one delta fetch.
const DELTA_DEBOUNCE: Duration = Duration::from_millis(800);

#[derive(Debug, Clone)]
pub enum Msg {
    Loaded(Res<Vec<SettingDef>>),
    Delta(DeltaRequest, Res<UpdatedSince<SettingDef>>),
    Tick,
    Search(String),
    OpenGroup(String),
    Back,
    Tab(usize),
    ToggleSection(String),
    Set(String, Value),
    SetEditor(String, String),
    Text(String, String),
    Unit(String, TimeUnit),
    Editor(String, text_editor::Action),
    Multi(String, String, bool),
    Reveal(String),
    Reset(String),
    ResetSection(usize, usize),
    Cancel,
    Save,
    SaveConfirmed,
    KeepMine(String),
    TakeServer(String),
    Action(String),
    ActionNow(String),
    ActionDone(Res<ActionResult>),
    AssetPick(String),
    AssetPicked(String, Result<Option<Picked>, String>),
    AssetDelete(String),
    AssetDeleteNow(String),
    AssetDone(Res<()>),
    Lookup(String, Res<Vec<LookupOption>>),
    RoomQuery(String, String),
    RoomResults(String, Res<Vec<AdminRoom>>),
    RoomAdd(String, String, String),
    RoomRemove(String, String),
    RoomName(Res<Box<AdminRoom>>),
    Link(String),
    LdapTest,
    LdapSearch,
    LdapSearchNow,
    LdapSync,
    LdapSyncNow,
    LdapDone(Res<String>),
    AddOAuth,
    AddOAuthNow,
    RemoveOAuth(String),
    RemoveOAuthNow(String),
    RefreshOAuth,
    Saml,
    SamlNow,
    SamlParsed(Res<SamlMetadata>),
    DialogText(String),
    CloseDialog,
    DiscardAndGo(Option<AdminSection>),
    DiscardAndBack,
    DiscardAndClose,
}

/// Small dialogs of the settings page.
pub enum Dialog {
    /// Confirm a save of settings with alerts: (label, alert text).
    Alerts(Vec<(String, String)>),
    LdapUser(String),
    OAuthName(String),
    SamlUrl(String),
}

/// One open group page with its edit session and widget state.
pub struct GroupView {
    pub page: GroupPage,
    pub session: EditSession,
    pub tab: usize,
    pub collapsed: HashSet<String>,
    pub editors: HashMap<String, text_editor::Content>,
    pub texts: HashMap<String, String>,
    pub units: HashMap<String, TimeUnit>,
    pub reveal: HashSet<String>,
    pub errors: HashMap<String, String>,
    pub room_query: HashMap<String, String>,
    pub room_results: HashMap<String, Vec<AdminRoom>>,
    pub saving: bool,
    pub busy: bool,
}

impl GroupView {
    fn new(page: GroupPage, store: &SettingsStore) -> Self {
        let mut gv = Self {
            session: EditSession::new(&page.group),
            page,
            tab: 0,
            collapsed: HashSet::new(),
            editors: HashMap::new(),
            texts: HashMap::new(),
            units: HashMap::new(),
            reveal: HashSet::new(),
            errors: HashMap::new(),
            room_query: HashMap::new(),
            room_results: HashMap::new(),
            saving: false,
            busy: false,
        };
        // Collapsible sections start closed except the first one of each tab.
        for tab in &gv.page.tabs {
            for (i, s) in tab.sections.iter().enumerate() {
                if i > 0 && GroupPage::is_collapsible(tab, s) {
                    gv.collapsed.insert(s.name.clone());
                }
            }
        }
        gv.sync(store);
        gv
    }

    /// Brings the text widgets in line with the effective values (after
    /// resets, cancel, remote changes, metadata imports).
    fn sync(&mut self, store: &SettingsStore) {
        let ids: Vec<String> = self.page.setting_ids().map(str::to_owned).collect();
        for id in ids {
            let Some(def) = store.get(&id) else { continue };
            let value = self
                .session
                .effective(store, &id)
                .cloned()
                .unwrap_or(Value::Null);
            match def.kind {
                SettingType::Code => {
                    let text = value.as_str().unwrap_or_default();
                    let same = self.editors.get(&id).is_some_and(|c| {
                        c.text().trim_end_matches('\n') == text.trim_end_matches('\n')
                    });
                    if !same {
                        self.editors
                            .insert(id.clone(), text_editor::Content::with_text(text));
                    }
                }
                SettingType::String if def.multiline => {
                    let text = value.as_str().unwrap_or_default();
                    let same = self.editors.get(&id).is_some_and(|c| {
                        c.text().trim_end_matches('\n') == text.trim_end_matches('\n')
                    });
                    if !same {
                        self.editors
                            .insert(id.clone(), text_editor::Content::with_text(text));
                    }
                }
                SettingType::Int => {
                    let cur = self
                        .texts
                        .get(&id)
                        .and_then(|t| t.trim().parse::<i64>().ok());
                    if cur != value.as_i64() || !self.texts.contains_key(&id) {
                        self.texts.insert(
                            id.clone(),
                            value.as_i64().map(|n| n.to_string()).unwrap_or_default(),
                        );
                    }
                }
                SettingType::Timespan => {
                    // An invalid text (kept as a string value) stays as typed.
                    let Some(ms) = value.as_u64().or(value.is_null().then_some(0)) else {
                        continue;
                    };
                    let unit = *self
                        .units
                        .entry(id.clone())
                        .or_insert_with(|| timespan::highest_unit(ms));
                    let cur = self
                        .texts
                        .get(&id)
                        .and_then(|t| t.trim().parse::<f64>().ok())
                        .map(|n| timespan::to_ms(n, unit));
                    if cur != Some(ms) {
                        self.texts.insert(id.clone(), timespan_text(ms, unit));
                    }
                }
                _ => {}
            }
        }
    }

    /// Recomputes the inline validation errors from the edited values.
    fn revalidate(&mut self, store: &SettingsStore) {
        use rustket_core::admin::settings_editor::SettingError;
        self.errors = self
            .session
            .validate(store)
            .into_iter()
            .map(|(id, e)| {
                let text = match &e {
                    SettingError::OutOfRange { min, max } => t!(
                        e.i18n_key(),
                        min = min.unwrap_or_default(),
                        max = max.unwrap_or_default()
                    )
                    .into_owned(),
                    _ => t!(e.i18n_key()).into_owned(),
                };
                (id, text)
            })
            .collect();
    }

    /// Ids of invalid settings in display order.
    pub fn invalid_ids(&self) -> Vec<&str> {
        self.page
            .setting_ids()
            .filter(|id| self.errors.contains_key(*id))
            .collect()
    }

    /// Switches to the tab of the first invalid setting and expands its section.
    fn reveal_first_error(&mut self, store: &SettingsStore) {
        let Some(first) = self.invalid_ids().first().map(|s| (*s).to_owned()) else {
            return;
        };
        let _ = store;
        for (ti, tab) in self.page.tabs.iter().enumerate() {
            if let Some(sec) = tab.sections.iter().find(|s| s.setting_ids.contains(&first)) {
                self.tab = ti;
                self.collapsed.remove(&sec.name);
                return;
            }
        }
    }

    fn section_ids(
        &self,
        tab: usize,
        section: usize,
    ) -> Option<&rustket_core::admin::settings_editor::Section> {
        self.page.tabs.get(tab)?.sections.get(section)
    }
}

/// `ms` in `unit`, exact (fractions shown rather than truncated).
fn timespan_text(ms: u64, unit: TimeUnit) -> String {
    let n = timespan::from_ms(ms, unit);
    if n.fract() == 0.0 {
        format!("{}", n as u64)
    } else {
        format!("{n}")
    }
}

pub struct State {
    pub store: SettingsStore,
    pub index: GroupIndex,
    pub loaded: bool,
    pub error: Option<String>,
    pub search: String,
    pub group: Option<GroupView>,
    pub dialog: Option<Dialog>,
    pub delta_inflight: bool,
    pub edit_ctx: EditContext,
    pub lookups: HashMap<String, Vec<LookupOption>>,
    pub room_names: HashMap<String, String>,
    /// Bumped after asset uploads (cache buster of previews).
    pub asset_rev: u64,
}

impl Default for State {
    fn default() -> Self {
        Self::new()
    }
}

impl State {
    pub fn new() -> Self {
        Self {
            store: SettingsStore::default(),
            index: GroupIndex::default(),
            loaded: false,
            error: None,
            search: String::new(),
            group: None,
            dialog: None,
            delta_inflight: false,
            edit_ctx: EditContext::default(),
            lookups: HashMap::new(),
            room_names: HashMap::new(),
            asset_rev: 0,
        }
    }

    pub fn load(&mut self, cx: &mut Cx<'_, '_>) -> Task<Message> {
        self.edit_ctx = EditContext::from_workspace(cx.ctx.workspace, cx.license);
        cx.ctx.rest_task(
            |r| async move { r.private_settings().await },
            |r| Message::Settings(Msg::Loaded(r)),
        )
    }

    pub fn is_dirty(&self) -> bool {
        self.group
            .as_ref()
            .is_some_and(|g| g.session.is_dirty(&self.store))
    }

    pub fn discard(&mut self) {
        if let Some(g) = &mut self.group {
            g.session.cancel();
        }
    }

    /// Opens `group` directly (e.g. for snapshots and links).
    pub fn open_group(&mut self, group: &str, cx: &mut Cx<'_, '_>) -> Task<Message> {
        if self.index.is_stale(&self.store) {
            self.index = GroupIndex::build(&self.store);
        }
        let page = self.index.group_page(&self.store, group);
        let mut tasks = Vec::new();
        // Options of `lookup` settings and names of picked rooms.
        for id in page.setting_ids() {
            let Some(def) = self.store.get(id) else {
                continue;
            };
            match def.kind {
                SettingType::Lookup => {
                    if let Some(ep) = def.lookup_endpoint.clone()
                        && !self.lookups.contains_key(&ep)
                    {
                        let e2 = ep.clone();
                        tasks.push(cx.ctx.rest_task(
                            move |r| async move { r.settings_lookup(&ep).await },
                            move |r| Message::Settings(Msg::Lookup(e2, r)),
                        ));
                    }
                }
                SettingType::RoomPick => {
                    for rid in room_ids(def.value.as_ref()) {
                        if !self.room_names.contains_key(&rid) {
                            tasks.push(cx.ctx.rest_task(
                                move |r| async move { r.admin_room(&rid).await.map(Box::new) },
                                |r| Message::Settings(Msg::RoomName(r)),
                            ));
                        }
                    }
                }
                _ => {}
            }
        }
        self.group = Some(GroupView::new(page, &self.store));
        Task::batch(tasks)
    }

    /// A `private-settings-changed` event: partial merge, then a delta fetch.
    pub fn on_setting_event(&mut self, kind: ChangeKind, s: &Setting) {
        let action = match kind {
            ChangeKind::Inserted => SettingEventAction::Inserted,
            ChangeKind::Updated => SettingEventAction::Updated,
            ChangeKind::Removed => SettingEventAction::Removed,
        };
        let patch = SettingPatch {
            id: s.id.clone(),
            value: Some(s.value.clone()),
            enterprise: s.enterprise,
            ..SettingPatch::default()
        };
        self.store.apply_stream_event(action, patch);
    }

    pub fn subscription(&self) -> Subscription<Msg> {
        if self.loaded && !self.delta_inflight && self.store.delta_request().is_some() {
            iced::time::every(DELTA_DEBOUNCE).map(|_| Msg::Tick)
        } else {
            Subscription::none()
        }
    }

    fn after_change(&mut self) {
        if let Some(g) = &mut self.group {
            g.sync(&self.store);
            g.revalidate(&self.store);
        }
    }

    pub fn update(&mut self, m: Msg, cx: &mut Cx<'_, '_>) -> Task<Message> {
        // Permissions may change while the page is open.
        self.edit_ctx = EditContext::from_workspace(cx.ctx.workspace, cx.license);
        match m {
            Msg::Loaded(r) => {
                self.loaded = true;
                match r {
                    Ok(defs) => {
                        let defs = defs
                            .into_iter()
                            .filter(|d| !OMITTED.contains(&d.id.as_str()))
                            .collect();
                        self.store.replace_all(defs);
                        self.index = GroupIndex::build(&self.store);
                        self.error = None;
                    }
                    Err(f) => self.error = Some(err_text(&f, cx.i18n)),
                }
            }
            Msg::Tick => {
                if self.delta_inflight {
                    return Task::none();
                }
                let Some(req) = self.store.delta_request() else {
                    return Task::none();
                };
                self.delta_inflight = true;
                return cx.ctx.rest_task(
                    move |r| async move {
                        match req.since {
                            Some(since) => r.private_settings_since(since).await,
                            None => r.private_settings().await.map(|update| UpdatedSince {
                                update,
                                remove: Vec::new(),
                                dropped: 0,
                            }),
                        }
                    },
                    move |r| Message::Settings(Msg::Delta(req, r)),
                );
            }
            Msg::Delta(req, r) => {
                self.delta_inflight = false;
                match r {
                    Ok(delta) => {
                        self.store.apply_delta(req, delta);
                        if self.index.is_stale(&self.store) {
                            self.index = GroupIndex::build(&self.store);
                        }
                        // Settings added or removed in the open group (custom OAuth).
                        if let Some(g) = &mut self.group {
                            let page = self.index.group_page(&self.store, &g.page.group);
                            if page != g.page {
                                g.page = page;
                                g.tab = g.tab.min(g.page.tabs.len().saturating_sub(1));
                            }
                            g.session.prune(&self.store);
                        }
                        self.after_change();
                    }
                    Err(e) => tracing::debug!(error = %e, "settings delta failed"),
                }
            }
            Msg::Search(s) => self.search = s,
            Msg::OpenGroup(g) => return self.open_group(&g, cx),
            Msg::Back => {
                if self.is_dirty() {
                    cx.ask(Confirm::ask(
                        t!("admin.unsaved_title").into_owned(),
                        t!("admin.unsaved_body").into_owned(),
                        t!("admin.discard").into_owned(),
                        Message::Settings(Msg::DiscardAndBack),
                    ));
                    return Task::none();
                }
                self.group = None;
            }
            Msg::DiscardAndBack => {
                self.discard();
                self.group = None;
            }
            Msg::DiscardAndGo(_) | Msg::DiscardAndClose => {}
            Msg::Link(link) => return self.link(link, cx),
            m => {
                let task = self.group_update(m, cx);
                if let Some(g) = &mut self.group {
                    g.revalidate(&self.store);
                }
                return task;
            }
        }
        Task::none()
    }

    fn link(&mut self, link: String, cx: &mut Cx<'_, '_>) -> Task<Message> {
        let rel = link.trim_start_matches('/');
        if let Some(group) = rel.strip_prefix("admin/settings/") {
            let group = group.split(['/', '?', '#']).next().unwrap_or("").to_owned();
            if self.index.group(&group).is_some() && !self.is_dirty() {
                return self.open_group(&group, cx);
            }
        }
        let url = if link.starts_with("http://") || link.starts_with("https://") {
            link
        } else {
            format!("{}/{rel}", cx.ctx.server.as_str().trim_end_matches('/'))
        };
        cx.effects.push(crate::component::Effect::OpenUrl(url));
        Task::none()
    }

    fn group_update(&mut self, m: Msg, cx: &mut Cx<'_, '_>) -> Task<Message> {
        let store = &self.store;
        let ectx = &self.edit_ctx;
        let Some(g) = &mut self.group else {
            return Task::none();
        };
        match m {
            Msg::Tab(i) => g.tab = i,
            Msg::ToggleSection(s) => {
                if !g.collapsed.remove(&s) {
                    g.collapsed.insert(s);
                }
            }
            Msg::Set(id, v) => {
                if g.session.is_editable(store, ectx, &id) {
                    g.session.set_value(store, &id, v);
                    g.errors.remove(&id);
                }
            }
            Msg::SetEditor(id, e) => {
                if g.session.is_editable(store, ectx, &id) {
                    g.session.set_editor(store, &id, &e);
                }
            }
            Msg::Text(id, text) => {
                let Some(def) = store.get(&id) else {
                    return Task::none();
                };
                if !g.session.is_editable(store, ectx, &id) {
                    return Task::none();
                }
                let value = match def.kind {
                    SettingType::Timespan => {
                        let unit = g.units.get(&id).copied().unwrap_or(TimeUnit::Minutes);
                        // Whole non-negative numbers only (like the web input);
                        // anything else stays a string and fails validation.
                        match text.trim().parse::<u64>() {
                            Ok(n) => json!(n.saturating_mul(unit.millis())),
                            Err(_) if text.trim().is_empty() => json!(0),
                            Err(_) => Value::String(text.clone()),
                        }
                    }
                    // NaN / empty stays a string and fails validation (no NaN sent).
                    _ => match text.trim().parse::<i64>() {
                        Ok(n) => json!(n),
                        Err(_) => Value::String(text.clone()),
                    },
                };
                g.texts.insert(id.clone(), text);
                g.session.set_value(store, &id, value);
                g.errors.remove(&id);
            }
            Msg::Unit(id, unit) => {
                g.units.insert(id.clone(), unit);
                // The value stays; only its display changes.
                if let Some(ms) = g.session.effective(store, &id).and_then(Value::as_u64) {
                    g.texts.insert(id, timespan_text(ms, unit));
                }
            }
            Msg::Editor(id, action) => {
                let edit = action.is_edit();
                if edit && !g.session.is_editable(store, ectx, &id) {
                    return Task::none();
                }
                if let Some(c) = g.editors.get_mut(&id) {
                    c.perform(action);
                    if edit {
                        let text = c.text();
                        let orig_nl = g
                            .session
                            .effective(store, &id)
                            .and_then(Value::as_str)
                            .is_some_and(|s| s.ends_with('\n'));
                        let text = if !orig_nl && text.ends_with('\n') {
                            text.trim_end_matches('\n').to_owned()
                        } else {
                            text
                        };
                        g.session.set_value(store, &id, Value::String(text));
                        g.errors.remove(&id);
                    }
                }
            }
            Msg::Multi(id, key, on) => {
                if g.session.is_editable(store, ectx, &id) {
                    let mut list: Vec<Value> = g
                        .session
                        .effective(store, &id)
                        .and_then(Value::as_array)
                        .cloned()
                        .unwrap_or_default();
                    list.retain(|v| v.as_str() != Some(key.as_str()));
                    if on {
                        list.push(Value::String(key));
                    }
                    g.session.set_value(store, &id, Value::Array(list));
                }
            }
            Msg::Reveal(id) => {
                if !g.reveal.remove(&id) {
                    g.reveal.insert(id);
                }
            }
            Msg::Reset(id) => {
                g.session.reset(store, &id);
                g.sync(store);
            }
            Msg::ResetSection(t, s) => {
                if let Some(sec) = g.section_ids(t, s).cloned() {
                    g.session.reset_section(store, ectx, &sec);
                    g.sync(store);
                }
            }
            Msg::Cancel => {
                g.session.cancel();
                g.errors.clear();
                g.sync(store);
            }
            Msg::KeepMine(id) => g.session.keep_mine(store, &id),
            Msg::TakeServer(id) => {
                g.session.discard(&id);
                g.sync(store);
            }
            Msg::Save => {
                g.revalidate(store);
                if !g.errors.is_empty() {
                    g.reveal_first_error(store);
                    return Task::none();
                }
                let alerts: Vec<(String, String)> = g
                    .session
                    .alerts(store)
                    .into_iter()
                    .filter_map(|id| {
                        let def = store.get(&id)?;
                        Some((cx.i18n.label_for(def), strip_tags(cx.i18n.alert_for(def)?)))
                    })
                    .collect();
                if !alerts.is_empty() {
                    self.dialog = Some(Dialog::Alerts(alerts));
                    return Task::none();
                }
                return self.save(cx);
            }
            Msg::SaveConfirmed => {
                self.dialog = None;
                return self.save(cx);
            }
            Msg::Action(id) => {
                let Some(def) = store.get(&id) else {
                    return Task::none();
                };
                if def.value.as_ref().and_then(Value::as_str) == Some("restart_server") {
                    cx.ask(Confirm {
                        title: cx.i18n.label_for(def),
                        body: t!("admin.settings.restart_body").into_owned(),
                        danger: true,
                        yes_label: t!("admin.settings.run").into_owned(),
                        yes: Box::new(Message::Settings(Msg::ActionNow(id))),
                    });
                    return Task::none();
                }
                return self.group_update(Msg::ActionNow(id), cx);
            }
            Msg::ActionNow(id) => {
                let Some(v) = store.get(&id).and_then(|d| d.value.clone()) else {
                    return Task::none();
                };
                g.busy = true;
                return cx.ctx.rest_task(
                    move |r| async move {
                        match &v {
                            Value::String(method) => r.setting_action_method(method).await,
                            Value::Object(o) => {
                                let method =
                                    o.get("method").and_then(Value::as_str).unwrap_or("POST");
                                let path = o.get("path").and_then(Value::as_str).unwrap_or("");
                                r.setting_action_endpoint(method, path).await
                            }
                            _ => Ok(ActionResult::default()),
                        }
                    },
                    |r| Message::Settings(Msg::ActionDone(r)),
                );
            }
            Msg::ActionDone(r) => {
                g.busy = false;
                match r {
                    Ok(res) => {
                        let text = res
                            .message
                            .as_deref()
                            .map(|m| strip_tags(&cx.i18n.action_message(m, &res.params)))
                            .unwrap_or_else(|| t!("admin.done").into_owned());
                        cx.ok(text);
                    }
                    Err(f) => cx.fail(&f),
                }
            }
            Msg::AssetPick(id) => {
                let exts = store
                    .get(&id)
                    .and_then(|d| d.file_constraints.as_ref())
                    .map(|c| c.extensions.clone())
                    .unwrap_or_default();
                return pick_file(exts, 20 * 1024 * 1024, move |r| {
                    Message::Settings(Msg::AssetPicked(id.clone(), r))
                });
            }
            Msg::AssetPicked(id, r) => match r {
                Ok(Some(p)) => {
                    let Some(name) = store.get(&id).and_then(|d| d.asset.clone()) else {
                        return Task::none();
                    };
                    g.busy = true;
                    let upload = p.upload();
                    return cx.ctx.rest_task(
                        move |r| async move { r.assets_set(&name, upload, false).await },
                        |r| Message::Settings(Msg::AssetDone(r)),
                    );
                }
                Ok(None) => {}
                Err(e) => cx.effects.push(crate::component::Effect::Toast(
                    rustket_ui::widget::BannerKind::Error,
                    t!("admin.failed").into_owned(),
                    e,
                )),
            },
            Msg::AssetDelete(id) => {
                if let Some(def) = store.get(&id)
                    && let Some(name) = def.asset.clone()
                {
                    cx.ask(Confirm::danger(
                        t!("admin.settings.asset_delete").into_owned(),
                        t!(
                            "admin.settings.asset_delete_body",
                            name = cx.i18n.label_for(def)
                        )
                        .into_owned(),
                        Message::Settings(Msg::AssetDeleteNow(name)),
                    ));
                }
            }
            Msg::AssetDeleteNow(name) => {
                g.busy = true;
                return cx.ctx.rest_task(
                    move |r| async move { r.assets_unset(&name, false).await },
                    |r| Message::Settings(Msg::AssetDone(r)),
                );
            }
            Msg::AssetDone(r) => {
                g.busy = false;
                if cx.check(&r) {
                    self.asset_rev += 1;
                    cx.ok(t!("admin.settings.asset_saved").into_owned());
                }
            }
            Msg::Lookup(ep, r) => match r {
                Ok(opts) => {
                    self.lookups.insert(ep, opts);
                }
                Err(e) => tracing::debug!(error = %e, "lookup failed"),
            },
            Msg::RoomQuery(id, q) => {
                g.room_query.insert(id.clone(), q.clone());
                if q.trim().len() < 2 {
                    g.room_results.remove(&id);
                    return Task::none();
                }
                return cx.ctx.rest_task(
                    move |r| async move { r.admin_rooms_autocomplete(q.trim()).await },
                    move |r| Message::Settings(Msg::RoomResults(id.clone(), r)),
                );
            }
            Msg::RoomResults(id, Ok(rooms)) => {
                g.room_results.insert(id, rooms);
            }
            Msg::RoomAdd(id, rid, name) => {
                if g.session.is_editable(store, ectx, &id) {
                    let mut ids = room_ids(g.session.effective(store, &id));
                    if !ids.contains(&rid) {
                        ids.push(rid.clone());
                    }
                    self.room_names.insert(rid, name);
                    let v = Value::Array(ids.into_iter().map(|i| json!({ "_id": i })).collect());
                    g.session.set_value(store, &id, v);
                    g.room_query.remove(&id);
                    g.room_results.remove(&id);
                }
            }
            Msg::RoomRemove(id, rid) => {
                if g.session.is_editable(store, ectx, &id) {
                    let ids: Vec<Value> = room_ids(g.session.effective(store, &id))
                        .into_iter()
                        .filter(|i| *i != rid)
                        .map(|i| json!({ "_id": i }))
                        .collect();
                    g.session.set_value(store, &id, Value::Array(ids));
                }
            }
            Msg::RoomName(Ok(room)) => {
                self.room_names.insert(room.id.clone(), room.display_name());
            }
            Msg::LdapTest => {
                g.busy = true;
                return cx.ctx.rest_task(
                    |r| async move { r.ldap_test_connection().await },
                    |r| Message::Settings(Msg::LdapDone(r)),
                );
            }
            Msg::LdapSearch => self.dialog = Some(Dialog::LdapUser(String::new())),
            Msg::LdapSearchNow => {
                if let Some(Dialog::LdapUser(u)) = self.dialog.take() {
                    g.busy = true;
                    return cx.ctx.rest_task(
                        move |r| async move {
                            // The web tests the connection first.
                            r.ldap_test_connection().await?;
                            r.ldap_test_search(u.trim()).await
                        },
                        |r| Message::Settings(Msg::LdapDone(r)),
                    );
                }
            }
            Msg::LdapSync => cx.ask(Confirm::ask(
                t!("admin.settings.ldap_sync").into_owned(),
                cx.i18n
                    .t("LDAP_Sync_Now_Description")
                    .map(strip_tags)
                    .unwrap_or_else(|| t!("admin.settings.ldap_sync_body").into_owned()),
                t!("admin.settings.run").into_owned(),
                Message::Settings(Msg::LdapSyncNow),
            )),
            Msg::LdapSyncNow => return cx.guarded(Op::LdapSync),
            Msg::LdapDone(r) => {
                g.busy = false;
                match r {
                    Ok(m) => cx.ok(strip_tags(cx.i18n.t_or_key(&m))),
                    Err(f) => cx.fail(&f),
                }
            }
            Msg::AddOAuth => self.dialog = Some(Dialog::OAuthName(String::new())),
            Msg::AddOAuthNow => {
                if let Some(Dialog::OAuthName(n)) = &self.dialog {
                    let name = n.trim().to_owned();
                    if name.is_empty() {
                        return Task::none();
                    }
                    self.dialog = None;
                    return cx.guarded(Op::AddCustomOAuth(name));
                }
            }
            Msg::RemoveOAuth(name) => cx.ask(Confirm::danger(
                t!("admin.settings.oauth_remove").into_owned(),
                t!("admin.settings.oauth_remove_body", name = name.clone()).into_owned(),
                Message::Settings(Msg::RemoveOAuthNow(name)),
            )),
            Msg::RemoveOAuthNow(name) => {
                return cx.guarded(Op::RemoveCustomOAuth(name.to_lowercase()));
            }
            Msg::RefreshOAuth => return cx.guarded(Op::RefreshOAuth),
            Msg::Saml => self.dialog = Some(Dialog::SamlUrl(String::new())),
            Msg::SamlNow => {
                if let Some(Dialog::SamlUrl(u)) = self.dialog.take() {
                    g.busy = true;
                    return cx.ctx.rest_task(
                        move |r| async move { r.saml_parse_metadata(u.trim()).await },
                        |r| Message::Settings(Msg::SamlParsed(r)),
                    );
                }
            }
            Msg::SamlParsed(r) => {
                g.busy = false;
                match r {
                    Ok(meta) => {
                        let mut any = false;
                        for (id, v) in [
                            ("SAML_Custom_Default_entry_point", meta.entry_point),
                            (
                                "SAML_Custom_Default_idp_slo_redirect_url",
                                meta.idp_slo_redirect_url,
                            ),
                            ("SAML_Custom_Default_cert", meta.cert),
                            (
                                "SAML_Custom_Default_identifier_format",
                                meta.identifier_format,
                            ),
                        ] {
                            if let Some(v) = v.filter(|v| !v.is_empty())
                                && store.get(id).is_some()
                            {
                                g.session.set_value(store, id, Value::String(v));
                                any = true;
                            }
                        }
                        g.sync(store);
                        if any {
                            cx.ok(t!("admin.settings.saml_loaded").into_owned());
                        } else {
                            cx.ok(cx.i18n.t_or_key("SAML_Metadata_no_values").to_owned());
                        }
                    }
                    Err(f) => cx.fail(&f),
                }
            }
            Msg::DialogText(s) => {
                if let Some(Dialog::LdapUser(x) | Dialog::OAuthName(x) | Dialog::SamlUrl(x)) =
                    &mut self.dialog
                {
                    *x = s;
                }
            }
            Msg::CloseDialog => self.dialog = None,
            _ => {}
        }
        Task::none()
    }

    fn save(&mut self, cx: &mut Cx<'_, '_>) -> Task<Message> {
        let Some(g) = &mut self.group else {
            return Task::none();
        };
        let changes = g.session.change_set(&self.store);
        if changes.is_empty() {
            return Task::none();
        }
        g.saving = true;
        cx.guarded(Op::SaveSettings(changes))
    }

    pub fn on_guarded(&mut self, op: &Op, r: &Res<Done>, cx: &mut Cx<'_, '_>) -> Task<Message> {
        match (op, r) {
            (Op::SaveSettings(changes), Ok(_)) => {
                self.saved(changes);
                cx.ok(cx
                    .i18n
                    .t("Settings_updated")
                    .map(str::to_owned)
                    .unwrap_or_else(|| t!("admin.settings.saved").into_owned()));
            }
            (Op::SaveSettings(_), Err(f)) => {
                if let Some(g) = &mut self.group {
                    g.saving = false;
                }
                cx.fail(f);
            }
            (Op::LdapSync, Ok(Done::Text(m))) => cx.ok(strip_tags(cx.i18n.t_or_key(m))),
            (Op::AddCustomOAuth(_) | Op::RemoveCustomOAuth(_) | Op::RefreshOAuth, Ok(_)) => {
                cx.ok(t!("admin.done").into_owned());
                // New/removed settings arrive with the next delta.
                self.store
                    .apply_stream_event(SettingEventAction::Other, SettingPatch::default());
            }
            (_, r) => {
                cx.check(r);
            }
        }
        Task::none()
    }

    fn saved(&mut self, changes: &[SettingChange]) {
        self.store.apply_saved(changes);
        if let Some(g) = &mut self.group {
            g.saving = false;
            g.session.apply_saved(&self.store, changes);
            g.sync(&self.store);
        }
    }

    pub fn on_guard_cancelled(&mut self, op: &Op) {
        if let (Op::SaveSettings(_), Some(g)) = (op, &mut self.group) {
            g.saving = false;
        }
    }

    pub fn view<'a>(&'a self, v: &V<'a>) -> Element<'a, Message> {
        match &self.group {
            None => group::list_view(self, v),
            Some(g) => group::page_view(self, g, v),
        }
    }

    pub fn footer<'a>(&'a self, v: &V<'a>) -> Option<Element<'a, Message>> {
        let g = self.group.as_ref()?;
        group::footer(self, g, v)
    }

    pub fn overlay<'a>(&'a self, v: &V<'a>) -> Option<Element<'a, Message>> {
        group::dialog_view(self, v)
    }
}

/// Room ids of a `roomPick` value (`[{_id, name?}]` or `""`).
pub fn room_ids(v: Option<&Value>) -> Vec<String> {
    v.and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|r| r.get("_id").and_then(Value::as_str).map(str::to_owned))
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn def(v: Value) -> SettingDef {
        serde_json::from_value(v).unwrap()
    }

    fn store() -> SettingsStore {
        SettingsStore::new(vec![
            def(json!({"_id": "Accounts", "type": "group", "i18nLabel": "Accounts"})),
            def(
                json!({"_id": "A_bool", "type": "boolean", "group": "Accounts", "value": true, "packageValue": true, "sorter": 1}),
            ),
            def(
                json!({"_id": "A_int", "type": "int", "group": "Accounts", "value": 5, "packageValue": 5, "sorter": 2}),
            ),
            def(
                json!({"_id": "A_span", "type": "timespan", "group": "Accounts", "value": 7_200_000, "packageValue": 7_200_000, "sorter": 3}),
            ),
            def(
                json!({"_id": "A_code", "type": "code", "code": "application/json", "group": "Accounts", "value": "{}", "packageValue": "{}", "sorter": 4}),
            ),
        ])
    }

    #[test]
    fn widget_texts_follow_values() {
        let s = store();
        let index = GroupIndex::build(&s);
        let page = index.group_page(&s, "Accounts");
        let mut g = GroupView::new(page, &s);
        assert_eq!(g.texts["A_int"], "5");
        assert_eq!(g.units["A_span"], TimeUnit::Hours);
        assert_eq!(g.texts["A_span"], "2");
        assert_eq!(g.editors["A_code"].text().trim_end(), "{}");
        g.session.set_value(&s, "A_int", json!(9));
        g.sync(&s);
        assert_eq!(g.texts["A_int"], "9");
        g.session.cancel();
        g.sync(&s);
        assert_eq!(g.texts["A_int"], "5");
    }

    #[test]
    fn room_pick_values() {
        assert_eq!(
            room_ids(Some(&json!([{"_id": "a"}, {"_id": "b", "name": "x"}]))),
            ["a", "b"]
        );
        assert!(room_ids(Some(&json!(""))).is_empty());
    }
}
