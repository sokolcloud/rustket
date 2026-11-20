//! A picture of interactive widgets: drawn as they look at rest, but not
//! focusable, not hovered and never publishing a message.

use iced::advanced::layout::{self, Layout};
use iced::advanced::renderer;
use iced::advanced::widget::operation::Scrollable;
use iced::advanced::widget::{Id, Operation, Tree, Widget};
use iced::advanced::{Clipboard, Shell};
use iced::{Element, Event, Length, Rectangle, Size, Theme, Vector, mouse};

pub struct Inert<'a, Message> {
    content: Element<'a, Message>,
}

/// Shows `content` inert: no Tab stop or focus ring, no hover or pointer
/// cursor, and its messages are dropped (e.g. live samples of sidebar rows).
pub fn inert<'a, Message: 'a>(content: impl Into<Element<'a, Message>>) -> Inert<'a, Message> {
    Inert {
        content: content.into(),
    }
}

impl<'a, Message: 'a> From<Inert<'a, Message>> for Element<'a, Message> {
    fn from(i: Inert<'a, Message>) -> Self {
        Element::new(i)
    }
}

impl<Message> Widget<Message, Theme, iced::Renderer> for Inert<'_, Message> {
    fn size(&self) -> Size<Length> {
        self.content.as_widget().size()
    }

    fn size_hint(&self) -> Size<Length> {
        self.content.as_widget().size_hint()
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

    fn draw(
        &self,
        tree: &Tree,
        renderer: &mut iced::Renderer,
        theme: &Theme,
        style: &renderer::Style,
        layout: Layout<'_>,
        _cursor: mouse::Cursor,
        viewport: &Rectangle,
    ) {
        self.content.as_widget().draw(
            &tree.children[0],
            renderer,
            theme,
            style,
            layout,
            mouse::Cursor::Unavailable,
            viewport,
        );
    }

    /// Operations see the content (its texts, …) but none of its focusable
    /// or editable widgets: no Tab stop, nothing for the focus cycle.
    fn operate(
        &mut self,
        tree: &mut Tree,
        layout: Layout<'_>,
        renderer: &iced::Renderer,
        operation: &mut dyn Operation,
    ) {
        self.content.as_widget_mut().operate(
            &mut tree.children[0],
            layout,
            renderer,
            &mut NoFocus(operation),
        );
    }

    fn update(
        &mut self,
        tree: &mut Tree,
        event: &Event,
        layout: Layout<'_>,
        _cursor: mouse::Cursor,
        renderer: &iced::Renderer,
        clipboard: &mut dyn Clipboard,
        _shell: &mut Shell<'_, Message>,
        viewport: &Rectangle,
    ) {
        // Only redraws, without a cursor (buttons take their resting status
        // then); whatever the content publishes is dropped.
        if let Event::Window(iced::window::Event::RedrawRequested(_)) = event {
            let mut dropped = Vec::new();
            let mut shell = Shell::new(&mut dropped);
            self.content.as_widget_mut().update(
                &mut tree.children[0],
                event,
                layout,
                mouse::Cursor::Unavailable,
                renderer,
                clipboard,
                &mut shell,
                viewport,
            );
        }
    }

    fn mouse_interaction(
        &self,
        _tree: &Tree,
        _layout: Layout<'_>,
        _cursor: mouse::Cursor,
        _viewport: &Rectangle,
        _renderer: &iced::Renderer,
    ) -> mouse::Interaction {
        mouse::Interaction::None
    }
}

/// Passes an operation on, without the focusable and text-input widgets.
struct NoFocus<'a>(&'a mut dyn Operation);

impl Operation for NoFocus<'_> {
    fn traverse(&mut self, operate: &mut dyn FnMut(&mut dyn Operation)) {
        self.0.traverse(&mut |op| operate(&mut NoFocus(op)));
    }

    fn container(&mut self, id: Option<&Id>, bounds: Rectangle) {
        self.0.container(id, bounds);
    }

    fn scrollable(
        &mut self,
        id: Option<&Id>,
        bounds: Rectangle,
        content_bounds: Rectangle,
        translation: Vector,
        state: &mut dyn Scrollable,
    ) {
        self.0
            .scrollable(id, bounds, content_bounds, translation, state);
    }

    fn text(&mut self, id: Option<&Id>, bounds: Rectangle, text: &str) {
        self.0.text(id, bounds, text);
    }

    fn custom(&mut self, id: Option<&Id>, bounds: Rectangle, state: &mut dyn std::any::Any) {
        self.0.custom(id, bounds, state);
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use iced::advanced::widget::operation::Focusable;
    use iced::widget::button;
    use iced_runtime::user_interface::{Cache, UserInterface};

    /// Focusable widgets and texts.
    struct Count(usize, usize);
    impl Operation for Count {
        fn traverse(&mut self, operate: &mut dyn FnMut(&mut dyn Operation)) {
            operate(self);
        }
        fn focusable(&mut self, _: Option<&Id>, _: Rectangle, _: &mut dyn Focusable) {
            self.0 += 1;
        }
        fn text(&mut self, _: Option<&Id>, _: Rectangle, _: &str) {
            self.1 += 1;
        }
    }

    /// Not focusable, its text still seen, and a click publishes nothing.
    #[test]
    fn inert_content_is_not_focusable_and_drops_clicks() {
        let el = |inert_it: bool| -> Element<'static, u8> {
            let b = crate::widget::focus_ring(
                button(iced::widget::text("x"))
                    .width(100)
                    .height(40)
                    .on_press(1u8),
                Some(1u8),
            );
            if inert_it { inert(b).into() } else { b.into() }
        };
        let mut renderer = iced::Renderer::Secondary(iced_tiny_skia::Renderer::new(
            crate::theme::font_regular(),
            iced::Pixels(crate::theme::TEXT_BODY),
        ));
        for (inert_it, focusable, published) in [(false, 1, true), (true, 0, false)] {
            let mut ui = UserInterface::build(
                el(inert_it),
                Size::new(200.0, 100.0),
                Cache::default(),
                &mut renderer,
            );
            let mut count = Count(0, 0);
            ui.operate(&renderer, &mut count);
            assert_eq!(count.0, focusable);
            assert_eq!(count.1, 1, "the text is still seen");
            let at = iced::Point::new(10.0, 10.0);
            let cursor = mouse::Cursor::Available(at);
            let mut out = Vec::new();
            let (state, _) = ui.update(
                &[
                    Event::Mouse(mouse::Event::CursorMoved { position: at }),
                    Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)),
                    Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)),
                ],
                cursor,
                &mut renderer,
                &mut iced::advanced::clipboard::Null,
                &mut out,
            );
            assert_eq!(!out.is_empty(), published);
            let iced_runtime::user_interface::State::Updated {
                mouse_interaction, ..
            } = state
            else {
                panic!("outdated");
            };
            assert_eq!(mouse_interaction == mouse::Interaction::None, inert_it);
        }
    }
}
