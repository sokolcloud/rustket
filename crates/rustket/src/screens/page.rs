//! Shared frame of full-page screens (settings, account, directory, admin).

use iced::widget::{Row, button, column, container, responsive, row, scrollable, text};
use iced::{Alignment, Element, Length, Theme};
use rust_i18n::t;
use rustket_ui::icon::{self, Icon};
use rustket_ui::style;
use rustket_ui::theme::{
    SPACE_1, SPACE_2, SPACE_4, SPACE_6, TEXT_BODY, TEXT_CAPTION, TEXT_TITLE, font_semibold, tokens,
};
use rustket_ui::widget::{divider, empty_state, focus_ring, icon_button};

/// The page body's scrollable.
pub const SCROLL_ID: &str = "page-scroll";

/// Width of a page's left navigation list.
pub const NAV_WIDTH: f32 = 232.0;

/// Narrowest content next to the navigation list; below it the entries are
/// shown as a wrapping row above the content.
pub const MIN_CONTENT_WIDTH: f32 = 420.0;

/// In the narrow layout, below this content height the wrapping entries
/// scroll with the body instead of taking a fixed share of the height.
pub const MIN_PINNED_NAV_HEIGHT: f32 = 400.0;

/// Title bar with a close button above `body`.
pub fn page<'a, M: Clone + 'a>(title: String, body: Element<'a, M>, on_close: M) -> Element<'a, M> {
    frame(title, scroll(body), on_close)
}

/// Title bar with a close button above `content`.
fn frame<'a, M: Clone + 'a>(title: String, content: Element<'a, M>, on_close: M) -> Element<'a, M> {
    let head = row![
        text(title)
            .size(TEXT_TITLE)
            .font(font_semibold())
            .width(Length::Fill),
        icon_button(Icon::X, Some(on_close), t!("app.close")),
    ]
    .align_y(Alignment::Center)
    .padding([SPACE_4, SPACE_6]);
    container(column![head, divider(), content].height(Length::Fill))
        .width(Length::Fill)
        .height(Length::Fill)
        .style(style::container_room)
        .into()
}

/// The padded, scrolled page body (id [`SCROLL_ID`]).
fn scroll<'a, M: 'a>(body: Element<'a, M>) -> Element<'a, M> {
    scroll_with_head(None, body)
}

/// [`scroll`] with `head` scrolled above the padded body.
fn scroll_with_head<'a, M: 'a>(
    head: Option<Element<'a, M>>,
    body: Element<'a, M>,
) -> Element<'a, M> {
    let body = container(body).padding(SPACE_6).width(Length::Fill);
    let content: Element<'a, M> = match head {
        Some(h) => column![h, divider(), body].into(),
        None => body.into(),
    };
    scrollable(content)
        .id(SCROLL_ID)
        .height(Length::Fill)
        .style(style::scrollable)
        .into()
}

/// An entry of a page's navigation.
#[derive(Debug, Clone)]
pub struct NavEntry<M> {
    pub icon: Icon,
    pub label: String,
    pub selected: bool,
    pub on_press: M,
}

/// A row of the left navigation list: icon, label, an optional caption on
/// the right ("Premium").
pub fn nav_item<'a, M: Clone + 'a>(
    ic: Icon,
    label: String,
    badge: Option<String>,
    selected: bool,
    on_press: M,
) -> Element<'a, M> {
    let mut r = row![
        icon::sized(ic, 16.0),
        text(label).size(TEXT_BODY).width(Length::Fill)
    ]
    .spacing(SPACE_2)
    .align_y(Alignment::Center);
    if let Some(b) = badge {
        r = r.push(text(b).size(TEXT_CAPTION).style(style::text_secondary));
    }
    focus_ring(
        button(r)
            .padding([6, 10])
            .width(Length::Fill)
            .style(style::button_list_item(selected))
            .on_press(on_press.clone()),
        Some(on_press),
    )
    .into()
}

/// The left navigation list (fixed width, scrolled when tall).
pub fn nav_list<'a, M: 'a>(items: Vec<Element<'a, M>>) -> Element<'a, M> {
    container(
        scrollable(
            iced::widget::Column::with_children(items)
                .spacing(SPACE_1)
                .padding(SPACE_2),
        )
        .height(Length::Fill)
        .style(style::scrollable),
    )
    .width(NAV_WIDTH)
    .height(Length::Fill)
    .style(style::container_sidebar)
    .into()
}

/// A compact navigation entry of the narrow layout; the selected one is
/// outlined in the accent colour.
fn nav_chip<'a, M: Clone + 'a>(e: &NavEntry<M>) -> Element<'a, M> {
    let selected = e.selected;
    let item = style::button_list_item(selected);
    focus_ring(
        button(
            row![
                icon::sized(e.icon, 14.0),
                text(e.label.clone()).size(TEXT_BODY)
            ]
            .spacing(SPACE_1)
            .align_y(Alignment::Center),
        )
        .padding([4, 10])
        .style(move |theme: &Theme, status| {
            let mut s = item(theme, status);
            if selected {
                let primary = tokens(theme).primary;
                s.text_color = primary;
                s.border = s.border.color(primary).width(1);
            }
            s
        })
        .on_press(e.on_press.clone()),
        Some(e.on_press.clone()),
    )
    .into()
}

/// [`page`] with a left navigation list next to the scrolled body. When the
/// page is narrower than [`NAV_WIDTH`] + [`MIN_CONTENT_WIDTH`] (logical
/// pixels, so zoom counts) the entries wrap in rows above the body instead;
/// lower than [`MIN_PINNED_NAV_HEIGHT`] too, those rows scroll with the body
/// so they cannot take the whole height. `body` is built again whenever the
/// layout changes.
pub fn page_with_nav<'a, M: Clone + 'a>(
    title: String,
    nav: Vec<NavEntry<M>>,
    body: impl Fn() -> Element<'a, M> + 'a,
    on_close: M,
) -> Element<'a, M> {
    let content = responsive(move |size| {
        if size.width >= NAV_WIDTH + MIN_CONTENT_WIDTH {
            let items = nav
                .iter()
                .map(|e| {
                    nav_item(
                        e.icon,
                        e.label.clone(),
                        None,
                        e.selected,
                        e.on_press.clone(),
                    )
                })
                .collect();
            row![nav_list(items), scroll(body())]
                .height(Length::Fill)
                .into()
        } else {
            let chips: Element<'a, M> = container(
                Row::with_children(nav.iter().map(nav_chip))
                    .spacing(SPACE_1)
                    .wrap()
                    .vertical_spacing(SPACE_1),
            )
            .padding([SPACE_2, SPACE_4])
            .width(Length::Fill)
            .into();
            if size.height < MIN_PINNED_NAV_HEIGHT {
                scroll_with_head(Some(chips), body())
            } else {
                column![chips, divider(), scroll(body())]
                    .height(Length::Fill)
                    .into()
            }
        }
    });
    frame(title, content.into(), on_close)
}

/// "Coming soon" body of a stub.
pub fn placeholder<'a, M: 'a>(what: String) -> Element<'a, M> {
    container(empty_state(Icon::Info, what, t!("app.not_yet")))
        .height(360)
        .into()
}
