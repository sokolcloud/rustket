//! Popover and right-click context menus.

use iced::advanced::layout::{self, Layout};
use iced::advanced::overlay;
use iced::advanced::renderer;
use iced::advanced::widget::{Operation, Tree, Widget, tree};
use iced::advanced::{Clipboard, Shell};
use iced::widget::{button, column, container, row, space, text};
use iced::{
    Alignment, Element, Event, Length, Point, Rectangle, Size, Theme, Vector, border, keyboard,
    mouse,
};

use super::popover::{Placement, popover};

use super::ellipsis::ellipsis;
use super::misc::divider;
use crate::icon::{self, Icon};
use crate::style;
use crate::theme::{
    DEFAULT_FONT, RADIUS_SM, SPACE_1, SPACE_2, SPACE_3, TEXT_BODY, TEXT_CAPTION, tokens,
};

/// One actionable menu row.
#[derive(Debug, Clone)]
pub struct MenuItem<Message> {
    pub icon: Option<Icon>,
    pub label: String,
    /// Optional right-aligned hint (keyboard shortcut).
    pub hint: Option<String>,
    pub danger: bool,
    pub disabled: bool,
    pub on_select: Message,
}

impl<Message> MenuItem<Message> {
    pub fn new(label: impl Into<String>, on_select: Message) -> Self {
        Self {
            icon: None,
            label: label.into(),
            hint: None,
            danger: false,
            disabled: false,
            on_select,
        }
    }

    pub fn icon(mut self, icon: Icon) -> Self {
        self.icon = Some(icon);
        self
    }

    pub fn hint(mut self, hint: impl Into<String>) -> Self {
        self.hint = Some(hint.into());
        self
    }

    pub fn danger(mut self, danger: bool) -> Self {
        self.danger = danger;
        self
    }

    pub fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }
}

#[derive(Debug, Clone)]
pub enum MenuEntry<Message> {
    Item(MenuItem<Message>),
    /// Small non-interactive caption heading a group.
    Header(String),
    Separator,
}

impl<Message> From<MenuItem<Message>> for MenuEntry<Message> {
    fn from(item: MenuItem<Message>) -> Self {
        MenuEntry::Item(item)
    }
}

/// Minimum menu width; menus grow with their longest item up to [`MENU_MAX_WIDTH`].
pub const MENU_WIDTH: f32 = 220.0;
pub const MENU_MAX_WIDTH: f32 = 320.0;

/// Width that shows every item of `entries` unclipped, within
/// [`MENU_WIDTH`]..=[`MENU_MAX_WIDTH`] (longer labels end with "…").
pub fn menu_width<Message>(entries: &[MenuEntry<Message>]) -> f32 {
    // Card padding, row padding, icon and spacing around the label.
    const CHROME: f32 = 2.0 * SPACE_1 + 2.0 * 8.0 + 16.0 + SPACE_3 + 2.0;
    let widest = entries
        .iter()
        .filter_map(|e| match e {
            MenuEntry::Item(item) => {
                let hint = item.hint.as_deref().map_or(0.0, |h| {
                    SPACE_3 + super::ellipsis::natural_width(h, TEXT_CAPTION, DEFAULT_FONT)
                });
                Some(super::ellipsis::natural_width(&item.label, TEXT_BODY, DEFAULT_FONT) + hint)
            }
            MenuEntry::Header(h) => Some(super::ellipsis::natural_width(
                h,
                TEXT_CAPTION,
                DEFAULT_FONT,
            )),
            MenuEntry::Separator => None,
        })
        .fold(0.0, f32::max);
    (widest + CHROME).ceil().clamp(MENU_WIDTH, MENU_MAX_WIDTH)
}

