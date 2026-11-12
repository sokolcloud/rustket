//! Headless widget tests: drive `iced_runtime::UserInterface` with the tiny-skia
//! renderer (no window, no GPU) and check behaviour and that drawing succeeds.

use std::cell::Cell;
use std::rc::Rc;
use std::time::{Duration, Instant};

use iced::advanced::layout::{self, Layout};
use iced::advanced::renderer::{self as adv_renderer, Headless};
use iced::advanced::widget::operation::{self, focusable};
use iced::advanced::widget::{self, Id, Operation, Tree, Widget};
use iced::advanced::{Clipboard, clipboard};
use iced::widget::{button, column, container, scrollable, space, text, text_input};
use iced::{
    Color, Element, Event, Length, Point, Rectangle, Size, Theme, Vector, keyboard, mouse, window,
};
use iced_runtime::user_interface::{Cache, State, UserInterface};

use rustket_ui::widget as w;

#[derive(Debug, Clone, PartialEq)]
enum M {
    Base,
    Trig,
    Dismiss,
    Close(u64),
    Item,
    Resize(f32),
    Input(String),
    Scrolled,
    Noop,
    Toggle(u64),
    Reorder(u64, Option<u64>),
    AutoScroll(f32),
}

struct NoClip;

impl Clipboard for NoClip {
    fn read(&self, _kind: clipboard::Kind) -> Option<String> {
        None
    }

    fn write(&mut self, _kind: clipboard::Kind, _contents: String) {}
}

const SIZE: Size = Size::new(800.0, 600.0);

/// Registers the bundled fonts once, as the app does at startup: text
/// measurements must not depend on the runner's system fonts.
fn load_fonts() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        let mut fonts = iced::advanced::graphics::text::font_system()
            .write()
            .expect("font system");
        for f in rustket_ui::fonts() {
            fonts.load_font(f);
        }
    });
}

fn renderer() -> iced::Renderer {
    load_fonts();
    iced::Renderer::Secondary(iced_tiny_skia::Renderer::new(
        rustket_ui::theme::DEFAULT_FONT,
        iced::Pixels(14.0),
    ))
}

type Ui<'a> = UserInterface<'a, M, Theme, iced::Renderer>;

fn moved(p: Point) -> Event {
    Event::Mouse(mouse::Event::CursorMoved { position: p })
}

fn press() -> Event {
    Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left))
}

fn release() -> Event {
    Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left))
}

fn redraw() -> Event {
    Event::Window(window::Event::RedrawRequested(Instant::now()))
}

fn wheel(y: f32) -> Event {
    Event::Mouse(mouse::Event::WheelScrolled {
        delta: mouse::ScrollDelta::Lines { x: 0.0, y },
    })
}

fn escape() -> Event {
    Event::Keyboard(keyboard::Event::KeyPressed {
        key: keyboard::Key::Named(keyboard::key::Named::Escape),
        modified_key: keyboard::Key::Named(keyboard::key::Named::Escape),
        physical_key: keyboard::key::Physical::Code(keyboard::key::Code::Escape),
        location: keyboard::Location::Standard,
        modifiers: keyboard::Modifiers::default(),
        text: None,
        repeat: false,
    })
}

fn draw(ui: &mut Ui<'_>, r: &mut iced::Renderer, cursor: mouse::Cursor) {
    let theme = rustket_ui::theme(rustket_ui::Mode::Light);
    ui.draw(
        r,
        &theme,
        &adv_renderer::Style {
            text_color: Color::BLACK,
        },
        cursor,
    );
}

/// Feeds events with the cursor at `at`, draws, returns messages and interaction.
fn step(
    ui: &mut Ui<'_>,
    r: &mut iced::Renderer,
    events: &[Event],
    at: Point,
) -> (Vec<M>, mouse::Interaction) {
    let mut msgs = Vec::new();
    let cursor = mouse::Cursor::Available(at);
    let (state, _) = ui.update(events, cursor, r, &mut NoClip, &mut msgs);
    let interaction = match state {
        State::Updated {
            mouse_interaction, ..
        } => mouse_interaction,
        State::Outdated => mouse::Interaction::None,
    };
    draw(ui, r, cursor);
    (msgs, interaction)
}

fn screenshot(r: &mut iced::Renderer) {
    let _ = r.screenshot(
        Size::new(SIZE.width as u32, SIZE.height as u32),
        1.0,
        Color::WHITE,
    );
}

/// Offsets of every scrollable, in tree order.
fn scroll_offsets(ui: &mut Ui<'_>, r: &iced::Renderer) -> Vec<Vector> {
    struct Probe(Vec<Vector>);
    impl Operation for Probe {
        fn traverse(&mut self, operate: &mut dyn FnMut(&mut dyn Operation<()>)) {
            operate(self);
        }

        fn scrollable(
            &mut self,
            _id: Option<&Id>,
            _bounds: Rectangle,
            _content_bounds: Rectangle,
            translation: Vector,
            _state: &mut dyn widget::operation::Scrollable,
        ) {
            self.0.push(translation);
        }
    }
    let mut probe = Probe(Vec::new());
    ui.operate(r, &mut probe);
    probe.0
}

/// Scrolls every scrollable by `dy`.
fn scroll_by(ui: &mut Ui<'_>, r: &iced::Renderer, dy: f32) {
    struct ScrollBy(f32);
    impl Operation for ScrollBy {
        fn traverse(&mut self, operate: &mut dyn FnMut(&mut dyn Operation<()>)) {
            operate(self);
        }

        fn scrollable(
            &mut self,
            _id: Option<&Id>,
            bounds: Rectangle,
            content_bounds: Rectangle,
            _translation: Vector,
            state: &mut dyn widget::operation::Scrollable,
        ) {
            let offset = widget::operation::scrollable::AbsoluteOffset { x: 0.0, y: self.0 };
            state.scroll_by(offset, bounds, content_bounds);
        }
    }
    ui.operate(r, &mut ScrollBy(dy));
}

/// RGBA of the pixel at `p` in what was drawn last.
fn pixel(r: &mut iced::Renderer, p: Point) -> [u8; 4] {
    let shot = r.screenshot(
        Size::new(SIZE.width as u32, SIZE.height as u32),
        1.0,
        Color::WHITE,
    );
    let i = (p.y as usize * SIZE.width as usize + p.x as usize) * 4;
    [shot[i], shot[i + 1], shot[i + 2], shot[i + 3]]
}

fn auto_scrolls(msgs: &[M]) -> Vec<f32> {
    msgs.iter()
        .filter_map(|m| match m {
            M::AutoScroll(dy) => Some(*dy),
            _ => None,
        })
        .collect()
}

fn is_focused(ui: &mut Ui<'_>, r: &iced::Renderer, id: Id) -> bool {
    struct Probe(Id, bool);
    impl Operation for Probe {
        fn traverse(&mut self, operate: &mut dyn FnMut(&mut dyn Operation<()>)) {
            operate(self);
        }

        fn focusable(
            &mut self,
            id: Option<&Id>,
            _bounds: Rectangle,
            state: &mut dyn widget::operation::Focusable,
        ) {
            if id == Some(&self.0) {
                self.1 = state.is_focused();
            }
        }
    }
    let mut probe = Probe(id, false);
    ui.operate(r, &mut probe);
    probe.1
}

fn focus(ui: &mut Ui<'_>, r: &iced::Renderer, id: Id) {
    let mut op = operation::map(focusable::focus::<()>(id), |()| ());
    ui.operate(r, &mut op);
}

fn timeline<'a>(input: &Id) -> Element<'a, M> {
    let mut col = column![
        text_input("composer", "")
            .id(input.clone())
            .on_input(M::Input)
    ]
    .spacing(4);
    for i in 0..200 {
        col = col.push(text(format!("row {i}")).height(20));
    }
    scrollable(col)
        .width(Length::Fill)
        .height(Length::Fill)
        .on_scroll(|_| M::Scrolled)
        .into()
}

