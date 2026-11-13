//! Drag-and-drop reordering of sections by their headers.

use std::time::{Duration, Instant};

use iced::advanced::layout::{self, Layout};
use iced::advanced::overlay;
use iced::advanced::renderer;
use iced::advanced::widget::{Operation, Tree, Widget, tree};
use iced::advanced::{Clipboard, Shell};
use iced::{
    Color, Element, Event, Length, Rectangle, Size, Theme, Vector, keyboard, mouse, window,
};

use crate::theme::{SPACE_2, radius_md, tokens};

/// A press must move this far vertically before it becomes a drag.
const DRAG_THRESHOLD: f32 = 6.0;
/// Auto-scroll starts this close to the viewport's top or bottom edge.
const EDGE: f32 = 28.0;
/// Auto-scroll distance per tick at the edge; up to twice that beyond it.
const SCROLL_STEP: f32 = 16.0;
/// Minimum time between two auto-scroll messages.
const SCROLL_TICK: Duration = Duration::from_millis(50);

/// A draggable header: `(key, top, height)`, `top` relative to the area's
/// top. `key` must be stable for the section (it survives rebuilds of the
/// content during a drag).
pub type ReorderHandle = (u64, f32, f32);

type OnReorder<'a, M> = Box<dyn Fn(u64, Option<u64>) -> M + 'a>;
type OnScroll<'a, M> = Box<dyn Fn(f32) -> M + 'a>;

/// Wraps a list whose sections start with the `handles` (in order). Dragging
/// a handle vertically publishes `on_reorder(dragged, before)` on release:
/// insert the dragged section before `before`, or at the end for `None`.
/// A press that does not move is a click for the content. An empty `handles`
/// disables dragging.
pub struct ReorderArea<'a, M> {
    content: Element<'a, M>,
    handles: Vec<ReorderHandle>,
    on_reorder: OnReorder<'a, M>,
    on_auto_scroll: Option<OnScroll<'a, M>>,
}

pub fn reorder_area<'a, M: 'a>(
    content: impl Into<Element<'a, M>>,
    handles: Vec<ReorderHandle>,
    on_reorder: impl Fn(u64, Option<u64>) -> M + 'a,
) -> ReorderArea<'a, M> {
    ReorderArea {
        content: content.into(),
        handles,
        on_reorder: Box::new(on_reorder),
        on_auto_scroll: None,
    }
}

impl<'a, M> ReorderArea<'a, M> {
    /// Near the viewport's edges a drag publishes `f(dy)` (throttled) for the
    /// caller to scroll the enclosing scrollable by `dy`.
    pub fn on_auto_scroll(mut self, f: impl Fn(f32) -> M + 'a) -> Self {
        self.on_auto_scroll = Some(Box::new(f));
        self
    }
}

impl<'a, M: 'a> From<ReorderArea<'a, M>> for Element<'a, M> {
    fn from(area: ReorderArea<'a, M>) -> Self {
        Element::new(area)
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq)]
enum Drag {
    #[default]
    Idle,
    Pressed {
        key: u64,
        origin_y: f32,
    },
    /// `y`: the last known cursor y (content coordinates).
    Dragging {
        key: u64,
        y: f32,
        origin_y: f32,
    },
}

#[derive(Default)]
struct State {
    drag: Drag,
    /// Time of the last auto-scroll message.
    last_scroll: Option<Instant>,
    /// Viewport y and direction of the last auto-scroll message while the
    /// cursor stays in an edge zone.
    scrolled: Option<(f32, bool)>,
    /// Auto-scroll is on once, during this drag, the cursor has been outside
    /// the edge zones or has moved past the press point towards an edge.
    armed: bool,
}

/// Number of handles whose centre is above `y` (relative to the area): the
/// insertion slot.
fn slot_at(handles: &[ReorderHandle], y: f32) -> usize {
    handles
        .iter()
        .filter(|(_, top, h)| top + h / 2.0 < y)
        .count()
}

/// `Some(before)` when dropping `key` at `slot` moves it.
fn drop_target(handles: &[ReorderHandle], key: u64, slot: usize) -> Option<Option<u64>> {
    let from = handles.iter().position(|(k, ..)| *k == key)?;
    (slot != from && slot != from + 1).then(|| handles.get(slot).map(|(k, ..)| *k))
}

