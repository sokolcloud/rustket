//! Widgets that render an element as a real iced overlay above their base.
//!
//! Overlays are laid out and drawn after the whole widget tree (each in its own
//! renderer layer) and receive events first, so a modal drawn this way covers and
//! blocks everything, including content in scrollables and other overlays of the
//! base.
//!
//! A modal covers the viewport its parent hands to `overlay()`: the whole
//! window, or the area below the custom title bar of `window_frame`. Pointer
//! input outside that area is left to the base.

use iced::advanced::layout::{self, Layout};
use iced::advanced::overlay;
use iced::advanced::renderer;
use iced::advanced::widget::{self, Operation, Tree, Widget};
use iced::advanced::{Clipboard, Shell};
use iced::{
    Color, Element, Event, Length, Padding, Point, Rectangle, Size, Theme, Vector, keyboard, mouse,
    touch,
};

/// How the layer is placed and how it treats input to the base.
pub(crate) enum Kind<Message> {
    /// Centred content over a backdrop; blocks the base. Backdrop clicks and Escape
    /// publish the message.
    Modal {
        on_dismiss: Option<Message>,
        backdrop: fn(&Theme) -> Color,
        padding: f32,
    },
    /// Content pinned to the bottom-right corner; the base stays interactive.
    Corner { padding: f32 },
}

/// Always wraps the base (even with no layer) so toggling the layer never changes
/// the widget tree shape: the base keeps its state (scroll offsets, focus, caches).
pub(crate) struct Layered<'a, Message> {
    base: Element<'a, Message>,
    /// Placeholder `space()` while inactive, so the tree keeps two children.
    layer: Element<'a, Message>,
    active: bool,
    /// Identity of the layer content; a different key resets its widget state.
    key: Option<u64>,
    kind: Kind<Message>,
}

/// Tree state of [`Layered`] (its type is also the tree tag, distinct from any base).
#[derive(Default)]
struct LayeredState {
    key: Option<u64>,
}

impl<'a, Message: Clone + 'a> Layered<'a, Message> {
    pub(crate) fn new(
        base: Element<'a, Message>,
        layer: Option<Element<'a, Message>>,
        kind: Kind<Message>,
    ) -> Self {
        let active = layer.is_some();
        Self {
            base,
            layer: layer.unwrap_or_else(|| iced::widget::space().into()),
            active,
            key: None,
            kind,
        }
    }

    pub(crate) fn key(mut self, key: Option<u64>) -> Self {
        self.key = key;
        self
    }

    fn blocking(&self) -> bool {
        self.active && matches!(self.kind, Kind::Modal { .. })
    }
}

impl<'a, Message: Clone + 'a> From<Layered<'a, Message>> for Element<'a, Message> {
    fn from(l: Layered<'a, Message>) -> Self {
        Element::new(l)
    }
}

