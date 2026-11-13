//! Shared frame of full-page screens (settings, account, directory).

use iced::widget::{column, container, row, scrollable, text};
use iced::{Alignment, Element, Length};
use rust_i18n::t;
use rustket_ui::icon::Icon;
use rustket_ui::style;
use rustket_ui::theme::{SPACE_4, SPACE_6, TEXT_TITLE, font_semibold};
use rustket_ui::widget::{divider, empty_state, icon_button};

/// The page body's scrollable.
pub const SCROLL_ID: &str = "page-scroll";

/// Title bar with a close button above `body`.
pub fn page<'a, M: Clone + 'a>(title: String, body: Element<'a, M>, on_close: M) -> Element<'a, M> {
    container(
        column![
            row![
                text(title)
                    .size(TEXT_TITLE)
                    .font(font_semibold())
                    .width(Length::Fill),
                icon_button(Icon::X, Some(on_close), t!("app.close")),
            ]
            .align_y(Alignment::Center)
            .padding([SPACE_4, SPACE_6]),
            divider(),
            scrollable(container(body).padding(SPACE_6).width(Length::Fill))
                .id(SCROLL_ID)
                .height(Length::Fill)
                .style(style::scrollable),
        ]
        .height(Length::Fill),
    )
    .width(Length::Fill)
    .height(Length::Fill)
    .style(style::container_room)
    .into()
}

/// "Coming soon" body of a stub.
pub fn placeholder<'a, M: 'a>(what: String) -> Element<'a, M> {
    container(empty_state(Icon::Info, what, t!("app.not_yet")))
        .height(360)
        .into()
}
