//! Omnichannel (livechat) agent state (`docs/omnichannel.md`). Per server, owned
//! next to the [`Workspace`]: REST results (routing config, license, departments,
//! queue snapshot, canned responses, custom fields) are stored here, and
//! [`OmnichannelState::recompute`] derives the flags of §2 from the workspace
//! (settings, permissions, `me`).
//!
//! Driven by the UI: call [`OmnichannelState::recompute_update`] after
//! `apply_me` (before applying further events) and after settings, permissions
//! or roles change, and act on the returned [`OmnichannelUpdate`] like on the
//! one from [`OmnichannelState::apply_event`].
//!
//! Departments: when [`OmnichannelState::wants_departments`] is true, load
//! `livechat/agents/{uid}/departments` *without* `enabledDepartmentsOnly`
//! ([`OmnichannelState::set_departments`]); the queue uses the enabled ones,
//! canned responses all of them (the server includes disabled departments there).
//!
//! Queue initialisation (§5.1): when [`OmnichannelState::wants_queue`] is true,
//! load the departments, subscribe `rocketchat::session::inquiry_queue_subscriptions`,
//! call [`queue::InquiryQueue::begin_snapshot`], load
//! `livechat/inquiries.queuedForUser?count=<pool size>` and pass it to
//! [`queue::InquiryQueue::apply_snapshot`]. Repeat on `reinit_queue`.

pub mod canned;
pub mod permissions;
pub mod queue;
pub mod room;
pub mod room_list;

use std::collections::HashSet;

use rocketchat::models::{
    AgentDepartment, AgentStatus, CustomFieldDef, LicenseInfo, Priority, RoutingConfig,
};
use rocketchat::session::{OmnichannelEvent, SessionEvent};

use self::canned::{CannedContext, CannedResponses};
use self::permissions as perm;
use self::permissions::settings;
use self::queue::{InquiryQueue, QueueContext, QueueUpdate, SortMechanism};
use crate::permissions::has_permission;
use crate::workspace::Workspace;

/// What an event did to the omnichannel state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct OmnichannelUpdate {
    /// Something visible changed (redraw).
    pub changed: bool,
    /// A new inquiry entered the queue (new-room sound).
    pub queue_grew: bool,
    /// Re-run the queue initialisation (§5.1 step 4).
    pub reinit_queue: bool,
    /// Re-fetch `livechat/config/routing` (routing method changed).
    pub refetch_routing: bool,
    /// Re-fetch the priorities (EE).
    pub refetch_priorities: bool,
    /// Re-fetch `licenses.info` (omnichannel became accessible).
    pub refetch_license: bool,
    /// Re-fetch `livechat/agents/{uid}/departments` and then
    /// `canned-responses.get` (department membership or canned visibility changed).
    pub refetch_canned: bool,
}

impl OmnichannelUpdate {
    /// Combines two updates (any flag set in either).
    pub fn merge(self, o: Self) -> Self {
        Self {
            changed: self.changed || o.changed,
            queue_grew: self.queue_grew || o.queue_grew,
            reinit_queue: self.reinit_queue || o.reinit_queue,
            refetch_routing: self.refetch_routing || o.refetch_routing,
            refetch_priorities: self.refetch_priorities || o.refetch_priorities,
            refetch_license: self.refetch_license || o.refetch_license,
            refetch_canned: self.refetch_canned || o.refetch_canned,
        }
    }
}

/// Omnichannel state of one server.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct OmnichannelState {
    /// `Livechat_enabled` and `view-l-room`.
    pub accessible: bool,
    /// `accessible`, logged in and the routing config loaded: show omnichannel UI.
    pub enabled: bool,
    /// Roles include `livechat-agent` (show the availability toggle only then).
    pub is_agent: bool,
    pub agent_status: AgentStatus,
    /// `livechat/config/routing`; `None` until loaded.
    pub routing: Option<RoutingConfig>,
    /// `licenses.info`; `None` until loaded (or not readable).
    pub license: Option<LicenseInfo>,
    /// `license.preventedActions.monthlyActiveContacts`.
    pub over_mac: bool,
    /// Manual-selection queue visible (§2 `manual_queue`).
    pub manual_queue: bool,
    /// Departments of the agent (enabled and disabled; see `department_enabled`).
    pub departments: Vec<AgentDepartment>,
    pub queue: InquiryQueue,
    pub canned: CannedResponses,
    pub custom_fields: Vec<CustomFieldDef>,
    pub priorities: Vec<Priority>,
    /// [`Workspace::me_generation`] seen by the last `recompute`.
    last_me_generation: u64,
    /// `statusLivechat` of a `MeChanged` since that `me` (recorded even while the
    /// user is not an agent yet: the roles change may be applied later).
    userdata_status: Option<AgentStatus>,
}

