//! Single-line text that truncates with "…" to the available width.

use std::borrow::Cow;
use std::hash::{DefaultHasher, Hash, Hasher};

use iced::advanced::layout::{self, Layout};
use iced::advanced::renderer;
use iced::advanced::text::{self as adv_text, Paragraph as _, Renderer as _};
use iced::advanced::widget::{self, Operation, Widget, tree};
use iced::alignment;
use iced::widget::text::{LineHeight, Shaping, Wrapping};
use iced::{Color, Element, Font, Length, Pixels, Rectangle, Size, Theme, mouse};
use unicode_segmentation::UnicodeSegmentation;

use crate::emoji::owned_spans;
use crate::theme::{DEFAULT_FONT, TEXT_BODY};

type FontVersion = iced::advanced::graphics::text::Version;
type Paragraph = <iced::Renderer as adv_text::Renderer>::Paragraph;
type ColorFn<'a> = Box<dyn Fn(&Theme) -> Option<Color> + 'a>;

/// At most this many graphemes are shaped; longer content always ends with "…".
pub const MAX_GRAPHEMES: usize = 256;

/// Single-line label that ends with "…" when it does not fit. Whitespace runs
/// (including newlines) collapse to one space, emoji use the colour emoji font.
///
/// The text is shaped once per (content, size, font); width changes only move the
/// cut point (no reshaping) for left-to-right text.
pub struct Ellipsis<'a> {
    content: Cow<'a, str>,
    size: f32,
    font: Font,
    width: Length,
    color: Option<ColorFn<'a>>,
}

/// Creates a truncating single-line label (fills the available width by default).
pub fn ellipsis<'a>(content: impl Into<Cow<'a, str>>) -> Ellipsis<'a> {
    Ellipsis {
        content: content.into(),
        size: TEXT_BODY,
        font: DEFAULT_FONT,
        width: Length::Fill,
        color: None,
    }
}

impl<'a> Ellipsis<'a> {
    pub fn size(mut self, size: f32) -> Self {
        self.size = size;
        self
    }

    pub fn font(mut self, font: Font) -> Self {
        self.font = font;
        self
    }

