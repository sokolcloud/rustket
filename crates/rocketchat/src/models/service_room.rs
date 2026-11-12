//! Rustket's hidden per-user service room.
//!
//! A private group owned by the user, hidden from the sidebar with `rooms.hide`.
//! Its `customFields` hold client data that Rustket syncs between the user's
//! devices (flags). The server copies a room's `customFields` into every
//! member's subscription and notifies it, so each device reads them from its
//! subscriptions, live and offline, without extra requests. Works on CE: only
//! `create-p` (default for `user`) and the owner's `edit-room` are needed.
//!
//! `groups.setCustomFields` replaces the whole object. Writers start from the
//! latest [`ServiceFields`] they read, so keys they do not know survive.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use super::{Room, RoomType, Subscription, lenient};

/// `customFields` key whose object marks a room as a Rustket service room.
pub const SERVICE_MARKER_KEY: &str = "rustketService";
/// Format version written into the marker. Readers accept any version.
pub const SERVICE_FORMAT: u64 = 1;
/// `customFields` key of the synced flags blob.
pub const FLAGS_KEY: &str = "rustketFlags";
/// Name prefix of the service room (see [`service_room_name`]).
pub const SERVICE_ROOM_PREFIX: &str = "rustket-sync-";

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

    /// `uid`'s own service room: a private group created by `uid`, with no
    /// other member, whose marker names `uid`. Anyone with `edit-room` can
    /// write a marker naming `uid`, and an owner can make `uid` an owner too,
    /// but nobody else can create a room as `uid`; the member count keeps out
    /// rooms that someone (an admin) added others to.
    pub fn is_service_room_of(&self, uid: &str) -> bool {
        !uid.is_empty()
            && self.t == RoomType::Private
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
    /// group `uid` owns whose marker names `uid`. Not enough on its own: the
    /// owner of someone else's group can make `uid` an owner and write such a
    /// marker. [`is_own_service_room`] also checks the room.
    pub fn is_service_room_of(&self, uid: &str) -> bool {
        self.t == RoomType::Private
            && self.u.id == uid
            && self
                .roles
                .as_ref()
                .is_some_and(|r| r.iter().any(|r| r == "owner"))
            && self.service_fields().is_some_and(|f| f.is_owned_by(uid))
    }
}

/// Whether `sub` and `room` are `uid`'s own service room: both pass their
/// checks (the room's creator, members and type cannot be forged by others).
pub fn is_own_service_room(sub: &Subscription, room: &Room, uid: &str) -> bool {
    sub.rid == room.id && sub.is_service_room_of(uid) && room.is_service_room_of(uid)
}

/// `uid`'s service rooms among the subscriptions with their rooms (see
/// [`is_own_service_room`]), the primary one first.
///
/// Two devices may create one each at the same time; every device then picks
/// the same primary (oldest `ts`, then smallest rid) and can fold the others
/// into it.
pub fn service_subscriptions<'a>(
    entries: impl IntoIterator<Item = (&'a Subscription, &'a Room)>,
    uid: &str,
) -> Vec<&'a Subscription> {
    let mut found: Vec<_> = entries
        .into_iter()
        .filter(|(s, r)| is_own_service_room(s, r, uid))
        .map(|(s, _)| s)
        .collect();
    found.sort_by(|a, b| {
        let ts = |s: &Subscription| s.ts.map_or(i64::MAX, |t| t.timestamp_millis());
        ts(a).cmp(&ts(b)).then_with(|| a.rid.cmp(&b.rid))
    });
    found
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
                room(&s.rid, creator, 1, s.custom_fields.clone().unwrap())
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
    fn room_names_are_lowercase_and_deterministic() {
        assert_eq!(service_room_name("aB3xYz", 0), "rustket-sync-ab3xyz");
        assert_eq!(service_room_name("aB3xYz", 1), "rustket-sync-ab3xyz-2");
        assert_eq!(service_room_name("a.b$c", 0), "rustket-sync-abc");
    }
}
