//! A keyed column whose children keep their widget state by key.
//!
//! iced 0.14's `keyed_column` only handles one contiguous insertion or removal
//! run (a pure append even inserts at the front) and otherwise diffs by
//! position, so a row is diffed against its neighbour's tree: stateful widgets
//! (an open context menu) move to another row and every `lazy` row rebuilds.
//! [`KeyedColumn`] matches the previous trees by key instead. Layout, drawing
//! and event handling are the same as iced's.

use std::collections::HashMap;
use std::hash::Hash;

use iced::advanced::layout::{self, Layout};
use iced::advanced::overlay;
use iced::advanced::renderer;
use iced::advanced::widget::{Operation, Tree, tree};
use iced::advanced::{Clipboard, Shell, Widget};
use iced::{Alignment, Element, Event, Length, Padding, Pixels, Rectangle, Size, Vector, mouse};

/// An empty [`KeyedColumn`].
pub fn keyed_list<'a, Key, Message, Theme, Renderer>()
-> KeyedColumn<'a, Key, Message, Theme, Renderer>
where
    Key: Copy + Eq + Hash,
    Renderer: renderer::Renderer,
{
    KeyedColumn::new()
}

/// Distributes its children vertically; each child's state follows its key.
pub struct KeyedColumn<'a, Key, Message, Theme = iced::Theme, Renderer = iced::Renderer>
where
    Key: Copy + Eq + Hash,
{
    spacing: f32,
    padding: Padding,
    width: Length,
    height: Length,
    max_width: f32,
    align_items: Alignment,
    keys: Vec<Key>,
    children: Vec<Element<'a, Message, Theme, Renderer>>,
}

impl<'a, Key, Message, Theme, Renderer> KeyedColumn<'a, Key, Message, Theme, Renderer>
where
    Key: Copy + Eq + Hash,
    Renderer: renderer::Renderer,
{
    pub fn new() -> Self {
        Self {
            spacing: 0.0,
            padding: Padding::ZERO,
            width: Length::Shrink,
            height: Length::Shrink,
            max_width: f32::INFINITY,
            align_items: Alignment::Start,
            keys: Vec::new(),
            children: Vec::new(),
        }
    }

    /// Vertical spacing between children.
    pub fn spacing(mut self, amount: impl Into<Pixels>) -> Self {
        self.spacing = amount.into().0;
        self
    }

    pub fn padding(mut self, padding: impl Into<Padding>) -> Self {
        self.padding = padding.into();
        self
    }

    pub fn width(mut self, width: impl Into<Length>) -> Self {
        self.width = width.into();
        self
    }

    pub fn height(mut self, height: impl Into<Length>) -> Self {
        self.height = height.into();
        self
    }

    pub fn max_width(mut self, max_width: impl Into<Pixels>) -> Self {
        self.max_width = max_width.into().0;
        self
    }

    /// Horizontal alignment of the children.
    pub fn align_items(mut self, align: Alignment) -> Self {
        self.align_items = align;
        self
    }

    /// Adds `child` under `key`. Keys should be unique; duplicates fall back to
    /// positional matching.
    pub fn push(
        mut self,
        key: Key,
        child: impl Into<Element<'a, Message, Theme, Renderer>>,
    ) -> Self {
        let child = child.into();
        let hint = child.as_widget().size_hint();
        self.width = self.width.enclose(hint.width);
        self.height = self.height.enclose(hint.height);
        self.keys.push(key);
        self.children.push(child);
        self
    }
}

impl<Key, Message, Theme, Renderer> Default for KeyedColumn<'_, Key, Message, Theme, Renderer>
where
    Key: Copy + Eq + Hash,
    Renderer: renderer::Renderer,
{
    fn default() -> Self {
        Self::new()
    }
}

/// The keys `tree` was last diffed with, if it is the tree of a [`KeyedColumn`]
/// with keys of type `Key` (for tests and debugging).
#[doc(hidden)]
pub fn keyed_list_keys<Key: 'static>(tree: &Tree) -> Option<&[Key]> {
    if tree.tag != tree::Tag::of::<State<Key>>() {
        return None;
    }
    match &tree.state {
        tree::State::Some(state) => state
            .downcast_ref::<State<Key>>()
            .map(|s| s.keys.as_slice()),
        tree::State::None => None,
    }
}

struct State<Key> {
    keys: Vec<Key>,
}

