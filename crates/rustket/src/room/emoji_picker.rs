//! Emoji picker popover: search, recent, custom and the Unicode categories of
//! `rustket-core`, with a skin tone selector. Reusable: the owner keeps an
//! [`EmojiPicker`], forwards its [`Message`]s to [`EmojiPicker::update`] and
//! acts on the returned [`Event`] (insert the key or react with it, then
//! return `Effect::RecentEmoji(key)` so the app records and saves it).
//! Strings: `locales/room_composer.yml` (`room_composer.emoji.*`).

use std::cell::RefCell;
use std::sync::{LazyLock, RwLock};

use iced::widget::{button, column, container, row, scrollable, text, text_input};
use iced::{Alignment, Element, Length, Task};
use rocketchat::ServerUrl;
use rust_i18n::t;
use rustket_core::emoji::{self, CategoryKind, EmojiCategory, ResolvedEmoji};
use rustket_core::{Dirty, ServerId, Workspace};
use rustket_ui::theme::{SPACE_1, SPACE_2, TEXT_CAPTION};
use rustket_ui::widget::{TooltipPosition, emoji_text, tooltip};
use rustket_ui::{Icon, icon, style};

use crate::images::{ImageRequest, Images};
use crate::room::composer::autocomplete::emoji_glyph_with;

/// Search input id (focused when the picker opens).
pub const SEARCH_ID: &str = "emoji-picker-search";
/// Emoji per grid row.
const COLUMNS: usize = 9;
const CELL: f32 = 36.0;
/// Room for the grid's scrollbar next to the last column.
const SCROLLBAR_WIDTH: f32 = 10.0;
const SCROLLBAR_GAP: f32 = 4.0;
const SEARCH_LIMIT: usize = 90;

/// Recently used emoji keys (most recent first), shared by every picker. The
/// app seeds it from `AppConfig::recent_emoji` and persists changes.
static RECENT: LazyLock<RwLock<Vec<String>>> = LazyLock::new(|| RwLock::new(Vec::new()));
/// Selected skin tone (0 = default), shared by every picker.
static TONE: LazyLock<RwLock<u8>> = LazyLock::new(|| RwLock::new(0));

/// Replaces the shared recent list (from the persisted config).
pub fn set_recent(list: &[String]) {
    if let Ok(mut r) = RECENT.write() {
        *r = list.to_vec();
    }
}

pub fn recent() -> Vec<String> {
    RECENT.read().map(|r| r.clone()).unwrap_or_default()
}

/// Records a used emoji in the shared list (the config copy is updated by
/// the app through `Effect::RecentEmoji`).
pub fn record(key: &str) {
    if let Ok(mut r) = RECENT.write() {
        emoji::push_recent(&mut r, key, rustket_core::config::MAX_RECENT_EMOJI);
    }
}

