//! UIKit layout blocks and block elements (`UK:blocks/*`).
//!
//! Decoding never fails: a block or element with an unknown `type`, or one that
//! lacks a field it cannot do without, becomes `Unknown(Value)`; missing texts
//! are `""`, missing lists are empty, invalid list entries are skipped.

use serde_json::{Map, Value};

use super::text::TextObject;

/// Most top-level blocks decoded from one list.
pub const MAX_BLOCKS: usize = 50;
/// Most elements decoded per `actions` / `context` block or info-card row.
pub const MAX_ELEMENTS: usize = 25;
/// Most options decoded per select / overflow / checkbox list.
pub const MAX_OPTIONS: usize = 500;
/// Most points rendered for a `linear_scale`.
pub const MAX_SCALE_POINTS: i64 = 101;

/// Engine name used to resolve `conditional` blocks (the web passes the same).
pub const ENGINE: &str = "rocket.chat";

type Obj = Map<String, Value>;

fn s(o: &Obj, k: &str) -> Option<String> {
    match o.get(k)? {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

fn flag(o: &Obj, k: &str) -> bool {
    o.get(k).and_then(Value::as_bool).unwrap_or(false)
}

fn text(o: &Obj, k: &str) -> Option<TextObject> {
    o.get(k).and_then(TextObject::from_value)
}

fn texts(o: &Obj, k: &str) -> Vec<TextObject> {
    match o.get(k) {
        Some(Value::Array(a)) => a.iter().filter_map(TextObject::from_value).collect(),
        Some(v) => TextObject::from_value(v).into_iter().collect(),
        None => Vec::new(),
    }
}

fn strings(v: Option<&Value>) -> Vec<String> {
    match v {
        Some(Value::Array(a)) => a
            .iter()
            .filter_map(|x| x.as_str().map(str::to_owned))
            .collect(),
        Some(Value::String(s)) => vec![s.clone()],
        _ => Vec::new(),
    }
}

fn options(o: &Obj, k: &str) -> Vec<UiOption> {
    match o.get(k) {
        Some(Value::Array(a)) => a
            .iter()
            .take(MAX_OPTIONS)
            .filter_map(UiOption::from_value)
            .collect(),
        _ => Vec::new(),
    }
}

fn int(o: &Obj, k: &str) -> Option<i64> {
    match o.get(k)? {
        Value::Number(n) => n.as_i64().or_else(|| n.as_f64().map(|f| f as i64)),
        Value::String(s) => s.trim().parse().ok(),
        _ => None,
    }
}

/// `appId` / `blockId` of a layout block.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BlockMeta {
    pub app_id: Option<String>,
    pub block_id: Option<String>,
}

impl BlockMeta {
    fn from_obj(o: &Obj) -> Self {
        Self {
            app_id: s(o, "appId").filter(|x| !x.is_empty()),
            block_id: s(o, "blockId").filter(|x| !x.is_empty()),
        }
    }
}

/// `{text, value, description?, url?}`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct UiOption {
    pub text: TextObject,
    pub value: String,
    pub description: Option<TextObject>,
    pub url: Option<String>,
}

impl UiOption {
    pub fn from_value(v: &Value) -> Option<Self> {
        let o = v.as_object()?;
        Some(Self {
            text: text(o, "text").unwrap_or_default(),
            value: s(o, "value")?,
            description: text(o, "description"),
            url: s(o, "url"),
        })
    }
}

/// `{label, options}`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct OptionGroup {
    pub label: TextObject,
    pub options: Vec<UiOption>,
}

fn option_groups(o: &Obj) -> Vec<OptionGroup> {
    match o.get("optionGroups") {
        Some(Value::Array(a)) => a
            .iter()
            .filter_map(Value::as_object)
            .map(|g| OptionGroup {
                label: text(g, "label").unwrap_or_default(),
                options: options(g, "options"),
            })
            .collect(),
        _ => Vec::new(),
    }
}

/// `ConfirmationDialog`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Confirm {
    pub title: TextObject,
    pub text: TextObject,
    pub confirm: TextObject,
    pub deny: TextObject,
    /// `primary` or `danger`.
    pub style: Option<String>,
}