fn toast_list(n: u64) -> Vec<w::Toast> {
    (0..n)
        .map(|id| w::Toast {
            id,
            kind: w::BannerKind::Info,
            title: "Message sent".into(),
            body: "delivered".into(),
        })
        .collect()
}

fn app<'a>(input: &Id, modal_open: bool, list: &'a [w::Toast]) -> Element<'a, M> {
    let base = w::modal(
        timeline(input),
        modal_open.then(|| container(text("dialog")).width(200).height(100).into()),
        Some(M::Dismiss),
    );
    w::toasts(base, list, M::Close)
}

#[test]
fn base_state_survives_modal_and_toasts() {
    let mut r = renderer();
    let input = Id::new("composer");
    let none = toast_list(0);
    let some = toast_list(2);
    let center = Point::new(400.0, 300.0);

    let mut ui = UserInterface::build(app(&input, false, &none), SIZE, Cache::default(), &mut r);
    step(&mut ui, &mut r, &[moved(center), wheel(-20.0)], center);
    focus(&mut ui, &r, input.clone());
    let scrolled = scroll_offsets(&mut ui, &r);
    assert!(
        scrolled[0].y > 100.0,
        "wheel scrolled the timeline: {scrolled:?}"
    );
    assert!(is_focused(&mut ui, &r, input.clone()));

    for (modal_open, list) in [(false, &some), (true, &some), (true, &none), (false, &none)] {
        let cache = ui.into_cache();
        ui = UserInterface::build(app(&input, modal_open, list), SIZE, cache, &mut r);
        step(&mut ui, &mut r, &[redraw()], center);
        assert_eq!(
            scroll_offsets(&mut ui, &r),
            scrolled,
            "modal {modal_open}, toasts {}",
            list.len()
        );
        // Focus probes do not reach the base while a modal blocks it; focus is
        // checked again once it is closed.
        if !modal_open {
            assert!(
                is_focused(&mut ui, &r, input.clone()),
                "toasts {}",
                list.len()
            );
        }
    }
}

#[test]
fn modal_blocks_base_and_dismisses() {
    let mut r = renderer();
    let view = || -> Element<'static, M> {
        w::modal(
            button(space().width(Length::Fill).height(Length::Fill)).on_press(M::Base),
            Some(
                container(button(text("ok")).on_press(M::Item))
                    .width(200)
                    .height(100)
                    .into(),
            ),
            Some(M::Dismiss),
        )
    };
    let mut ui = UserInterface::build(view(), SIZE, Cache::default(), &mut r);
    let outside = Point::new(50.0, 50.0);
    let (_, interaction) = step(&mut ui, &mut r, &[moved(outside)], outside);
    assert_ne!(
        interaction,
        mouse::Interaction::Pointer,
        "base button must not show a pointer"
    );
    let (msgs, _) = step(&mut ui, &mut r, &[press(), release()], outside);
    assert_eq!(msgs, vec![M::Dismiss]);
    let (msgs, _) = step(&mut ui, &mut r, &[wheel(-5.0)], outside);
    assert!(msgs.is_empty());
    let ok = Point::new(310.0, 260.0);
    let (msgs, _) = step(&mut ui, &mut r, &[moved(ok), press(), release()], ok);
    assert_eq!(msgs, vec![M::Item]);
    screenshot(&mut r);
}

#[test]
fn single_escape_dismisses_modal_with_focused_input() {
    let mut r = renderer();
    let id = Id::new("dialog-input");
    let view = |id: &Id| -> Element<'static, M> {
        w::modal(
            text("base"),
            Some(
                container(text_input("x", "").id(id.clone()).on_input(M::Input))
                    .width(300)
                    .into(),
            ),
            Some(M::Dismiss),
        )
    };
    let mut ui = UserInterface::build(view(&id), SIZE, Cache::default(), &mut r);
    let at = Point::new(400.0, 300.0);
    step(&mut ui, &mut r, &[redraw()], at);
    focus(&mut ui, &r, id.clone());
    let (msgs, _) = step(&mut ui, &mut r, &[escape()], at);
    assert_eq!(msgs, vec![M::Dismiss]);
}

#[test]
fn toast_cards_capture_clicks() {
    let mut r = renderer();
    let list = toast_list(1);
    fn view(list: &[w::Toast]) -> Element<'_, M> {
        w::toasts(
            button(space().width(Length::Fill).height(Length::Fill)).on_press(M::Base),
            list,
            M::Close,
        )
    }
    let mut ui = UserInterface::build(view(&list), SIZE, Cache::default(), &mut r);
    // Card: 340 wide, bottom-right with 20px margin; click on its text area.
    let on_card = Point::new(SIZE.width - 20.0 - 250.0, SIZE.height - 20.0 - 20.0);
    let (msgs, _) = step(
        &mut ui,
        &mut r,
        &[moved(on_card), press(), release()],
        on_card,
    );
    assert!(!msgs.contains(&M::Base), "{msgs:?}");
    let elsewhere = Point::new(100.0, 100.0);
    let (msgs, _) = step(
        &mut ui,
        &mut r,
        &[moved(elsewhere), press(), release()],
        elsewhere,
    );
    assert_eq!(msgs, vec![M::Base]);
}

#[test]
fn pinned_card_click_survives_toast_changes() {
    let mut r = renderer();
    fn view(list: &[w::Toast]) -> Element<'_, M> {
        let card = button(space().width(Length::Fill).height(60))
            .on_press(M::Trig)
            .into();
        w::toasts_with(
            button(space().width(Length::Fill).height(Length::Fill)).on_press(M::Base),
            vec![card],
            list,
            M::Close,
        )
    }
    let one = toast_list(1);
    let none = toast_list(0);
    // The pinned card sits in the corner, under any toasts.
    let on_card = Point::new(SIZE.width - 20.0 - 100.0, SIZE.height - 20.0 - 30.0);
    for (at_press, at_release) in [(&one, &none), (&none, &one)] {
        let mut ui = UserInterface::build(view(at_press), SIZE, Cache::default(), &mut r);
        let (msgs, _) = step(&mut ui, &mut r, &[moved(on_card), press()], on_card);
        assert!(msgs.is_empty(), "{msgs:?}");
        let cache = ui.into_cache();
        let mut ui = UserInterface::build(view(at_release), SIZE, cache, &mut r);
        let (msgs, _) = step(&mut ui, &mut r, &[release()], on_card);
        assert_eq!(
            msgs,
            vec![M::Trig],
            "toasts {} -> {}",
            at_press.len(),
            at_release.len()
        );
    }
}

/// Counts how often it is drawn.
struct DrawProbe(Rc<Cell<u32>>);

impl<Message> Widget<Message, Theme, iced::Renderer> for DrawProbe {
    fn size(&self) -> Size<Length> {
        Size::new(Length::Fixed(40.0), Length::Fixed(20.0))
    }

    fn layout(
        &mut self,
        _tree: &mut Tree,
        _renderer: &iced::Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        layout::Node::new(limits.resolve(40.0, 20.0, Size::new(40.0, 20.0)))
    }

    fn draw(
        &self,
        _tree: &Tree,
        _renderer: &mut iced::Renderer,
        _theme: &Theme,
        _style: &adv_renderer::Style,
        _layout: Layout<'_>,
        _cursor: mouse::Cursor,
        _viewport: &Rectangle,
    ) {
        self.0.set(self.0.get() + 1);
    }
}

