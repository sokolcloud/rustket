//! REST-based synchronisation: initial load, delta sync after reconnects and
//! per-room message sync. The results are plain data applied with
//! [`Workspace::apply_initial`](crate::Workspace::apply_initial),
//! [`Workspace::apply_delta`](crate::Workspace::apply_delta) and
//! [`Workspace::apply_room_sync`](crate::Workspace::apply_room_sync).
//!
//! # How `since` is chosen
//!
//! `updatedSince` is compared with server-side `_updatedAt` / `_deletedAt`, so the
//! client's clock must not be used directly: a client clock running ahead of the
//! server would skip changes. Each sync therefore computes the next `since` from
//! **server timestamps** it received — the newest `_updatedAt`/`_deletedAt` among
//! subscriptions and rooms — minus [`SAFETY_MARGIN`] to cover writes that were in
//! flight while the lists were read. When a response carries no timestamps (no
//! changes) the previous `since` is kept. Only when nothing is known at all (an
//! empty account on the first load) the local start time minus
//! [`CLOCK_SKEW_MARGIN`] is used. Re-applying an already known change is harmless
//! because every apply method is idempotent.

use rocketchat::models::{
    CustomEmoji, CustomUserStatus, Message, Permission, Removed, Role, Room, ServerInfo, Setting,
    Subscription, UpdatedSince, User, UserSummary,
};
use rocketchat::{RcDate, RestClient};

/// Subtracted from the newest server timestamp seen.
pub const SAFETY_MARGIN: chrono::Duration = chrono::Duration::seconds(30);
/// Subtracted from the local clock when no server timestamp is available.
pub const CLOCK_SKEW_MARGIN: chrono::Duration = chrono::Duration::minutes(10);

/// A non-essential request that failed during a sync.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyncWarning {
    /// Endpoint or data set, e.g. `permissions.listAll`.
    pub what: &'static str,
    pub error: String,
}

#[derive(Debug, Clone, Default)]
pub struct InitialData {
    pub me: User,
    pub server_info: Option<ServerInfo>,
    pub settings: Vec<Setting>,
    pub permissions: Vec<Permission>,
    pub roles: Vec<Role>,
    pub subscriptions: Vec<Subscription>,
    pub rooms: Vec<Room>,
    pub custom_emoji: Vec<CustomEmoji>,
    pub custom_statuses: Vec<CustomUserStatus>,
    /// Presence of DM partners: every requested partner, the ones the server
    /// left out (it only lists users who are not offline) as offline.
    pub presence: Vec<UserSummary>,
    /// `since` for the next [`delta_sync`] (always set by this module).
    pub next_since: Option<RcDate>,
    /// When the requests were sent (set by this module). Realtime events after
    /// this instant win over the snapshot; `None`: after the previous snapshot.
    pub started_at: Option<std::time::Instant>,
    pub warnings: Vec<SyncWarning>,
}

#[derive(Debug, Clone, Default)]
pub struct DeltaData {
    pub me: Option<User>,
    pub subscriptions: UpdatedSince<Subscription>,
    pub rooms: UpdatedSince<Room>,
    pub permissions: Option<UpdatedSince<Permission>>,
    pub custom_emoji: Option<UpdatedSince<CustomEmoji>>,
    /// All public settings (there is no delta endpoint; ~4 pages).
    pub settings: Option<Vec<Setting>>,
    pub roles: Option<Vec<Role>>,
    pub custom_statuses: Option<Vec<CustomUserStatus>>,
    pub presence: Vec<UserSummary>,
    /// `presence` is complete (`users.presence` answered with `full: true`, which
    /// it does for a `from` older than 10 minutes): it lists every user who is not
    /// offline, so everyone missing from it is offline.
    pub presence_full: bool,
    /// `since` for the next [`delta_sync`] (always set by this module).
    pub next_since: Option<RcDate>,
    /// When the requests were sent (set by this module). Realtime events after
    /// this instant win over the snapshot; `None`: after the previous snapshot.
    pub started_at: Option<std::time::Instant>,
    pub warnings: Vec<SyncWarning>,
}

#[derive(Debug, Clone, Default)]
pub struct RoomSync {
    pub rid: String,
    pub updated: Vec<Message>,
    /// Ids of deleted messages.
    pub deleted: Vec<String>,
    /// Entries that failed to parse; when non-zero the timeline should be reloaded.
    pub dropped: usize,
}

