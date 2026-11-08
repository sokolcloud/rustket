//! The non-generic part of a selectable text block: its shaped paragraph, its
//! plain text, hit testing and selection geometry. Scopes reach it through
//! `Operation::custom` (the leaf passes `&mut Core`), so it must not depend
//! on the leaf's `Link` type.

use std::ops::Range;

use iced::advanced::text::{self as adv_text};
use iced::{Point, Rectangle, Size, Vector};
use unicode_segmentation::UnicodeSegmentation;

pub(super) type Paragraph = <iced::Renderer as adv_text::Renderer>::Paragraph;

/// What a scope needs from one text block.
#[derive(Debug, Default)]
pub struct Core {
    pub(super) paragraph: Paragraph,
    /// The plain text of all spans (line separators normalised to `\n`).
    pub(super) text: String,
    /// Byte offset where each `\n`-separated line starts.
    pub(super) lines: Vec<usize>,
    /// Selected byte range (sorted, on char boundaries), set by the scope.
    pub(super) selection: Option<Range<usize>>,
    /// Last cursor position relative to the paragraph origin (`None` while
    /// the cursor is unavailable to this block, e.g. under a hover layer).
    pub(super) cursor: Option<Point>,
    /// Size of the paragraph area (the layout bounds).
    pub(super) size: Size,
    /// Offset of the paragraph origin inside the layout bounds (alignment).
    pub(super) origin: Vector,
    /// Prepended to the copied text when the selection starts at the block
    /// start (list markers).
    pub(super) prefix: Option<String>,
    /// Put between the previous block's copied text and this one.
    pub(super) joiner: &'static str,
    /// Inside a selection scope: shows the text cursor.
    pub(super) scoped: bool,
}

impl Core {
    /// Replaces the text (after a span change), keeping the selection within it.
    pub(super) fn set_text(&mut self, text: String) {
        self.lines = line_starts(&text);
        self.text = text;
        if let Some(sel) = self.selection.take() {
            let r = clamp_range(&self.text, sel);
            self.selection = (!r.is_empty()).then_some(r);
        }
    }

    pub(super) fn len(&self) -> usize {
        self.text.len()
    }

    /// Byte offset of the character boundary nearest to `p` (relative to the
    /// paragraph origin). Points outside clamp to the nearest line/end.
    pub(super) fn hit(&self, p: Point) -> usize {
        let buffer = self.paragraph.buffer();
        let Some(c) = buffer.hit(p.x, p.y) else {
            // Above the first line (or empty): the start; below: the end.
            return if p.y < 0.0 { 0 } else { self.len() };
        };
        let start = self.lines.get(c.line).copied().unwrap_or(self.len());
        floor_boundary(&self.text, start + c.index)
    }

    /// Whether `p` (relative to the bounds) is inside this block.
    pub(super) fn contains(&self, p: Point) -> bool {
        p.x >= 0.0 && p.y >= 0.0 && p.x <= self.size.width && p.y <= self.size.height
    }

    /// Highlight rectangles of `range`, relative to the paragraph origin: one
    /// per visual line, a short stub for selected line breaks and empty lines.
    pub(super) fn rects(&self, range: &Range<usize>) -> Vec<Rectangle> {
        let mut out = Vec::new();
        if range.is_empty() {
            return out;
        }
        let buffer = self.paragraph.buffer();
        let stub = buffer.metrics().font_size * 0.3;
        let runs: Vec<_> = buffer.layout_runs().collect();
        for (i, run) in runs.iter().enumerate() {
            let Some(&line_start) = self.lines.get(run.line_i) else {
                continue;
            };
            let line_end = self
                .lines
                .get(run.line_i + 1)
                .map_or(self.len(), |next| next - 1);
            let last_of_line = runs.get(i + 1).is_none_or(|n| n.line_i != run.line_i);
            let mut x0 = f32::INFINITY;
            let mut x1 = f32::NEG_INFINITY;
            for g in run.glyphs {
                let (a, b) = (line_start + g.start, line_start + g.end);
                if a < range.end && b > range.start {
                    x0 = x0.min(g.x);
                    x1 = x1.max(g.x + g.w);
                }
            }
            if run.glyphs.is_empty() {
                // An empty line: selected when the range spans it.
                if range.start <= line_start && range.end > line_start {
                    out.push(Rectangle::new(
                        Point::new(0.0, run.line_top),
                        Size::new(stub, run.line_height),
                    ));
                }
                continue;
            }
            // The line break after the last run is selected too.
            if last_of_line
                && line_end < self.len()
                && range.start <= line_end
                && range.end > line_end
            {
                let end = run.glyphs.iter().map(|g| g.x + g.w).fold(0.0, f32::max);
                x0 = x0.min(end);
                x1 = x1.max(end + stub);
            }
            if x1 > x0 {
                out.push(Rectangle::new(
                    Point::new(x0, run.line_top),
                    Size::new(x1 - x0, run.line_height),
                ));
            }
        }
        out
    }