#[test]
fn hover_toolbar_tooltip_closes_when_leaving_row() {
    let hits = Rc::new(Cell::new(0));
    let mut r = renderer();
    let view = |hits: Rc<Cell<u32>>| -> Element<'static, M> {
        let tip = iced::widget::tooltip(
            button(space()).width(30).height(30).on_press(M::Item),
            Element::new(DrawProbe(hits)),
            iced::widget::tooltip::Position::Bottom,
        )
        .delay(Duration::ZERO);
        let mut p = w::MessageRowProps::new(
            space().width(36).height(36),
            "alice",
            "10:00",
            text("hello world body"),
        );
        p.actions_toolbar = Some(w::message_toolbar(vec![tip.into()]));
        column![
            space().height(100),
            w::message_row(p),
            space().height(Length::Fill)
        ]
        .into()
    };
    let mut ui = UserInterface::build(view(hits.clone()), SIZE, Cache::default(), &mut r);
    // Find the toolbar button: scan the row's right edge for a pointer.
    let mut on_button = None;
    'scan: for y in (100..170).step_by(2) {
        for x in (600..800).rev().step_by(2) {
            let p = Point::new(x as f32, y as f32);
            let (_, i) = step(&mut ui, &mut r, &[moved(p)], p);
            if i == mouse::Interaction::Pointer {
                on_button = Some(p);
                break 'scan;
            }
        }
    }
    let on_button = on_button.expect("toolbar button is shown on hover");
    let shown = hits.get();
    step(&mut ui, &mut r, &[redraw()], on_button);
    step(&mut ui, &mut r, &[redraw()], on_button);
    assert!(
        hits.get() > shown,
        "tooltip is drawn while hovering its button"
    );
    // Press on the button, release outside the row: it must not stay pressed.
    step(&mut ui, &mut r, &[press()], on_button);
    step(&mut ui, &mut r, &[redraw()], on_button);
    let away = Point::new(on_button.x, 40.0);
    let (msgs, _) = step(&mut ui, &mut r, &[moved(away), release()], away);
    assert!(msgs.is_empty(), "{msgs:?}");
    step(&mut ui, &mut r, &[redraw()], away);
    let before = hits.get();
    let cache = ui.into_cache();
    let mut ui = UserInterface::build(view(hits.clone()), SIZE, cache, &mut r);
    step(&mut ui, &mut r, &[redraw()], away);
    assert_eq!(
        hits.get(),
        before,
        "tooltip still drawn after leaving the row"
    );
}

#[test]
fn popover_does_not_click_through() {
    let entries = vec![
        w::MenuEntry::Header("Group".into()),
        w::MenuItem::new("Disabled item", M::Item)
            .disabled(true)
            .into(),
        w::MenuEntry::Separator,
        w::MenuItem::new("Enabled", M::Item).into(),
    ];
    let mut r = renderer();
    let view = |entries: &[w::MenuEntry<M>]| -> Element<'_, M> {
        column![
            w::popover_menu(
                button(text("T")).width(30).height(30).on_press(M::Trig),
                entries,
                true,
                M::Dismiss,
                w::Placement::BelowStart,
            ),
            button(space().width(Length::Fill).height(Length::Fill)).on_press(M::Base),
        ]
        .into()
    };
    let mut ui = UserInterface::build(view(&entries), SIZE, Cache::default(), &mut r);
    for (what, p) in [
        ("header", Point::new(60.0, 46.0)),
        ("disabled", Point::new(60.0, 72.0)),
    ] {
        let (_, interaction) = step(&mut ui, &mut r, &[moved(p)], p);
        assert_ne!(interaction, mouse::Interaction::None, "{what}");
        let (msgs, _) = step(&mut ui, &mut r, &[press(), release(), wheel(-3.0)], p);
        assert!(msgs.is_empty(), "{what}: {msgs:?}");
    }
    let enabled = Point::new(60.0, 110.0);
    let (msgs, _) = step(
        &mut ui,
        &mut r,
        &[moved(enabled), press(), release()],
        enabled,
    );
    assert_eq!(msgs, vec![M::Item]);
    let outside = Point::new(600.0, 400.0);
    let (msgs, _) = step(
        &mut ui,
        &mut r,
        &[moved(outside), press(), release()],
        outside,
    );
    assert!(msgs.contains(&M::Dismiss), "{msgs:?}");
}

/// Arrows move a highlight over the enabled items (skipping headers,
/// separators and disabled items, wrapping), Enter selects it.
#[test]
fn popover_menu_works_from_the_keyboard() {
    use keyboard::key::Named;
    let entries = vec![
        w::MenuEntry::Header("Group".into()),
        w::MenuItem::new("One", M::Close(1)).into(),
        w::MenuItem::new("Disabled", M::Close(2))
            .disabled(true)
            .into(),
        w::MenuEntry::Separator,
        w::MenuItem::new("Three", M::Close(3)).into(),
    ];
    let mut r = renderer();
    let view = |entries: &[w::MenuEntry<M>]| -> Element<'_, M> {
        column![w::popover_menu(
            button(text("T")).width(30).height(30).on_press(M::Trig),
            entries,
            true,
            M::Dismiss,
            w::Placement::BelowStart,
        )]
        .into()
    };
    let mut ui = UserInterface::build(view(&entries), SIZE, Cache::default(), &mut r);
    let away = Point::new(700.0, 500.0);
    let (msgs, _) = step(&mut ui, &mut r, &[key(Named::Enter, false)], away);
    assert!(msgs.is_empty(), "nothing highlighted yet: {msgs:?}");
    let (msgs, _) = step(&mut ui, &mut r, &[key(Named::ArrowDown, false)], away);
    assert!(msgs.is_empty());
    let (msgs, _) = step(
        &mut ui,
        &mut r,
        &[key(Named::ArrowDown, false), key(Named::Enter, false)],
        away,
    );
    assert_eq!(msgs, vec![M::Close(3)], "the disabled item is skipped");
    let (msgs, _) = step(
        &mut ui,
        &mut r,
        &[key(Named::ArrowDown, false), key(Named::Enter, false)],
        away,
    );
    assert_eq!(msgs, vec![M::Close(1)], "wraps around");
    let (msgs, _) = step(
        &mut ui,
        &mut r,
        &[key(Named::ArrowUp, false), key(Named::Enter, false)],
        away,
    );
    assert_eq!(msgs, vec![M::Close(3)]);
}

/// Menus grow with long (e.g. Russian) labels instead of cutting them.
#[test]
fn menu_width_fits_long_labels() {
    load_fonts();
    let short: Vec<w::MenuEntry<M>> = vec![w::MenuItem::new("Copy", M::Item).into()];
    assert_eq!(w::menu_width(&short), w::MENU_WIDTH);
    let long: Vec<w::MenuEntry<M>> = vec![
        w::MenuItem::new("Copy", M::Item).into(),
        w::MenuItem::new("Преобразовать в команду", M::Item).into(),
    ];
    let width = w::menu_width(&long);
    assert!(
        width > w::MENU_WIDTH && width <= w::MENU_MAX_WIDTH,
        "{width}"
    );
    let huge: Vec<w::MenuEntry<M>> = vec![w::MenuItem::new("x".repeat(200), M::Item).into()];
    assert_eq!(w::menu_width(&huge), w::MENU_MAX_WIDTH);
}

/// Check marks take the leading icon slot that `menu_width` already counts,
/// and checked items select like any other.
#[test]
fn checkable_menu_items() {
    load_fonts();
    let plain: Vec<w::MenuEntry<M>> = vec![
        w::MenuItem::new("Newest first", M::Close(1)).into(),
        w::MenuItem::new("Oldest first", M::Close(2)).into(),
    ];
    let checkable: Vec<w::MenuEntry<M>> = vec![
        w::MenuItem::new("Newest first", M::Close(1))
            .checked(true)
            .into(),
        w::MenuItem::new("Oldest first", M::Close(2))
            .checked(false)
            .into(),
    ];
    assert_eq!(w::menu_width(&plain), w::menu_width(&checkable));
    let mut r = renderer();
    let view = |entries: &[w::MenuEntry<M>]| -> Element<'_, M> {
        column![w::popover_menu(
            button(text("T")).width(30).height(30).on_press(M::Trig),
            entries,
            true,
            M::Dismiss,
            w::Placement::BelowStart,
        )]
        .into()
    };
    let mut ui = UserInterface::build(view(&checkable), SIZE, Cache::default(), &mut r);
    let away = Point::new(700.0, 500.0);
    use keyboard::key::Named;
    let (msgs, _) = step(
        &mut ui,
        &mut r,
        &[key(Named::ArrowDown, false), key(Named::Enter, false)],
        away,
    );
    assert_eq!(msgs, vec![M::Close(1)]);
}