/// The menu card itself (usable standalone, e.g. inside a custom overlay).
/// Up/Down/Home/End highlight an item and Enter or Space selects it.
pub fn menu_panel<'a, Message: Clone + 'a>(entries: &[MenuEntry<Message>]) -> Element<'a, Message> {
    let actions = entries
        .iter()
        .map(|entry| match entry {
            MenuEntry::Item(item) if !item.disabled => Some(item.on_select.clone()),
            _ => None,
        })
        .collect();
    let rows = entries.iter().map(|entry| -> Element<'a, Message> {
        match entry {
            MenuEntry::Separator => container(divider()).padding([SPACE_1, 0.0]).into(),
            MenuEntry::Header(title) => container(
                text(title.clone())
                    .size(TEXT_CAPTION)
                    .style(style::text_hint),
            )
            .padding([SPACE_1, SPACE_2])
            .into(),
            MenuEntry::Item(item) => {
                let mut content = row![].spacing(SPACE_3).align_y(Alignment::Center);
                content = match item.icon {
                    Some(i) => content.push(icon::sized(i, 16.0)),
                    None => content.push(space().width(0)),
                };
                content = content.push(ellipsis(item.label.clone()).size(TEXT_BODY));
                if let Some(hint) = &item.hint {
                    content = content.push(
                        text(hint.clone())
                            .size(TEXT_CAPTION)
                            .style(style::text_hint),
                    );
                }
                button(content)
                    .width(Length::Fill)
                    .padding([6, 8])
                    .style(style::button_menu_item(item.danger))
                    .on_press_maybe((!item.disabled).then(|| item.on_select.clone()))
                    .into()
            }
        }
    });
    container(MenuNav {
        content: column(rows).spacing(0).into(),
        actions,
    })
    .padding(SPACE_1)
    .width(menu_width(entries))
    .style(style::container_popover)
    .into()
}

/// Keyboard navigation over the rows of a menu column: `actions[i]` is what
/// row `i` publishes (`None`: header, separator, disabled item).
struct MenuNav<'a, Message> {
    content: Element<'a, Message>,
    actions: Vec<Option<Message>>,
}

#[derive(Default)]
struct NavState {
    highlighted: Option<usize>,
}

impl<Message> MenuNav<'_, Message> {
    /// Next selectable row from `from` in direction `step` (wrapping).
    fn next(&self, from: Option<usize>, step: isize) -> Option<usize> {
        let n = self.actions.len() as isize;
        if n == 0 {
            return None;
        }
        let mut i = match from {
            Some(i) => i as isize,
            None if step > 0 => -1,
            None => n,
        };
        for _ in 0..n {
            i = (i + step).rem_euclid(n);
            if self.actions[i as usize].is_some() {
                return Some(i as usize);
            }
        }
        None
    }
}

impl<Message: Clone> Widget<Message, Theme, iced::Renderer> for MenuNav<'_, Message> {
    fn size(&self) -> Size<Length> {
        self.content.as_widget().size()
    }

    fn tag(&self) -> tree::Tag {
        tree::Tag::of::<NavState>()
    }

    fn state(&self) -> tree::State {
        tree::State::new(NavState::default())
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
        cursor: mouse::Cursor,
        viewport: &Rectangle,
    ) {
        use iced::advanced::Renderer as _;
        let state = tree.state.downcast_ref::<NavState>();
        if let Some(row) = state.highlighted.and_then(|i| layout.children().nth(i)) {
            renderer.fill_quad(
                renderer::Quad {
                    bounds: row.bounds(),
                    border: border::rounded(RADIUS_SM),
                    ..renderer::Quad::default()
                },
                tokens(theme).hover_bg,
            );
        }
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
        let highlighted = tree.state.downcast_ref::<NavState>().highlighted;
        match event {
            Event::Keyboard(keyboard::Event::KeyPressed {
                key: keyboard::Key::Named(named),
                modifiers,
                ..
            }) if !modifiers.command() && !modifiers.alt() => {
                use keyboard::key::Named;
                let moved = match named {
                    Named::ArrowDown => Some(self.next(highlighted, 1)),
                    Named::ArrowUp => Some(self.next(highlighted, -1)),
                    Named::Home => Some(self.next(None, 1)),
                    Named::End => Some(self.next(None, -1)),
                    _ => None,
                };
                if let Some(to) = moved {
                    tree.state.downcast_mut::<NavState>().highlighted = to;
                    shell.capture_event();
                    shell.request_redraw();
                    return;
                }
                if matches!(named, Named::Enter | Named::Space)
                    && let Some(Some(msg)) = highlighted.and_then(|i| self.actions.get(i))
                {
                    shell.publish(msg.clone());
                    shell.capture_event();
                    return;
                }
            }
            // The mouse takes over from the keyboard highlight.
            Event::Mouse(mouse::Event::CursorMoved { .. })
                if highlighted.is_some() && cursor.is_over(layout.bounds()) =>
            {
                tree.state.downcast_mut::<NavState>().highlighted = None;
                shell.request_redraw();
            }
            _ => {}
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
    }

    fn mouse_interaction(
        &self,
        tree: &Tree,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
        renderer: &iced::Renderer,
    ) -> mouse::Interaction {
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

impl<'a, Message: Clone + 'a> From<MenuNav<'a, Message>> for Element<'a, Message> {
    fn from(n: MenuNav<'a, Message>) -> Self {
        Element::new(n)
    }
}

/// Opens `entries` at the cursor when `underlay` is right-clicked. Selecting an
/// item (mouse or keyboard), clicking elsewhere, Escape or a window resize closes
/// it (state lives in the widget).
pub fn context_menu<'a, Message: Clone + 'a>(
    underlay: impl Into<Element<'a, Message>>,
    entries: Vec<MenuEntry<Message>>,
) -> Element<'a, Message> {
    Element::new(ContextMenu {
        underlay: underlay.into(),
        entries,
        panel: None,
    })
}

struct ContextMenu<'a, Message> {
    underlay: Element<'a, Message>,
    entries: Vec<MenuEntry<Message>>,
    /// Built only while the menu is open.
    panel: Option<Element<'a, Message>>,
}