    /// `Fill` (default) takes the available width; `Shrink` takes only what the
    /// (possibly truncated) text needs.
    pub fn width(mut self, width: impl Into<Length>) -> Self {
        self.width = width.into();
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

const ELLIPSIS: &str = "…";

fn line_height() -> LineHeight {
    LineHeight::Relative(1.3)
}

/// Bidi embedding/override/isolate controls (U+202A–U+202E, U+2066–U+2069).
fn is_bidi_control(c: char) -> bool {
    matches!(c, '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}')
}

/// Removes bidi embedding/override/isolate controls, which can make a display name
/// or preview render reversed or spoof another text ("\u{202E}gnp.exe").
/// [`ellipsis`] applies it automatically; use it for other single-line labels.
pub fn sanitize_bidi(text: &str) -> Cow<'_, str> {
    if text.chars().any(is_bidi_control) {
        Cow::Owned(text.chars().filter(|c| !is_bidi_control(*c)).collect())
    } else {
        Cow::Borrowed(text)
    }
}

/// Collapses whitespace runs to single spaces, trims, drops bidi controls and caps
/// the grapheme count. Returns the normalized text and whether it was cut.
pub(crate) fn normalize(content: &str) -> (String, bool) {
    let content = sanitize_bidi(content);
    let mut out = String::with_capacity(content.len().min(1024));
    let mut count = 0;
    let mut pending_space = false;
    for g in content.graphemes(true) {
        if g.chars().all(char::is_whitespace) {
            pending_space = !out.is_empty();
            continue;
        }
        if count + usize::from(pending_space) + 1 > MAX_GRAPHEMES {
            return (out, true);
        }
        if pending_space {
            out.push(' ');
            count += 1;
            pending_space = false;
        }
        out.push_str(g);
        count += 1;
    }
    (out, false)
}

/// Shapes `text` on one line with finite bounds (infinite bounds put right-to-left
/// glyphs at x = +inf, which panics in the software renderer).
fn shape(text: &str, size: f32, font: Font) -> Paragraph {
    let spans = owned_spans::<()>(text);
    let height = line_height().to_absolute(Pixels(size)).0;
    let mut paragraph = Paragraph::with_spans(adv_text::Text {
        content: spans.as_slice(),
        bounds: Size::new(100_000.0, height * 2.0),
        size: Pixels(size),
        line_height: line_height(),
        font,
        align_x: adv_text::Alignment::Left,
        align_y: alignment::Vertical::Top,
        shaping: Shaping::Advanced,
        wrapping: Wrapping::None,
    });
    let width = paragraph.min_width().ceil().max(1.0);
    paragraph.resize(Size::new(width, height * 2.0));
    paragraph
}

/// Logical layout of shaped text: grapheme start offsets, cumulative advance of
/// the first `n` graphemes (from the shaped glyph clusters, so it is independent
/// of visual order), and whether visual order equals logical order.
struct Metrics {
    boundaries: Vec<usize>,
    prefix: Vec<f32>,
    is_space: Vec<bool>,
    visual_is_logical: bool,
}

fn metrics(paragraph: &Paragraph, text: &str) -> Metrics {
    let mut boundaries: Vec<usize> = Vec::new();
    let mut is_space = Vec::new();
    for (i, g) in text.grapheme_indices(true) {
        boundaries.push(i);
        is_space.push(g.chars().all(char::is_whitespace));
    }
    let mut widths = vec![0.0f32; boundaries.len()];
    let mut visual_is_logical = true;
    let mut last_start = 0;
    for run in paragraph.buffer().layout_runs() {
        if run.rtl {
            visual_is_logical = false;
        }
        for glyph in run.glyphs {
            if glyph.level.is_rtl() || glyph.start < last_start {
                visual_is_logical = false;
            }
            last_start = glyph.start;
            let index = boundaries
                .partition_point(|b| *b <= glyph.start)
                .saturating_sub(1);
            if let Some(w) = widths.get_mut(index) {
                *w += glyph.w;
            }
        }
    }
    let mut prefix = Vec::with_capacity(widths.len() + 1);
    prefix.push(0.0);
    let mut sum = 0.0;
    for w in widths {
        sum += w;
        prefix.push(sum);
    }
    boundaries.push(text.len());
    Metrics {
        boundaries,
        prefix,
        is_space,
        visual_is_logical,
    }
}

/// Number of leading graphemes whose advance fits `limit`, without trailing spaces.
pub(crate) fn fit_graphemes(prefix: &[f32], is_space: &[bool], limit: f32) -> usize {
    let mut n = prefix.partition_point(|w| *w <= limit).saturating_sub(1);
    while n > 0 && is_space.get(n - 1).copied().unwrap_or(false) {
        n -= 1;
    }
    n
}

#[derive(Default)]
struct State {
    /// (content hash, size, font, font-system version) of the shaped text.
    key: Option<(u64, u32, Font, FontVersion)>,
    text: String,
    capped: bool,
    full: Paragraph,
    full_width: f32,
    metrics: Option<Metrics>,
    dots: Paragraph,
    dots_width: f32,
    /// Exact available width the cut below was computed for.
    width_key: Option<u32>,
    /// Some(x): draw `full` clipped at x followed by "…" (visual == logical order).
    cut: Option<f32>,
    /// Shaped "prefix…" for bidi text, with the grapheme count it was built for.
    fallback: Option<(usize, Paragraph)>,
    /// Recently used bidi prefixes, so dragging back and forth does not reshape.
    spare: Vec<(usize, Paragraph)>,
}

/// How many shaped bidi prefixes are kept per label.
const SPARE_PREFIXES: usize = 4;

impl State {
    fn shown_width(&self) -> f32 {
        match (&self.fallback, self.cut) {
            (Some((_, p)), _) => p.min_width(),
            (None, Some(x)) => x + self.dots_width,
            (None, None) => self.full_width,
        }
    }
}

fn hash_str(s: &str) -> u64 {
    let mut h = DefaultHasher::new();
    s.hash(&mut h);
    h.finish()
}

/// Changes whenever fonts are loaded, so cached shaping is redone.
fn font_version() -> FontVersion {
    iced::advanced::graphics::text::font_system()
        .read()
        .map_or(FontVersion::default(), |fs| fs.version())
}

impl<Message> Widget<Message, Theme, iced::Renderer> for Ellipsis<'_> {
    fn size(&self) -> Size<Length> {
        Size::new(self.width, Length::Shrink)
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
        _renderer: &iced::Renderer,
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
        if state.key != Some(key) {
            let (text, capped) = normalize(&self.content);
            state.full = shape(&text, size, font);
            state.full_width = state.full.min_width();
            state.metrics = Some(metrics(&state.full, &text));
            state.dots = shape(ELLIPSIS, size, font);
            state.dots_width = state.dots.min_width();
            state.text = text;
            state.capped = capped;
            state.key = Some(key);
            state.width_key = None;
            state.fallback = None;
            state.spare.clear();
        }
        let max_width = limits.max().width;
        let width_key = max_width.to_bits();
        if state.width_key != Some(width_key) {
            let available = if max_width.is_finite() {
                max_width.max(0.0)
            } else {
                f32::MAX
            };
            let fits = state.full_width <= available && !state.capped;
            let previous = state.fallback.take();
            state.cut = None;
            if !fits && let Some(m) = &state.metrics {
                let graphemes = m.prefix.len() - 1;
                let n = if state.capped && state.full_width + state.dots_width <= available {
                    graphemes
                } else {
                    fit_graphemes(&m.prefix, &m.is_space, available - state.dots_width)
                };
                if m.visual_is_logical {
                    state.cut = Some(m.prefix[n]);
                } else {
                    // Bidi: reshape only when the kept grapheme count changes.
                    if let Some(old) = previous {
                        if old.0 == n {
                            state.fallback = Some(old);
                        } else {
                            state.spare.insert(0, old);
                            state.spare.truncate(SPARE_PREFIXES);
                        }
                    }
                    if state.fallback.is_none() {
                        let reused = state
                            .spare
                            .iter()
                            .position(|(kept, _)| *kept == n)
                            .map(|i| state.spare.remove(i));
                        state.fallback = Some(reused.unwrap_or_else(|| {
                            let end = m.boundaries[n];
                            let shown = format!("{}{ELLIPSIS}", state.text[..end].trim_end());
                            (n, shape(&shown, size, font))
                        }));
                    }
                }
            } else if let Some(old) = previous {
                state.spare.insert(0, old);
                state.spare.truncate(SPARE_PREFIXES);
            }
            state.width_key = Some(width_key);
        }
        let height = line_height().to_absolute(Pixels(size)).0;
        let measured = state.shown_width().min(max_width);
        let width = match self.width {
            Length::Shrink => measured,
            _ => max_width,
        };
        layout::Node::new(limits.resolve(self.width, Length::Shrink, Size::new(width, height)))
    }