#[test]
fn split_handle_drag_does_not_scroll_sidebar() {
    let mut r = renderer();
    let input = Id::new("x");
    let view = |input: &Id| -> Element<'static, M> {
        w::split(
            timeline(input),
            text("content"),
            280.0,
            200.0,
            480.0,
            M::Resize,
        )
    };
    let mut ui = UserInterface::build(view(&input), SIZE, Cache::default(), &mut r);
    let handle = Point::new(280.0 + w::HANDLE_WIDTH / 2.0, 20.0);
    let (_, interaction) = step(&mut ui, &mut r, &[moved(handle)], handle);
    assert_eq!(interaction, mouse::Interaction::ResizingHorizontally);
    step(&mut ui, &mut r, &[press()], handle);
    let to = Point::new(330.0, 300.0);
    let (msgs, _) = step(&mut ui, &mut r, &[moved(to)], to);
    step(&mut ui, &mut r, &[release()], to);
    assert!(
        msgs.iter()
            .any(|m| matches!(m, M::Resize(w) if (*w - 330.0).abs() < 4.0)),
        "{msgs:?}"
    );
    assert!(!msgs.contains(&M::Scrolled), "{msgs:?}");
    assert_eq!(scroll_offsets(&mut ui, &r), vec![Vector::ZERO]);
    // Degenerate input does not panic.
    let _ = UserInterface::build(
        w::split(
            text("a"),
            text("b"),
            f32::NAN,
            f32::NAN,
            f32::NAN,
            M::Resize,
        ),
        SIZE,
        Cache::default(),
        &mut r,
    );
}

#[test]
fn ellipsis_renders_rtl_mixed_and_multiline() {
    let mut r = renderer();
    let samples = [
        "مرحبا بالعالم هذا نص طويل جدا للاختبار",
        "שלום עולם זה טקסט ארוך מאוד",
        "hello مرحبا world שלום",
        "Hi\nthis second line is very much longer than the first one",
        "👩‍💻 Привет мир, this is long enough to truncate",
        "",
    ];
    for width in [0.0, 30.0, 80.0, 150.0, 400.0] {
        let mut col = column![].spacing(4);
        for s in samples {
            col = col.push(container(w::ellipsis(s)).width(width));
            col = col.push(container(w::ellipsis(s).width(Length::Shrink)).width(width));
        }
        let mut ui: Ui<'_> =
            UserInterface::build(Element::from(col), SIZE, Cache::default(), &mut r);
        draw(&mut ui, &mut r, mouse::Cursor::Unavailable);
        screenshot(&mut r);
    }
    // Multi-line content collapses to one line and truncates.
    let el: Element<'_, M> = container(
        w::ellipsis("Hi\nthis second line is long enough to be cut").width(Length::Shrink),
    )
    .width(120)
    .into();
    let mut ui: Ui<'_> = UserInterface::build(el, SIZE, Cache::default(), &mut r);
    let mut op = TextProbe(Vec::new());
    ui.operate(&r, &mut op);
    let (bounds, content) = op.0.first().cloned().expect("ellipsis exposes its text");
    assert!(content.starts_with("Hi\n"));
    assert!(bounds.width <= 120.0 && bounds.width > 60.0, "{bounds:?}");
    assert!(bounds.height < 14.0 * 1.3 + 1.0, "single line: {bounds:?}");
}

struct TextProbe(Vec<(Rectangle, String)>);

impl Operation for TextProbe {
    fn traverse(&mut self, operate: &mut dyn FnMut(&mut dyn Operation<()>)) {
        operate(self);
    }

    fn text(&mut self, _id: Option<&Id>, bounds: Rectangle, text: &str) {
        self.0.push((bounds, text.to_owned()));
    }
}

#[test]
fn room_list_relayout_is_bounded() {
    let mut r = renderer();
    let long = "Lorem ipsum dolor sit amet, consectetur adipiscing elit. ".repeat(20);
    let rows = |width: f32, long: &str| -> Element<'static, M> {
        let mut col = column![];
        for i in 0..1000 {
            let mut p = w::RoomListItemProps::new(
                w::AvatarSource::initials(format!("Room {i}"), format!("r{i}")),
                format!("very-long-room-name-number-{i}-with-suffix"),
                M::Noop,
            );
            p.subtitle = Some(long.to_owned());
            p.time = Some("10:00".into());
            col = col.push(w::room_list_item(p));
        }
        container(scrollable(col)).width(width).into()
    };
    let ui = UserInterface::build(rows(300.0, &long), SIZE, Cache::default(), &mut r);
    let mut cache = ui.into_cache();
    let mut worst = Duration::ZERO;
    for width in [301.0, 302.0, 303.0, 304.0] {
        let t = Instant::now();
        let ui = UserInterface::build(rows(width, &long), SIZE, cache, &mut r);
        worst = worst.max(t.elapsed());
        cache = ui.into_cache();
    }
    eprintln!("1000 rooms, one drag step: {worst:?}");
    // Generous bound for unoptimized test builds; release is far below this.
    let limit = if cfg!(debug_assertions) { 4000 } else { 250 };
    assert!(worst < Duration::from_millis(limit), "{worst:?}");
}

fn key(named: keyboard::key::Named, shift: bool) -> Event {
    let mut modifiers = keyboard::Modifiers::default();
    if shift {
        modifiers.insert(keyboard::Modifiers::SHIFT);
    }
    Event::Keyboard(keyboard::Event::KeyPressed {
        key: keyboard::Key::Named(named),
        modified_key: keyboard::Key::Named(named),
        physical_key: keyboard::key::Physical::Code(keyboard::key::Code::Tab),
        location: keyboard::Location::Standard,
        modifiers,
        text: None,
        repeat: false,
    })
}

fn focused_of(ui: &mut Ui<'_>, r: &iced::Renderer, ids: &[&Id]) -> Vec<bool> {
    ids.iter()
        .map(|id| is_focused(ui, r, (*id).clone()))
        .collect()
}

#[test]
fn tab_cycles_inside_modal_and_never_reaches_base() {
    let mut r = renderer();
    let (base_id, a, b) = (Id::new("base"), Id::new("a"), Id::new("b"));
    let view = |base_id: &Id, a: &Id, b: &Id| -> Element<'static, M> {
        w::modal(
            text_input("base", "")
                .id(base_id.clone())
                .on_input(M::Input),
            Some(
                container(column![
                    text_input("a", "").id(a.clone()).on_input(M::Input),
                    text_input("b", "").id(b.clone()).on_input(M::Input),
                ])
                .width(300)
                .into(),
            ),
            Some(M::Dismiss),
        )
    };
    let mut ui = UserInterface::build(view(&base_id, &a, &b), SIZE, Cache::default(), &mut r);
    let at = Point::new(10.0, 10.0);
    step(&mut ui, &mut r, &[redraw()], at);
    let ids = [&base_id, &a, &b];
    step(
        &mut ui,
        &mut r,
        &[key(keyboard::key::Named::Tab, false)],
        at,
    );
    assert_eq!(focused_of(&mut ui, &r, &ids), [false, true, false]);
    step(
        &mut ui,
        &mut r,
        &[key(keyboard::key::Named::Tab, false)],
        at,
    );
    assert_eq!(focused_of(&mut ui, &r, &ids), [false, false, true]);
    step(
        &mut ui,
        &mut r,
        &[key(keyboard::key::Named::Tab, false)],
        at,
    );
    assert_eq!(
        focused_of(&mut ui, &r, &ids),
        [false, true, false],
        "wraps around"
    );
    step(&mut ui, &mut r, &[key(keyboard::key::Named::Tab, true)], at);
    assert_eq!(
        focused_of(&mut ui, &r, &ids),
        [false, false, true],
        "shift+tab goes back"
    );
    // App-level focus operations skip the base too.
    for _ in 0..3 {
        let mut op: Box<dyn Operation> = Box::new(focusable::focus_next::<()>());
        loop {
            ui.operate(&r, op.as_mut());
            match op.finish() {
                operation::Outcome::Chain(next) => op = next,
                _ => break,
            }
        }
        assert!(!is_focused(&mut ui, &r, base_id.clone()));
    }
}