#[derive(Default)]
struct ContextState {
    open: bool,
    at: Point,
}

impl<'a, Message: Clone + 'a> ContextMenu<'a, Message> {
    /// Builds the panel and syncs its tree (children[1]) when the menu is open.
    fn sync_panel(&mut self, tree: &mut Tree) -> bool {
        if !tree.state.downcast_ref::<ContextState>().open {
            self.panel = None;
            return false;
        }
        let entries = &self.entries;
        let panel = self.panel.get_or_insert_with(|| menu_panel(entries));
        tree.children[1].diff(&*panel);
        true
    }
}

impl<'a, Message: Clone + 'a> Widget<Message, Theme, iced::Renderer> for ContextMenu<'a, Message> {
    fn size(&self) -> Size<Length> {
        self.underlay.as_widget().size()
    }

    fn size_hint(&self) -> Size<Length> {
        self.underlay.as_widget().size_hint()
    }

    fn tag(&self) -> tree::Tag {
        tree::Tag::of::<ContextState>()
    }

    fn state(&self) -> tree::State {
        tree::State::new(ContextState::default())
    }

    fn children(&self) -> Vec<Tree> {
        vec![Tree::new(&self.underlay), Tree::empty()]
    }

    fn diff(&self, tree: &mut Tree) {
        tree.children[0].diff(&self.underlay);
    }

    fn layout(
        &mut self,
        tree: &mut Tree,
        renderer: &iced::Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        self.underlay
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
        self.underlay.as_widget().draw(
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
        self.underlay
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
        if matches!(
            event,
            Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Right))
        ) && let Some(at) = cursor.position_over(layout.bounds())
        {
            let state = tree.state.downcast_mut::<ContextState>();
            state.open = true;
            state.at = at;
            // A fresh menu: no highlight left from the last time.
            tree.children[1] = Tree::empty();
            self.panel = None;
            shell.capture_event();
            shell.request_redraw();
            return;
        }
        self.underlay.as_widget_mut().update(
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
        self.underlay.as_widget().mouse_interaction(
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
        if !self.sync_panel(tree) {
            return self.underlay.as_widget_mut().overlay(
                &mut tree.children[0],
                layout,
                renderer,
                viewport,
                translation,
            );
        }
        let (state, children) = (&mut tree.state, &mut tree.children);
        let state = state.downcast_mut::<ContextState>();
        let at = state.at + translation;
        let panel = self.panel.as_mut()?;
        Some(overlay::Element::new(Box::new(ContextOverlay {
            tree: &mut children[1],
            panel,
            state,
            at,
        })))
    }
}

struct ContextOverlay<'a, 'b, Message> {
    tree: &'b mut Tree,
    panel: &'b mut Element<'a, Message>,
    state: &'b mut ContextState,
    at: Point,
}