impl<'a, Message: Clone + 'a> Widget<Message, Theme, iced::Renderer> for Layered<'a, Message> {
    fn size(&self) -> Size<Length> {
        self.base.as_widget().size()
    }

    fn size_hint(&self) -> Size<Length> {
        self.base.as_widget().size_hint()
    }

    fn tag(&self) -> widget::tree::Tag {
        widget::tree::Tag::of::<LayeredState>()
    }

    fn state(&self) -> widget::tree::State {
        widget::tree::State::new(LayeredState { key: self.key })
    }

    fn children(&self) -> Vec<Tree> {
        vec![Tree::new(&self.base), Tree::new(&self.layer)]
    }

    fn diff(&self, tree: &mut Tree) {
        let state = tree.state.downcast_mut::<LayeredState>();
        if state.key != self.key {
            state.key = self.key;
            if let Some(layer_tree) = tree.children.get_mut(1) {
                *layer_tree = Tree::new(&self.layer);
            }
        }
        tree.diff_children(&[&self.base, &self.layer]);
    }

    fn layout(
        &mut self,
        tree: &mut Tree,
        renderer: &iced::Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        self.base
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
        // While a modal is up the base must not show hover effects.
        let cursor = if self.blocking() {
            mouse::Cursor::Unavailable
        } else {
            cursor
        };
        self.base.as_widget().draw(
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
        // Operations (scroll_to, snap_to, text…) always reach the base; while a
        // modal blocks it, focus operations skip the base so focus stays in the
        // dialog (which gets operations through the overlay).
        if self.blocking() {
            self.base.as_widget_mut().operate(
                &mut tree.children[0],
                layout,
                renderer,
                &mut WithoutFocus(operation),
            );
        } else {
            self.base
                .as_widget_mut()
                .operate(&mut tree.children[0], layout, renderer, operation);
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
        // While a modal is up, only window events (resize, redraw) reach the base.
        if self.blocking() && !matches!(event, Event::Window(_)) {
            return;
        }
        self.base.as_widget_mut().update(
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
        if self.blocking() {
            mouse::Interaction::None
        } else {
            self.base.as_widget().mouse_interaction(
                &tree.children[0],
                layout,
                cursor,
                viewport,
                renderer,
            )
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
        let (base_tree, layer_tree) = tree.children.split_at_mut(1);
        if !self.active {
            return self.base.as_widget_mut().overlay(
                &mut base_tree[0],
                layout,
                renderer,
                viewport,
                translation,
            );
        }
        let inner = match self.kind {
            // Overlays of the base (open menus) are dropped while a modal is shown.
            Kind::Modal { .. } => None,
            Kind::Corner { .. } => self.base.as_widget_mut().overlay(
                &mut base_tree[0],
                layout,
                renderer,
                viewport,
                translation,
            ),
        };
        Some(overlay::Element::new(Box::new(LayerOverlay {
            inner,
            tree: &mut layer_tree[0],
            content: &mut self.layer,
            kind: &self.kind,
            region: *viewport,
        })))
    }
}

struct LayerOverlay<'a, 'b, Message> {
    inner: Option<overlay::Element<'b, Message, Theme, iced::Renderer>>,
    tree: &'b mut Tree,
    content: &'b mut Element<'a, Message>,
    kind: &'b Kind<Message>,
    /// What a modal covers (absolute): the viewport given to `overlay()`.
    region: Rectangle,
}

impl<Message: Clone> LayerOverlay<'_, '_, Message> {
    /// The area a modal covers within `window`.
    fn modal_region(&self, window: Rectangle) -> Rectangle {
        self.region.intersection(&window).unwrap_or(window)
    }

    /// Whether the pointer is off a modal's area (over the custom title bar).
    fn outside_modal(&self, window: Rectangle, cursor: mouse::Cursor) -> bool {
        matches!(self.kind, Kind::Modal { .. })
            && cursor
                .position()
                .is_some_and(|p| !self.modal_region(window).contains(p))
    }

    fn content_layout<'l>(layout: Layout<'l>) -> Option<Layout<'l>> {
        layout.children().next()
    }

    fn inner_layout<'l>(layout: Layout<'l>) -> Option<Layout<'l>> {
        layout.children().nth(1)
    }

    /// Whether the cursor is over one of the cards of the corner layer: its content
    /// is a column of groups (columns) of cards. Content without children counts
    /// as one card.
    fn over_card(content_layout: Layout<'_>, cursor: mouse::Cursor) -> bool {
        let mut groups = content_layout.children().peekable();
        if groups.peek().is_none() {
            return cursor.is_over(content_layout.bounds());
        }
        groups
            .flat_map(|group| group.children())
            .any(|card| cursor.is_over(card.bounds()))
    }
}