/// Auto-scroll distance for a cursor at `y` in `viewport` (0: not at an edge).
fn edge_scroll(y: f32, viewport: &Rectangle) -> f32 {
    let edge = EDGE.min(viewport.height / 4.0).max(1.0);
    let (top, bottom) = (viewport.y + edge, viewport.y + viewport.height - edge);
    let speed = |depth: f32| SCROLL_STEP * (depth / edge).clamp(0.25, 2.0);
    if y < top {
        -speed(top - y)
    } else if y > bottom {
        speed(y - bottom)
    } else {
        0.0
    }
}

fn release_left() -> Event {
    Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left))
}

impl<M> ReorderArea<'_, M> {
    #[allow(clippy::too_many_arguments)]
    fn delegate(
        &mut self,
        tree: &mut Tree,
        event: &Event,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        renderer: &iced::Renderer,
        clipboard: &mut dyn Clipboard,
        shell: &mut Shell<'_, M>,
        viewport: &Rectangle,
    ) {
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
    }

    /// Publishes a throttled auto-scroll for the cursor at `y` (pressed at
    /// `origin_y`).
    fn auto_scroll(
        &self,
        state: &mut State,
        y: f32,
        origin_y: f32,
        now: Instant,
        shell: &mut Shell<'_, M>,
        viewport: &Rectangle,
    ) {
        let Some(on_scroll) = &self.on_auto_scroll else {
            return;
        };
        let dy = edge_scroll(y, viewport);
        let down = dy > 0.0;
        // A drag that starts in an edge zone does not scroll until the
        // cursor heads for that edge.
        state.armed |= dy == 0.0 || (down && y > origin_y) || (!down && y < origin_y);
        if dy == 0.0 {
            state.scrolled = None;
            return;
        }
        if !state.armed {
            return;
        }
        if let Some(at) = state.last_scroll
            && now < at + SCROLL_TICK
        {
            shell.request_redraw_at(at + SCROLL_TICK);
            return;
        }
        // The last scroll did not move the list: it is at its end.
        if let Some((vy, dir)) = state.scrolled
            && dir == down
            && (vy - viewport.y).abs() < 0.5
        {
            return;
        }
        shell.publish(on_scroll(dy));
        state.last_scroll = Some(now);
        state.scrolled = Some((viewport.y, down));
        shell.request_redraw_at(now + SCROLL_TICK);
    }
}