impl Confirm {
    fn from_value(v: &Value) -> Option<Self> {
        let o = v.as_object()?;
        Some(Self {
            title: text(o, "title").unwrap_or_default(),
            text: text(o, "text").unwrap_or_default(),
            confirm: text(o, "confirm").unwrap_or_default(),
            deny: text(o, "deny").unwrap_or_default(),
            style: s(o, "style"),
        })
    }
}

pub const ON_CHARACTER_ENTERED: &str = "on_character_entered";
pub const ON_ITEM_SELECTED: &str = "on_item_selected";

/// Fields every interactive element carries (`UK:blocks/Actionable.ts`).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Actionable {
    pub app_id: Option<String>,
    pub block_id: Option<String>,
    pub action_id: String,
    pub confirm: Option<Confirm>,
    /// `on_character_entered` / `on_item_selected`.
    pub dispatch_action_config: Vec<String>,
}

impl Actionable {
    fn from_obj(o: &Obj) -> Self {
        Self {
            app_id: s(o, "appId").filter(|x| !x.is_empty()),
            block_id: s(o, "blockId").filter(|x| !x.is_empty()),
            action_id: s(o, "actionId").unwrap_or_default(),
            confirm: o.get("confirm").and_then(Confirm::from_value),
            dispatch_action_config: strings(o.get("dispatchActionConfig")),
        }
    }

    /// The element's own `appId`/`blockId`, else the block's (§2.2; the element's
    /// values win).
    pub fn inherit(&self, block: &BlockMeta) -> Self {
        Self {
            app_id: self.app_id.clone().or_else(|| block.app_id.clone()),
            block_id: self.block_id.clone().or_else(|| block.block_id.clone()),
            ..self.clone()
        }
    }

    pub fn dispatches_on_character(&self) -> bool {
        self.dispatch_action_config
            .iter()
            .any(|d| d == ON_CHARACTER_ENTERED)
    }

    pub fn dispatches_on_select(&self) -> bool {
        self.dispatch_action_config
            .iter()
            .any(|d| d == ON_ITEM_SELECTED)
    }
}

/// `icon` element.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Icon {
    /// `phone`, `phone-off`, `phone-issue`, `clock`, `arrow-forward`, `info`,
    /// `phone-question-mark` (others render as a generic icon).
    pub icon: String,
    /// `default`, `danger`, `secondary`, `warning`.
    pub variant: String,
    pub framed: bool,
}

impl Icon {
    fn from_value(v: &Value) -> Option<Self> {
        let o = v.as_object()?;
        Some(Self {
            icon: s(o, "icon")?,
            variant: s(o, "variant").unwrap_or_else(|| "default".to_owned()),
            framed: flag(o, "framed"),
        })
    }
}

/// A block element (`UK:blocks/BlockElementType.ts`), plus the text objects that
/// appear as context / info-card items.
#[derive(Debug, Clone, PartialEq)]
pub enum Element {
    Button {
        a: Actionable,
        text: TextObject,
        url: Option<String>,
        value: Option<String>,
        /// `primary`, `secondary`, `danger`, `warning`, `success`.
        style: Option<String>,
        secondary: bool,
    },
    Overflow {
        a: Actionable,
        options: Vec<UiOption>,
    },
    StaticSelect {
        a: Actionable,
        placeholder: Option<TextObject>,
        options: Vec<UiOption>,
        groups: Vec<OptionGroup>,
        initial: Option<String>,
    },
    MultiStaticSelect {
        a: Actionable,
        placeholder: Option<TextObject>,
        options: Vec<UiOption>,
        groups: Vec<OptionGroup>,
        max: Option<u32>,
        initial: Option<Vec<String>>,
    },
    UsersSelect {
        a: Actionable,
        placeholder: Option<TextObject>,
        multi: bool,
    },
    ChannelsSelect {
        a: Actionable,
        placeholder: Option<TextObject>,
        multi: bool,
    },
    ConversationsSelect {
        a: Actionable,
        multi: bool,
    },
    DatePicker {
        a: Actionable,
        placeholder: Option<TextObject>,
        initial: Option<String>,
    },
    TimePicker {
        a: Actionable,
        placeholder: Option<TextObject>,
        initial: Option<String>,
    },
    PlainTextInput {
        a: Actionable,
        placeholder: Option<TextObject>,
        initial: Option<String>,
        multiline: bool,
        min_len: Option<u32>,
        max_len: Option<u32>,
    },
    LinearScale {
        a: Actionable,
        min: i64,
        max: i64,
        initial: Option<Value>,
        pre: Option<TextObject>,
        post: Option<TextObject>,
    },
    Checkbox {
        a: Actionable,
        options: Vec<UiOption>,
        /// `None`: no `initialOptions` (an empty selection).
        initial: Option<Vec<String>>,
    },
    ToggleSwitch {
        a: Actionable,
        options: Vec<UiOption>,
        initial: Option<Vec<String>>,
    },
    RadioButton {
        a: Actionable,
        options: Vec<UiOption>,
        initial: Option<String>,
    },
    Tab {
        a: Actionable,
        title: TextObject,
        disabled: bool,
        selected: bool,
    },
    IconButton {
        a: Actionable,
        icon: Icon,
        label: Option<String>,
        url: Option<String>,
        value: Option<String>,
    },
    Icon(Icon),
    Image {
        image_url: String,
        alt_text: String,
    },
    Text(TextObject),
    Unknown(Value),
}

