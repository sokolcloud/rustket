//! The manual-selection inquiry queue (`docs/omnichannel.md` §5.1–5.2): a REST
//! snapshot (`livechat/inquiries.queuedForUser`) kept current by
//! `stream-livechat-inquiry-queue-observer` events.

use std::collections::{HashMap, HashSet};

use rocketchat::RcDate;
use rocketchat::models::{Inquiry, InquiryEventKind, InquiryStatus};
use rocketchat::session::{OmnichannelEvent, SessionEvent};

use super::permissions::settings;

/// `priorityWeight` of an inquiry without priority.
pub const NO_PRIORITY_WEIGHT: i64 = 99;
/// `estimatedWaitingTimeQueue` of an inquiry without SLA.
pub const NO_SLA_MINUTES: i64 = 9_999_999;

/// `Omnichannel_sorting_mechanism` (EE; absent on CE = `Timestamp`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SortMechanism {
    /// `ts ↑, _updatedAt ↓`.
    #[default]
    Timestamp,
    /// `priorityWeight ↑, ts ↑, _updatedAt ↓`.
    Priority,
    /// `estimatedWaitingTimeQueue ↑, ts ↑, _updatedAt ↓`.
    Slas,
}

impl SortMechanism {
    pub fn from_setting(v: Option<&str>) -> Self {
        match v {
            Some("Priority") => Self::Priority,
            Some("SLAs") => Self::Slas,
            _ => Self::Timestamp,
        }
    }
}

/// Who the queue is filtered for.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct QueueContext {
    /// Current user id.
    pub me: String,
    /// Enabled departments of the agent.
    pub departments: HashSet<String>,
    pub sort: SortMechanism,
    /// `Livechat_guest_pool_max_number_incoming_livechats_displayed`; 0 = no cap.
    pub cap: usize,
}

/// What [`InquiryQueue::apply`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QueueUpdate {
    Unchanged,
    /// A new inquiry became visible (play the new-room sound).
    Inserted,
    Updated,
    Removed,
}

/// Queued inquiries visible to the agent.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct InquiryQueue {
    ctx: QueueContext,
    by_id: HashMap<String, Inquiry>,
    revision: u64,
}

fn ms(d: Option<RcDate>) -> i64 {
    d.map_or(0, |d| d.timestamp_millis())
}

impl InquiryQueue {
    pub fn new(ctx: QueueContext) -> Self {
        Self {
            ctx,
            ..Default::default()
        }
    }

    pub fn context(&self) -> &QueueContext {
        &self.ctx
    }

    /// Bumped on every visible change.
    pub fn revision(&self) -> u64 {
        self.revision
    }

    pub fn len(&self) -> usize {
        self.by_id.len()
    }

    pub fn is_empty(&self) -> bool {
        self.by_id.is_empty()
    }

    pub fn clear(&mut self) {
        if !self.by_id.is_empty() {
            self.by_id.clear();
            self.revision += 1;
        }
    }

    /// Whether `inq` belongs into this agent's queue (the server's
    /// `queuedForUser` filter applied to stream events).
    pub fn is_visible(&self, inq: &Inquiry) -> bool {
        if inq.status != InquiryStatus::Queued || inq.id.is_empty() || inq.rid.is_empty() {
            return false;
        }
        if let Some(agent) = &inq.default_agent
            && !agent.agent_id.is_empty()
        {
            return agent.agent_id == self.ctx.me;
        }
        match inq.department.as_deref().filter(|d| !d.is_empty()) {
            Some(d) => self.ctx.departments.contains(d),
            None => true,
        }
    }

    /// Changes the context and drops inquiries that are no longer visible.
    /// Returns whether anything changed. (Newly visible ones need a reload.)
    pub fn set_context(&mut self, ctx: QueueContext) -> bool {
        if ctx == self.ctx {
            return false;
        }
        self.ctx = ctx;
        let ids: Vec<String> = self
            .by_id
            .values()
            .filter(|i| !self.is_visible(i))
            .map(|i| i.id.clone())
            .collect();
        for id in ids {
            self.by_id.remove(&id);
        }
        // Sort or cap may have changed even when nothing was dropped.
        self.revision += 1;
        true
    }

    /// Replaces the queue with a REST snapshot (after subscribing the streams).
    pub fn replace(&mut self, snapshot: Vec<Inquiry>) {
        self.by_id.clear();
        for inq in snapshot {
            if self.is_visible(&inq) {
                self.insert_dedup(inq);
            }
        }
        self.revision += 1;
    }

