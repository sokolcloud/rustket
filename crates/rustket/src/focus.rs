//! Keyboard focus between the main parts of the window.
//!
//! - Tab / Shift+Tab move through every focusable widget of the window
//!   (inputs, and buttons/rows wrapped in `rustket_ui::widget::focus_ring`);
//!   modal dialogs keep Tab inside themselves. The focused widget is
//!   scrolled into view. Nothing under a panel that covers the room column
//!   (expanded thread, narrow window) takes the focus.
//! - F6 / Shift+F6 move between the regions: sidebar (its search field),
//!   timeline (the last focusable on screen, i.e. near the newest message),
//!   composer, right panel (the thread composer when a thread is open).
//!   Regions without anything focusable are skipped. The custom title bar
//!   (workspaces, add, settings, window controls) is not an F6 region: it is
//!   the first stop of Tab instead. Ctrl+Shift+Arrow is not used: text
//!   fields select words with it.
//! - Escape closes the innermost thing: popovers and menus, then dialogs, an
//!   input's focus (an empty thread composer closes the thread), then the
//!   right panel, or the settings/account/directory/administration page.
//! - Alt+Up/Down open the previous/next room; the sidebar scrolls to it.

use iced::widget::Id;
use rustket_ui::widget::FocusRegion;

/// Container ids of the regions (one room view is shown at a time).
pub const SIDEBAR: &str = "region-sidebar";
pub const TIMELINE: &str = "region-timeline";
pub const COMPOSER: &str = "region-composer";
pub const PANEL: &str = "region-panel";

/// Widget id of the composer editor of room `rid` (thread `tmid`), as
/// `room::composer::Composer` builds it.
pub fn composer_id(rid: &str, tmid: Option<&str>) -> Id {
    Id::from(format!("composer-{rid}-{}", tmid.unwrap_or("")))
}