/// Rebuilds `children` (diffed last with `old_keys`) in the order of `keys`,
/// moving each previous tree to the child with the same key. A repeated key
/// takes the trees of that key in their previous order.
fn diff_by_key<Key, Message, Theme, Renderer>(
    children: &mut Vec<Tree>,
    old_keys: &[Key],
    keys: &[Key],
    widgets: &[Element<'_, Message, Theme, Renderer>],
) where
    Key: Copy + Eq + Hash,
    Renderer: renderer::Renderer,
{
    #[cfg(not(test))]
    debug_assert_eq!(
        keys.iter().collect::<std::collections::HashSet<_>>().len(),
        keys.len(),
        "duplicate keys in a keyed list"
    );
    if old_keys == keys && children.len() == widgets.len() {
        for (tree, widget) in children.iter_mut().zip(widgets) {
            tree.diff(widget);
        }
        return;
    }
    let mut old: Vec<Option<Tree>> = std::mem::take(children).into_iter().map(Some).collect();
    // `first[key]`: the first unused previous position of `key`; `next[i]`:
    // the following position with the same key.
    let mut first = HashMap::with_capacity(old.len());
    let mut next = vec![None; old.len()];
    for (i, key) in old_keys.iter().enumerate().take(old.len()).rev() {
        next[i] = first.insert(*key, Some(i)).flatten();
    }
    children.reserve(widgets.len());
    for (key, widget) in keys.iter().zip(widgets) {
        let from = first.get_mut(key).and_then(|slot| {
            let i = slot.take()?;
            *slot = next[i];
            old[i].take()
        });
        match from {
            Some(mut tree) => {
                // `Tree::diff` rebuilds the tree if the widget type changed.
                tree.diff(widget);
                children.push(tree);
            }
            None => children.push(Tree::new(widget)),
        }
    }
}

impl<Key, Message, Theme, Renderer> Widget<Message, Theme, Renderer>
    for KeyedColumn<'_, Key, Message, Theme, Renderer>
where
    Key: Copy + Eq + Hash + 'static,
    Renderer: renderer::Renderer,
{
    fn tag(&self) -> tree::Tag {
        tree::Tag::of::<State<Key>>()
    }

    fn state(&self) -> tree::State {
        tree::State::new(State {
            keys: self.keys.clone(),
        })
    }

    fn children(&self) -> Vec<Tree> {
        self.children.iter().map(Tree::new).collect()
    }

    fn diff(&self, tree: &mut Tree) {
        let Tree {
            state, children, ..
        } = tree;
        let state = state.downcast_mut::<State<Key>>();
        diff_by_key(children, &state.keys, &self.keys, &self.children);
        if state.keys != self.keys {
            state.keys.clone_from(&self.keys);
        }
    }

    fn size(&self) -> Size<Length> {
        Size {
            width: self.width,
            height: self.height,
        }
    }

    fn layout(
        &mut self,
        tree: &mut Tree,
        renderer: &Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        let limits = limits
            .max_width(self.max_width)
            .width(self.width)
            .height(self.height);
        layout::flex::resolve(
            layout::flex::Axis::Vertical,
            renderer,
            &limits,
            self.width,
            self.height,
            self.padding,
            self.spacing,
            self.align_items,
            &mut self.children,
            &mut tree.children,
        )
    }

    fn operate(
        &mut self,
        tree: &mut Tree,
        layout: Layout<'_>,
        renderer: &Renderer,
        operation: &mut dyn Operation,
    ) {
        operation.container(None, layout.bounds());
        operation.traverse(&mut |operation| {
            self.children
                .iter_mut()
                .zip(&mut tree.children)
                .zip(layout.children())
                .for_each(|((child, state), layout)| {
                    child
                        .as_widget_mut()
                        .operate(state, layout, renderer, operation);
                });
        });
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
        for ((child, tree), layout) in self
            .children
            .iter_mut()
            .zip(&mut tree.children)
            .zip(layout.children())
        {
            child.as_widget_mut().update(
                tree, event, layout, cursor, renderer, clipboard, shell, viewport,
            );
        }
    }

    fn mouse_interaction(
        &self,
        tree: &Tree,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
        renderer: &Renderer,
    ) -> mouse::Interaction {
        self.children
            .iter()
            .zip(&tree.children)
            .zip(layout.children())
            .map(|((child, tree), layout)| {
                child
                    .as_widget()
                    .mouse_interaction(tree, layout, cursor, viewport, renderer)
            })
            .max()
            .unwrap_or_default()
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
        for ((child, state), layout) in self
            .children
            .iter()
            .zip(&tree.children)
            .zip(layout.children())
        {
            child
                .as_widget()
                .draw(state, renderer, theme, style, layout, cursor, viewport);
        }
    }

    fn overlay<'b>(
        &'b mut self,
        tree: &'b mut Tree,
        layout: Layout<'b>,
        renderer: &Renderer,
        viewport: &Rectangle,
        translation: Vector,
    ) -> Option<overlay::Element<'b, Message, Theme, Renderer>> {
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

impl<'a, Key, Message, Theme, Renderer> From<KeyedColumn<'a, Key, Message, Theme, Renderer>>
    for Element<'a, Message, Theme, Renderer>
where
    Key: Copy + Eq + Hash + 'static,
    Message: 'a,
    Theme: 'a,
    Renderer: renderer::Renderer + 'a,
{
    fn from(column: KeyedColumn<'a, Key, Message, Theme, Renderer>) -> Self {
        Self::new(column)
    }
}

#[cfg(test)]
mod tests;