    fn insert_dedup(&mut self, inq: Inquiry) {
        // One inquiry per room: a different id for the same room replaces it.
        let stale: Vec<String> = self
            .by_id
            .values()
            .filter(|o| o.rid == inq.rid && o.id != inq.id)
            .map(|o| o.id.clone())
            .collect();
        for id in stale {
            self.by_id.remove(&id);
        }
        self.by_id.insert(inq.id.clone(), inq);
    }

    /// Applies one stream event. `added` is treated like `changed`.
    pub fn apply(&mut self, kind: InquiryEventKind, inq: Inquiry) -> QueueUpdate {
        if kind == InquiryEventKind::Removed || !self.is_visible(&inq) {
            return if self.by_id.remove(&inq.id).is_some() {
                self.revision += 1;
                QueueUpdate::Removed
            } else {
                QueueUpdate::Unchanged
            };
        }
        let update = match self.by_id.get(&inq.id) {
            Some(old) if *old == inq => return QueueUpdate::Unchanged,
            Some(old)
                if old.updated_at.is_some()
                    && inq.updated_at.is_some()
                    && inq.updated_at < old.updated_at =>
            {
                return QueueUpdate::Unchanged;
            }
            Some(_) => QueueUpdate::Updated,
            None => QueueUpdate::Inserted,
        };
        self.insert_dedup(inq);
        self.revision += 1;
        update
    }

    /// Applies a decoded session event; non-queue events are ignored.
    pub fn apply_event(&mut self, ev: &OmnichannelEvent) -> QueueUpdate {
        match ev {
            OmnichannelEvent::InquiryQueue { kind, inquiry, .. } => {
                self.apply(*kind, (**inquiry).clone())
            }
            _ => QueueUpdate::Unchanged,
        }
    }

    /// Visible inquiries, sorted by the mechanism and capped.
    pub fn items(&self) -> Vec<&Inquiry> {
        let mut v: Vec<&Inquiry> = self.by_id.values().collect();
        let sort = self.ctx.sort;
        v.sort_by(|a, b| {
            let primary = match sort {
                SortMechanism::Timestamp => std::cmp::Ordering::Equal,
                SortMechanism::Priority => a
                    .priority_weight
                    .unwrap_or(NO_PRIORITY_WEIGHT)
                    .cmp(&b.priority_weight.unwrap_or(NO_PRIORITY_WEIGHT)),
                SortMechanism::Slas => a
                    .estimated_waiting_time_queue
                    .unwrap_or(NO_SLA_MINUTES)
                    .cmp(&b.estimated_waiting_time_queue.unwrap_or(NO_SLA_MINUTES)),
            };
            primary
                .then_with(|| ms(a.ts).cmp(&ms(b.ts)))
                .then_with(|| ms(b.updated_at).cmp(&ms(a.updated_at)))
                .then_with(|| a.id.cmp(&b.id))
        });
        if self.ctx.cap > 0 {
            v.truncate(self.ctx.cap);
        }
        v
    }

    pub fn get(&self, id: &str) -> Option<&Inquiry> {
        self.by_id.get(id)
    }

    /// The queued inquiry of room `rid`.
    pub fn for_room(&self, rid: &str) -> Option<&Inquiry> {
        self.by_id.values().find(|i| i.rid == rid)
    }
}

