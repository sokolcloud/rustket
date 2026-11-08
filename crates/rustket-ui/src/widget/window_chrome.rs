//! Client-side window decorations: the title-bar row and its workspace
//! items, window control buttons, and the frame with invisible resize edges
//! around an undecorated window.

use iced::advanced::layout::{self, Layout};
use iced::advanced::overlay;
use iced::advanced::renderer;
use iced::advanced::widget::{Operation, Tree, Widget, tree};
use iced::advanced::{Clipboard, Shell};
use iced::widget::{button, column, container, mouse_area, row, space, stack};
use iced::window::Direction;
use iced::{
    Alignment, Element, Event, Length, Rectangle, Size, Theme, Vector, border, mouse, touch,
};

use super::avatar::{AvatarShape, AvatarSource, face};
use super::badge::{BadgeKind, badge, dot};
use super::focus_ring::focus_ring;
use super::misc::{TooltipPosition, tooltip};
use crate::icon::{self, Icon};
use crate::theme::{RADIUS_MD, RADIUS_PILL, tokens};

/// Height of the custom title bar.
pub const TITLE_BAR_HEIGHT: f32 = 40.0;
/// Width of one workspace item in the title bar.
pub const TITLE_BAR_ITEM: f32 = 38.0;
/// Width of one window control button.
pub const WINDOW_CONTROL_WIDTH: f32 = 44.0;
/// Thickness of the invisible resize edges of an undecorated window.
pub const RESIZE_EDGE: f32 = 5.0;
/// Length of the corner grips along each edge.
const RESIZE_CORNER: f32 = 14.0;
const FACE: f32 = 26.0;

fn item_style(selected: bool) -> impl Fn(&Theme, button::Status) -> button::Style {
    move |theme, status| {
        let t = tokens(theme);
        let hovered = matches!(status, button::Status::Hovered | button::Status::Pressed);
        button::Style {
            background: (hovered && !selected).then(|| t.hover_bg.into()),
            text_color: t.text_primary,
            border: border::rounded(RADIUS_MD),
            ..button::Style::default()
        }
    }
}

/// A workspace (server) in the title bar: face, selection ring and bar,
/// unread badge, signed-out emblem and a tooltip with `name`. `on_press`
/// `None` renders it inert (a modal covers the window). The tree has the
/// same shape for every state.
pub fn title_bar_workspace<'a, Message: Clone + 'a>(
    source: &AvatarSource,
    name: impl Into<String>,
    unread: Option<(u32, BadgeKind)>,
    selected: bool,
    signed_out: bool,
    on_press: Option<Message>,
) -> Element<'a, Message> {
    let ringed = container(face(source, FACE, AvatarShape::Rounded))
        .padding(2)
        .style(move |theme: &Theme| {
            let t = tokens(theme);
            container::Style {
                border: border::rounded(RADIUS_MD + 2.0)
                    .width(2)
                    .color(if selected {
                        t.primary
                    } else {
                        iced::Color::TRANSPARENT
                    }),
                ..container::Style::default()
            }
        });
    let badge_el: Element<'a, Message> = match unread {
        Some((0, kind)) => dot(kind, 9.0),
        Some((count, kind)) => badge(count, kind),
        None => space().into(),
    };
    let emblem: Element<'a, Message> = if signed_out {
        container(icon::sized(Icon::LogOut, 8.0))
            .center(13)
            .style(|theme: &Theme| {
                let t = tokens(theme);
                container::Style {
                    text_color: Some(t.on_danger),
                    background: Some(t.danger.into()),
                    border: border::rounded(7).width(1.5).color(t.surface_rail),
                    ..container::Style::default()
                }
            })
            .into()
    } else {
        space().into()
    };
    let bar = container(space().width(16).height(3)).style(move |theme: &Theme| {
        let t = tokens(theme);
        container::Style {
            background: selected.then(|| t.primary.into()),
            border: border::rounded(
                border::Radius::default()
                    .top_left(RADIUS_PILL)
                    .top_right(RADIUS_PILL),
            ),
            ..container::Style::default()
        }
    });
    let cell = |content: Element<'a, Message>, x: Alignment, y: Alignment, pad: iced::Padding| {
        container(content)
            .width(Length::Fill)
            .height(Length::Fill)
            .align_x(x)
            .align_y(y)
            .padding(pad)
    };
    let layers = stack![
        cell(
            ringed.into(),
            Alignment::Center,
            Alignment::Center,
            0.into()
        ),
        cell(
            badge_el,
            Alignment::End,
            Alignment::Start,
            iced::Padding::ZERO.top(2.0)
        ),
        cell(
            emblem,
            Alignment::End,
            Alignment::End,
            iced::Padding::ZERO.right(4.0).bottom(4.0)
        ),
        cell(bar.into(), Alignment::Center, Alignment::End, 0.into()),
    ]
    .width(TITLE_BAR_ITEM)
    .height(TITLE_BAR_HEIGHT);
    let b = button(layers)
        .padding(0)
        .style(item_style(selected))
        .on_press_maybe(on_press.clone());
    let b = focus_ring(b, on_press).radius(RADIUS_MD + 2.0);
    tooltip(b, name, TooltipPosition::Bottom)
}

