//! Horizontally resizable sidebar + content layout.

use iced::advanced::layout::{self, Layout};
use iced::advanced::renderer;
use iced::advanced::widget::{self, Widget, tree};
use iced::advanced::{Clipboard, Shell};
use iced::widget::{container, row};
use iced::{Element, Event, Length, Rectangle, Size, Theme, mouse};

use crate::theme::tokens;

/// Width of the handle's own layout slot (the grab area); a 1px line is drawn at
/// its left edge, the rest shows the parent background.
pub const HANDLE_WIDTH: f32 = 6.0;

/// Lays out `sidebar` at `width` and `content` filling the rest, with a drag handle
/// in between. Dragging publishes `on_resize(new_width)` clamped to `min..=max`;
/// the caller stores the width (and may persist it).
pub fn split<'a, Message: 'a>(
    sidebar: impl Into<Element<'a, Message>>,
    content: impl Into<Element<'a, Message>>,
    width: f32,
    min: f32,
    max: f32,
    on_resize: impl Fn(f32) -> Message + 'a,
) -> Element<'a, Message> {
    let min = if min.is_finite() { min.max(0.0) } else { 0.0 };
    let max = if max.is_finite() {
        max.max(min)
    } else {
        min.max(10_000.0)
    };
    let width = if width.is_finite() {
        width.clamp(min, max)
    } else {
        min
    };
    row![
        container(sidebar).width(width).height(Length::Fill),
        Element::new(DragHandle {
            width,
            min,
            max,
            on_resize: Box::new(on_resize),
        }),
        container(content).width(Length::Fill).height(Length::Fill),
    ]
    .height(Length::Fill)
    .into()
}

struct DragHandle<'a, Message> {
    width: f32,
    min: f32,
    max: f32,
    on_resize: Box<dyn Fn(f32) -> Message + 'a>,
}

#[derive(Default)]
struct State {
    /// Cursor x and pane width when the drag started.
    drag: Option<(f32, f32)>,
    hovered: bool,
}

impl<Message> Widget<Message, Theme, iced::Renderer> for DragHandle<'_, Message> {
    fn size(&self) -> Size<Length> {
        Size::new(Length::Fixed(HANDLE_WIDTH), Length::Fill)
    }

    fn tag(&self) -> tree::Tag {
        tree::Tag::of::<State>()
    }

    fn state(&self) -> tree::State {
        tree::State::new(State::default())
    }

    fn layout(
        &mut self,
        _tree: &mut widget::Tree,
        _renderer: &iced::Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        layout::atomic(limits, Length::Fixed(HANDLE_WIDTH), Length::Fill)
    }

    fn draw(
        &self,
        tree: &widget::Tree,
        renderer: &mut iced::Renderer,
        theme: &Theme,
        _style: &renderer::Style,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        _viewport: &Rectangle,
    ) {
        use iced::advanced::Renderer as _;
        let state = tree.state.downcast_ref::<State>();
        let t = tokens(theme);
        let bounds = layout.bounds();
        let active = state.drag.is_some() || cursor.is_over(grab_area(bounds));
        let (color, w) = if active {
            (t.primary, 2.0)
        } else {
            (t.border, 1.0)
        };
        renderer.fill_quad(
            renderer::Quad {
                bounds: Rectangle {
                    x: bounds.x,
                    width: w,
                    ..bounds
                },
                ..renderer::Quad::default()
            },
            color,
        );
    }

    fn update(
        &mut self,
        tree: &mut widget::Tree,
        event: &Event,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        _renderer: &iced::Renderer,
        _clipboard: &mut dyn Clipboard,
        shell: &mut Shell<'_, Message>,
        _viewport: &Rectangle,
    ) {
        let state = tree.state.downcast_mut::<State>();
        let area = grab_area(layout.bounds());
        match event {
            Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)) => {
                if let Some(pos) = cursor.position_over(area) {
                    state.drag = Some((pos.x, self.width));
                    shell.capture_event();
                    shell.request_redraw();
                }
            }
            Event::Mouse(mouse::Event::CursorMoved { position }) => {
                if let Some((start_x, start_width)) = state.drag {
                    let new_width = (start_width + position.x - start_x).clamp(self.min, self.max);
                    if new_width.is_finite() && (new_width - self.width).abs() >= 0.5 {
                        shell.publish((self.on_resize)(new_width));
                    }
                    shell.capture_event();
                }
                let hovered = cursor.is_over(area);
                if hovered != state.hovered {
                    state.hovered = hovered;
                    shell.request_redraw();
                }
            }
            Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left))
                if state.drag.is_some() =>
            {
                state.drag = None;
                shell.capture_event();
                shell.request_redraw();
            }
            _ => {}
        }
    }

    fn mouse_interaction(
        &self,
        tree: &widget::Tree,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        _viewport: &Rectangle,
        _renderer: &iced::Renderer,
    ) -> mouse::Interaction {
        let state = tree.state.downcast_ref::<State>();
        if state.drag.is_some() || cursor.is_over(grab_area(layout.bounds())) {
            mouse::Interaction::ResizingHorizontally
        } else {
            mouse::Interaction::None
        }
    }
}

/// The handle's own slot: it never overlaps the panes.
fn grab_area(bounds: Rectangle) -> Rectangle {
    bounds
}
