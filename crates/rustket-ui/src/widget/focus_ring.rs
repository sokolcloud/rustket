//! Keyboard focus for widgets iced 0.14 cannot focus (buttons, list rows):
//! the wrapper takes part in focus operations (Tab / `focus_cycle`, F6
//! regions), draws a focus ring while focused and activates on Enter/Space.
//! A mouse press anywhere drops the focus, so the ring only shows for
//! keyboard use.

use iced::advanced::layout::{self, Layout};
use iced::advanced::overlay;
use iced::advanced::renderer;
use iced::advanced::widget::{Id, Operation, Tree, operation, tree};
use iced::advanced::{Clipboard, Shell, Widget};
use iced::{Border, Color, Element, Event, Length, Rectangle, Size, Theme, Vector};
use iced::{keyboard, mouse, touch};

use crate::theme::{RADIUS_SM, Tokens, tokens};

/// Width of the outer focus ring.
pub const FOCUS_RING_WIDTH: f32 = 2.0;

/// Makes `content` keyboard focusable. `on_activate` is published on
/// Enter/Space while focused; without it the wrapper is not focusable (a
/// disabled button). The tree shape is the same either way.
pub fn focus_ring<'a, Message: Clone + 'a>(
    content: impl Into<Element<'a, Message>>,
    on_activate: Option<Message>,
) -> FocusRing<'a, Message> {
    FocusRing {
        content: content.into(),
        on_activate,
        radius: RADIUS_SM,
        id: None,
        within: false,
    }
}

/// Draws the focus ring around `content` while something inside it has the
/// keyboard focus (e.g. a borderless text field in a styled box). Not
/// focusable itself.
pub fn focus_within<'a, Message: Clone + 'a>(
    content: impl Into<Element<'a, Message>>,
) -> FocusRing<'a, Message> {
    FocusRing {
        within: true,
        ..focus_ring(content, None)
    }
}

pub struct FocusRing<'a, Message> {
    content: Element<'a, Message>,
    on_activate: Option<Message>,
    radius: f32,
    id: Option<Id>,
    /// [`focus_within`].
    within: bool,
}

impl<Message> FocusRing<'_, Message> {
    /// Corner radius of the ring (match the wrapped widget).
    pub fn radius(mut self, radius: f32) -> Self {
        self.radius = radius;
        self
    }

    /// Widget id, for `iced::widget::operation::focus`.
    pub fn id(mut self, id: impl Into<Id>) -> Self {
        self.id = Some(id.into());
        self
    }
}

#[derive(Debug, Default)]
struct State {
    focused: bool,
    /// [`focus_within`]: something inside has the focus.
    inner: bool,
}

impl operation::Focusable for State {
    fn is_focused(&self) -> bool {
        self.focused
    }

    fn focus(&mut self) {
        self.focused = true;
    }

    fn unfocus(&mut self) {
        self.focused = false;
    }
}

/// Colours of the ring: the focus colour outside and a surface-coloured line
/// inside, so the ring stays visible on filled (primary, selected) widgets.
pub fn ring_colors(t: &Tokens) -> (Color, Color) {
    (t.border_focus, t.focus_ring_inner)
}

/// Draws the focus ring inside `bounds`.
pub fn draw_ring(renderer: &mut iced::Renderer, theme: &Theme, bounds: Rectangle, radius: f32) {
    use iced::advanced::Renderer as _;
    let (outer, inner) = ring_colors(tokens(theme));
    renderer.with_layer(bounds, |r| {
        r.fill_quad(
            renderer::Quad {
                bounds,
                border: Border {
                    color: outer,
                    width: FOCUS_RING_WIDTH,
                    radius: radius.into(),
                },
                ..renderer::Quad::default()
            },
            Color::TRANSPARENT,
        );
        let inset = FOCUS_RING_WIDTH;
        if bounds.width > inset * 4.0 && bounds.height > inset * 4.0 {
            r.fill_quad(
                renderer::Quad {
                    bounds: bounds.shrink(inset),
                    border: Border {
                        color: inner,
                        width: 1.0,
                        radius: (radius - inset).max(0.0).into(),
                    },
                    ..renderer::Quad::default()
                },
                Color::TRANSPARENT,
            );
        }
    });
}

