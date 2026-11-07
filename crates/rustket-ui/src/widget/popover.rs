//! Anchored popover: shows an element next to a trigger while `open` is true.
//!
//! Unlike `iced_aw::DropDown`, the popup is aligned to the trigger's edges
//! (below/above, start/end) and flips to stay inside the window.

use iced::advanced::layout::{self, Layout};
use iced::advanced::overlay;
use iced::advanced::renderer;
use iced::advanced::widget::{self, Operation, Tree, Widget};
use iced::advanced::{Clipboard, Shell};
use iced::{Element, Event, Length, Point, Rectangle, Size, Theme, Vector, keyboard, mouse, touch};

/// Where the popup goes relative to the trigger.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Placement {
    /// Below the trigger, left edges aligned.
    #[default]
    BelowStart,
    /// Below the trigger, right edges aligned.
    BelowEnd,
    /// Above the trigger, left edges aligned (e.g. composer autocomplete).
    AboveStart,
    /// Above the trigger, right edges aligned.
    AboveEnd,
    /// To the right of the trigger, top edges aligned.
    RightStart,
}

/// Wraps `trigger`; while `open`, shows `popup` anchored to it. Clicking outside both
/// or pressing Escape publishes `on_dismiss`.
pub fn popover<'a, Message: Clone + 'a>(
    trigger: impl Into<Element<'a, Message>>,
    popup: impl Into<Element<'a, Message>>,
    open: bool,
    placement: Placement,
    on_dismiss: Message,
) -> Element<'a, Message> {
    Element::new(Popover {
        trigger: trigger.into(),
        popup: popup.into(),
        open,
        placement,
        gap: 4.0,
        on_dismiss,
    })
}

struct Popover<'a, Message> {
    trigger: Element<'a, Message>,
    popup: Element<'a, Message>,
    open: bool,
    placement: Placement,
    gap: f32,
    on_dismiss: Message,
}