fn tone() -> u8 {
    TONE.read().map(|t| *t).unwrap_or(0)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Tab {
    Recent,
    Custom,
    Unicode(EmojiCategory),
}

impl Tab {
    fn kind(self) -> CategoryKind {
        match self {
            Self::Recent => CategoryKind::Recent,
            Self::Custom => CategoryKind::Custom,
            Self::Unicode(c) => CategoryKind::Unicode(c),
        }
    }
}

#[derive(Debug, Clone)]
pub enum Message {
    Query(String),
    Tab(Tab),
    Pick(String),
    Tone(u8),
    ToggleTones,
    Close,
}

/// What the owner should do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// An emoji was chosen (skin tone applied); the picker should close.
    Picked(String),
    Close,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Cell {
    key: String,
    emoji: ResolvedEmoji,
}

type CacheKey = (Option<Tab>, u8, String, u64, Vec<String>);

#[derive(Debug, Default)]
pub struct EmojiPicker {
    pub query: String,
    tab: Option<Tab>,
    tones_open: bool,
    cache: RefCell<Option<(CacheKey, Vec<Cell>)>>,
}

impl EmojiPicker {
    pub fn new() -> Self {
        Self::default()
    }

    /// Focuses the search field.
    pub fn focus<M: Send + 'static>() -> Task<M> {
        iced::widget::operation::focus(SEARCH_ID)
    }

    pub fn update(&mut self, msg: Message) -> Option<Event> {
        match msg {
            Message::Query(q) => self.query = q,
            Message::Tab(t) => {
                self.tab = Some(t);
                self.query.clear();
            }
            Message::Pick(key) => {
                let key = apply_tone(&key, tone());
                record(&key);
                return Some(Event::Picked(key));
            }
            Message::Tone(t) => {
                if let Ok(mut cur) = TONE.write() {
                    *cur = t.min(5);
                }
                self.tones_open = false;
            }
            Message::ToggleTones => self.tones_open = !self.tones_open,
            Message::Close => return Some(Event::Close),
        }
        None
    }

    /// The tab shown: the chosen one, else recent (when not empty), else people.
    fn current_tab(&self, recent: &[String]) -> Tab {
        self.tab.unwrap_or(if recent.is_empty() {
            Tab::Unicode(EmojiCategory::People)
        } else {
            Tab::Recent
        })
    }

    fn cells(&self, ws: &Workspace) -> Vec<Cell> {
        let recent = recent();
        let tab = if self.query.trim().is_empty() {
            Some(self.current_tab(&recent))
        } else {
            None
        };
        let key: CacheKey = (
            tab,
            tone(),
            self.query.trim().to_lowercase(),
            ws.revision(Dirty::EMOJI),
            recent.clone(),
        );
        if let Some((k, cells)) = self.cache.borrow().as_ref()
            && *k == key
        {
            return cells.clone();
        }
        let tone = tone();
        let toned = |key: String, emoji: ResolvedEmoji| -> Cell {
            if tone > 0
                && matches!(emoji, ResolvedEmoji::Unicode(_))
                && let Some(k) = emoji::with_skin_tone(&key, tone)
                && let Some(u) = emoji::to_unicode(&k)
            {
                return Cell {
                    key,
                    emoji: ResolvedEmoji::Unicode(u),
                };
            }
            Cell { key, emoji }
        };
        let cells: Vec<Cell> = match tab {
            None => ws
                .emoji()
                .search(&self.query, SEARCH_LIMIT)
                .into_iter()
                .map(|m| toned(m.key, m.emoji))
                .collect(),
            Some(tab) => ws
                .emoji()
                .picker_categories(&recent)
                .into_iter()
                .find(|c| c.kind == tab.kind())
                .map(|c| {
                    c.emojis
                        .into_iter()
                        .map(|e| {
                            if tab == Tab::Recent {
                                // Recent keys already carry their tone.
                                Cell {
                                    key: e.key,
                                    emoji: e.emoji,
                                }
                            } else {
                                toned(e.key, e.emoji)
                            }
                        })
                        .collect()
                })
                .unwrap_or_default(),
        };
        *self.cache.borrow_mut() = Some((key, cells.clone()));
        cells
    }

    /// Requests the custom emoji images the picker will show.
    pub fn want_images(
        &self,
        ws: &Workspace,
        server_id: &ServerId,
        server: &ServerUrl,
        images: &Images,
    ) {
        for c in self.cells(ws) {
            if let Some(url) = c.emoji.custom_url(server) {
                images.want(ImageRequest {
                    server_id: server_id.clone(),
                    url,
                });
            }
        }
    }

    pub fn view<'a>(
        &'a self,
        ws: &'a Workspace,
        server_id: &'a ServerId,
        server: &'a ServerUrl,
        images: &'a Images,
        title: Option<String>,
    ) -> Element<'a, Message> {
        let recent = recent();
        let current = self.current_tab(&recent);
        let searching = !self.query.trim().is_empty();

        let mut tabs = row![].spacing(0).align_y(Alignment::Center);
        let mut all_tabs = Vec::new();
        if !recent.is_empty() {
            all_tabs.push(Tab::Recent);
        }
        if !ws.emoji().is_empty() {
            all_tabs.push(Tab::Custom);
        }
        all_tabs.extend(EmojiCategory::ALL.into_iter().map(Tab::Unicode));
        for tab in all_tabs {
            let glyph: Element<'a, Message> = match tab {
                Tab::Recent => icon(Icon::Clock).size(16.0).into(),
                Tab::Custom => icon(Icon::SmilePlus).size(16.0).into(),
                Tab::Unicode(c) => emoji_text(category_glyph(c), 16.0).into(),
            };
            let b = button(container(glyph).center(Length::Fill))
                .width(Length::Fixed(32.0))
                .height(Length::Fixed(30.0))
                .padding(0)
                .style(if !searching && tab == current {
                    style::button_icon_active
                } else {
                    style::button_icon
                })
                .on_press(Message::Tab(tab));
            tabs = tabs.push(tooltip(b, category_name(tab.kind()), TooltipPosition::Top));
        }

        let cells = self.cells(ws);
        let mut grid = column![].spacing(0);
        if cells.is_empty() {
            grid = grid.push(
                container(
                    text(t!("room_composer.emoji.no_results"))
                        .size(TEXT_CAPTION)
                        .style(style::text_hint),
                )
                .padding(SPACE_2),
            );
        }
        for chunk in cells.chunks(COLUMNS) {
            let mut r = row![].spacing(0);
            for c in chunk {
                let glyph = emoji_glyph_with(&c.emoji, server_id, server, images, 24.0);
                r = r.push(
                    button(container(glyph).center(Length::Fill))
                        .width(Length::Fixed(CELL))
                        .height(Length::Fixed(CELL))
                        .padding(0)
                        .style(style::button_icon)
                        .on_press(Message::Pick(c.key.clone())),
                );
            }
            grid = grid.push(r);
        }

        let section = if searching {
            t!("room_composer.emoji.search_results").into_owned()
        } else {
            category_name(current.kind())
        };
        let tone_now = tone();
        let tone_button =
            button(container(emoji_text(tone_glyph(tone_now), 16.0)).center(Length::Fill))
                .width(Length::Fixed(32.0))
                .height(Length::Fixed(30.0))
                .padding(0)
                .style(style::button_icon)
                .on_press(Message::ToggleTones);
        let mut header = row![
            text_input(&t!("room_composer.emoji.search"), &self.query)
                .id(SEARCH_ID)
                .on_input(Message::Query)
                .style(style::text_input)
                .padding([6, 8])
                .width(Length::Fill),
            tooltip(
                tone_button,
                t!("room_composer.emoji.skin_tone"),
                TooltipPosition::Top
            ),
        ]
        .spacing(SPACE_1)
        .align_y(Alignment::Center);
        if let Some(title) = title {
            header = row![text(title).size(TEXT_CAPTION).style(style::text_secondary)]
                .push(header)
                .spacing(SPACE_2)
                .align_y(Alignment::Center);
        }
        let mut body = column![header].spacing(SPACE_1);
        if self.tones_open {
            let mut tones = row![].spacing(0);
            for t in 0..=5u8 {
                tones = tones.push(
                    button(container(emoji_text(tone_glyph(t), 18.0)).center(Length::Fill))
                        .width(Length::Fixed(CELL))
                        .height(Length::Fixed(CELL))
                        .padding(0)
                        .style(if t == tone_now {
                            style::button_icon_active
                        } else {
                            style::button_icon
                        })
                        .on_press(Message::Tone(t)),
                );
            }
            body = body.push(tones);
        }
        body = body
            .push(tabs)
            .push(
                text(section)
                    .size(TEXT_CAPTION)
                    .style(style::text_secondary),
            )
            .push(
                scrollable(grid)
                    .spacing(SCROLLBAR_GAP)
                    .height(Length::Fixed(CELL * 7.0))
                    .style(style::scrollable),
            );
        container(body.padding(SPACE_2))
            .width(Length::Fixed(
                CELL * COLUMNS as f32 + SPACE_2 * 2.0 + SCROLLBAR_WIDTH + SCROLLBAR_GAP,
            ))
            .style(style::container_popover)
            .into()
    }
}

