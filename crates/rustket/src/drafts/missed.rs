//! "These scheduled messages were not sent on time": scheduled messages
//! whose time passed while Rustket was off (or not connected) are not sent
//! automatically; this dialog lists them over every server with Send now,
//! Reschedule and Delete (and Send all / Delete all). Closing it leaves them
//! in the Drafts & scheduled view, marked as not sent.

use iced::widget::{column, container, row, scrollable, text};
use iced::{Alignment, Element, Length};
use rust_i18n::t;
use rustket_core::ServerId;
use rustket_ui::Icon;
use rustket_ui::style;
use rustket_ui::theme::{SPACE_1, SPACE_2, SPACE_3, TEXT_BODY, TEXT_SMALL, font_semibold};
use rustket_ui::widget::{
    danger_button, dialog, icon_button_with, primary_button, secondary_button,
};

use crate::later::when::{self, WhenPicker};

/// The dialog's state (`app::Modal::Missed`).
#[derive(Debug, Default)]
pub struct Missed {
    /// The message being rescheduled.
    pub reschedule: Option<(ServerId, String, WhenPicker)>,
}

#[derive(Debug, Clone)]
pub enum MissedMsg {
    SendNow(ServerId, String),
    Reschedule(ServerId, String),
    Delete(ServerId, String),
    SendAll,
    DeleteAll,
    Picker(when::Msg),
    CancelReschedule,
}

/// One listed message.
pub struct Item {
    pub sid: ServerId,
    pub id: String,
    /// Room (and server, with several) it goes to.
    pub place: String,
    pub when: String,
    pub preview: String,
}

pub fn view<'a, M: Clone + 'a>(
    state: &'a Missed,
    items: Vec<Item>,
    now: chrono::DateTime<chrono::Local>,
    morning: u32,
    clock_24h: bool,
    wrap: impl Fn(MissedMsg) -> M + Copy + 'a,
    close: M,
) -> Element<'a, M> {
    if let Some((_, _, picker)) = &state.reschedule {
        let back = wrap(MissedMsg::CancelReschedule);
        return dialog(
            t!("scheduled.reschedule").into_owned(),
            picker
                .view(now, morning, clock_24h)
                .map(move |m| wrap(MissedMsg::Picker(m))),
            vec![when::cancel(back.clone())],
            Some(back),
        );
    }
    let mut list = column![].spacing(SPACE_3);
    for it in items {
        let (sid, id) = (it.sid.clone(), it.id.clone());
        let tools = row![
            icon_button_with(
                Icon::Send,
                Some(wrap(MissedMsg::SendNow(sid.clone(), id.clone()))),
                t!("scheduled.send_now"),
                false,
                28.0
            ),
            icon_button_with(
                Icon::CalendarClock,
                Some(wrap(MissedMsg::Reschedule(sid.clone(), id.clone()))),
                t!("scheduled.reschedule"),
                false,
                28.0
            ),
            icon_button_with(
                Icon::Trash2,
                Some(wrap(MissedMsg::Delete(sid, id))),
                t!("scheduled.delete"),
                false,
                28.0
            ),
        ]
        .spacing(SPACE_1);
        list = list.push(
            row![
                column![
                    row![
                        text(it.when).size(TEXT_SMALL).style(style::text_danger),
                        text("·").size(TEXT_SMALL).style(style::text_hint),
                        text(it.place).size(TEXT_SMALL).font(font_semibold()),
                    ]
                    .spacing(SPACE_1),
                    text(it.preview).size(TEXT_BODY),
                ]
                .spacing(SPACE_1)
                .width(Length::Fill),
                tools,
            ]
            .spacing(SPACE_2)
            .align_y(Alignment::Center),
        );
    }
    let body = column![
        text(t!("scheduled.missed_body").into_owned())
            .size(TEXT_SMALL)
            .style(style::text_secondary),
        container(scrollable(list).height(Length::Shrink)).max_height(360),
    ]
    .spacing(SPACE_3);
    dialog(
        t!("scheduled.missed_title").into_owned(),
        body,
        vec![
            secondary_button(t!("scheduled.decide_later"), Some(close.clone())).into(),
            danger_button(t!("scheduled.delete_all"), Some(wrap(MissedMsg::DeleteAll))).into(),
            primary_button(t!("scheduled.send_all"), Some(wrap(MissedMsg::SendAll))).into(),
        ],
        Some(close),
    )
}