fn max_date(a: Option<RcDate>, b: Option<RcDate>) -> Option<RcDate> {
    match (a, b) {
        (Some(x), Some(y)) => Some(if x > y { x } else { y }),
        (x, None) => x,
        (None, y) => y,
    }
}

fn newest_server_ts(
    subs: &UpdatedSince<Subscription>,
    rooms: &UpdatedSince<Room>,
) -> Option<RcDate> {
    let mut m = None;
    for s in &subs.update {
        m = max_date(m, s.updated_at);
    }
    for r in &rooms.update {
        m = max_date(m, r.updated_at);
    }
    for r in subs.remove.iter().chain(rooms.remove.iter()) {
        m = max_date(m, r.deleted_at);
    }
    m
}

fn next_since(
    previous: Option<RcDate>,
    subs: &UpdatedSince<Subscription>,
    rooms: &UpdatedSince<Room>,
    local_start: RcDate,
) -> RcDate {
    match (newest_server_ts(subs, rooms), previous) {
        (Some(n), prev) => {
            let candidate = RcDate(n.0 - SAFETY_MARGIN);
            // Never move backwards past a previous since.
            match prev {
                Some(p) if p > candidate => p,
                _ => candidate,
            }
        }
        (None, Some(p)) => p,
        (None, None) => RcDate(local_start.0 - CLOCK_SKEW_MARGIN),
    }
}

fn warn<T>(
    what: &'static str,
    r: rocketchat::Result<T>,
    warnings: &mut Vec<SyncWarning>,
) -> Option<T> {
    match r {
        Ok(v) => Some(v),
        Err(e) => {
            tracing::warn!(what, error = %e, "sync: non-essential request failed");
            warnings.push(SyncWarning {
                what,
                error: e.to_string(),
            });
            None
        }
    }
}

/// DM partners' user ids (from `uids`, or derived from the rid of 1:1 DMs).
pub fn dm_partner_ids(me: &str, subs: &[Subscription], rooms: &[Room]) -> Vec<String> {
    use std::collections::{BTreeSet, HashMap};
    let rooms: HashMap<&str, &Room> = rooms.iter().map(|r| (r.id.as_str(), r)).collect();
    let mut out = BTreeSet::new();
    for s in subs
        .iter()
        .filter(|s| s.t == rocketchat::models::RoomType::Direct)
    {
        if let Some(uids) = rooms.get(s.rid.as_str()).and_then(|r| r.uids.as_ref()) {
            out.extend(uids.iter().filter(|u| *u != me).cloned());
        } else if !me.is_empty() && s.rid.len() == me.len() * 2 && s.rid.contains(me) {
            let other = s.rid.replacen(me, "", 1);
            if !other.is_empty() && other != me {
                out.insert(other);
            }
        }
    }
    out.into_iter().collect()
}

/// `users.presence?ids=` leaves out offline users (like the web client's
/// `presence.ts`, a requested user missing from the answer is offline).
fn with_offline_fallback(requested: &[&str], mut users: Vec<UserSummary>) -> Vec<UserSummary> {
    let listed: std::collections::HashSet<String> = users.iter().map(|u| u.id.clone()).collect();
    for id in requested.iter().filter(|id| !listed.contains(**id)) {
        users.push(UserSummary {
            id: (*id).to_owned(),
            status: Some(rocketchat::models::UserStatus::Offline),
            ..Default::default()
        });
    }
    users
}