impl<Message: Clone> Widget<Message, Theme, iced::Renderer> for FocusRing<'_, Message> {
    fn size(&self) -> Size<Length> {
        self.content.as_widget().size()
    }

    fn size_hint(&self) -> Size<Length> {
        self.content.as_widget().size_hint()
    }

    fn tag(&self) -> tree::Tag {
        tree::Tag::of::<State>()
    }

    fn state(&self) -> tree::State {
        tree::State::new(State::default())
    }

    fn children(&self) -> Vec<Tree> {
        vec![Tree::new(&self.content)]
    }

    fn diff(&self, tree: &mut Tree) {
        tree.diff_children(std::slice::from_ref(&self.content));
    }

    fn layout(
        &mut self,
        tree: &mut Tree,
        renderer: &iced::Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        self.content
            .as_widget_mut()
            .layout(&mut tree.children[0], renderer, limits)
    }

    fn operate(
        &mut self,
        tree: &mut Tree,
        layout: Layout<'_>,
        renderer: &iced::Renderer,
        operation: &mut dyn Operation,
    ) {
        let state = tree.state.downcast_mut::<State>();
        if self.on_activate.is_some() {
            operation.focusable(self.id.as_ref(), layout.bounds(), state);
        } else {
            state.focused = false;
        }
        operation.traverse(&mut |operation| {
            self.content.as_widget_mut().operate(
                &mut tree.children[0],
                layout,
                renderer,
                operation,
            );
        });
    }

    fn update(
        &mut self,
        tree: &mut Tree,
        event: &Event,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        renderer: &iced::Renderer,
        clipboard: &mut dyn Clipboard,
        shell: &mut Shell<'_, Message>,
        viewport: &Rectangle,
    ) {
        let state = tree.state.downcast_mut::<State>();
        match event {
            Event::Mouse(mouse::Event::ButtonPressed(_))
            | Event::Touch(touch::Event::FingerPressed { .. })
                if state.focused =>
            {
                state.focused = false;
                shell.request_redraw();
            }
            Event::Keyboard(keyboard::Event::KeyPressed {
                key: keyboard::Key::Named(keyboard::key::Named::Enter | keyboard::key::Named::Space),
                modifiers,
                repeat: false,
                ..
            }) if state.focused && !modifiers.command() && !modifiers.alt() => {
                if let Some(msg) = self.on_activate.clone() {
                    shell.publish(msg);
                    shell.capture_event();
                    return;
                }
            }
            _ => {}
        }
        self.content.as_widget_mut().update(
            &mut tree.children[0],
            event,
            layout,
            cursor,
            renderer,
            clipboard,
            shell,
            viewport,
        );
        if self.within {
            // Focus changes come from events and widget operations; both
            // are followed by a redraw event, seen here before drawing.
            let inner =
                super::layer::has_focus(&mut self.content, &mut tree.children[0], layout, renderer);
            let state = tree.state.downcast_mut::<State>();
            if state.inner != inner {
                state.inner = inner;
                shell.request_redraw();
            }
        }
    }

    fn draw(
        &self,
        tree: &Tree,
        renderer: &mut iced::Renderer,
        theme: &Theme,
        style: &renderer::Style,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
    ) {
        self.content.as_widget().draw(
            &tree.children[0],
            renderer,
            theme,
            style,
            layout,
            cursor,
            viewport,
        );
        let state = tree.state.downcast_ref::<State>();
        if (state.focused && self.on_activate.is_some()) || (self.within && state.inner) {
            draw_ring(renderer, theme, layout.bounds(), self.radius);
        }
    }

    fn mouse_interaction(
        &self,
        tree: &Tree,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
        renderer: &iced::Renderer,
    ) -> mouse::Interaction {
        self.content.as_widget().mouse_interaction(
            &tree.children[0],
            layout,
            cursor,
            viewport,
            renderer,
        )
    }

    fn overlay<'b>(
        &'b mut self,
        tree: &'b mut Tree,
        layout: Layout<'b>,
        renderer: &iced::Renderer,
        viewport: &Rectangle,
        translation: Vector,
    ) -> Option<overlay::Element<'b, Message, Theme, iced::Renderer>> {
        self.content.as_widget_mut().overlay(
            &mut tree.children[0],
            layout,
            renderer,
            viewport,
            translation,
        )
    }
}

