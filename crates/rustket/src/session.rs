//! Bridge between a running `rocketchat::Session` and iced: one
//! [`Subscription`] per logged-in server, plus the event dispatcher that maps
//! every [`SessionEvent`] onto a `Workspace::apply_*` call.

use std::hash::{Hash, Hasher};
use std::sync::Arc;
use std::time::Duration;

use iced::Subscription;
use iced::futures::{SinkExt, Stream};
use rocketchat::session::{ChangeKind, ConnectionState as RcConn, ResyncReason};
use rocketchat::tls::Fingerprint;
use rocketchat::{
    Credentials, RestClient, ServerUrl, Session, SessionEvent, SessionOptions, TlsConfig,
    TlsContext,
};
use rustket_core::workspace::ConnectionState;
use rustket_core::{Change, ServerId, Workspace};

use crate::error::Failure;

/// Cheap, cloneable handle to a running session. The session closes when the
/// app drops its last handle (logout, token change, server removal).
#[derive(Clone)]
pub struct SessionHandle(pub Arc<Session>);

impl SessionHandle {
    pub fn rest(&self) -> &RestClient {
        self.0.rest()
    }

    pub fn session(&self) -> &Session {
        &self.0
    }
}

impl std::fmt::Debug for SessionHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("SessionHandle")
            .field(&self.0.server().as_str())
            .finish()
    }
}

/// What the session subscription of one server publishes.
#[derive(Debug, Clone)]
pub enum ServerEvent {
    /// The session is connected and logged in; keep the handle.
    Ready(SessionHandle),
    Event(Box<SessionEvent>),
    /// The first connection failed (rejected token, certificate, …). Transient
    /// network failures are retried inside the subscription and reported as
    /// `Event(Connection(Reconnecting))`.
    StartFailed(Failure),
    /// The event stream ended (session closed).
    Ended,
}

/// Identity and inputs of a session subscription. iced restarts the session only
/// when the hash changes: server, URL, user, token, pins or an explicit retry
/// `generation`.
#[derive(Clone)]
pub struct SessionSpec {
    pub server_id: ServerId,
    pub url: ServerUrl,
    pub credentials: Credentials,
    pub pins: Vec<(String, Fingerprint)>,
    pub generation: u64,
}

impl std::fmt::Debug for SessionSpec {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SessionSpec")
            .field("server_id", &self.server_id)
            .field("url", &self.url.as_str())
            .field("user_id", &self.credentials.user_id)
            .field("generation", &self.generation)
            .finish_non_exhaustive()
    }
}

impl Hash for SessionSpec {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.server_id.hash(state);
        self.url.as_str().hash(state);
        self.credentials.user_id.hash(state);
        self.credentials.auth_token.hash(state);
        self.pins.hash(state);
        self.generation.hash(state);
    }
}

pub fn tls_for(pins: &[(String, Fingerprint)]) -> Result<TlsContext, rocketchat::Error> {
    pins.iter()
        .fold(TlsConfig::new(), |c, (host, fp)| c.trust(host, *fp))
        .build()
}

/// Output of a session subscription: server, the [`SessionSpec::generation`]
/// it was started for (late events of an older session are dropped by the app)
/// and the event.
pub type Tagged = (ServerId, u64, ServerEvent);

/// The subscription for one logged-in server.
pub fn subscription(spec: SessionSpec) -> Subscription<Tagged> {
    Subscription::run_with(spec, run)
}