    fn draw(
        &self,
        tree: &widget::Tree,
        renderer: &mut iced::Renderer,
        theme: &Theme,
        style: &renderer::Style,
        layout: Layout<'_>,
        _cursor: mouse::Cursor,
        viewport: &Rectangle,
    ) {
        let state = tree.state.downcast_ref::<State>();
        let bounds = layout.bounds();
        let Some(clip) = bounds.intersection(viewport) else {
            return;
        };
        let color = self
            .color
            .as_ref()
            .and_then(|f| f(theme))
            .unwrap_or(style.text_color);
        match (&state.fallback, state.cut) {
            (Some((_, p)), _) => renderer.fill_paragraph(p, bounds.position(), color, clip),
            (None, Some(x)) => {
                let text_clip = Rectangle {
                    width: (bounds.x + x - clip.x).clamp(0.0, clip.width),
                    ..clip
                };
                renderer.fill_paragraph(&state.full, bounds.position(), color, text_clip);
                let dots_at = iced::Point::new(bounds.x + x, bounds.y);
                renderer.fill_paragraph(&state.dots, dots_at, color, clip);
            }
            (None, None) => renderer.fill_paragraph(&state.full, bounds.position(), color, clip),
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

impl<'a, Message: 'a> From<Ellipsis<'a>> for Element<'a, Message> {
    fn from(e: Ellipsis<'a>) -> Self {
        Element::new(e)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fits_whole_graphemes_and_drops_trailing_spaces() {
        // "ab cd": widths 1 each.
        let prefix = [0.0, 1.0, 2.0, 3.0, 4.0, 5.0];
        let space = [false, false, true, false, false];
        assert_eq!(fit_graphemes(&prefix, &space, 5.0), 5);
        assert_eq!(fit_graphemes(&prefix, &space, 4.5), 4);
        assert_eq!(fit_graphemes(&prefix, &space, 3.0), 2);
        assert_eq!(fit_graphemes(&prefix, &space, 0.5), 0);
        assert_eq!(fit_graphemes(&prefix, &space, -3.0), 0);
    }

    #[test]
    fn strips_bidi_controls() {
        assert_eq!(sanitize_bidi("\u{202E}gnp.exe"), "gnp.exe");
        assert!(matches!(sanitize_bidi("plain"), Cow::Borrowed(_)));
        assert_eq!(normalize("a\u{2066}b\u{2069}").0, "ab");
    }

    #[test]
    fn normalizes_whitespace_and_caps() {
        assert_eq!(
            normalize("  Hi\n\tthere  you "),
            ("Hi there you".to_owned(), false)
        );
        let long = "ab ".repeat(200);
        let (text, capped) = normalize(&long);
        assert!(capped);
        assert_eq!(text.graphemes(true).count(), MAX_GRAPHEMES);
        assert_eq!(normalize(""), (String::new(), false));
    }
}