impl<'a, Message: Clone + 'a> From<FocusRing<'a, Message>> for Element<'a, Message> {
    fn from(f: FocusRing<'a, Message>) -> Self {
        Element::new(f)
    }
}

/// A part of the window that F6 / Shift+F6 move between: a container with
/// `id`; `prefer` is focused when it is inside, else (with `last_visible`)
/// the last line of focusables on screen, else the first focusable.
#[derive(Debug, Clone)]
pub struct FocusRegion {
    pub id: Id,
    pub prefer: Option<Id>,
    pub last_visible: bool,
}

impl FocusRegion {
    pub fn new(id: impl Into<Id>) -> Self {
        Self {
            id: id.into(),
            prefer: None,
            last_visible: false,
        }
    }

    pub fn prefer(mut self, id: impl Into<Id>) -> Self {
        self.prefer = Some(id.into());
        self
    }

    /// Targets the first focusable of the last line visible in its
    /// scrollable (a timeline anchored at the newest message), not the
    /// first one built.
    pub fn last_visible(mut self) -> Self {
        self.last_visible = true;
        self
    }
}

/// A scrollable around the current point of a traversal.
#[derive(Debug, Clone, Copy)]
pub(crate) struct ScrollFrame {
    /// Position among all scrollables of the traversal.
    ordinal: usize,
    bounds: Rectangle,
    content: Rectangle,
    translation: Vector,
    /// Visible area, in the content's (untranslated) coordinates.
    clip: Rectangle,
}

/// Follows the scrollables of a traversal: an operation reports
/// `scrollable`, then `enter`/`leave` around each `traverse`.
#[derive(Debug, Default, Clone)]
pub(crate) struct ScrollTrack {
    pending: Option<ScrollFrame>,
    stack: Vec<ScrollFrame>,
    seen: usize,
}

impl ScrollTrack {
    pub(crate) fn scrollable(
        &mut self,
        bounds: Rectangle,
        content: Rectangle,
        translation: Vector,
    ) {
        let visible = match self.stack.last() {
            Some(outer) => outer.clip.intersection(&bounds).unwrap_or(Rectangle {
                width: 0.0,
                height: 0.0,
                ..bounds
            }),
            None => bounds,
        };
        self.pending = Some(ScrollFrame {
            ordinal: self.seen,
            bounds,
            content,
            translation,
            clip: visible + translation,
        });
        self.seen += 1;
    }

    /// Another call came between `scrollable` and `traverse`.
    pub(crate) fn interrupt(&mut self) {
        self.pending = None;
    }

    pub(crate) fn enter(&mut self) -> bool {
        match self.pending.take() {
            Some(frame) => {
                self.stack.push(frame);
                true
            }
            None => false,
        }
    }

    pub(crate) fn leave(&mut self, entered: bool) {
        if entered {
            self.stack.pop();
        }
    }

    /// Whether `bounds` shows in the enclosing scrollables.
    pub(crate) fn is_visible(&self, bounds: Rectangle) -> bool {
        self.stack.last().is_none_or(|f| {
            f.clip
                .intersection(&bounds)
                .is_some_and(|r| r.width >= 1.0 && r.height >= 1.0)
        })
    }

