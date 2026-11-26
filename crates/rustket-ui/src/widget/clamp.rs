//! Wrapped text limited to a number of lines; the last shown line ends with "…"
//! when the text goes on (CSS `line-clamp`, which iced 0.14 does not have).
//!
//! The text is shaped once per (content, size, font) and wrapped to the
//! available width; width changes only re-wrap it. When it needs more than
//! `lines` lines, the first `lines - 1` are drawn from that paragraph (clipped
//! below them) and the rest of the text goes into an [`Ellipsis`] on the last
//! line, which cuts it with "…" (right-to-left and mixed text included).

use std::borrow::Cow;
use std::hash::{DefaultHasher, Hash, Hasher};

use iced::advanced::graphics::text::cosmic_text::LayoutGlyph;
use iced::advanced::layout::{self, Layout};
use iced::advanced::renderer;
use iced::advanced::text::{self as adv_text, Paragraph as _, Renderer as _};
use iced::advanced::widget::{self, Operation, Tree, Widget, tree};
use iced::alignment;
use iced::widget::text::{LineHeight, Shaping, Wrapping};
use iced::{Color, Element, Font, Length, Pixels, Rectangle, Size, Theme, mouse};
use unicode_segmentation::UnicodeSegmentation;

use super::ellipsis::{Ellipsis, ellipsis, normalize};
use crate::emoji::owned_spans;
use crate::theme::{TEXT_BODY, font_regular};

type FontVersion = iced::advanced::graphics::text::Version;
type Paragraph = <iced::Renderer as adv_text::Renderer>::Paragraph;
type ColorFn<'a> = Box<dyn Fn(&Theme) -> Option<Color> + 'a>;

/// Height the text is wrapped in: finite (see `ellipsis::shape`) and far more
/// than any clamp needs.
const WRAP_HEIGHT: f32 = 100_000.0;

/// Text wrapped to the available width and cut to at most `lines` lines (2 by
/// default). Whitespace runs (including newlines) collapse to one space,
/// emoji use the colour emoji font. The height is that of the lines shown.
pub struct Clamp<'a> {
    content: Cow<'a, str>,
    lines: usize,
    size: f32,
    font: Font,
    color: Option<ColorFn<'a>>,
}

/// Creates a clamped multi-line label that fills the available width.
pub fn clamp<'a>(content: impl Into<Cow<'a, str>>) -> Clamp<'a> {
    Clamp {
        content: content.into(),
        lines: 2,
        size: TEXT_BODY,
        font: font_regular(),
        color: None,
    }
}

impl<'a> Clamp<'a> {
    /// The most lines shown (at least 1).
    pub fn lines(mut self, lines: usize) -> Self {
        self.lines = lines.max(1);
        self
    }

    pub fn size(mut self, size: f32) -> Self {
        self.size = size;
        self
    }

    pub fn font(mut self, font: Font) -> Self {
        self.font = font;
        self
    }

    pub fn color(mut self, color: Color) -> Self {
        self.color = Some(Box::new(move |_| Some(color)));
        self
    }

    /// Colour from the theme (e.g. `|t| Some(tokens(t).text_secondary)`).
    pub fn style(mut self, f: impl Fn(&Theme) -> Option<Color> + 'a) -> Self {
        self.color = Some(Box::new(f));
        self
    }
}

fn line_height() -> LineHeight {
    LineHeight::Relative(1.3)
}

fn shape(text: &str, size: f32, font: Font) -> Paragraph {
    let spans = owned_spans::<()>(text);
    Paragraph::with_spans(adv_text::Text {
        content: spans.as_slice(),
        bounds: Size::new(WRAP_HEIGHT, WRAP_HEIGHT),
        size: Pixels(size),
        line_height: line_height(),
        font,
        align_x: adv_text::Alignment::Left,
        align_y: alignment::Vertical::Top,
        shaping: Shaping::Advanced,
        wrapping: Wrapping::WordOrGlyph,
    })
}

/// What the wrapped paragraph says about line `index` (0-based).
struct LineInfo {
    /// Visual lines of the whole text.
    total: usize,
    /// Byte offset where line `index` starts.
    start: Option<usize>,
    /// Where line `index` is cut so that "…" still fits after it (see
    /// [`ltr_cut`]), when that line runs left to right in logical order.
    ltr_cut: Option<f32>,
    /// The paragraph's base direction is right to left.
    rtl: bool,
}

