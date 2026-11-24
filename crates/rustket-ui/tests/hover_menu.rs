//! The "…" menu of a message row's hover toolbar stays open while the pointer
//! is on it. Over the menu card iced gives the row no cursor, so the row is
//! not hovered there: the toolbar and its menu are pinned while the app says
//! the menu is open.
//!
//! The window is driven like iced_winit 0.14: one persistent
//! `UserInterface`, an input is one `update`, a frame is
//! `update(RedrawRequested)` + `draw`, repeated while a redraw is requested;
//! a published message changes the app state and rebuilds the interface.

use std::cell::Cell;
use std::rc::Rc;
use std::time::Instant;

use iced::advanced::layout::{self, Layout};
use iced::advanced::overlay;
use iced::advanced::renderer as adv_renderer;
use iced::advanced::widget::{Id, Operation, Tree, Widget};
use iced::advanced::{Clipboard, Shell, clipboard};
use iced::widget::{button, column, container, space, text};
use iced::{
    Color, Element, Event, Length, Point, Rectangle, Size, Theme, Vector, event, keyboard, mouse,
    window,
};
use iced_runtime::user_interface::{State, UserInterface};
use rustket_ui::Icon;
use rustket_ui::widget as w;

#[derive(Debug, Clone, PartialEq)]
enum M {
    Toggle,
    Item(u8),
    Dismiss,
    Reply,
}

struct NoClip;

impl Clipboard for NoClip {
    fn read(&self, _: clipboard::Kind) -> Option<String> {
        None
    }
    fn write(&mut self, _: clipboard::Kind, _: String) {}
}

const SIZE: Size = Size::new(800.0, 600.0);
const MAX_FRAMES: usize = 8;

fn renderer() -> iced::Renderer {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        let mut fonts = iced::advanced::graphics::text::font_system()
            .write()
            .expect("font system");
        for f in rustket_ui::fonts() {
            fonts.load_font(f);
        }
    });
    iced::Renderer::Secondary(iced_tiny_skia::Renderer::new(
        rustket_ui::theme::font_regular(),
        iced::Pixels(14.0),
    ))
}

/// Draw count and last drawn bounds of a wrapped element.
#[derive(Clone, Default)]
struct Probe(Rc<Cell<(u32, Option<Rectangle>)>>);

impl Probe {
    fn count(&self) -> u32 {
        self.0.get().0
    }
    fn bounds(&self) -> Option<Rectangle> {
        self.0.get().1
    }
}

struct Seen<'a> {
    content: Element<'a, M>,
    probe: Probe,
}

impl Widget<M, Theme, iced::Renderer> for Seen<'_> {
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
        style: &adv_renderer::Style,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
    ) {
        let (n, _) = self.probe.0.get();
        self.probe.0.set((n + 1, Some(layout.bounds())));
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
        shell: &mut Shell<'_, M>,
        viewport: &Rectangle,
    ) {
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
    ) -> Option<overlay::Element<'b, M, Theme, iced::Renderer>> {
        self.content.as_widget_mut().overlay(
            &mut tree.children[0],
            layout,
            renderer,
            viewport,
            translation,
        )
    }
}

fn seen<'a>(content: impl Into<Element<'a, M>>, probe: &Probe) -> Element<'a, M> {
    Element::new(Seen {
        content: content.into(),
        probe: probe.clone(),
    })
}

#[derive(Clone, Default)]
struct Probes {
    popup: Probe,
    toolbar: Probe,
    tip: Probe,
}