impl OmnichannelState {
    pub fn new() -> Self {
        Self::default()
    }

    /// Recomputes the §2 flags and the queue/canned contexts from the workspace.
    /// Call after `Dirty::ME | SETTINGS | PERMISSIONS | ROLES` changes and after
    /// storing REST results. Returns whether anything changed; see
    /// [`Self::recompute_update`] for the follow-up requests.
    pub fn recompute(&mut self, ws: &Workspace) -> bool {
        self.recompute_update(ws).changed
    }

    /// [`Self::recompute`], also reporting what to (re)load: the routing config
    /// and license when omnichannel became accessible, the queue when it became
    /// wanted, canned responses when their context changed.
    pub fn recompute_update(&mut self, ws: &Workspace) -> OmnichannelUpdate {
        let before = self.clone();
        let me = ws.me();
        self.accessible = ws.settings().bool_or(settings::ENABLED, false)
            && has_permission(ws, perm::VIEW_L_ROOM, None);
        self.enabled = self.accessible && me.is_some() && self.routing.is_some();
        self.is_agent = me.is_some_and(|m| m.has_role(perm::ROLE_AGENT));
        // A `/me` fetched after the last `userData` wins, even when it equals the
        // previous `/me`; otherwise `userData` is newer.
        if ws.me_generation() != self.last_me_generation {
            self.last_me_generation = ws.me_generation();
            self.userdata_status = None;
        }
        let me_status = me.and_then(|m| m.status_livechat);
        self.agent_status = match self.userdata_status.or(me_status) {
            Some(s) if self.is_agent => s,
            _ => AgentStatus::NotAvailable,
        };
        self.over_mac = self
            .license
            .as_ref()
            .is_some_and(LicenseInfo::over_mac_limit);
        self.manual_queue = self.enabled
            && has_permission(ws, perm::VIEW_LIVECHAT_QUEUE, None)
            && self
                .routing
                .is_some_and(|r| r.show_queue && !r.auto_assign_agent)
            && self.agent_status.is_available();

        let me_id = ws.user_id().unwrap_or_default().to_owned();
        let departments: HashSet<String> = self
            .department_ids()
            .into_iter()
            .map(str::to_owned)
            .collect();
        let cap = ws.settings().i64_or(settings::POOL_MAX_DISPLAYED, 0).max(0);
        self.queue.set_context(QueueContext {
            me: me_id.clone(),
            departments: departments.clone(),
            sort: SortMechanism::from_setting(ws.settings().str(settings::SORTING_MECHANISM)),
            cap: usize::try_from(cap).unwrap_or(usize::MAX),
        });
        if !self.manual_queue {
            self.queue.clear();
        }
        self.canned.set_context(CannedContext {
            me: me_id,
            departments: self
                .departments
                .iter()
                .map(|d| d.department_id.clone())
                .collect(),
            view_all: has_permission(ws, perm::VIEW_ALL_CANNED_RESPONSES, None),
        });
        let became_accessible = self.accessible && !before.accessible;
        OmnichannelUpdate {
            changed: *self != before,
            reinit_queue: self.manual_queue && !before.manual_queue,
            refetch_routing: became_accessible && self.routing.is_none(),
            refetch_license: became_accessible && self.license.is_none(),
            refetch_canned: self.canned.context() != before.canned.context()
                && self.canned_enabled(ws),
            ..Default::default()
        }
    }

    /// Whether the UI should keep `livechat/agents/{uid}/departments` loaded
    /// (the queue filter and canned-response stream events need it).
    pub fn wants_departments(&self, ws: &Workspace) -> bool {
        self.enabled && (self.wants_queue() || self.canned_enabled(ws))
    }

    /// Whether the UI should run the queue initialisation and keep the queue
    /// streams subscribed.
    pub fn wants_queue(&self) -> bool {
        self.manual_queue
    }

    pub fn set_routing(&mut self, routing: Option<RoutingConfig>) {
        self.routing = routing;
    }

