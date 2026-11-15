//! Rustket's hidden per-user service room.
//!
//! A private group owned by the user, hidden from the sidebar with `rooms.hide`.
//! Its `customFields` hold client data that Rustket syncs between the user's
//! devices (flags). The server copies a room's `customFields` into every
//! member's subscription and notifies it, so each device reads them from its
//! subscriptions, live and offline, without extra requests. Works on CE: only
//! `create-p` (default for `user`) and the owner's `edit-room` are needed.
//!
//! Nothing on a room proves who made it: a peer can get a private group
//! created in the user's name, alone in it, with any `customFields`
//! (`teams.create` with `owner`, also when the team itself then fails to be
//! created and no team field is left). The account therefore vouches for its
//! service rooms in its own preferences, which only the user can write
//! ([`ServiceBinding`]); a room that is not bound there is never trusted.
//!
//! `groups.setCustomFields` replaces the whole object. Writers start from the
//! latest [`ServiceFields`] they read, so keys they do not know survive.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use super::{Room, RoomType, Subscription, User, lenient};

/// `customFields` key whose object marks a room as a Rustket service room.
pub const SERVICE_MARKER_KEY: &str = "rustketService";
/// Format version written into the marker. Readers accept any version.
pub const SERVICE_FORMAT: u64 = 1;
/// `customFields` key of the synced flags blob.
pub const FLAGS_KEY: &str = "rustketFlags";
/// Name prefix of the service room (see [`service_room_name`]).
pub const SERVICE_ROOM_PREFIX: &str = "rustket-sync-";

/// Preference holding the binding entries: a list of `{action, label}` that
/// every user can write for themselves (`users.setPreferences`) and no one
/// else can read.
pub const BINDING_PREFERENCE: &str = "dontAskAgainList";
/// `action` prefix of a binding entry; the room id follows.
pub const BINDING_ACTION_PREFIX: &str = "rustket-service-room:";
/// `label` of a binding entry (the web client lists the labels in the
/// account preferences).
pub const BINDING_LABEL: &str = "Rustket sync";
/// 2026-12-01T00:00:00Z. Versions before the binding created service rooms
/// without one: while an account has no binding at all, rooms the server
/// created before this instant are still accepted, and bound by the next
/// write.
pub const UNBOUND_CUTOFF_MS: i64 = 1_796_083_200_000;

/// The service rooms the account vouches for: the room ids recorded in the
/// user's own preferences (`me.settings.preferences`, from `/me`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ServiceBinding {
    rids: Vec<String>,
}

impl ServiceBinding {
    pub fn from_preferences(preferences: Option<&Map<String, Value>>) -> Self {
        let rids = preferences
            .and_then(|p| p.get(BINDING_PREFERENCE))
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(binding_rid)
            .map(str::to_owned)
            .collect();
        Self { rids }
    }

    /// The binding in a `/me` document.
    pub fn of(me: &User) -> Self {
        Self::from_preferences(me.preferences())
    }

    pub fn is_empty(&self) -> bool {
        self.rids.is_empty()
    }

    pub fn contains(&self, rid: &str) -> bool {
        self.rids.iter().any(|r| r == rid)
    }

    /// Whether `room` may hold the account's data: it is bound, or the
    /// account has no binding yet and the server created the room before
    /// [`UNBOUND_CUTOFF_MS`] (the room's `ts` is set by the server). The
    /// caller still checks [`is_own_service_room`].
    pub fn trusts(&self, room: &Room) -> bool {
        if self.rids.is_empty() {
            room.ts
                .is_some_and(|t| t.timestamp_millis() < UNBOUND_CUTOFF_MS)
        } else {
            self.contains(&room.id)
        }
    }

    /// The [`BINDING_PREFERENCE`] value that also binds `rids`: the current
    /// list, other entries untouched, plus the missing ones. `None` when all
    /// of them are bound already.
    pub fn preference_with(
        preferences: Option<&Map<String, Value>>,
        rids: &[String],
    ) -> Option<Value> {
        let bound = Self::from_preferences(preferences);
        let mut list = preferences
            .and_then(|p| p.get(BINDING_PREFERENCE))
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let before = list.len();
        for rid in rids {
            let entry = binding_entry(rid);
            if !rid.is_empty() && !bound.contains(rid) && !list.contains(&entry) {
                list.push(entry);
            }
        }
        (list.len() > before).then_some(Value::Array(list))
    }
}

