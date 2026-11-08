//! The selection scope: one selection over every [`Rich`](super::Rich) block
//! inside it, in document (traversal) order.

use std::ops::Range;

use iced::advanced::clipboard;
use iced::advanced::layout::{self, Layout};
use iced::advanced::mouse::click::{self, Click};
use iced::advanced::overlay;
use iced::advanced::renderer;
use iced::advanced::widget::{Operation, Tree, Widget, tree};
use iced::advanced::{Clipboard, Shell};
use iced::keyboard::{self, Modifiers};
use iced::window::RedrawRequest;
use iced::{Element, Event, Length, Point, Rectangle, Size, Theme, Vector, mouse};

use super::DRAG_THRESHOLD;
use super::core::{Core, clamp_range, word_at};
use crate::icon::Icon;
use crate::widget::menu::{MenuEntry, MenuItem, menu_panel};

/// A position in the scope: block index (traversal order) and byte offset.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(super) struct Pos {
    pub leaf: usize,
    pub offset: usize,
}

impl Pos {
    const fn new(leaf: usize, offset: usize) -> Self {
        Self { leaf, offset }
    }
}

#[derive(Debug, Clone, Copy)]
enum MenuMsg {
    Copy,
}

/// Wraps `content` (e.g. a message body) so the text blocks in it share one
/// selection (see the module docs).
pub struct Scope<'a, Message> {
    content: Element<'a, Message, Theme, iced::Renderer>,
    copy_label: String,
    /// The Copy menu, built only while it is open.
    menu: Option<Element<'a, MenuMsg, Theme, iced::Renderer>>,
}

/// A selection scope around `content`.
pub fn scope<'a, Message>(
    content: impl Into<Element<'a, Message, Theme, iced::Renderer>>,
) -> Scope<'a, Message> {
    Scope {
        content: content.into(),
        copy_label: "Copy".to_owned(),
        menu: None,
    }
}

impl<Message> Scope<'_, Message> {
    /// Label of the right-click Copy entry (translated by the caller).
    pub fn copy_label(mut self, label: impl Into<String>) -> Self {
        self.copy_label = label.into();
        self
    }
}

#[derive(Default)]
pub(super) struct State {
    anchor: Option<Pos>,
    focus: Option<Pos>,
    /// Press position while the left button is down after a single click.
    drag: Option<Point>,
    moved: bool,
    last_click: Option<Click>,
    /// The last press was inside: keyboard shortcuts act on this scope.
    focused: bool,
    modifiers: Modifiers,
    /// Open Copy menu: where, and the text it copies.
    menu: Option<(Point, String)>,
}

impl State {
    /// Selects everything (the blocks are set by the caller).
    pub(super) fn select_all(&mut self) {
        self.anchor = Some(Pos::new(0, 0));
        self.focus = Some(Pos::new(usize::MAX, usize::MAX));
    }

    fn range(&self) -> Option<(Pos, Pos)> {
        let (a, f) = (self.anchor?, self.focus?);
        (a != f).then(|| (a.min(f), a.max(f)))
    }

    fn clear(&mut self) -> bool {
        let had = self.range().is_some();
        self.anchor = None;
        self.focus = None;
        self.drag = None;
        had
    }
}

/// Runs `op` over the blocks of `content`.
fn run<Message>(
    content: &mut Element<'_, Message, Theme, iced::Renderer>,
    tree: &mut Tree,
    layout: Layout<'_>,
    renderer: &iced::Renderer,
    op: &mut dyn Operation,
) {
    content.as_widget_mut().operate(tree, layout, renderer, op);
}

/// Calls `f(index, core)` for every block, in traversal order.
struct Each<F: FnMut(usize, &mut Core) + Send> {
    f: F,
    next: usize,
}

impl<F: FnMut(usize, &mut Core) + Send> Operation for Each<F> {
    fn traverse(&mut self, operate: &mut dyn FnMut(&mut dyn Operation)) {
        operate(self);
    }

    fn custom(
        &mut self,
        _id: Option<&iced::advanced::widget::Id>,
        _: Rectangle,
        state: &mut dyn std::any::Any,
    ) {
        if let Some(core) = state.downcast_mut::<Core>() {
            (self.f)(self.next, core);
            self.next += 1;
        }
    }
}