    /// The scroll that brings `bounds` into view in the innermost enclosing
    /// scrollable, if it is not visible yet.
    pub(crate) fn reveal(&self, bounds: Rectangle) -> Option<ScrollFix> {
        let f = self.stack.last()?;
        let target = reveal_translation(f.bounds, f.content, f.translation.y, bounds)?;
        Some(ScrollFix {
            ordinal: f.ordinal,
            target,
            max: (f.content.height - f.bounds.height).max(0.0),
            verify: false,
            seen: 0,
        })
    }
}

/// Spacing kept around a focusable scrolled into view.
const REVEAL_MARGIN: f32 = 8.0;

/// The vertical translation of a scrollable (`viewport`, `content`, current
/// `translation`) that shows `target`; `None` when it is fully visible.
fn reveal_translation(
    viewport: Rectangle,
    content: Rectangle,
    translation: f32,
    target: Rectangle,
) -> Option<f32> {
    let top = viewport.y + translation;
    let bottom = top + viewport.height;
    let below = target.y + target.height;
    if target.y >= top && below <= bottom {
        return None;
    }
    let to_top = target.y - viewport.y - REVEAL_MARGIN;
    let t = if target.height + 2.0 * REVEAL_MARGIN > viewport.height {
        if target.y < bottom && below > top {
            return None;
        }
        to_top
    } else if target.y < top {
        to_top
    } else {
        below - viewport.y - viewport.height + REVEAL_MARGIN
    };
    let max = (content.height - viewport.height).max(0.0);
    Some(t.clamp(0.0, max).round())
}

/// Scrolls the `ordinal`-th scrollable to show a focused widget. The
/// operation does not know whether the scrollable is anchored at the start
/// or the end, so it sets the offset for the start, checks the resulting
/// translation in a second pass and mirrors it for an end anchor.
#[derive(Debug, Clone, Copy)]
pub(crate) struct ScrollFix {
    ordinal: usize,
    target: f32,
    max: f32,
    verify: bool,
    seen: usize,
}

impl<T> Operation<T> for ScrollFix {
    fn traverse(&mut self, operate: &mut dyn FnMut(&mut dyn Operation<T>)) {
        operate(self);
    }

    fn scrollable(
        &mut self,
        _id: Option<&Id>,
        _bounds: Rectangle,
        _content: Rectangle,
        translation: Vector,
        state: &mut dyn operation::Scrollable,
    ) {
        if self.seen == self.ordinal {
            use iced::widget::scrollable::AbsoluteOffset;
            if !self.verify {
                state.scroll_to(AbsoluteOffset {
                    x: None,
                    y: Some(self.target),
                });
            } else if (translation.y - self.target).abs() > 0.5 {
                state.scroll_to(AbsoluteOffset {
                    x: None,
                    y: Some((self.max - self.target).max(0.0)),
                });
            }
        }
        self.seen += 1;
    }

    fn finish(&self) -> operation::Outcome<T> {
        if self.verify {
            operation::Outcome::None
        } else {
            operation::Outcome::Chain(Box::new(ScrollFix {
                verify: true,
                seen: 0,
                ..*self
            }))
        }
    }
}

/// What a region holds, found by [`RegionScan`].
#[derive(Debug, Default, Clone, Copy)]
struct Found {
    count: usize,
    prefer: Option<usize>,
    last_visible: Option<usize>,
    last_visible_y: Option<f32>,
}

impl Found {
    /// Index (among the region's focusables) of the one F6 focuses.
    fn target(&self, region: &FocusRegion) -> usize {
        self.prefer
            .or(if region.last_visible {
                self.last_visible.or(self.count.checked_sub(1))
            } else {
                None
            })
            .unwrap_or(0)
    }
}

