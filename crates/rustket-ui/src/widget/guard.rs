//! A transparent wrapper that re-checks the widget tag before diffing.
//!
//! iced 0.14's `keyed_column` diffs its children by calling `Widget::diff`
//! directly, skipping the tag check `Tree::diff` does, and its key search only
//! handles one contiguous insertion or removal run before falling back to
//! positional diffing. A child whose widget type differs from the tree it is
//! matched with then downcasts foreign state; `lazy` panics with "Downcast widget
//! state". Every child of a keyed column should be wrapped in [`guard`].

use iced::advanced::layout::{self, Layout};
use iced::advanced::overlay;
use iced::advanced::renderer;
use iced::advanced::widget::{self, Operation, Tree, Widget};
use iced::advanced::{Clipboard, Shell};
use iced::{Element, Event, Length, Rectangle, Size, Vector, mouse};

/// Wraps `content` so a tree of another widget type is rebuilt instead of diffed.
pub fn guard<'a, Message, Theme, Renderer>(
    content: impl Into<Element<'a, Message, Theme, Renderer>>,
) -> Element<'a, Message, Theme, Renderer>
where
    Message: 'a,
    Theme: 'a,
    Renderer: renderer::Renderer + 'a,
{
    Element::new(Guard {
        content: content.into(),
    })
}

struct Guard<'a, Message, Theme, Renderer> {
    content: Element<'a, Message, Theme, Renderer>,
}

impl<Message, Theme, Renderer> Widget<Message, Theme, Renderer>
    for Guard<'_, Message, Theme, Renderer>
where
    Renderer: renderer::Renderer,
{
    fn size(&self) -> Size<Length> {
        self.content.as_widget().size()
    }

    fn size_hint(&self) -> Size<Length> {
        self.content.as_widget().size_hint()
    }

    fn tag(&self) -> widget::tree::Tag {
        self.content.as_widget().tag()
    }

    fn state(&self) -> widget::tree::State {
        self.content.as_widget().state()
    }

    fn children(&self) -> Vec<Tree> {
        self.content.as_widget().children()
    }

    fn diff(&self, tree: &mut Tree) {
        let content = self.content.as_widget();
        if tree.tag == content.tag() {
            content.diff(tree);
        } else {
            *tree = Tree::new(content);
        }
    }

    fn layout(
        &mut self,
        tree: &mut Tree,
        renderer: &Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        self.content.as_widget_mut().layout(tree, renderer, limits)
    }

    fn draw(
        &self,
        tree: &Tree,
        renderer: &mut Renderer,
        theme: &Theme,
        style: &renderer::Style,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
    ) {
        self.content
            .as_widget()
            .draw(tree, renderer, theme, style, layout, cursor, viewport);
    }

    fn operate(
        &mut self,
        tree: &mut Tree,
        layout: Layout<'_>,
        renderer: &Renderer,
        operation: &mut dyn Operation,
    ) {
        self.content
            .as_widget_mut()
            .operate(tree, layout, renderer, operation);
    }

    fn update(
        &mut self,
        tree: &mut Tree,
        event: &Event,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        renderer: &Renderer,
        clipboard: &mut dyn Clipboard,
        shell: &mut Shell<'_, Message>,
        viewport: &Rectangle,
    ) {
        self.content.as_widget_mut().update(
            tree, event, layout, cursor, renderer, clipboard, shell, viewport,
        );
    }

    fn mouse_interaction(
        &self,
        tree: &Tree,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
        renderer: &Renderer,
    ) -> mouse::Interaction {
        self.content
            .as_widget()
            .mouse_interaction(tree, layout, cursor, viewport, renderer)
    }

    fn overlay<'b>(
        &'b mut self,
        tree: &'b mut Tree,
        layout: Layout<'b>,
        renderer: &Renderer,
        viewport: &Rectangle,
        translation: Vector,
    ) -> Option<overlay::Element<'b, Message, Theme, Renderer>> {
        self.content
            .as_widget_mut()
            .overlay(tree, layout, renderer, viewport, translation)
    }
}