impl<M> Widget<M, Theme, iced::Renderer> for ReorderArea<'_, M> {
    fn size(&self) -> Size<Length> {
        self.content.as_widget().size()
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
        self.content
            .as_widget_mut()
            .operate(&mut tree.children[0], layout, renderer, operation);
    }

    fn update(
        &mut self,
        tree: &mut Tree,
        event: &Event,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        renderer: &iced::Renderer,
        clipboard: &mut dyn Clipboard,
        shell: &mut Shell<'_, M>,
        viewport: &Rectangle,
    ) {
        let bounds = layout.bounds();
        // Content coordinates, also while the cursor is outside an enclosing
        // scrollable (it then passes a levitating, translated cursor).
        let pos = cursor.land().position();
        let drag = tree.state.downcast_ref::<State>().drag;
        match drag {
            Drag::Idle => {
                if let Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)) = event
                    && let Some(p) = cursor.position_over(bounds)
                {
                    let y = p.y - bounds.y;
                    if let Some((key, ..)) = self
                        .handles
                        .iter()
                        .find(|(_, top, h)| y >= *top && y < top + h)
                    {
                        tree.state.downcast_mut::<State>().drag = Drag::Pressed {
                            key: *key,
                            origin_y: p.y,
                        };
                    }
                }
                self.delegate(
                    tree, event, layout, cursor, renderer, clipboard, shell, viewport,
                );
            }
            Drag::Pressed { key, origin_y } => {
                let state = tree.state.downcast_mut::<State>();
                match event {
                    Event::Mouse(mouse::Event::CursorMoved { .. })
                        if pos.is_some_and(|p| (p.y - origin_y).abs() > DRAG_THRESHOLD) =>
                    {
                        let y = pos.map_or(origin_y, |p| p.y);
                        state.drag = Drag::Dragging { key, y, origin_y };
                        state.last_scroll = None;
                        state.scrolled = None;
                        state.armed = false;
                        self.auto_scroll(state, y, origin_y, Instant::now(), shell, viewport);
                        shell.capture_event();
                        shell.request_redraw();
                        // Hovered widgets see the cursor leave.
                        self.delegate(
                            tree,
                            event,
                            layout,
                            mouse::Cursor::Unavailable,
                            renderer,
                            clipboard,
                            shell,
                            viewport,
                        );
                        return;
                    }
                    Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left))
                    | Event::Mouse(mouse::Event::ButtonPressed(_))
                    | Event::Window(window::Event::Unfocused) => state.drag = Drag::Idle,
                    _ => {}
                }
                self.delegate(
                    tree, event, layout, cursor, renderer, clipboard, shell, viewport,
                );
            }
            Drag::Dragging { key, y, origin_y } => {
                let y = pos.map_or(y, |p| p.y);
                let rel = |y: f32| y - bounds.y;
                let old_slot = {
                    let state = tree.state.downcast_mut::<State>();
                    let old = match state.drag {
                        Drag::Dragging { y, .. } => slot_at(&self.handles, rel(y)),
                        _ => 0,
                    };
                    state.drag = Drag::Dragging { key, y, origin_y };
                    old
                };
                let end = match event {
                    Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)) => {
                        // Dropped beside the list (or outside the window): cancelled.
                        let inside =
                            pos.is_some_and(|p| p.x >= bounds.x && p.x <= bounds.x + bounds.width);
                        let target =
                            drop_target(&self.handles, key, slot_at(&self.handles, rel(y)))
                                .filter(|_| inside);
                        if let Some(before) = target {
                            shell.publish((self.on_reorder)(key, before));
                        }
                        Some(event.clone())
                    }
                    Event::Mouse(mouse::Event::ButtonPressed(_))
                    | Event::Keyboard(keyboard::Event::KeyPressed {
                        key: keyboard::Key::Named(keyboard::key::Named::Escape),
                        ..
                    })
                    | Event::Window(window::Event::Unfocused) => Some(release_left()),
                    _ => None,
                };
                if let Some(release) = end {
                    tree.state.downcast_mut::<State>().drag = Drag::Idle;
                    if !matches!(event, Event::Window(_)) {
                        shell.capture_event();
                    }
                    shell.request_redraw();
                    // The pressed header resets without a click.
                    self.delegate(
                        tree,
                        &release,
                        layout,
                        mouse::Cursor::Unavailable,
                        renderer,
                        clipboard,
                        shell,
                        viewport,
                    );
                    return;
                }
                if slot_at(&self.handles, rel(y)) != old_slot {
                    shell.request_redraw();
                }
                let now = match event {
                    Event::Window(window::Event::RedrawRequested(now)) => Some(*now),
                    Event::Mouse(mouse::Event::CursorMoved { .. }) => Some(Instant::now()),
                    _ => None,
                };
                if let Some(now) = now {
                    let state = tree.state.downcast_mut::<State>();
                    self.auto_scroll(state, y, origin_y, now, shell, viewport);
                }
                // The wheel still scrolls the enclosing scrollable.
                let wheel = matches!(event, Event::Mouse(mouse::Event::WheelScrolled { .. }));
                self.delegate(
                    tree,
                    event,
                    layout,
                    mouse::Cursor::Unavailable,
                    renderer,
                    clipboard,
                    shell,
                    viewport,
                );
                if matches!(event, Event::Mouse(_)) && !wheel {
                    shell.capture_event();
                }
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
        use iced::advanced::Renderer as _;
        let state = tree.state.downcast_ref::<State>();
        let Drag::Dragging { key, y, .. } = state.drag else {
            self.content.as_widget().draw(
                &tree.children[0],
                renderer,
                theme,
                style,
                layout,
                cursor,
                viewport,
            );
            return;
        };
        // No hover styles under the dragged header.
        self.content.as_widget().draw(
            &tree.children[0],
            renderer,
            theme,
            style,
            layout,
            mouse::Cursor::Unavailable,
            viewport,
        );
        let t = tokens(theme);
        let bounds = layout.bounds();
        let (x, width) = (bounds.x + SPACE_2, (bounds.width - 2.0 * SPACE_2).max(0.0));
        let quad = |renderer: &mut iced::Renderer, top: f32, height: f32, color: Color| {
            renderer.fill_quad(
                renderer::Quad {
                    bounds: Rectangle {
                        x,
                        y: top,
                        width,
                        height,
                    },
                    border: iced::border::rounded(radius_md()),
                    ..renderer::Quad::default()
                },
                color,
            );
        };
        if let Some((_, top, h)) = self.handles.iter().find(|(k, ..)| *k == key) {
            quad(
                renderer,
                bounds.y + top,
                *h,
                Color {
                    a: 0.16,
                    ..t.primary
                },
            );
        }
        let slot = slot_at(&self.handles, y - bounds.y);
        if drop_target(&self.handles, key, slot).is_none() {
            return;
        }
        let line_y = self
            .handles
            .get(slot)
            .map_or(bounds.y + bounds.height, |(_, top, _)| bounds.y + top);
        // The section the dragged one lands after.
        if let Some(prev) = slot.checked_sub(1).and_then(|i| self.handles.get(i)) {
            let top = bounds.y + prev.1;
            quad(
                renderer,
                top,
                line_y - top,
                Color {
                    a: 0.06,
                    ..t.primary
                },
            );
        }
        // The line stays in view when the slot's edge is scrolled away.
        let lo = viewport.y.max(bounds.y);
        let hi = (viewport.y + viewport.height).min(bounds.y + bounds.height) - 2.0;
        let line_y = if hi >= lo {
            (line_y - 1.0).clamp(lo, hi)
        } else {
            line_y - 1.0
        };
        renderer.fill_quad(
            renderer::Quad {
                bounds: Rectangle {
                    x,
                    y: line_y,
                    width,
                    height: 2.0,
                },
                border: iced::border::rounded(1.0),
                ..renderer::Quad::default()
            },
            t.primary,
        );
    }

    fn mouse_interaction(
        &self,
        tree: &Tree,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
        renderer: &iced::Renderer,
    ) -> mouse::Interaction {
        if matches!(
            tree.state.downcast_ref::<State>().drag,
            Drag::Dragging { .. }
        ) {
            return mouse::Interaction::Grabbing;
        }
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
    ) -> Option<overlay::Element<'b, M, Theme, iced::Renderer>> {
        self.content.as_widget_mut().overlay(
            &mut tree.children[0],
            layout,
            renderer,
            viewport,
            translation,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const H: [ReorderHandle; 3] = [(1, 0.0, 20.0), (2, 100.0, 20.0), (3, 200.0, 20.0)];

    #[test]
    fn slots_and_targets() {
        assert_eq!(slot_at(&H, 5.0), 0);
        assert_eq!(slot_at(&H, 15.0), 1);
        assert_eq!(slot_at(&H, 150.0), 2);
        assert_eq!(slot_at(&H, 500.0), 3);
        // Own slots are no-ops.
        assert_eq!(drop_target(&H, 2, 1), None);
        assert_eq!(drop_target(&H, 2, 2), None);
        assert_eq!(drop_target(&H, 2, 0), Some(Some(1)));
        assert_eq!(drop_target(&H, 1, 3), Some(None));
        assert_eq!(drop_target(&H, 9, 0), None);
    }

    #[test]
    fn edge_scroll_speeds() {
        let vp = Rectangle::new(iced::Point::new(0.0, 100.0), Size::new(200.0, 400.0));
        assert_eq!(edge_scroll(300.0, &vp), 0.0);
        assert!(edge_scroll(110.0, &vp) < 0.0);
        assert!(edge_scroll(490.0, &vp) > 0.0);
        assert_eq!(edge_scroll(-1000.0, &vp), -2.0 * SCROLL_STEP);
    }
}
