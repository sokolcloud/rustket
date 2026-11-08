//! Modal dialogs of message lists: delete confirmation, report, image lightbox.

use iced::widget::{column, container, image, row, text, text_input};
use iced::{Alignment, ContentFit, Element, Length};
use rust_i18n::t;
use rustket_ui::icon::Icon;
use rustket_ui::style;
use rustket_ui::theme::{FONT_SEMIBOLD, SPACE_2, SPACE_3, SPACE_4, TEXT_BODY, TEXT_SMALL};
use rustket_ui::widget::{
    danger_button, dialog, ellipsis, icon_button_with, loading_placeholder, secondary_button,
};

use super::interact::Msg as Message;
use super::viewer::ViewerMsg;
use crate::images::Loaded;
use crate::room::render::ImageRef;

/// The open dialog.
#[derive(Debug, Clone, PartialEq)]
pub enum Dialog {
    Delete {
        mid: String,
        busy: bool,
    },
    Report {
        mid: String,
        description: String,
        busy: bool,
    },
}

pub const REPORT_INPUT_ID: &str = "timeline-report-description";

pub fn delete(busy: bool) -> Element<'static, Message> {
    dialog(
        t!("room_timeline.delete_title").into_owned(),
        text(t!("room_timeline.delete_body").into_owned()).size(TEXT_BODY),
        vec![
            secondary_button(
                t!("room_timeline.cancel").into_owned(),
                Some(Message::CloseDialog),
            )
            .into(),
            danger_button(
                t!("room_timeline.delete_confirm").into_owned(),
                (!busy).then_some(Message::ConfirmDelete),
            )
            .into(),
        ],
        Some(Message::CloseDialog),
    )
}

pub fn report(description: &str, busy: bool) -> Element<'static, Message> {
    let ready = !description.trim().is_empty() && !busy;
    let input = text_input(&t!("room_timeline.report_placeholder"), description)
        .id(REPORT_INPUT_ID)
        .on_input(Message::ReportText)
        .on_submit_maybe(ready.then_some(Message::SubmitReport))
        .padding([SPACE_2, SPACE_3])
        .style(style::text_input);
    dialog(
        t!("room_timeline.report_title").into_owned(),
        column![
            text(t!("room_timeline.report_body").into_owned())
                .size(TEXT_SMALL)
                .style(style::text_secondary),
            input,
        ]
        .spacing(SPACE_3),
        vec![
            secondary_button(
                t!("room_timeline.cancel").into_owned(),
                Some(Message::CloseDialog),
            )
            .into(),
            danger_button(
                t!("room_timeline.report_send").into_owned(),
                ready.then_some(Message::SubmitReport),
            )
            .into(),
        ],
        Some(Message::CloseDialog),
    )
}

/// Full-window image viewer: fit or 1:1 with zoom, open in browser, save.
pub fn lightbox(
    img: &ImageRef,
    zoom: bool,
    loaded: Option<&Loaded>,
) -> Element<'static, ViewerMsg> {
    let tool =
        |i: Icon, label: String, msg: ViewerMsg, active: bool| -> Element<'static, ViewerMsg> {
            icon_button_with(i, Some(msg), label, active, 32.0)
        };
    let header = row![
        ellipsis(img.title.clone())
            .size(TEXT_BODY)
            .font(FONT_SEMIBOLD)
            .width(Length::Fill),
        tool(
            if zoom { Icon::Minimize } else { Icon::ZoomIn },
            if zoom {
                t!("room_timeline.image_fit").into_owned()
            } else {
                t!("room_timeline.image_zoom").into_owned()
            },
            ViewerMsg::ToggleZoom,
            zoom,
        ),
        tool(
            Icon::ExternalLink,
            t!("room_timeline.open_in_viewer").into_owned(),
            ViewerMsg::OpenImageExternally,
            false,
        ),
        tool(
            Icon::Download,
            t!("room_timeline.save_image").into_owned(),
            ViewerMsg::SaveImage,
            false,
        ),
        tool(
            Icon::X,
            t!("room_timeline.close").into_owned(),
            ViewerMsg::CloseImage,
            false
        ),
    ]
    .spacing(SPACE_2)
    .align_y(Alignment::Center);
    let body: Element<'static, ViewerMsg> = match loaded {
        Some(l) if zoom => image::viewer(l.handle.clone())
            .width(Length::Fill)
            .height(Length::Fill)
            .min_scale(0.25)
            .max_scale(8.0)
            .into(),
        Some(l) => container(
            image(l.handle.clone())
                .content_fit(ContentFit::ScaleDown)
                .width(Length::Fill)
                .height(Length::Fill),
        )
        .center(Length::Fill)
        .into(),
        None => loading_placeholder(None),
    };
    let size = loaded
        .map(|l| format!("{} × {}", l.width, l.height))
        .unwrap_or_default();
    container(
        column![
            header,
            container(body).height(Length::Fill).width(Length::Fill),
            text(size).size(TEXT_SMALL).style(style::text_hint),
        ]
        .spacing(SPACE_3),
    )
    .padding(SPACE_4)
    .width(Length::Fill)
    .height(Length::Fill)
    .style(style::container_modal)
    .into()
}
