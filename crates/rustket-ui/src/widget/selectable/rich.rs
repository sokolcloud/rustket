//! The text block: renders spans exactly like `iced::widget::rich_text`
//! (fonts, colours, sizes, span backgrounds, underlines, links) and draws the
//! selection its [`Scope`](super::Scope) gives it.

use iced::advanced::layout::{self, Layout};
use iced::advanced::renderer::{self, Renderer as _};
use iced::advanced::text::{Paragraph as _, Renderer as _, Span};
use iced::advanced::widget::{Operation, Tree, Widget, tree};
use iced::advanced::{Clipboard, Shell};
use iced::alignment;
use iced::widget::text::{Alignment, LineHeight, Shaping, Wrapping};
use iced::{
    Color, Element, Event, Font, Length, Pixels, Point, Rectangle, Size, Theme, Vector, mouse,
};

use super::DRAG_THRESHOLD;
use super::core::{Core, Paragraph, normalize_breaks};
use crate::theme::tokens;

/// A selectable rich text block (see the module docs).
pub struct Rich<'a, Link, Message> {
    spans: Vec<Span<'static, Link, Font>>,
    size: Option<Pixels>,
    line_height: LineHeight,
    width: Length,
    height: Length,
    font: Option<Font>,
    color: Option<Color>,
    align_x: Alignment,
    align_y: alignment::Vertical,
    wrapping: Wrapping,
    prefix: Option<String>,
    joiner: &'static str,
    on_link_click: Option<Box<dyn Fn(Link) -> Message + 'a>>,
}

/// A selectable rich text block of `spans`. Line separators in the spans are
/// normalised to `\n`.
pub fn rich<'a, Link, Message>(spans: Vec<Span<'static, Link, Font>>) -> Rich<'a, Link, Message>
where
    Link: Clone + 'static,
{
    let spans = spans
        .into_iter()
        .map(|mut s| {
            if let std::borrow::Cow::Owned(t) = normalize_breaks(&s.text) {
                s.text = t.into();
            }
            s
        })
        .collect();
    Rich {
        spans,
        size: None,
        line_height: LineHeight::default(),
        width: Length::Shrink,
        height: Length::Shrink,
        font: None,
        color: None,
        align_x: Alignment::Default,
        align_y: alignment::Vertical::Top,
        wrapping: Wrapping::default(),
        prefix: None,
        joiner: "\n",
        on_link_click: None,
    }
}

impl<'a, Link: Clone + 'static, Message> Rich<'a, Link, Message> {
    pub fn size(mut self, size: impl Into<Pixels>) -> Self {
        self.size = Some(size.into());
        self
    }

    pub fn line_height(mut self, line_height: impl Into<LineHeight>) -> Self {
        self.line_height = line_height.into();
        self
    }

    pub fn width(mut self, width: impl Into<Length>) -> Self {
        self.width = width.into();
        self
    }

    pub fn height(mut self, height: impl Into<Length>) -> Self {
        self.height = height.into();
        self
    }

    /// Default font of spans without their own.
    pub fn font(mut self, font: Font) -> Self {
        self.font = Some(font);
        self
    }

    /// Default colour of spans without their own (else the theme's text colour).
    pub fn color(mut self, color: impl Into<Color>) -> Self {
        self.color = Some(color.into());
        self
    }

    pub fn align_x(mut self, alignment: impl Into<Alignment>) -> Self {
        self.align_x = alignment.into();
        self
    }

    pub fn align_y(mut self, alignment: impl Into<alignment::Vertical>) -> Self {
        self.align_y = alignment.into();
        self
    }

    pub fn wrapping(mut self, wrapping: Wrapping) -> Self {
        self.wrapping = wrapping;
        self
    }

    /// Text put before this block's copy when the selection includes its
    /// start (e.g. `"• "` for a list item).
    pub fn copy_prefix(mut self, prefix: impl Into<String>) -> Self {
        self.prefix = Some(prefix.into());
        self
    }

    /// Separator between the previous block's copied text and this one
    /// (default `"\n"`; `"\t"` for table cells of one row).
    pub fn joiner(mut self, joiner: &'static str) -> Self {
        self.joiner = joiner;
        self
    }

    /// Message for a clicked link (a press and release on the same link
    /// without dragging).
    pub fn on_link_click(mut self, f: impl Fn(Link) -> Message + 'a) -> Self {
        self.on_link_click = Some(Box::new(f));
        self
    }

    fn origin(&self, core: &Core, bounds: Rectangle) -> Vector {
        let min = core.paragraph.min_bounds();
        bounds.anchor(min, core.paragraph.align_x(), core.paragraph.align_y()) - bounds.position()
    }
}

