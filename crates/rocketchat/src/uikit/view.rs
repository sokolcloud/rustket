//! UIKit surfaces: modal, contextual bar and banner views (`UK:surfaces/*`).

use serde_json::{Map, Value};

use super::blocks::{Element, LayoutBlock};
use super::decode_blocks;
use super::text::{StrOrText, TextObject};

/// The JSON of a view as received plus its parsed form. Submit/close echo `raw`
/// (re-serialising the parsed form would drop fields this crate does not model).
#[derive(Debug, Clone, PartialEq)]
pub struct RawView<T> {
    pub raw: Value,
    pub view: T,
}

impl<T: ViewKind> RawView<T> {
    /// Parses a view object; `None` when `raw` is not an object.
    pub fn from_value(raw: Value) -> Option<Self> {
        let view = T::parse(raw.as_object()?);
        Some(Self { raw, view })
    }
}

/// A parsed view type.
pub trait ViewKind: Sized {
    fn parse(o: &Map<String, Value>) -> Self;
    /// View id (`id`, or `viewId` for banners).
    fn id(&self) -> &str;
    fn app_id(&self) -> &str;
    fn blocks(&self) -> &[LayoutBlock];
}

fn s(o: &Map<String, Value>, k: &str) -> String {
    o.get(k)
        .and_then(Value::as_str)
        .map(str::to_owned)
        .unwrap_or_default()
}

fn button(o: &Map<String, Value>, k: &str) -> Option<Element> {
    o.get(k)
        .map(Element::from_value)
        .filter(|e| matches!(e, Element::Button { .. }))
}

fn blocks(o: &Map<String, Value>) -> Vec<LayoutBlock> {
    o.get("blocks").map(decode_blocks).unwrap_or_default()
}

/// `ModalView`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ModalView {
    pub app_id: String,
    pub id: String,
    pub title: TextObject,
    pub show_icon: bool,
    /// Footer close button (`button` element).
    pub close: Option<Element>,
    /// Footer submit button (`button` element).
    pub submit: Option<Element>,
    pub blocks: Vec<LayoutBlock>,
    pub clear_on_close: Option<bool>,
    pub notify_on_close: Option<bool>,
}

impl ViewKind for ModalView {
    fn parse(o: &Map<String, Value>) -> Self {
        Self {
            app_id: s(o, "appId"),
            id: s(o, "id"),
            title: o
                .get("title")
                .and_then(TextObject::from_value)
                .unwrap_or_default(),
            show_icon: o.get("showIcon").and_then(Value::as_bool).unwrap_or(false),
            close: button(o, "close"),
            submit: button(o, "submit"),
            blocks: blocks(o),
            clear_on_close: o.get("clearOnClose").and_then(Value::as_bool),
            notify_on_close: o.get("notifyOnClose").and_then(Value::as_bool),
        }
    }
    fn id(&self) -> &str {
        &self.id
    }
    fn app_id(&self) -> &str {
        &self.app_id
    }
    fn blocks(&self) -> &[LayoutBlock] {
        &self.blocks
    }
}

/// `ContextualBarView`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ContextualBarView {
    pub app_id: String,
    pub id: String,
    pub title: TextObject,
    pub close: Option<Element>,
    pub submit: Option<Element>,
    pub blocks: Vec<LayoutBlock>,
}

impl ViewKind for ContextualBarView {
    fn parse(o: &Map<String, Value>) -> Self {
        Self {
            app_id: s(o, "appId"),
            id: s(o, "id"),
            title: o
                .get("title")
                .and_then(TextObject::from_value)
                .unwrap_or_default(),
            close: button(o, "close"),
            submit: button(o, "submit"),
            blocks: blocks(o),
        }
    }
    fn id(&self) -> &str {
        &self.id
    }
    fn app_id(&self) -> &str {
        &self.app_id
    }
    fn blocks(&self) -> &[LayoutBlock] {
        &self.blocks
    }
}

/// `BannerView`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct BannerView {
    pub app_id: String,
    pub view_id: String,
    pub inline: bool,
    /// `neutral`, `info`, `success`, `warning`, `danger`.
    pub variant: Option<String>,
    pub icon: Option<String>,
    pub title: Option<StrOrText>,
    pub blocks: Vec<LayoutBlock>,
}

impl ViewKind for BannerView {
    fn parse(o: &Map<String, Value>) -> Self {
        Self {
            app_id: s(o, "appId"),
            view_id: s(o, "viewId"),
            inline: o.get("inline").and_then(Value::as_bool).unwrap_or(false),
            variant: o.get("variant").and_then(Value::as_str).map(str::to_owned),
            icon: o.get("icon").and_then(Value::as_str).map(str::to_owned),
            title: o.get("title").and_then(StrOrText::from_value),
            blocks: blocks(o),
        }
    }
    fn id(&self) -> &str {
        &self.view_id
    }
    fn app_id(&self) -> &str {
        &self.app_id
    }
    fn blocks(&self) -> &[LayoutBlock] {
        &self.blocks
    }
}

impl<T: ViewKind> RawView<T> {
    pub fn id(&self) -> &str {
        self.view.id()
    }
}