/// One `message_row` at `top` with a toolbar like the timeline's: a button
/// with a tooltip and the "…" menu (`BelowEnd`).
fn view(top: f32, grouped: bool, open: bool, p: &Probes) -> Element<'static, M> {
    let entries: Vec<w::MenuEntry<M>> = (0..8)
        .map(|i| w::MenuItem::new(format!("Action {i}"), M::Item(i)).into())
        .collect();
    let more = w::popover(
        container(w::icon_button_with(
            Icon::Ellipsis,
            Some(M::Toggle),
            "More",
            open,
            28.0,
        ))
        .id("trigger"),
        seen(w::menu_panel(&entries), &p.popup),
        open,
        w::Placement::BelowEnd,
        M::Dismiss,
    );
    let reply = iced::widget::tooltip(
        container(button(space()).width(28).height(28).on_press(M::Reply)).id("reply"),
        seen(container(text("Reply")).padding(4), &p.tip),
        iced::widget::tooltip::Position::Bottom,
    );
    let mut props = w::MessageRowProps::new(
        space().width(36).height(36),
        "alice",
        "12:00",
        text("a reply in the thread"),
    );
    props.is_sequential = grouped;
    props.actions_toolbar = Some(seen(
        w::message_toolbar(vec![reply.into(), more]),
        &p.toolbar,
    ));
    props.actions_menu_open = open;
    column![
        space().height(top),
        container(w::message_row(props))
            .id("row")
            .width(Length::Fill),
        space().height(Length::Fill),
    ]
    .into()
}

type Ui = UserInterface<'static, M, Theme, iced::Renderer>;

fn bounds_of(ui: &mut Ui, r: &iced::Renderer, id: &'static str) -> Rectangle {
    struct Find(Id, Option<Rectangle>);
    impl Operation for Find {
        fn traverse(&mut self, operate: &mut dyn FnMut(&mut dyn Operation)) {
            operate(self);
        }
        fn container(&mut self, id: Option<&Id>, bounds: Rectangle) {
            if id == Some(&self.0) {
                self.1 = Some(bounds);
            }
        }
    }
    let mut f = Find(Id::new(id), None);
    ui.operate(r, &mut f);
    f.1.unwrap_or_else(|| panic!("no {id}"))
}

/// What one drawn frame showed, and whether it asked for another one.
#[derive(Debug, Clone, Copy)]
struct Frame {
    popup: bool,
    toolbar: bool,
    tip: bool,
    more: bool,
}

struct Win {
    top: f32,
    grouped: bool,
    open: bool,
    p: Probes,
    ui: Option<Ui>,
    r: iced::Renderer,
    cursor: Point,
    published: Vec<M>,
    /// The last drawn frame: what the screen shows.
    screen: Option<Frame>,
}

impl Win {
    fn new(top: f32, grouped: bool) -> Self {
        let mut w = Self {
            top,
            grouped,
            open: false,
            p: Probes::default(),
            ui: None,
            r: renderer(),
            cursor: Point::new(-100.0, -100.0),
            published: Vec::new(),
            screen: None,
        };
        w.rebuild();
        w.settle();
        w
    }

    fn ui(&mut self) -> &mut Ui {
        self.ui.as_mut().expect("built")
    }

    fn rebuild(&mut self) {
        let cache = self.ui.take().map(Ui::into_cache).unwrap_or_default();
        let v = view(self.top, self.grouped, self.open, &self.p);
        self.ui = Some(UserInterface::build(v, SIZE, cache, &mut self.r));
    }

    fn bounds(&mut self, id: &'static str) -> Rectangle {
        let r = renderer();
        bounds_of(self.ui(), &r, id)
    }

    /// One `update`; the app handles what it published. Whether a redraw is due.
    fn update(&mut self, events: &[Event]) -> bool {
        self.update_status(events).0
    }

    /// [`Win::update`] and the event statuses.
    fn update_status(&mut self, events: &[Event]) -> (bool, Vec<event::Status>) {
        let mut msgs = Vec::new();
        let cursor = mouse::Cursor::Available(self.cursor);
        let ui = self.ui.as_mut().expect("built");
        let (state, statuses) = ui.update(events, cursor, &mut self.r, &mut NoClip, &mut msgs);
        let mut redraw = matches!(
            state,
            State::Updated { redraw_request, .. } if redraw_request != window::RedrawRequest::Wait
        );
        if !msgs.is_empty() {
            for m in &msgs {
                match m {
                    M::Toggle => self.open = !self.open,
                    M::Dismiss | M::Item(_) => self.open = false,
                    M::Reply => {}
                }
            }
            self.published.extend(msgs);
            self.rebuild();
            redraw = true;
        }
        (redraw, statuses)
    }