impl<'a, Message: Clone + 'a> Widget<Message, Theme, iced::Renderer> for Popover<'a, Message> {
    fn size(&self) -> Size<Length> {
        self.trigger.as_widget().size()
    }

    fn size_hint(&self) -> Size<Length> {
        self.trigger.as_widget().size_hint()
    }

    fn children(&self) -> Vec<Tree> {
        vec![Tree::new(&self.trigger), Tree::new(&self.popup)]
    }

    fn diff(&self, tree: &mut Tree) {
        tree.diff_children(&[&self.trigger, &self.popup]);
    }

    fn layout(
        &mut self,
        tree: &mut Tree,
        renderer: &iced::Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        self.trigger
            .as_widget_mut()
            .layout(&mut tree.children[0], renderer, limits)
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
        self.trigger.as_widget().draw(
            &tree.children[0],
            renderer,
            theme,
            style,
            layout,
            cursor,
            viewport,
        );
    }

    fn operate(
        &mut self,
        tree: &mut Tree,
        layout: Layout<'_>,
        renderer: &iced::Renderer,
        operation: &mut dyn Operation,
    ) {
        self.trigger
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
        shell: &mut Shell<'_, Message>,
        viewport: &Rectangle,
    ) {
        self.trigger.as_widget_mut().update(
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

    fn mouse_interaction(
        &self,
        tree: &Tree,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
        renderer: &iced::Renderer,
    ) -> mouse::Interaction {
        self.trigger.as_widget().mouse_interaction(
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
        let (trigger_tree, rest) = tree.children.split_at_mut(1);
        if !self.open {
            return self.trigger.as_widget_mut().overlay(
                &mut trigger_tree[0],
                layout,
                renderer,
                viewport,
                translation,
            );
        }
        let anchor = layout.bounds() + translation;
        Some(overlay::Element::new(Box::new(PopupOverlay {
            tree: &mut rest[0],
            popup: &mut self.popup,
            anchor,
            placement: self.placement,
            gap: self.gap,
            on_dismiss: &self.on_dismiss,
        })))
    }
}

struct PopupOverlay<'a, 'b, Message> {
    tree: &'b mut Tree,
    popup: &'b mut Element<'a, Message>,
    anchor: Rectangle,
    placement: Placement,
    gap: f32,
    on_dismiss: &'b Message,
}

/// Computes the popup origin for `size` next to `anchor`, flipping/clamping so the
/// popup stays within `window`.
pub(crate) fn place(
    anchor: Rectangle,
    size: Size,
    window: Size,
    placement: Placement,
    gap: f32,
) -> Point {
    let below = anchor.y + anchor.height + gap;
    let above = anchor.y - size.height - gap;
    let (mut x, mut y) = match placement {
        Placement::BelowStart => (anchor.x, below),
        Placement::BelowEnd => (anchor.x + anchor.width - size.width, below),
        Placement::AboveStart => (anchor.x, above),
        Placement::AboveEnd => (anchor.x + anchor.width - size.width, above),
        Placement::RightStart => (anchor.x + anchor.width + gap, anchor.y),
    };
    // Flip vertically when there is no room.
    match placement {
        Placement::BelowStart | Placement::BelowEnd
            if y + size.height > window.height && above >= 0.0 =>
        {
            y = above
        }
        Placement::AboveStart | Placement::AboveEnd
            if y < 0.0 && below + size.height <= window.height =>
        {
            y = below
        }
        _ => {}
    }
    // Flip horizontally for side placement.
    if placement == Placement::RightStart && x + size.width > window.width {
        x = anchor.x - size.width - gap;
    }
    x = x.min(window.width - size.width).max(0.0);
    y = y.min(window.height - size.height).max(0.0);
    Point::new(x, y)
}

impl<Message: Clone> overlay::Overlay<Message, Theme, iced::Renderer>
    for PopupOverlay<'_, '_, Message>
{
    fn layout(&mut self, renderer: &iced::Renderer, bounds: Size) -> layout::Node {
        let limits = layout::Limits::new(Size::ZERO, bounds);
        let node = self
            .popup
            .as_widget_mut()
            .layout(self.tree, renderer, &limits);
        let origin = place(self.anchor, node.size(), bounds, self.placement, self.gap);
        node.move_to(origin)
    }

    fn draw(
        &self,
        renderer: &mut iced::Renderer,
        theme: &Theme,
        style: &renderer::Style,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
    ) {
        let bounds = layout.bounds();
        self.popup
            .as_widget()
            .draw(self.tree, renderer, theme, style, layout, cursor, &bounds);
    }

    fn operate(
        &mut self,
        layout: Layout<'_>,
        renderer: &iced::Renderer,
        operation: &mut dyn widget::Operation,
    ) {
        self.popup
            .as_widget_mut()
            .operate(self.tree, layout, renderer, operation);
    }

    fn update(
        &mut self,
        event: &Event,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        renderer: &iced::Renderer,
        clipboard: &mut dyn Clipboard,
        shell: &mut Shell<'_, Message>,
    ) {
        match event {
            Event::Keyboard(keyboard::Event::KeyPressed {
                key: keyboard::Key::Named(keyboard::key::Named::Escape),
                ..
            }) => {
                shell.publish(self.on_dismiss.clone());
                shell.capture_event();
                return;
            }
            Event::Mouse(mouse::Event::ButtonPressed(_))
            | Event::Touch(touch::Event::FingerPressed { .. })
                if !cursor.is_over(layout.bounds()) && !cursor.is_over(self.anchor) =>
            {
                shell.publish(self.on_dismiss.clone());
            }
            _ => {}
        }
        let bounds = layout.bounds();
        self.popup.as_widget_mut().update(
            self.tree, event, layout, cursor, renderer, clipboard, shell, &bounds,
        );
        // Nothing under the popup (headers, separators, disabled items) may react
        // to presses or the wheel; releases and moves pass so drags can finish.
        if matches!(
            event,
            Event::Mouse(mouse::Event::ButtonPressed(_) | mouse::Event::WheelScrolled { .. })
                | Event::Touch(touch::Event::FingerPressed { .. })
        ) && cursor.is_over(bounds)
        {
            shell.capture_event();
        }
    }

    fn mouse_interaction(
        &self,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        renderer: &iced::Renderer,
    ) -> mouse::Interaction {
        let bounds = layout.bounds();
        let interaction = self
            .popup
            .as_widget()
            .mouse_interaction(self.tree, layout, cursor, &bounds, renderer);
        if cursor.is_over(bounds) {
            interaction.max(mouse::Interaction::Idle)
        } else {
            interaction
        }
    }

    fn overlay<'c>(
        &'c mut self,
        layout: Layout<'c>,
        renderer: &iced::Renderer,
    ) -> Option<overlay::Element<'c, Message, Theme, iced::Renderer>> {
        let bounds = layout.bounds();
        self.popup
            .as_widget_mut()
            .overlay(self.tree, layout, renderer, &bounds, Vector::ZERO)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const WINDOW: Size = Size::new(800.0, 600.0);

    fn anchor(x: f32, y: f32) -> Rectangle {
        Rectangle::new(Point::new(x, y), Size::new(32.0, 32.0))
    }

    #[test]
    fn below_start_aligns_left_edges() {
        let p = place(
            anchor(100.0, 50.0),
            Size::new(200.0, 100.0),
            WINDOW,
            Placement::BelowStart,
            4.0,
        );
        assert_eq!(p, Point::new(100.0, 86.0));
    }

    #[test]
    fn below_end_aligns_right_edges() {
        let p = place(
            anchor(300.0, 50.0),
            Size::new(200.0, 100.0),
            WINDOW,
            Placement::BelowEnd,
            4.0,
        );
        assert_eq!(p, Point::new(132.0, 86.0));
    }

    #[test]
    fn flips_up_near_bottom_and_clamps_right() {
        let p = place(
            anchor(700.0, 550.0),
            Size::new(200.0, 100.0),
            WINDOW,
            Placement::BelowStart,
            4.0,
        );
        assert_eq!(p, Point::new(600.0, 446.0));
    }

    #[test]
    fn above_flips_down_near_top() {
        let p = place(
            anchor(10.0, 10.0),
            Size::new(200.0, 100.0),
            WINDOW,
            Placement::AboveStart,
            4.0,
        );
        assert_eq!(p, Point::new(10.0, 46.0));
    }

    #[test]
    fn side_flips_left() {
        let p = place(
            anchor(700.0, 10.0),
            Size::new(200.0, 100.0),
            WINDOW,
            Placement::RightStart,
            4.0,
        );
        assert_eq!(p, Point::new(496.0, 10.0));
    }
}