/// Whether `ev` requires re-running the queue initialisation (departments,
/// `queuedForUser`, stream set): reconnect/resync, department membership,
/// availability, or a change of the routing method, pool size or sort setting.
pub fn needs_reinit(ev: &SessionEvent) -> bool {
    match ev {
        SessionEvent::Resync { .. } => true,
        SessionEvent::UserData(_) => ev.agent_status().is_some(),
        SessionEvent::SettingChanged { setting, .. } => matches!(
            setting.id.as_str(),
            settings::ROUTING_METHOD | settings::POOL_MAX_DISPLAYED | settings::SORTING_MECHANISM
        ),
        SessionEvent::RolesChanged(_) | SessionEvent::PermissionChanged { .. } => true,
        _ => matches!(
            ev.omnichannel(),
            Some(OmnichannelEvent::DepartmentAgentChanged(_))
        ),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use rocketchat::models::SelectedAgent;

    fn ts(ms: i64) -> Option<RcDate> {
        RcDate::from_millis(ms)
    }

    fn inq(id: &str, rid: &str, t: i64) -> Inquiry {
        Inquiry {
            id: id.into(),
            rid: rid.into(),
            name: format!("Visitor {id}"),
            status: InquiryStatus::Queued,
            ts: ts(t),
            updated_at: ts(t),
            ..Default::default()
        }
    }

    fn ctx(depts: &[&str]) -> QueueContext {
        QueueContext {
            me: "me".into(),
            departments: depts.iter().map(|d| (*d).to_owned()).collect(),
            ..Default::default()
        }
    }

    fn ids(q: &InquiryQueue) -> Vec<&str> {
        q.items().iter().map(|i| i.id.as_str()).collect()
    }

    #[test]
    fn visibility_filter() {
        let q = InquiryQueue::new(ctx(&["d1"]));
        let mut i = inq("a", "r", 1);
        assert!(q.is_visible(&i), "no department: public");
        i.department = Some("d1".into());
        assert!(q.is_visible(&i));
        i.department = Some("d2".into());
        assert!(!q.is_visible(&i), "foreign department");
        i.department = None;
        i.default_agent = Some(SelectedAgent {
            agent_id: "other".into(),
            username: None,
        });
        assert!(!q.is_visible(&i), "another agent's default chat");
        i.default_agent = Some(SelectedAgent {
            agent_id: "me".into(),
            username: None,
        });
        i.department = Some("d2".into());
        assert!(q.is_visible(&i), "default agent wins over department");
        let mut t = inq("b", "r2", 1);
        t.status = InquiryStatus::Taken;
        assert!(!q.is_visible(&t));
        let mut v = inq("c", "r3", 1);
        v.status = InquiryStatus::Verifying;
        assert!(!q.is_visible(&v));
        assert!(!q.is_visible(&inq("d", "", 1)), "no rid: never a room");
        // Without departments, department inquiries are excluded (server rule).
        let q = InquiryQueue::new(ctx(&[]));
        let mut i = inq("a", "r", 1);
        i.department = Some("d1".into());
        assert!(!q.is_visible(&i));
    }

    #[test]
    fn events_and_dedupe() {
        let mut q = InquiryQueue::new(ctx(&["d1"]));
        assert_eq!(
            q.apply(InquiryEventKind::Added, inq("a", "r1", 10)),
            QueueUpdate::Inserted
        );
        assert_eq!(
            q.apply(InquiryEventKind::Changed, inq("a", "r1", 10)),
            QueueUpdate::Unchanged
        );
        let mut newer = inq("a", "r1", 10);
        newer.updated_at = ts(20);
        newer.message = Some("hi".into());
        assert_eq!(
            q.apply(InquiryEventKind::Changed, newer),
            QueueUpdate::Updated
        );
        // Older update is ignored.
        let mut older = inq("a", "r1", 10);
        older.updated_at = ts(15);
        older.message = Some("old".into());
        assert_eq!(
            q.apply(InquiryEventKind::Changed, older),
            QueueUpdate::Unchanged
        );
        assert_eq!(q.get("a").unwrap().message.as_deref(), Some("hi"));
        // Taken -> removed; removed twice is harmless (close sends it twice).
        let mut taken = inq("a", "r1", 10);
        taken.status = InquiryStatus::Taken;
        taken.updated_at = ts(30);
        assert_eq!(
            q.apply(InquiryEventKind::Changed, taken),
            QueueUpdate::Removed
        );
        assert_eq!(
            q.apply(InquiryEventKind::Removed, inq("a", "r1", 10)),
            QueueUpdate::Unchanged
        );
        // Re-queued after a return/forward: visible again.
        let mut requeued = inq("a", "r1", 10);
        requeued.updated_at = ts(40);
        assert_eq!(
            q.apply(InquiryEventKind::Changed, requeued),
            QueueUpdate::Inserted
        );
        // Same room, different inquiry id: replaced, not duplicated.
        assert_eq!(
            q.apply(InquiryEventKind::Changed, inq("b", "r1", 50)),
            QueueUpdate::Inserted
        );
        assert_eq!(ids(&q), ["b"]);
        assert_eq!(q.for_room("r1").unwrap().id, "b");
        // Moved to a foreign department: dropped.
        let mut moved = inq("b", "r1", 50);
        moved.department = Some("d9".into());
        moved.updated_at = ts(60);
        assert_eq!(
            q.apply(InquiryEventKind::Changed, moved),
            QueueUpdate::Removed
        );
        assert!(q.is_empty());
    }

    #[test]
    fn snapshot_sort_and_cap() {
        let mut q = InquiryQueue::new(ctx(&["d1"]));
        let mut foreign = inq("x", "rx", 1);
        foreign.department = Some("d2".into());
        let mut b = inq("b", "rb", 200);
        b.priority_weight = Some(1);
        b.estimated_waiting_time_queue = Some(5);
        let mut c = inq("c", "rc", 100);
        c.priority_weight = Some(3);
        let mut c2 = inq("c2", "rc2", 100);
        c2.updated_at = ts(500);
        q.replace(vec![inq("a", "ra", 300), b, c, c2, foreign]);
        assert_eq!(q.len(), 4);
        // Timestamp: ts ↑, then _updatedAt ↓.
        assert_eq!(ids(&q), ["c2", "c", "b", "a"]);
        let mut cx = q.context().clone();
        cx.sort = SortMechanism::Priority;
        q.set_context(cx.clone());
        assert_eq!(ids(&q), ["b", "c", "c2", "a"]);
        cx.sort = SortMechanism::Slas;
        q.set_context(cx.clone());
        assert_eq!(ids(&q), ["b", "c2", "c", "a"]);
        cx.cap = 2;
        q.set_context(cx.clone());
        assert_eq!(ids(&q), ["b", "c2"]);
        assert_eq!(q.len(), 4, "the cap only limits what is shown");
        // After leaving d1, d1 inquiries are not accepted.
        cx.departments.clear();
        q.set_context(cx);
        let mut d = inq("d", "rd", 1);
        d.department = Some("d1".into());
        q.apply(InquiryEventKind::Added, d);
        assert!(q.get("d").is_none());
        assert_eq!(
            SortMechanism::from_setting(Some("SLAs")),
            SortMechanism::Slas
        );
        assert_eq!(SortMechanism::from_setting(None), SortMechanism::Timestamp);
    }

    #[test]
    fn captured_snapshot() {
        let v = crate::test_util::fixture("omnichannel/inquiries_queued_for_user.json");
        let list: Vec<Inquiry> = serde_json::from_value(v["inquiries"].clone()).unwrap();
        let dept = list[0].department.clone().unwrap();
        let mut q = InquiryQueue::new(ctx(&[]));
        q.replace(list.clone());
        assert!(
            q.is_empty(),
            "department inquiry, agent without departments"
        );
        let mut q = InquiryQueue::new(ctx(&[dept.as_str()]));
        q.replace(list);
        assert_eq!(q.items()[0].rid, "CS2PFviwDBkDoQDWP");
    }

    #[test]
    fn context_change_refilters() {
        let mut q = InquiryQueue::new(ctx(&["d1"]));
        let mut a = inq("a", "ra", 1);
        a.department = Some("d1".into());
        q.replace(vec![a, inq("b", "rb", 2)]);
        assert_eq!(q.len(), 2);
        assert!(q.set_context(ctx(&[])));
        assert_eq!(ids(&q), ["b"]);
        assert!(!q.set_context(ctx(&[])));
    }

    #[test]
    fn reinit_triggers() {
        use rocketchat::models::Setting;
        use rocketchat::session::{ChangeKind, ResyncReason};
        use serde_json::json;
        assert!(needs_reinit(&SessionEvent::Resync {
            reason: ResyncReason::Reconnected
        }));
        assert!(needs_reinit(&SessionEvent::UserData(
            json!({"type":"updated","diff":{"statusLivechat":"available"}})
        )));
        assert!(!needs_reinit(&SessionEvent::UserData(
            json!({"type":"updated","diff":{"name":"x"}})
        )));
        let setting = |id: &str| SessionEvent::SettingChanged {
            kind: ChangeKind::Updated,
            setting: Box::new(Setting {
                id: id.into(),
                ..Default::default()
            }),
            public: true,
        };
        assert!(needs_reinit(&setting(settings::POOL_MAX_DISPLAYED)));
        assert!(needs_reinit(&setting(settings::ROUTING_METHOD)));
        assert!(!needs_reinit(&setting("Site_Name")));
        assert!(needs_reinit(&SessionEvent::Unknown {
            collection: "stream-notify-user".into(),
            event_name: "u/departmentAgentData".into(),
            args: json!([{"action":"removed","_id":"x","agentId":"u","departmentId":"d"}]),
        }));
    }
}