/// Everything a freshly logged-in workspace needs. `me`, `subscriptions.get` and
/// `rooms.get` are essential (their failure fails the load); everything else is
/// fetched concurrently and failures become [`SyncWarning`]s.
pub async fn initial_load(rest: &RestClient) -> rocketchat::Result<InitialData> {
    let started_at = Some(std::time::Instant::now());
    let local_start = RcDate::now();
    let (me, subs, rooms, info, settings, perms, roles, emoji, statuses) = tokio::join!(
        rest.me(),
        rest.subscriptions_get(None),
        rest.rooms_get(None),
        rest.info(),
        rest.settings_public_all(),
        rest.permissions_list_all(None),
        rest.roles_list(),
        rest.emoji_custom_list(None),
        rest.custom_user_status_list(),
    );
    let me = me?;
    let subs = subs?;
    let rooms = rooms?;
    if subs.dropped + rooms.dropped > 0 {
        tracing::warn!(
            subscriptions = subs.dropped,
            rooms = rooms.dropped,
            "initial load: skipped unparsable entries"
        );
    }
    let mut warnings = Vec::new();
    let server_info = warn("info", info, &mut warnings);
    let settings = warn("settings.public", settings, &mut warnings).unwrap_or_default();
    let permissions = warn("permissions.listAll", perms, &mut warnings)
        .map(|p| p.update)
        .unwrap_or_default();
    let roles = warn("roles.list", roles, &mut warnings).unwrap_or_default();
    let custom_emoji = warn("emoji-custom.list", emoji, &mut warnings)
        .map(|e| e.update)
        .unwrap_or_default();
    let custom_statuses =
        warn("custom-user-status.list", statuses, &mut warnings).unwrap_or_default();

    let partners = dm_partner_ids(&me.id, &subs.update, &rooms.update);
    let presence = if partners.is_empty() {
        Vec::new()
    } else {
        let ids: Vec<&str> = partners.iter().map(String::as_str).collect();
        let mut users = Vec::new();
        // Keep the query string short.
        for chunk in ids.chunks(100) {
            if let Some(p) = warn(
                "users.presence",
                rest.users_presence(None, chunk).await,
                &mut warnings,
            ) {
                users.extend(with_offline_fallback(chunk, p.users));
            }
        }
        users
    };

    let next = next_since(None, &subs, &rooms, local_start);
    Ok(InitialData {
        me,
        server_info,
        settings,
        permissions,
        roles,
        subscriptions: subs.update,
        rooms: rooms.update,
        custom_emoji,
        custom_statuses,
        presence,
        next_since: Some(next),
        started_at,
        warnings,
    })
}

/// Changes since `since` (from [`InitialData::next_since`] or the previous
/// [`DeltaData::next_since`]). Subscriptions and rooms are essential.
pub async fn delta_sync(rest: &RestClient, since: RcDate) -> rocketchat::Result<DeltaData> {
    let started_at = Some(std::time::Instant::now());
    let local_start = RcDate::now();
    let (me, subs, rooms, perms, emoji, settings, roles, statuses, presence) = tokio::join!(
        rest.me(),
        rest.subscriptions_get(Some(since)),
        rest.rooms_get(Some(since)),
        rest.permissions_list_all(Some(since)),
        rest.emoji_custom_list(Some(since)),
        rest.settings_public_all(),
        rest.roles_list(),
        rest.custom_user_status_list(),
        rest.users_presence(Some(since), &[]),
    );
    let subs = subs?;
    let rooms = rooms?;
    let mut warnings = Vec::new();
    let me = warn("me", me, &mut warnings);
    let permissions = warn("permissions.listAll", perms, &mut warnings);
    let custom_emoji = warn("emoji-custom.list", emoji, &mut warnings);
    let settings = warn("settings.public", settings, &mut warnings);
    let roles = warn("roles.list", roles, &mut warnings);
    let custom_statuses = warn("custom-user-status.list", statuses, &mut warnings);
    let (presence, presence_full) = warn("users.presence", presence, &mut warnings)
        .map(|p| (p.users, p.full))
        .unwrap_or_default();
    let next = next_since(Some(since), &subs, &rooms, local_start);
    Ok(DeltaData {
        me,
        subscriptions: subs,
        rooms,
        permissions,
        custom_emoji,
        settings,
        roles,
        custom_statuses,
        presence,
        presence_full,
        next_since: Some(next),
        started_at,
        warnings,
    })
}

