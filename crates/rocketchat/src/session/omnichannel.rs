//! Omnichannel realtime events and on-demand stream subscriptions.
//!
//! These streams are not decoded into dedicated [`SessionEvent`] variants (the
//! enum is matched exhaustively by consumers): they arrive as
//! [`SessionEvent::Unknown`] and [`SessionEvent::omnichannel`] turns them into an
//! [`OmnichannelEvent`]. Agent availability arrives through
//! [`SessionEvent::UserData`] (see [`SessionEvent::agent_status`]).

use std::sync::Arc;

use serde::de::DeserializeOwned;
use serde_json::Value;
use tokio::sync::watch;

use super::events::{NotificationPayload, SessionEvent, streams};
use super::supervisor::{Registry, StreamSpec, StreamStatus};
use super::{Session, stream_error};
use crate::error::{Error, Result};
use crate::models::{
    AgentStatus, CannedResponse, DepartmentAgentChange, Inquiry, InquiryEventKind, Room, RoomType,
    lenient,
};

/// `{uid}/departmentAgentData` (in [`super::NOTIFY_USER_EVENTS`]).
pub const DEPARTMENT_AGENT_DATA: &str = "departmentAgentData";
/// `stream-notify-logged` event of EE priority changes (in [`super::NOTIFY_LOGGED_EVENTS`]).
pub const PRIORITY_CHANGED: &str = "omnichannel.priority-changed";
/// Key of [`streams::CANNED_RESPONSES`].
pub const CANNED_RESPONSES_KEY: &str = "canned-responses";

/// A decoded omnichannel realtime event.
#[derive(Debug, Clone, PartialEq)]
pub enum OmnichannelEvent {
    /// Queue event on `key` (`public`, `agent/{uid}`, `department/{id}`).
    /// `removed` may arrive twice for one close.
    InquiryQueue {
        key: String,
        kind: InquiryEventKind,
        inquiry: Box<Inquiry>,
    },
    /// The user's department membership changed (often delivered twice):
    /// re-initialise the queue.
    DepartmentAgentChanged(Box<DepartmentAgentChange>),
    /// EE canned response saved. With `agents` set, only those users are concerned.
    CannedResponseChanged {
        response: Box<CannedResponse>,
        agents: Option<Vec<String>>,
    },
    CannedResponseRemoved {
        id: String,
        agents: Option<Vec<String>>,
    },
    /// EE priority changed: refetch the priorities.
    PriorityChanged {
        id: String,
        client_action: bool,
        name: Option<String>,
    },
    /// `stream-room-data`: the full document of a watched room.
    RoomData(Box<Room>),
}

impl SessionEvent {
    /// The agent availability of a `UserData` event, if it changed (lenient:
    /// `"available "` with a trailing space is available).
    pub fn agent_status(&self) -> Option<AgentStatus> {
        match self {
            Self::UserData(v) => AgentStatus::from_user_data(v),
            _ => None,
        }
    }

    /// Decodes an omnichannel stream event (they arrive as `Unknown`).
    pub fn omnichannel(&self) -> Option<OmnichannelEvent> {
        match self {
            Self::Unknown {
                collection,
                event_name,
                args,
            } => decode_omnichannel(
                collection,
                event_name,
                args.as_array().map(Vec::as_slice).unwrap_or_default(),
            ),
            _ => None,
        }
    }
}

impl NotificationPayload {
    /// The room to open, if any. Queued-inquiry notifications (manual routing)
    /// carry no `rid` (and `_id: ""`): never open room `""`.
    pub fn room_id(&self) -> Option<&str> {
        Some(self.rid.trim()).filter(|r| !r.is_empty())
    }

    /// A "new chat in queue" notification: omnichannel without a room id. Focus
    /// the queue instead of opening a room.
    pub fn is_queued_inquiry(&self) -> bool {
        self.room_id().is_none() && self.room_type == Some(RoomType::Livechat)
    }
}

fn parse<T: DeserializeOwned>(v: &Value) -> Option<T> {
    match T::deserialize(v) {
        Ok(t) => Some(t),
        Err(e) => {
            tracing::debug!(error = %e, ty = std::any::type_name::<T>(), "invalid omnichannel payload");
            None
        }
    }
}

fn opt_str(v: Option<&Value>) -> Option<String> {
    v.and_then(Value::as_str).map(str::to_owned)
}