fn each<F: FnMut(usize, &mut Core) + Send>(f: F) -> Each<F> {
    Each { f, next: 0 }
}

/// Where the cursor is, from what each block recorded during this event.
/// `exact`: only a point inside a block; otherwise the nearest position in
/// document order (dragging past the blocks).
struct Locate {
    exact: bool,
    found: Option<Pos>,
    candidate: Option<Pos>,
}

impl Locate {
    fn visit(&mut self, i: usize, core: &Core) {
        if self.found.is_some() {
            return;
        }
        let Some(p) = core.cursor else {
            return;
        };
        let b = p + core.origin;
        if core.contains(b) {
            self.found = Some(Pos::new(i, core.hit(p)));
            return;
        }
        if self.exact {
            return;
        }
        if b.y < 0.0 {
            // Above this block, below the previous one: end of the previous.
            self.found = Some(self.candidate.unwrap_or(Pos::new(i, 0)));
        } else if b.y <= core.size.height {
            // Beside the block (left or right of a line).
            self.candidate = Some(Pos::new(i, core.hit(p)));
        } else {
            self.candidate = Some(Pos::new(i, core.len()));
        }
    }

    fn result(&self) -> Option<Pos> {
        self.found.or(self.candidate)
    }
}

impl<Message> Scope<'_, Message> {
    fn locate(
        &mut self,
        tree: &mut Tree,
        layout: Layout<'_>,
        renderer: &iced::Renderer,
        exact: bool,
    ) -> Option<Pos> {
        let mut loc = Locate {
            exact,
            found: None,
            candidate: None,
        };
        let mut op = each(|i, core: &mut Core| loc.visit(i, core));
        run(&mut self.content, tree, layout, renderer, &mut op);
        loc.result()
    }

    /// Gives every block its part of the selection.
    fn apply(
        &mut self,
        tree: &mut Tree,
        layout: Layout<'_>,
        renderer: &iced::Renderer,
        range: Option<(Pos, Pos)>,
    ) {
        let mut op = each(|i, core: &mut Core| {
            core.scoped = true;
            core.selection = range.and_then(|(s, e)| {
                if i < s.leaf || i > e.leaf {
                    return None;
                }
                let a = if i == s.leaf { s.offset } else { 0 };
                let b = if i == e.leaf { e.offset } else { core.len() };
                let r = clamp_range(&core.text, a..b);
                (!r.is_empty()).then_some(r)
            });
        });
        run(&mut self.content, tree, layout, renderer, &mut op);
    }

    /// The selected text of all blocks (see [`copy_text`]).
    fn selected_text(
        &mut self,
        tree: &mut Tree,
        layout: Layout<'_>,
        renderer: &iced::Renderer,
    ) -> String {
        let mut out = String::new();
        let mut op = each(|_, core: &mut Core| copy_piece(&mut out, core));
        run(&mut self.content, tree, layout, renderer, &mut op);
        out
    }

    /// The text of block `leaf` (`None` when there is no such block).
    fn block_range(
        &mut self,
        tree: &mut Tree,
        layout: Layout<'_>,
        renderer: &iced::Renderer,
        at: Pos,
        word: bool,
    ) -> Option<Range<usize>> {
        let mut out = None;
        let mut op = each(|i, core: &mut Core| {
            if i == at.leaf {
                out = Some(if word {
                    word_at(&core.text, at.offset)
                } else {
                    0..core.len()
                });
            }
        });
        run(&mut self.content, tree, layout, renderer, &mut op);
        out
    }
}

/// Appends the selected part of `core` to `out`: the block's joiner between
/// blocks, its copy prefix when the selection starts at the block start.
pub(super) fn copy_piece(out: &mut String, core: &Core) {
    let Some(sel) = &core.selection else {
        return;
    };
    let piece = core.selected();
    if piece.is_empty() {
        return;
    }
    if !out.is_empty() {
        out.push_str(core.joiner);
    }
    if sel.start == 0
        && let Some(prefix) = &core.prefix
    {
        out.push_str(prefix);
    }
    out.push_str(piece);
}

