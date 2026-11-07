//! The `md` AST produced by `@rocket.chat/message-parser`.
//!
//! Every node enum has an `Unknown(Value)` fallback, so a node type added by a newer
//! server (or a malformed known node) never breaks deserialisation of a message.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// `md` root: a list of blocks (or a single `BIG_EMOJI`).
pub type Root = Vec<Block>;

/// Deepest JSON nesting accepted for an `md` tree. Real messages stay far below.
pub const MAX_MD_DEPTH: usize = 32;
/// Most JSON values accepted in one `md` tree.
pub const MAX_MD_NODES: usize = 20_000;
/// Most string bytes accepted in one `md` tree.
pub const MAX_MD_TEXT_BYTES: usize = 1 << 20;

/// Whether an `md` value is small enough to decode. Any user can send a message
/// with an empty `msg` and an arbitrary `md`, which the server stores and relays
/// verbatim, and decoding the node enums costs memory proportional to depth
/// times size, so oversized trees are dropped (the text in `msg` stays).
pub fn within_limits(v: &Value) -> bool {
    let mut stack = vec![(v, 1usize)];
    let (mut nodes, mut bytes) = (0usize, 0usize);
    while let Some((v, depth)) = stack.pop() {
        nodes += 1;
        if depth > MAX_MD_DEPTH || nodes > MAX_MD_NODES {
            return false;
        }
        match v {
            Value::String(s) => bytes += s.len(),
            Value::Array(a) => stack.extend(a.iter().map(|c| (c, depth + 1))),
            Value::Object(o) => {
                for (k, c) in o {
                    bytes += k.len();
                    stack.push((c, depth + 1));
                }
            }
            _ => {}
        }
        if bytes > MAX_MD_TEXT_BYTES {
            return false;
        }
    }
    true
}

/// Lenient `Option<Root>` field deserialiser that drops trees outside
/// [`within_limits`] before decoding them.
pub fn opt_root<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<Root>, D::Error> {
    let Some(v) = Option::<Value>::deserialize(d)? else {
        return Ok(None);
    };
    if !within_limits(&v) {
        tracing::debug!("dropping oversized md tree");
        return Ok(None);
    }
    Ok(super::lenient::try_convert(v))
}

/// `{"type": "PLAIN_TEXT", "value": ".."}` used as a leaf inside other nodes.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(tag = "type", rename = "PLAIN_TEXT")]
pub struct Plain {
    #[serde(default)]
    pub value: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "SCREAMING_SNAKE_CASE")]
pub enum Block {
    Paragraph {
        #[serde(default)]
        value: Vec<Inline>,
    },
    BigEmoji {
        #[serde(default)]
        value: Vec<Inline>,
    },
    Code {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        language: Option<String>,
        #[serde(default)]
        value: Vec<CodeLine>,
    },
    Heading {
        #[serde(default = "one")]
        level: u8,
        #[serde(default)]
        value: Vec<Inline>,
    },
    /// `> text` — a list of paragraphs.
    Quote {
        #[serde(default)]
        value: Vec<Block>,
    },
    /// Legacy name of `QUOTE`.
    Blockquote {
        #[serde(default)]
        value: Vec<Block>,
    },
    SpoilerBlock {
        #[serde(default)]
        value: Vec<Block>,
    },
    UnorderedList {
        #[serde(default)]
        value: Vec<ListItem>,
    },
    OrderedList {
        #[serde(default)]
        value: Vec<ListItem>,
    },
    Tasks {
        #[serde(default)]
        value: Vec<Task>,
    },
    /// A standalone list item (appears in `Blocks` in the typings).
    ListItem {
        #[serde(default)]
        value: Vec<Inline>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        number: Option<u64>,
    },
    LineBreak,
    HorizontalRule {
        /// `[start, end]` source span (or a legacy `Plain`).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        fallback: Option<Value>,
    },
    Katex {
        #[serde(default)]
        value: String,
    },
    Table {
        value: TableValue,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        fallback: Option<Value>,
    },
    #[serde(untagged)]
    Unknown(Value),
}