/// Decodes the stream data of an omnichannel event.
pub fn decode_omnichannel(
    collection: &str,
    event_name: &str,
    args: &[Value],
) -> Option<OmnichannelEvent> {
    let first = args.first()?;
    match collection {
        streams::INQUIRY_QUEUE => {
            let kind = InquiryEventKind::parse(first.get("type")?.as_str()?)?;
            let inquiry: Inquiry = parse(first)?;
            if inquiry.id.is_empty() {
                return None;
            }
            Some(OmnichannelEvent::InquiryQueue {
                key: event_name.to_owned(),
                kind,
                inquiry: Box::new(inquiry),
            })
        }
        streams::CANNED_RESPONSES => {
            let agents: Option<Vec<String>> = args
                .get(1)
                .and_then(|m| m.get("agentsId"))
                .map(|a| lenient::vec_from_value(a.clone()));
            match first.get("type")?.as_str()? {
                "removed" => Some(OmnichannelEvent::CannedResponseRemoved {
                    id: opt_str(first.get("_id")).filter(|s| !s.is_empty())?,
                    agents,
                }),
                "changed" | "added" | "inserted" | "updated" => {
                    let response: CannedResponse = parse(first)?;
                    (!response.id.is_empty()).then(|| OmnichannelEvent::CannedResponseChanged {
                        response: Box::new(response),
                        agents,
                    })
                }
                _ => None,
            }
        }
        streams::ROOM_DATA => {
            let room: Room = parse(first)?;
            (!room.id.is_empty()).then(|| OmnichannelEvent::RoomData(Box::new(room)))
        }
        streams::NOTIFY_USER => {
            let (_, kind) = event_name.split_once('/')?;
            if kind != DEPARTMENT_AGENT_DATA {
                return None;
            }
            let change: DepartmentAgentChange = parse(first)?;
            Some(OmnichannelEvent::DepartmentAgentChanged(Box::new(change)))
        }
        streams::NOTIFY_LOGGED if event_name == PRIORITY_CHANGED => {
            Some(OmnichannelEvent::PriorityChanged {
                id: opt_str(first.get("id"))?,
                client_action: first
                    .get("clientAction")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
                name: opt_str(first.get("name")),
            })
        }
        _ => None,
    }
}

/// Inquiry queue keys the web client subscribes: `agent/{uid}`, `public` and
/// `department/{id}` for every enabled department of the agent.
pub fn inquiry_queue_subscriptions<'a>(
    uid: &str,
    departments: impl IntoIterator<Item = &'a str>,
) -> Vec<(&'static str, String)> {
    let mut out = vec![
        (streams::INQUIRY_QUEUE, format!("agent/{uid}")),
        (streams::INQUIRY_QUEUE, "public".to_owned()),
    ];
    out.extend(
        departments
            .into_iter()
            .filter(|d| !d.is_empty() && !d.contains('/'))
            .map(|d| (streams::INQUIRY_QUEUE, format!("department/{d}"))),
    );
    out
}

/// `stream-canned-responses` / `canned-responses`.
pub fn canned_responses_subscription() -> (&'static str, String) {
    (streams::CANNED_RESPONSES, CANNED_RESPONSES_KEY.to_owned())
}

/// `stream-room-data` / `{rid}` (rooms shown without a subscription).
pub fn room_data_subscription(rid: &str) -> (&'static str, String) {
    (streams::ROOM_DATA, rid.to_owned())
}