    fn frame(&mut self) -> Frame {
        let before = (
            self.p.popup.count(),
            self.p.toolbar.count(),
            self.p.tip.count(),
        );
        let more = self.update(&[Event::Window(
            window::Event::RedrawRequested(Instant::now()),
        )]);
        let theme = rustket_ui::theme(rustket_ui::Mode::Light);
        let cursor = mouse::Cursor::Available(self.cursor);
        self.ui.as_mut().expect("built").draw(
            &mut self.r,
            &theme,
            &adv_renderer::Style {
                text_color: Color::BLACK,
            },
            cursor,
        );
        let f = Frame {
            popup: self.p.popup.count() > before.0,
            toolbar: self.p.toolbar.count() > before.1,
            tip: self.p.tip.count() > before.2,
            more,
        };
        self.screen = Some(f);
        f
    }

    /// Frames while a redraw is requested (at most `MAX_FRAMES`).
    fn settle(&mut self) -> Vec<Frame> {
        let mut out = vec![self.frame()];
        while out.len() < MAX_FRAMES && out.last().is_some_and(|f| f.more) {
            out.push(self.frame());
        }
        out
    }

    fn input(&mut self, events: &[Event]) -> Vec<Frame> {
        if self.update(events) {
            self.settle()
        } else {
            Vec::new()
        }
    }

    fn move_to(&mut self, p: Point) -> Vec<Frame> {
        self.cursor = p;
        self.input(&[Event::Mouse(mouse::Event::CursorMoved { position: p })])
    }

    /// Escape: its status and the frames after it.
    fn escape(&mut self) -> (Vec<event::Status>, Vec<Frame>) {
        let esc = keyboard::Key::Named(keyboard::key::Named::Escape);
        let (redraw, statuses) =
            self.update_status(&[Event::Keyboard(keyboard::Event::KeyPressed {
                key: esc.clone(),
                modified_key: esc,
                physical_key: keyboard::key::Physical::Code(keyboard::key::Code::Escape),
                location: keyboard::Location::Standard,
                modifiers: keyboard::Modifiers::empty(),
                text: None,
                repeat: false,
            })]);
        let frames = if redraw { self.settle() } else { Vec::new() };
        (statuses, frames)
    }

    fn click(&mut self) -> Vec<Frame> {
        let mut f = self.input(&[Event::Mouse(mouse::Event::ButtonPressed(
            mouse::Button::Left,
        ))]);
        f.extend(self.input(&[Event::Mouse(mouse::Event::ButtonReleased(
            mouse::Button::Left,
        ))]));
        f
    }

    /// Hover the row, click "…": the menu is open and drawn. The trigger, the
    /// row and the menu card.
    fn open_menu(&mut self) -> (Rectangle, Rectangle, Rectangle) {
        let row = self.bounds("row");
        self.move_to(Point::new(row.x + 300.0, row.center_y()));
        let trigger = self.bounds("trigger");
        self.move_to(trigger.center());
        self.click();
        assert_eq!(std::mem::take(&mut self.published), [M::Toggle]);
        assert!(self.open);
        let f = self.settle();
        assert!(f.iter().all(|f| f.popup && f.toolbar), "opens: {f:?}");
        let card = self.p.popup.bounds().expect("menu drawn");
        (trigger, row, card)
    }
}

/// A walk from "…" into the menu and on through it, then a click.
#[derive(Debug)]
struct Walk {
    up: bool,
    /// Steps after which the screen did not show the open menu.
    menu_gone: usize,
    /// Drawn frames without the menu.
    menu_frames_missing: usize,
    /// Drawn frames without the toolbar (the menu's anchor).
    toolbar_frames_missing: usize,
    /// Steps that ended in a redraw loop.
    loops: usize,
    clicked: Vec<M>,
    trace: String,
}