impl<Message: Clone> overlay::Overlay<Message, Theme, iced::Renderer>
    for ContextOverlay<'_, '_, Message>
{
    fn layout(&mut self, renderer: &iced::Renderer, bounds: Size) -> layout::Node {
        let limits = layout::Limits::new(Size::ZERO, bounds);
        let node = self
            .panel
            .as_widget_mut()
            .layout(self.tree, renderer, &limits);
        let size = node.size();
        // Open towards the free side, like native menus.
        let mut at = self.at;
        if at.x + size.width > bounds.width {
            at.x = (at.x - size.width).max(0.0);
        }
        if at.y + size.height > bounds.height {
            at.y = (at.y - size.height).max(0.0);
        }
        layout::Node::with_children(bounds, vec![node.move_to(at)])
    }

    fn draw(
        &self,
        renderer: &mut iced::Renderer,
        theme: &Theme,
        style: &renderer::Style,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
    ) {
        let Some(panel) = layout.children().next() else {
            return;
        };
        let bounds = panel.bounds();
        self.panel
            .as_widget()
            .draw(self.tree, renderer, theme, style, panel, cursor, &bounds);
    }

    fn operate(
        &mut self,
        layout: Layout<'_>,
        renderer: &iced::Renderer,
        operation: &mut dyn Operation,
    ) {
        if let Some(panel) = layout.children().next() {
            self.panel
                .as_widget_mut()
                .operate(self.tree, panel, renderer, operation);
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
        let Some(panel) = layout.children().next() else {
            return;
        };
        let bounds = panel.bounds();
        let close = |state: &mut ContextState, shell: &mut Shell<'_, Message>| {
            state.open = false;
            shell.request_redraw();
        };
        match event {
            Event::Keyboard(keyboard::Event::KeyPressed {
                key: keyboard::Key::Named(keyboard::key::Named::Escape),
                ..
            }) => {
                close(self.state, shell);
                shell.capture_event();
                return;
            }
            Event::Window(iced::window::Event::Resized { .. }) => {
                close(self.state, shell);
                return;
            }
            Event::Mouse(mouse::Event::ButtonPressed(_))
            | Event::Touch(iced::touch::Event::FingerPressed { .. })
                if !cursor.is_over(bounds) =>
            {
                close(self.state, shell);
                shell.capture_event();
                return;
            }
            _ => {}
        }
        let captured = shell.is_event_captured();
        self.panel.as_widget_mut().update(
            self.tree, event, panel, cursor, renderer, clipboard, shell, &bounds,
        );
        match event {
            // Item buttons publish on release; presses outside closed it already.
            Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left))
            | Event::Touch(iced::touch::Event::FingerLifted { .. }) => {
                close(self.state, shell);
            }
            // Enter/Space on a highlighted item: MenuNav published and captured.
            Event::Keyboard(keyboard::Event::KeyPressed {
                key: keyboard::Key::Named(keyboard::key::Named::Enter | keyboard::key::Named::Space),
                ..
            }) if !captured && shell.is_event_captured() => {
                close(self.state, shell);
            }
            _ => {}
        }
        // The menu is modal: nothing underneath reacts to presses or the wheel.
        if matches!(
            event,
            Event::Mouse(mouse::Event::ButtonPressed(_) | mouse::Event::WheelScrolled { .. })
                | Event::Touch(iced::touch::Event::FingerPressed { .. })
        ) {
            shell.capture_event();
        }
    }

    fn mouse_interaction(
        &self,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        renderer: &iced::Renderer,
    ) -> mouse::Interaction {
        let Some(panel) = layout.children().next() else {
            return mouse::Interaction::None;
        };
        let bounds = panel.bounds();
        self.panel
            .as_widget()
            .mouse_interaction(self.tree, panel, cursor, &bounds, renderer)
            .max(mouse::Interaction::Idle)
    }

    fn overlay<'c>(
        &'c mut self,
        layout: Layout<'c>,
        renderer: &iced::Renderer,
    ) -> Option<overlay::Element<'c, Message, Theme, iced::Renderer>> {
        let panel = layout.children().next()?;
        let bounds = panel.bounds();
        self.panel
            .as_widget_mut()
            .overlay(self.tree, panel, renderer, &bounds, Vector::ZERO)
    }
}

/// Shows `entries` anchored to `trigger` while `open` is true; clicking outside or
/// pressing Escape publishes `on_dismiss`. Selecting an item should also close it
/// (the caller resets `open` in its update).
pub fn popover_menu<'a, Message: Clone + 'a>(
    trigger: impl Into<Element<'a, Message>>,
    entries: &[MenuEntry<Message>],
    open: bool,
    on_dismiss: Message,
    placement: Placement,
) -> Element<'a, Message> {
    // Closed menus are not measured or built.
    let panel = if open {
        menu_panel(entries)
    } else {
        space().into()
    };
    popover(trigger, panel, open, placement, on_dismiss)
}
