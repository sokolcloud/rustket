//! Rendering of the Drafts & scheduled view and its dialogs.

use chrono::{DateTime, Local};
use iced::widget::{button, column, container, row, scrollable, text, text_editor};
use iced::{Alignment, Element, Length};
use rust_i18n::t;
use rustket_core::Workspace;
use rustket_core::config::UiPrefs;
use rustket_core::room_list;
use rustket_core::scheduled::Scheduled;
use rustket_ui::icon::{self, Icon};
use rustket_ui::style;
use rustket_ui::theme::{SPACE_1, SPACE_2, SPACE_3, SPACE_4, TEXT_BODY, TEXT_SMALL, font_semibold};
use rustket_ui::widget::{
    danger_button, dialog, empty_state, header_bar, icon_button, icon_button_with, modal_keyed,
    primary_button,
};

use super::{Dialog, DraftsView, Msg, preview};
use crate::later::when;

/// Characters of a row's text.
const PREVIEW_CHARS: usize = 240;

/// What the view reads besides its state.
pub struct Ctx<'a> {
    pub workspace: &'a Workspace,
    pub prefs: &'a UiPrefs,
    pub local_now: DateTime<Local>,
}

impl Ctx<'_> {
    fn moment(&self, at_ms: i64) -> String {
        crate::format::moment(at_ms, self.local_now, self.prefs.clock_24h)
    }
}

/// Where a message goes: the room's name, "Thread in #room" for a reply.
pub fn place(ws: &Workspace, prefs: &UiPrefs, rid: &str, tmid: Option<&str>) -> String {
    let real = ws.use_real_names(prefs.use_real_names);
    let room = ws
        .room(rid)
        .map(|e| room_list::display_name(ws, e, real))
        .unwrap_or_default();
    match tmid {
        Some(_) => t!("later.thread_in", room = room.as_str()).into_owned(),
        None => room,
    }
}

fn section_title<'a>(label: String, n: usize) -> Element<'a, Msg> {
    container(
        text(format!("{label} ({n})"))
            .size(TEXT_SMALL)
            .font(font_semibold())
            .style(style::text_secondary),
    )
    .padding([SPACE_2, SPACE_4])
    .into()
}

fn tool(ic: Icon, label: String, on: Msg) -> Element<'static, Msg> {
    icon_button_with(ic, Some(on), label, false, 28.0)
}

