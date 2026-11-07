//! Popover and right-click context menus.

use iced::widget::{button, column, container, row, space, text};
use iced::{Alignment, Element, Length};
use iced_aw::ContextMenu;

use super::popover::{Placement, popover};

use super::ellipsis::ellipsis;
use super::misc::divider;
use crate::icon::{self, Icon};
use crate::style;
use crate::theme::{SPACE_1, SPACE_2, SPACE_3, TEXT_BODY, TEXT_CAPTION};

/// One actionable menu row.
#[derive(Debug, Clone)]
pub struct MenuItem<Message> {
    pub icon: Option<Icon>,
    pub label: String,
    /// Optional right-aligned hint (keyboard shortcut).
    pub hint: Option<String>,
    pub danger: bool,
    pub disabled: bool,
    pub on_select: Message,
}

impl<Message> MenuItem<Message> {
    pub fn new(label: impl Into<String>, on_select: Message) -> Self {
        Self {
            icon: None,
            label: label.into(),
            hint: None,
            danger: false,
            disabled: false,
            on_select,
        }
    }

    pub fn icon(mut self, icon: Icon) -> Self {
        self.icon = Some(icon);
        self
    }

    pub fn hint(mut self, hint: impl Into<String>) -> Self {
        self.hint = Some(hint.into());
        self
    }

    pub fn danger(mut self, danger: bool) -> Self {
        self.danger = danger;
        self
    }

    pub fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }
}

#[derive(Debug, Clone)]
pub enum MenuEntry<Message> {
    Item(MenuItem<Message>),
    /// Small non-interactive caption heading a group.
    Header(String),
    Separator,
}

impl<Message> From<MenuItem<Message>> for MenuEntry<Message> {
    fn from(item: MenuItem<Message>) -> Self {
        MenuEntry::Item(item)
    }
}

pub const MENU_WIDTH: f32 = 220.0;

/// The menu card itself (usable standalone, e.g. inside a custom overlay).
pub fn menu_panel<'a, Message: Clone + 'a>(entries: &[MenuEntry<Message>]) -> Element<'a, Message> {
    let rows = entries.iter().map(|entry| -> Element<'a, Message> {
        match entry {
            MenuEntry::Separator => container(divider()).padding([SPACE_1, 0.0]).into(),
            MenuEntry::Header(title) => container(
                text(title.clone())
                    .size(TEXT_CAPTION)
                    .style(style::text_hint),
            )
            .padding([SPACE_1, SPACE_2])
            .into(),
            MenuEntry::Item(item) => {
                let mut content = row![].spacing(SPACE_3).align_y(Alignment::Center);
                content = match item.icon {
                    Some(i) => content.push(icon::sized(i, 16.0)),
                    None => content.push(space().width(0)),
                };
                content = content.push(ellipsis(item.label.clone()).size(TEXT_BODY));
                if let Some(hint) = &item.hint {
                    content = content.push(
                        text(hint.clone())
                            .size(TEXT_CAPTION)
                            .style(style::text_hint),
                    );
                }
                button(content)
                    .width(Length::Fill)
                    .padding([6, 8])
                    .style(style::button_menu_item(item.danger))
                    .on_press_maybe((!item.disabled).then(|| item.on_select.clone()))
                    .into()
            }
        }
    });
    container(column(rows).spacing(0))
        .padding(SPACE_1)
        .width(MENU_WIDTH)
        .style(style::container_popover)
        .into()
}

/// Opens `entries` at the cursor when `underlay` is right-clicked. Selecting an
/// item, clicking elsewhere or pressing Escape closes it (state lives in the widget).
pub fn context_menu<'a, Message: Clone + 'a>(
    underlay: impl Into<Element<'a, Message>>,
    entries: Vec<MenuEntry<Message>>,
) -> Element<'a, Message> {
    ContextMenu::new(underlay, move || menu_panel(&entries))
        .style(style::context_menu_backdrop)
        .into()
}

/// Shows `entries` anchored to `trigger` while `open` is true; clicking outside or
/// pressing Escape publishes `on_dismiss`. Selecting an item should also close it
/// (the caller resets `open` in its update).
pub fn popover_menu<'a, Message: Clone + 'a>(
    trigger: impl Into<Element<'a, Message>>,
    entries: &[MenuEntry<Message>],
    open: bool,
    on_dismiss: Message,
    placement: Placement,
) -> Element<'a, Message> {
    popover(trigger, menu_panel(entries), open, placement, on_dismiss)
}