#[cfg(test)]
mod tests {
    use super::guard;
    use iced::advanced::widget::Tree;
    use iced::widget::{container, keyed_column, lazy, sensor, space, text};
    use iced::{Element, Theme};
    use std::panic::{AssertUnwindSafe, catch_unwind};

    type El = Element<'static, (), Theme, ()>;

    #[derive(Clone, Copy)]
    enum Kind {
        Lazy,
        Container,
        Sensor,
    }

    fn child(kind: Kind, n: u64) -> El {
        match kind {
            Kind::Lazy => lazy(n, |n| -> El { text(n.to_string()).into() }).into(),
            Kind::Container => container(text(n.to_string())).into(),
            Kind::Sensor => sensor(space().height(1)).key(n).into(),
        }
    }

    fn frame(rows: &[(u64, Kind)], guarded: bool) -> El {
        let mut col = keyed_column::<u64, (), Theme, ()>([]);
        for &(key, kind) in rows {
            let el = child(kind, key);
            col = col.push(key, if guarded { guard(el) } else { el });
        }
        col.into()
    }

    /// Diffs frame `b` over the tree built from frame `a` and the reverse;
    /// `false` if either panics.
    fn diff_both_ways(a: &[(u64, Kind)], b: &[(u64, Kind)], guarded: bool) -> bool {
        let one_way = |from: &[(u64, Kind)], to: &[(u64, Kind)]| {
            catch_unwind(AssertUnwindSafe(|| {
                let old = frame(from, guarded);
                let new = frame(to, guarded);
                let mut tree = Tree::new(old.as_widget());
                tree.diff(new.as_widget());
                tree.diff(old.as_widget());
            }))
            .is_ok()
        };
        one_way(a, b) && one_way(b, a)
    }

    type Frame = &'static [(u64, Kind)];

    const CASES: &[(Frame, Frame)] = &[
        // Another widget type under the same key.
        (&[(1, Kind::Lazy)], &[(1, Kind::Container)]),
        (
            &[(0, Kind::Sensor), (1, Kind::Lazy), (2, Kind::Lazy)],
            &[(0, Kind::Sensor), (1, Kind::Container), (2, Kind::Lazy)],
        ),
        // Another thread in the same panel: the first key changes and one row
        // goes, so the search drops the first tree and the new parent row is
        // diffed against the sensor's tree.
        (
            &[
                (7, Kind::Lazy),
                (0, Kind::Sensor),
                (1, Kind::Lazy),
                (2, Kind::Lazy),
            ],
            &[(8, Kind::Lazy), (0, Kind::Sensor), (3, Kind::Lazy)],
        ),
        // Two separate changes (a row dropped at the top, one added below the
        // sensor): the search gives up and diffs by position.
        (
            &[
                (9, Kind::Lazy),
                (0, Kind::Sensor),
                (1, Kind::Lazy),
                (2, Kind::Lazy),
            ],
            &[
                (0, Kind::Sensor),
                (5, Kind::Lazy),
                (1, Kind::Lazy),
                (2, Kind::Lazy),
            ],
        ),
    ];

    #[test]
    fn unguarded_keyed_column_panics_on_type_change() {
        // Documents the iced 0.14 behaviour the guard exists for.
        let hook = std::panic::take_hook();
        std::panic::set_hook(Box::new(|_| {}));
        let results: Vec<bool> = CASES
            .iter()
            .map(|(a, b)| diff_both_ways(a, b, false))
            .collect();
        std::panic::set_hook(hook);
        assert!(results.iter().all(|ok| !ok), "{results:?}");
    }

    #[test]
    fn guarded_keyed_column_survives_type_changes() {
        for (i, (a, b)) in CASES.iter().enumerate() {
            assert!(diff_both_ways(a, b, true), "case {i}");
        }
    }
}