fn initial_option_value(o: &Obj) -> Option<String> {
    o.get("initialOption")
        .and_then(Value::as_object)
        .and_then(|io| s(io, "value"))
}

fn initial_options(o: &Obj) -> Option<Vec<String>> {
    match o.get("initialOptions") {
        Some(Value::Array(a)) => Some(
            a.iter()
                .filter_map(Value::as_object)
                .filter_map(|io| s(io, "value"))
                .collect(),
        ),
        _ => None,
    }
}

impl Element {
    /// Decodes one element; unknown types and unusable values become `Unknown`.
    pub fn from_value(v: &Value) -> Self {
        Self::decode(v).unwrap_or_else(|| Self::Unknown(v.clone()))
    }

    fn decode(v: &Value) -> Option<Self> {
        let o = v.as_object()?;
        let kind = o.get("type")?.as_str()?;
        let a = || Actionable::from_obj(o);
        let placeholder = || text(o, "placeholder");
        Some(match kind {
            "button" => Self::Button {
                a: a(),
                text: text(o, "text").unwrap_or_default(),
                url: s(o, "url").filter(|u| !u.is_empty()),
                value: s(o, "value"),
                style: s(o, "style"),
                secondary: flag(o, "secondary"),
            },
            "overflow" => Self::Overflow {
                a: a(),
                options: options(o, "options"),
            },
            "static_select" => Self::StaticSelect {
                a: a(),
                placeholder: placeholder(),
                options: options(o, "options"),
                groups: option_groups(o),
                initial: s(o, "initialValue").or_else(|| initial_option_value(o)),
            },
            "multi_static_select" => Self::MultiStaticSelect {
                a: a(),
                placeholder: placeholder(),
                options: options(o, "options"),
                groups: option_groups(o),
                max: int(o, "maxSelectItems").and_then(|m| u32::try_from(m).ok()),
                initial: match o.get("initialValue") {
                    Some(v @ (Value::Array(_) | Value::String(_))) => Some(strings(Some(v))),
                    _ => initial_options(o).or_else(|| initial_option_value(o).map(|v| vec![v])),
                },
            },
            "users_select" | "multi_users_select" => Self::UsersSelect {
                a: a(),
                placeholder: placeholder(),
                multi: kind.starts_with("multi_"),
            },
            "channels_select" | "multi_channels_select" => Self::ChannelsSelect {
                a: a(),
                placeholder: placeholder(),
                multi: kind.starts_with("multi_"),
            },
            "conversations_select" | "multi_conversations_select" => Self::ConversationsSelect {
                a: a(),
                multi: kind.starts_with("multi_"),
            },
            "datepicker" => Self::DatePicker {
                a: a(),
                placeholder: placeholder(),
                initial: s(o, "initialDate"),
            },
            "time_picker" => Self::TimePicker {
                a: a(),
                placeholder: placeholder(),
                initial: s(o, "initialTime"),
            },
            "plain_text_input" => Self::PlainTextInput {
                a: a(),
                placeholder: placeholder(),
                initial: s(o, "initialValue"),
                multiline: flag(o, "multiline"),
                min_len: int(o, "minLength").and_then(|m| u32::try_from(m).ok()),
                max_len: int(o, "maxLength").and_then(|m| u32::try_from(m).ok()),
            },
            "linear_scale" => Self::LinearScale {
                a: a(),
                min: int(o, "minValue").unwrap_or(0),
                max: int(o, "maxValue").unwrap_or(10),
                initial: o
                    .get("initialValue")
                    .filter(|v| v.is_number() || v.is_string())
                    .cloned(),
                pre: text(o, "preLabel"),
                post: text(o, "postLabel"),
            },
            "checkbox" => Self::Checkbox {
                a: a(),
                options: options(o, "options"),
                initial: initial_options(o),
            },
            "toggle_switch" => Self::ToggleSwitch {
                a: a(),
                options: options(o, "options"),
                initial: initial_options(o),
            },
            "radio_button" => Self::RadioButton {
                a: a(),
                options: options(o, "options"),
                initial: s(o, "initialValue").or_else(|| initial_option_value(o)),
            },
            "tab" => Self::Tab {
                a: a(),
                title: text(o, "title").unwrap_or_default(),
                disabled: flag(o, "disabled"),
                selected: flag(o, "selected"),
            },
            "icon_button" => Self::IconButton {
                a: a(),
                icon: o.get("icon").and_then(Icon::from_value)?,
                label: s(o, "label"),
                url: s(o, "url").filter(|u| !u.is_empty()),
                value: s(o, "value"),
            },
            "icon" => Self::Icon(Icon::from_value(v)?),
            "image" => Self::Image {
                image_url: s(o, "imageUrl")?,
                alt_text: s(o, "altText").unwrap_or_default(),
            },
            "plain_text" | "mrkdwn" => Self::Text(TextObject::from_value(v)?),
            _ => return None,
        })
    }