fn line_info(
    paragraph: &Paragraph,
    text: &str,
    graphemes: &[usize],
    index: usize,
    limit: f32,
) -> LineInfo {
    let mut info = LineInfo {
        total: 0,
        start: None,
        ltr_cut: None,
        rtl: false,
    };
    let mut previous_end = 0;
    for run in paragraph.buffer().layout_runs() {
        if info.total == 0 {
            info.rtl = run.rtl;
        }
        if info.total == index {
            let first = run.glyphs.iter().map(|g| g.start).min();
            info.start = Some(first.unwrap_or(previous_end));
            info.ltr_cut = (!run.rtl)
                .then(|| ltr_cut(run.glyphs, text, graphemes, limit))
                .flatten();
        }
        previous_end = run
            .glyphs
            .iter()
            .map(|g| g.end)
            .max()
            .unwrap_or(previous_end);
        info.total += 1;
    }
    info
}

/// Right edge of the longest run of whole graphemes from the line start that
/// ends within `limit`, trailing spaces dropped. `None` when the glyphs are not
/// left to right in logical order (bidi text goes through [`Ellipsis`]).
fn ltr_cut(glyphs: &[LayoutGlyph], text: &str, graphemes: &[usize], limit: f32) -> Option<f32> {
    let mut cut = glyphs.first().map_or(0.0, |g| g.x);
    // (grapheme start, right edge, is space) of the grapheme being read.
    let mut current: Option<(usize, f32, bool)> = None;
    let mut last_start = 0;
    let mut done = false;
    for g in glyphs {
        if g.level.is_rtl() || g.start < last_start {
            return None;
        }
        last_start = g.start;
        if done {
            continue;
        }
        let at = floor_in(graphemes, g.start);
        let right = g.x + g.w;
        match current {
            Some((start, edge, space)) if start == at => {
                current = Some((start, edge.max(right), space));
            }
            _ => {
                if let Some((_, edge, space)) = current {
                    if edge > limit {
                        done = true;
                        continue;
                    }
                    if !space {
                        cut = edge;
                    }
                }
                let space = text[at..].chars().next().is_some_and(char::is_whitespace);
                current = Some((at, right, space));
            }
        }
    }
    if !done
        && let Some((_, edge, space)) = current
        && edge <= limit
        && !space
    {
        cut = edge;
    }
    Some(cut)
}

/// The rest of `text` from `at` for the cut last line, led by a mark of the
/// whole paragraph's base direction (`rtl`): shaped alone, its base would come
/// from its own first strong character, and a tail that starts in the other
/// script would run (and put "…") the other way than the lines above.
fn tail_text(text: &str, at: usize, rtl: bool) -> String {
    let mark = if rtl { '\u{200F}' } else { '\u{200E}' };
    let rest = text.get(at..).unwrap_or_default();
    let mut out = String::with_capacity(rest.len() + mark.len_utf8());
    out.push(mark);
    out.push_str(rest);
    out
}

/// The largest of the sorted grapheme starts `graphemes` at or before `at`.
fn floor_in(graphemes: &[usize], at: usize) -> usize {
    let i = graphemes.partition_point(|b| *b <= at);
    i.checked_sub(1)
        .and_then(|i| graphemes.get(i))
        .copied()
        .unwrap_or(0)
}

/// The largest grapheme boundary of `text` at or before `at`.
fn grapheme_floor(text: &str, at: usize) -> usize {
    if at >= text.len() {
        return text.len();
    }
    text.grapheme_indices(true)
        .map(|(i, _)| i)
        .take_while(|i| *i <= at)
        .last()
        .unwrap_or(0)
}

/// Splits wrapped text for a clamp of `lines` lines: how many lines are drawn
/// from the full paragraph, and the byte offset of the rest that goes on the
/// cut last line (`None` when everything fits). Pure, for tests.
fn split_at_line(
    text: &str,
    total_lines: usize,
    last_line_start: Option<usize>,
    lines: usize,
) -> (usize, Option<usize>) {
    let lines = lines.max(1);
    if total_lines <= lines {
        return (total_lines.max(1), None);
    }
    let start = last_line_start.unwrap_or(0).min(text.len());
    let start = if text.is_char_boundary(start) {
        grapheme_floor(text, start)
    } else {
        0
    };
    (lines - 1, Some(start))
}