/// A square icon button sized for the title bar (add workspace, settings).
pub fn title_bar_button<'a, Message: Clone + 'a>(
    glyph: Icon,
    label: impl Into<String>,
    active: bool,
    on_press: Option<Message>,
) -> Element<'a, Message> {
    let b = button(container(icon::sized(glyph, 18.0)).center(Length::Fill))
        .width(32)
        .height(32)
        .padding(0)
        .style(move |theme: &Theme, status| {
            let t = tokens(theme);
            let hovered = matches!(status, button::Status::Hovered | button::Status::Pressed);
            button::Style {
                background: if active {
                    Some(t.selected_bg.into())
                } else {
                    hovered.then(|| t.hover_bg.into())
                },
                text_color: match status {
                    button::Status::Disabled => t.text_disabled,
                    _ if active || hovered => t.text_primary,
                    _ => t.text_secondary,
                },
                border: border::rounded(RADIUS_MD),
                ..button::Style::default()
            }
        })
        .on_press_maybe(on_press.clone());
    let b = focus_ring(b, on_press).radius(RADIUS_MD);
    tooltip(b, label, TooltipPosition::Bottom)
}

/// Labels of the window control buttons (tooltips and accessibility).
pub struct WindowControlLabels {
    pub minimize: String,
    pub maximize: String,
    pub restore: String,
    pub close: String,
}

fn control<'a, Message: Clone + 'a>(
    glyph: Icon,
    size: f32,
    danger: bool,
    label: String,
    on_press: Message,
) -> Element<'a, Message> {
    let b = button(container(icon::sized(glyph, size)).center(Length::Fill))
        .width(WINDOW_CONTROL_WIDTH)
        .height(TITLE_BAR_HEIGHT)
        .padding(0)
        .style(move |theme: &Theme, status| {
            let t = tokens(theme);
            let (bg, fg) = match status {
                button::Status::Hovered if danger => (Some(t.danger), t.on_danger),
                button::Status::Pressed if danger => (Some(t.danger_pressed), t.on_danger),
                button::Status::Hovered => (Some(t.hover_bg), t.text_primary),
                button::Status::Pressed => (Some(t.pressed_bg), t.text_primary),
                _ => (None, t.text_secondary),
            };
            button::Style {
                background: bg.map(Into::into),
                text_color: fg,
                ..button::Style::default()
            }
        })
        .on_press(on_press.clone());
    let b = focus_ring(b, Some(on_press)).radius(0.0);
    tooltip(b, label, TooltipPosition::Bottom)
}

/// Minimize, maximize/restore (the icon follows `maximized`) and close.
pub fn window_controls<'a, Message: Clone + 'a>(
    maximized: bool,
    labels: WindowControlLabels,
    on_minimize: Message,
    on_toggle_maximize: Message,
    on_close: Message,
) -> Element<'a, Message> {
    let (max_icon, max_label) = if maximized {
        (Icon::Copy, labels.restore)
    } else {
        (Icon::Square, labels.maximize)
    };
    row![
        control(Icon::Minus, 16.0, false, labels.minimize, on_minimize),
        control(max_icon, 13.0, false, max_label, on_toggle_maximize),
        control(Icon::X, 17.0, true, labels.close, on_close),
    ]
    .into()
}

