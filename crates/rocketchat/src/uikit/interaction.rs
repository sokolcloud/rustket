//! UIKit interactions (`UK:interactions/*`): what the client sends to
//! `POST /api/apps/ui.interaction/{appId}` and what the server answers or
//! pushes on `stream-notify-user` `{uid}/uiInteraction` (docs/uikit.md §4).

use std::collections::HashMap;

use serde::Serialize;
use serde_json::{Map, Value};

use super::view::{BannerView, ContextualBarView, ModalView, RawView};
use crate::models::ButtonContext;

/// `viewSubmit` bodies above this size are sent in the compact form
/// `{id, appId, state}` (the server's JSON body limit is 100 KiB).
pub const MAX_SUBMIT_BODY: usize = 90_000;

/// Where a block action happened.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "type", content = "id", rename_all = "lowercase")]
pub enum Container {
    /// Message id.
    Message(String),
    /// View id (modal, contextual bar, banner).
    View(String),
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BlockActionPayload {
    pub block_id: String,
    /// Absent for `url` buttons and tabs.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ViewPayload {
    /// The view as received with `state` added (or the compact form).
    pub view: Value,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ViewClosedPayload {
    pub view_id: String,
    pub view: Value,
    pub is_cleared: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ActionButtonPayload {
    pub context: ButtonContext,
    /// Composer text for `messageBoxAction`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

/// An interaction sent by the client. Every variant carries the `triggerId`
/// registered for it.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "type")]
pub enum UserInteraction {
    #[serde(rename = "blockAction", rename_all = "camelCase")]
    BlockAction {
        action_id: String,
        payload: BlockActionPayload,
        container: Container,
        #[serde(skip_serializing_if = "Option::is_none")]
        mid: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        tmid: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        rid: Option<String>,
        trigger_id: String,
    },
    #[serde(rename = "viewSubmit", rename_all = "camelCase")]
    ViewSubmit {
        payload: ViewPayload,
        view_id: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        rid: Option<String>,
        trigger_id: String,
    },
    #[serde(rename = "viewClosed", rename_all = "camelCase")]
    ViewClosed {
        payload: ViewClosedPayload,
        #[serde(skip_serializing_if = "Option::is_none")]
        rid: Option<String>,
        trigger_id: String,
    },
    #[serde(rename = "actionButton", rename_all = "camelCase")]
    ActionButton {
        action_id: String,
        payload: ActionButtonPayload,
        #[serde(skip_serializing_if = "Option::is_none")]
        mid: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        tmid: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        rid: Option<String>,
        trigger_id: String,
    },
}

/// `{...view, state}` (the web form), with `id` set to `id` when given.
fn view_with_state(raw: &Value, state: &Value, id: Option<&str>) -> Value {
    let mut o = raw.as_object().cloned().unwrap_or_default();
    if let Some(id) = id {
        o.insert("id".into(), Value::String(id.to_owned()));
    }
    o.insert("state".into(), state.clone());
    Value::Object(o)
}

impl UserInteraction {
    pub fn trigger_id(&self) -> &str {
        match self {
            Self::BlockAction { trigger_id, .. }
            | Self::ViewSubmit { trigger_id, .. }
            | Self::ViewClosed { trigger_id, .. }
            | Self::ActionButton { trigger_id, .. } => trigger_id,
        }
    }

    pub fn type_name(&self) -> &'static str {
        match self {
            Self::BlockAction { .. } => "blockAction",
            Self::ViewSubmit { .. } => "viewSubmit",
            Self::ViewClosed { .. } => "viewClosed",
            Self::ActionButton { .. } => "actionButton",
        }
    }

    /// `viewSubmit` with `{...raw_view, state}`; when that body would exceed
    /// [`MAX_SUBMIT_BODY`], the compact form `{id, appId, state}` (§5.3).
    pub fn view_submit(
        raw_view: &Value,
        state: Value,
        view_id: &str,
        app_id: &str,
        rid: Option<String>,
        trigger_id: String,
    ) -> Self {
        let full = Self::ViewSubmit {
            payload: ViewPayload {
                view: view_with_state(raw_view, &state, None),
            },
            view_id: view_id.to_owned(),
            rid: rid.clone(),
            trigger_id: trigger_id.clone(),
        };
        let size = serde_json::to_vec(&full).map_or(usize::MAX, |b| b.len());
        if size <= MAX_SUBMIT_BODY {
            return full;
        }
        let mut compact = Map::new();
        compact.insert("id".into(), Value::String(view_id.to_owned()));
        compact.insert("appId".into(), Value::String(app_id.to_owned()));
        compact.insert("state".into(), state);
        Self::ViewSubmit {
            payload: ViewPayload {
                view: Value::Object(compact),
            },
            view_id: view_id.to_owned(),
            rid,
            trigger_id,
        }
    }

    /// `viewClosed` with `{...raw_view, state}`.
    pub fn view_closed(
        raw_view: &Value,
        state: Value,
        view_id: &str,
        is_cleared: bool,
        rid: Option<String>,
        trigger_id: String,
    ) -> Self {
        Self::ViewClosed {
            payload: ViewClosedPayload {
                view_id: view_id.to_owned(),
                view: view_with_state(raw_view, &state, None),
                is_cleared,
            },
            rid,
            trigger_id,
        }
    }

    /// Banner close: `{...view, id: view.viewId, state}` with `isCleared: true`.
    pub fn banner_closed(
        raw_view: &Value,
        state: Value,
        view_id: &str,
        trigger_id: String,
    ) -> Self {
        Self::ViewClosed {
            payload: ViewClosedPayload {
                view_id: view_id.to_owned(),
                view: view_with_state(raw_view, &state, Some(view_id)),
                is_cleared: true,
            },
            rid: None,
            trigger_id,
        }
    }
}

/// Validation errors by action id. The typings say an array of objects,
/// apps-engine sends one object; both are accepted.
pub fn normalize_errors(v: &Value) -> HashMap<String, String> {
    let mut out = HashMap::new();
    let mut add = |o: &Map<String, Value>| {
        for (k, v) in o {
            let msg = match v {
                Value::String(s) => s.clone(),
                Value::Null => continue,
                other => other.to_string(),
            };
            out.entry(k.clone()).or_insert(msg);
        }
    };
    match v {
        Value::Object(o) => add(o),
        Value::Array(a) => a.iter().filter_map(Value::as_object).for_each(add),
        _ => {}
    }
    out
}

/// A server interaction (HTTP answer or `{uid}/uiInteraction` push).
#[derive(Debug, Clone, PartialEq)]
pub enum ServerInteraction {
    ModalOpen {
        trigger_id: String,
        app_id: String,
        view: Box<RawView<ModalView>>,
    },
    ModalUpdate {
        trigger_id: String,
        app_id: String,
        view: Box<RawView<ModalView>>,
    },
    /// `view_id` = `view.id`, else `viewId` (cloud announcements).
    ModalClose {
        trigger_id: String,
        app_id: String,
        view_id: Option<String>,
    },
    ContextualBarOpen {
        trigger_id: String,
        app_id: String,
        view: Box<RawView<ContextualBarView>>,
    },
    ContextualBarUpdate {
        trigger_id: String,
        app_id: String,
        view: Box<RawView<ContextualBarView>>,
    },
    ContextualBarClose {
        trigger_id: String,
        app_id: String,
        view_id: String,
    },
    BannerOpen {
        trigger_id: String,
        app_id: String,
        view: Box<RawView<BannerView>>,
    },
    BannerUpdate {
        trigger_id: String,
        app_id: String,
        view: Box<RawView<BannerView>>,
    },
    BannerClose {
        trigger_id: String,
        app_id: String,
        view_id: String,
    },
    Errors {
        trigger_id: String,
        app_id: String,
        view_id: String,
        errors: HashMap<String, String>,
    },
    /// Unknown `type`, or a known type without a usable view.
    Unknown(Value),
}

fn str_of(o: &Map<String, Value>, k: &str) -> String {
    o.get(k)
        .and_then(Value::as_str)
        .map(str::to_owned)
        .unwrap_or_default()
}

impl ServerInteraction {
    /// Decodes a server interaction; never fails.
    pub fn from_value(v: Value) -> Self {
        Self::decode(&v).unwrap_or(Self::Unknown(v))
    }

    fn decode(v: &Value) -> Option<Self> {
        let o = v.as_object()?;
        let kind = o.get("type")?.as_str()?;
        let trigger_id = str_of(o, "triggerId");
        let app_id = str_of(o, "appId");
        let view = || o.get("view").filter(|v| v.is_object()).cloned();
        let view_id = || {
            o.get("view")
                .and_then(|v| v.get("id"))
                .and_then(Value::as_str)
                .or_else(|| o.get("viewId").and_then(Value::as_str))
                .map(str::to_owned)
        };
        Some(match kind {
            "modal.open" => Self::ModalOpen {
                view: Box::new(RawView::from_value(view()?)?),
                trigger_id,
                app_id,
            },
            "modal.update" => Self::ModalUpdate {
                view: Box::new(RawView::from_value(view()?)?),
                trigger_id,
                app_id,
            },
            "modal.close" => Self::ModalClose {
                view_id: view_id(),
                trigger_id,
                app_id,
            },
            "contextual_bar.open" => Self::ContextualBarOpen {
                view: Box::new(RawView::from_value(view()?)?),
                trigger_id,
                app_id,
            },
            "contextual_bar.update" => Self::ContextualBarUpdate {
                view: Box::new(RawView::from_value(view()?)?),
                trigger_id,
                app_id,
            },
            "contextual_bar.close" => Self::ContextualBarClose {
                view_id: view_id()?,
                trigger_id,
                app_id,
            },
            "banner.open" => {
                // The view's fields sit at the top level.
                let mut raw = o.clone();
                raw.remove("type");
                raw.remove("triggerId");
                Self::BannerOpen {
                    view: Box::new(RawView::from_value(Value::Object(raw))?),
                    trigger_id,
                    app_id,
                }
            }
            "banner.update" => Self::BannerUpdate {
                view: Box::new(RawView::from_value(view()?)?),
                trigger_id,
                app_id,
            },
            "banner.close" => Self::BannerClose {
                view_id: o
                    .get("viewId")
                    .and_then(Value::as_str)
                    .or_else(|| {
                        o.get("view")
                            .and_then(|v| v.get("viewId"))
                            .and_then(Value::as_str)
                    })?
                    .to_owned(),
                trigger_id,
                app_id,
            },
            "errors" => Self::Errors {
                view_id: str_of(o, "viewId"),
                errors: o.get("errors").map(normalize_errors).unwrap_or_default(),
                trigger_id,
                app_id,
            },
            _ => return None,
        })
    }

    /// The `triggerId` (`""` when absent or for `Unknown` without one).
    pub fn trigger_id(&self) -> &str {
        match self {
            Self::ModalOpen { trigger_id, .. }
            | Self::ModalUpdate { trigger_id, .. }
            | Self::ModalClose { trigger_id, .. }
            | Self::ContextualBarOpen { trigger_id, .. }
            | Self::ContextualBarUpdate { trigger_id, .. }
            | Self::ContextualBarClose { trigger_id, .. }
            | Self::BannerOpen { trigger_id, .. }
            | Self::BannerUpdate { trigger_id, .. }
            | Self::BannerClose { trigger_id, .. }
            | Self::Errors { trigger_id, .. } => trigger_id,
            Self::Unknown(v) => v.get("triggerId").and_then(Value::as_str).unwrap_or(""),
        }
    }

    pub fn app_id(&self) -> &str {
        match self {
            Self::ModalOpen { app_id, .. }
            | Self::ModalUpdate { app_id, .. }
            | Self::ModalClose { app_id, .. }
            | Self::ContextualBarOpen { app_id, .. }
            | Self::ContextualBarUpdate { app_id, .. }
            | Self::ContextualBarClose { app_id, .. }
            | Self::BannerOpen { app_id, .. }
            | Self::BannerUpdate { app_id, .. }
            | Self::BannerClose { app_id, .. }
            | Self::Errors { app_id, .. } => app_id,
            Self::Unknown(v) => v.get("appId").and_then(Value::as_str).unwrap_or(""),
        }
    }

    /// The wire `type` (`"unknown"` for unknown interactions).
    pub fn type_name(&self) -> &'static str {
        match self {
            Self::ModalOpen { .. } => "modal.open",
            Self::ModalUpdate { .. } => "modal.update",
            Self::ModalClose { .. } => "modal.close",
            Self::ContextualBarOpen { .. } => "contextual_bar.open",
            Self::ContextualBarUpdate { .. } => "contextual_bar.update",
            Self::ContextualBarClose { .. } => "contextual_bar.close",
            Self::BannerOpen { .. } => "banner.open",
            Self::BannerUpdate { .. } => "banner.update",
            Self::BannerClose { .. } => "banner.close",
            Self::Errors { .. } => "errors",
            Self::Unknown(_) => "unknown",
        }
    }
}

/// Answer of `POST /api/apps/ui.interaction/{appId}` (HTTP 2xx).
#[derive(Debug, Clone, PartialEq)]
pub enum InteractionResponse {
    /// Empty body: the app handler returned nothing (keeps a submitted view open).
    Empty,
    /// `{}`, `{success: true|false}` or any body without `type`.
    Ack,
    Interaction(ServerInteraction),
}

impl InteractionResponse {
    /// Decodes a 2xx body.
    pub fn from_body(body: &[u8]) -> crate::Result<Self> {
        if body.iter().all(u8::is_ascii_whitespace) {
            return Ok(Self::Empty);
        }
        let v: Value = serde_json::from_slice(body).map_err(|e| {
            crate::Error::Protocol(format!("invalid ui.interaction response ({e})"))
        })?;
        Ok(Self::from_value(v))
    }

    pub fn from_value(v: Value) -> Self {
        if v.get("type").and_then(Value::as_str).is_some() {
            Self::Interaction(ServerInteraction::from_value(v))
        } else {
            Self::Ack
        }
    }
}