impl<Message: Clone> overlay::Overlay<Message, Theme, iced::Renderer>
    for LayerOverlay<'_, '_, Message>
{
    fn layout(&mut self, renderer: &iced::Renderer, bounds: Size) -> layout::Node {
        let padding = match self.kind {
            Kind::Modal { padding, .. } | Kind::Corner { padding } => *padding,
        };
        let area = match self.kind {
            Kind::Modal { .. } => self.modal_region(Rectangle::with_size(bounds)),
            Kind::Corner { .. } => Rectangle::with_size(bounds),
        };
        let limits = layout::Limits::new(Size::ZERO, area.size()).shrink(Padding::new(padding));
        let node = self
            .content
            .as_widget_mut()
            .layout(self.tree, renderer, &limits);
        let size = node.size();
        let origin = match self.kind {
            Kind::Modal { .. } => Point::new(
                area.x + ((area.width - size.width) / 2.0).max(0.0),
                area.y + ((area.height - size.height) / 2.0).max(0.0),
            ),
            Kind::Corner { padding } => Point::new(
                (bounds.width - size.width - padding).max(0.0),
                (bounds.height - size.height - padding).max(0.0),
            ),
        };
        let mut children = vec![node.move_to(origin)];
        if let Some(inner) = &mut self.inner {
            children.push(inner.as_overlay_mut().layout(renderer, bounds));
        }
        layout::Node::with_children(bounds, children)
    }

    fn draw(
        &self,
        renderer: &mut iced::Renderer,
        theme: &Theme,
        style: &renderer::Style,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
    ) {
        use iced::advanced::Renderer as _;
        let window = layout.bounds();
        if let (Some(inner), Some(inner_layout)) = (&self.inner, Self::inner_layout(layout)) {
            renderer.with_layer(window, |renderer| {
                inner
                    .as_overlay()
                    .draw(renderer, theme, style, inner_layout, cursor);
            });
        }
        let Some(content_layout) = Self::content_layout(layout) else {
            return;
        };
        renderer.with_layer(window, |renderer| {
            if let Kind::Modal { backdrop, .. } = self.kind {
                renderer.fill_quad(
                    renderer::Quad {
                        bounds: self.modal_region(window),
                        ..renderer::Quad::default()
                    },
                    backdrop(theme),
                );
            }
            self.content.as_widget().draw(
                self.tree,
                renderer,
                theme,
                style,
                content_layout,
                cursor,
                &content_layout.bounds(),
            );
        });
    }

    fn operate(
        &mut self,
        layout: Layout<'_>,
        renderer: &iced::Renderer,
        operation: &mut dyn widget::Operation,
    ) {
        if let Some(content_layout) = Self::content_layout(layout) {
            self.content
                .as_widget_mut()
                .operate(self.tree, content_layout, renderer, operation);
        }
        if let (Some(inner), Some(inner_layout)) = (&mut self.inner, Self::inner_layout(layout)) {
            inner
                .as_overlay_mut()
                .operate(inner_layout, renderer, operation);
        }
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
        let Some(content_layout) = Self::content_layout(layout) else {
            return;
        };
        let content_bounds = content_layout.bounds();
        let outside = self.outside_modal(layout.bounds(), cursor);
        if outside
            && matches!(
                event,
                Event::Mouse(mouse::Event::ButtonPressed(_))
                    | Event::Touch(touch::Event::FingerPressed { .. })
            )
        {
            // A press on the title bar: neither the dialog nor a dismissal.
            return;
        }
        if let Kind::Modal { on_dismiss, .. } = self.kind {
            match event {
                // A focused input would just unfocus on Escape: dismiss right away.
                // Otherwise the content sees Escape first (e.g. to close its own popup).
                Event::Keyboard(keyboard::Event::KeyPressed {
                    key: keyboard::Key::Named(keyboard::key::Named::Escape),
                    ..
                }) if has_focus(self.content, self.tree, content_layout, renderer) => {
                    if let Some(msg) = on_dismiss {
                        shell.publish(msg.clone());
                    }
                    shell.capture_event();
                    return;
                }
                // Tab / Shift+Tab cycle focus within the dialog.
                Event::Keyboard(keyboard::Event::KeyPressed {
                    key: keyboard::Key::Named(keyboard::key::Named::Tab),
                    modifiers,
                    ..
                }) => {
                    cycle_focus(
                        self.content,
                        self.tree,
                        content_layout,
                        renderer,
                        !modifiers.shift(),
                    );
                    shell.capture_event();
                    shell.request_redraw();
                    return;
                }
                _ => {}
            }
        }
        self.content.as_widget_mut().update(
            self.tree,
            event,
            content_layout,
            cursor,
            renderer,
            clipboard,
            shell,
            &content_bounds,
        );
        match self.kind {
            Kind::Modal { on_dismiss, .. } => {
                if shell.is_event_captured() {
                    return;
                }
                match event {
                    Event::Keyboard(keyboard::Event::KeyPressed {
                        key: keyboard::Key::Named(keyboard::key::Named::Escape),
                        ..
                    }) => {
                        if let Some(msg) = on_dismiss {
                            shell.publish(msg.clone());
                        }
                        shell.capture_event();
                    }
                    Event::Keyboard(_) => shell.capture_event(),
                    Event::Mouse(mouse::Event::ButtonPressed(_))
                    | Event::Touch(touch::Event::FingerPressed { .. }) => {
                        if !cursor.is_over(content_bounds)
                            && let Some(msg) = on_dismiss
                        {
                            shell.publish(msg.clone());
                        }
                        shell.capture_event();
                    }
                    Event::Mouse(_) | Event::Touch(_) if !outside => shell.capture_event(),
                    _ => {}
                }
            }
            Kind::Corner { .. } => {
                if shell.is_event_captured() {
                    return;
                }
                // Presses and wheel on a card stop there; moves and releases pass
                // through so drags that end over a toast still finish.
                if matches!(
                    event,
                    Event::Mouse(
                        mouse::Event::ButtonPressed(_) | mouse::Event::WheelScrolled { .. }
                    ) | Event::Touch(touch::Event::FingerPressed { .. })
                ) && Self::over_card(content_layout, cursor)
                {
                    shell.capture_event();
                    return;
                }
                if let (Some(inner), Some(inner_layout)) =
                    (&mut self.inner, Self::inner_layout(layout))
                {
                    inner.as_overlay_mut().update(
                        event,
                        inner_layout,
                        cursor,
                        renderer,
                        clipboard,
                        shell,
                    );
                }
            }
        }
    }

    fn mouse_interaction(
        &self,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        renderer: &iced::Renderer,
    ) -> mouse::Interaction {
        let Some(content_layout) = Self::content_layout(layout) else {
            return mouse::Interaction::None;
        };
        if self.outside_modal(layout.bounds(), cursor) {
            return mouse::Interaction::None;
        }
        let content = self.content.as_widget().mouse_interaction(
            self.tree,
            content_layout,
            cursor,
            &content_layout.bounds(),
            renderer,
        );
        match self.kind {
            // Anything but `None` hides the cursor from the base (no hover underneath).
            Kind::Modal { .. } => content.max(mouse::Interaction::Idle),
            Kind::Corner { .. } => {
                if content != mouse::Interaction::None || Self::over_card(content_layout, cursor) {
                    content.max(mouse::Interaction::Idle)
                } else if let (Some(inner), Some(inner_layout)) =
                    (&self.inner, Self::inner_layout(layout))
                {
                    inner
                        .as_overlay()
                        .mouse_interaction(inner_layout, cursor, renderer)
                } else {
                    mouse::Interaction::None
                }
            }
        }
    }

    fn overlay<'c>(
        &'c mut self,
        layout: Layout<'c>,
        renderer: &iced::Renderer,
    ) -> Option<overlay::Element<'c, Message, Theme, iced::Renderer>> {
        let mut children = Vec::new();
        let mut layouts = layout.children();
        let content_layout = layouts.next();
        let inner_layout = layouts.next();
        if let (Some(inner), Some(inner_layout)) = (&mut self.inner, inner_layout)
            && let Some(nested) = inner.as_overlay_mut().overlay(inner_layout, renderer)
        {
            children.push(nested);
        }
        if let Some(content_layout) = content_layout {
            let bounds = layout.bounds();
            if let Some(nested) = self.content.as_widget_mut().overlay(
                self.tree,
                content_layout,
                renderer,
                &bounds,
                Vector::ZERO,
            ) {
                children.push(nested);
            }
        }
        match children.len() {
            0 => None,
            1 => children.pop(),
            _ => Some(overlay::Group::with_children(children).overlay()),
        }
    }
}

