//! Keyboard focus around the right panel.
#![allow(clippy::unwrap_used)]
use iced::advanced::widget::{Id, Operation, operation};
use iced::widget::{column, text_input};
use iced::{Element, Event, Point, Rectangle, Size, mouse};
use iced_runtime::user_interface::{Cache, UserInterface};
use rustket_ui::widget::side_panel;

#[derive(Debug, Clone)]
enum M {
    Input,
}

/// Room column with an input at its left edge; panel with an input.
fn view(side_width: Option<f32>) -> Element<'static, M> {
    side_panel(
        column![text_input("", "").id("main").on_input(|_| M::Input)],
        column![text_input("", "").id("side").on_input(|_| M::Input)],
        side_width,
    )
}

struct Focused(Vec<Id>);
impl Operation for Focused {
    fn traverse(&mut self, operate: &mut dyn FnMut(&mut dyn Operation)) {
        operate(self);
    }
    fn focusable(&mut self, id: Option<&Id>, _: Rectangle, state: &mut dyn operation::Focusable) {
        if state.is_focused() {
            self.0.extend(id.cloned());
        }
    }
}

struct NoClip;
impl iced::advanced::Clipboard for NoClip {
    fn read(&self, _: iced::advanced::clipboard::Kind) -> Option<String> {
        None
    }
    fn write(&mut self, _: iced::advanced::clipboard::Kind, _: String) {}
}

fn renderer() -> iced::Renderer {
    iced::Renderer::Secondary(iced_tiny_skia::Renderer::new(
        rustket_ui::theme::DEFAULT_FONT,
        iced::Pixels(rustket_ui::theme::TEXT_BODY),
    ))
}

fn focus(
    ui: &mut UserInterface<'_, M, iced::Theme, iced::Renderer>,
    r: &iced::Renderer,
    id: &'static str,
) -> Vec<Id> {
    let mut op = operation::focusable::focus::<()>(Id::new(id));
    ui.operate(r, &mut op);
    let mut f = Focused(Vec::new());
    ui.operate(r, &mut f);
    f.0
}

/// A docked panel leaves the room column focusable; one drawn over it (too
/// narrow for both) hides the room column's inputs from the focus.
#[test]
fn a_covering_panel_hides_the_room_column_from_the_focus() {
    let mut r = renderer();
    let mut ui = UserInterface::build(
        view(Some(300.0)),
        Size::new(800.0, 400.0),
        Cache::default(),
        &mut r,
    );
    assert_eq!(focus(&mut ui, &r, "main"), [Id::new("main")]);
    let mut ui = UserInterface::build(
        view(Some(300.0)),
        Size::new(500.0, 400.0),
        ui.into_cache(),
        &mut r,
    );
    assert!(focus(&mut ui, &r, "main").is_empty());
    assert_eq!(focus(&mut ui, &r, "side"), [Id::new("side")]);
}

/// A click into the docked panel takes the focus from the room column even
/// though the panel captures it.
#[test]
fn a_click_in_the_panel_unfocuses_the_room_column() {
    let mut r = renderer();
    let mut ui = UserInterface::build(
        view(Some(300.0)),
        Size::new(800.0, 400.0),
        Cache::default(),
        &mut r,
    );
    assert_eq!(focus(&mut ui, &r, "main"), [Id::new("main")]);
    let at = mouse::Cursor::Available(Point::new(600.0, 10.0));
    let mut out = Vec::new();
    let events = [
        Event::Mouse(mouse::Event::CursorMoved {
            position: Point::new(600.0, 10.0),
        }),
        Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)),
    ];
    let _ = ui.update(&events, at, &mut r, &mut NoClip, &mut out);
    let mut f = Focused(Vec::new());
    ui.operate(&r, &mut f);
    assert_eq!(f.0, [Id::new("side")]);
}
