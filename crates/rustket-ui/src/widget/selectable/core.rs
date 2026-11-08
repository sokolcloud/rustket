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
    /// Byte ranges of concealed spans (hidden spoilers): never highlighted
    /// nor copied.
    pub(super) concealed: Vec<Range<usize>>,
    /// The last left press landed on a link of this block (the block
    /// captured it; its scope still handles it).
    pub(super) link_press: bool,
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
        self.hit_with_affinity(p).0
    }

    /// Byte offset of the character under `p` (its start), for word
    /// selection: [`Self::hit`] gives the boundary after a glyph when `p` is
    /// on its right half.
    pub(super) fn hit_under(&self, p: Point) -> usize {
        let (offset, line_start, after_glyph) = self.hit_with_affinity(p);
        if !after_glyph || offset <= line_start {
            return offset;
        }
        self.text
            .get(line_start..offset)
            .and_then(|s| s.grapheme_indices(true).next_back())
            .map_or(offset, |(i, _)| line_start + i)
    }

    /// The hit offset, the start of its line, and whether it is the boundary
    /// after the glyph under `p` (cosmic-text's `Affinity::Before`).
    fn hit_with_affinity(&self, p: Point) -> (usize, usize, bool) {
        let buffer = self.paragraph.buffer();
        let Some(c) = buffer.hit(p.x, p.y) else {
            // Above the first line (or empty): the start; below: the end.
            let at = if p.y < 0.0 { 0 } else { self.len() };
            return (at, at, false);
        };
        let start = self.lines.get(c.line).copied().unwrap_or(self.len());
        (
            floor_boundary(&self.text, start + c.index),
            start,
            c.affinity.before(),
        )
    }

    /// Whether byte `i` is inside a concealed span.
    fn is_concealed(&self, i: usize) -> bool {
        self.concealed.iter().any(|r| r.contains(&i))
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
            // Runs of adjacent selected glyphs (concealed ones split them).
            let mut segments: Vec<(f32, f32)> = Vec::new();
            let mut open = false;
            for g in run.glyphs {
                let (a, b) = (line_start + g.start, line_start + g.end);
                if a < range.end && b > range.start && !self.is_concealed(a) {
                    match segments.last_mut() {
                        Some(seg) if open => {
                            seg.0 = seg.0.min(g.x);
                            seg.1 = seg.1.max(g.x + g.w);
                        }
                        _ => segments.push((g.x, g.x + g.w)),
                    }
                    open = true;
                } else {
                    open = false;
                }
            }
            // The line break after the last run is selected too.
            if last_of_line
                && line_end < self.len()
                && range.start <= line_end
                && range.end > line_end
            {
                let end = run.glyphs.iter().map(|g| g.x + g.w).fold(0.0, f32::max);
                match segments.last_mut() {
                    Some(seg) if open && seg.1 >= end => seg.1 = end + stub,
                    _ => segments.push((end, end + stub)),
                }
            }
            for (x0, x1) in segments {
                if x1 > x0 {
                    out.push(Rectangle::new(
                        Point::new(x0, run.line_top),
                        Size::new(x1 - x0, run.line_height),
                    ));
                }
            }
        }
        out
    }

    /// The selected text without concealed spans (empty without a
    /// selection).
    pub(super) fn selected(&self) -> String {
        let Some(sel) = &self.selection else {
            return String::new();
        };
        let mut out = String::new();
        let mut at = sel.start;
        let mut hidden: Vec<_> = self
            .concealed
            .iter()
            .filter(|r| r.start < sel.end && r.end > sel.start)
            .collect();
        hidden.sort_by_key(|r| r.start);
        for r in hidden {
            if r.start > at {
                out.push_str(self.text.get(at..r.start).unwrap_or_default());
            }
            at = at.max(r.end);
        }
        if at < sel.end {
            out.push_str(self.text.get(at..sel.end).unwrap_or_default());
        }
        out
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