fn walk(top: f32, grouped: bool) -> Walk {
    let mut w = Win::new(top, grouped);
    let (trigger, row, card) = w.open_menu();
    let up = card.y + card.height <= trigger.y;
    let x = trigger.center_x();
    // The item nearest to the trigger, then 70 px further into the card.
    let first = if up {
        card.y + card.height - 18.0
    } else {
        card.y + 18.0
    };
    let last = if up { first - 70.0 } else { first + 70.0 };
    let step = if up { -3.0 } else { 3.0 };
    let mut y = trigger.center_y();
    let mut out = Walk {
        up,
        menu_gone: 0,
        menu_frames_missing: 0,
        toolbar_frames_missing: 0,
        loops: 0,
        clicked: Vec::new(),
        trace: format!(
            "row y {:.1}..{:.1}, card y {:.1}..{:.1}:",
            row.y,
            row.y + row.height,
            card.y,
            card.y + card.height,
        ),
    };
    let mut points = Vec::new();
    while (last - y).abs() > 3.0 {
        y += step;
        points.push(Point::new(x, y));
        // Rest on the first item with small wiggles.
        if (y - first).abs() < 1.5 {
            for dx in [1.0, -1.0, 1.0, 0.0] {
                points.push(Point::new(x + dx, y));
            }
        }
    }
    for p in points {
        let frames = w.move_to(p);
        let s: String = frames
            .iter()
            .map(|f| if f.popup { 'T' } else { 'F' })
            .collect();
        out.trace += &format!(
            " {:.0}{}:{}",
            p.y,
            if row.contains(p) { "r" } else { "" },
            if s.is_empty() { "-" } else { &s }
        );
        out.menu_frames_missing += frames.iter().filter(|f| !f.popup).count();
        out.toolbar_frames_missing += frames.iter().filter(|f| !f.toolbar).count();
        if frames.len() >= MAX_FRAMES && frames.last().is_some_and(|f| f.more) {
            out.loops += 1;
        }
        if !w.screen.is_some_and(|f| f.popup) {
            out.menu_gone += 1;
        }
    }
    w.published.clear();
    w.click();
    out.clicked = std::mem::take(&mut w.published);
    out
}

fn assert_stays_open(name: &str, top: f32, grouped: bool, want_up: bool) {
    let wk = walk(top, grouped);
    assert_eq!(wk.up, want_up, "{name}: direction\n{wk:?}");
    assert_eq!(wk.menu_gone, 0, "{name}: the menu vanished\n{wk:?}");
    assert_eq!(wk.menu_frames_missing, 0, "{name}: the menu blinks\n{wk:?}");
    assert_eq!(
        wk.toolbar_frames_missing, 0,
        "{name}: the toolbar blinks\n{wk:?}"
    );
    assert_eq!(wk.loops, 0, "{name}: endless redraws\n{wk:?}");
    assert!(
        matches!(wk.clicked.as_slice(), [M::Item(_)]),
        "{name}: the click on an item\n{wk:?}"
    );
}

/// The card overlaps the lower part of a two-line row: at HEAD the row's
/// hover flipped every frame (blinking, endless redraws).
#[test]
fn menu_opening_down_stays_open_under_the_pointer() {
    assert_stays_open("down, two-line row", 100.0, false, false);
}

/// Near the bottom the card flips above the row: at HEAD the first motion
/// toward it un-hovered the row and the menu was gone.
#[test]
fn menu_flipped_up_stays_open_under_the_pointer() {
    assert_stays_open("up, two-line row", 480.0, false, true);
}

/// A grouped one-line row: the card starts below the row.
#[test]
fn menu_of_a_grouped_row_stays_open_under_the_pointer() {
    assert_stays_open("down, grouped row", 100.0, true, false);
}