/// Captures Escape, like a widget closing its own popup.
struct EscapeEater;

impl Widget<M, Theme, iced::Renderer> for EscapeEater {
    fn size(&self) -> Size<Length> {
        Size::new(Length::Fixed(10.0), Length::Fixed(10.0))
    }

    fn layout(
        &mut self,
        _tree: &mut Tree,
        _renderer: &iced::Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        layout::Node::new(limits.resolve(10.0, 10.0, Size::new(10.0, 10.0)))
    }

    fn draw(
        &self,
        _tree: &Tree,
        _renderer: &mut iced::Renderer,
        _theme: &Theme,
        _style: &adv_renderer::Style,
        _layout: Layout<'_>,
        _cursor: mouse::Cursor,
        _viewport: &Rectangle,
    ) {
    }

    fn update(
        &mut self,
        _tree: &mut Tree,
        event: &Event,
        _layout: Layout<'_>,
        _cursor: mouse::Cursor,
        _renderer: &iced::Renderer,
        _clipboard: &mut dyn Clipboard,
        shell: &mut iced::advanced::Shell<'_, M>,
        _viewport: &Rectangle,
    ) {
        if let Event::Keyboard(keyboard::Event::KeyPressed {
            key: keyboard::Key::Named(keyboard::key::Named::Escape),
            ..
        }) = event
        {
            shell.publish(M::Item);
            shell.capture_event();
        }
    }
}

#[test]
fn escape_goes_to_content_first() {
    let mut r = renderer();
    let at = Point::new(10.0, 10.0);
    let mut ui = UserInterface::build(
        w::modal(
            text("base"),
            Some(Element::new(EscapeEater)),
            Some(M::Dismiss),
        ),
        SIZE,
        Cache::default(),
        &mut r,
    );
    step(&mut ui, &mut r, &[redraw()], at);
    let (msgs, _) = step(&mut ui, &mut r, &[escape()], at);
    assert_eq!(msgs, vec![M::Item]);
    let mut ui = UserInterface::build(
        w::modal(text("base"), Some(text("plain").into()), Some(M::Dismiss)),
        SIZE,
        Cache::default(),
        &mut r,
    );
    let (msgs, _) = step(&mut ui, &mut r, &[escape()], at);
    assert_eq!(msgs, vec![M::Dismiss]);
}

#[test]
fn keyed_modal_resets_replaced_dialog_state() {
    let mut r = renderer();
    let id = Id::new("field");
    let view = |key: &'static str, id: &Id| -> Element<'static, M> {
        w::modal_keyed(
            text("base"),
            Some((
                key,
                container(text_input("x", "").id(id.clone()).on_input(M::Input))
                    .width(300)
                    .into(),
            )),
            Some(M::Dismiss),
        )
    };
    let at = Point::new(10.0, 10.0);
    let mut ui = UserInterface::build(view("a", &id), SIZE, Cache::default(), &mut r);
    step(&mut ui, &mut r, &[redraw()], at);
    focus(&mut ui, &r, id.clone());
    let cache = ui.into_cache();
    let mut ui = UserInterface::build(view("a", &id), SIZE, cache, &mut r);
    assert!(is_focused(&mut ui, &r, id.clone()), "same key keeps state");
    let cache = ui.into_cache();
    let mut ui = UserInterface::build(view("b", &id), SIZE, cache, &mut r);
    assert!(!is_focused(&mut ui, &r, id.clone()), "new key starts fresh");
}

fn drag_split_releasing_over(overlay_at: Point, view: impl Fn() -> Element<'static, M>) {
    let mut r = renderer();
    let mut ui = UserInterface::build(view(), SIZE, Cache::default(), &mut r);
    let handle = Point::new(280.0 + w::HANDLE_WIDTH / 2.0, 20.0);
    step(&mut ui, &mut r, &[moved(handle), press()], handle);
    let (msgs, _) = step(&mut ui, &mut r, &[moved(overlay_at)], overlay_at);
    assert!(
        msgs.iter().any(|m| matches!(m, M::Resize(_))),
        "dragging: {msgs:?}"
    );
    step(&mut ui, &mut r, &[release()], overlay_at);
    let later = Point::new(overlay_at.x - 40.0, overlay_at.y);
    let (msgs, _) = step(&mut ui, &mut r, &[moved(later)], later);
    assert!(
        !msgs.iter().any(|m| matches!(m, M::Resize(_))),
        "drag ended: {msgs:?}"
    );
}

#[test]
fn drag_released_over_toast_ends() {
    static LIST: std::sync::LazyLock<Vec<w::Toast>> = std::sync::LazyLock::new(|| toast_list(1));
    let on_card = Point::new(SIZE.width - 20.0 - 250.0, SIZE.height - 20.0 - 20.0);
    drag_split_releasing_over(on_card, || {
        w::toasts(
            w::split(
                text("side"),
                text("content"),
                280.0,
                100.0,
                780.0,
                M::Resize,
            ),
            &LIST,
            M::Close,
        )
    });
}

#[test]
fn drag_released_over_popup_ends() {
    static ENTRIES: std::sync::LazyLock<Vec<w::MenuEntry<M>>> = std::sync::LazyLock::new(|| {
        vec![
            w::MenuEntry::Header("Group".into()),
            w::MenuItem::new("Item", M::Item).into(),
        ]
    });
    // Popup anchored at the top of the content pane, below a 30px trigger.
    drag_split_releasing_over(Point::new(420.0, 46.0), || {
        w::split(
            text("side"),
            column![w::popover_menu(
                button(text("T")).width(30).height(30).on_press(M::Trig),
                &ENTRIES,
                true,
                M::Dismiss,
                w::Placement::BelowStart,
            )]
            .padding(iced::Padding::ZERO.left(100.0)),
            280.0,
            100.0,
            780.0,
            M::Resize,
        )
    });
}

#[test]
fn bidi_ellipsis_is_cheap_and_correct() {
    let mut r = renderer();
    // An RLO-prefixed name must not collapse to just "…".
    let el: Element<'_, M> =
        container(w::ellipsis("\u{202E}gnp.exe some text that is long").width(Length::Shrink))
            .width(120)
            .into();
    let mut ui: Ui<'_> = UserInterface::build(el, SIZE, Cache::default(), &mut r);
    let mut op = TextProbe(Vec::new());
    ui.operate(&r, &mut op);
    assert!(op.0[0].0.width > 60.0, "{:?}", op.0[0].0);
    draw(&mut ui, &mut r, mouse::Cursor::Unavailable);
    screenshot(&mut r);

    let rtl = "مرحبا بالعالم هذا نص طويل جدا للاختبار وهو أطول من العرض المتاح ".repeat(4);
    let rows = |width: f32, long: &str| -> Element<'static, M> {
        let mut col = column![];
        for i in 0..1000 {
            let mut p = w::RoomListItemProps::new(
                w::AvatarSource::initials(format!("غرفة {i}"), format!("r{i}")),
                format!("غرفة طويلة جدا رقم {i} مع لاحقة"),
                M::Noop,
            );
            p.subtitle = Some(long.to_owned());
            col = col.push(w::room_list_item(p));
        }
        container(scrollable(col)).width(width).into()
    };
    let ui = UserInterface::build(rows(300.0, &rtl), SIZE, Cache::default(), &mut r);
    let mut cache = ui.into_cache();
    let mut worst = Duration::ZERO;
    for width in [301.0, 302.0, 303.0, 304.0, 303.0, 302.0] {
        let t = Instant::now();
        let ui = UserInterface::build(rows(width, &rtl), SIZE, cache, &mut r);
        worst = worst.max(t.elapsed());
        cache = ui.into_cache();
    }
    eprintln!("1000 RTL rooms, one drag step: {worst:?}");
    let limit = if cfg!(debug_assertions) { 4000 } else { 250 };
    assert!(worst < Duration::from_millis(limit), "{worst:?}");
}

