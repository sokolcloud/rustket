//! The "Hidden words" card (Privacy tab): phrases whose messages are
//! collapsed in every room (`rustket_core::word_filter`).

use iced::widget::{column, row, text};
use iced::{Alignment, Element, Length};
use rust_i18n::t;
use rustket_core::word_filter::{AddError, MAX_PHRASE_CHARS, MAX_PHRASES};
use rustket_ui::icon::Icon;
use rustket_ui::theme::{SPACE_1, SPACE_2, TEXT_BODY};
use rustket_ui::widget::{icon_button, secondary_button, text_field};

use super::widgets::{caption, section, switch};
use super::{Message, Settings, SettingsView};

pub const INPUT_ID: &str = "settings-hidden-word";

fn error_text(e: AddError) -> String {
    match e {
        AddError::Empty => t!("moderation.hidden_empty").into_owned(),
        AddError::TooLong => t!("moderation.hidden_too_long", max = MAX_PHRASE_CHARS).into_owned(),
        AddError::Duplicate => t!("moderation.hidden_duplicate").into_owned(),
        AddError::TooMany => t!("moderation.hidden_too_many", max = MAX_PHRASES).into_owned(),
    }
}

pub(super) fn card<'a>(s: &'a Settings, v: &SettingsView<'a>) -> Element<'a, Message> {
    let words = &v.config.ui.hidden_words;
    let input = text_field(
        t!("moderation.hidden_add_label"),
        &s.hidden_word_draft,
        Message::HiddenWordDraft,
    )
    .placeholder(t!("moderation.hidden_placeholder"))
    .error(s.hidden_word_error.map(error_text))
    .id(INPUT_ID)
    .on_submit(Message::AddHiddenWord)
    .width(Length::Fill);
    let add = row![
        input,
        secondary_button(
            t!("moderation.hidden_add").into_owned(),
            (!s.hidden_word_draft.trim().is_empty()).then_some(Message::AddHiddenWord),
        )
        .icon(Icon::Plus),
    ]
    .spacing(SPACE_2)
    .align_y(Alignment::End);
    let list: Element<'a, Message> = if words.phrases.is_empty() {
        caption(t!("moderation.hidden_none").into_owned()).into()
    } else {
        let mut col = column![].spacing(SPACE_1);
        for (i, p) in words.phrases.iter().enumerate() {
            col = col.push(
                row![
                    text(p.clone()).size(TEXT_BODY).width(Length::Fill),
                    icon_button(
                        Icon::X,
                        Some(Message::RemoveHiddenWord(i)),
                        t!("moderation.hidden_remove"),
                    ),
                ]
                .spacing(SPACE_2)
                .align_y(Alignment::Center),
            );
        }
        col.into()
    };
    section(
        t!("moderation.hidden_title").into_owned(),
        vec![
            caption(t!("moderation.hidden_help").into_owned()).into(),
            switch(
                t!("moderation.hidden_whole_word").into_owned(),
                Some(t!("moderation.hidden_whole_word_help").into_owned()),
                words.whole_word,
                Some(Message::SetHiddenWholeWord as fn(bool) -> Message),
            ),
            add.into(),
            list,
        ],
    )
}
