//! The room column with a right panel: docked next to it while both fit,
//! narrower when the window is narrow, and over the room column only when
//! not even that fits. The room column never shrinks below
//! [`MIN_MAIN_WIDTH`] because of a docked panel (room title, composer
//! placeholder), and the panel not below [`MIN_SIDE_WIDTH`].

use iced::advanced::layout::{self, Layout};
use iced::advanced::overlay;
use iced::advanced::renderer;
use iced::advanced::widget::{Operation, Tree, Widget};
use iced::advanced::{Clipboard, Shell};
use iced::{Element, Event, Length, Point, Rectangle, Size, Theme, Vector, mouse};

/// Narrowest room column next to a docked panel.
pub const MIN_MAIN_WIDTH: f32 = 360.0;
/// Narrowest docked panel.
pub const MIN_SIDE_WIDTH: f32 = 300.0;

/// `main` with `side` on top, aligned right. `side_width`: the preferred
/// width of a docked panel (the room column gets the rest); `None` for no
/// panel or one drawn over `main` (it is laid out with the whole size).
/// Both children are always present, so their widget state survives
/// opening, closing and resizing the panel.
pub fn side_panel<'a, Message: 'a>(
    main: impl Into<Element<'a, Message>>,
    side: impl Into<Element<'a, Message>>,
    side_width: Option<f32>,
) -> Element<'a, Message> {
    Element::new(SidePanel {
        children: [main.into(), side.into()],
        side_width,
    })
}

/// (room column width, docked panel width) for `width` available.
/// `None` panel width: the panel is laid out with the whole width.
pub fn widths(width: f32, side_width: Option<f32>) -> (f32, Option<f32>) {
    let Some(pref) = side_width.filter(|w| *w > 0.0) else {
        return (width, None);
    };
    if width - pref >= MIN_MAIN_WIDTH {
        (width - pref, Some(pref))
    } else if width - MIN_MAIN_WIDTH >= MIN_SIDE_WIDTH {
        (MIN_MAIN_WIDTH, Some(width - MIN_MAIN_WIDTH))
    } else {
        // Not even both minimums fit: the panel covers the room column.
        (width, Some(width.max(0.0)))
    }
}

/// Whether the panel is drawn over part of the room column.
fn covers(layout: Layout<'_>) -> bool {
    let mut parts = layout.children();
    let (Some(main), Some(side)) = (parts.next(), parts.next()) else {
        return false;
    };
    let (main, side) = (main.bounds(), side.bounds());
    side.width >= 1.0 && side.x < main.x + main.width - 0.5
}

/// Forwards an operation, but hides the focusables (and drops a focus they
/// still hold) of the covered room column.
struct Covered<'a>(&'a mut dyn Operation);

impl Operation for Covered<'_> {
    fn traverse(&mut self, operate: &mut dyn FnMut(&mut dyn Operation)) {
        self.0.traverse(&mut |op| operate(&mut Covered(op)));
    }

    fn container(&mut self, id: Option<&iced::advanced::widget::Id>, bounds: Rectangle) {
        self.0.container(id, bounds);
    }

    fn focusable(
        &mut self,
        _id: Option<&iced::advanced::widget::Id>,
        _bounds: Rectangle,
        state: &mut dyn iced::advanced::widget::operation::Focusable,
    ) {
        if state.is_focused() {
            state.unfocus();
        }
    }

    fn scrollable(
        &mut self,
        id: Option<&iced::advanced::widget::Id>,
        bounds: Rectangle,
        content_bounds: Rectangle,
        translation: Vector,
        state: &mut dyn iced::advanced::widget::operation::Scrollable,
    ) {
        self.0
            .scrollable(id, bounds, content_bounds, translation, state);
    }

    fn text(&mut self, id: Option<&iced::advanced::widget::Id>, bounds: Rectangle, text: &str) {
        self.0.text(id, bounds, text);
    }

    fn text_input(
        &mut self,
        id: Option<&iced::advanced::widget::Id>,
        bounds: Rectangle,
        state: &mut dyn iced::advanced::widget::operation::TextInput,
    ) {
        self.0.text_input(id, bounds, state);
    }

    fn custom(
        &mut self,
        id: Option<&iced::advanced::widget::Id>,
        bounds: Rectangle,
        state: &mut dyn std::any::Any,
    ) {
        self.0.custom(id, bounds, state);
    }
}

struct SidePanel<'a, Message> {
    children: [Element<'a, Message>; 2],
    side_width: Option<f32>,
}