fn right_press() -> Event {
    Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Right))
}

/// A context-menu item chosen from the keyboard closes the menu: later
/// Enters (e.g. sending from the composer) must not fire it again.
#[test]
fn context_menu_keyboard_selection_closes_it() {
    use keyboard::key::Named;
    let entries = vec![
        w::MenuItem::new("One", M::Close(1)).into(),
        w::MenuItem::new("Two", M::Close(2)).into(),
    ];
    let mut r = renderer();
    let view = |entries: &[w::MenuEntry<M>]| -> Element<'static, M> {
        column![w::context_menu(
            container(text("row")).width(Length::Fill).height(100),
            entries.to_vec(),
        )]
        .into()
    };
    let mut ui = UserInterface::build(view(&entries), SIZE, Cache::default(), &mut r);
    let at = Point::new(100.0, 50.0);
    step(&mut ui, &mut r, &[moved(at), right_press()], at);
    let away = Point::new(700.0, 500.0);
    let (msgs, _) = step(
        &mut ui,
        &mut r,
        &[key(Named::ArrowDown, false), key(Named::Enter, false)],
        at,
    );
    assert_eq!(msgs, vec![M::Close(1)]);
    for i in 0..3 {
        let cache = ui.into_cache();
        ui = UserInterface::build(view(&entries), SIZE, cache, &mut r);
        let (msgs, _) = step(&mut ui, &mut r, &[key(Named::Enter, false)], away);
        assert!(msgs.is_empty(), "enter #{i} after selection: {msgs:?}");
    }
    // Reopening starts without a stale highlight.
    step(&mut ui, &mut r, &[moved(at), right_press()], at);
    let (msgs, _) = step(&mut ui, &mut r, &[key(Named::Enter, false)], at);
    assert!(msgs.is_empty(), "{msgs:?}");
    // Escape and clicks outside close it too.
    step(&mut ui, &mut r, &[escape()], at);
    let (msgs, _) = step(
        &mut ui,
        &mut r,
        &[key(Named::ArrowDown, false), key(Named::Enter, false)],
        at,
    );
    assert!(msgs.is_empty(), "closed by Escape: {msgs:?}");
    // Mouse selection still works.
    step(&mut ui, &mut r, &[moved(at), right_press()], at);
    let item = Point::new(at.x + 40.0, at.y + 18.0);
    let (msgs, _) = step(&mut ui, &mut r, &[moved(item), press(), release()], item);
    assert_eq!(msgs, vec![M::Close(1)]);
    let (msgs, _) = step(
        &mut ui,
        &mut r,
        &[key(Named::ArrowDown, false), key(Named::Enter, false)],
        at,
    );
    assert!(msgs.is_empty(), "closed by a click: {msgs:?}");
}

/// Up/Down reach the suggestion list only while its own field has focus.
#[test]
fn arrow_keys_only_act_for_the_focused_field() {
    use keyboard::key::Named;
    let mut r = renderer();
    let view = || -> Element<'static, M> {
        column![
            text_input("name", "").id("name").on_input(M::Input),
            w::arrow_keys(
                text_input("members", "").id("members").on_input(M::Input),
                |d| M::Close(if d < 0 { 0 } else { 1 }),
            ),
        ]
        .into()
    };
    let mut ui = UserInterface::build(view(), SIZE, Cache::default(), &mut r);
    let at = Point::new(700.0, 500.0);
    focus(&mut ui, &r, Id::new("name"));
    let (msgs, _) = step(&mut ui, &mut r, &[key(Named::ArrowDown, false)], at);
    assert!(msgs.is_empty(), "{msgs:?}");
    focus(&mut ui, &r, Id::new("members"));
    let (msgs, _) = step(
        &mut ui,
        &mut r,
        &[key(Named::ArrowDown, false), key(Named::ArrowUp, false)],
        at,
    );
    assert_eq!(msgs, vec![M::Close(1), M::Close(0)]);
}

const DRAG_HEADER_H: f32 = 27.0;
const DRAG_ROW_H: f32 = 40.0;

/// Sections `keys` (a header button and `rows` rows each) in a reorder area,
/// in a 300 px wide scrollable of `height` placed `top` px down; free space
/// to its right.
fn reorder_list(keys: &[u64], rows: usize, top: f32, height: f32) -> Element<'static, M> {
    let mut col = column![].spacing(0);
    let mut handles = Vec::new();
    let mut y = 0.0;
    for &k in keys {
        handles.push((k, y, DRAG_HEADER_H));
        let header = button(text(format!("S{k}")))
            .width(Length::Fill)
            .height(DRAG_HEADER_H)
            .on_press(M::Toggle(k));
        col = col.push(w::context_menu(
            header,
            vec![w::MenuItem::new("Menu", M::Close(k)).into()],
        ));
        for _ in 0..rows {
            col = col.push(
                button(text("row"))
                    .width(Length::Fill)
                    .height(DRAG_ROW_H)
                    .on_press(M::Item),
            );
        }
        y += DRAG_HEADER_H + rows as f32 * DRAG_ROW_H;
    }
    let area = w::reorder_area(col, handles, M::Reorder).on_auto_scroll(M::AutoScroll);
    column![
        space().height(top),
        iced::widget::row![
            container(scrollable(area)).width(300).height(height),
            space().width(Length::Fill),
        ],
    ]
    .into()
}

/// Screen y of the centre of header `i` (sections with `rows` rows, list at `top`).
fn header_y(i: usize, rows: usize, top: f32) -> f32 {
    top + i as f32 * (DRAG_HEADER_H + rows as f32 * DRAG_ROW_H) + DRAG_HEADER_H / 2.0
}

fn reorders(msgs: &[M]) -> Vec<M> {
    msgs.iter()
        .filter(|m| matches!(m, M::Reorder(..) | M::Toggle(_)))
        .cloned()
        .collect()
}

/// Presses at `from`, moves to each of `path`, releases at the last point;
/// returns the toggles and reorders.
fn drag_path(ui: &mut Ui<'_>, r: &mut iced::Renderer, from: Point, path: &[Point]) -> Vec<M> {
    let mut all = Vec::new();
    let (msgs, _) = step(ui, r, &[moved(from), press()], from);
    all.extend(msgs);
    let mut at = from;
    for &p in path {
        let (msgs, _) = step(ui, r, &[moved(p)], p);
        all.extend(msgs);
        at = p;
    }
    let (msgs, _) = step(ui, r, &[release()], at);
    all.extend(msgs);
    reorders(&all)
}

#[test]
fn reorder_click_toggles_and_drag_reorders() {
    let mut r = renderer();
    let mut ui = UserInterface::build(
        reorder_list(&[1, 2, 3], 2, 0.0, 600.0),
        SIZE,
        Cache::default(),
        &mut r,
    );
    let x = 100.0;
    let h0 = Point::new(x, header_y(0, 2, 0.0));
    // A small move is still a click.
    let msgs = drag_path(&mut ui, &mut r, h0, &[Point::new(x, h0.y + 3.0)]);
    assert_eq!(msgs, vec![M::Toggle(1)]);
    // Just below the second header's centre: before the third.
    let h1 = header_y(1, 2, 0.0);
    let msgs = drag_path(&mut ui, &mut r, h0, &[Point::new(x, h1 + 5.0)]);
    assert_eq!(msgs, vec![M::Reorder(1, Some(3))]);
    // Over a long section's rows: after it (here the end).
    let msgs = drag_path(
        &mut ui,
        &mut r,
        h0,
        &[Point::new(x, header_y(2, 2, 0.0) + 50.0)],
    );
    assert_eq!(msgs, vec![M::Reorder(1, None)]);
    // Upwards.
    let h2 = Point::new(x, header_y(2, 2, 0.0));
    let msgs = drag_path(&mut ui, &mut r, h2, &[Point::new(x, h0.y - 5.0)]);
    assert_eq!(msgs, vec![M::Reorder(3, Some(1))]);
    // Grabbing while dragging, and hover styles are not shown: the hovered
    // second header looks as when the cursor is away.
    let probe = Point::new(250.0, h1);
    let away = Point::new(500.0, h1);
    step(&mut ui, &mut r, &[moved(away), redraw()], away);
    let plain = pixel(&mut r, probe);
    let p = Point::new(x, h1);
    step(&mut ui, &mut r, &[moved(p), redraw()], p);
    assert_ne!(pixel(&mut r, probe), plain, "hover is visible when idle");
    step(&mut ui, &mut r, &[moved(h0), press()], h0);
    let (_, interaction) = step(&mut ui, &mut r, &[moved(p), redraw()], p);
    assert_eq!(interaction, mouse::Interaction::Grabbing);
    assert_eq!(pixel(&mut r, probe), plain);
    step(&mut ui, &mut r, &[release()], p);
    let (_, interaction) = step(&mut ui, &mut r, &[moved(p)], p);
    assert_ne!(interaction, mouse::Interaction::Grabbing);
}

