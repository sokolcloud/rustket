//! Channel bookmarks under the room header (`rustket_core::bookmarks`):
//! chips for the links of the topic, announcement and description, and the
//! "Add bookmark" dialog that appends a link to the description
//! (`rooms.saveRoomSettings`). Strings: `locales/bookmarks.yml`.

use iced::widget::{button, column, container, row, text};
use iced::{Alignment, Element, Length};
use rust_i18n::t;
use rustket_core::Workspace;
use rustket_core::bookmarks::{self, Bookmark};
use rustket_ui::icon::{self, Icon};
use rustket_ui::style;
use rustket_ui::theme::{SPACE_1, SPACE_2, SPACE_3, SPACE_4, TEXT_CAPTION, TEXT_SMALL};
use rustket_ui::widget::{
    TooltipPosition, dialog, divider, icon_button_with, primary_button, secondary_button,
    text_field, tooltip,
};

/// Characters of a chip's label.
const LABEL_CHARS: usize = 32;
/// Id of the URL field (focused when the dialog opens).
pub const URL_ID: &str = "bookmark-url";

#[derive(Debug, Clone)]
pub enum Msg {
    Title(String),
    Url(String),
    Save,
    Cancel,
}

/// The "Add bookmark" dialog.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AddBookmark {
    pub title: String,
    pub url: String,
    pub saving: bool,
    pub error: Option<String>,
}

/// The links of room `rid`, in field order.
pub fn of_room(ws: &Workspace, rid: &str) -> Vec<Bookmark> {
    let Some(r) = ws.room(rid).and_then(|e| e.room.as_ref()) else {
        return Vec::new();
    };
    bookmarks::extract(
        r.topic.as_deref(),
        r.announcement.as_deref(),
        r.description.as_deref(),
    )
}

/// Who may add bookmarks: the users who may edit the room info.
pub fn can_add(ws: &Workspace, rid: &str) -> bool {
    super::panel::settings::offered(ws, rid)
}

/// The URL as typed, with `https://` when it has no scheme; `None` when it
/// is not a web address.
pub fn normalize_url(input: &str) -> Option<String> {
    let s = input.trim();
    let s = if s.contains("://") {
        s.to_owned()
    } else {
        format!("https://{s}")
    };
    bookmarks::is_web_url(&s).then_some(s)
}

/// The room description with the link appended (`None`: invalid URL).
pub fn new_description(ws: &Workspace, rid: &str, d: &AddBookmark) -> Option<String> {
    let url = normalize_url(&d.url)?;
    let current = ws
        .room(rid)
        .and_then(|e| e.room.as_ref())
        .and_then(|r| r.description.clone())
        .unwrap_or_default();
    Some(bookmarks::append(&current, &d.title, &url))
}

/// The chip row; `None` when the room has no links.
pub fn bar<'a, M: Clone + 'a>(
    links: &[Bookmark],
    open: impl Fn(String) -> M,
    add: Option<M>,
) -> Option<Element<'a, M>> {
    if links.is_empty() {
        return None;
    }
    let mut chips: Vec<Element<'a, M>> = Vec::new();
    for b in links {
        let content = row![
            icon::sized(Icon::Link2, 12.0).style(style::text_secondary),
            text(bookmarks::label(b, LABEL_CHARS)).size(TEXT_CAPTION),
        ]
        .spacing(SPACE_1)
        .align_y(Alignment::Center);
        let chip = button(content)
            .padding([2.0, SPACE_2])
            .style(style::button_reaction(false))
            .on_press(open(b.url.clone()));
        chips.push(tooltip(chip, b.url.clone(), TooltipPosition::Bottom));
    }
    if let Some(m) = add {
        chips.push(icon_button_with(
            Icon::BookmarkPlus,
            Some(m),
            t!("bookmarks.add"),
            false,
            22.0,
        ));
    }
    let strip = container(row(chips).spacing(SPACE_1).wrap().vertical_spacing(SPACE_1))
        .width(Length::Fill)
        .padding([SPACE_1 + 1.0, SPACE_4])
        .style(style::container_header);
    Some(column![strip, divider()].into())
}

impl AddBookmark {
    pub fn view<'a, M: Clone + 'a>(
        &'a self,
        wrap: impl Fn(Msg) -> M + Copy + 'a,
    ) -> Element<'a, M> {
        let content = column![
            text_field(t!("bookmarks.url"), &self.url, move |v| wrap(Msg::Url(v)))
                .placeholder("https://")
                .id(URL_ID)
                .error(self.error.clone())
                .on_submit(wrap(Msg::Save)),
            text_field(t!("bookmarks.title_label"), &self.title, move |v| wrap(
                Msg::Title(v)
            ))
            .placeholder(t!("bookmarks.title_placeholder"))
            .on_submit(wrap(Msg::Save)),
            text(t!("bookmarks.add_hint"))
                .size(TEXT_SMALL)
                .style(style::text_hint),
        ]
        .spacing(SPACE_3);
        let ready = !self.saving && !self.url.trim().is_empty();
        dialog(
            t!("bookmarks.add_title").into_owned(),
            content,
            vec![
                secondary_button(t!("room_composer.cancel"), Some(wrap(Msg::Cancel))).into(),
                primary_button(t!("bookmarks.save"), ready.then(|| wrap(Msg::Save)))
                    .loading(self.saving)
                    .into(),
            ],
            Some(wrap(Msg::Cancel)),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urls_get_a_scheme() {
        assert_eq!(
            normalize_url(" example.com/x ").as_deref(),
            Some("https://example.com/x")
        );
        assert_eq!(
            normalize_url("http://a.example.com").as_deref(),
            Some("http://a.example.com")
        );
        assert_eq!(normalize_url("ftp://a.example.com"), None);
        assert_eq!(normalize_url("not a url"), None);
    }
}
