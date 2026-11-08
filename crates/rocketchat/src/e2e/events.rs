//! The room-level `stream-notify-room` event `{rid}/e2e.keyRequest` (§6).
//!
//! It is subscribed by `Session::watch_room` and reaches consumers as
//! [`SessionEvent::Unknown`] (adding a `SessionEvent` variant would break
//! exhaustive matches downstream); decode it with
//! [`RoomE2eKeyRequest::from_event`]. Not to be confused with
//! [`SessionEvent::E2EKeyRequest`], the user-stream `{uid}/e2ekeyRequest` that
//! 8.9 never emits.

use serde_json::Value;

use crate::session::{SessionEvent, streams};

/// Event name suffix of the room key request.
pub const ROOM_KEY_REQUEST_EVENT: &str = "e2e.keyRequest";

/// A member asked for the room key (after joining, or via
/// `e2e.requestSubscriptionKeys`). Treat it as "run key distribution for this
/// room now".
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoomE2eKeyRequest {
    pub rid: String,
    /// The room's `e2eKeyId` at the time (absent for rooms without a key).
    pub key_id: Option<String>,
}

impl RoomE2eKeyRequest {
    /// Decodes `stream-notify-room` `{rid}/e2e.keyRequest` with args `[e2eKeyId]`.
    pub fn decode(collection: &str, event_name: &str, args: &Value) -> Option<Self> {
        if collection != streams::NOTIFY_ROOM {
            return None;
        }
        let (rid, kind) = event_name.split_once('/')?;
        if kind != ROOM_KEY_REQUEST_EVENT || rid.is_empty() {
            return None;
        }
        let key_id = args
            .as_array()
            .and_then(|a| a.first())
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(str::to_owned);
        Some(Self {
            rid: rid.to_owned(),
            key_id,
        })
    }

    pub fn from_event(ev: &SessionEvent) -> Option<Self> {
        match ev {
            SessionEvent::Unknown {
                collection,
                event_name,
                args,
            } => Self::decode(collection, event_name, args),
            _ => None,
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn decodes_room_key_request() {
        let ev = SessionEvent::Unknown {
            collection: "stream-notify-room".into(),
            event_name: "rid1/e2e.keyRequest".into(),
            args: json!(["53f73d9a-cd00-412a-8362-af2ae0c203ca"]),
        };
        assert_eq!(
            RoomE2eKeyRequest::from_event(&ev),
            Some(RoomE2eKeyRequest {
                rid: "rid1".into(),
                key_id: Some("53f73d9a-cd00-412a-8362-af2ae0c203ca".into())
            })
        );
        let no_kid =
            RoomE2eKeyRequest::decode("stream-notify-room", "rid1/e2e.keyRequest", &json!([null]));
        assert_eq!(no_kid.unwrap().key_id, None);
        assert!(
            RoomE2eKeyRequest::decode("stream-notify-room", "rid1/e2e.keyRequest", &json!([]))
                .is_some()
        );
        assert!(
            RoomE2eKeyRequest::decode("stream-notify-user", "rid1/e2e.keyRequest", &json!([]))
                .is_none()
        );
        assert!(
            RoomE2eKeyRequest::decode("stream-notify-room", "rid1/e2ekeyRequest", &json!([]))
                .is_none()
        );
        assert!(
            RoomE2eKeyRequest::decode("stream-notify-room", "/e2e.keyRequest", &json!([]))
                .is_none()
        );
        assert!(
            RoomE2eKeyRequest::from_event(&SessionEvent::E2EKeyRequest {
                rid: "r".into(),
                key_id: "k".into()
            })
            .is_none()
        );
    }

    #[test]
    fn session_decodes_it_as_unknown() {
        let fields = json!({"eventName": "rid1/e2e.keyRequest", "args": ["kid"]});
        let msg = meteor_ddp::DataMessage {
            kind: meteor_ddp::DataKind::Changed,
            collection: "stream-notify-room".into(),
            id: "id".into(),
            fields: fields.as_object().cloned(),
            cleared: Vec::new(),
            before: None,
        };
        let ev = crate::session::decode_event(&msg);
        assert_eq!(
            RoomE2eKeyRequest::from_event(&ev).map(|r| r.key_id),
            Some(Some("kid".into()))
        );
    }
}