impl<'a, Message: 'a> Widget<Message, Theme, iced::Renderer> for Scope<'a, Message> {
    fn tag(&self) -> tree::Tag {
        tree::Tag::of::<State>()
    }

    fn state(&self) -> tree::State {
        tree::State::new(State::default())
    }

    fn children(&self) -> Vec<Tree> {
        vec![Tree::new(&self.content), Tree::empty()]
    }

    fn diff(&self, tree: &mut Tree) {
        tree.children[0].diff(&self.content);
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
        let node = self
            .content
            .as_widget_mut()
            .layout(&mut tree.children[0], renderer, limits);
        // Blocks inside a scope show the text cursor.
        let mut op = each(|_, core: &mut Core| core.scoped = true);
        run(
            &mut self.content,
            &mut tree.children[0],
            Layout::new(&node),
            renderer,
            &mut op,
        );
        node
    }

    fn operate(
        &mut self,
        tree: &mut Tree,
        layout: Layout<'_>,
        renderer: &iced::Renderer,
        operation: &mut dyn Operation,
    ) {
        // Before the blocks: public operations group blocks by scope.
        operation.custom(None, layout.bounds(), tree.state.downcast_mut::<State>());
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
        if let Event::Keyboard(keyboard::Event::ModifiersChanged(m)) = event {
            tree.state.downcast_mut::<State>().modifiers = *m;
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
        let captured = shell.is_event_captured();
        let (state, children) = (&mut tree.state, &mut tree.children);
        let state = state.downcast_mut::<State>();
        let content = &mut children[0];
        let over = cursor.position_over(layout.bounds());
        let at = cursor.land().position();
        match event {
            Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)) => {
                let Some(p) = over else {
                    state.focused = false;
                    state.last_click = None;
                    if state.clear() {
                        self.apply(content, layout, renderer, None);
                        shell.request_redraw();
                    }
                    return;
                };
                if captured {
                    return;
                }
                state.focused = true;
                let click = Click::new(p, mouse::Button::Left, state.last_click);
                state.last_click = Some(click);
                let hit = self.locate(content, layout, renderer, true);
                let had = state.range().is_some();
                match (hit, click.kind()) {
                    (None, _) => {
                        state.clear();
                    }
                    (Some(h), click::Kind::Single) => {
                        if state.modifiers.shift() && state.anchor.is_some() {
                            state.focus = Some(h);
                            state.moved = true;
                        } else {
                            state.anchor = Some(h);
                            state.focus = Some(h);
                            state.moved = false;
                        }
                        state.drag = Some(p);
                    }
                    (Some(h), kind) => {
                        let word = kind == click::Kind::Double;
                        if let Some(r) = self.block_range(content, layout, renderer, h, word) {
                            state.anchor = Some(Pos::new(h.leaf, r.start));
                            state.focus = Some(Pos::new(h.leaf, r.end));
                        }
                        state.drag = None;
                    }
                }
                if had || state.range().is_some() {
                    self.apply(content, layout, renderer, state.range());
                    shell.request_redraw();
                }
            }
            Event::Mouse(mouse::Event::CursorMoved { .. }) => {
                let (Some(start), Some(p)) = (state.drag, at) else {
                    return;
                };
                if !state.moved && start.distance(p) <= DRAG_THRESHOLD {
                    return;
                }
                state.moved = true;
                if let Some(h) = self.locate(content, layout, renderer, false)
                    && state.focus != Some(h)
                {
                    state.focus = Some(h);
                    self.apply(content, layout, renderer, state.range());
                    shell.request_redraw();
                }
            }
            Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)) => {
                if state.drag.take().is_some() && state.moved {
                    // A drag is not a click on what is underneath.
                    shell.capture_event();
                }
            }
            Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Right)) => {
                let Some(p) = over else {
                    return;
                };
                if captured || state.range().is_none() {
                    return;
                }
                let text = self.selected_text(content, layout, renderer);
                if text.is_empty() {
                    return;
                }
                state.menu = Some((p, text));
                children[1] = Tree::empty();
                self.menu = None;
                shell.capture_event();
                shell.request_redraw();
            }
            Event::Keyboard(keyboard::Event::KeyPressed {
                key,
                physical_key,
                modifiers,
                ..
            }) if state.focused && !captured => {
                let named = |n| *key == keyboard::Key::Named(n);
                match key.to_latin(*physical_key) {
                    Some('a') if modifiers.command() => {
                        state.select_all();
                        self.apply(content, layout, renderer, state.range());
                        shell.capture_event();
                        shell.request_redraw();
                    }
                    Some('c') if modifiers.command() => {
                        let text = self.selected_text(content, layout, renderer);
                        if !text.is_empty() {
                            clipboard.write(clipboard::Kind::Standard, text);
                            shell.capture_event();
                        }
                    }
                    _ if named(keyboard::key::Named::Escape) && state.clear() => {
                        self.apply(content, layout, renderer, None);
                        shell.capture_event();
                        shell.request_redraw();
                    }
                    _ => {}
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
        self.content.as_widget().mouse_interaction(
            &tree.children[0],
            layout,
            cursor,
            viewport,
            renderer,
        )
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
        let open = tree.state.downcast_ref::<State>().menu.is_some();
        if !open {
            self.menu = None;
            return self.content.as_widget_mut().overlay(
                &mut tree.children[0],
                layout,
                renderer,
                viewport,
                translation,
            );
        }
        let label = self.copy_label.clone();
        let panel = self.menu.get_or_insert_with(|| {
            menu_panel(&[MenuEntry::Item(
                MenuItem::new(label, MenuMsg::Copy)
                    .icon(Icon::Copy)
                    .hint("Ctrl+C"),
            )])
        });
        let (state, children) = (&mut tree.state, &mut tree.children);
        children[1].diff(&*panel);
        let state = state.downcast_mut::<State>();
        let at = state.menu.as_ref()?.0 + translation;
        Some(overlay::Element::new(Box::new(CopyMenu {
            tree: &mut children[1],
            panel,
            state,
            at,
        })))
    }
}