/// With the pointer off the menu and the row the menu stays (it closes on a
/// click outside); a dismiss or a selected item hides the menu and the
/// toolbar, and the frames settle.
#[test]
fn dismiss_and_select_hide_the_menu_and_the_toolbar() {
    for (top, up) in [(100.0, false), (480.0, true)] {
        let mut w = Win::new(top, false);
        let (_, row, card) = w.open_menu();
        let on_card = if up {
            Point::new(card.center_x(), card.y + 20.0)
        } else {
            Point::new(card.center_x(), card.y + card.height - 20.0)
        };
        assert!(!row.contains(on_card));
        w.move_to(on_card);
        let away = Point::new(100.0, if up { 50.0 } else { 550.0 });
        assert!(!row.contains(away) && !card.contains(away));
        w.move_to(away);
        assert!(
            w.screen.is_some_and(|f| f.popup && f.toolbar),
            "up={up}: the open menu stays while the pointer is elsewhere"
        );
        let frames = w.click();
        assert_eq!(std::mem::take(&mut w.published), [M::Dismiss]);
        assert!(!w.open);
        let last = frames.last().copied().expect("frames after the dismiss");
        assert!(
            !last.popup && !last.toolbar && !last.more,
            "up={up}: after dismiss {frames:?}"
        );

        let mut w = Win::new(top, false);
        w.open_menu();
        w.move_to(on_card);
        let frames = w.click();
        assert!(
            matches!(w.published.as_slice(), [M::Item(_)]),
            "up={up}: {:?}",
            w.published
        );
        let last = frames.last().copied().expect("frames after the selection");
        assert!(
            !last.popup && !last.toolbar && !last.more,
            "up={up}: after selecting {frames:?}"
        );
    }
}

/// A toolbar tooltip still closes, and the toolbar hides, when the pointer
/// leaves the row (no menu open: nothing is pinned).
#[test]
fn toolbar_tooltip_and_toolbar_hide_when_leaving_the_row() {
    let mut w = Win::new(100.0, false);
    let row = w.bounds("row");
    w.move_to(Point::new(row.x + 300.0, row.center_y()));
    let reply = w.bounds("reply");
    w.move_to(reply.center());
    let f = w.settle();
    assert!(f.iter().any(|f| f.tip && f.toolbar), "tooltip {f:?}");
    // Straight up and out of the row, step by step.
    let mut y = reply.center_y();
    while y > row.y - 10.0 {
        y -= 3.0;
        w.move_to(Point::new(reply.center_x(), y));
    }
    let s = w.screen.expect("drawn");
    assert!(
        !s.tip && !s.toolbar && !s.more,
        "after leaving the row {s:?}"
    );
    // And in one jump from the button to above the row.
    let mut w = Win::new(100.0, false);
    w.move_to(Point::new(row.x + 300.0, row.center_y()));
    w.move_to(reply.center());
    w.settle();
    let frames = w.move_to(Point::new(reply.center_x(), 40.0));
    let s = w.screen.expect("drawn");
    assert!(
        !s.tip && !s.toolbar && !s.more,
        "after a jump out {frames:?}"
    );
}

/// Escape closes the open menu wherever the pointer is (on the card or away
/// from the row): the menu captures it, so the app's Escape does not see it.
#[test]
fn escape_closes_the_open_menu() {
    for (top, up) in [(100.0, false), (480.0, true)] {
        for on_card in [true, false] {
            let mut w = Win::new(top, false);
            let (_, row, card) = w.open_menu();
            let p = if on_card {
                card.center()
            } else {
                Point::new(100.0, if up { 50.0 } else { 550.0 })
            };
            assert!(!row.contains(p));
            w.move_to(p);
            assert!(
                w.screen.is_some_and(|f| f.popup && f.toolbar),
                "up={up} on_card={on_card}: open before Escape"
            );
            let (statuses, frames) = w.escape();
            assert_eq!(statuses, [event::Status::Captured]);
            assert_eq!(std::mem::take(&mut w.published), [M::Dismiss]);
            assert!(!w.open);
            let last = frames.last().copied().expect("frames after Escape");
            assert!(
                !last.popup && !last.toolbar && !last.more,
                "up={up} on_card={on_card}: after Escape {frames:?}"
            );
        }
    }
}