#[test]
fn reorder_release_over_own_slot_or_outside_does_nothing() {
    let mut r = renderer();
    let mut ui = UserInterface::build(
        reorder_list(&[1, 2, 3], 2, 0.0, 600.0),
        SIZE,
        Cache::default(),
        &mut r,
    );
    let x = 100.0;
    let h1 = Point::new(x, header_y(1, 2, 0.0));
    // Over its own rows and just above itself: both its own slots.
    for to in [h1.y + 60.0, h1.y - 30.0] {
        let msgs = drag_path(&mut ui, &mut r, h1, &[Point::new(x, to)]);
        assert!(msgs.is_empty(), "{to}: {msgs:?}");
    }
    // Released beside the list: the drag ends without a drop.
    let msgs = drag_path(
        &mut ui,
        &mut r,
        h1,
        &[Point::new(x, 10.0), Point::new(500.0, 10.0)],
    );
    assert!(msgs.is_empty(), "{msgs:?}");
    let back = Point::new(x, 300.0);
    let (msgs, interaction) = step(&mut ui, &mut r, &[moved(back), release()], back);
    assert!(reorders(&msgs).is_empty(), "drag ended: {msgs:?}");
    assert_ne!(interaction, mouse::Interaction::Grabbing);
    // Clicks work again.
    let msgs = drag_path(&mut ui, &mut r, h1, &[]);
    assert_eq!(msgs, vec![M::Toggle(2)]);
}

#[test]
fn reorder_cancels_on_escape_other_presses_and_unfocus() {
    let mut r = renderer();
    let mut ui = UserInterface::build(
        reorder_list(&[1, 2, 3], 2, 0.0, 600.0),
        SIZE,
        Cache::default(),
        &mut r,
    );
    use keyboard::key::Named;
    let x = 100.0;
    let h0 = Point::new(x, header_y(0, 2, 0.0));
    let to = Point::new(x, header_y(2, 2, 0.0) + 5.0);
    let unfocused = Event::Window(window::Event::Unfocused);
    for cancel in [escape(), right_press(), unfocused] {
        step(&mut ui, &mut r, &[moved(h0), press()], h0);
        let (msgs, _) = step(&mut ui, &mut r, &[moved(to), cancel.clone()], to);
        let (later, interaction) = step(&mut ui, &mut r, &[release()], to);
        assert!(
            reorders(&msgs).is_empty() && reorders(&later).is_empty(),
            "{cancel:?}: {msgs:?} {later:?}"
        );
        assert_ne!(interaction, mouse::Interaction::Grabbing, "{cancel:?}");
        // No context menu opened mid-drag.
        let (msgs, _) = step(
            &mut ui,
            &mut r,
            &[key(Named::ArrowDown, false), key(Named::Enter, false)],
            to,
        );
        assert!(msgs.is_empty(), "{cancel:?}: {msgs:?}");
    }
    // The header still clicks afterwards.
    assert_eq!(drag_path(&mut ui, &mut r, h0, &[]), vec![M::Toggle(1)]);
}

#[test]
fn reorder_passes_right_presses_to_the_context_menu() {
    use keyboard::key::Named;
    let mut r = renderer();
    let mut ui = UserInterface::build(
        reorder_list(&[1, 2, 3], 2, 0.0, 600.0),
        SIZE,
        Cache::default(),
        &mut r,
    );
    let h1 = Point::new(100.0, header_y(1, 2, 0.0));
    step(&mut ui, &mut r, &[moved(h1), right_press()], h1);
    let (msgs, _) = step(
        &mut ui,
        &mut r,
        &[key(Named::ArrowDown, false), key(Named::Enter, false)],
        h1,
    );
    assert_eq!(msgs, vec![M::Close(2)]);
}

/// The dragged section and the drop target are found by key: sections
/// that appear or vanish mid-drag do not shift them.
#[test]
fn reorder_survives_handles_changing_mid_drag() {
    let mut r = renderer();
    let x = 100.0;
    let build = |ui: Option<Ui<'_>>, keys: &[u64], r: &mut iced::Renderer| {
        let cache = ui.map(Ui::into_cache).unwrap_or_default();
        UserInterface::build(reorder_list(keys, 2, 0.0, 600.0), SIZE, cache, r)
    };
    let mut ui = build(None, &[1, 2, 3], &mut r);
    let h0 = Point::new(x, header_y(0, 2, 0.0));
    step(&mut ui, &mut r, &[moved(h0), press()], h0);
    let p = Point::new(x, header_y(1, 2, 0.0) + 5.0);
    step(&mut ui, &mut r, &[moved(p)], p);
    // Section 2 vanished: 3 is now the second header.
    let mut ui = build(Some(ui), &[1, 3], &mut r);
    let p = Point::new(x, header_y(1, 2, 0.0) - 5.0);
    step(&mut ui, &mut r, &[moved(p)], p);
    let (msgs, _) = step(&mut ui, &mut r, &[release()], p);
    assert!(reorders(&msgs).is_empty(), "own slot: {msgs:?}");
    // A new section above: the drop still names keys.
    let mut ui = build(Some(ui), &[1, 3], &mut r);
    step(&mut ui, &mut r, &[moved(h0), press()], h0);
    let p = Point::new(x, header_y(1, 2, 0.0) + 5.0);
    step(&mut ui, &mut r, &[moved(p)], p);
    let mut ui = build(Some(ui), &[4, 1, 3], &mut r);
    let p = Point::new(x, header_y(2, 2, 0.0) + 5.0);
    let (msgs, _) = step(&mut ui, &mut r, &[moved(p), release()], p);
    assert_eq!(reorders(&msgs), vec![M::Reorder(1, None)]);
    // The dragged section vanished: no drop.
    let mut ui = build(Some(ui), &[1, 3], &mut r);
    step(&mut ui, &mut r, &[moved(h0), press()], h0);
    let p = Point::new(x, header_y(1, 2, 0.0) + 5.0);
    step(&mut ui, &mut r, &[moved(p)], p);
    let mut ui = build(Some(ui), &[3], &mut r);
    let (msgs, interaction) = step(&mut ui, &mut r, &[moved(h0), release()], h0);
    assert!(reorders(&msgs).is_empty(), "{msgs:?}");
    assert_ne!(interaction, mouse::Interaction::Grabbing);
}

