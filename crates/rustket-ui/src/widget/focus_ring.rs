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
/// `id`; `prefer` is focused when it is inside, else the first focusable.
#[derive(Debug, Clone)]
pub struct FocusRegion {
    pub id: Id,
    pub prefer: Option<Id>,
}

impl FocusRegion {
    pub fn new(id: impl Into<Id>) -> Self {
        Self {
            id: id.into(),
            prefer: None,
        }
    }

    pub fn prefer(mut self, id: impl Into<Id>) -> Self {
        self.prefer = Some(id.into());
        self
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
    /// Per region: (has a focusable, has the preferred one).
    found: Vec<(bool, bool)>,
    focused: Option<usize>,
    forward: bool,
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
        operate(self);
        if entered.is_some() {
            self.stack.pop();
        }
    }

    fn container(&mut self, id: Option<&Id>, _bounds: Rectangle) {
        self.pending = id.cloned();
    }

    fn focusable(
        &mut self,
        id: Option<&Id>,
        _bounds: Rectangle,
        state: &mut dyn operation::Focusable,
    ) {
        self.pending = None;
        let Some(i) = self.current() else {
            return;
        };
        self.found[i].0 = true;
        if id.is_some() && id == self.regions[i].prefer.as_ref() {
            self.found[i].1 = true;
        }
        if state.is_focused() {
            self.focused = Some(i);
        }
    }

    fn scrollable(
        &mut self,
        _id: Option<&Id>,
        _bounds: Rectangle,
        _content: Rectangle,
        _translation: Vector,
        _state: &mut dyn operation::Scrollable,
    ) {
        self.pending = None;
    }

    fn finish(&self) -> operation::Outcome<T> {
        let Some(target) = next_region(&self.found, self.focused, self.forward) else {
            return operation::Outcome::None;
        };
        operation::Outcome::Chain(Box::new(FocusIn {
            region: self.regions[target].clone(),
            use_prefer: self.found[target].1,
            pending: None,
            depth_inside: 0,
            inside: false,
            done: false,
        }))
    }
}

/// Target index for [`focus_region`]: the next region (in `step` direction)
/// after `current` that holds a focusable.
fn next_region(found: &[(bool, bool)], current: Option<usize>, forward: bool) -> Option<usize> {
    let n = found.len();
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
        .find(|&i| found[i].0)
}

/// Focuses the preferred (or first) focusable of region `target` and
/// unfocuses everything else.
struct FocusIn {
    region: FocusRegion,
    use_prefer: bool,
    pending: Option<Id>,
    depth_inside: usize,
    inside: bool,
    done: bool,
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
        operate(self);
        if self.inside {
            self.depth_inside -= 1;
            if self.depth_inside == 0 {
                self.inside = false;
            }
        }
    }

    fn container(&mut self, id: Option<&Id>, _bounds: Rectangle) {
        self.pending = id.cloned();
    }

    fn focusable(
        &mut self,
        id: Option<&Id>,
        _bounds: Rectangle,
        state: &mut dyn operation::Focusable,
    ) {
        self.pending = None;
        let wanted = self.inside
            && !self.done
            && (!self.use_prefer || (id.is_some() && id == self.region.prefer.as_ref()));
        if wanted {
            state.focus();
            self.done = true;
        } else {
            state.unfocus();
        }
    }

    fn scrollable(
        &mut self,
        _id: Option<&Id>,
        _bounds: Rectangle,
        _content: Rectangle,
        _translation: Vector,
        _state: &mut dyn operation::Scrollable,
    ) {
        self.pending = None;
    }
}

/// F6 / Shift+F6: moves the keyboard focus to the next / previous region
/// (wrapping) that holds something focusable.
pub fn focus_region<T: Send + 'static>(regions: Vec<FocusRegion>, forward: bool) -> iced::Task<T> {
    iced::advanced::widget::operate(RegionScan {
        found: vec![(false, false); regions.len()],
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
        let found = [(true, false), (false, false), (true, true)];
        assert_eq!(next_region(&found, None, true), Some(0));
        assert_eq!(next_region(&found, None, false), Some(2));
        assert_eq!(next_region(&found, Some(0), true), Some(2));
        assert_eq!(next_region(&found, Some(2), true), Some(0));
        assert_eq!(next_region(&found, Some(0), false), Some(2));
        assert_eq!(next_region(&found, Some(2), false), Some(0));
        assert_eq!(next_region(&[(false, false)], None, true), None);
        assert_eq!(next_region(&[], Some(0), true), None);
    }
}