/// Forwards an operation except focus-related calls.
struct WithoutFocus<'a>(&'a mut dyn Operation);

impl Operation for WithoutFocus<'_> {
    fn traverse(&mut self, operate: &mut dyn FnMut(&mut dyn Operation)) {
        self.0.traverse(&mut |op| operate(&mut WithoutFocus(op)));
    }

    fn container(&mut self, id: Option<&widget::Id>, bounds: Rectangle) {
        self.0.container(id, bounds);
    }

    fn scrollable(
        &mut self,
        id: Option<&widget::Id>,
        bounds: Rectangle,
        content_bounds: Rectangle,
        translation: Vector,
        state: &mut dyn widget::operation::Scrollable,
    ) {
        self.0
            .scrollable(id, bounds, content_bounds, translation, state);
    }

    fn text(&mut self, id: Option<&widget::Id>, bounds: Rectangle, text: &str) {
        self.0.text(id, bounds, text);
    }

    fn custom(
        &mut self,
        id: Option<&widget::Id>,
        bounds: Rectangle,
        state: &mut dyn std::any::Any,
    ) {
        self.0.custom(id, bounds, state);
    }
}

/// Counts focusables and finds the focused one.
#[derive(Default)]
struct FocusCount {
    total: usize,
    focused: Option<usize>,
}