fn one() -> u8 {
    1
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "SCREAMING_SNAKE_CASE")]
pub enum Inline {
    PlainText {
        #[serde(default)]
        value: String,
    },
    Bold {
        #[serde(default)]
        value: Vec<Inline>,
    },
    Italic {
        #[serde(default)]
        value: Vec<Inline>,
    },
    Strike {
        #[serde(default)]
        value: Vec<Inline>,
    },
    Spoiler {
        #[serde(default)]
        value: Vec<Inline>,
    },
    InlineCode {
        value: Plain,
    },
    Link {
        value: LinkValue,
    },
    Image {
        value: ImageValue,
    },
    MentionUser {
        value: Plain,
    },
    MentionChannel {
        value: Plain,
    },
    Emoji(Emoji),
    Color {
        value: Rgba,
    },
    InlineKatex {
        #[serde(default)]
        value: String,
    },
    Timestamp {
        value: TimestampValue,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        fallback: Option<Value>,
    },
    #[serde(untagged)]
    Unknown(Value),
}

/// `EMOJI`: either `{value: Plain, shortCode}` or `{value: null, unicode}`.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct Emoji {
    #[serde(default)]
    pub value: Option<Plain>,
    #[serde(rename = "shortCode", default, skip_serializing_if = "Option::is_none")]
    pub short_code: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unicode: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LinkValue {
    pub src: Plain,
    #[serde(default)]
    pub label: OneOrMany<Inline>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ImageValue {
    pub src: Plain,
    #[serde(default)]
    pub label: OneOrMany<Inline>,
}

/// A node or list of nodes (`LINK.label` is `Markup | Markup[]`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum OneOrMany<T> {
    Many(Vec<T>),
    One(Box<T>),
}

impl<T> Default for OneOrMany<T> {
    fn default() -> Self {
        Self::Many(Vec::new())
    }
}