fn binding_rid(entry: &Value) -> Option<&str> {
    entry
        .get("action")?
        .as_str()?
        .strip_prefix(BINDING_ACTION_PREFIX)
        .filter(|rid| !rid.is_empty())
}

fn binding_entry(rid: &str) -> Value {
    let mut entry = Map::new();
    entry.insert(
        "action".into(),
        format!("{BINDING_ACTION_PREFIX}{rid}").into(),
    );
    entry.insert("label".into(), BINDING_LABEL.into());
    Value::Object(entry)
}

/// Value of [`SERVICE_MARKER_KEY`].
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ServiceMarker {
    #[serde(deserialize_with = "lenient::or_default")]
    pub v: u64,
    /// User id of the owner the room was created for.
    #[serde(
        skip_serializing_if = "Option::is_none",
        deserialize_with = "lenient::opt"
    )]
    pub owner: Option<String>,
}

/// `customFields` of a service room: the marker plus data keys.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ServiceFields {
    fields: Map<String, Value>,
}

impl ServiceFields {
    /// Fields of a new service room for `owner`: the marker only.
    pub fn new(owner: &str) -> Self {
        let marker = ServiceMarker {
            v: SERVICE_FORMAT,
            owner: Some(owner.to_owned()),
        };
        let mut fields = Map::new();
        fields.insert(SERVICE_MARKER_KEY.into(), marker_value(&marker));
        Self { fields }
    }

    /// Parses a room's or subscription's `customFields`. `None` unless it is an
    /// object carrying an object marker.
    pub fn parse(custom_fields: Option<&Value>) -> Option<Self> {
        let fields = custom_fields?.as_object()?;
        fields.get(SERVICE_MARKER_KEY)?.as_object()?;
        Some(Self {
            fields: fields.clone(),
        })
    }

    pub fn marker(&self) -> ServiceMarker {
        self.fields
            .get(SERVICE_MARKER_KEY)
            .cloned()
            .and_then(lenient::try_convert)
            .unwrap_or_default()
    }

    /// Whether the marker names `uid` as the owner.
    pub fn is_owned_by(&self, uid: &str) -> bool {
        !uid.is_empty() && self.marker().owner.as_deref() == Some(uid)
    }

    /// A data key. The marker is not data.
    pub fn get(&self, key: &str) -> Option<&Value> {
        (key != SERVICE_MARKER_KEY)
            .then(|| self.fields.get(key))
            .flatten()
    }

    /// Sets a data key; refuses the marker key.
    pub fn set(&mut self, key: &str, value: Value) -> bool {
        if key == SERVICE_MARKER_KEY {
            return false;
        }
        self.fields.insert(key.to_owned(), value);
        true
    }

    pub fn remove(&mut self, key: &str) -> Option<Value> {
        if key == SERVICE_MARKER_KEY {
            return None;
        }
        self.fields.remove(key)
    }

    /// The whole object for `groups.setCustomFields`. The marker never goes
    /// below [`SERVICE_FORMAT`] and keeps fields a newer client added to it.
    pub fn to_custom_fields(&self) -> Value {
        let mut fields = self.fields.clone();
        if let Some(Value::Object(m)) = fields.get_mut(SERVICE_MARKER_KEY) {
            let v = m.get("v").and_then(Value::as_u64).unwrap_or(0);
            if v < SERVICE_FORMAT {
                m.insert("v".into(), SERVICE_FORMAT.into());
            }
        }
        Value::Object(fields)
    }
}

fn marker_value(marker: &ServiceMarker) -> Value {
    serde_json::to_value(marker).unwrap_or_else(|_| Value::Object(Map::new()))
}

impl Room {
    /// The service-room fields, whoever owns the room.
    pub fn service_fields(&self) -> Option<ServiceFields> {
        ServiceFields::parse(self.custom_fields.as_ref())
    }

