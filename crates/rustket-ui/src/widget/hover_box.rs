//! Container that reacts to hover by itself, so the app does not have to track
//! which row is hovered.

use iced::advanced::layout::{self, Layout};
use iced::advanced::overlay;
use iced::advanced::renderer;
use iced::advanced::widget::{Operation, Tree, Widget, tree};
use iced::advanced::{Clipboard, Shell};
use iced::widget::container;
use iced::{Element, Event, Length, Rectangle, Size, Theme, Vector, mouse};

type StyleFn<'a> = Box<dyn Fn(&Theme, bool) -> container::Style + 'a>;

/// Draws `content` with a background chosen by `style(theme, hovered)` and, while
/// hovered, the `on_hover` layers on top (laid out over the content's bounds, e.g.
/// a toolbar aligned top-right or a timestamp in the gutter).
pub struct HoverBox<'a, Message> {
    children: Vec<Element<'a, Message>>,
    style: StyleFn<'a>,
}

pub fn hover_box<'a, Message: 'a>(
    content: impl Into<Element<'a, Message>>,
    on_hover: Vec<Element<'a, Message>>,
    style: impl Fn(&Theme, bool) -> container::Style + 'a,
) -> HoverBox<'a, Message> {
    let mut children = vec![content.into()];
    children.extend(on_hover);
    HoverBox {
        children,
        style: Box::new(style),
    }
}

#[derive(Default)]
struct State {
    hovered: bool,
}

impl<'a, Message: 'a> From<HoverBox<'a, Message>> for Element<'a, Message> {
    fn from(h: HoverBox<'a, Message>) -> Self {
        Element::new(h)
    }
}

impl<Message> Widget<Message, Theme, iced::Renderer> for HoverBox<'_, Message> {
    fn size(&self) -> Size<Length> {
        self.children[0].as_widget().size()
    }

    fn tag(&self) -> tree::Tag {
        tree::Tag::of::<State>()
    }

    fn state(&self) -> tree::State {
        tree::State::new(State::default())
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
        let content =
            self.children[0]
                .as_widget_mut()
                .layout(&mut tree.children[0], renderer, limits);
        let size = content.size();
        let item_limits = layout::Limits::new(Size::ZERO, size);
        let mut nodes = vec![content];
        for (child, tree) in self
            .children
            .iter_mut()
            .zip(tree.children.iter_mut())
            .skip(1)
        {
            nodes.push(child.as_widget_mut().layout(tree, renderer, &item_limits));
        }
        layout::Node::with_children(size, nodes)
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
        let bounds = layout.bounds();
        let Some(visible) = bounds.intersection(viewport) else {
            return;
        };
        let hovered = cursor.is_over(bounds);
        let s = (self.style)(theme, hovered);
        if s.background.is_some() || s.border.width > 0.0 {
            container::draw_background(renderer, &s, bounds);
        }
        let style = renderer::Style {
            text_color: s.text_color.unwrap_or(style.text_color),
        };
        let mut parts = self
            .children
            .iter()
            .zip(&tree.children)
            .zip(layout.children());
        if let Some(((content, tree), layout)) = parts.next() {
            content
                .as_widget()
                .draw(tree, renderer, theme, &style, layout, cursor, viewport);
        }
        if hovered {
            renderer.with_layer(visible, |renderer| {
                for ((item, tree), layout) in parts {
                    item.as_widget()
                        .draw(tree, renderer, theme, &style, layout, cursor, viewport);
                }
            });
        }
    }

    fn operate(
        &mut self,
        tree: &mut Tree,
        layout: Layout<'_>,
        renderer: &iced::Renderer,
        operation: &mut dyn Operation,
    ) {
        for ((child, tree), layout) in self
            .children
            .iter_mut()
            .zip(&mut tree.children)
            .zip(layout.children())
        {
            child
                .as_widget_mut()
                .operate(tree, layout, renderer, operation);
        }
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
        let hovered = cursor.is_over(layout.bounds());
        let was_hovered = {
            let state = tree.state.downcast_mut::<State>();
            let was = state.hovered;
            if was != hovered {
                state.hovered = hovered;
                shell.request_redraw();
            }
            was
        };
        // Hover layers get events while hovered, on the event that leaves the row
        // (so tooltips close) and on every release (so buttons do not stay pressed).
        let resets = matches!(
            event,
            Event::Mouse(mouse::Event::ButtonReleased(_) | mouse::Event::CursorLeft)
                | Event::Touch(_)
        );
        if hovered || was_hovered || resets {
            let item_trees = &mut tree.children[1..];
            for ((child, tree), layout) in self.children[1..]
                .iter_mut()
                .zip(item_trees.iter_mut())
                .zip(layout.children().skip(1))
                .rev()
            {
                child.as_widget_mut().update(
                    tree, event, layout, cursor, renderer, clipboard, shell, viewport,
                );
                if shell.is_event_captured() {
                    return;
                }
            }
        }
        // Hover layers are on top: hide the cursor from the content where they are interactive.
        let over_item = hovered
            && self.children[1..]
                .iter()
                .zip(tree.children[1..].iter())
                .zip(layout.children().skip(1))
                .any(|((child, tree), layout)| {
                    child
                        .as_widget()
                        .mouse_interaction(tree, layout, cursor, viewport, renderer)
                        != mouse::Interaction::None
                });
        let content_cursor = if over_item {
            mouse::Cursor::Unavailable
        } else {
            cursor
        };
        if let Some(content_layout) = layout.children().next() {
            self.children[0].as_widget_mut().update(
                &mut tree.children[0],
                event,
                content_layout,
                content_cursor,
                renderer,
                clipboard,
                shell,
                viewport,
            );
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
        if cursor.is_over(layout.bounds()) {
            for ((child, tree), layout) in self.children[1..]
                .iter()
                .zip(&tree.children[1..])
                .zip(layout.children().skip(1))
                .rev()
            {
                let interaction = child
                    .as_widget()
                    .mouse_interaction(tree, layout, cursor, viewport, renderer);
                if interaction != mouse::Interaction::None {
                    return interaction;
                }
            }
        }
        match layout.children().next() {
            Some(content_layout) => self.children[0].as_widget().mouse_interaction(
                &tree.children[0],
                content_layout,
                cursor,
                viewport,
                renderer,
            ),
            None => mouse::Interaction::None,
        }
    }

    fn overlay<'b>(
        &'b mut self,
        tree: &'b mut Tree,
        layout: Layout<'b>,
        renderer: &iced::Renderer,
        viewport: &Rectangle,
        translation: Vector,
    ) -> Option<overlay::Element<'b, Message, Theme, iced::Renderer>> {
        // Overlays of the hover layers (tooltips, menus) only exist while hovered.
        if !tree.state.downcast_ref::<State>().hovered {
            let content_layout = layout.children().next()?;
            return self.children[0].as_widget_mut().overlay(
                &mut tree.children[0],
                content_layout,
                renderer,
                viewport,
                translation,
            );
        }
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