impl<T> OneOrMany<T> {
    pub fn as_slice(&self) -> &[T] {
        match self {
            Self::Many(v) => v,
            Self::One(b) => std::slice::from_ref(b),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
pub struct Rgba {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    #[serde(default)]
    pub a: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TimestampValue {
    /// Unix seconds as a string.
    pub timestamp: String,
    /// `t T d D f F R`; absent means the default format.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub format: Option<String>,
}

impl TimestampValue {
    pub fn date(&self) -> Option<crate::RcDate> {
        let secs: i64 = self.timestamp.trim().parse().ok()?;
        crate::RcDate::from_millis(secs.checked_mul(1000)?)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename = "CODE_LINE")]
pub struct CodeLine {
    pub value: Plain,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename = "LIST_ITEM")]
pub struct ListItem {
    #[serde(default)]
    pub value: Vec<Inline>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub number: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename = "TASK")]
pub struct Task {
    #[serde(default)]
    pub status: bool,
    #[serde(default)]
    pub value: Vec<Inline>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TableValue {
    #[serde(default)]
    pub header: Vec<TableCell>,
    #[serde(default)]
    pub rows: Vec<TableRow>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename = "TABLE_ROW")]
pub struct TableRow {
    #[serde(default)]
    pub value: Vec<TableCell>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename = "TABLE_CELL")]
pub struct TableCell {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub align: Option<String>,
    #[serde(default)]
    pub value: Vec<Inline>,
}

impl Inline {
    /// Plain-text rendering of the node (for previews/notifications).
    pub fn plain_text(&self, out: &mut String) {
        match self {
            Inline::PlainText { value } | Inline::InlineKatex { value } => out.push_str(value),
            Inline::Bold { value }
            | Inline::Italic { value }
            | Inline::Strike { value }
            | Inline::Spoiler { value } => value.iter().for_each(|i| i.plain_text(out)),
            Inline::InlineCode { value } => out.push_str(&value.value),
            Inline::Link { value } => {
                let before = out.len();
                value
                    .label
                    .as_slice()
                    .iter()
                    .for_each(|i| i.plain_text(out));
                if out[before..].trim().is_empty() {
                    out.truncate(before);
                    out.push_str(&value.src.value);
                }
            }
            Inline::Image { value } => value
                .label
                .as_slice()
                .iter()
                .for_each(|i| i.plain_text(out)),
            Inline::MentionUser { value } => {
                out.push('@');
                out.push_str(&value.value);
            }
            Inline::MentionChannel { value } => {
                out.push('#');
                out.push_str(&value.value);
            }
            Inline::Emoji(e) => {
                if let Some(u) = &e.unicode {
                    out.push_str(u);
                } else if let Some(s) = &e.short_code {
                    out.push(':');
                    out.push_str(s);
                    out.push(':');
                }
            }
            Inline::Color { value } => {
                out.push_str(&format!("#{:02x}{:02x}{:02x}", value.r, value.g, value.b));
            }
            Inline::Timestamp { value, .. } => out.push_str(&value.timestamp),
            Inline::Unknown(_) => {}
        }
    }
}

impl Block {
    pub fn plain_text(&self, out: &mut String) {
        let inl = |v: &[Inline], out: &mut String| v.iter().for_each(|i| i.plain_text(out));
        match self {
            Block::Paragraph { value }
            | Block::BigEmoji { value }
            | Block::Heading { value, .. }
            | Block::ListItem { value, .. } => inl(value, out),
            Block::Code { value, .. } => {
                for (i, l) in value.iter().enumerate() {
                    if i > 0 {
                        out.push('\n');
                    }
                    out.push_str(&l.value.value);
                }
            }
            Block::Quote { value }
            | Block::Blockquote { value }
            | Block::SpoilerBlock { value } => {
                for (i, b) in value.iter().enumerate() {
                    if i > 0 {
                        out.push('\n');
                    }
                    b.plain_text(out);
                }
            }
            Block::UnorderedList { value } | Block::OrderedList { value } => {
                for (i, item) in value.iter().enumerate() {
                    if i > 0 {
                        out.push('\n');
                    }
                    inl(&item.value, out);
                }
            }
            Block::Tasks { value } => {
                for (i, t) in value.iter().enumerate() {
                    if i > 0 {
                        out.push('\n');
                    }
                    inl(&t.value, out);
                }
            }
            Block::Katex { value } => out.push_str(value),
            Block::Table { value, .. } => {
                for row in std::iter::once(&value.header).chain(value.rows.iter().map(|r| &r.value))
                {
                    for (i, c) in row.iter().enumerate() {
                        if i > 0 {
                            out.push('\t');
                        }
                        inl(&c.value, out);
                    }
                    out.push('\n');
                }
            }
            Block::LineBreak | Block::HorizontalRule { .. } | Block::Unknown(_) => {}
        }
    }
}

impl Inline {
    /// Whether this node or any descendant is [`Inline::Unknown`].
    pub fn has_unknown(&self) -> bool {
        match self {
            Inline::Unknown(_) => true,
            Inline::Bold { value }
            | Inline::Italic { value }
            | Inline::Strike { value }
            | Inline::Spoiler { value } => value.iter().any(Inline::has_unknown),
            Inline::Link { value } => value.label.as_slice().iter().any(Inline::has_unknown),
            Inline::Image { value } => value.label.as_slice().iter().any(Inline::has_unknown),
            _ => false,
        }
    }
}

impl Block {
    /// Whether this block or any descendant is an `Unknown` node.
    pub fn has_unknown(&self) -> bool {
        let inl = |v: &[Inline]| v.iter().any(Inline::has_unknown);
        match self {
            Block::Unknown(_) => true,
            Block::Paragraph { value }
            | Block::BigEmoji { value }
            | Block::Heading { value, .. }
            | Block::ListItem { value, .. } => inl(value),
            Block::Quote { value }
            | Block::Blockquote { value }
            | Block::SpoilerBlock { value } => value.iter().any(Block::has_unknown),
            Block::UnorderedList { value } | Block::OrderedList { value } => {
                value.iter().any(|i| inl(&i.value))
            }
            Block::Tasks { value } => value.iter().any(|t| inl(&t.value)),
            Block::Table { value, .. } => value
                .header
                .iter()
                .chain(value.rows.iter().flat_map(|r| r.value.iter()))
                .any(|c| inl(&c.value)),
            Block::Code { .. }
            | Block::LineBreak
            | Block::HorizontalRule { .. }
            | Block::Katex { .. } => false,
        }
    }
}

/// Whether any node of `root` is unknown to this crate.
pub fn contains_unknown(root: &[Block]) -> bool {
    root.iter().any(Block::has_unknown)
}

/// Plain-text rendering of a whole `md` root, one line per block.
pub fn to_plain_text(root: &[Block]) -> String {
    let mut out = String::new();
    for (i, b) in root.iter().enumerate() {
        if i > 0 {
            out.push('\n');
        }
        b.plain_text(&mut out);
    }
    out
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn message_with_md(md: Value) -> crate::models::Message {
        serde_json::from_value(serde_json::json!({"_id": "m", "rid": "r", "msg": "", "md": md}))
            .unwrap()
    }

    #[test]
    fn hostile_md_is_dropped_on_every_path() {
        // Too deep.
        let mut node = serde_json::json!({"type": "PLAIN_TEXT", "value": "x"});
        for _ in 0..20 {
            node = serde_json::json!({"type": "BOLD", "value": [node]});
        }
        let deep = serde_json::json!([{"type": "PARAGRAPH", "value": [node]}]);
        assert!(!within_limits(&deep));
        assert!(message_with_md(deep).md.is_none());
        // Too much text.
        let big = serde_json::json!([{"type": "PARAGRAPH", "value": [
            {"type": "PLAIN_TEXT", "value": "x".repeat(MAX_MD_TEXT_BYTES + 1)}
        ]}]);
        assert!(message_with_md(big).md.is_none());
        // Too many nodes.
        let wide: Vec<Value> = (0..MAX_MD_NODES)
            .map(|_| serde_json::json!({"type": "PLAIN_TEXT", "value": "x"}))
            .collect();
        let many = serde_json::json!([{"type": "PARAGRAPH", "value": wide}]);
        assert!(message_with_md(many).md.is_none());
        // Realistic nesting is kept (quote > list > item > paragraph > bold > italic > link).
        let ok = serde_json::json!([{"type": "QUOTE", "value": [{"type": "UNORDERED_LIST", "value": [
            {"type": "LIST_ITEM", "value": [{"type": "BOLD", "value": [{"type": "ITALIC", "value": [
                {"type": "LINK", "value": {"src": {"type": "PLAIN_TEXT", "value": "https://x"},
                 "label": [{"type": "PLAIN_TEXT", "value": "x"}]}}
            ]}]}]}
        ]}]}]);
        assert!(within_limits(&ok));
        assert!(message_with_md(ok).md.is_some());
        // Attachments use the same guard.
        let m: crate::models::Message = serde_json::from_value(serde_json::json!({
            "_id": "m", "rid": "r", "msg": "",
            "attachments": [{"descriptionMd": [{"type": "PARAGRAPH", "value": [
                {"type": "PLAIN_TEXT", "value": "x".repeat(MAX_MD_TEXT_BYTES + 1)}]}]}]
        }))
        .unwrap();
        assert!(m.attachments.unwrap()[0].description_md.is_none());
    }

    #[test]
    fn unknown_nodes_are_tolerated() {
        let json = r#"[
            {"type":"PARAGRAPH","value":[
                {"type":"PLAIN_TEXT","value":"a"},
                {"type":"FANCY_NEW_INLINE","value":42},
                {"type":"LINK","value":{"src":"not-a-plain"}}
            ]},
            {"type":"HOLOGRAM","value":[1,2,3]},
            {"type":"LINE_BREAK","value":null}
        ]"#;
        let root: Root = serde_json::from_str(json).unwrap();
        assert_eq!(root.len(), 3);
        let Block::Paragraph { value } = &root[0] else {
            panic!()
        };
        assert!(matches!(value[1], Inline::Unknown(_)));
        assert!(matches!(value[2], Inline::Unknown(_)));
        assert!(matches!(root[1], Block::Unknown(_)));
        assert!(matches!(root[2], Block::LineBreak));
        assert!(root[0].has_unknown());
        assert!(contains_unknown(&root));
        let nested: Root = serde_json::from_str(
            r#"[{"type":"QUOTE","value":[{"type":"PARAGRAPH","value":[{"type":"BOLD","value":[{"type":"NEW_THING"}]}]}]}]"#,
        )
        .unwrap();
        assert!(contains_unknown(&nested));
    }

    #[test]
    fn round_trip() {
        let json = r#"[{"type":"PARAGRAPH","value":[{"type":"EMOJI","value":{"type":"PLAIN_TEXT","value":"smile"},"shortCode":"smile"},{"type":"INLINE_CODE","value":{"type":"PLAIN_TEXT","value":"x"}}]}]"#;
        let root: Root = serde_json::from_str(json).unwrap();
        let back = serde_json::to_value(&root).unwrap();
        assert_eq!(back, serde_json::from_str::<Value>(json).unwrap());
        assert_eq!(to_plain_text(&root), ":smile:x");
    }
}