    /// The JSON `type` (`"unknown"` for undecodable values).
    pub fn type_name(&self) -> &str {
        match self {
            Self::Button { .. } => "button",
            Self::Overflow { .. } => "overflow",
            Self::StaticSelect { .. } => "static_select",
            Self::MultiStaticSelect { .. } => "multi_static_select",
            Self::UsersSelect { multi: false, .. } => "users_select",
            Self::UsersSelect { multi: true, .. } => "multi_users_select",
            Self::ChannelsSelect { multi: false, .. } => "channels_select",
            Self::ChannelsSelect { multi: true, .. } => "multi_channels_select",
            Self::ConversationsSelect { multi: false, .. } => "conversations_select",
            Self::ConversationsSelect { multi: true, .. } => "multi_conversations_select",
            Self::DatePicker { .. } => "datepicker",
            Self::TimePicker { .. } => "time_picker",
            Self::PlainTextInput { .. } => "plain_text_input",
            Self::LinearScale { .. } => "linear_scale",
            Self::Checkbox { .. } => "checkbox",
            Self::ToggleSwitch { .. } => "toggle_switch",
            Self::RadioButton { .. } => "radio_button",
            Self::Tab { .. } => "tab",
            Self::IconButton { .. } => "icon_button",
            Self::Icon(_) => "icon",
            Self::Image { .. } => "image",
            Self::Text(t) if t.is_mrkdwn() => "mrkdwn",
            Self::Text(_) => "plain_text",
            Self::Unknown(_) => "unknown",
        }
    }

    pub fn actionable(&self) -> Option<&Actionable> {
        match self {
            Self::Button { a, .. }
            | Self::Overflow { a, .. }
            | Self::StaticSelect { a, .. }
            | Self::MultiStaticSelect { a, .. }
            | Self::UsersSelect { a, .. }
            | Self::ChannelsSelect { a, .. }
            | Self::ConversationsSelect { a, .. }
            | Self::DatePicker { a, .. }
            | Self::TimePicker { a, .. }
            | Self::PlainTextInput { a, .. }
            | Self::LinearScale { a, .. }
            | Self::Checkbox { a, .. }
            | Self::ToggleSwitch { a, .. }
            | Self::RadioButton { a, .. }
            | Self::Tab { a, .. }
            | Self::IconButton { a, .. } => Some(a),
            Self::Icon(_) | Self::Image { .. } | Self::Text(_) | Self::Unknown(_) => None,
        }
    }