/// The resize cursor shown over an edge or corner.
pub fn resize_cursor(direction: Direction) -> mouse::Interaction {
    match direction {
        Direction::North | Direction::South => mouse::Interaction::ResizingVertically,
        Direction::East | Direction::West => mouse::Interaction::ResizingHorizontally,
        Direction::NorthWest | Direction::SouthEast => mouse::Interaction::ResizingDiagonallyDown,
        Direction::NorthEast | Direction::SouthWest => mouse::Interaction::ResizingDiagonallyUp,
    }
}

/// An undecorated window's frame: `title` above `content` inside a 1 px
/// border (none while maximized), with invisible resize edges and corners
/// that report the pressed direction. While maximized the edges do nothing;
/// the tree keeps its shape either way.
///
/// The top edge and the corner grips lie above the title bar; the side and
/// bottom edges lie under `content`, so a widget there that takes the press
/// or shows a cursor (a scrollbar, a button) wins and the edge only gets
/// presses on inert areas. Modals in `content` cover `content` only: the
/// title bar and the edges keep working while one is open.
pub fn window_frame<'a, Message: Clone + 'a>(
    title: Element<'a, Message>,
    content: Element<'a, Message>,
    maximized: bool,
    on_resize: impl Fn(Direction) -> Message + 'a,
) -> Element<'a, Message> {
    let edge = |d: Direction, w: Length, h: Length| -> Element<'a, Message> {
        let area = mouse_area(space().width(w).height(h));
        if maximized {
            area.into()
        } else {
            area.on_press(on_resize(d))
                .interaction(resize_cursor(d))
                .into()
        }
    };
    let e = Length::Fixed(RESIZE_EDGE);
    let corner = Length::Fixed(RESIZE_CORNER);
    let side_corner = Length::Fixed(RESIZE_CORNER - RESIZE_EDGE);
    let top_edges = column![
        row![
            edge(Direction::NorthWest, corner, e),
            edge(Direction::North, Length::Fill, e),
            edge(Direction::NorthEast, corner, e),
        ],
        row![
            edge(Direction::NorthWest, e, side_corner),
            space().width(Length::Fill),
            edge(Direction::NorthEast, e, side_corner),
        ],
    ]
    .width(Length::Fill);
    let side = |mid: Direction, bottom: Direction| {
        column![
            space().width(e).height(Length::Fixed(RESIZE_CORNER)),
            edge(mid, e, Length::Fill),
            edge(bottom, e, side_corner),
        ]
        .height(Length::Fill)
    };
    let lower_edges = column![
        row![
            side(Direction::West, Direction::SouthWest),
            space().width(Length::Fill).height(Length::Fill),
            side(Direction::East, Direction::SouthEast),
        ]
        .height(Length::Fill),
        row![
            edge(Direction::SouthWest, corner, e),
            edge(Direction::South, Length::Fill, e),
            edge(Direction::SouthEast, corner, e),
        ],
    ];
    let framed = container(column![title, scope(content, Scope::Viewport)])
        .width(Length::Fill)
        .height(Length::Fill)
        .padding(if maximized { 0 } else { 1 })
        .style(move |theme: &Theme| {
            let t = tokens(theme);
            container::Style {
                background: Some(t.surface_room.into()),
                border: border::width(if maximized { 0 } else { 1 }).color(t.border_strong),
                ..container::Style::default()
            }
        });
    let frame = stack![lower_edges, framed, top_edges]
        .width(Length::Fill)
        .height(Length::Fill);
    scope(
        frame.into(),
        Scope::EdgeGuard(if maximized { 0.0 } else { RESIZE_EDGE }),
    )
}

/// What a [`ScopeWidget`] changes for the overlays of its content.
#[derive(Debug, Clone, Copy)]
enum Scope {
    /// Overlays get the widget's own bounds as their viewport: a modal
    /// covers this area, not the whole window.
    Viewport,
    /// Pointer input within this many px of the widget's edges (the resize
    /// edges) is left to the base: presses never reach the overlays there.
    EdgeGuard(f32),
}

fn scope<'a, Message: Clone + 'a>(
    content: Element<'a, Message>,
    scope: Scope,
) -> Element<'a, Message> {
    Element::new(ScopeWidget { content, scope })
}