/// Byte ranges of the concealed spans: text drawn in the colour of its own
/// opaque background (hidden spoilers) is not selectable, so a selection
/// neither reveals nor copies it.
fn concealed_ranges<Link>(spans: &[Span<'static, Link, Font>]) -> Vec<std::ops::Range<usize>> {
    let mut out = Vec::new();
    let mut at = 0;
    for s in spans {
        let len = s.text.len();
        let hidden = s.color.is_some_and(|c| {
            c.a > 0.0
                && s.highlight
                    .is_some_and(|h| h.background == iced::Background::Color(c))
        });
        if hidden && len > 0 {
            out.push(at..at + len);
        }
        at += len;
    }
    out
}

struct State<Link> {
    core: Core,
    spans: Vec<Span<'static, Link, Font>>,
    hovered: Option<usize>,
    /// Link span pressed and where (a drag cancels the click).
    pressed: Option<(usize, Point)>,
}

impl<Link: Clone + 'static, Message> Widget<Message, Theme, iced::Renderer>
    for Rich<'_, Link, Message>
{
    fn tag(&self) -> tree::Tag {
        tree::Tag::of::<State<Link>>()
    }

    fn state(&self) -> tree::State {
        tree::State::new(State::<Link> {
            core: Core {
                joiner: "\n",
                ..Core::default()
            },
            spans: Vec::new(),
            hovered: None,
            pressed: None,
        })
    }

    fn size(&self) -> Size<Length> {
        Size::new(self.width, self.height)
    }

    fn layout(
        &mut self,
        tree: &mut Tree,
        renderer: &iced::Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        let state = tree.state.downcast_mut::<State<Link>>();
        state.core.prefix.clone_from(&self.prefix);
        state.core.joiner = self.joiner;
        let node = layout::sized(limits, self.width, self.height, |limits| {
            let bounds = limits.max();
            let size = self.size.unwrap_or_else(|| renderer.default_size());
            let font = self.font.unwrap_or_else(|| renderer.default_font());
            let text = iced::advanced::Text {
                content: (),
                bounds,
                size,
                line_height: self.line_height,
                font,
                align_x: self.align_x,
                align_y: self.align_y,
                shaping: Shaping::Advanced,
                wrapping: self.wrapping,
            };
            if state.spans != self.spans {
                state.core.paragraph =
                    Paragraph::with_spans(text.with_content(self.spans.as_slice()));
                state.spans.clone_from(&self.spans);
                state
                    .core
                    .set_text(self.spans.iter().map(|s| s.text.as_ref()).collect());
                state.core.concealed = concealed_ranges(&self.spans);
            } else {
                match state.core.paragraph.compare(text) {
                    iced::advanced::text::Difference::None => {}
                    iced::advanced::text::Difference::Bounds => {
                        state.core.paragraph.resize(bounds);
                    }
                    iced::advanced::text::Difference::Shape => {
                        state.core.paragraph =
                            Paragraph::with_spans(text.with_content(self.spans.as_slice()));
                    }
                }
            }
            state.core.paragraph.min_bounds()
        });
        let bounds = Rectangle::new(Point::ORIGIN, node.size());
        state.core.size = node.size();
        state.core.origin = self.origin(&state.core, bounds);
        node
    }

    fn operate(
        &mut self,
        tree: &mut Tree,
        layout: Layout<'_>,
        _renderer: &iced::Renderer,
        operation: &mut dyn Operation,
    ) {
        let state = tree.state.downcast_mut::<State<Link>>();
        operation.text(None, layout.bounds(), &state.core.text);
        operation.custom(None, layout.bounds(), &mut state.core);
    }

    fn update(
        &mut self,
        tree: &mut Tree,
        event: &Event,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        _renderer: &iced::Renderer,
        _clipboard: &mut dyn Clipboard,
        shell: &mut Shell<'_, Message>,
        _viewport: &Rectangle,
    ) {
        let Event::Mouse(mouse_event) = event else {
            return;
        };
        let state = tree.state.downcast_mut::<State<Link>>();
        if matches!(mouse_event, mouse::Event::ButtonPressed(_)) {
            state.core.link_press = false;
        }
        let bounds = layout.bounds();
        let origin = bounds.position() + state.core.origin;
        // Scopes read this to hit test the block, also outside its bounds.
        state.core.cursor = cursor
            .land()
            .position()
            .map(|p| p - (origin - Point::ORIGIN));
        if self.on_link_click.is_none() {
            return;
        }
        let was = state.hovered;
        state.hovered = cursor
            .position_over(bounds)
            .and_then(|p| state.core.paragraph.hit_span(p - (origin - Point::ORIGIN)))
            .filter(|&i| self.spans.get(i).is_some_and(|s| s.link.is_some()));
        if was != state.hovered {
            shell.request_redraw();
        }
        match mouse_event {
            mouse::Event::ButtonPressed(mouse::Button::Left) => {
                state.pressed = state.hovered.zip(cursor.position());
                state.core.link_press = state.pressed.is_some();
                if state.pressed.is_some() {
                    // Not a press on what is around the text (rows that open
                    // on press); the scope still starts a selection here.
                    shell.capture_event();
                }
            }
            mouse::Event::CursorMoved { position } => {
                if state
                    .pressed
                    .is_some_and(|(_, at)| at.distance(*position) > DRAG_THRESHOLD)
                {
                    state.pressed = None;
                }
            }
            mouse::Event::ButtonReleased(mouse::Button::Left) => {
                if let Some((span, _)) = state.pressed.take()
                    && state.hovered == Some(span)
                    && let Some(link) = self.spans.get(span).and_then(|s| s.link.clone())
                    && let Some(on_click) = &self.on_link_click
                {
                    shell.publish(on_click(link));
                    shell.capture_event();
                }
            }
            _ => {}
        }
    }

    fn mouse_interaction(
        &self,
        tree: &Tree,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        _viewport: &Rectangle,
        _renderer: &iced::Renderer,
    ) -> mouse::Interaction {
        let state = tree.state.downcast_ref::<State<Link>>();
        if state.hovered.is_some() && self.on_link_click.is_some() {
            return mouse::Interaction::Pointer;
        }
        let over_text = cursor.position_over(layout.bounds()).is_some_and(|p| {
            let local = p - (layout.position() + state.core.origin - Point::ORIGIN);
            let min = state.core.paragraph.min_bounds();
            local.x >= 0.0 && local.y >= 0.0 && local.x <= min.width && local.y <= min.height
        });
        if state.core.scoped && over_text {
            mouse::Interaction::Text
        } else {
            mouse::Interaction::None
        }
    }

    fn draw(
        &self,
        tree: &Tree,
        renderer: &mut iced::Renderer,
        theme: &Theme,
        defaults: &renderer::Style,
        layout: Layout<'_>,
        _cursor: mouse::Cursor,
        viewport: &Rectangle,
    ) {
        let bounds = layout.bounds();
        if !bounds.intersects(viewport) {
            return;
        }
        let state = tree.state.downcast_ref::<State<Link>>();
        let core = &state.core;
        let translation = bounds.position() - Point::ORIGIN + core.origin;
        let text_color = self.color.unwrap_or(defaults.text_color);
        let default_size = self.size.unwrap_or_else(|| renderer.default_size());

        for (index, span) in self.spans.iter().enumerate() {
            let hovered = self.on_link_click.is_some() && state.hovered == Some(index);
            if span.highlight.is_none() && !span.underline && !span.strikethrough && !hovered {
                continue;
            }
            let regions = core.paragraph.span_bounds(index);
            if let Some(highlight) = span.highlight {
                for r in &regions {
                    let r = Rectangle::new(
                        r.position() - Vector::new(span.padding.left, span.padding.top),
                        r.size() + Size::new(span.padding.x(), span.padding.y()),
                    );
                    renderer.fill_quad(
                        renderer::Quad {
                            bounds: r + translation,
                            border: highlight.border,
                            ..renderer::Quad::default()
                        },
                        highlight.background,
                    );
                }
            }
            if span.underline || span.strikethrough || hovered {
                let size = span.size.unwrap_or(default_size);
                let line_height = span
                    .line_height
                    .unwrap_or(self.line_height)
                    .to_absolute(size);
                let color = span.color.unwrap_or(text_color);
                let baseline =
                    translation + Vector::new(0.0, size.0 + (line_height.0 - size.0) / 2.0);
                let mut line = |dy: f32| {
                    for r in &regions {
                        renderer.fill_quad(
                            renderer::Quad {
                                bounds: Rectangle::new(
                                    r.position() + baseline - Vector::new(0.0, dy),
                                    Size::new(r.width, 1.0),
                                ),
                                ..renderer::Quad::default()
                            },
                            color,
                        );
                    }
                };
                if span.underline || hovered {
                    line(size.0 * 0.08);
                }
                if span.strikethrough {
                    line(size.0 / 2.0);
                }
            }
        }

        if let Some(sel) = &core.selection {
            let color = tokens(theme).selection;
            for r in core.rects(sel) {
                renderer.fill_quad(
                    renderer::Quad {
                        bounds: r + translation,
                        ..renderer::Quad::default()
                    },
                    color,
                );
            }
        }

        renderer.fill_paragraph(
            &core.paragraph,
            bounds.position() + core.origin,
            text_color,
            *viewport,
        );
    }
}

impl<'a, Link, Message> From<Rich<'a, Link, Message>>
    for Element<'a, Message, Theme, iced::Renderer>
where
    Link: Clone + 'static,
    Message: 'a,
{
    fn from(r: Rich<'a, Link, Message>) -> Self {
        Element::new(r)
    }
}