    fn actionable_mut(&mut self) -> Option<&mut Actionable> {
        match self {
            Self::Button { a, .. }
            | Self::Overflow { a, .. }
            | Self::StaticSelect { a, .. }
            | Self::MultiStaticSelect { a, .. }
            | Self::UsersSelect { a, .. }
            | Self::ChannelsSelect { a, .. }
            | Self::ConversationsSelect { a, .. }
            | Self::DatePicker { a, .. }
            | Self::TimePicker { a, .. }
            | Self::PlainTextInput { a, .. }
            | Self::LinearScale { a, .. }
            | Self::Checkbox { a, .. }
            | Self::ToggleSwitch { a, .. }
            | Self::RadioButton { a, .. }
            | Self::Tab { a, .. }
            | Self::IconButton { a, .. } => Some(a),
            Self::Icon(_) | Self::Image { .. } | Self::Text(_) | Self::Unknown(_) => None,
        }
    }

    pub fn action_id(&self) -> Option<&str> {
        self.actionable().map(|a| a.action_id.as_str())
    }

    /// A copy with `appId`/`blockId` inherited from `block` (see [`Actionable::inherit`]).
    pub fn inherited(&self, block: &BlockMeta) -> Self {
        let mut e = self.clone();
        if let Some(a) = e.actionable_mut() {
            *a = a.inherit(block);
        }
        e
    }

    /// `url` of a link button / icon button.
    pub fn url(&self) -> Option<&str> {
        match self {
            Self::Button { url, .. } | Self::IconButton { url, .. } => url.as_deref(),
            _ => None,
        }
    }

    /// Initial value for view state (§2.4, `getInitialValue`), keeping `0` and
    /// `""`. `None` when the element has none (it is then absent from `state`).
    pub fn initial_value(&self) -> Option<Value> {
        match self {
            Self::StaticSelect { initial, .. }
            | Self::DatePicker { initial, .. }
            | Self::TimePicker { initial, .. }
            | Self::PlainTextInput { initial, .. }
            | Self::RadioButton { initial, .. } => initial.clone().map(Value::String),
            Self::MultiStaticSelect { initial, .. }
            | Self::Checkbox { initial, .. }
            | Self::ToggleSwitch { initial, .. } => initial
                .as_ref()
                .map(|v| Value::Array(v.iter().cloned().map(Value::String).collect())),
            Self::LinearScale { initial, .. } => initial.clone(),
            _ => None,
        }
    }

    /// Whether the element may appear in `slot` (the element unions typed on
    /// each block, plus the web's rule that users/channels selects render only
    /// in input blocks; conversations selects and stray tabs never render).
    pub fn allowed_in(&self, slot: Slot) -> bool {
        use Element as E;
        match slot {
            Slot::Actions => matches!(
                self,
                E::Button { .. }
                    | E::DatePicker { .. }
                    | E::LinearScale { .. }
                    | E::MultiStaticSelect { .. }
                    | E::Overflow { .. }
                    | E::StaticSelect { .. }
                    | E::ToggleSwitch { .. }
                    | E::Checkbox { .. }
                    | E::RadioButton { .. }
                    | E::TimePicker { .. }
            ),
            Slot::Input => matches!(
                self,
                E::ChannelsSelect { .. }
                    | E::DatePicker { .. }
                    | E::LinearScale { .. }
                    | E::MultiStaticSelect { .. }
                    | E::UsersSelect { .. }
                    | E::PlainTextInput { .. }
                    | E::StaticSelect { .. }
                    | E::Checkbox { .. }
                    | E::RadioButton { .. }
                    | E::TimePicker { .. }
                    | E::ToggleSwitch { .. }
            ),
            Slot::SectionAccessory => matches!(
                self,
                E::Button { .. }
                    | E::DatePicker { .. }
                    | E::Image { .. }
                    | E::MultiStaticSelect { .. }
                    | E::Overflow { .. }
                    | E::StaticSelect { .. }
            ),
            Slot::Context => matches!(self, E::Text(_) | E::Image { .. }),
            Slot::CalloutAccessory => matches!(self, E::Button { .. } | E::Overflow { .. }),
            Slot::InfoCardElement => matches!(self, E::Icon(_) | E::Text(_)),
            Slot::InfoCardAction => matches!(self, E::IconButton { .. }),
            Slot::TabNavigation => matches!(self, E::Tab { .. }),
        }
    }
}

