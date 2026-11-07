//! Typed DDP wire messages.

use std::fmt;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// DDP protocol version requested in the `connect` message.
pub(crate) const DDP_VERSION: &str = "1";
/// Versions offered in the `connect` message, most preferred first.
pub(crate) const DDP_SUPPORT: &[&str] = &["1", "pre2", "pre1"];

/// A message sent by the client.
#[derive(Debug, Serialize)]
#[serde(tag = "msg", rename_all = "camelCase")]
pub(crate) enum ClientMessage<'a> {
    Connect {
        version: &'a str,
        support: &'a [&'a str],
    },
    Ping {
        id: &'a str,
    },
    Pong {
        #[serde(skip_serializing_if = "Option::is_none")]
        id: Option<&'a str>,
    },
    Method {
        method: &'a str,
        params: &'a [Value],
        id: &'a str,
    },
    Sub {
        id: &'a str,
        name: &'a str,
        params: &'a [Value],
    },
    Unsub {
        id: &'a str,
    },
}

/// A message received from the server.
#[derive(Debug, Deserialize)]
#[serde(tag = "msg", rename_all = "camelCase")]
pub(crate) enum ServerMessage {
    Connected {
        #[serde(default)]
        session: String,
    },
    Failed {
        #[serde(default)]
        version: Option<String>,
    },
    Ping {
        #[serde(default)]
        id: Option<String>,
    },
    Pong {
        #[serde(default)]
        #[allow(dead_code)]
        id: Option<String>,
    },
    Result {
        id: String,
        #[serde(default)]
        result: Option<Value>,
        #[serde(default)]
        error: Option<MeteorError>,
    },
    Updated {
        #[serde(default)]
        #[allow(dead_code)]
        methods: Vec<String>,
    },
    Ready {
        #[serde(default)]
        subs: Vec<String>,
    },
    Nosub {
        id: String,
        #[serde(default)]
        error: Option<MeteorError>,
    },
    Added(RawData),
    Changed(RawData),
    Removed(RawData),
    AddedBefore(RawData),
    MovedBefore(RawData),
    Error {
        #[serde(default)]
        reason: Option<String>,
    },
}

#[derive(Debug, Deserialize)]
pub(crate) struct RawData {
    collection: String,
    id: String,
    #[serde(default)]
    fields: Option<Map<String, Value>>,
    #[serde(default)]
    cleared: Vec<String>,
    #[serde(default)]
    before: Option<String>,
}

impl RawData {
    pub(crate) fn into_data(self, kind: DataKind) -> DataMessage {
        DataMessage {
            kind,
            collection: self.collection,
            id: self.id,
            fields: self.fields,
            cleared: self.cleared,
            before: self.before,
        }
    }
}

/// Kind of a DDP data message.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DataKind {
    /// `added`: a document was added to the client's view of a collection.
    Added,
    /// `changed`: fields of a document changed (also used by Rocket.Chat streams).
    Changed,
    /// `removed`: a document was removed.
    Removed,
    /// `addedBefore`: ordered variant of `added`.
    AddedBefore,
    /// `movedBefore`: a document moved within an ordered collection.
    MovedBefore,
}

/// A DDP data message (`added`, `changed`, `removed`, `addedBefore`, `movedBefore`).
#[derive(Debug, Clone, PartialEq)]
pub struct DataMessage {
    /// Which data message this is.
    pub kind: DataKind,
    /// Collection name (for Rocket.Chat streams: the stream name).
    pub collection: String,
    /// Document id.
    pub id: String,
    /// New or changed fields, if any.
    pub fields: Option<Map<String, Value>>,
    /// Fields removed from the document (`changed` only).
    pub cleared: Vec<String>,
    /// Ordered collections: id of the document this one is placed before.
    pub before: Option<String>,
}

impl DataMessage {
    /// `fields.eventName` as sent by Rocket.Chat-style streams.
    pub fn event_name(&self) -> Option<&str> {
        self.fields.as_ref()?.get("eventName")?.as_str()
    }