struct CopyMenu<'a, 'b> {
    tree: &'b mut Tree,
    panel: &'b mut Element<'a, MenuMsg, Theme, iced::Renderer>,
    state: &'b mut State,
    at: Point,
}

impl<Message> overlay::Overlay<Message, Theme, iced::Renderer> for CopyMenu<'_, '_> {
    fn layout(&mut self, renderer: &iced::Renderer, bounds: Size) -> layout::Node {
        let limits = layout::Limits::new(Size::ZERO, bounds);
        let node = self
            .panel
            .as_widget_mut()
            .layout(self.tree, renderer, &limits);
        let size = node.size();
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
        if let Some(panel) = layout.children().next() {
            let bounds = panel.bounds();
            self.panel
                .as_widget()
                .draw(self.tree, renderer, theme, style, panel, cursor, &bounds);
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
        match event {
            Event::Keyboard(keyboard::Event::KeyPressed {
                key: keyboard::Key::Named(keyboard::key::Named::Escape),
                ..
            }) => {
                self.state.menu = None;
                shell.capture_event();
                shell.request_redraw();
                return;
            }
            Event::Window(iced::window::Event::Resized { .. }) => {
                self.state.menu = None;
                shell.request_redraw();
                return;
            }
            Event::Mouse(mouse::Event::ButtonPressed(_)) if !cursor.is_over(bounds) => {
                self.state.menu = None;
                shell.capture_event();
                shell.request_redraw();
                return;
            }
            _ => {}
        }
        let mut msgs = Vec::new();
        let mut local = Shell::new(&mut msgs);
        self.panel.as_widget_mut().update(
            self.tree, event, panel, cursor, renderer, clipboard, &mut local, &bounds,
        );
        let captured = local.is_event_captured();
        let redraw = !matches!(local.redraw_request(), RedrawRequest::Wait);
        let relayout = local.is_layout_invalid();
        drop(local);
        if captured {
            shell.capture_event();
        }
        if redraw {
            shell.request_redraw();
        }
        if relayout {
            shell.invalidate_layout();
        }
        if msgs.iter().any(|m| matches!(m, MenuMsg::Copy))
            && let Some((_, text)) = self.state.menu.take()
        {
            clipboard.write(clipboard::Kind::Standard, text);
            shell.request_redraw();
        }
        if matches!(
            event,
            Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left))
        ) {
            self.state.menu = None;
            shell.request_redraw();
        }
        // Modal: nothing underneath reacts to presses or the wheel.
        if matches!(
            event,
            Event::Mouse(mouse::Event::ButtonPressed(_) | mouse::Event::WheelScrolled { .. })
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
}

impl<'a, Message: 'a> From<Scope<'a, Message>> for Element<'a, Message, Theme, iced::Renderer> {
    fn from(s: Scope<'a, Message>) -> Self {
        Element::new(s)
    }
}