/// Where an element sits in its block (for [`Element::allowed_in`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Slot {
    Actions,
    Input,
    SectionAccessory,
    Context,
    CalloutAccessory,
    InfoCardElement,
    InfoCardAction,
    TabNavigation,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum CalloutVariant {
    #[default]
    Info,
    Danger,
    Warning,
    Success,
}

/// Image of a `preview` block.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PreviewImage {
    pub url: String,
    /// `(width, height)`.
    pub dimensions: Option<(u32, u32)>,
}

impl PreviewImage {
    fn from_value(v: &Value) -> Option<Self> {
        let o = v.as_object()?;
        let dims = o
            .get("dimensions")
            .and_then(Value::as_object)
            .and_then(|d| {
                let w = u32::try_from(int(d, "width")?).ok()?;
                let h = u32::try_from(int(d, "height")?).ok()?;
                Some((w, h))
            });
        Some(Self {
            url: s(o, "url")?,
            dimensions: dims,
        })
    }
}

/// `preview` block (`UK:blocks/layout/PreviewBlock.ts`).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PreviewBlock {
    pub meta: BlockMeta,
    pub title: Vec<TextObject>,
    pub description: Vec<TextObject>,
    /// Elements of the `footer` context block.
    pub footer: Option<Vec<Element>>,
    pub thumb: Option<PreviewImage>,
    pub preview: Option<PreviewImage>,
    pub external_url: Option<String>,
    pub oembed_url: Option<String>,
    /// The block has an `externalUrl` or `oembedUrl` key (even if empty).
    pub has_link_key: bool,
}

impl PreviewBlock {
    /// The web shows the cover image only with a link key and known dimensions.
    pub fn shows_cover(&self) -> bool {
        self.has_link_key
            && self
                .preview
                .as_ref()
                .is_some_and(|p| p.dimensions.is_some())
    }
}

/// `info_card` row.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct InfoCardRow {
    /// `default` or `secondary`.
    pub background: String,
    pub elements: Vec<Element>,
    pub action: Option<Box<Element>>,
}

/// A layout block (`UK:blocks/LayoutBlockType.ts`).
#[derive(Debug, Clone, PartialEq)]
pub enum LayoutBlock {
    Section {
        meta: BlockMeta,
        text: Option<TextObject>,
        fields: Vec<TextObject>,
        accessory: Option<Box<Element>>,
    },
    Divider {
        meta: BlockMeta,
    },
    Image {
        meta: BlockMeta,
        image_url: String,
        alt_text: String,
        title: Option<TextObject>,
    },
    Actions {
        meta: BlockMeta,
        elements: Vec<Element>,
    },
    Context {
        meta: BlockMeta,
        elements: Vec<Element>,
    },
    Input {
        meta: BlockMeta,
        label: TextObject,
        element: Box<Element>,
        hint: Option<TextObject>,
        optional: bool,
    },
    Conditional {
        meta: BlockMeta,
        /// `when.engine`; `None` matches every engine.
        engines: Option<Vec<String>>,
        render: Vec<LayoutBlock>,
    },
    Preview(Box<PreviewBlock>),
    VideoConf {
        meta: BlockMeta,
        call_id: String,
    },
    Callout {
        meta: BlockMeta,
        title: Option<TextObject>,
        text: TextObject,
        variant: CalloutVariant,
        accessory: Option<Box<Element>>,
    },
    TabNavigation {
        meta: BlockMeta,
        tabs: Vec<Element>,
    },
    InfoCard {
        meta: BlockMeta,
        rows: Vec<InfoCardRow>,
    },
    Unknown(Value),
}

fn elements(o: &Obj, k: &str) -> Vec<Element> {
    match o.get(k) {
        Some(Value::Array(a)) => a
            .iter()
            .take(MAX_ELEMENTS)
            .map(Element::from_value)
            .collect(),
        _ => Vec::new(),
    }
}

