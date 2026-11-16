//! The keyboard shortcuts cheat sheet (Ctrl/Cmd+/, status menu "Keyboard
//! shortcuts", Settings), built from `crate::keymap::GROUPS`. Stateless.

use iced::widget::{column, row, text};
use iced::{Alignment, Element, Length};
use rust_i18n::t;
use rustket_ui::style;
use rustket_ui::theme::{
    SPACE_1, SPACE_2, SPACE_3, SPACE_4, TEXT_BODY, TEXT_CAPTION, font_semibold,
};
use rustket_ui::widget::{dialog, kbd, secondary_button};

use crate::keymap::{self, ESCAPE_CLOSE_LABEL, ESCAPE_LABEL};
use crate::message::Message;

/// `escape_marks_read`: `UiPrefs::escape_marks_read` (the Esc row says
/// what Esc does).
pub fn view<'a>(escape_marks_read: bool) -> Element<'a, Message> {
    let mut body = column![].spacing(SPACE_4);
    for g in keymap::GROUPS {
        let mut rows = column![
            text(t!(g.label))
                .size(TEXT_CAPTION)
                .font(font_semibold())
                .style(style::text_secondary)
        ]
        .spacing(SPACE_2);
        for e in g.entries.iter().filter(|e| e.is_available()) {
            let label = if e.label == ESCAPE_LABEL && !escape_marks_read {
                ESCAPE_CLOSE_LABEL
            } else {
                e.label
            };
            let mut keys = row![].spacing(SPACE_1).align_y(Alignment::Center);
            for (i, chord) in e.chords.iter().enumerate() {
                if i > 0 {
                    keys = keys.push(text("/").size(TEXT_CAPTION).style(style::text_hint));
                }
                keys = keys.push(kbd(keymap::display(chord)));
            }
            rows = rows.push(
                row![text(t!(label)).size(TEXT_BODY).width(Length::Fill), keys]
                    .spacing(SPACE_3)
                    .align_y(Alignment::Center),
            );
        }
        body = body.push(rows);
    }
    dialog(
        t!("shortcuts.title"),
        body,
        vec![secondary_button(t!("app.close"), Some(Message::CloseModal)).into()],
        Some(Message::CloseModal),
    )
}