    pub fn set_license(&mut self, license: Option<LicenseInfo>) {
        self.license = license;
    }

    /// Stores `livechat/agents/{uid}/departments`; call [`Self::recompute`] next.
    pub fn set_departments(&mut self, departments: Vec<AgentDepartment>) {
        self.departments = departments;
    }

    /// Enabled department ids (for the queue stream keys).
    pub fn department_ids(&self) -> Vec<&str> {
        self.departments
            .iter()
            .filter(|d| d.department_enabled)
            .map(|d| d.department_id.as_str())
            .collect()
    }

    pub fn has_module(&self, module: &str) -> bool {
        self.license.as_ref().is_some_and(|l| l.has_module(module))
    }

    /// Canned responses are usable in an omnichannel room (§16): license
    /// module, `Canned_Responses_Enable` and `view-canned-responses`.
    pub fn canned_enabled(&self, ws: &Workspace) -> bool {
        self.has_module(LicenseInfo::CANNED_RESPONSES)
            && ws
                .settings()
                .bool_or(settings::CANNED_RESPONSES_ENABLE, true)
            && has_permission(ws, perm::VIEW_CANNED_RESPONSES, None)
    }

    /// Applies a session event. Call [`Self::recompute_update`] afterwards when
    /// the workspace changed too.
    pub fn apply_event(&mut self, ev: &SessionEvent) -> OmnichannelUpdate {
        let mut u = OmnichannelUpdate {
            reinit_queue: self.manual_queue && queue::needs_reinit(ev),
            ..Default::default()
        };
        // An unexpected `statusLivechat` (already from `/me`) is ignored.
        if let Some(status) = ev.agent_status()
            && status != AgentStatus::Unknown
        {
            self.userdata_status = Some(status);
            if self.is_agent && status != self.agent_status {
                self.agent_status = status;
                u.changed = true;
                // Availability gates the manual queue.
                u.reinit_queue = true;
            }
        }
        // A load that failed earlier is tried again after a reconnect.
        if matches!(ev, SessionEvent::Resync { .. }) && self.accessible {
            u.refetch_routing |= self.routing.is_none();
            u.refetch_license |= self.license.is_none();
        }
        if let SessionEvent::SettingChanged { setting, .. } = ev
            && matches!(
                setting.id.as_str(),
                settings::ROUTING_METHOD | settings::ENABLED
            )
        {
            u.refetch_routing = true;
        }
        match ev.omnichannel() {
            Some(ref oe @ OmnichannelEvent::InquiryQueue { .. }) => {
                if self.manual_queue {
                    let shown = self.queue.shown_len();
                    if self.queue.apply_event(oe) != QueueUpdate::Unchanged {
                        u.changed = true;
                        // Only growth of the displayed (capped) list sounds.
                        u.queue_grew = self.queue.shown_len() > shown;
                    }
                }
            }
            Some(OmnichannelEvent::DepartmentAgentChanged(_)) => {
                u.reinit_queue = self.manual_queue;
                u.refetch_canned = true;
            }
            Some(
                ref oe @ (OmnichannelEvent::CannedResponseChanged { .. }
                | OmnichannelEvent::CannedResponseRemoved { .. }),
            ) => {
                u.changed |= self.canned.apply_event(oe);
            }
            Some(OmnichannelEvent::PriorityChanged { .. }) => u.refetch_priorities = true,
            Some(OmnichannelEvent::RoomData(_)) | None => {}
        }
        u
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use crate::test_util::me;
    use crate::workspace::Change;
    use rocketchat::models::{Inquiry, InquiryEventKind, InquiryStatus, Permission, Setting};
    use rocketchat::session::{ChangeKind, ResyncReason};
    use serde_json::json;

    fn perm(id: &str, roles: &[&str]) -> Permission {
        serde_json::from_value(json!({"_id": id, "roles": roles})).unwrap()
    }

    fn agent_ws(roles: &[&str]) -> Workspace {
        let mut ws = Workspace::new();
        let mut m = me("me", "me");
        m.roles = Some(roles.iter().map(|r| (*r).to_owned()).collect());
        m.status_livechat = Some(AgentStatus::Available);
        ws.apply_me(m);
        ws.apply_setting(
            Change::Inserted,
            Setting {
                id: settings::ENABLED.into(),
                value: json!(true),
                ..Default::default()
            },
        );
        for p in [perm::VIEW_L_ROOM, perm::VIEW_LIVECHAT_QUEUE] {
            ws.apply_permission(
                Change::Inserted,
                self::perm(p, &["livechat-agent", "livechat-manager", "admin"]),
            );
        }
        ws
    }

    fn manual() -> RoutingConfig {
        RoutingConfig {
            preview_room: true,
            show_queue: true,
            return_queue: true,
            ..Default::default()
        }
    }

    fn auto() -> RoutingConfig {
        RoutingConfig {
            auto_assign_agent: true,
            show_queue_link: true,
            enable_trigger_action: true,
            ..Default::default()
        }
    }

    /// A routing config / license load that failed is retried on Resync.
    #[test]
    fn resync_refetches_failed_loads() {
        let ws = agent_ws(&["user", "livechat-agent"]);
        let mut o = OmnichannelState::new();
        let u = o.recompute_update(&ws);
        assert!(u.refetch_routing && u.refetch_license);
        // Both loads failed: nothing else asks for them again but a Resync.
        assert!(!o.recompute_update(&ws).refetch_routing);
        let resync = SessionEvent::Resync {
            reason: ResyncReason::Reconnected,
        };
        let u = o.apply_event(&resync);
        assert!(u.refetch_routing && u.refetch_license);
        o.set_routing(Some(manual()));
        o.set_license(Some(LicenseInfo::default()));
        o.recompute(&ws);
        let u = o.apply_event(&resync);
        assert!(!u.refetch_routing && !u.refetch_license);
    }

    #[test]
    fn flags_follow_section_2() {
        let ws = agent_ws(&["user", "livechat-agent"]);
        let mut o = OmnichannelState::new();
        assert!(o.recompute(&ws));
        assert!(o.accessible && o.is_agent);
        assert!(!o.enabled, "routing config not loaded yet");
        o.set_routing(Some(auto()));
        o.recompute(&ws);
        assert!(o.enabled && !o.manual_queue);
        assert_eq!(o.agent_status, AgentStatus::Available);
        o.set_routing(Some(manual()));
        o.recompute(&ws);
        assert!(o.manual_queue && o.wants_queue());
        assert!(!o.recompute(&ws), "idempotent");

        // Admin without the agent role: accessible, but no toggle and no queue.
        let ws = agent_ws(&["user", "admin"]);
        let mut a = OmnichannelState::new();
        a.set_routing(Some(manual()));
        a.recompute(&ws);
        assert!(a.enabled && !a.is_agent && !a.manual_queue);
        assert_eq!(a.agent_status, AgentStatus::NotAvailable);

        // Livechat disabled.
        let mut ws = agent_ws(&["user", "livechat-agent"]);
        ws.apply_setting(
            Change::Updated,
            Setting {
                id: settings::ENABLED.into(),
                value: json!(false),
                ..Default::default()
            },
        );
        let mut d = OmnichannelState::new();
        d.set_routing(Some(manual()));
        d.recompute(&ws);
        assert!(!d.accessible && !d.enabled && !d.manual_queue);

        // License / MAC.
        let mut l = OmnichannelState::new();
        l.set_license(Some(
            serde_json::from_value(json!({"activeModules":["livechat-enterprise"],
                "preventedActions":{"monthlyActiveContacts":true}}))
            .unwrap(),
        ));
        l.recompute(&agent_ws(&["livechat-agent"]));
        assert!(l.over_mac && l.has_module(LicenseInfo::LIVECHAT_ENTERPRISE));
        assert!(!l.canned_enabled(&agent_ws(&["livechat-agent"])));
    }

    #[test]
    fn availability_events_gate_queue() {
        let ws = agent_ws(&["livechat-agent"]);
        let mut o = OmnichannelState::new();
        o.set_routing(Some(manual()));
        o.recompute(&ws);
        o.queue.replace(vec![Inquiry {
            id: "i".into(),
            rid: "r".into(),
            status: InquiryStatus::Queued,
            ..Default::default()
        }]);
        assert_eq!(o.queue.len(), 1);
        let u = o.apply_event(&status_event("not-available"));
        assert!(u.changed && u.reinit_queue);
        assert_eq!(o.agent_status, AgentStatus::NotAvailable);
        o.recompute(&ws);
        assert!(!o.manual_queue, "unavailable: no manual queue");
        assert!(o.queue.is_empty());
        // `me` did not change, so the userData status sticks.
        assert_eq!(o.agent_status, AgentStatus::NotAvailable);
        let u = o.apply_event(&status_event("available "));
        assert!(u.changed);
        o.recompute(&ws);
        assert!(o.manual_queue);
        let u = o.apply_event(&status_event("bogus"));
        assert!(!u.changed);
    }

    #[test]
    fn queue_and_misc_events() {
        let ws = agent_ws(&["livechat-agent"]);
        let mut o = OmnichannelState::new();
        o.set_routing(Some(manual()));
        o.recompute(&ws);
        let ev = SessionEvent::Unknown {
            collection: rocketchat::session::streams::INQUIRY_QUEUE.into(),
            event_name: "public".into(),
            args: json!([{"type":"added","_id":"i1","rid":"r1","status":"queued","name":"V"}]),
        };
        let u = o.apply_event(&ev);
        assert!(u.changed && u.queue_grew && !u.reinit_queue);
        assert_eq!(o.queue.items()[0].id, "i1");
        let u = o.apply_event(&SessionEvent::Unknown {
            collection: "stream-notify-user".into(),
            event_name: "me/departmentAgentData".into(),
            args: json!([{"action":"inserted","_id":"x","agentId":"me","departmentId":"d"}]),
        });
        assert!(u.reinit_queue);
        let u = o.apply_event(&SessionEvent::Resync {
            reason: ResyncReason::Reconnected,
        });
        assert!(u.reinit_queue);
        let u = o.apply_event(&SessionEvent::SettingChanged {
            kind: ChangeKind::Updated,
            setting: Box::new(Setting {
                id: settings::ROUTING_METHOD.into(),
                value: json!("Auto_Selection"),
                ..Default::default()
            }),
            public: true,
        });
        assert!(u.refetch_routing && u.reinit_queue);
        let u = o.apply_event(&SessionEvent::Unknown {
            collection: "stream-notify-logged".into(),
            event_name: "omnichannel.priority-changed".into(),
            args: json!([{"id":"p","clientAction":true}]),
        });
        assert!(u.refetch_priorities);

        // Departments feed the queue filter.
        o.set_departments(vec![AgentDepartment {
            department_id: "d1".into(),
            department_enabled: true,
            ..Default::default()
        }]);
        o.recompute(&ws);
        assert_eq!(o.department_ids(), ["d1"]);
        let dept = Inquiry {
            id: "i2".into(),
            rid: "r2".into(),
            status: InquiryStatus::Queued,
            department: Some("d1".into()),
            ..Default::default()
        };
        assert_eq!(
            o.queue.apply(InquiryEventKind::Changed, dept),
            QueueUpdate::Inserted
        );
    }

    /// The user re-read from `/me` after a `userData` hint.
    fn status_event(s: &str) -> SessionEvent {
        SessionEvent::MeChanged(Box::new(
            serde_json::from_value(json!({"_id": "me", "statusLivechat": s})).unwrap(),
        ))
    }

    #[test]
    fn fresh_me_wins_over_older_user_data() {
        let mut ws = agent_ws(&["livechat-agent"]);
        let mut o = OmnichannelState::new();
        o.set_routing(Some(manual()));
        o.recompute(&ws);
        o.apply_event(&status_event("not-available"));
        o.recompute(&ws);
        assert_eq!(o.agent_status, AgentStatus::NotAvailable);
        // Resync re-reads `/me` (same document as at login): it is newer.
        let me = ws.me().cloned().unwrap();
        ws.apply_me(me);
        let u = o.recompute_update(&ws);
        assert_eq!(o.agent_status, AgentStatus::Available);
        assert!(o.manual_queue && u.reinit_queue, "queue became wanted");
    }

    #[test]
    fn user_data_before_agent_role_is_kept() {
        let mut ws = agent_ws(&["user"]);
        let mut o = OmnichannelState::new();
        o.set_routing(Some(manual()));
        o.recompute(&ws);
        assert!(!o.is_agent);
        // Made an agent: roles-change, then userData; both applied before the
        // next recompute.
        ws.apply_user_role_change("me", "livechat-agent", true, None);
        let u = o.apply_event(&status_event("not-available"));
        assert!(!u.changed, "not an agent yet");
        o.recompute(&ws);
        assert!(o.is_agent);
        assert_eq!(o.agent_status, AgentStatus::NotAvailable);
        let u = o.apply_event(&status_event("available"));
        assert!(u.changed);
        let u = o.recompute_update(&ws);
        assert_eq!(o.agent_status, AgentStatus::Available);
        assert!(o.manual_queue && u.reinit_queue);
    }

    #[test]
    fn becoming_accessible_requests_routing_and_license() {
        let mut ws = agent_ws(&["livechat-agent"]);
        ws.apply_setting(
            Change::Updated,
            Setting {
                id: settings::ENABLED.into(),
                value: json!(false),
                ..Default::default()
            },
        );
        let mut o = OmnichannelState::new();
        let u = o.recompute_update(&ws);
        assert!(!o.accessible && !u.refetch_routing);
        let u = o.apply_event(&SessionEvent::SettingChanged {
            kind: ChangeKind::Updated,
            setting: Box::new(Setting {
                id: settings::ENABLED.into(),
                value: json!(true),
                ..Default::default()
            }),
            public: true,
        });
        assert!(u.refetch_routing);
        ws.apply_setting(
            Change::Updated,
            Setting {
                id: settings::ENABLED.into(),
                value: json!(true),
                ..Default::default()
            },
        );
        let u = o.recompute_update(&ws);
        assert!(o.accessible && !o.enabled);
        assert!(u.refetch_routing && u.refetch_license);
        o.set_routing(Some(auto()));
        let u = o.recompute_update(&ws);
        assert!(o.enabled && !u.refetch_routing);
    }

    #[test]
    fn foreign_role_change_does_not_reinit() {
        let ws = agent_ws(&["livechat-agent"]);
        let mut o = OmnichannelState::new();
        o.set_routing(Some(manual()));
        o.recompute(&ws);
        let u = o.apply_event(&SessionEvent::RolesChanged(Box::new(
            rocketchat::session::RoleAssignment {
                kind: "added".into(),
                role: "moderator".into(),
                user: None,
                scope: Some("GENERAL".into()),
            },
        )));
        assert!(!u.reinit_queue);
        assert!(!o.recompute_update(&ws).reinit_queue, "queue still wanted");
    }

    #[test]
    fn canned_department_responses_under_auto_routing() {
        let ws = agent_ws(&["livechat-agent"]);
        let mut o = OmnichannelState::new();
        o.set_routing(Some(auto()));
        o.recompute(&ws);
        let r: rocketchat::models::CannedResponse = serde_json::from_value(json!({
            "_id":"c1","shortcut":"hi","text":"Hello","scope":"department","departmentId":"d1"
        }))
        .unwrap();
        o.canned.replace(vec![r]);
        o.recompute(&ws);
        assert_eq!(o.canned.len(), 1);
        let u = o.apply_event(&SessionEvent::Unknown {
            collection: "stream-notify-user".into(),
            event_name: "me/departmentAgentData".into(),
            args: json!([{"action":"inserted","_id":"x","agentId":"me","departmentId":"d2"}]),
        });
        assert!(u.refetch_canned && !u.reinit_queue);
        // Disabled departments still count for canned responses.
        o.set_departments(vec![AgentDepartment {
            department_id: "d3".into(),
            department_enabled: false,
            ..Default::default()
        }]);
        o.recompute(&ws);
        assert!(o.canned.context().departments.contains("d3"));
        assert!(o.department_ids().is_empty());
    }

    #[test]
    fn queue_grew_follows_the_capped_list() {
        let mut ws = agent_ws(&["livechat-agent"]);
        ws.apply_setting(
            Change::Inserted,
            Setting {
                id: settings::POOL_MAX_DISPLAYED.into(),
                value: json!(1),
                ..Default::default()
            },
        );
        let mut o = OmnichannelState::new();
        o.set_routing(Some(manual()));
        o.recompute(&ws);
        let added = |id: &str, ts: i64| SessionEvent::Unknown {
            collection: rocketchat::session::streams::INQUIRY_QUEUE.into(),
            event_name: "public".into(),
            args: json!([{"type":"added","_id":id,"rid":format!("r{id}"),"status":"queued",
                "ts":{"$date":ts}}]),
        };
        assert!(o.apply_event(&added("a", 1)).queue_grew);
        let u = o.apply_event(&added("b", 2));
        assert!(u.changed && !u.queue_grew, "beyond the cap");
    }
}