/// `key` with skin tone `tone` when the emoji has tones.
pub fn apply_tone(key: &str, tone: u8) -> String {
    if tone == 0 {
        return key.to_owned();
    }
    emoji::with_skin_tone(key, tone).unwrap_or_else(|| key.to_owned())
}

fn tone_glyph(tone: u8) -> &'static str {
    match tone {
        1 => "✋🏻",
        2 => "✋🏼",
        3 => "✋🏽",
        4 => "✋🏾",
        5 => "✋🏿",
        _ => "✋",
    }
}

fn category_glyph(c: EmojiCategory) -> &'static str {
    match c {
        EmojiCategory::People => "😀",
        EmojiCategory::Nature => "🐻",
        EmojiCategory::Food => "🍔",
        EmojiCategory::Activity => "⚽",
        EmojiCategory::Travel => "🚗",
        EmojiCategory::Objects => "💡",
        EmojiCategory::Symbols => "❤️",
        EmojiCategory::Flags => "🏁",
    }
}

pub fn category_name(kind: CategoryKind) -> String {
    match kind {
        CategoryKind::Recent => t!("room_composer.emoji.recent"),
        CategoryKind::Custom => t!("room_composer.emoji.custom"),
        CategoryKind::Unicode(c) => match c {
            EmojiCategory::People => t!("room_composer.emoji.people"),
            EmojiCategory::Nature => t!("room_composer.emoji.nature"),
            EmojiCategory::Food => t!("room_composer.emoji.food"),
            EmojiCategory::Activity => t!("room_composer.emoji.activities"),
            EmojiCategory::Travel => t!("room_composer.emoji.travel"),
            EmojiCategory::Objects => t!("room_composer.emoji.objects"),
            EmojiCategory::Symbols => t!("room_composer.emoji.symbols"),
            EmojiCategory::Flags => t!("room_composer.emoji.flags"),
        },
    }
    .into_owned()
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[test]
    fn tone_applies_only_to_toned_emoji() {
        assert_eq!(apply_tone(":thumbsup:", 3), ":thumbsup_tone3:");
        assert_eq!(apply_tone(":thumbsup:", 0), ":thumbsup:");
        assert_eq!(apply_tone(":heart:", 2), ":heart:");
    }

    #[test]
    fn search_and_tabs() {
        let ws = Workspace::new();
        let mut p = EmojiPicker::new();
        assert!(p.update(Message::Query("thumbsup".into())).is_none());
        let cells = p.cells(&ws);
        assert_eq!(cells[0].key, ":thumbsup:");
        p.update(Message::Tab(Tab::Unicode(EmojiCategory::Flags)));
        assert!(p.query.is_empty());
        let cells = p.cells(&ws);
        assert!(cells.iter().any(|c| c.key == ":flag_ru:"));
        assert_eq!(p.update(Message::Close), Some(Event::Close));
    }
}
