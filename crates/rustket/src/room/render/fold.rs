//! Folding of long message bodies and big code blocks: thresholds and the
//! rendered-height estimate. The toggles live in [`super::RenderState`]
//! (`expanded`), keyed by message id and [`EXPAND_BODY`] or the code block's
//! index in document order (blocks in hidden spoilers count too).

use rocketchat::models::Message;
use rocketchat::models::md::{Block, Inline};

/// Code blocks with more lines than this fold.
pub const CODE_FOLD_MIN: usize = 15;
/// Lines a folded code block keeps.
pub const CODE_FOLD_KEEP: usize = 10;
/// Bodies estimated taller than this many lines fold.
pub const BODY_FOLD_MIN_LINES: usize = 24;
/// Height of a folded body's text part (about 16 lines).
pub const BODY_FOLD_HEIGHT: f32 = 320.0;
/// Estimated lines a folded body renders before the clip: whole blocks
/// up to about this many, so that a column of many short paragraphs is not
/// squeezed into [`BODY_FOLD_HEIGHT`] (its spacing alone would fill it).
pub const BODY_FOLD_KEEP_LINES: usize = 14;
/// `expanded` index of the body itself (code blocks count from 0).
pub const EXPAND_BODY: usize = usize::MAX;
/// Characters per wrapped line in the estimate.
const CHARS_PER_LINE: usize = 90;

/// Whether a code block of `lines` lines folds.
pub fn code_folds(lines: usize) -> bool {
    lines > CODE_FOLD_MIN
}

/// Lines a code block shows folded (its lines plus the footer row).
fn folded_code_lines(lines: usize) -> usize {
    if code_folds(lines) {
        CODE_FOLD_KEEP + 1
    } else {
        lines
    }
}

/// Estimated rendered height, in text lines, of the text part of `msg`
/// (big code blocks counted folded).
pub fn estimated_lines(msg: &Message) -> usize {
    match &msg.md {
        Some(root) if !root.is_empty() => root.iter().map(block_lines).sum(),
        _ => text_lines(&msg.msg),
    }
}

/// Leading top-level blocks of `root` a folded body renders: whole
/// blocks until [`BODY_FOLD_KEEP_LINES`] is reached, at least one (the
/// clip cuts a long last one).
pub fn folded_blocks(root: &[Block]) -> usize {
    let mut lines = 0;
    for (i, b) in root.iter().enumerate() {
        if lines >= BODY_FOLD_KEEP_LINES {
            return i.max(1);
        }
        lines += block_lines(b);
    }
    root.len()
}

/// Whether the text part of `msg` folds.
pub fn body_folds(msg: &Message) -> bool {
    estimated_lines(msg) > BODY_FOLD_MIN_LINES
}

fn text_lines(s: &str) -> usize {
    if s.trim().is_empty() {
        return 0;
    }
    s.split('\n')
        .map(|l| l.chars().count().div_ceil(CHARS_PER_LINE).max(1))
        .sum()
}

fn inline_lines(v: &[Inline]) -> usize {
    let mut s = String::new();
    v.iter().for_each(|i| i.plain_text(&mut s));
    text_lines(&s).max(1)
}

fn block_lines(b: &Block) -> usize {
    match b {
        Block::Paragraph { value }
        | Block::Heading { value, .. }
        | Block::ListItem { value, .. } => inline_lines(value),
        Block::BigEmoji { .. } => 2,
        // The language/copy header takes a line.
        Block::Code { value, .. } => folded_code_lines(value.len()) + 1,
        Block::Quote { value } | Block::Blockquote { value } | Block::SpoilerBlock { value } => {
            value.iter().map(block_lines).sum()
        }
        Block::UnorderedList { value } | Block::OrderedList { value } => {
            value.iter().map(|i| inline_lines(&i.value)).sum()
        }
        Block::Tasks { value } => value.iter().map(|t| inline_lines(&t.value)).sum(),
        Block::LineBreak | Block::HorizontalRule { .. } => 1,
        Block::Katex { value } => value.lines().count().max(1),
        Block::Table { value, .. } => 1 + value.rows.len(),
        Block::Unknown(_) => {
            let mut s = String::new();
            b.plain_text(&mut s);
            text_lines(&s)
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn msg(md: serde_json::Value) -> Message {
        Message {
            id: "m1".into(),
            md: Some(serde_json::from_value(md).unwrap()),
            ..Message::default()
        }
    }

    fn para(s: &str) -> serde_json::Value {
        serde_json::json!({"type": "PARAGRAPH", "value": [{"type": "PLAIN_TEXT", "value": s}]})
    }

    fn code(lines: usize) -> serde_json::Value {
        let value: Vec<_> = (0..lines)
            .map(|i| {
                serde_json::json!({"type": "CODE_LINE", "value": {"type": "PLAIN_TEXT", "value": format!("line {i}")}})
            })
            .collect();
        serde_json::json!({"type": "CODE", "language": "rust", "value": value})
    }

    #[test]
    fn estimates() {
        assert_eq!(
            estimated_lines(&msg(serde_json::json!([para("a"), para("b")]))),
            2
        );
        // 200 characters wrap to three lines.
        let long = "x".repeat(200);
        assert_eq!(estimated_lines(&msg(serde_json::json!([para(&long)]))), 3);
        // A 40-line block counts folded: 10 lines, the footer and the header.
        assert_eq!(estimated_lines(&msg(serde_json::json!([code(40)]))), 12);
        assert_eq!(estimated_lines(&msg(serde_json::json!([code(15)]))), 16);
        // Without `md`, the raw text lines (wrapped).
        let plain = Message {
            msg: format!("a\n```\nb\n```\n{long}"),
            ..Message::default()
        };
        assert_eq!(estimated_lines(&plain), 7);
        assert_eq!(estimated_lines(&Message::default()), 0);
    }

    #[test]
    fn thresholds() {
        assert!(!code_folds(15));
        assert!(code_folds(16));
        let rows = |n: usize| {
            msg(serde_json::Value::Array(
                (0..n).map(|_| para("p")).collect(),
            ))
        };
        assert!(!body_folds(&rows(BODY_FOLD_MIN_LINES)));
        assert!(body_folds(&rows(BODY_FOLD_MIN_LINES + 1)));
        // One big code block alone folds itself, not the body.
        assert!(!body_folds(&msg(serde_json::json!([code(200)]))));
    }

    /// A folded body of many one-line paragraphs renders only the first
    /// ones (a column of all of them would be squeezed into the fold's
    /// height, down to nothing past ~80 paragraphs).
    #[test]
    fn folded_bodies_keep_leading_blocks() {
        let rows = |n: usize| -> Vec<Block> {
            serde_json::from_value(serde_json::Value::Array(
                (0..n).map(|_| para("p")).collect(),
            ))
            .unwrap()
        };
        let many = rows(100);
        assert!(body_folds(&msg(serde_json::to_value(&many).unwrap())));
        assert_eq!(folded_blocks(&many), BODY_FOLD_KEEP_LINES);
        // Spacing included, what is kept fits the fold's height.
        let row = rustket_ui::theme::TEXT_BODY * 1.4 + 4.0;
        assert!(BODY_FOLD_KEEP_LINES as f32 * row <= BODY_FOLD_HEIGHT + row);
        assert_eq!(folded_blocks(&rows(3)), 3);
        let long = "x".repeat(90 * 40);
        let big: Vec<Block> =
            serde_json::from_value(serde_json::json!([para(&long), para("b")])).unwrap();
        assert_eq!(folded_blocks(&big), 1, "one long block, cut by the clip");
    }
}