/// Messages of a room changed or deleted since `last_update` (use
/// `Timeline::last_updated_at`, a server timestamp). [`SAFETY_MARGIN`] is
/// subtracted: the server compares exclusively, several messages can share a
/// millisecond, and a message is stamped before it is broadcast (URL previews
/// can delay the broadcast by seconds), so a newer one may arrive first.
pub async fn sync_room(
    rest: &RestClient,
    rid: &str,
    last_update: RcDate,
) -> rocketchat::Result<RoomSync> {
    let r = rest
        .chat_sync_messages(rid, RcDate(last_update.0 - SAFETY_MARGIN))
        .await?;
    if r.dropped > 0 {
        tracing::warn!(
            rid,
            dropped = r.dropped,
            "room sync: skipped unparsable entries"
        );
    }
    Ok(RoomSync {
        rid: rid.to_owned(),
        updated: r.updated,
        deleted: r.deleted.into_iter().map(|d: Removed| d.id).collect(),
        dropped: r.dropped,
    })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use crate::test_util::{fixture, sub, ts};
    use rocketchat::models::RoomType;

    fn sub_at(rid: &str, ms: i64) -> Subscription {
        let mut s = sub(rid, rid, RoomType::Channel);
        s.updated_at = Some(ts(ms));
        s
    }

    fn room_at(rid: &str, ms: i64) -> Room {
        let mut r = crate::test_util::room(rid, rid, RoomType::Channel);
        r.updated_at = Some(ts(ms));
        r
    }

    fn removed_at(id: &str, ms: i64) -> Removed {
        Removed {
            id: id.into(),
            rid: None,
            deleted_at: Some(ts(ms)),
        }
    }

    #[test]
    fn since_is_newest_server_timestamp_minus_margin() {
        let margin = SAFETY_MARGIN.num_milliseconds();
        let local = ts(10_000_000_000);
        let mut subs = UpdatedSince::<Subscription> {
            update: vec![sub_at("a", 5_000_000), sub_at("b", 3_000_000)],
            ..Default::default()
        };
        let mut rooms = UpdatedSince::<Room> {
            update: vec![room_at("a", 4_000_000), room_at("b", 1_000_000)],
            ..Default::default()
        };
        // Newest in subscriptions.
        assert_eq!(
            next_since(None, &subs, &rooms, local),
            ts(5_000_000 - margin)
        );
        // Newest in rooms.
        rooms.update.push(room_at("c", 6_000_000));
        assert_eq!(
            next_since(None, &subs, &rooms, local),
            ts(6_000_000 - margin)
        );
        // Newest in a room removal.
        rooms.remove = vec![removed_at("x", 2_000_000), removed_at("y", 7_000_000)];
        assert_eq!(
            next_since(None, &subs, &rooms, local),
            ts(7_000_000 - margin)
        );
        // Newest in a subscription removal.
        subs.remove = vec![removed_at("s", 8_000_000), removed_at("t", 100)];
        assert_eq!(
            next_since(None, &subs, &rooms, local),
            ts(8_000_000 - margin)
        );
        // Never goes backwards.
        assert_eq!(
            next_since(Some(ts(9_000_000)), &subs, &rooms, local),
            ts(9_000_000)
        );
        assert_eq!(
            next_since(Some(ts(100)), &subs, &rooms, local),
            ts(8_000_000 - margin)
        );
    }

    #[test]
    fn since_from_fixtures() {
        let subs: UpdatedSince<Subscription> =
            serde_json::from_value(fixture("subscriptions_get.json")).unwrap();
        let rooms: UpdatedSince<Room> = serde_json::from_value(fixture("rooms_get.json")).unwrap();
        // Newest `_updatedAt` in the captured fixtures (rooms_get.json).
        let newest = RcDate::parse_iso("2026-11-07T16:07:57.501Z").unwrap();
        // Local clock far in the future must not matter.
        let local = ts(newest.timestamp_millis() + 86_400_000);
        assert_eq!(
            next_since(None, &subs, &rooms, local),
            RcDate(newest.0 - SAFETY_MARGIN)
        );
        // Empty delta keeps the previous since.
        let empty = UpdatedSince::default();
        assert_eq!(
            next_since(Some(ts(5)), &empty, &UpdatedSince::default(), local),
            ts(5)
        );
        // Nothing known: local start minus skew margin.
        assert_eq!(
            next_since(None, &empty, &UpdatedSince::default(), local),
            RcDate(local.0 - CLOCK_SKEW_MARGIN)
        );
    }

    #[test]
    fn missing_presence_means_offline() {
        let listed = UserSummary {
            id: "a".into(),
            status: Some(rocketchat::models::UserStatus::Away),
            ..Default::default()
        };
        let out = with_offline_fallback(&["a", "b"], vec![listed]);
        assert_eq!(out.len(), 2);
        assert_eq!(out[1].id, "b");
        assert_eq!(out[1].status, Some(rocketchat::models::UserStatus::Offline));
    }

    #[test]
    fn dm_partners() {
        let me = "AAAAAAAAAAAAAAAAA";
        let other = "BBBBBBBBBBBBBBBBB";
        let mut a = sub(&format!("{other}{me}"), "bob", RoomType::Direct);
        a.t = RoomType::Direct;
        let mut g = sub("groupdm", "x, y", RoomType::Direct);
        g.t = RoomType::Direct;
        let mut room = crate::test_util::room("groupdm", "", RoomType::Direct);
        room.uids = Some(vec![me.into(), "C".into(), "D".into()]);
        let c = sub("chan", "c", RoomType::Channel);
        let ids = dm_partner_ids(me, &[a, g, c], &[room]);
        assert_eq!(ids, vec![other.to_owned(), "C".into(), "D".into()]);
    }
}