impl Operation for FocusCount {
    fn traverse(&mut self, operate: &mut dyn FnMut(&mut dyn Operation)) {
        operate(self);
    }

    fn focusable(
        &mut self,
        _id: Option<&widget::Id>,
        _bounds: Rectangle,
        state: &mut dyn widget::operation::Focusable,
    ) {
        if state.is_focused() {
            self.focused = Some(self.total);
        }
        self.total += 1;
    }
}

fn count_focus<Message>(
    content: &mut Element<'_, Message>,
    tree: &mut Tree,
    layout: Layout<'_>,
    renderer: &iced::Renderer,
) -> FocusCount {
    let mut count = FocusCount::default();
    content
        .as_widget_mut()
        .operate(tree, layout, renderer, &mut count);
    count
}

pub(crate) fn has_focus<Message>(
    content: &mut Element<'_, Message>,
    tree: &mut Tree,
    layout: Layout<'_>,
    renderer: &iced::Renderer,
) -> bool {
    count_focus(content, tree, layout, renderer)
        .focused
        .is_some()
}

/// Moves focus to the next (`forward`) or previous focusable of the whole
/// window, wrapping around (iced's `focus_next` unfocuses after the last one).
pub fn focus_cycle<T: Send + 'static>(forward: bool) -> iced::Task<T> {
    use iced::advanced::widget::operation::{focusable, then};
    fn index(count: focusable::Count, forward: bool) -> usize {
        match (count.focused, forward) {
            (None, true) => 0,
            (None, false) => count.total.saturating_sub(1),
            (Some(i), true) => (i + 1) % count.total,
            (Some(i), false) => (i + count.total - 1) % count.total,
        }
    }
    if forward {
        iced::advanced::widget::operate(then(focusable::count(), |count| FocusIndex {
            target: index(count, true),
            current: 0,
        }))
    } else {
        iced::advanced::widget::operate(then(focusable::count(), |count| FocusIndex {
            target: index(count, false),
            current: 0,
        }))
    }
}

/// Focuses the `target`-th focusable and unfocuses the others.
struct FocusIndex {
    target: usize,
    current: usize,
}

impl<T> Operation<T> for FocusIndex {
    fn traverse(&mut self, operate: &mut dyn FnMut(&mut dyn Operation<T>)) {
        operate(self);
    }

    fn focusable(
        &mut self,
        _id: Option<&widget::Id>,
        _bounds: Rectangle,
        state: &mut dyn widget::operation::Focusable,
    ) {
        if self.current == self.target {
            state.focus();
        } else if state.is_focused() {
            state.unfocus();
        }
        self.current += 1;
    }
}

/// Moves focus to the next/previous focusable of `content`, wrapping around.
fn cycle_focus<Message>(
    content: &mut Element<'_, Message>,
    tree: &mut Tree,
    layout: Layout<'_>,
    renderer: &iced::Renderer,
    forward: bool,
) {
    let count = count_focus(content, tree, layout, renderer);
    if count.total == 0 {
        return;
    }
    let target = match (count.focused, forward) {
        (None, true) => 0,
        (None, false) => count.total - 1,
        (Some(i), true) => (i + 1) % count.total,
        (Some(i), false) => (i + count.total - 1) % count.total,
    };
    content.as_widget_mut().operate(
        tree,
        layout,
        renderer,
        &mut FocusIndex { target, current: 0 },
    );
}