impl Session {
    /// Subscribes extra streams on demand (`(stream, event/key)` pairs, e.g. from
    /// [`inquiry_queue_subscriptions`]). Events arrive on the session's event
    /// stream until the returned guard is dropped; decode them with
    /// [`SessionEvent::omnichannel`].
    ///
    /// A `not-allowed` refusal is final for that stream (listed by
    /// [`StreamWatch::refused`], not retried); other failures are retried in the
    /// background. Fails only if every stream failed for another reason.
    pub async fn watch_streams(&self, list: &[(&'static str, String)]) -> Result<StreamWatch> {
        if let Some((s, e)) = list.iter().find(|(_, e)| e.is_empty()) {
            return Err(Error::Protocol(format!("invalid stream key {s}:{e:?}")));
        }
        let (stop, stop_rx) = watch::channel(());
        let mut ids = Vec::with_capacity(list.len());
        let mut names = Vec::with_capacity(list.len());
        for (stream, event) in list {
            let spec = StreamSpec {
                stream,
                event: event.clone(),
                optional: true,
            };
            names.push(spec.name());
            ids.push(self.spawn_supervisor(spec, Some(stop_rx.clone())));
        }
        let watch = StreamWatch {
            ids,
            names,
            registry: Arc::clone(&self.core.registry),
            _stop: stop,
        };
        if watch.ids.is_empty() {
            return Ok(watch);
        }
        let mut conn = self.ddp.watch_state();
        let offline = async {
            let _ = conn.wait_for(|s| !s.is_connected()).await;
        };
        tokio::select! {
            r = self.wait_settled(&watch.ids) => r?,
            () = offline => {
                self.core.registry.commit(&watch.ids);
                return Ok(watch);
            }
        }
        let down = self.core.registry.down(Some(&watch.ids));
        if down.len() == watch.ids.len() {
            let status = watch
                .ids
                .first()
                .and_then(|id| self.core.registry.status(*id));
            return Err(stream_error(status));
        }
        if !down.is_empty() {
            tracing::warn!(missing = ?down, "some streams unavailable; retrying");
        }
        self.core.registry.commit(&watch.ids);
        Ok(watch)
    }
}

/// Keeps the subscriptions of [`Session::watch_streams`] alive; dropping it
/// unsubscribes.
#[derive(Debug)]
pub struct StreamWatch {
    ids: Vec<u64>,
    names: Vec<String>,
    registry: Arc<Registry>,
    _stop: watch::Sender<()>,
}

impl StreamWatch {
    /// `"<stream>:<key>"` of every watched stream.
    pub fn names(&self) -> &[String] {
        &self.names
    }

    /// Streams the server refused with `not-allowed` (not retried).
    pub fn refused(&self) -> Vec<String> {
        self.ids
            .iter()
            .zip(&self.names)
            .filter(|(id, _)| self.registry.status(**id) == Some(StreamStatus::Refused))
            .map(|(_, n)| n.clone())
            .collect()
    }

    /// Streams not currently subscribed and not refused (retried, or waiting for
    /// the connection).
    pub fn missing(&self) -> Vec<String> {
        self.registry.not_ready(&self.ids)
    }
}

impl Drop for StreamWatch {
    fn drop(&mut self) {
        self.registry.remove(&self.ids);
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use serde_json::json;

    #[test]
    fn queue_keys() {
        let subs = inquiry_queue_subscriptions("u1", ["d1", "", "x/y"]);
        let keys: Vec<&str> = subs.iter().map(|(_, k)| k.as_str()).collect();
        assert_eq!(keys, ["agent/u1", "public", "department/d1"]);
        assert!(subs.iter().all(|(s, _)| *s == streams::INQUIRY_QUEUE));
    }

    #[test]
    fn canned_and_priority() {
        let ev = decode_omnichannel(
            streams::CANNED_RESPONSES,
            "canned-responses",
            &[
                json!({"type":"changed","_id":"c1","shortcut":"hi","text":"Hello","scope":"user","userId":"u"}),
                json!({"agentsId":["u","v"]}),
            ],
        )
        .unwrap();
        let OmnichannelEvent::CannedResponseChanged { response, agents } = ev else {
            panic!()
        };
        assert_eq!(response.shortcut, "hi");
        assert_eq!(agents.unwrap(), ["u", "v"]);
        let ev = decode_omnichannel(
            streams::CANNED_RESPONSES,
            "canned-responses",
            &[json!({"type":"removed","_id":"c1"})],
        );
        assert_eq!(
            ev,
            Some(OmnichannelEvent::CannedResponseRemoved {
                id: "c1".into(),
                agents: None
            })
        );
        let ev = decode_omnichannel(
            streams::NOTIFY_LOGGED,
            PRIORITY_CHANGED,
            &[json!({"id":"p1","clientAction":true})],
        );
        assert_eq!(
            ev,
            Some(OmnichannelEvent::PriorityChanged {
                id: "p1".into(),
                client_action: true,
                name: None
            })
        );
        assert!(decode_omnichannel(streams::NOTIFY_LOGGED, "user-status", &[json!([])]).is_none());
    }
}