#[derive(Default)]
struct State {
    /// (content hash, size, font, font-system version) of the shaped text.
    key: Option<(u64, u32, Font, FontVersion)>,
    text: String,
    /// Byte offsets where the graphemes of `text` start.
    graphemes: Vec<usize>,
    full: Paragraph,
    dots: Paragraph,
    dots_width: f32,
    /// (available width, lines) the split below was computed for.
    width_key: Option<(u32, usize)>,
    /// Lines drawn from `full`.
    shown: usize,
    /// The last line, when the text needs more than the clamp's lines.
    last: Last,
}

#[derive(Default)]
enum Last {
    #[default]
    None,
    /// Line `shown` of `full`, drawn up to x, then "…".
    Cut(f32),
    /// The rest of the text from the start of that line, cut by [`Ellipsis`]
    /// (bidi lines, whose visual order differs from the logical one).
    Tail(Box<(Ellipsis<'static>, Tree)>),
}

fn hash_str(s: &str) -> u64 {
    let mut h = DefaultHasher::new();
    s.hash(&mut h);
    h.finish()
}

fn font_version() -> FontVersion {
    iced::advanced::graphics::text::font_system()
        .read()
        .map_or(FontVersion::default(), |fs| fs.version())
}

impl<Message> Widget<Message, Theme, iced::Renderer> for Clamp<'_> {
    fn size(&self) -> Size<Length> {
        Size::new(Length::Fill, Length::Shrink)
    }

    fn tag(&self) -> tree::Tag {
        tree::Tag::of::<State>()
    }

    fn state(&self) -> tree::State {
        tree::State::new(State::default())
    }

    fn layout(
        &mut self,
        tree: &mut widget::Tree,
        renderer: &iced::Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        let state = tree.state.downcast_mut::<State>();
        let (size, font) = (self.size, self.font);
        let key = (
            hash_str(&self.content),
            size.to_bits(),
            font,
            font_version(),
        );
        let line = line_height().to_absolute(Pixels(size)).0;
        let max_width = limits.max().width;
        let width = if max_width.is_finite() {
            max_width.max(1.0)
        } else {
            WRAP_HEIGHT
        };
        let width_key = (width.to_bits(), self.lines);
        if state.key != Some(key) {
            let (mut text, capped) = normalize(&self.content);
            if capped {
                text.push('…');
            }
            state.full = shape(&text, size, font);
            state.graphemes = text.grapheme_indices(true).map(|(i, _)| i).collect();
            state.text = text;
            state.dots = shape("…", size, font);
            state.dots_width = state.dots.min_width();
            state.key = Some(key);
            state.width_key = None;
        }
        if state.width_key != Some(width_key) {
            state.full.resize(Size::new(width, WRAP_HEIGHT));
            let info = line_info(
                &state.full,
                &state.text,
                &state.graphemes,
                self.lines - 1,
                width - state.dots_width,
            );
            let (shown, rest) = split_at_line(&state.text, info.total, info.start, self.lines);
            state.shown = shown;
            state.last = match (rest, info.ltr_cut) {
                (None, _) => Last::None,
                (Some(_), Some(x)) => Last::Cut(x),
                (Some(at), None) => {
                    let tail = ellipsis(tail_text(&state.text, at, info.rtl))
                        .size(size)
                        .font(font);
                    let tree = match std::mem::take(&mut state.last) {
                        Last::Tail(old) => old.1,
                        _ => Tree::new(&tail as &dyn Widget<(), Theme, iced::Renderer>),
                    };
                    Last::Tail(Box::new((tail, tree)))
                }
            };
            state.width_key = Some(width_key);
        }
        let mut children = Vec::new();
        let mut height = line * state.shown as f32;
        match &mut state.last {
            Last::None => {}
            Last::Cut(_) => height += line,
            Last::Tail(tail) => {
                let (tail, tail_tree) = &mut **tail;
                let tail_limits = layout::Limits::new(Size::ZERO, Size::new(width, line));
                let node = Widget::<(), Theme, iced::Renderer>::layout(
                    tail,
                    tail_tree,
                    renderer,
                    &tail_limits,
                )
                .move_to(iced::Point::new(0.0, height));
                height += line;
                children.push(node);
            }
        }
        let size = limits.resolve(Length::Fill, Length::Shrink, Size::new(width, height));
        layout::Node::with_children(size, children)
    }

    fn draw(
        &self,
        tree: &widget::Tree,
        renderer: &mut iced::Renderer,
        theme: &Theme,
        style: &renderer::Style,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
    ) {
        let state = tree.state.downcast_ref::<State>();
        let bounds = layout.bounds();
        let color = self
            .color
            .as_ref()
            .and_then(|f| f(theme))
            .unwrap_or(style.text_color);
        let line = line_height().to_absolute(Pixels(self.size)).0;
        let full = Rectangle {
            height: line * state.shown as f32,
            ..bounds
        };
        if state.shown > 0
            && let Some(clip) = full.intersection(viewport)
        {
            renderer.fill_paragraph(&state.full, bounds.position(), color, clip);
        }
        let last_y = bounds.y + full.height;
        match &state.last {
            Last::None => {}
            Last::Cut(x) => {
                let line_clip = Rectangle {
                    y: last_y,
                    width: *x,
                    height: line,
                    ..bounds
                };
                if let Some(clip) = line_clip.intersection(viewport) {
                    renderer.fill_paragraph(&state.full, bounds.position(), color, clip);
                }
                if let Some(clip) = bounds.intersection(viewport) {
                    let at = iced::Point::new(bounds.x + x, last_y);
                    renderer.fill_paragraph(&state.dots, at, color, clip);
                }
            }
            Last::Tail(tail) => {
                if let Some(child) = layout.children().next() {
                    let (tail, tail_tree) = &**tail;
                    let style = renderer::Style { text_color: color };
                    Widget::<(), Theme, iced::Renderer>::draw(
                        tail, tail_tree, renderer, theme, &style, child, cursor, viewport,
                    );
                }
            }
        }
    }

    fn operate(
        &mut self,
        _tree: &mut widget::Tree,
        layout: Layout<'_>,
        _renderer: &iced::Renderer,
        operation: &mut dyn Operation,
    ) {
        operation.text(None, layout.bounds(), &self.content);
    }
}

impl<'a, Message: 'a> From<Clamp<'a>> for Element<'a, Message> {
    fn from(c: Clamp<'a>) -> Self {
        Element::new(c)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[test]
    fn fitting_text_is_drawn_whole() {
        assert_eq!(split_at_line("short", 1, None, 2), (1, None));
        assert_eq!(split_at_line("two lines", 2, Some(4), 2), (2, None));
        assert_eq!(split_at_line("", 0, None, 2), (1, None));
    }

    #[test]
    fn longer_text_keeps_first_lines_and_cuts_the_rest() {
        let text = "one two three four";
        assert_eq!(split_at_line(text, 4, Some(4), 2), (1, Some(4)));
        assert_eq!(split_at_line(text, 4, Some(0), 1), (0, Some(0)));
        // A start inside a grapheme cluster moves back to its beginning.
        let thai = "กี่ข้อ";
        let inside = "กี่".len() + "ข".len();
        let (_, at) = split_at_line(thai, 3, Some(inside), 2);
        assert!(thai.is_char_boundary(at.unwrap()));
        assert_eq!(at, Some("กี่".len()));
        // Out-of-range offsets are clamped.
        assert_eq!(split_at_line("ab", 3, Some(99), 2), (1, Some(2)));
    }

    fn load_fonts() {
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(|| {
            let mut fonts = iced::advanced::graphics::text::font_system()
                .write()
                .unwrap();
            for f in crate::fonts() {
                fonts.load_font(f);
            }
        });
    }

    /// The cut of the first line of `text` wrapped to `width`, checked
    /// against the glyphs: within `width - dots`, at a grapheme boundary
    /// after a non-space, and the longest such run.
    fn check_cut(text: &str, width: f32) {
        load_fonts();
        let mut p = shape(text, 13.0, font_regular());
        p.resize(Size::new(width, WRAP_HEIGHT));
        let dots = shape("…", 13.0, font_regular()).min_width();
        let graphemes: Vec<usize> = text.grapheme_indices(true).map(|(i, _)| i).collect();
        let limit = width - dots;
        let run = p.buffer().layout_runs().next().unwrap();
        assert!(!run.rtl);
        let cut = ltr_cut(run.glyphs, text, &graphemes, limit).unwrap();
        assert!(cut <= limit + 0.01, "{text:.12}: {cut} > {limit}");
        // Right edge of each grapheme of the line, in order.
        let mut edges: Vec<(usize, f32)> = Vec::new();
        for g in run.glyphs {
            let at = floor_in(&graphemes, g.start);
            match edges.last_mut() {
                Some((start, edge)) if *start == at => *edge = edge.max(g.x + g.w),
                _ => edges.push((at, g.x + g.w)),
            }
        }
        let is_space = |at: usize| text[at..].chars().next().is_some_and(char::is_whitespace);
        // The cut ends a non-space grapheme...
        let i = edges
            .iter()
            .position(|(at, edge)| (edge - cut).abs() < 0.01 && !is_space(*at))
            .unwrap_or_else(|| panic!("{text:.12}: {cut} is no grapheme edge"));
        // ...and the next non-space grapheme does not fit.
        if let Some((_, edge)) = edges[i + 1..].iter().find(|(at, _)| !is_space(*at)) {
            assert!(*edge > limit, "{text:.12}: {edge} fits in {limit}");
        }
    }

    #[test]
    fn ltr_cut_keeps_whole_graphemes_that_fit_before_the_dots() {
        let latin = "Lorem ipsum dolor sit amet, consectetur adipiscing elit, sed do eiusmod";
        let zh = "这是一个很长的中文消息用来检查两行截断是否稳定工作".repeat(3);
        let th = "นี่คือข้อความภาษาไทยที่ยาวมากเพื่อทดสอบการตัดบรรทัด".repeat(3);
        let emoji = "👩‍💻 hello 🎉 world 👍🏽 again ".repeat(4);
        let word = "Averyveryverylongwordwithoutanyspaces".repeat(3);
        for width in [60.0, 133.0, 250.0] {
            for text in [latin, &zh, &th, &emoji, &word] {
                check_cut(text, width);
            }
        }
    }

    #[test]
    fn ltr_cut_drops_trailing_spaces() {
        load_fonts();
        let text = "ab    cd";
        let mut p = shape(text, 13.0, font_regular());
        p.resize(Size::new(1000.0, WRAP_HEIGHT));
        let graphemes: Vec<usize> = text.grapheme_indices(true).map(|(i, _)| i).collect();
        let run = p.buffer().layout_runs().next().unwrap();
        let b = run.glyphs.iter().find(|g| g.start == 1).unwrap();
        let c = run.glyphs.iter().find(|g| g.start == 6).unwrap();
        // Room up to just before "c": the cut is after "b", not after the spaces.
        let cut = ltr_cut(run.glyphs, text, &graphemes, c.x + c.w - 0.5).unwrap();
        assert!((cut - (b.x + b.w)).abs() < 0.01, "{cut}");
    }

    #[test]
    fn ltr_cut_leaves_rtl_lines_to_the_ellipsis() {
        load_fonts();
        let text = "مرحبا بالعالم هذا نص طويل";
        let p = shape(text, 13.0, font_regular());
        let graphemes: Vec<usize> = text.grapheme_indices(true).map(|(i, _)| i).collect();
        let run = p.buffer().layout_runs().next().unwrap();
        assert_eq!(ltr_cut(run.glyphs, text, &graphemes, 50.0), None);
    }

    /// The bidi tail keeps the paragraph's base direction when the last
    /// line starts in the other script.
    #[test]
    fn bidi_tail_keeps_the_paragraph_direction() {
        load_fonts();
        let base_rtl = |text: &str| {
            let p = shape(text, 13.0, font_regular());
            p.buffer().layout_runs().next().unwrap().rtl
        };
        for (text, second, rtl) in [
            (
                "Release notes for the team: مرحبا بالعالم and some more English text here",
                "مرحبا",
                false,
            ),
            (
                "مرحبا بالعالم هذا نص طويل Release notes for the team and more",
                "Release",
                true,
            ),
        ] {
            let at = text.find(second).unwrap();
            // Wrap so that line 2 starts at (or within) `second`.
            let mut p = shape(text, 13.0, font_regular());
            let first = shape(text[..at].trim_end(), 13.0, font_regular()).min_width();
            p.resize(Size::new(first + 1.0, WRAP_HEIGHT));
            let graphemes: Vec<usize> = text.grapheme_indices(true).map(|(i, _)| i).collect();
            let info = line_info(&p, text, &graphemes, 1, first);
            assert_eq!(info.rtl, rtl, "{text:.12}");
            // Line 2 starts in the other script: the bare tail flips.
            let at = info.start.unwrap();
            assert_ne!(base_rtl(&text[at..]), rtl, "{text:.12}");
            let (tail, _) = normalize(&tail_text(text, at, info.rtl));
            assert_eq!(base_rtl(&tail), rtl, "{text:.12}");
        }
    }
}