/// In a scrolled list the cursor may leave the scrollable: positions stay
/// in content coordinates and the list auto-scrolls (throttled).
#[test]
fn reorder_in_a_scrolled_list_beyond_its_bounds() {
    let mut r = renderer();
    let keys = [1, 2, 3, 4, 5, 6];
    let (top, height, rows) = (100.0, 150.0, 1);
    let mut ui = UserInterface::build(
        reorder_list(&keys, rows, top, height),
        SIZE,
        Cache::default(),
        &mut r,
    );
    let x = 100.0;
    let over = Point::new(x, top + 50.0);
    step(&mut ui, &mut r, &[moved(over), wheel(-4.0)], over);
    let t = scroll_offsets(&mut ui, &r)[0].y;
    let last = header_y(5, rows, top) - t;
    assert!(last > top && last < top + height, "offset {t}");
    // Drag the last header above the list.
    let from = Point::new(x, last);
    step(&mut ui, &mut r, &[moved(from), press()], from);
    let above = Point::new(x, top - 50.0);
    let before = Instant::now();
    let (msgs, interaction) = step(&mut ui, &mut r, &[moved(above)], above);
    assert_eq!(interaction, mouse::Interaction::Grabbing);
    assert!(
        matches!(auto_scrolls(&msgs)[..], [dy] if dy < 0.0),
        "{msgs:?}"
    );
    let redraw_in = |ms: u64| {
        Event::Window(window::Event::RedrawRequested(
            before + Duration::from_millis(ms),
        ))
    };
    // Throttled; and a scroll that did not move the list is not repeated.
    let (msgs, _) = step(&mut ui, &mut r, &[redraw_in(0)], above);
    assert!(msgs.is_empty(), "{msgs:?}");
    let (msgs, _) = step(&mut ui, &mut r, &[redraw_in(200)], above);
    assert!(msgs.is_empty(), "{msgs:?}");
    // Once the list moved, the next tick scrolls again, also after the
    // cursor briefly left the edge zone.
    scroll_by(&mut ui, &r, -10.0);
    let (msgs, _) = step(&mut ui, &mut r, &[redraw_in(400)], above);
    assert!(
        matches!(auto_scrolls(&msgs)[..], [dy] if dy < 0.0),
        "{msgs:?}"
    );
    scroll_by(&mut ui, &r, -10.0);
    let mid = Point::new(x, top + height / 2.0);
    let (msgs, _) = step(&mut ui, &mut r, &[moved(mid)], mid);
    assert!(msgs.is_empty(), "{msgs:?}");
    let (msgs, _) = step(&mut ui, &mut r, &[redraw_in(410)], above);
    assert!(msgs.is_empty(), "within the tick: {msgs:?}");
    let (msgs, _) = step(&mut ui, &mut r, &[redraw_in(600)], above);
    assert!(
        matches!(auto_scrolls(&msgs)[..], [dy] if dy < 0.0),
        "{msgs:?}"
    );
    let t = scroll_offsets(&mut ui, &r)[0].y;
    // The slot follows the content y of the levitating cursor.
    let rel = above.y - top + t;
    let section = DRAG_HEADER_H + rows as f32 * DRAG_ROW_H;
    let slot = (0..keys.len())
        .filter(|i| *i as f32 * section + DRAG_HEADER_H / 2.0 < rel)
        .count();
    assert!(slot < 5, "{slot}");
    let (msgs, _) = step(&mut ui, &mut r, &[release()], above);
    assert_eq!(reorders(&msgs), vec![M::Reorder(6, Some(keys[slot]))]);
    // Wheel scrolling still reaches the scrollable during a drag.
    let first = Point::new(x, header_y(4, rows, top) - t);
    step(&mut ui, &mut r, &[moved(first), press()], first);
    let p = Point::new(x, first.y + 20.0);
    step(&mut ui, &mut r, &[moved(p), wheel(1.0)], p);
    assert!(scroll_offsets(&mut ui, &r)[0].y < t, "wheel scrolled up");
    step(&mut ui, &mut r, &[escape()], p);
}

/// A drag that starts in an edge zone does not auto-scroll until the cursor
/// heads for that edge.
#[test]
fn reorder_auto_scroll_waits_for_the_cursor_to_head_for_the_edge() {
    let mut r = renderer();
    let keys = [1, 2, 3, 4, 5, 6];
    let (top, height, rows) = (100.0, 150.0, 1);
    let mut ui = UserInterface::build(
        reorder_list(&keys, rows, top, height),
        SIZE,
        Cache::default(),
        &mut r,
    );
    let x = 100.0;
    let section = DRAG_HEADER_H + rows as f32 * DRAG_ROW_H;
    // The third header at the top of the viewport.
    scroll_by(&mut ui, &r, 2.0 * section);
    assert_eq!(scroll_offsets(&mut ui, &r)[0].y, 2.0 * section);
    let from = Point::new(x, top + DRAG_HEADER_H / 2.0);
    step(&mut ui, &mut r, &[moved(from), press()], from);
    // Away from the top edge, still in its zone.
    let down = Point::new(x, from.y + 8.0);
    let (msgs, interaction) = step(&mut ui, &mut r, &[moved(down)], down);
    assert_eq!(interaction, mouse::Interaction::Grabbing);
    assert!(msgs.is_empty(), "{msgs:?}");
    let later = Event::Window(window::Event::RedrawRequested(
        Instant::now() + Duration::from_millis(200),
    ));
    let (msgs, _) = step(&mut ui, &mut r, &[later], down);
    assert!(msgs.is_empty(), "{msgs:?}");
    // Towards the top edge: scrolls up.
    let up = Point::new(x, top + 2.0);
    let (msgs, _) = step(&mut ui, &mut r, &[moved(up)], up);
    assert!(
        matches!(auto_scrolls(&msgs)[..], [dy] if dy < 0.0),
        "{msgs:?}"
    );
    step(&mut ui, &mut r, &[escape()], up);
}

/// Number of drawn pixels within 2 per channel of `c`.
fn pixels_of(r: &mut iced::Renderer, c: Color) -> usize {
    let shot = r.screenshot(
        Size::new(SIZE.width as u32, SIZE.height as u32),
        1.0,
        Color::WHITE,
    );
    let want = c.into_rgba8();
    shot.as_chunks::<4>()
        .0
        .iter()
        .filter(|px| (0..3).all(|i| px[i].abs_diff(want[i]) <= 2))
        .count()
}

/// Flag marks on rows, the header swatch and menu swatches are drawn in
/// their palette colour, and only where a flag is set.
#[test]
fn flags_are_drawn_in_their_colour() {
    let t = &rustket_ui::theme::LIGHT;
    let view = |flagged: bool| -> Element<'static, M> {
        let mut p = w::RoomListItemProps::new(
            w::AvatarSource::initials("general", "g"),
            "general",
            M::Item,
        );
        let mut entries = vec![w::MenuEntry::Header("Flag".into())];
        for i in [1usize, 5] {
            let mut item = w::MenuItem::new(format!("Flag {i}"), M::Noop).checked(i == 1);
            if flagged {
                item = item.swatch(i);
            }
            entries.push(item.into());
        }
        if flagged {
            p.flag = Some(w::FlagMark::new(0, "Red"));
        }
        column![
            w::section_header_full(
                "Urgent",
                false,
                Some(1),
                flagged.then_some(4),
                None,
                M::Toggle(1)
            ),
            w::room_list_item(p),
            w::menu_panel(&entries),
        ]
        .width(300)
        .into()
    };
    let mut r = renderer();
    for flagged in [false, true] {
        let mut ui = UserInterface::build(view(flagged), SIZE, Cache::default(), &mut r);
        let away = Point::new(700.0, 500.0);
        step(&mut ui, &mut r, &[redraw()], away);
        for i in [0, 4, 1, 5] {
            let n = pixels_of(&mut r, t.flag[i]);
            if flagged {
                assert!(n >= 12, "flag {i}: {n} pixels");
            } else {
                assert_eq!(n, 0, "flag {i} without a flag");
            }
        }
    }
}

#[test]
fn menu_width_makes_room_for_swatches() {
    load_fonts();
    let label = "Waiting for reply from the team";
    let plain = vec![w::MenuEntry::from(w::MenuItem::new(label, M::Noop))];
    let swatched = vec![w::MenuEntry::from(
        w::MenuItem::new(label, M::Noop).swatch(2),
    )];
    assert!(w::menu_width(&swatched) > w::menu_width(&plain));
}