    /// Whether the room has the shape of `uid`'s service room: a private
    /// group created by `uid`, with no other member, whose marker names
    /// `uid`, and which is neither a team nor a discussion (a `prid` would
    /// let the members of the parent read it). The member count keeps out
    /// rooms that someone (an admin) added others to.
    ///
    /// Necessary, not sufficient: `teams.create` with `owner` creates such a
    /// room in someone else's name, and leaves no team field on it when the
    /// team insert fails after the room was made. Only a room the account
    /// bound is trusted ([`ServiceBinding::trusts`]).
    pub fn is_service_room_of(&self, uid: &str) -> bool {
        !uid.is_empty()
            && self.t == RoomType::Private
            && self.prid.is_none()
            && self.team_id.is_none()
            && self.team_main != Some(true)
            && self.u.as_ref().is_some_and(|u| u.id == uid)
            && self.users_count.is_some_and(|n| n <= 1)
            && self.service_fields().is_some_and(|f| f.is_owned_by(uid))
    }
}

impl Subscription {
    /// The service-room fields, whoever owns the room.
    pub fn service_fields(&self) -> Option<ServiceFields> {
        ServiceFields::parse(self.custom_fields.as_ref())
    }

    /// Whether this subscription may be `uid`'s own service room: a private
    /// group `uid` owns whose marker names `uid`, not a team or a discussion.
    /// Not enough on its own: the owner of someone else's group can make
    /// `uid` an owner and write such a marker. [`is_own_service_room`] also
    /// checks the room.
    pub fn is_service_room_of(&self, uid: &str) -> bool {
        !uid.is_empty()
            && self.t == RoomType::Private
            && self.prid.is_none()
            && self.team_id.is_none()
            && self.team_main != Some(true)
            && self.u.id == uid
            && self
                .roles
                .as_ref()
                .is_some_and(|r| r.iter().any(|r| r == "owner"))
            && self.service_fields().is_some_and(|f| f.is_owned_by(uid))
    }
}

/// Whether `sub` and `room` have the shape of `uid`'s own service room: both
/// pass their checks. Enough to keep the room out of lists; its data is only
/// used when the account bound it ([`trusted_service_subscriptions`]).
pub fn is_own_service_room(sub: &Subscription, room: &Room, uid: &str) -> bool {
    sub.rid == room.id && sub.is_service_room_of(uid) && room.is_service_room_of(uid)
}

/// The subscriptions with their rooms that have the shape of `uid`'s service
/// room (see [`is_own_service_room`]), the primary one first. Candidates
/// only: use [`trusted_service_subscriptions`] to read or write data.
///
/// Two devices may create one each at the same time; every device then picks
/// the same primary (oldest room `ts`, then smallest rid) and can fold the
/// others into it. The room's `ts` is set by the server; the subscription's
/// can be chosen by whoever creates the room, so it is not used.
pub fn service_subscriptions<'a>(
    entries: impl IntoIterator<Item = (&'a Subscription, &'a Room)>,
    uid: &str,
) -> Vec<&'a Subscription> {
    let mut found: Vec<_> = entries
        .into_iter()
        .filter(|(s, r)| is_own_service_room(s, r, uid))
        .map(|(s, r)| (r.ts.map_or(i64::MAX, |t| t.timestamp_millis()), s))
        .collect();
    found.sort_by(|(a_ts, a), (b_ts, b)| a_ts.cmp(b_ts).then_with(|| a.rid.cmp(&b.rid)));
    found.into_iter().map(|(_, s)| s).collect()
}

/// `uid`'s service rooms: the candidates of [`service_subscriptions`] that
/// `binding` trusts, the primary one first.
pub fn trusted_service_subscriptions<'a>(
    entries: impl IntoIterator<Item = (&'a Subscription, &'a Room)>,
    uid: &str,
    binding: &ServiceBinding,
) -> Vec<&'a Subscription> {
    service_subscriptions(entries.into_iter().filter(|(_, r)| binding.trusts(r)), uid)
}