/// The F6 regions in order, for the open room `rid` and its open thread.
pub fn regions(room: Option<(&str, Option<&str>)>) -> Vec<FocusRegion> {
    let mut v = vec![FocusRegion::new(SIDEBAR).prefer(crate::workspace::sidebar::SEARCH_INPUT_ID)];
    if let Some((rid, tmid)) = room {
        v.push(FocusRegion::new(TIMELINE).last_visible());
        v.push(FocusRegion::new(COMPOSER).prefer(composer_id(rid, None)));
        let panel = FocusRegion::new(PANEL);
        v.push(match tmid {
            Some(t) => panel.prefer(composer_id(rid, Some(t))),
            None => panel,
        });
    }
    v
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::tree_probe::focus_after;
    use iced::advanced::widget::Id as WId;
    use iced::widget::{button, column, container, space, text, text_input};
    use iced::{Element, Event, Size, keyboard, mouse};
    use iced_runtime::user_interface::{Cache, UserInterface};
    use rustket_ui::widget::{focus_cycle, focus_region, focus_ring, menu_nav};

    #[derive(Debug, Clone, PartialEq)]
    enum M {
        Input(String),
        Press(&'static str),
    }

    fn key(named: keyboard::key::Named) -> Event {
        let k = keyboard::Key::Named(named);
        Event::Keyboard(keyboard::Event::KeyPressed {
            key: k.clone(),
            modified_key: k,
            physical_key: keyboard::key::Physical::Unidentified(
                keyboard::key::NativeCode::Unidentified,
            ),
            location: keyboard::Location::Standard,
            modifiers: keyboard::Modifiers::empty(),
            text: None,
            repeat: false,
        })
    }

    fn ring(name: &'static str) -> Element<'static, M> {
        focus_ring(
            button(text(name)).on_press(M::Press(name)),
            Some(M::Press(name)),
        )
        .id(name)
        .into()
    }

    /// Runs `events` through `view` after focusing `focused`; returns the
    /// published messages.
    fn drive(view: Element<'_, M>, focused: &'static str, events: &[Event]) -> Vec<M> {
        let mut renderer = iced::Renderer::Secondary(iced_tiny_skia::Renderer::new(
            rustket_ui::theme::font_regular(),
            iced::Pixels(rustket_ui::theme::TEXT_BODY),
        ));
        let mut ui = UserInterface::build(
            view,
            Size::new(800.0, 600.0),
            Cache::default(),
            &mut renderer,
        );
        let mut op = iced::advanced::widget::operation::focusable::focus::<()>(WId::new(focused));
        ui.operate(&renderer, &mut op);
        let mut out = Vec::new();
        for e in events {
            let _ = ui.update(
                std::slice::from_ref(e),
                mouse::Cursor::Available(iced::Point::new(700.0, 500.0)),
                &mut renderer,
                &mut NoClip,
                &mut out,
            );
        }
        out
    }

    struct NoClip;
    impl iced::advanced::Clipboard for NoClip {
        fn read(&self, _: iced::advanced::clipboard::Kind) -> Option<String> {
            None
        }
        fn write(&mut self, _: iced::advanced::clipboard::Kind, _: String) {}
    }

    fn form() -> Element<'static, M> {
        column![
            text_input("", "").id("a").on_input(M::Input),
            ring("b"),
            focus_ring(button(text("off")), None::<M>).id("disabled"),
            text_input("", "").id("c").on_input(M::Input),
        ]
        .into()
    }

    /// Tab reaches wrapped buttons (not disabled ones) between inputs.
    #[test]
    fn tab_order_includes_buttons() {
        assert_eq!(
            focus_after(form(), Some("a"), focus_cycle(true)),
            [Some(WId::new("b"))]
        );
        assert_eq!(
            focus_after(form(), Some("b"), focus_cycle(true)),
            [Some(WId::new("c"))]
        );
        assert_eq!(
            focus_after(form(), Some("c"), focus_cycle(false)),
            [Some(WId::new("b"))]
        );
        assert_eq!(
            focus_after(form(), Some("c"), focus_cycle(true)),
            [Some(WId::new("a"))]
        );
    }

    /// Enter/Space activate the focused button; a mouse press takes the
    /// keyboard focus away (no ring for mouse users).
    #[test]
    fn enter_and_space_activate_and_the_mouse_drops_focus() {
        use keyboard::key::Named;
        assert_eq!(drive(form(), "b", &[key(Named::Enter)]), [M::Press("b")]);
        assert_eq!(drive(form(), "b", &[key(Named::Space)]), [M::Press("b")]);
        assert!(drive(form(), "a", &[key(Named::Enter)]).is_empty());
        let click = Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left));
        assert!(drive(form(), "b", &[click, key(Named::Enter)]).is_empty());
    }

    fn window(thread: bool) -> Element<'static, M> {
        let panel: Element<'static, M> = if thread {
            text_input("", "")
                .id(composer_id("r", Some("t")))
                .on_input(M::Input)
                .into()
        } else {
            space().into()
        };
        column![
            container(column![
                ring("status"),
                text_input("", "")
                    .id(crate::workspace::sidebar::SEARCH_INPUT_ID)
                    .on_input(M::Input),
                ring("row"),
            ])
            .id(SIDEBAR),
            container(column![ring("reaction")]).id(TIMELINE),
            container(column![
                ring("attach"),
                text_input("", "")
                    .id(composer_id("r", None))
                    .on_input(M::Input),
            ])
            .id(COMPOSER),
            container(panel).id(PANEL),
        ]
        .into()
    }

    /// F6 / Shift+F6 cycle sidebar (search), timeline, composer (editor),
    /// panel (thread composer); empty regions are skipped.
    #[test]
    fn f6_moves_between_regions() {
        let search = || Some(WId::new(crate::workspace::sidebar::SEARCH_INPUT_ID));
        let editor = || Some(composer_id("r", None));
        let f6 = |thread: bool, from: Option<&'static str>, forward: bool| {
            let tmid = thread.then_some("t");
            focus_after(
                window(thread),
                from,
                focus_region(regions(Some(("r", tmid))), forward),
            )
        };
        assert_eq!(f6(false, None, true), [search()]);
        assert_eq!(f6(false, Some("row"), true), [Some(WId::new("reaction"))]);
        assert_eq!(f6(false, Some("reaction"), true), [editor()]);
        // The empty panel is skipped.
        assert_eq!(f6(false, Some("attach"), true), [search()]);
        assert_eq!(f6(false, Some("status"), false), [editor()]);
        assert_eq!(
            f6(true, Some("attach"), true),
            [Some(composer_id("r", Some("t")))]
        );
    }

    /// The status menu's rows: Up/Down highlight entries and take the focus
    /// from its text field, Enter selects; nothing is selected without a
    /// highlight.
    #[test]
    fn menu_rows_take_arrow_keys() {
        use keyboard::key::Named;
        let menu = || -> Element<'static, M> {
            menu_nav(
                column![
                    text("caption"),
                    button(text("online")).on_press(M::Press("online")),
                    text_input("", "").id("menu-text").on_input(M::Input),
                    button(text("away")).on_press(M::Press("away")),
                ],
                vec![None, Some(M::Press("online")), None, Some(M::Press("away"))],
            )
        };
        assert_eq!(
            drive(
                menu(),
                "menu-text",
                &[key(Named::ArrowDown), key(Named::Enter)]
            ),
            [M::Press("online")]
        );
        assert_eq!(
            drive(
                menu(),
                "menu-text",
                &[key(Named::ArrowUp), key(Named::Enter)]
            ),
            [M::Press("away")]
        );
        assert_eq!(
            drive(
                menu(),
                "menu-text",
                &[
                    key(Named::ArrowDown),
                    key(Named::ArrowDown),
                    key(Named::ArrowDown),
                    key(Named::Space)
                ]
            ),
            [M::Press("online")],
            "wraps around"
        );
        assert!(drive(menu(), "menu-text", &[key(Named::Enter)]).is_empty());
    }

    /// A message row's hover toolbar reached with Tab is shown and takes
    /// Enter although the mouse is elsewhere.
    #[test]
    fn hover_toolbars_open_for_keyboard_focus() {
        use keyboard::key::Named;
        let row = || -> Element<'static, M> {
            rustket_ui::widget::hover_box(
                container(text("message"))
                    .width(iced::Length::Fill)
                    .height(40),
                vec![ring("react")],
                |_, _| iced::widget::container::Style::default(),
            )
            .into()
        };
        let redraw = Event::Window(iced::window::Event::RedrawRequested(
            std::time::Instant::now(),
        ));
        assert_eq!(
            drive(row(), "react", &[redraw, key(Named::Enter)]),
            [M::Press("react")]
        );
        assert_eq!(
            focus_after(row(), None, focus_cycle(true)),
            [Some(WId::new("react"))]
        );
    }

    #[test]
    fn composer_ids_match_the_composer() {
        let c = crate::room::composer::Composer::new("r1");
        assert_eq!(c.editor_id(), &composer_id("r1", None));
        let t = crate::room::composer::Composer::for_thread("r1", "t1");
        assert_eq!(t.editor_id(), &composer_id("r1", Some("t1")));
        assert_eq!(regions(None).len(), 1);
        assert_eq!(regions(Some(("r1", Some("t1")))).len(), 4);
    }
}