/// Transparent wrapper: everything goes to `content`; only its overlays are
/// adjusted (see [`Scope`]).
struct ScopeWidget<'a, Message> {
    content: Element<'a, Message>,
    scope: Scope,
}

impl<'a, Message: Clone + 'a> Widget<Message, Theme, iced::Renderer> for ScopeWidget<'a, Message> {
    fn size(&self) -> Size<Length> {
        self.content.as_widget().size()
    }

    fn size_hint(&self) -> Size<Length> {
        self.content.as_widget().size_hint()
    }

    fn tag(&self) -> tree::Tag {
        self.content.as_widget().tag()
    }

    fn state(&self) -> tree::State {
        self.content.as_widget().state()
    }

    fn children(&self) -> Vec<Tree> {
        self.content.as_widget().children()
    }

    fn diff(&self, tree: &mut Tree) {
        self.content.as_widget().diff(tree);
    }

    fn layout(
        &mut self,
        tree: &mut Tree,
        renderer: &iced::Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        self.content.as_widget_mut().layout(tree, renderer, limits)
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
        self.content
            .as_widget()
            .draw(tree, renderer, theme, style, layout, cursor, viewport);
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
            .operate(tree, layout, renderer, operation);
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
        renderer: &iced::Renderer,
    ) -> mouse::Interaction {
        self.content
            .as_widget()
            .mouse_interaction(tree, layout, cursor, viewport, renderer)
    }

    fn overlay<'b>(
        &'b mut self,
        tree: &'b mut Tree,
        layout: Layout<'b>,
        renderer: &iced::Renderer,
        viewport: &Rectangle,
        translation: Vector,
    ) -> Option<overlay::Element<'b, Message, Theme, iced::Renderer>> {
        let bounds = layout.bounds() + translation;
        match self.scope {
            Scope::Viewport => {
                let area = bounds.intersection(viewport).unwrap_or(bounds);
                self.content
                    .as_widget_mut()
                    .overlay(tree, layout, renderer, &area, translation)
            }
            Scope::EdgeGuard(thickness) => {
                let inner = self.content.as_widget_mut().overlay(
                    tree,
                    layout,
                    renderer,
                    viewport,
                    translation,
                )?;
                if thickness <= 0.0 {
                    return Some(inner);
                }
                Some(overlay::Element::new(Box::new(EdgeFilter {
                    inner,
                    window: bounds,
                    thickness,
                })))
            }
        }
    }
}

/// Keeps overlays (an open modal) off the resize edges: presses there go to
/// the base, other pointer events reach the overlays without a cursor and
/// without capturing.
struct EdgeFilter<'b, Message> {
    inner: overlay::Element<'b, Message, Theme, iced::Renderer>,
    window: Rectangle,
    thickness: f32,
}

impl<Message> EdgeFilter<'_, Message> {
    fn on_edge(&self, cursor: mouse::Cursor) -> bool {
        let inside = self.window.shrink(self.thickness);
        cursor
            .position()
            .is_some_and(|p| self.window.contains(p) && !inside.contains(p))
    }
}