/// Which focusables each region holds and which region has the focus.
#[derive(Debug, Default, Clone)]
struct RegionScan {
    regions: Vec<FocusRegion>,
    /// Container id announced right before its `traverse`.
    pending: Option<Id>,
    /// Indices of the regions the traversal is inside.
    stack: Vec<usize>,
    found: Vec<Found>,
    focused: Option<usize>,
    forward: bool,
    scroll: ScrollTrack,
}

impl RegionScan {
    fn current(&self) -> Option<usize> {
        self.stack.last().copied()
    }
}

impl<T: Send + 'static> Operation<T> for RegionScan {
    fn traverse(&mut self, operate: &mut dyn FnMut(&mut dyn Operation<T>)) {
        let entered = self
            .pending
            .take()
            .and_then(|id| self.regions.iter().position(|r| r.id == id));
        if let Some(i) = entered {
            self.stack.push(i);
        }
        let scrolled = self.scroll.enter();
        operate(self);
        self.scroll.leave(scrolled);
        if entered.is_some() {
            self.stack.pop();
        }
    }

    fn container(&mut self, id: Option<&Id>, _bounds: Rectangle) {
        self.pending = id.cloned();
        self.scroll.interrupt();
    }

    fn focusable(
        &mut self,
        id: Option<&Id>,
        bounds: Rectangle,
        state: &mut dyn operation::Focusable,
    ) {
        self.pending = None;
        self.scroll.interrupt();
        let Some(i) = self.current() else {
            return;
        };
        let visible = self.scroll.is_visible(bounds);
        let found = &mut self.found[i];
        if id.is_some() && id == self.regions[i].prefer.as_ref() && found.prefer.is_none() {
            found.prefer = Some(found.count);
        }
        if visible {
            // The first of the last line on screen (a message toolbar's
            // first button, not its "more" button).
            let same_line = found
                .last_visible_y
                .is_some_and(|y| (y - bounds.y).abs() < 1.0);
            if !same_line {
                found.last_visible = Some(found.count);
            }
            found.last_visible_y = Some(bounds.y);
        }
        found.count += 1;
        if state.is_focused() {
            self.focused = Some(i);
        }
    }

    fn scrollable(
        &mut self,
        _id: Option<&Id>,
        bounds: Rectangle,
        content: Rectangle,
        translation: Vector,
        _state: &mut dyn operation::Scrollable,
    ) {
        self.pending = None;
        self.scroll.scrollable(bounds, content, translation);
    }

    fn finish(&self) -> operation::Outcome<T> {
        let has: Vec<bool> = self.found.iter().map(|f| f.count > 0).collect();
        let Some(target) = next_region(&has, self.focused, self.forward) else {
            return operation::Outcome::None;
        };
        let region = self.regions[target].clone();
        operation::Outcome::Chain(Box::new(FocusIn {
            index: self.found[target].target(&region),
            region,
            current: 0,
            pending: None,
            depth_inside: 0,
            inside: false,
            scroll: ScrollTrack::default(),
            reveal: None,
        }))
    }
}

/// Target index for [`focus_region`]: the next region (in `step` direction)
/// after `current` that holds a focusable.
fn next_region(has: &[bool], current: Option<usize>, forward: bool) -> Option<usize> {
    let n = has.len();
    if n == 0 {
        return None;
    }
    let start = match (current, forward) {
        (Some(c), _) => c,
        (None, true) => n - 1,
        (None, false) => 0,
    };
    (1..=n)
        .map(|k| {
            if forward {
                (start + k) % n
            } else {
                (start + n - k % n) % n
            }
        })
        .find(|&i| has[i])
}

/// Focuses the `index`-th focusable of region `target`, unfocuses
/// everything else and scrolls the focused one into view.
struct FocusIn {
    region: FocusRegion,
    index: usize,
    /// Focusables of the region seen so far.
    current: usize,
    pending: Option<Id>,
    depth_inside: usize,
    inside: bool,
    scroll: ScrollTrack,
    reveal: Option<ScrollFix>,
}