    /// The selected text (empty without a selection).
    pub(super) fn selected(&self) -> &str {
        self.selection
            .as_ref()
            .and_then(|r| self.text.get(r.clone()))
            .unwrap_or_default()
    }
}

/// Byte offsets where each line of `text` starts.
pub(super) fn line_starts(text: &str) -> Vec<usize> {
    std::iter::once(0)
        .chain(text.match_indices('\n').map(|(i, _)| i + 1))
        .collect()
}

/// Normalises paragraph separators to `\n` so lines match the shaped buffer
/// (`\r\n` becomes one `\n`). Borrowed when there is nothing to change.
pub(super) fn normalize_breaks(text: &str) -> std::borrow::Cow<'_, str> {
    const SEPARATORS: [char; 6] = ['\r', '\u{1c}', '\u{1d}', '\u{1e}', '\u{85}', '\u{2029}'];
    if !text.contains(SEPARATORS) {
        return text.into();
    }
    text.replace("\r\n", "\n").replace(SEPARATORS, "\n").into()
}

/// The char boundary at or before `i`, within `text`.
pub(super) fn floor_boundary(text: &str, i: usize) -> usize {
    let mut i = i.min(text.len());
    while !text.is_char_boundary(i) {
        i -= 1;
    }
    i
}

/// `range` limited to `text` and moved onto character boundaries.
pub(super) fn clamp_range(text: &str, range: Range<usize>) -> Range<usize> {
    let a = floor_boundary(text, range.start);
    let b = floor_boundary(text, range.end);
    a.min(b)..a.max(b)
}

/// The word (or run of whitespace / punctuation) at `offset`, for
/// double-click selection.
pub(super) fn word_at(text: &str, offset: usize) -> Range<usize> {
    let mut last = 0..0;
    for (i, w) in text.split_word_bound_indices() {
        let r = i..i + w.len();
        if r.contains(&offset) {
            return r;
        }
        last = r;
    }
    // At the very end: the last word.
    if offset >= text.len() {
        last
    } else {
        offset..offset
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn line_starts_and_breaks() {
        assert_eq!(line_starts("ab\ncd\n"), [0, 3, 6]);
        assert_eq!(line_starts(""), [0]);
        assert_eq!(normalize_breaks("a\r\nb\rc\u{2029}d"), "a\nb\nc\nd");
        assert!(matches!(
            normalize_breaks("plain\ntext"),
            std::borrow::Cow::Borrowed(_)
        ));
    }

    #[test]
    fn clamps_to_char_boundaries() {
        let t = "aж😀";
        assert_eq!(clamp_range(t, 0..2), 0..1);
        assert_eq!(clamp_range(t, 2..100), 1..t.len());
        #[allow(clippy::reversed_empty_ranges)]
        let reversed = 5..1;
        assert_eq!(clamp_range(t, reversed), 1..3);
    }

    #[test]
    fn words() {
        let t = "hello, wörld  foo_bar";
        assert_eq!(&t[word_at(t, 1)], "hello");
        assert_eq!(&t[word_at(t, 5)], ",");
        assert_eq!(&t[word_at(t, 8)], "wörld");
        assert_eq!(&t[word_at(t, 14)], "  ");
        assert_eq!(&t[word_at(t, 16)], "foo_bar");
        assert_eq!(&t[word_at(t, t.len())], "foo_bar");
        assert_eq!(word_at("", 0), 0..0);
    }
}
