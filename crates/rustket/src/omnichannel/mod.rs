//! Omnichannel (livechat) agent UI (`docs/omnichannel.md` §22.3), on top of
//! `rustket_core::omnichannel` and the `rocketchat` livechat layer.
//! Strings: `locales/omnichannel.yml` (`omnichannel.*`).
//!
//! - [`Omni`] (per server, in `ServerState::omni`) keeps the
//!   [`OmnichannelState`]: routing config, license, departments, queue,
//!   canned responses, custom fields; it is recomputed after `me`, settings,
//!   permissions or roles changed and fed with the session events
//!   (`glue.rs`, hooked into `App::update`). Views read the published
//!   snapshot ([`shared`]).
//! - [`status`]: availability toggle in the status menu.
//! - [`sidebar`]: queue, "Chats in progress" and "On hold" groups.
//! - [`room`]: composer callouts, header quick actions and [`dialogs`].
//! - [`panel`]: room info, edit, contact, history, canned responses.
//! - [`contact_center`]: contacts and chats, in place of the room area.
//! - [`system`]: omnichannel system message texts; [`canned`]: `!` autocomplete.

pub mod canned;
pub mod contact_center;
pub mod dialogs;
mod glue;
pub mod panel;
pub mod room;
pub mod shared;
pub mod sidebar;
pub mod status;
pub mod system;

use std::collections::HashSet;
use std::sync::Arc;

use iced::Task;
use rocketchat::models::{
    AgentDepartment, CannedResponse, CustomFieldDef, Inquiry, LicenseInfo, Paginated, Priority,
    Room, RoutingConfig,
};
use rocketchat::rest::LivechatPage;
use rocketchat::session::{
    StreamWatch, canned_responses_subscription, inquiry_queue_subscriptions,
};
use rust_i18n::t;
use rustket_core::omnichannel::permissions::settings;
use rustket_core::omnichannel::queue::SnapshotToken;
use rustket_core::omnichannel::{OmnichannelState, OmnichannelUpdate};
use rustket_core::{Dirty, ServerId, Workspace};

use crate::error::{Failure, Res};
use crate::room::RoomView;
use crate::screens::directory::open;
use crate::session::SessionHandle;

pub use glue::AgentSignOut;
pub use status::is_available_agent;

/// Default queue page when the pool size setting is 0 (`count=0` is only
/// "unlimited" with `API_Allow_Infinite_Count`, which agents cannot read).
const QUEUE_COUNT: u64 = 100;

/// A stream subscription guard that can travel in a message.
#[derive(Debug, Clone)]
pub struct Watch(pub Arc<StreamWatch>);

#[derive(Debug, Clone)]
pub enum Message {
    Routing(Res<RoutingConfig>),
    License(Res<LicenseInfo>),
    Departments(Res<Vec<AgentDepartment>>),
    Queue(SnapshotToken, Res<Paginated<Inquiry>>),
    QueueWatch(Vec<String>, Res<Watch>),
    CannedWatch(Res<Watch>),
    CustomFields(Res<Paginated<CustomFieldDef>>),
    Canned(Res<Vec<CannedResponse>>),
    Priorities(Res<Paginated<Priority>>),
    /// Back-off of a failed load ended (session `Arc` address, timer token).
    Retry(usize, Load, u64),
    /// A room fetched for opening (queued chat preview, contact center).
    Opened(Box<Res<open::Opened>>),
    /// A closed chat whose view stays open, re-read after its subscription
    /// was removed (`closedAt`, `open: false`).
    Refreshed(Box<Res<open::Opened>>),
    ContactCenter(contact_center::Msg),
}

/// A session-wide load retried after a failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Load {
    Routing,
    License,
}

/// Wait before retry number `attempt` (1-based) of a failed load: 5 s,
/// doubling, at most 5 min.
fn retry_delay(attempt: u32) -> std::time::Duration {
    let secs = 5u64.saturating_mul(1 << attempt.saturating_sub(1).min(6));
    std::time::Duration::from_secs(secs.min(300))
}