impl LayoutBlock {
    /// Decodes one block; unknown types and unusable values become `Unknown`.
    pub fn from_value(v: &Value) -> Self {
        Self::decode(v, true).unwrap_or_else(|| Self::Unknown(v.clone()))
    }

    fn decode(v: &Value, top: bool) -> Option<Self> {
        let o = v.as_object()?;
        let kind = o.get("type")?.as_str()?;
        let meta = BlockMeta::from_obj(o);
        Some(match kind {
            "section" => Self::Section {
                meta,
                text: text(o, "text"),
                fields: texts(o, "fields").into_iter().take(MAX_ELEMENTS).collect(),
                accessory: o.get("accessory").map(|a| Box::new(Element::from_value(a))),
            },
            "divider" => Self::Divider { meta },
            "image" => Self::Image {
                meta,
                image_url: s(o, "imageUrl")?,
                alt_text: s(o, "altText").unwrap_or_default(),
                title: text(o, "title"),
            },
            "actions" => Self::Actions {
                meta,
                elements: elements(o, "elements"),
            },
            "context" => Self::Context {
                meta,
                elements: elements(o, "elements"),
            },
            "input" => Self::Input {
                meta,
                label: text(o, "label").unwrap_or_default(),
                element: Box::new(Element::from_value(o.get("element")?)),
                hint: text(o, "hint"),
                optional: flag(o, "optional"),
            },
            // Not recursive: blocks inside `render` are plain layout blocks.
            "conditional" if top => Self::Conditional {
                meta,
                engines: o
                    .get("when")
                    .and_then(Value::as_object)
                    .and_then(|w| w.get("engine"))
                    .map(|e| strings(Some(e))),
                render: match o.get("render") {
                    Some(Value::Array(a)) => a
                        .iter()
                        .take(MAX_BLOCKS)
                        .map(|b| Self::decode(b, false).unwrap_or_else(|| Self::Unknown(b.clone())))
                        .collect(),
                    _ => Vec::new(),
                },
            },
            "preview" => Self::Preview(Box::new(PreviewBlock {
                meta,
                title: texts(o, "title"),
                description: texts(o, "description"),
                footer: o
                    .get("footer")
                    .and_then(Value::as_object)
                    .map(|f| elements(f, "elements")),
                thumb: o.get("thumb").and_then(PreviewImage::from_value),
                preview: o.get("preview").and_then(PreviewImage::from_value),
                external_url: s(o, "externalUrl").filter(|u| !u.is_empty()),
                oembed_url: s(o, "oembedUrl").filter(|u| !u.is_empty()),
                has_link_key: o.contains_key("externalUrl") || o.contains_key("oembedUrl"),
            })),
            "video_conf" => Self::VideoConf {
                meta,
                call_id: s(o, "callId")?,
            },
            "callout" => Self::Callout {
                meta,
                title: text(o, "title"),
                text: text(o, "text").unwrap_or_default(),
                variant: match o.get("variant").and_then(Value::as_str) {
                    Some("danger") => CalloutVariant::Danger,
                    Some("warning") => CalloutVariant::Warning,
                    Some("success") => CalloutVariant::Success,
                    _ => CalloutVariant::Info,
                },
                accessory: o.get("accessory").map(|a| Box::new(Element::from_value(a))),
            },
            "tab_navigation" => Self::TabNavigation {
                meta,
                tabs: elements(o, "tabs"),
            },
            "info_card" => Self::InfoCard {
                meta,
                rows: match o.get("rows") {
                    Some(Value::Array(a)) => a
                        .iter()
                        .take(MAX_ELEMENTS)
                        .filter_map(Value::as_object)
                        .map(|r| InfoCardRow {
                            background: s(r, "background").unwrap_or_else(|| "default".into()),
                            elements: elements(r, "elements"),
                            action: r.get("action").map(|a| Box::new(Element::from_value(a))),
                        })
                        .collect(),
                    _ => Vec::new(),
                },
            },
            _ => return None,
        })
    }