impl<Message> Widget<Message, Theme, iced::Renderer> for SidePanel<'_, Message> {
    fn size(&self) -> Size<Length> {
        Size::new(Length::Fill, Length::Fill)
    }

    fn children(&self) -> Vec<Tree> {
        self.children.iter().map(Tree::new).collect()
    }

    fn diff(&self, tree: &mut Tree) {
        tree.diff_children(&self.children);
    }

    fn layout(
        &mut self,
        tree: &mut Tree,
        renderer: &iced::Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        let size = limits.resolve(Length::Fill, Length::Fill, Size::ZERO);
        let (main_w, side_w) = widths(size.width, self.side_width);
        let [main, side] = &mut self.children;
        let main = main.as_widget_mut().layout(
            &mut tree.children[0],
            renderer,
            &layout::Limits::new(Size::ZERO, Size::new(main_w, size.height)),
        );
        let side_max = Size::new(side_w.unwrap_or(size.width), size.height);
        let side = side.as_widget_mut().layout(
            &mut tree.children[1],
            renderer,
            &layout::Limits::new(Size::ZERO, side_max),
        );
        let x = (size.width - side.size().width).max(0.0);
        layout::Node::with_children(size, vec![main, side.move_to(Point::new(x, 0.0))])
    }

    fn operate(
        &mut self,
        tree: &mut Tree,
        layout: Layout<'_>,
        renderer: &iced::Renderer,
        operation: &mut dyn Operation,
    ) {
        operation.container(None, layout.bounds());
        let covered = covers(layout);
        operation.traverse(&mut |operation| {
            for (i, ((child, state), layout)) in self
                .children
                .iter_mut()
                .zip(&mut tree.children)
                .zip(layout.children())
                .enumerate()
            {
                if i == 0 && covered {
                    // Keyboard focus (Tab, F6) never goes under the panel.
                    child
                        .as_widget_mut()
                        .operate(state, layout, renderer, &mut Covered(operation));
                } else {
                    child
                        .as_widget_mut()
                        .operate(state, layout, renderer, operation);
                }
            }
        });
    }

    fn update(
        &mut self,
        tree: &mut Tree,
        event: &Event,
        layout: Layout<'_>,
        mut cursor: mouse::Cursor,
        renderer: &iced::Renderer,
        clipboard: &mut dyn Clipboard,
        shell: &mut Shell<'_, Message>,
        viewport: &Rectangle,
    ) {
        // Like `stack`: the panel first; the room column does not see the
        // cursor where the panel covers it.
        let is_over = cursor.is_over(layout.bounds());
        for (i, ((child, state), layout)) in self
            .children
            .iter_mut()
            .rev()
            .zip(tree.children.iter_mut().rev())
            .zip(layout.children().rev())
            .enumerate()
        {
            child.as_widget_mut().update(
                state, event, layout, cursor, renderer, clipboard, shell, viewport,
            );
            if shell.is_event_captured() {
                // A press inside the panel is still a press outside the room
                // column: its inputs and focus rings drop the focus.
                if i == 0
                    && matches!(
                        event,
                        Event::Mouse(mouse::Event::ButtonPressed(_))
                            | Event::Touch(iced::touch::Event::FingerPressed { .. })
                    )
                    && let (Some(main), Some(main_state), Some(main_layout)) = (
                        self.children.first_mut(),
                        tree.children.first_mut(),
                        layout.children().next(),
                    )
                {
                    main.as_widget_mut().update(
                        main_state,
                        event,
                        main_layout,
                        mouse::Cursor::Unavailable,
                        renderer,
                        clipboard,
                        shell,
                        viewport,
                    );
                }
                return;
            }
            if i == 0 && is_over && !cursor.is_levitating() {
                let interaction = child
                    .as_widget()
                    .mouse_interaction(state, layout, cursor, viewport, renderer);
                if interaction != mouse::Interaction::None {
                    cursor = cursor.levitate();
                }
            }
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
        self.children
            .iter()
            .rev()
            .zip(tree.children.iter().rev())
            .zip(layout.children().rev())
            .map(|((child, state), layout)| {
                child
                    .as_widget()
                    .mouse_interaction(state, layout, cursor, viewport, renderer)
            })
            .find(|i| *i != mouse::Interaction::None)
            .unwrap_or_default()
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
        let Some(clipped) = layout.bounds().intersection(viewport) else {
            return;
        };
        let mut parts = layout.children();
        let (Some(main_l), Some(side_l)) = (parts.next(), parts.next()) else {
            return;
        };
        let side_hovered = self.children[1].as_widget().mouse_interaction(
            &tree.children[1],
            side_l,
            cursor,
            viewport,
            renderer,
        ) != mouse::Interaction::None;
        let main_cursor = if side_hovered {
            mouse::Cursor::Unavailable
        } else {
            cursor
        };
        self.children[0].as_widget().draw(
            &tree.children[0],
            renderer,
            theme,
            style,
            main_l,
            main_cursor,
            viewport,
        );
        renderer.with_layer(clipped, |renderer| {
            self.children[1].as_widget().draw(
                &tree.children[1],
                renderer,
                theme,
                style,
                side_l,
                cursor,
                viewport,
            );
        });
    }

    fn overlay<'b>(
        &'b mut self,
        tree: &'b mut Tree,
        layout: Layout<'b>,
        renderer: &iced::Renderer,
        viewport: &Rectangle,
        translation: Vector,
    ) -> Option<overlay::Element<'b, Message, Theme, iced::Renderer>> {
        overlay::from_children(
            &mut self.children,
            tree,
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

    #[test]
    fn docked_panels_leave_the_room_column_readable() {
        // Wide: the preferred panel width.
        assert_eq!(widths(1200.0, Some(440.0)), (760.0, Some(440.0)));
        assert_eq!(widths(800.0, Some(440.0)), (360.0, Some(440.0)));
        // 1024 px window (rail + sidebar ≈ 345 px): the panel shrinks.
        assert_eq!(widths(679.0, Some(440.0)), (MIN_MAIN_WIDTH, Some(319.0)));
        // Too narrow for both: the panel covers the room column.
        assert_eq!(widths(600.0, Some(440.0)), (600.0, Some(600.0)));
        // No docked panel: everything for the room column.
        assert_eq!(widths(700.0, None), (700.0, None));
    }
}