/// Omnichannel controller of one server.
#[derive(Default)]
pub struct Omni {
    pub state: OmnichannelState,
    /// The session the loads belong to (`Arc` address).
    session: Option<usize>,
    /// Workspace revision (`me`, settings, permissions, roles) of the last recompute.
    rev: Option<u64>,
    routing_loading: bool,
    license_loading: bool,
    departments_loading: bool,
    /// Failed attempts in a row (retry back-off).
    routing_failures: u32,
    license_failures: u32,
    /// Token of the one pending retry timer per load (others are stale).
    routing_retry: Option<u64>,
    license_retry: Option<u64>,
    retry_seq: u64,
    /// After the departments arrived: start the queue snapshot / load canned responses.
    queue_after_departments: bool,
    canned_after_departments: bool,
    fields_loaded: bool,
    queue_watch: Option<Watch>,
    /// Stream names wanted for the queue (watch pending or held).
    queue_keys: Vec<String>,
    canned_watch: Option<Watch>,
    canned_wanted: bool,
    pub contact_center: Option<contact_center::ContactCenter>,
    /// Livechat subscriptions seen (the new-room sound is for new ones).
    seen_live: HashSet<String>,
    /// Views of chats whose subscription was just removed but that stay open
    /// (`omnichannelHideConversationAfterClosing` off, or a dialog request in
    /// flight): put back with their room after the removal was applied.
    parked: Vec<(Room, Box<RoomView>)>,
    /// The state changed since the last publish.
    dirty: bool,
}

/// User-facing text of a failed omnichannel call, with wording for the
/// codes of `docs/omnichannel.md` §3–§16.
pub fn error_text(f: &Failure) -> String {
    let e = f.error();
    if e.is_enterprise_only() {
        return t!("omnichannel.err.enterprise").into_owned();
    }
    let key = match e.livechat_error_code() {
        Some("error-comment-is-required") => "omnichannel.err.comment_required",
        Some("error-tags-must-be-assigned-before-closing-chat") => "omnichannel.err.tags_required",
        Some(
            "error-not-authorized" | "error-not-allowed" | "error-unauthorized" | "Not_authorized",
        ) => "omnichannel.err.not_allowed",
        Some(
            "error-room-already-closed" | "This_conversation_is_already_closed" | "room-closed",
        ) => "omnichannel.err.already_closed",
        Some("error-inquiry-taken") => "omnichannel.err.inquiry_taken",
        Some("error-agent-status-service-offline") => "omnichannel.err.agent_offline",
        Some("error-user-is-offline") => "omnichannel.err.target_offline",
        Some("error-forwarding-chat-same-department") => "omnichannel.err.same_department",
        Some("error-no-agents-available-for-service-on-department") => {
            "omnichannel.err.no_agents_in_department"
        }
        Some("error-selected-agent-room-agent-are-same") => "omnichannel.err.same_agent",
        Some("error-room-onHold") => "omnichannel.err.on_hold",
        Some("error-mac-limit-reached") => "omnichannel.err.mac_limit",
        Some("error-transcript-already-requested") => "omnichannel.err.transcript_requested",
        Some("error-business-hours-are-closed") => "omnichannel.err.business_hours",
        Some("Agent not found") => "omnichannel.err.not_agent",
        _ => return f.user_text(),
    };
    t!(key).into_owned()
}

/// The new-room sound of the user (`newRoomNotification`, default `door`);
/// `None` when switched off, when Rustket's "Play a sound" setting
/// (`play_sounds`) is off or when the notification or master volume is zero
/// (like the message sound, `desktop::notifications::sound_allowed`).
pub fn new_room_sound(ws: &Workspace, play_sounds: bool) -> Option<String> {
    if !play_sounds {
        return None;
    }
    let muted = ["notificationsSoundVolume", "masterVolume"]
        .iter()
        .any(|k| {
            crate::desktop::user_pref(ws, k)
                .and_then(serde_json::Value::as_f64)
                .is_some_and(|v| v <= 0.0)
        });
    if muted {
        return None;
    }
    let name = crate::desktop::user_pref(ws, "newRoomNotification")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("door");
    (name != "none").then(|| name.to_owned())
}