/// Name of `uid`'s service room. Deterministic, so a second device creating it
/// at the same time gets `error-duplicate-channel-name`. `attempt` > 0 adds a
/// suffix, for when the name is taken by a room that is not the service room.
/// Lowercase ASCII only, which every `UTF8_Channel_Names_Validation` in
/// practice accepts.
pub fn service_room_name(uid: &str, attempt: u32) -> String {
    let id: String = uid
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .map(|c| c.to_ascii_lowercase())
        .collect();
    match attempt {
        0 => format!("{SERVICE_ROOM_PREFIX}{id}"),
        n => format!("{SERVICE_ROOM_PREFIX}{id}-{}", n + 1),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::RcDate;
    use crate::models::UserRef;

    fn sub(rid: &str, uid: &str, fields: Value, roles: &[&str], ts: i64) -> Subscription {
        Subscription {
            rid: rid.into(),
            t: RoomType::Private,
            u: UserRef {
                id: uid.into(),
                ..Default::default()
            },
            roles: Some(roles.iter().map(|r| (*r).to_owned()).collect()),
            custom_fields: Some(fields),
            ts: RcDate::from_millis(ts),
            ..Default::default()
        }
    }

    #[test]
    fn new_fields_carry_the_marker() {
        let f = ServiceFields::new("U1");
        assert_eq!(
            f.to_custom_fields(),
            json!({ "rustketService": { "v": 1, "owner": "U1" } })
        );
        assert!(f.is_owned_by("U1"));
        assert!(!f.is_owned_by("U2"));
        assert!(!f.is_owned_by(""));
    }

    #[test]
    fn parse_is_lenient_and_needs_an_object_marker() {
        assert!(ServiceFields::parse(None).is_none());
        assert!(ServiceFields::parse(Some(&json!("x"))).is_none());
        assert!(ServiceFields::parse(Some(&json!({ "other": 1 }))).is_none());
        assert!(ServiceFields::parse(Some(&json!({ "rustketService": true }))).is_none());
        let f = ServiceFields::parse(Some(&json!({
            "rustketService": { "v": "bogus", "owner": 7, "extra": 1 },
        })))
        .unwrap();
        assert_eq!(f.marker(), ServiceMarker::default());
        assert!(!f.is_owned_by("7"));
    }

    #[test]
    fn writes_keep_unknown_keys_and_never_lower_the_format() {
        let mut f = ServiceFields::parse(Some(&json!({
            "rustketService": { "v": 3, "owner": "U1", "future": true },
            "rustketOther": { "a": 1 },
        })))
        .unwrap();
        assert!(!f.set(SERVICE_MARKER_KEY, json!(null)));
        assert!(f.remove(SERVICE_MARKER_KEY).is_none());
        assert!(f.get(SERVICE_MARKER_KEY).is_none());
        assert!(f.set(FLAGS_KEY, json!({ "rooms": {} })));
        assert_eq!(f.get(FLAGS_KEY), Some(&json!({ "rooms": {} })));
        assert_eq!(
            f.to_custom_fields(),
            json!({
                "rustketService": { "v": 3, "owner": "U1", "future": true },
                "rustketOther": { "a": 1 },
                "rustketFlags": { "rooms": {} },
            })
        );
        let mut old = ServiceFields::parse(Some(&json!({
            "rustketService": { "owner": "U1" },
        })))
        .unwrap();
        old.remove(FLAGS_KEY);
        assert_eq!(
            old.to_custom_fields(),
            json!({ "rustketService": { "owner": "U1", "v": 1 } })
        );
    }

    fn room(rid: &str, creator: &str, users: u64, fields: Value) -> Room {
        Room {
            id: rid.into(),
            t: RoomType::Private,
            u: Some(UserRef {
                id: creator.into(),
                ..Default::default()
            }),
            users_count: Some(users),
            custom_fields: Some(fields),
            ..Default::default()
        }
    }

    #[test]
    fn own_service_rooms_need_owner_marker_type_and_role() {
        let fields = ServiceFields::new("U1").to_custom_fields();
        assert!(sub("R1", "U1", fields.clone(), &["owner"], 1).is_service_room_of("U1"));
        // Someone else's group naming U1, where U1 is a plain member.
        assert!(!sub("R1", "U1", fields.clone(), &[], 1).is_service_room_of("U1"));
        assert!(!sub("R1", "U2", fields.clone(), &["owner"], 1).is_service_room_of("U2"));
        let mut channel = sub("R1", "U1", fields.clone(), &["owner"], 1);
        channel.t = RoomType::Channel;
        assert!(!channel.is_service_room_of("U1"));
        let mut no_roles = sub("R1", "U1", fields.clone(), &[], 1);
        no_roles.roles = None;
        assert!(!no_roles.is_service_room_of("U1"));

        let own = room("R1", "U1", 1, fields.clone());
        assert!(own.is_service_room_of("U1"));
        assert!(own.service_fields().is_some());
        assert!(!own.is_service_room_of(""));
        // Created by someone else (who may have left), or shared.
        let foreign = room("R1", "U9", 1, fields.clone());
        assert!(!foreign.is_service_room_of("U1"));
        assert!(foreign.service_fields().is_some());
        assert!(!room("R1", "U1", 2, fields.clone()).is_service_room_of("U1"));
        let unknown = Room {
            users_count: None,
            ..own.clone()
        };
        assert!(!unknown.is_service_room_of("U1"));

        // A group someone else created, with U1 made an owner: the
        // subscription passes, the pair does not.
        let s = sub("R1", "U1", fields.clone(), &["owner"], 1);
        assert!(is_own_service_room(&s, &own, "U1"));
        assert!(!is_own_service_room(&s, &foreign, "U1"));
        assert!(!is_own_service_room(&s, &room("R2", "U1", 1, fields), "U1"));
    }

    #[test]
    fn teams_and_discussions_are_never_service_rooms() {
        let fields = ServiceFields::new("U1").to_custom_fields();
        let own = room("R1", "U1", 1, fields.clone());
        let s = sub("R1", "U1", fields.clone(), &["owner"], 1);
        assert!(is_own_service_room(&s, &own, "U1"));

        // `teams.create` with `owner` set to U1 by someone else: creator,
        // member count, owner role and marker all name U1.
        let team: Room = serde_json::from_value(json!({
            "_id": "R1", "t": "p", "name": "x",
            "u": { "_id": "U1", "username": "u1" },
            "usersCount": 1,
            "teamMain": true, "teamId": "T1",
            "customFields": fields.clone(),
        }))
        .unwrap();
        assert!(!team.is_service_room_of("U1"));
        assert!(!is_own_service_room(&s, &team, "U1"));
        assert!(service_subscriptions([(&s, &team)], "U1").is_empty());

        for forged in [
            Room {
                team_main: Some(true),
                ..own.clone()
            },
            Room {
                team_id: Some("T1".into()),
                ..own.clone()
            },
            Room {
                team_id: Some("T1".into()),
                team_main: Some(false),
                ..own.clone()
            },
            Room {
                prid: Some("PARENT".into()),
                ..own.clone()
            },
        ] {
            assert!(!forged.is_service_room_of("U1"), "{forged:?}");
            assert!(!is_own_service_room(&s, &forged, "U1"));
        }
        // An explicit `teamMain: false` without a team is still a plain group.
        let plain = Room {
            team_main: Some(false),
            ..own.clone()
        };
        assert!(plain.is_service_room_of("U1"));

        for forged in [
            Subscription {
                team_main: Some(true),
                ..s.clone()
            },
            Subscription {
                team_id: Some("T1".into()),
                ..s.clone()
            },
            Subscription {
                prid: Some("PARENT".into()),
                ..s.clone()
            },
        ] {
            assert!(!forged.is_service_room_of("U1"), "{forged:?}");
            assert!(!is_own_service_room(&forged, &own, "U1"));
        }
        let mut nobody = s.clone();
        nobody.u.id = String::new();
        assert!(!nobody.is_service_room_of(""));
    }

    fn prefs(list: Value) -> Map<String, Value> {
        let mut p = Map::new();
        p.insert(BINDING_PREFERENCE.into(), list);
        p
    }

    #[test]
    fn binding_reads_only_its_own_entries() {
        assert!(ServiceBinding::from_preferences(None).is_empty());
        assert!(ServiceBinding::from_preferences(Some(&Map::new())).is_empty());
        assert!(ServiceBinding::from_preferences(Some(&prefs(json!("[]")))).is_empty());
        let p = prefs(json!([
            { "action": "hideRoom", "label": "Hide room" },
            { "action": "rustket-service-room:R1", "label": "Rustket sync" },
            { "action": "rustket-service-room:", "label": "x" },
            { "action": 7 },
            "junk",
            { "action": "rustket-service-room:R2" },
        ]));
        let b = ServiceBinding::from_preferences(Some(&p));
        assert!(b.contains("R1") && b.contains("R2"));
        assert!(!b.contains("") && !b.contains("hideRoom") && !b.contains("R3"));
        assert!(!b.is_empty());
        let me: User = serde_json::from_value(json!({
            "_id": "U1", "settings": { "preferences": p },
        }))
        .unwrap();
        assert_eq!(ServiceBinding::of(&me), b);
    }

    #[test]
    fn binding_writes_keep_the_other_entries() {
        let p = prefs(json!([
            { "action": "hideRoom", "label": "Hide room" },
            { "action": "rustket-service-room:R1", "label": "Rustket sync" },
        ]));
        assert_eq!(
            ServiceBinding::preference_with(Some(&p), &["R1".into()]),
            None
        );
        assert_eq!(ServiceBinding::preference_with(Some(&p), &[]), None);
        assert_eq!(
            ServiceBinding::preference_with(Some(&p), &[String::new()]),
            None
        );
        assert_eq!(
            ServiceBinding::preference_with(Some(&p), &["R1".into(), "R2".into(), "R2".into()]),
            Some(json!([
                { "action": "hideRoom", "label": "Hide room" },
                { "action": "rustket-service-room:R1", "label": "Rustket sync" },
                { "action": "rustket-service-room:R2", "label": "Rustket sync" },
            ]))
        );
        let first = ServiceBinding::preference_with(None, &["R9".into()]).unwrap();
        assert_eq!(
            first,
            json!([{ "action": "rustket-service-room:R9", "label": "Rustket sync" }])
        );
        assert!(ServiceBinding::from_preferences(Some(&prefs(first))).contains("R9"));
    }

    /// `teams.create` with `owner` whose team insert failed: the room is in
    /// the victim's name and carries no team field, so its shape is that of
    /// a real service room. Only the binding tells them apart.
    #[test]
    fn a_room_created_in_the_users_name_is_not_trusted_unbound() {
        let fields = ServiceFields::new("U1").to_custom_fields();
        let after = UNBOUND_CUTOFF_MS + 1;
        let forged: Room = serde_json::from_value(json!({
            "_id": "Rforged", "t": "p", "name": "q7", "fname": "q7 ",
            "u": { "_id": "U1", "username": "u1" },
            "usersCount": 1,
            "ts": { "$date": after },
            "customFields": fields.clone(),
        }))
        .unwrap();
        let forged_sub = sub("Rforged", "U1", fields.clone(), &["owner"], 0);
        assert!(is_own_service_room(&forged_sub, &forged, "U1"));

        // No binding yet (the victim never stored a flag): not trusted.
        let none = ServiceBinding::default();
        assert!(!none.trusts(&forged));
        assert!(trusted_service_subscriptions([(&forged_sub, &forged)], "U1", &none).is_empty());

        // The victim's own room is bound; the forged one is not, older or not.
        let own = Room {
            ts: RcDate::from_millis(after + 1_000),
            ..room("Rown", "U1", 1, fields.clone())
        };
        let own_sub = sub("Rown", "U1", fields.clone(), &["owner"], 5);
        let bound = ServiceBinding::from_preferences(Some(&prefs(json!([binding_entry("Rown")]))));
        let rids: Vec<_> =
            trusted_service_subscriptions([(&forged_sub, &forged), (&own_sub, &own)], "U1", &bound)
                .into_iter()
                .map(|s| s.rid.as_str())
                .collect();
        assert_eq!(rids, ["Rown"]);

        // A binding does not replace the shape checks.
        let foreign = Room {
            id: "Rown".into(),
            ..room("Rown", "U9", 1, fields.clone())
        };
        assert!(trusted_service_subscriptions([(&own_sub, &foreign)], "U1", &bound).is_empty());
    }

    #[test]
    fn unbound_rooms_are_accepted_only_from_before_the_binding() {
        let fields = ServiceFields::new("U1").to_custom_fields();
        let old = Room {
            ts: RcDate::from_millis(UNBOUND_CUTOFF_MS - 1),
            ..room("Rold", "U1", 1, fields.clone())
        };
        let new = Room {
            ts: RcDate::from_millis(UNBOUND_CUTOFF_MS),
            ..room("Rnew", "U1", 1, fields.clone())
        };
        let undated = room("Rundated", "U1", 1, fields.clone());
        let none = ServiceBinding::default();
        assert!(none.trusts(&old));
        assert!(!none.trusts(&new));
        assert!(!none.trusts(&undated));
        // Once the account bound a room, age no longer counts.
        let bound = ServiceBinding::from_preferences(Some(&prefs(json!([binding_entry("Rnew")]))));
        assert!(!bound.trusts(&old));
        assert!(bound.trusts(&new));
        assert!(!bound.trusts(&undated));
    }

    #[test]
    fn primary_service_room_is_the_oldest() {
        let fields = ServiceFields::new("U1").to_custom_fields();
        let subs = [
            sub("Rb", "U1", fields.clone(), &["owner"], 20),
            sub("Rx", "U1", json!({}), &["owner"], 1),
            sub("Rc", "U1", fields.clone(), &["owner"], 10),
            sub("Ra", "U1", fields.clone(), &["owner"], 10),
            sub("Rf", "U1", fields.clone(), &["owner"], 0),
        ];
        let rooms: Vec<_> = subs
            .iter()
            .map(|s| {
                let creator = if s.rid == "Rf" { "U9" } else { "U1" };
                Room {
                    ts: s.ts,
                    ..room(&s.rid, creator, 1, s.custom_fields.clone().unwrap())
                }
            })
            .collect();
        let rids: Vec<_> = service_subscriptions(subs.iter().zip(&rooms), "U1")
            .into_iter()
            .map(|s| s.rid.as_str())
            .collect();
        assert_eq!(rids, ["Ra", "Rc", "Rb"]);
        assert!(service_subscriptions(subs.iter().zip(&rooms), "U2").is_empty());
    }

    #[test]
    fn primary_ignores_the_subscription_ts() {
        let fields = ServiceFields::new("U1").to_custom_fields();
        // A backdated subscription (`subscriptionExtra.ts`) on a newer room.
        let subs = [
            sub("Rnew", "U1", fields.clone(), &["owner"], 0),
            sub("Rold", "U1", fields.clone(), &["owner"], 500),
            sub("Rnots", "U1", fields.clone(), &["owner"], 1),
        ];
        let rooms = [
            Room {
                ts: RcDate::from_millis(2_000),
                ..room("Rnew", "U1", 1, fields.clone())
            },
            Room {
                ts: RcDate::from_millis(1_000),
                ..room("Rold", "U1", 1, fields.clone())
            },
            room("Rnots", "U1", 1, fields.clone()),
        ];
        let rids: Vec<_> = service_subscriptions(subs.iter().zip(&rooms), "U1")
            .into_iter()
            .map(|s| s.rid.as_str())
            .collect();
        assert_eq!(rids, ["Rold", "Rnew", "Rnots"]);
    }

    #[test]
    fn room_names_are_lowercase_and_deterministic() {
        assert_eq!(service_room_name("aB3xYz", 0), "rustket-sync-ab3xyz");
        assert_eq!(service_room_name("aB3xYz", 1), "rustket-sync-ab3xyz-2");
        assert_eq!(service_room_name("a.b$c", 0), "rustket-sync-abc");
    }
}
