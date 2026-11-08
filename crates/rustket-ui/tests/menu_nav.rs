//! Keyboard navigation of a menu column with a text field (the status menu).
#![allow(clippy::unwrap_used)]
use iced::advanced::widget::Id as WId;
use iced::widget::{button, column, text, text_input};
use iced::{Element, Event, Size, keyboard, mouse};
use iced_runtime::user_interface::{Cache, UserInterface};
use rustket_ui::widget::{Placement, menu_nav, popover};

#[derive(Debug, Clone, PartialEq)]
enum M {
    Input(String),
    Press(&'static str),
    Dismiss,
    Submit,
}

fn key(named: keyboard::key::Named, txt: Option<&str>) -> Event {
    let k = keyboard::Key::Named(named);
    Event::Keyboard(keyboard::Event::KeyPressed {
        key: k.clone(),
        modified_key: k,
        physical_key: keyboard::key::Physical::Unidentified(
            keyboard::key::NativeCode::Unidentified,
        ),
        location: keyboard::Location::Standard,
        modifiers: keyboard::Modifiers::empty(),
        text: txt.map(Into::into),
        repeat: false,
    })
}

struct NoClip;
impl iced::advanced::Clipboard for NoClip {
    fn read(&self, _: iced::advanced::clipboard::Kind) -> Option<String> {
        None
    }
    fn write(&mut self, _: iced::advanced::clipboard::Kind, _: String) {}
}

fn view(open: bool) -> Element<'static, M> {
    let menu = menu_nav(
        column![
            text("caption"),
            button(text("away")).on_press(M::Press("away")),
            text_input("", "")
                .id("menu-text")
                .on_input(M::Input)
                .on_submit(M::Submit),
            button(text("logout")).on_press(M::Press("logout")),
        ],
        vec![None, Some(M::Press("away")), None, Some(M::Press("logout"))],
    );
    popover(
        button(text("trigger")).on_press(M::Press("trigger")),
        menu,
        open,
        Placement::BelowStart,
        M::Dismiss,
    )
}

fn renderer() -> iced::Renderer {
    iced::Renderer::Secondary(iced_tiny_skia::Renderer::new(
        iced::Font::DEFAULT,
        iced::Pixels(14.0),
    ))
}

fn focus_field(ui: &mut UserInterface<'_, M, iced::Theme, iced::Renderer>, r: &iced::Renderer) {
    let mut op = iced::advanced::widget::operation::focusable::focus::<()>(WId::new("menu-text"));
    ui.operate(r, &mut op);
}

/// A highlight left from an earlier opening is dropped once the text field
/// has the focus again: Space and Enter belong to the field.
#[test]
fn a_stale_highlight_does_not_take_keys_from_the_field() {
    let mut r = renderer();
    let cursor = mouse::Cursor::Available(iced::Point::new(5.0, 5.0));
    let size = Size::new(800.0, 600.0);
    let mut out = Vec::new();

    let mut ui = UserInterface::build(view(true), size, Cache::default(), &mut r);
    focus_field(&mut ui, &r);
    for e in [
        key(keyboard::key::Named::ArrowUp, None),
        key(keyboard::key::Named::Escape, None),
    ] {
        let _ = ui.update(
            std::slice::from_ref(&e),
            cursor,
            &mut r,
            &mut NoClip,
            &mut out,
        );
    }
    assert!(out.contains(&M::Dismiss));
    out.clear();
    let ui = UserInterface::build(view(false), size, ui.into_cache(), &mut r);
    let mut ui = UserInterface::build(view(true), size, ui.into_cache(), &mut r);
    focus_field(&mut ui, &r);
    let space = key(keyboard::key::Named::Space, Some(" "));
    let _ = ui.update(
        std::slice::from_ref(&space),
        cursor,
        &mut r,
        &mut NoClip,
        &mut out,
    );
    assert!(!out.iter().any(|m| matches!(m, M::Press(_))), "{out:?}");
    out.clear();
    let enter = key(keyboard::key::Named::Enter, None);
    let _ = ui.update(
        std::slice::from_ref(&enter),
        cursor,
        &mut r,
        &mut NoClip,
        &mut out,
    );
    assert_eq!(out, [M::Submit]);
}

/// Home/End move the caret of the focused field; Down still enters the
/// rows.
#[test]
fn home_and_end_stay_in_the_text_field() {
    let mut r = renderer();
    let cursor = mouse::Cursor::Available(iced::Point::new(5.0, 5.0));
    let mut out = Vec::new();
    let mut ui = UserInterface::build(
        view(true),
        Size::new(800.0, 600.0),
        Cache::default(),
        &mut r,
    );
    focus_field(&mut ui, &r);
    let evs = [
        key(keyboard::key::Named::Home, None),
        key(keyboard::key::Named::End, None),
        key(keyboard::key::Named::Space, Some(" ")),
    ];
    let _ = ui.update(&evs, cursor, &mut r, &mut NoClip, &mut out);
    assert!(!out.iter().any(|m| matches!(m, M::Press(_))), "{out:?}");
    out.clear();
    let evs = [
        key(keyboard::key::Named::ArrowDown, None),
        key(keyboard::key::Named::Enter, None),
    ];
    let _ = ui.update(&evs, cursor, &mut r, &mut NoClip, &mut out);
    assert_eq!(out, [M::Press("away")]);
}