impl Omni {
    /// Back to the logged-out state (logout, session dropped).
    pub fn reset(&mut self, sid: &ServerId) {
        *self = Self::default();
        shared::forget(sid);
    }

    /// Publishes the state when it changed since the last publish.
    fn publish(&mut self, sid: &ServerId) {
        if std::mem::take(&mut self.dirty) {
            shared::publish(sid, &self.state);
        }
    }

    /// Marks the state changed (published after the current update).
    pub fn touch(&mut self) {
        self.dirty = true;
    }

    fn rest_task<T: Send + 'static>(
        h: &SessionHandle,
        call: impl FnOnce(
            rocketchat::RestClient,
        )
            -> std::pin::Pin<Box<dyn Future<Output = rocketchat::Result<T>> + Send>>
        + Send
        + 'static,
        map: impl FnOnce(Res<T>) -> Message + Send + 'static,
    ) -> Task<Message> {
        let rest = h.rest().clone();
        Task::perform(async move { call(rest).await.map_err(Failure::from) }, map)
    }

    /// Per update: (re)starts the loads for a new session, recomputes after
    /// workspace changes and keeps the stream watches in line.
    pub fn tick(
        &mut self,
        sid: &ServerId,
        ws: &Workspace,
        session: Option<&SessionHandle>,
    ) -> Task<Message> {
        let Some(h) = session else {
            if self.session.is_some() {
                self.session = None;
                self.queue_watch = None;
                self.canned_watch = None;
                self.queue_keys.clear();
                self.canned_wanted = false;
                self.routing_loading = false;
                self.license_loading = false;
                self.departments_loading = false;
                self.routing_failures = 0;
                self.license_failures = 0;
                self.routing_retry = None;
                self.license_retry = None;
            }
            return Task::none();
        };
        if ws.me().is_none() {
            return Task::none();
        }
        let ptr = Arc::as_ptr(&h.0) as usize;
        let new_session = self.session != Some(ptr);
        let mut u = OmnichannelUpdate::default();
        if new_session {
            self.session = Some(ptr);
            self.rev = None;
            self.queue_watch = None;
            self.canned_watch = None;
            self.queue_keys.clear();
            self.canned_wanted = false;
            self.routing_loading = false;
            self.license_loading = false;
            self.departments_loading = false;
            self.routing_failures = 0;
            self.license_failures = 0;
            self.routing_retry = None;
            self.license_retry = None;
        }
        let rev = ws.revision(Dirty::ME | Dirty::SETTINGS | Dirty::PERMISSIONS | Dirty::ROLES);
        if self.rev != Some(rev) {
            self.rev = Some(rev);
            self.dirty = true;
            u = u.merge(self.state.recompute_update(ws));
            if new_session && self.state.accessible {
                u.refetch_routing = true;
                u.refetch_license = true;
                u.reinit_queue = self.state.wants_queue();
                u.refetch_canned = true;
            }
        }
        let mut tasks = vec![self.handle(u, ws, h)];
        tasks.push(self.sync_watches(ws, h));
        self.publish(sid);
        Task::batch(tasks)
    }

    /// Runs the follow-up loads an update asks for.
    pub fn handle(
        &mut self,
        u: OmnichannelUpdate,
        ws: &Workspace,
        h: &SessionHandle,
    ) -> Task<Message> {
        let mut tasks = Vec::new();
        if u.refetch_routing && !self.routing_loading && self.state.accessible {
            self.routing_loading = true;
            tasks.push(Self::rest_task(
                h,
                |r| Box::pin(async move { r.livechat_routing_config().await }),
                Message::Routing,
            ));
        }
        if u.refetch_license && !self.license_loading && self.state.accessible {
            self.license_loading = true;
            tasks.push(Self::rest_task(
                h,
                |r| Box::pin(async move { r.licenses_info().await }),
                Message::License,
            ));
        }
        let wants_queue = u.reinit_queue && self.state.wants_queue();
        let wants_canned = u.refetch_canned && self.state.canned_enabled(ws);
        if (wants_queue || wants_canned) && self.state.wants_departments(ws) {
            self.queue_after_departments |= wants_queue;
            self.canned_after_departments |= wants_canned;
            if !self.departments_loading
                && let Some(uid) = ws.user_id().map(str::to_owned)
            {
                self.departments_loading = true;
                tasks.push(Self::rest_task(
                    h,
                    move |r| {
                        Box::pin(async move { r.livechat_agent_departments(&uid, false).await })
                    },
                    Message::Departments,
                ));
            }
        } else {
            if wants_queue {
                tasks.push(self.load_queue(ws, h));
            }
            if wants_canned {
                tasks.push(Self::load_canned(h));
            }
        }
        if u.refetch_priorities && self.state.has_module(LicenseInfo::LIVECHAT_ENTERPRISE) {
            tasks.push(Self::rest_task(
                h,
                |r| Box::pin(async move { r.livechat_priorities().await }),
                Message::Priorities,
            ));
        }
        if self.state.enabled && !self.fields_loaded {
            self.fields_loaded = true;
            tasks.push(Self::rest_task(
                h,
                |r| {
                    Box::pin(
                        async move { r.livechat_custom_fields(LivechatPage::new(0, 100)).await },
                    )
                },
                Message::CustomFields,
            ));
        }
        Task::batch(tasks)
    }

    fn load_queue(&mut self, ws: &Workspace, h: &SessionHandle) -> Task<Message> {
        let token = self.state.queue.begin_snapshot();
        let cap = ws.settings().i64_or(settings::POOL_MAX_DISPLAYED, 0);
        let count = u64::try_from(cap)
            .ok()
            .filter(|c| *c > 0)
            .unwrap_or(QUEUE_COUNT);
        Self::rest_task(
            h,
            move |r| Box::pin(async move { r.livechat_inquiries_queued(Some(count), None).await }),
            move |res| Message::Queue(token, res),
        )
    }

    fn load_canned(h: &SessionHandle) -> Task<Message> {
        Self::rest_task(
            h,
            |r| Box::pin(async move { r.canned_responses_all().await }),
            Message::Canned,
        )
    }

    /// Subscribes / drops the queue and canned-response streams as wanted.
    fn sync_watches(&mut self, ws: &Workspace, h: &SessionHandle) -> Task<Message> {
        let mut tasks = Vec::new();
        let wanted: Vec<(&'static str, String)> = match (self.state.wants_queue(), ws.user_id()) {
            (true, Some(uid)) => inquiry_queue_subscriptions(uid, self.state.department_ids()),
            _ => Vec::new(),
        };
        let names: Vec<String> = wanted.iter().map(|(s, k)| format!("{s}:{k}")).collect();
        if names != self.queue_keys {
            self.queue_keys = names.clone();
            self.queue_watch = None;
            if !wanted.is_empty() {
                let h = h.clone();
                tasks.push(Task::perform(
                    async move {
                        h.session()
                            .watch_streams(&wanted)
                            .await
                            .map(|w| Watch(Arc::new(w)))
                            .map_err(Failure::from)
                    },
                    move |r| Message::QueueWatch(names.clone(), r),
                ));
            }
        }
        let canned = self.state.canned_enabled(ws);
        if canned != self.canned_wanted {
            self.canned_wanted = canned;
            self.canned_watch = None;
            if canned {
                let h = h.clone();
                tasks.push(Task::perform(
                    async move {
                        h.session()
                            .watch_streams(&[canned_responses_subscription()])
                            .await
                            .map(|w| Watch(Arc::new(w)))
                            .map_err(Failure::from)
                    },
                    Message::CannedWatch,
                ));
            }
        }
        Task::batch(tasks)
    }

    /// Applies a result; returns the follow-up loads.
    pub fn apply(
        &mut self,
        m: Message,
        sid: &ServerId,
        ws: &Workspace,
        h: Option<&SessionHandle>,
    ) -> Task<Message> {
        let mut follow = OmnichannelUpdate::default();
        let mut tasks = Vec::new();
        self.dirty = true;
        match m {
            Message::Routing(r) => {
                self.routing_loading = false;
                match r {
                    Ok(cfg) => {
                        self.routing_failures = 0;
                        self.routing_retry = None;
                        self.state.set_routing(Some(cfg));
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "omnichannel routing config");
                        self.routing_failures = self.routing_failures.saturating_add(1);
                        tasks.push(self.retry(Load::Routing));
                    }
                }
                follow = self.state.recompute_update(ws);
            }
            Message::License(r) => {
                self.license_loading = false;
                match r {
                    Ok(l) => {
                        self.license_failures = 0;
                        self.license_retry = None;
                        self.state.set_license(Some(l));
                        follow.refetch_canned = true;
                    }
                    Err(e) => {
                        tracing::debug!(error = %e, "licenses.info");
                        self.license_failures = self.license_failures.saturating_add(1);
                        tasks.push(self.retry(Load::License));
                    }
                }
                // Canned responses depend on the license (only a new one).
                let canned = follow.refetch_canned;
                follow = self.state.recompute_update(ws);
                follow.refetch_canned |= canned;
            }
            Message::Departments(r) => {
                self.departments_loading = false;
                match r {
                    Ok(d) => self.state.set_departments(d),
                    Err(e) => tracing::warn!(error = %e, "agent departments"),
                }
                follow = self.state.recompute_update(ws);
                if let Some(h) = h {
                    if std::mem::take(&mut self.queue_after_departments) && self.state.wants_queue()
                    {
                        tasks.push(self.load_queue(ws, h));
                    }
                    if std::mem::take(&mut self.canned_after_departments)
                        && self.state.canned_enabled(ws)
                    {
                        tasks.push(Self::load_canned(h));
                    }
                }
                follow.reinit_queue = false;
                follow.refetch_canned = false;
            }
            Message::Queue(token, r) => match r {
                Ok(p) => {
                    self.state.queue.apply_snapshot(token, p.items);
                }
                Err(e) => tracing::warn!(error = %e, "queued inquiries"),
            },
            Message::QueueWatch(names, r) => {
                if names == self.queue_keys {
                    match r {
                        Ok(w) => {
                            let refused = w.0.refused();
                            if !refused.is_empty() {
                                tracing::warn!(?refused, "queue streams refused");
                            }
                            self.queue_watch = Some(w);
                        }
                        Err(e) => tracing::warn!(error = %e, "queue streams"),
                    }
                }
            }
            Message::CannedWatch(r) => match r {
                Ok(w) if self.canned_wanted => self.canned_watch = Some(w),
                Ok(_) => {}
                Err(e) => tracing::debug!(error = %e, "canned responses stream"),
            },
            Message::CustomFields(r) => match r {
                Ok(p) => self.state.custom_fields = p.items,
                Err(e) => tracing::debug!(error = %e, "livechat custom fields"),
            },
            Message::Canned(r) => match r {
                Ok(list) => self.state.canned.replace(list),
                Err(e) => tracing::debug!(error = %e, "canned responses"),
            },
            Message::Priorities(r) => match r {
                Ok(p) => self.state.priorities = p.items,
                Err(e) => tracing::debug!(error = %e, "priorities"),
            },
            Message::Retry(session, load, token) => follow = self.retry_due(session, load, token),
            Message::Opened(_) | Message::Refreshed(_) | Message::ContactCenter(_) => {}
        }
        if let Some(h) = h {
            tasks.push(self.handle(follow, ws, h));
            tasks.push(self.sync_watches(ws, h));
        }
        self.publish(sid);
        Task::batch(tasks)
    }

    /// Tries `load` of the current session again after a back-off, unless a
    /// retry of it is already pending (one timer chain per load, whatever
    /// started the failed load).
    fn retry(&mut self, load: Load) -> Task<Message> {
        let Some(session) = self.session else {
            return Task::none();
        };
        let (pending, attempt) = match load {
            Load::Routing => (&mut self.routing_retry, self.routing_failures),
            Load::License => (&mut self.license_retry, self.license_failures),
        };
        if pending.is_some() {
            return Task::none();
        }
        self.retry_seq += 1;
        let token = self.retry_seq;
        *pending = Some(token);
        let delay = retry_delay(attempt);
        // The timer is created when the task runs (inside the runtime).
        Task::perform(async move { tokio::time::sleep(delay).await }, move |()| {
            Message::Retry(session, load, token)
        })
    }

    /// The loads a retry timer asks for: only the pending timer of `load` of
    /// the current session, while the load is still missing.
    fn retry_due(&mut self, session: usize, load: Load, token: u64) -> OmnichannelUpdate {
        let mut u = OmnichannelUpdate::default();
        if self.session != Some(session) {
            return u;
        }
        let pending = match load {
            Load::Routing => &mut self.routing_retry,
            Load::License => &mut self.license_retry,
        };
        if *pending != Some(token) {
            return u;
        }
        *pending = None;
        match load {
            Load::Routing => u.refetch_routing = self.state.routing.is_none(),
            Load::License => u.refetch_license = self.state.license.is_none(),
        }
        u
    }

    /// The Resync after a reconnect: watches that failed are tried again.
    pub fn on_resync(&mut self) {
        if self.queue_watch.is_none() {
            self.queue_keys.clear();
        }
        if self.canned_watch.is_none() {
            self.canned_wanted = false;
        }
    }

    /// Whether `rid` is a livechat subscription not seen before (sound).
    pub fn first_seen(&mut self, rid: &str) -> bool {
        self.seen_live.insert(rid.to_owned())
    }

    /// The subscription of `rid` was removed: a chat that comes back to the
    /// user (forwarded back, re-routed) is new again.
    pub fn forget_seen(&mut self, rid: &str) {
        self.seen_live.remove(rid);
    }

    pub fn park(&mut self, room: Room, view: RoomView) {
        self.parked.push((room, Box::new(view)));
    }

    pub fn take_parked(&mut self) -> Vec<(Room, Box<RoomView>)> {
        std::mem::take(&mut self.parked)
    }

    pub fn open_contact_center(&mut self) {
        self.contact_center = Some(contact_center::ContactCenter::new());
    }

    /// Closed on explicit navigation (a room opened by the user).
    pub fn close_contact_center(&mut self) {
        self.contact_center = None;
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn errors_are_translated() {
        rust_i18n::set_locale("en");
        let f = Failure::from(rocketchat::Error::Api {
            status: 400,
            error: Some("error-comment-is-required".into()),
            error_type: None,
            message: None,
            details: None,
        });
        assert_eq!(error_text(&f), t!("omnichannel.err.comment_required"));
        let f = Failure::from(rocketchat::Error::Api {
            status: 400,
            error: Some("This is an enterprise feature [error-action-not-allowed]".into()),
            error_type: Some("error-action-not-allowed".into()),
            message: None,
            details: None,
        });
        assert_eq!(error_text(&f), t!("omnichannel.err.enterprise"));
        let f = Failure::from(rocketchat::Error::Api {
            status: 400,
            error: Some("something odd".into()),
            error_type: None,
            message: None,
            details: None,
        });
        assert_eq!(error_text(&f), "something odd");
    }

    #[test]
    fn new_room_sound_follows_the_preferences() {
        let ws_with = |prefs: serde_json::Value| {
            let mut ws = Workspace::new();
            ws.apply_me(
                serde_json::from_value(serde_json::json!({
                    "_id": "me", "username": "me", "settings": {"preferences": prefs}
                }))
                .unwrap(),
            );
            ws
        };
        let ws = ws_with(serde_json::json!({}));
        assert_eq!(new_room_sound(&ws, true).as_deref(), Some("door"));
        assert_eq!(new_room_sound(&ws, false), None);
        let ws = ws_with(serde_json::json!({"newRoomNotification": "none"}));
        assert_eq!(new_room_sound(&ws, true), None);
        let ws = ws_with(serde_json::json!({"notificationsSoundVolume": 0}));
        assert_eq!(new_room_sound(&ws, true), None);
        let ws = ws_with(serde_json::json!({"masterVolume": 0, "newRoomNotification": "chime"}));
        assert_eq!(new_room_sound(&ws, true), None);
        let ws = ws_with(serde_json::json!({"newRoomNotification": "chime"}));
        assert_eq!(new_room_sound(&ws, true).as_deref(), Some("chime"));
    }

    #[test]
    fn failed_loads_back_off() {
        use std::time::Duration;
        assert_eq!(retry_delay(1), Duration::from_secs(5));
        assert_eq!(retry_delay(2), Duration::from_secs(10));
        assert_eq!(retry_delay(4), Duration::from_secs(40));
        assert_eq!(retry_delay(7), Duration::from_secs(300));
        assert_eq!(retry_delay(u32::MAX), Duration::from_secs(300));
    }

    fn routing_failed(o: &mut Omni, ws: &Workspace, sid: &ServerId) {
        let _ = o.apply(
            Message::Routing(Err(Failure::from(rocketchat::Error::Status {
                status: 503,
                body: String::new(),
            }))),
            sid,
            ws,
            None,
        );
    }

    /// A failed routing load is retried by one timer chain, for the same
    /// session only, until it is loaded.
    #[test]
    fn routing_retry_refetches_until_loaded() {
        let mut o = Omni {
            session: Some(7),
            ..Omni::default()
        };
        let ws = Workspace::new();
        let sid = ServerId("s".into());
        routing_failed(&mut o, &ws, &sid);
        assert_eq!(o.routing_failures, 1);
        let token = o.routing_retry.unwrap();
        // A second failure (e.g. a Resync refetch) starts no second timer.
        routing_failed(&mut o, &ws, &sid);
        assert_eq!(o.routing_retry, Some(token));
        // Another session's or a stale timer does nothing.
        assert!(!o.retry_due(8, Load::Routing, token).refetch_routing);
        assert!(!o.retry_due(7, Load::Routing, token + 1).refetch_routing);
        assert!(!o.retry_due(7, Load::License, token).refetch_license);
        // The pending timer refetches once.
        assert!(o.retry_due(7, Load::Routing, token).refetch_routing);
        assert_eq!(o.routing_retry, None);
        assert!(!o.retry_due(7, Load::Routing, token).refetch_routing);
        // After a successful load a leftover timer does nothing.
        routing_failed(&mut o, &ws, &sid);
        let token = o.routing_retry.unwrap();
        let _ = o.apply(Message::Routing(Ok(Default::default())), &sid, &ws, None);
        assert_eq!(o.routing_failures, 0);
        assert!(!o.retry_due(7, Load::Routing, token).refetch_routing);
    }

    #[test]
    fn removed_chats_are_new_again() {
        let mut o = Omni::default();
        assert!(o.first_seen("L"));
        assert!(!o.first_seen("L"));
        o.forget_seen("L");
        assert!(o.first_seen("L"));
    }
}