    /// `fields.args` as sent by Rocket.Chat-style streams.
    pub fn args(&self) -> Option<&[Value]> {
        self.fields
            .as_ref()?
            .get("args")?
            .as_array()
            .map(Vec::as_slice)
    }
}

/// An error returned by the server (`Meteor.Error`), from a method `result` or a `nosub`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MeteorError {
    /// Error code: a string (e.g. `"too-many-requests"`) or a number (e.g. `403`).
    #[serde(default)]
    pub error: Value,
    /// Short human-readable reason.
    #[serde(default)]
    pub reason: Option<String>,
    /// Full message, usually `"<reason> [<error>]"`.
    #[serde(default)]
    pub message: Option<String>,
    /// Usually `"Meteor.Error"`.
    #[serde(default, rename = "errorType")]
    pub error_type: Option<String>,
    /// Additional details (e.g. `{"timeToReset": 1234}` for rate limiting).
    #[serde(default)]
    pub details: Option<Value>,
}

impl MeteorError {
    /// The error code rendered as a string (`"403"`, `"too-many-requests"`).
    pub fn code(&self) -> String {
        match &self.error {
            Value::String(s) => s.clone(),
            Value::Null => String::new(),
            other => other.to_string(),
        }
    }

    /// Whether the error code equals `code` (compares strings and numbers alike).
    pub fn is(&self, code: &str) -> bool {
        self.code() == code
    }
}

impl fmt::Display for MeteorError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Some(message) = &self.message {
            f.write_str(message)
        } else if let Some(reason) = &self.reason {
            write!(f, "{reason} [{}]", self.code())
        } else {
            write!(f, "server error [{}]", self.code())
        }
    }
}

impl std::error::Error for MeteorError {}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn serializes_client_messages() {
        let connect = ClientMessage::Connect {
            version: DDP_VERSION,
            support: DDP_SUPPORT,
        };
        assert_eq!(
            serde_json::to_value(&connect).unwrap(),
            json!({"msg":"connect","version":"1","support":["1","pre2","pre1"]})
        );
        assert_eq!(
            serde_json::to_value(ClientMessage::Pong { id: None }).unwrap(),
            json!({"msg":"pong"})
        );
        let params = [json!(1)];
        assert_eq!(
            serde_json::to_value(ClientMessage::Method {
                method: "m",
                params: &params,
                id: "3"
            })
            .unwrap(),
            json!({"msg":"method","method":"m","params":[1],"id":"3"})
        );
    }

    #[test]
    fn parses_server_messages() {
        let m: ServerMessage = serde_json::from_value(json!({"msg":"result","id":"1"})).unwrap();
        assert!(matches!(
            m,
            ServerMessage::Result {
                result: None,
                error: None,
                ..
            }
        ));

        let m: ServerMessage = serde_json::from_value(json!({
            "msg":"result","id":"2",
            "error":{"isClientSafe":true,"error":403,"reason":"User not found","message":"User not found [403]","errorType":"Meteor.Error"}
        }))
        .unwrap();
        let ServerMessage::Result { error: Some(e), .. } = m else {
            panic!("expected error");
        };
        assert_eq!(e.code(), "403");
        assert!(e.is("403"));
        assert_eq!(e.to_string(), "User not found [403]");

        let m: ServerMessage = serde_json::from_value(json!({
            "msg":"changed","collection":"stream-room-messages","id":"id",
            "fields":{"eventName":"GENERAL","args":[{"msg":"hi"}]}
        }))
        .unwrap();
        let ServerMessage::Changed(raw) = m else {
            panic!("expected changed");
        };
        let data = raw.into_data(DataKind::Changed);
        assert_eq!(data.event_name(), Some("GENERAL"));
        assert_eq!(data.args().unwrap()[0]["msg"], "hi");

        assert!(serde_json::from_value::<ServerMessage>(json!({"msg":"weird"})).is_err());
    }
}