impl DraftsView {
    pub fn view<'a>(&'a self, c: &Ctx<'a>) -> Element<'a, Msg> {
        let ws = c.workspace;
        let header = header_bar(
            Some(icon::sized(Icon::NotebookPen, 18.0).into()),
            t!("scheduled.title").into_owned(),
            None,
            vec![icon_button(Icon::X, Some(Msg::Close), t!("later.close"))],
        );
        let scheduled = ws.scheduled().sorted();
        let drafts = super::drafts(ws);
        let body: Element<'a, Msg> = if scheduled.is_empty() && drafts.is_empty() {
            container(empty_state(
                Icon::NotebookPen,
                t!("scheduled.empty_title"),
                t!("scheduled.empty_body"),
            ))
            .center(Length::Fill)
            .into()
        } else {
            let mut col = column![].spacing(0);
            if !scheduled.is_empty() {
                col = col.push(section_title(
                    t!("scheduled.section_scheduled").into_owned(),
                    scheduled.len(),
                ));
                for s in &scheduled {
                    col = col.push(self.scheduled_row(c, s));
                }
            }
            if !drafts.is_empty() {
                col = col.push(section_title(
                    t!("scheduled.section_drafts").into_owned(),
                    drafts.len(),
                ));
                for (rid, tmid, text) in &drafts {
                    col = col.push(draft_row(c, rid, tmid.as_deref(), text));
                }
            }
            scrollable(col.padding(iced::Padding::default().bottom(SPACE_4)))
                .height(Length::Fill)
                .into()
        };
        let main = column![header, body]
            .width(Length::Fill)
            .height(Length::Fill);
        container(modal_keyed(main, self.overlay(c), Some(Msg::CloseDialog)))
            .style(style::container_room)
            .width(Length::Fill)
            .height(Length::Fill)
            .into()
    }

    fn scheduled_row(&self, c: &Ctx<'_>, s: &Scheduled) -> Element<'static, Msg> {
        let m = &s.message;
        let where_ = place(c.workspace, c.prefs, &m.rid, m.tmid.as_deref());
        let (when_line, when_style): (String, fn(&iced::Theme) -> text::Style) = if s.missed {
            (
                t!(
                    "scheduled.missed_at",
                    time = missed_when(c.moment(s.send_at), s.error.as_deref())
                )
                .into_owned(),
                style::text_danger,
            )
        } else {
            (c.moment(s.send_at), style::text_secondary)
        };
        let id = s.id().to_owned();
        let tools = row![
            tool(
                Icon::Send,
                t!("scheduled.send_now").into_owned(),
                Msg::SendNow(id.clone())
            ),
            tool(
                Icon::CalendarClock,
                t!("scheduled.reschedule").into_owned(),
                Msg::Reschedule(id.clone())
            ),
            tool(
                Icon::Pencil,
                t!("scheduled.edit").into_owned(),
                Msg::Edit(id.clone())
            ),
            tool(
                Icon::Trash2,
                t!("scheduled.delete").into_owned(),
                Msg::Delete(id)
            ),
        ]
        .spacing(SPACE_1)
        .align_y(Alignment::Center);
        let info = column![
            row![
                icon::sized(Icon::ClockArrowUp, 14.0).style(when_style),
                text(when_line).size(TEXT_SMALL).style(when_style),
                text("·").size(TEXT_SMALL).style(style::text_hint),
                text(where_)
                    .size(TEXT_SMALL)
                    .font(font_semibold())
                    .wrapping(text::Wrapping::None),
            ]
            .spacing(SPACE_1)
            .align_y(Alignment::Center),
            text(preview(&m.msg, PREVIEW_CHARS)).size(TEXT_BODY),
        ]
        .spacing(SPACE_1)
        .width(Length::Fill);
        let open = Msg::Open {
            rid: m.rid.clone(),
            tmid: m.tmid.clone(),
        };
        let content = row![
            button(info)
                .padding(0)
                .style(style::button_ghost)
                .on_press(open)
                .width(Length::Fill),
            tools
        ]
        .spacing(SPACE_3)
        .align_y(Alignment::Center);
        container(content)
            .padding([SPACE_2, SPACE_4])
            .width(Length::Fill)
            .into()
    }

    /// The open dialog with a key for the modal.
    fn overlay<'a>(&'a self, c: &Ctx<'_>) -> Option<(u8, Element<'a, Msg>)> {
        let close = Msg::CloseDialog;
        let excerpt = |id: &str| -> Element<'a, Msg> {
            let line = c
                .workspace
                .scheduled()
                .get(id)
                .map(|s| preview(&s.message.msg, 120))
                .unwrap_or_default();
            text(line)
                .size(TEXT_SMALL)
                .style(style::text_secondary)
                .into()
        };
        match self.dialog.as_ref()? {
            Dialog::Reschedule { id, picker } => Some((
                1,
                dialog(
                    t!("scheduled.reschedule").into_owned(),
                    column![
                        excerpt(id),
                        picker
                            .view(c.local_now, c.prefs.morning_hour(), c.prefs.clock_24h)
                            .map(Msg::Picker),
                    ]
                    .spacing(SPACE_3),
                    vec![when::cancel(close.clone())],
                    Some(close),
                ),
            )),
            Dialog::Edit {
                confirm: Some(b), ..
            } => Some((
                4,
                crate::room::composer::broad::dialog(b, Msg::BroadCancelled, Msg::BroadConfirmed),
            )),
            Dialog::Edit { content, .. } => {
                let editor = text_editor(content)
                    .on_action(Msg::EditAction)
                    .padding([SPACE_2, SPACE_3])
                    .size(TEXT_BODY)
                    .height(Length::Fixed(160.0));
                let save = (!content.text().trim().is_empty()).then_some(Msg::SaveEdit);
                Some((
                    2,
                    dialog(
                        t!("scheduled.edit").into_owned(),
                        editor,
                        vec![
                            when::cancel(close.clone()),
                            primary_button(t!("scheduled.save"), save).into(),
                        ],
                        Some(close),
                    ),
                ))
            }
            Dialog::Delete(id) => Some((
                3,
                dialog(
                    t!("scheduled.delete_title").into_owned(),
                    excerpt(id),
                    vec![
                        when::cancel(close.clone()),
                        danger_button(t!("scheduled.delete"), Some(Msg::ConfirmDelete)).into(),
                    ],
                    Some(close),
                ),
            )),
        }
    }
}

fn draft_row(c: &Ctx<'_>, rid: &str, tmid: Option<&str>, text_: &str) -> Element<'static, Msg> {
    let where_ = place(c.workspace, c.prefs, rid, tmid);
    let info = column![
        row![
            icon::sized(Icon::Pencil, 14.0).style(style::text_secondary),
            text(where_)
                .size(TEXT_SMALL)
                .font(font_semibold())
                .wrapping(text::Wrapping::None),
        ]
        .spacing(SPACE_1)
        .align_y(Alignment::Center),
        text(preview(text_, PREVIEW_CHARS)).size(TEXT_BODY),
    ]
    .spacing(SPACE_1)
    .width(Length::Fill);
    container(
        button(info)
            .padding([SPACE_2, SPACE_4])
            .width(Length::Fill)
            .style(style::button_list_item(false))
            .on_press(Msg::Open {
                rid: rid.to_owned(),
                tmid: tmid.map(str::to_owned),
            }),
    )
    .width(Length::Fill)
    .into()
}

/// When a missed scheduled message was due, with why it was not sent when
/// it came back from the outbox.
pub fn missed_when(time: String, error: Option<&str>) -> String {
    match error {
        Some(e) => format!("{time} · {e}"),
        None => time,
    }
}