    /// The JSON `type` (`"unknown"` for undecodable values).
    pub fn type_name(&self) -> &'static str {
        match self {
            Self::Section { .. } => "section",
            Self::Divider { .. } => "divider",
            Self::Image { .. } => "image",
            Self::Actions { .. } => "actions",
            Self::Context { .. } => "context",
            Self::Input { .. } => "input",
            Self::Conditional { .. } => "conditional",
            Self::Preview(_) => "preview",
            Self::VideoConf { .. } => "video_conf",
            Self::Callout { .. } => "callout",
            Self::TabNavigation { .. } => "tab_navigation",
            Self::InfoCard { .. } => "info_card",
            Self::Unknown(_) => "unknown",
        }
    }

    pub fn meta(&self) -> Option<&BlockMeta> {
        match self {
            Self::Section { meta, .. }
            | Self::Divider { meta }
            | Self::Image { meta, .. }
            | Self::Actions { meta, .. }
            | Self::Context { meta, .. }
            | Self::Input { meta, .. }
            | Self::Conditional { meta, .. }
            | Self::VideoConf { meta, .. }
            | Self::Callout { meta, .. }
            | Self::TabNavigation { meta, .. }
            | Self::InfoCard { meta, .. } => Some(meta),
            Self::Preview(p) => Some(&p.meta),
            Self::Unknown(_) => None,
        }
    }

    /// Interactive elements of this block (not descending into conditionals),
    /// with `appId`/`blockId` inherited from the block.
    pub fn inherited_elements(&self) -> Vec<Element> {
        let Some(meta) = self.meta() else {
            return Vec::new();
        };
        let mut out: Vec<&Element> = Vec::new();
        match self {
            Self::Section { accessory, .. } | Self::Callout { accessory, .. } => {
                out.extend(accessory.as_deref());
            }
            Self::Actions { elements, .. } | Self::Context { elements, .. } => {
                out.extend(elements.iter());
            }
            Self::Input { element, .. } => out.push(element),
            Self::TabNavigation { tabs, .. } => out.extend(tabs.iter()),
            Self::InfoCard { rows, .. } => {
                out.extend(rows.iter().filter_map(|r| r.action.as_deref()));
            }
            _ => {}
        }
        out.into_iter()
            .filter(|e| e.actionable().is_some())
            .map(|e| e.inherited(meta))
            .collect()
    }
}

/// Replaces each `conditional` block by its `render` list when `when.engine`
/// is absent or contains `engine`, else drops it (`resolveConditionalBlocks`).
pub fn resolve_conditionals(blocks: &[LayoutBlock], engine: &str) -> Vec<LayoutBlock> {
    let mut out = Vec::with_capacity(blocks.len());
    for b in blocks {
        match b {
            LayoutBlock::Conditional {
                engines, render, ..
            } => {
                if engines
                    .as_ref()
                    .is_none_or(|e| e.iter().any(|x| x == engine))
                {
                    out.extend(render.iter().cloned());
                }
            }
            other => out.push(other.clone()),
        }
    }
    out
}

/// A UIKit surface: decides which block types render (§2.5).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Surface {
    Message,
    Modal,
    ContextualBar,
    Banner,
}

impl Surface {
    /// Whether this surface renders `block` (after conditional resolution).
    pub fn allows(self, block: &LayoutBlock) -> bool {
        use LayoutBlock as B;
        let common = matches!(
            block,
            B::Actions { .. }
                | B::Context { .. }
                | B::Divider { .. }
                | B::Image { .. }
                | B::Section { .. }
                | B::Preview(_)
        );
        common
            || match self {
                // `input` renders read-only in messages.
                Self::Message => matches!(
                    block,
                    B::VideoConf { .. } | B::Callout { .. } | B::InfoCard { .. } | B::Input { .. }
                ),
                Self::Modal => matches!(block, B::Input { .. } | B::Callout { .. }),
                Self::ContextualBar => matches!(
                    block,
                    B::Input { .. } | B::Callout { .. } | B::TabNavigation { .. }
                ),
                Self::Banner => matches!(block, B::Input { .. } | B::InfoCard { .. }),
            }
    }

    /// The blocks of `blocks` this surface renders: conditionals resolved for
    /// [`ENGINE`], disallowed and unknown blocks dropped, capped at [`MAX_BLOCKS`].
    pub fn renderable(self, blocks: &[LayoutBlock]) -> Vec<LayoutBlock> {
        resolve_conditionals(blocks, ENGINE)
            .into_iter()
            .filter(|b| self.allows(b))
            .take(MAX_BLOCKS)
            .collect()
    }
}