fn run(spec: &SessionSpec) -> impl Stream<Item = Tagged> + use<> {
    let spec = spec.clone();
    let generation = spec.generation;
    let inner = iced::stream::channel(256, async move |mut out| {
        let id = spec.server_id.clone();
        let tls = match tls_for(&spec.pins) {
            Ok(t) => t,
            Err(e) => {
                let _ = out
                    .send((id, ServerEvent::StartFailed(Failure::from(e))))
                    .await;
                return;
            }
        };
        let mut attempt: u32 = 0;
        let (session, mut events) = loop {
            let _ = out
                .send((
                    id.clone(),
                    ServerEvent::Event(Box::new(SessionEvent::Connection(RcConn::Connecting))),
                ))
                .await;
            match Session::start(
                spec.url.clone(),
                &tls,
                spec.credentials.clone(),
                SessionOptions::default(),
            )
            .await
            {
                Ok(pair) => break pair,
                Err(e) if is_retryable(&e) => {
                    attempt += 1;
                    let delay = backoff(attempt);
                    tracing::info!(server = %spec.url.as_str(), attempt, error = %e, "session start failed; retrying");
                    let _ = out
                        .send((
                            id.clone(),
                            ServerEvent::Event(Box::new(SessionEvent::Connection(
                                RcConn::Reconnecting {
                                    attempt,
                                    retry_in: delay,
                                    reason: e.to_string(),
                                },
                            ))),
                        ))
                        .await;
                    tokio::time::sleep(delay).await;
                }
                Err(e) => {
                    let _ = out
                        .send((id, ServerEvent::StartFailed(Failure::from(e))))
                        .await;
                    return;
                }
            }
        };
        let handle = SessionHandle(Arc::new(session));
        if out
            .send((id.clone(), ServerEvent::Ready(handle)))
            .await
            .is_err()
        {
            return;
        }
        while let Some(ev) = events.recv().await {
            if out
                .send((id.clone(), ServerEvent::Event(Box::new(ev))))
                .await
                .is_err()
            {
                return;
            }
        }
        let _ = out.send((id, ServerEvent::Ended)).await;
    });
    iced::futures::StreamExt::map(inner, move |(id, ev)| (id, generation, ev))
}

fn is_retryable(e: &rocketchat::Error) -> bool {
    use rocketchat::Error as E;
    !matches!(
        e,
        E::Unauthorized { .. } | E::Certificate { .. } | E::Tls(_) | E::Url(_)
    )
}

fn backoff(attempt: u32) -> Duration {
    let secs = 2u64.saturating_pow(attempt.min(6)).min(60);
    Duration::from_secs(secs)
}

/// What the app has to do after an event was applied.
#[derive(Debug, Clone, PartialEq)]
pub enum Followup {
    None,
    /// `Ready`: run `sync::initial_load`.
    InitialLoad,
    /// `Resync`: run `delta_sync` (+ `sync_room` for open rooms).
    /// `reconnected`: open rooms also re-check their room watch and history.
    Resync {
        reconnected: bool,
    },
    /// A room's history was replaced (`MessagesImported`): its timelines were
    /// reset; open room views reload the first page.
    RoomReset(String),
    /// The token is no longer valid; show the login screen with this reason.
    LoggedOut(String),
    /// A user or room avatar changed; drop cached images.
    AvatarChanged {
        username: Option<String>,
        rid: Option<String>,
    },
    /// Hand the notification to `desktop::Desktop::on_notification`.
    Notification(Box<rocketchat::session::DesktopNotification>),
}

fn change(kind: ChangeKind) -> Change {
    match kind {
        ChangeKind::Inserted => Change::Inserted,
        ChangeKind::Updated => Change::Updated,
        ChangeKind::Removed => Change::Removed,
    }
}

pub fn map_connection(s: &RcConn) -> ConnectionState {
    match s {
        RcConn::Connecting => ConnectionState::Connecting,
        RcConn::Connected => ConnectionState::Connected,
        RcConn::Reconnecting { attempt, .. } => ConnectionState::Reconnecting { attempt: *attempt },
        RcConn::Failed { .. } | RcConn::Closed => ConnectionState::Disconnected,
    }
}

