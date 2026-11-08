//! Press tracking for keyboard shortcuts and a click area that does not fire
//! on drags.
//!
//! [`press_guard`] wraps the whole window content and counts every pointer
//! press before any widget sees it. A scope that takes a press remembers the
//! count; its Ctrl+A / Ctrl+C / Escape act only while no later press happened
//! anywhere (a toolbar button over the row, the composer, a side panel), so
//! they do not compete with a text input focused since. A shortcut key
//! (Ctrl/Cmd+…, Escape) while any widget has the keyboard focus counts as
//! such a press too, so an input focused with Tab or F6 keeps Ctrl+A/Ctrl+C.
//! Presses inside
//! overlays (menus) are not counted: a scope drops its focus itself when it
//! lets a right press open another menu. Without a guard the count stays 0
//! and scopes rely on the presses they see themselves.

use std::cell::Cell;

use iced::advanced::layout::{self, Layout};
use iced::advanced::overlay;
use iced::advanced::renderer;
use iced::advanced::widget::{Operation, Tree, Widget, tree};
use iced::advanced::{Clipboard, Shell};
use iced::{
    Element, Event, Length, Point, Rectangle, Size, Theme, Vector, keyboard, mouse, touch,
};

use super::DRAG_THRESHOLD;

thread_local! {
    static PRESSES: Cell<u64> = const { Cell::new(0) };
}

/// Pointer presses counted by [`press_guard`] so far (this thread).
pub(super) fn presses() -> u64 {
    PRESSES.with(Cell::get)
}

/// Counts pointer presses for the selection scopes in `content` (see the
/// module docs). Wrap the window content once, unconditionally.
pub fn press_guard<'a, Message: Clone + 'a>(
    content: impl Into<Element<'a, Message, Theme, iced::Renderer>>,
) -> Element<'a, Message, Theme, iced::Renderer> {
    Element::new(Forward {
        content: content.into(),
        kind: Kind::Guard,
    })
}

/// `content` that publishes `on_click` for a left click on it: a press and a
/// release over it, neither captured by a widget inside (buttons, links, a
/// selection drag), without moving more than [`DRAG_THRESHOLD`] in between.
/// Unlike `mouse_area::on_press`, starting a text selection inside does not
/// trigger it.
pub fn click_area<'a, Message: Clone + 'a>(
    content: impl Into<Element<'a, Message, Theme, iced::Renderer>>,
    on_click: Message,
) -> Element<'a, Message, Theme, iced::Renderer> {
    Element::new(Forward {
        content: content.into(),
        kind: Kind::Click(on_click),
    })
}

enum Kind<Message> {
    Guard,
    Click(Message),
}

struct Forward<'a, Message> {
    content: Element<'a, Message, Theme, iced::Renderer>,
    kind: Kind<Message>,
}

#[derive(Default)]
struct ClickState {
    pressed: Option<Point>,
}

impl<Message: Clone> Widget<Message, Theme, iced::Renderer> for Forward<'_, Message> {
    fn tag(&self) -> tree::Tag {
        tree::Tag::of::<ClickState>()
    }

    fn state(&self) -> tree::State {
        tree::State::new(ClickState::default())
    }

    fn children(&self) -> Vec<Tree> {
        vec![Tree::new(&self.content)]
    }

    fn diff(&self, tree: &mut Tree) {
        tree.diff_children(std::slice::from_ref(&self.content));
    }

    fn size(&self) -> Size<Length> {
        self.content.as_widget().size()
    }

    fn size_hint(&self) -> Size<Length> {
        self.content.as_widget().size_hint()
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
        shell: &mut Shell<'_, Message>,
        viewport: &Rectangle,
    ) {
        if matches!(self.kind, Kind::Guard)
            && matches!(
                event,
                Event::Mouse(mouse::Event::ButtonPressed(_))
                    | Event::Touch(touch::Event::FingerPressed { .. })
            )
        {
            PRESSES.with(|p| p.set(p.get().wrapping_add(1)));
        }
        // A widget focused without a press (Tab, F6, a programmatic focus)
        // owns the shortcuts too: count it like a press elsewhere.
        if let Kind::Guard = self.kind
            && let Event::Keyboard(keyboard::Event::KeyPressed { key, modifiers, .. }) = event
            && (modifiers.command() || *key == keyboard::Key::Named(keyboard::key::Named::Escape))
            && crate::widget::layer::has_focus(
                &mut self.content,
                &mut tree.children[0],
                layout,
                renderer,
            )
        {
            PRESSES.with(|p| p.set(p.get().wrapping_add(1)));
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
        let Kind::Click(on_click) = &self.kind else {
            return;
        };
        let captured = shell.is_event_captured();
        let state = tree.state.downcast_mut::<ClickState>();
        match event {
            Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)) => {
                state.pressed = cursor.position_over(layout.bounds()).filter(|_| !captured);
            }
            Event::Mouse(mouse::Event::CursorMoved { position }) => {
                if state
                    .pressed
                    .is_some_and(|at| at.distance(*position) > DRAG_THRESHOLD)
                {
                    state.pressed = None;
                }
            }
            Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)) => {
                let pressed = state.pressed.take();
                if pressed.is_some() && !captured && cursor.is_over(layout.bounds()) {
                    shell.publish(on_click.clone());
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
        viewport: &Rectangle,
        renderer: &iced::Renderer,
    ) -> mouse::Interaction {
        let inner = self.content.as_widget().mouse_interaction(
            &tree.children[0],
            layout,
            cursor,
            viewport,
            renderer,
        );
        if matches!(self.kind, Kind::Click(_))
            && inner == mouse::Interaction::None
            && cursor.is_over(layout.bounds())
        {
            mouse::Interaction::Pointer
        } else {
            inner
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