impl<T> Operation<T> for FocusIn {
    fn traverse(&mut self, operate: &mut dyn FnMut(&mut dyn Operation<T>)) {
        let entering = !self.inside && self.pending.take().is_some_and(|id| id == self.region.id);
        if entering {
            self.inside = true;
        }
        if self.inside {
            self.depth_inside += 1;
        }
        let scrolled = self.scroll.enter();
        operate(self);
        self.scroll.leave(scrolled);
        if self.inside {
            self.depth_inside -= 1;
            if self.depth_inside == 0 {
                self.inside = false;
            }
        }
    }

    fn container(&mut self, id: Option<&Id>, _bounds: Rectangle) {
        self.pending = id.cloned();
        self.scroll.interrupt();
    }

    fn focusable(
        &mut self,
        _id: Option<&Id>,
        bounds: Rectangle,
        state: &mut dyn operation::Focusable,
    ) {
        self.pending = None;
        self.scroll.interrupt();
        if self.inside {
            let wanted = self.current == self.index;
            self.current += 1;
            if wanted {
                state.focus();
                self.reveal = self.scroll.reveal(bounds);
                return;
            }
        }
        state.unfocus();
    }

    fn scrollable(
        &mut self,
        _id: Option<&Id>,
        bounds: Rectangle,
        content: Rectangle,
        translation: Vector,
        _state: &mut dyn operation::Scrollable,
    ) {
        self.pending = None;
        self.scroll.scrollable(bounds, content, translation);
    }

    fn finish(&self) -> operation::Outcome<T> {
        match self.reveal {
            Some(fix) => operation::Outcome::Chain(Box::new(fix)),
            None => operation::Outcome::None,
        }
    }
}

/// F6 / Shift+F6: moves the keyboard focus to the next / previous region
/// (wrapping) that holds something focusable.
pub fn focus_region<T: Send + 'static>(regions: Vec<FocusRegion>, forward: bool) -> iced::Task<T> {
    iced::advanced::widget::operate(RegionScan {
        found: vec![Found::default(); regions.len()],
        regions,
        forward,
        ..RegionScan::default()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn regions_wrap_and_skip_empty_ones() {
        let found = [true, false, true];
        assert_eq!(next_region(&found, None, true), Some(0));
        assert_eq!(next_region(&found, None, false), Some(2));
        assert_eq!(next_region(&found, Some(0), true), Some(2));
        assert_eq!(next_region(&found, Some(2), true), Some(0));
        assert_eq!(next_region(&found, Some(0), false), Some(2));
        assert_eq!(next_region(&found, Some(2), false), Some(0));
        assert_eq!(next_region(&[false], None, true), None);
        assert_eq!(next_region(&[], Some(0), true), None);
    }

    #[test]
    fn reveal_scrolls_only_what_is_hidden() {
        let viewport = Rectangle::new(iced::Point::new(0.0, 100.0), Size::new(200.0, 300.0));
        let content = Rectangle::new(iced::Point::new(0.0, 100.0), Size::new(200.0, 1000.0));
        let at = |y: f32| Rectangle::new(iced::Point::new(0.0, y), Size::new(200.0, 30.0));
        // Visible: content y 100..400 at translation 0.
        assert_eq!(reveal_translation(viewport, content, 0.0, at(150.0)), None);
        // Below: its bottom lands at the viewport bottom (plus margin).
        assert_eq!(
            reveal_translation(viewport, content, 0.0, at(420.0)),
            Some(450.0 - 400.0 + REVEAL_MARGIN)
        );
        // Above: its top lands at the viewport top (minus margin).
        assert_eq!(
            reveal_translation(viewport, content, 500.0, at(300.0)),
            Some(200.0 - REVEAL_MARGIN)
        );
        // Clamped to the content.
        assert_eq!(
            reveal_translation(viewport, content, 500.0, at(102.0)),
            Some(0.0)
        );
        assert_eq!(
            reveal_translation(viewport, content, 0.0, at(1066.0)),
            Some(700.0)
        );
    }
}