/// Applies one realtime event to the workspace and tells the caller which
/// asynchronous follow-up (sync, logout handling) is needed.
pub fn apply_event(ws: &mut Workspace, ev: &SessionEvent) -> Followup {
    use SessionEvent as E;
    match ev {
        E::Connection(s) => {
            // A closed session after logout keeps the LoggedOut state.
            if *ws.connection() != ConnectionState::LoggedOut {
                ws.apply_connection(map_connection(s));
            }
            Followup::None
        }
        E::Ready { me } => {
            ws.apply_me((**me).clone());
            ws.apply_connection(ConnectionState::Connected);
            Followup::InitialLoad
        }
        E::Resync { reason } => {
            tracing::debug!(?reason, "resync requested");
            let reconnected = matches!(reason, ResyncReason::Reconnected);
            if reconnected {
                ws.apply_connection(ConnectionState::Connected);
            }
            Followup::Resync { reconnected }
        }
        E::LoggedOut { reason } => {
            ws.apply_logged_out();
            Followup::LoggedOut(reason.clone())
        }
        E::ForceLogout { .. } => {
            ws.apply_logged_out();
            Followup::LoggedOut(String::from("force_logout"))
        }
        E::Message {
            message,
            room_participant,
        } => {
            // `__my_messages__` also carries messages of public channels the
            // user never joined; keep them only for rooms that are open.
            if *room_participant
                || ws.subscription(&message.rid).is_some()
                || ws.timeline(&message.rid).is_some()
            {
                ws.apply_message((**message).clone());
            }
            Followup::None
        }
        // Streams that are down are re-subscribed by the session, which sends
        // a Resync once they are back; nothing to change in the state here.
        E::Degraded { .. } => Followup::None,
        // An import replaced history in bulk: reload the room from scratch.
        E::MessagesImported { rid } => {
            ws.reset_room_timelines(rid);
            Followup::RoomReset(rid.clone())
        }
        E::EphemeralMessage(m) => {
            ws.apply_ephemeral_message((**m).clone());
            Followup::None
        }
        E::MessageDeleted { rid, id } => {
            ws.apply_message_deleted(rid, id);
            Followup::None
        }
        E::MessagesDeletedBulk(b) => {
            let ids: Vec<String> = match &b.ids {
                Some(ids) => ids.clone(),
                None => ws
                    .timeline(&b.rid)
                    .map(|t| {
                        t.messages()
                            .filter(|m| m.ts.is_some_and(|ts| b.ts.contains(ts)))
                            .filter(|m| !(b.exclude_pinned && m.pinned == Some(true)))
                            .filter(|m| !(b.ignore_discussion && m.drid.is_some()))
                            .filter(|m| {
                                b.users.is_empty()
                                    || m.u.username.as_ref().is_some_and(|u| b.users.contains(u))
                            })
                            .map(|m| m.id.clone())
                            .collect()
                    })
                    .unwrap_or_default(),
            };
            if b.files_only == Some(true) {
                // Messages stay; their attachments changed. Let the resync fetch them.
                return Followup::Resync { reconnected: false };
            }
            for id in ids {
                ws.apply_message_deleted(&b.rid, &id);
            }
            Followup::None
        }
        E::UserActivity {
            rid,
            username,
            activities,
            tmid,
        } => {
            ws.apply_user_activity(rid, username, activities, tmid.as_deref());
            Followup::None
        }
        E::MessagesRead { rid, until, tmid } => {
            ws.apply_messages_read(rid, *until, tmid.as_deref());
            Followup::None
        }
        E::SubscriptionChanged { kind, sub } => {
            ws.apply_subscription(change(*kind), (**sub).clone());
            Followup::None
        }
        E::RoomChanged { kind, room } => {
            ws.apply_room(change(*kind), (**room).clone());
            Followup::None
        }
        E::UserStatus {
            uid,
            username,
            status,
            status_text,
        } => {
            ws.apply_user_status(uid, Some(username), *status, status_text.as_deref());
            Followup::None
        }
        E::AvatarUpdated { username, rid, .. } => Followup::AvatarChanged {
            username: username.clone(),
            rid: rid.clone(),
        },
        E::EmojiCustomUpdated(e) => {
            ws.apply_custom_emoji(Change::Updated, (**e).clone());
            Followup::None
        }
        E::EmojiCustomDeleted(e) => {
            ws.apply_custom_emoji(Change::Removed, (**e).clone());
            Followup::None
        }
        E::CustomUserStatusUpdated(s) => {
            ws.apply_custom_status(Change::Updated, (**s).clone());
            Followup::None
        }
        E::CustomUserStatusDeleted(s) => {
            ws.apply_custom_status(Change::Removed, (**s).clone());
            Followup::None
        }
        E::RolesChanged(r) => {
            if let Some(u) = &r.user {
                let added = r.kind != "removed";
                ws.apply_user_role_change(&u.id, &r.role, added, r.scope.as_deref());
            }
            Followup::None
        }
        E::RoleChanged { kind, role } => {
            ws.apply_role(change(*kind), (**role).clone());
            Followup::None
        }
        E::PermissionChanged { kind, permission } => {
            ws.apply_permission(change(*kind), (**permission).clone());
            Followup::None
        }
        E::SettingChanged { kind, setting, .. } => {
            ws.apply_setting(change(*kind), (**setting).clone());
            Followup::None
        }
        E::Notification(n) => Followup::Notification(n.clone()),
        // Not modelled in the workspace yet (later phases): user renames and
        // deletions, own-document diffs, UIKit, video conferences, E2E, banners.
        // Re-read from `/me` by the session (the raw `userData` is never
        // delivered: other room members can forge it).
        E::MeChanged(me) => {
            ws.apply_me((**me).clone());
            Followup::None
        }
        E::UserNameChanged { .. } | E::UserDeleted { .. } | E::UserData(_) => Followup::None,
        E::RoomVideoConference { .. }
        | E::CustomSoundUpdated(_)
        | E::CustomSoundDeleted(_)
        | E::UiInteraction(_)
        | E::VideoConference(_)
        | E::E2EKeyRequest { .. }
        | E::Banner(_)
        | E::BannerChanged { .. }
        | E::Calendar(_)
        | E::Webdav(_)
        | E::Unknown { .. } => Followup::None,
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use rocketchat::models::{Message, Room, RoomType, Subscription as Sub, User, UserRef};
    use rocketchat::session::BulkDelete;
    use rocketchat::{RcDate, models::UserStatus};
    use rustket_core::Dirty;

    fn me() -> User {
        User {
            id: "me".into(),
            username: Some("alice".into()),
            ..Default::default()
        }
    }

    fn sub(rid: &str) -> Sub {
        Sub {
            id: format!("s-{rid}"),
            rid: rid.into(),
            name: rid.into(),
            t: RoomType::Channel,
            open: true,
            u: UserRef {
                id: "me".into(),
                username: Some("alice".into()),
                name: None,
            },
            ..Default::default()
        }
    }

    fn msg(rid: &str, id: &str, ms: i64, user: &str) -> Message {
        Message {
            id: id.into(),
            rid: rid.into(),
            msg: "hi".into(),
            ts: Some(RcDate::from_millis(ms).unwrap()),
            u: UserRef {
                id: user.into(),
                username: Some(user.into()),
                name: None,
            },
            ..Default::default()
        }
    }

    #[test]
    fn dispatcher_applies_events() {
        let mut ws = Workspace::new();
        assert_eq!(
            apply_event(&mut ws, &SessionEvent::Ready { me: Box::new(me()) }),
            Followup::InitialLoad
        );
        assert_eq!(ws.username(), Some("alice"));
        assert_eq!(*ws.connection(), ConnectionState::Connected);

        apply_event(
            &mut ws,
            &SessionEvent::SubscriptionChanged {
                kind: ChangeKind::Inserted,
                sub: Box::new(sub("r1")),
            },
        );
        apply_event(
            &mut ws,
            &SessionEvent::RoomChanged {
                kind: ChangeKind::Inserted,
                room: Box::new(Room {
                    id: "r1".into(),
                    t: RoomType::Channel,
                    name: Some("general".into()),
                    ..Default::default()
                }),
            },
        );
        assert!(
            ws.room("r1")
                .is_some_and(|e| e.sub.is_some() && e.room.is_some())
        );

        ws.open_room("r1");
        for (i, id) in ["a", "b", "c"].iter().enumerate() {
            apply_event(
                &mut ws,
                &SessionEvent::Message {
                    message: Box::new(msg("r1", id, 1000 + i as i64, "bob")),
                    room_participant: true,
                },
            );
        }
        assert_eq!(ws.timeline("r1").unwrap().len(), 3);
        // Ephemeral messages are flagged as such.
        apply_event(
            &mut ws,
            &SessionEvent::EphemeralMessage(Box::new(msg("r1", "e", 1003, "rocket.cat"))),
        );
        assert!(ws.is_ephemeral("r1", "e"));
        assert!(!ws.is_ephemeral("r1", "c"));
        apply_event(
            &mut ws,
            &SessionEvent::MessageDeleted {
                rid: "r1".into(),
                id: "e".into(),
            },
        );
        apply_event(
            &mut ws,
            &SessionEvent::MessageDeleted {
                rid: "r1".into(),
                id: "a".into(),
            },
        );
        assert_eq!(ws.timeline("r1").unwrap().len(), 2);

        // Bulk delete by time range (exclusive bounds).
        let bulk = BulkDelete {
            rid: "r1".into(),
            ts: rocketchat::session::TimeRange {
                gt: Some(RcDate::from_millis(1000).unwrap()),
                lt: Some(RcDate::from_millis(1002).unwrap()),
                ..Default::default()
            },
            ..Default::default()
        };
        apply_event(&mut ws, &SessionEvent::MessagesDeletedBulk(Box::new(bulk)));
        let ids: Vec<_> = ws
            .timeline("r1")
            .unwrap()
            .messages()
            .map(|m| m.id.clone())
            .collect();
        assert_eq!(ids, vec!["c".to_owned()]);

        apply_event(
            &mut ws,
            &SessionEvent::UserActivity {
                rid: "r1".into(),
                username: "bob".into(),
                activities: vec!["user-typing".into()],
                tmid: None,
            },
        );
        assert!(
            ws.typing_label("r1", None, std::time::Instant::now())
                .is_some()
        );

        apply_event(
            &mut ws,
            &SessionEvent::UserStatus {
                uid: "bob-id".into(),
                username: "bob".into(),
                status: UserStatus::Busy,
                status_text: None,
            },
        );
        let changes = ws.take_changes();
        assert!(
            changes
                .areas
                .contains(Dirty::ROOMS | Dirty::TIMELINES | Dirty::TYPING)
        );
        assert!(changes.areas.contains(Dirty::PRESENCE));

        // Subscription removal drops the room.
        apply_event(
            &mut ws,
            &SessionEvent::SubscriptionChanged {
                kind: ChangeKind::Removed,
                sub: Box::new(sub("r1")),
            },
        );
        assert!(ws.room("r1").is_none());
    }

    #[test]
    fn user_data_is_ignored_and_me_changed_applied() {
        let mut ws = Workspace::new();
        apply_event(&mut ws, &SessionEvent::Ready { me: Box::new(me()) });
        let name = ws.me().and_then(|m| m.name.clone());
        apply_event(
            &mut ws,
            &SessionEvent::UserData(serde_json::json!({
                "id": "me", "type": "updated", "diff": {"name": "SPOOF", "username": "spoof"}
            })),
        );
        assert_eq!(ws.username(), Some("alice"));
        assert_eq!(ws.me().and_then(|m| m.name.clone()), name);
        let mut fetched = me();
        fetched.name = Some("Alice Renamed".into());
        apply_event(&mut ws, &SessionEvent::MeChanged(Box::new(fetched)));
        assert_eq!(
            ws.me().and_then(|m| m.name.as_deref()),
            Some("Alice Renamed")
        );
    }

    #[test]
    fn dispatcher_followups() {
        let mut ws = Workspace::new();
        assert_eq!(
            apply_event(
                &mut ws,
                &SessionEvent::Resync {
                    reason: ResyncReason::Reconnected
                }
            ),
            Followup::Resync { reconnected: true }
        );
        assert_eq!(
            apply_event(
                &mut ws,
                &SessionEvent::Resync {
                    reason: ResyncReason::Overflow
                }
            ),
            Followup::Resync { reconnected: false }
        );
        assert_eq!(
            apply_event(
                &mut ws,
                &SessionEvent::Connection(RcConn::Reconnecting {
                    attempt: 2,
                    retry_in: Duration::from_secs(1),
                    reason: "x".into()
                })
            ),
            Followup::None
        );
        assert_eq!(
            *ws.connection(),
            ConnectionState::Reconnecting { attempt: 2 }
        );
        assert_eq!(
            apply_event(
                &mut ws,
                &SessionEvent::LoggedOut {
                    reason: "revoked".into()
                }
            ),
            Followup::LoggedOut("revoked".into())
        );
        // A later Closed does not hide the logged-out state.
        apply_event(&mut ws, &SessionEvent::Connection(RcConn::Closed));
        assert_eq!(*ws.connection(), ConnectionState::LoggedOut);
        assert_eq!(
            apply_event(
                &mut ws,
                &SessionEvent::AvatarUpdated {
                    username: Some("bob".into()),
                    rid: None,
                    etag: None
                }
            ),
            Followup::AvatarChanged {
                username: Some("bob".into()),
                rid: None
            }
        );
    }

    fn hash_of(spec: &SessionSpec) -> u64 {
        let mut h = std::collections::hash_map::DefaultHasher::new();
        spec.hash(&mut h);
        h.finish()
    }

    #[test]
    fn subscription_identity() {
        let spec = SessionSpec {
            server_id: ServerId("s1".into()),
            url: ServerUrl::parse("http://localhost:3000").unwrap(),
            credentials: Credentials::new("u1", "tok"),
            pins: vec![],
            generation: 0,
        };
        let same = spec.clone();
        assert_eq!(hash_of(&spec), hash_of(&same));
        let mut token = spec.clone();
        token.credentials.auth_token = "other".into();
        assert_ne!(hash_of(&spec), hash_of(&token));
        let mut server = spec.clone();
        server.server_id = ServerId("s2".into());
        assert_ne!(hash_of(&spec), hash_of(&server));
        let mut retry = spec.clone();
        retry.generation = 1;
        assert_ne!(hash_of(&spec), hash_of(&retry));
        let mut url = spec.clone();
        url.url = ServerUrl::parse("https://localhost:3000").unwrap();
        assert_ne!(hash_of(&spec), hash_of(&url));
        let mut user = spec.clone();
        user.credentials.user_id = "u2".into();
        assert_ne!(hash_of(&spec), hash_of(&user));
        let mut pins = spec.clone();
        pins.pins = vec![("localhost".into(), [1; 32])];
        assert_ne!(hash_of(&spec), hash_of(&pins));
        // Debug output never contains the token.
        assert!(!format!("{spec:?}").contains("tok\""));
        assert!(!format!("{spec:?}").contains("auth_token"));
    }

    #[test]
    fn retry_policy() {
        use rocketchat::Error as E;
        let unauthorized = E::Unauthorized {
            message: None,
            code: None,
        };
        let cert = E::Certificate {
            host: Some("h".into()),
            reason: "x".into(),
        };
        for (e, want) in [
            (unauthorized, false),
            (cert, false),
            (E::Tls("x".into()), false),
            (E::Url("x".into()), false),
            (E::Timeout("connect"), true),
            (E::Protocol("closed".into()), true),
            (
                E::Status {
                    status: 502,
                    body: String::new(),
                },
                true,
            ),
        ] {
            assert_eq!(is_retryable(&e), want, "{e:?}");
        }
        assert_eq!(backoff(1), Duration::from_secs(2));
        assert_eq!(backoff(3), Duration::from_secs(8));
        assert!(backoff(5) > backoff(4));
        assert_eq!(backoff(10), Duration::from_secs(60));
        assert_eq!(backoff(u32::MAX), Duration::from_secs(60));
    }

    fn ready() -> Workspace {
        let mut ws = Workspace::new();
        apply_event(&mut ws, &SessionEvent::Ready { me: Box::new(me()) });
        for rid in ["r1", "r2"] {
            apply_event(
                &mut ws,
                &SessionEvent::SubscriptionChanged {
                    kind: ChangeKind::Inserted,
                    sub: Box::new(sub(rid)),
                },
            );
        }
        ws
    }

    fn ids(ws: &Workspace, rid: &str) -> Vec<String> {
        ws.timeline(rid)
            .map(|t| t.messages().map(|m| m.id.clone()).collect())
            .unwrap_or_default()
    }

    fn put(ws: &mut Workspace, m: Message) {
        apply_event(
            ws,
            &SessionEvent::Message {
                message: Box::new(m),
                room_participant: true,
            },
        );
    }

    #[test]
    fn force_logout_and_roles() {
        let mut ws = ready();
        assert_eq!(
            apply_event(&mut ws, &SessionEvent::ForceLogout { session_id: None }),
            Followup::LoggedOut("force_logout".into())
        );
        assert_eq!(*ws.connection(), ConnectionState::LoggedOut);

        let mut ws = ready();
        let role = |kind: &str| {
            SessionEvent::RolesChanged(Box::new(rocketchat::session::RoleAssignment {
                kind: kind.into(),
                role: "moderator".into(),
                user: Some(UserRef {
                    id: "me".into(),
                    username: Some("alice".into()),
                    name: None,
                }),
                scope: Some("r1".into()),
            }))
        };
        apply_event(&mut ws, &role("added"));
        assert!(
            ws.subscription("r1")
                .and_then(|s| s.roles.clone())
                .is_some_and(|r| r.contains(&"moderator".to_owned()))
        );
        apply_event(&mut ws, &role("removed"));
        assert!(
            !ws.subscription("r1")
                .and_then(|s| s.roles.clone())
                .unwrap_or_default()
                .contains(&"moderator".to_owned())
        );
    }

    #[test]
    fn bulk_delete_variants() {
        let mut ws = ready();
        ws.open_room("r1");
        let mut pinned = msg("r1", "pinned", 10, "bob");
        pinned.pinned = Some(true);
        let mut disc = msg("r1", "disc", 11, "bob");
        disc.drid = Some("d1".into());
        for m in [
            msg("r1", "a", 1, "bob"),
            msg("r1", "b", 2, "eve"),
            pinned,
            disc,
            msg("r1", "c", 12, "bob"),
        ] {
            put(&mut ws, m);
        }
        let all = rocketchat::session::TimeRange::default();
        // ids win over the range.
        let by_ids = BulkDelete {
            rid: "r1".into(),
            ids: Some(vec!["a".into()]),
            ts: all.clone(),
            ..Default::default()
        };
        apply_event(
            &mut ws,
            &SessionEvent::MessagesDeletedBulk(Box::new(by_ids)),
        );
        assert!(!ids(&ws, "r1").contains(&"a".to_owned()));
        // users filter + excludePinned + ignoreDiscussion.
        let filtered = BulkDelete {
            rid: "r1".into(),
            ts: all.clone(),
            users: vec!["bob".into()],
            exclude_pinned: true,
            ignore_discussion: true,
            ..Default::default()
        };
        apply_event(
            &mut ws,
            &SessionEvent::MessagesDeletedBulk(Box::new(filtered)),
        );
        assert_eq!(ids(&ws, "r1"), vec!["b", "pinned", "disc"]);
        // filesOnly: nothing removed, resync requested.
        let files = BulkDelete {
            rid: "r1".into(),
            ts: all,
            files_only: Some(true),
            ..Default::default()
        };
        assert_eq!(
            apply_event(&mut ws, &SessionEvent::MessagesDeletedBulk(Box::new(files))),
            Followup::Resync { reconnected: false }
        );
        assert_eq!(ids(&ws, "r1").len(), 3);
    }

    #[test]
    fn non_participant_messages_and_imports() {
        let mut ws = ready();
        // A public channel the user never joined and has not opened: ignored.
        apply_event(
            &mut ws,
            &SessionEvent::Message {
                message: Box::new(msg("foreign", "x", 1, "bob")),
                room_participant: false,
            },
        );
        assert!(ws.timeline("foreign").is_none());
        // ... but kept while that room is open (previewed).
        ws.open_room("foreign");
        apply_event(
            &mut ws,
            &SessionEvent::Message {
                message: Box::new(msg("foreign", "y", 2, "bob")),
                room_participant: false,
            },
        );
        assert_eq!(ids(&ws, "foreign"), vec!["y"]);
        // MessagesRead clears the unread flag.
        let mut unread = msg("foreign", "z", 3, "bob");
        unread.unread = Some(true);
        put(&mut ws, unread);
        apply_event(
            &mut ws,
            &SessionEvent::MessagesRead {
                rid: "foreign".into(),
                until: None,
                tmid: None,
            },
        );
        assert!(
            ws.timeline("foreign")
                .unwrap()
                .messages()
                .all(|m| m.unread.is_none())
        );
        // MessagesImported resets the room's timeline.
        assert_eq!(
            apply_event(
                &mut ws,
                &SessionEvent::MessagesImported {
                    rid: "foreign".into()
                }
            ),
            Followup::RoomReset("foreign".into())
        );
        assert!(ids(&ws, "foreign").is_empty());
    }
}