impl<Message> overlay::Overlay<Message, Theme, iced::Renderer> for EdgeFilter<'_, Message> {
    fn layout(&mut self, renderer: &iced::Renderer, bounds: Size) -> layout::Node {
        self.inner.as_overlay_mut().layout(renderer, bounds)
    }

    fn draw(
        &self,
        renderer: &mut iced::Renderer,
        theme: &Theme,
        style: &renderer::Style,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
    ) {
        let cursor = if self.on_edge(cursor) {
            mouse::Cursor::Unavailable
        } else {
            cursor
        };
        self.inner
            .as_overlay()
            .draw(renderer, theme, style, layout, cursor);
    }

    fn operate(
        &mut self,
        layout: Layout<'_>,
        renderer: &iced::Renderer,
        operation: &mut dyn Operation,
    ) {
        self.inner
            .as_overlay_mut()
            .operate(layout, renderer, operation);
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
        let pointer = matches!(event, Event::Mouse(_) | Event::Touch(_));
        if !pointer || !self.on_edge(cursor) {
            self.inner
                .as_overlay_mut()
                .update(event, layout, cursor, renderer, clipboard, shell);
            return;
        }
        if matches!(
            event,
            Event::Mouse(mouse::Event::ButtonPressed(_))
                | Event::Touch(touch::Event::FingerPressed { .. })
        ) {
            return;
        }
        // Moves and releases still end hovers and presses in the overlays.
        let mut messages = Vec::new();
        let mut inner_shell = Shell::new(&mut messages);
        self.inner.as_overlay_mut().update(
            event,
            layout,
            mouse::Cursor::Unavailable,
            renderer,
            clipboard,
            &mut inner_shell,
        );
        let layout_invalid = inner_shell.is_layout_invalid();
        let widgets_invalid = inner_shell.are_widgets_invalid();
        let redraw = inner_shell.redraw_request();
        drop(inner_shell);
        for m in messages {
            shell.publish(m);
        }
        if layout_invalid {
            shell.invalidate_layout();
        }
        if widgets_invalid {
            shell.invalidate_widgets();
        }
        shell.request_redraw_at(redraw);
    }

    fn mouse_interaction(
        &self,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        renderer: &iced::Renderer,
    ) -> mouse::Interaction {
        if self.on_edge(cursor) {
            mouse::Interaction::None
        } else {
            self.inner
                .as_overlay()
                .mouse_interaction(layout, cursor, renderer)
        }
    }

    fn overlay<'c>(
        &'c mut self,
        layout: Layout<'c>,
        renderer: &iced::Renderer,
    ) -> Option<overlay::Element<'c, Message, Theme, iced::Renderer>> {
        self.inner.as_overlay_mut().overlay(layout, renderer)
    }

    fn index(&self) -> f32 {
        self.inner.as_overlay().index()
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use iced::advanced::widget::Tree;
    use iced::{Point, Size};
    use iced_runtime::user_interface::{Cache, UserInterface};

    // Compared through `Debug`.
    #[allow(dead_code)]
    #[derive(Debug, Clone)]
    enum Msg {
        Resize(Direction),
        Title,
        Content,
        Bar,
        Dismiss,
    }

    fn renderer() -> iced::Renderer {
        iced::Renderer::Secondary(iced_tiny_skia::Renderer::new(
            crate::theme::DEFAULT_FONT,
            iced::Pixels(crate::theme::TEXT_BODY),
        ))
    }

    /// 400 x 300: a 40 px title, a 50 px button in the middle of the content
    /// and a 10 x 100 "scrollbar" button at the top right of the content.
    fn frame(maximized: bool, modal: bool) -> Element<'static, Msg> {
        let title = mouse_area(space().width(Length::Fill).height(40)).on_press(Msg::Title);
        let content = row![
            container(button(space().width(50).height(50)).on_press(Msg::Content))
                .center(Length::Fill),
            iced::widget::column![
                button(space().width(10).height(100)).on_press(Msg::Bar),
                space().height(Length::Fill),
            ],
        ];
        let dialog: Element<'static, Msg> = container(space().width(60).height(40)).into();
        let content = crate::widget::modal(content, modal.then_some(dialog), Some(Msg::Dismiss));
        window_frame(title.into(), content, maximized, Msg::Resize)
    }

    /// Messages (as `Debug` text: `Direction` has no `PartialEq`) and the
    /// cursor after a click at `at`.
    fn click(maximized: bool, modal: bool, at: Point) -> (Vec<String>, mouse::Interaction) {
        let mut r = renderer();
        let mut ui = UserInterface::build(
            frame(maximized, modal),
            Size::new(400.0, 300.0),
            Cache::default(),
            &mut r,
        );
        let cursor = mouse::Cursor::Available(at);
        let mut out = Vec::new();
        let events = [
            iced::Event::Mouse(mouse::Event::CursorMoved { position: at }),
            iced::Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)),
            iced::Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)),
        ];
        let (state, _) = ui.update(&events, cursor, &mut r, &mut NoClip, &mut out);
        let interaction = match state {
            iced_runtime::user_interface::State::Updated {
                mouse_interaction, ..
            } => mouse_interaction,
            iced_runtime::user_interface::State::Outdated => mouse::Interaction::None,
        };
        (out.iter().map(|m| format!("{m:?}")).collect(), interaction)
    }

    struct NoClip;
    impl iced::advanced::Clipboard for NoClip {
        fn read(&self, _: iced::advanced::clipboard::Kind) -> Option<String> {
            None
        }
        fn write(&mut self, _: iced::advanced::clipboard::Kind, _: String) {}
    }

    fn resize(d: Direction) -> Vec<String> {
        vec![format!("{:?}", Msg::Resize(d))]
    }

    #[test]
    fn edges_resize_on_inert_areas_only() {
        for modal in [false, true] {
            let cases = [
                (Point::new(200.0, 1.0), Direction::North),
                (Point::new(200.0, 298.0), Direction::South),
                (Point::new(1.0, 150.0), Direction::West),
                (Point::new(398.0, 250.0), Direction::East),
                (Point::new(2.0, 2.0), Direction::NorthWest),
                (Point::new(10.0, 2.0), Direction::NorthWest),
                (Point::new(2.0, 10.0), Direction::NorthWest),
                (Point::new(397.0, 2.0), Direction::NorthEast),
                (Point::new(2.0, 297.0), Direction::SouthWest),
                (Point::new(397.0, 297.0), Direction::SouthEast),
            ];
            for (at, d) in cases {
                let (out, cursor) = click(false, modal, at);
                assert_eq!(out, resize(d), "{at:?}, modal {modal}");
                assert_eq!(cursor, resize_cursor(d), "{at:?}, modal {modal}");
            }
            // The title bar below the top edge, also under a modal.
            let (out, _) = click(false, modal, Point::new(200.0, 20.0));
            assert_eq!(out, vec!["Title"], "modal {modal}");
            let (out, _) = click(false, modal, Point::new(1.0, 20.0));
            assert_eq!(out, vec!["Title"], "modal {modal}");
        }
        // Content that takes the press wins over the edge under it.
        let (out, _) = click(false, false, Point::new(398.0, 100.0));
        assert_eq!(out, vec!["Bar"]);
        let (out, _) = click(false, false, Point::new(195.0, 170.0));
        assert_eq!(out, vec!["Content"]);
    }

    #[test]
    fn a_modal_covers_the_content_only() {
        let (out, _) = click(false, true, Point::new(195.0, 170.0));
        assert!(out.is_empty(), "the dialog takes it: {out:?}");
        let (out, _) = click(false, true, Point::new(60.0, 250.0));
        assert_eq!(out, vec!["Dismiss"]);
        let (out, _) = click(true, true, Point::new(200.0, 1.0));
        assert_eq!(out, vec!["Title"]);
    }

    #[test]
    fn maximized_edges_pass_clicks_through() {
        let (out, _) = click(true, false, Point::new(200.0, 1.0));
        assert_eq!(out, vec!["Title"]);
        let (out, _) = click(true, false, Point::new(0.0, 150.0));
        assert!(out.is_empty(), "{out:?}");
    }

    #[test]
    fn maximizing_keeps_the_tree_shape() {
        fn same(a: &Tree, b: &Tree) -> bool {
            a.tag == b.tag
                && a.children.len() == b.children.len()
                && a.children.iter().zip(&b.children).all(|(a, b)| same(a, b))
        }
        let (a, b) = (frame(false, false), frame(true, false));
        assert!(same(&Tree::new(a.as_widget()), &Tree::new(b.as_widget())));
        let other: Element<'static, Msg> = iced::widget::column![space()].into();
        assert!(!same(
            &Tree::new(a.as_widget()),
            &Tree::new(other.as_widget())
        ));
    }

    #[test]
    fn workspace_item_has_one_shape_for_every_state() {
        let src = AvatarSource::initials("Acme", "https://acme.example");
        let shape = |unread, selected, signed_out| {
            let e: Element<'_, Msg> = title_bar_workspace(
                &src,
                "Acme",
                unread,
                selected,
                signed_out,
                Some(Msg::Content),
            );
            let t = Tree::new(e.as_widget());
            (t.tag, t.children.len(), t.children[0].children.len())
        };
        let base = shape(None, false, false);
        assert_eq!(base, shape(Some((3, BadgeKind::Mention)), true, true));
        assert_eq!(base, shape(Some((0, BadgeKind::Unread)), false, true));
    }
}
