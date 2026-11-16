//! Modal dialogs of message lists: delete confirmation, report, who reacted,
//! image lightbox.

use iced::widget::{button, column, container, image, row, scrollable, text, text_input};
use iced::{Alignment, ContentFit, Element, Length};
use rust_i18n::t;
use rustket_core::reactions::{self, ReactionGroup};
use rustket_ui::icon::Icon;
use rustket_ui::style;
use rustket_ui::theme::{
    SPACE_1, SPACE_2, SPACE_3, SPACE_4, TEXT_BODY, TEXT_CAPTION, TEXT_SMALL, font_semibold,
};
use rustket_ui::widget::{
    danger_button, dialog, ellipsis, icon_button_with, loading_placeholder, reaction_pill,
    sanitize_bidi, secondary_button,
};

use super::interact::Msg as Message;
use super::viewer::ViewerMsg;
use crate::images::Loaded;
use crate::room::ViewCtx;
use crate::room::panel::common;
use crate::room::render::{ImageRef, RenderCtx, RenderState, emoji_element, emoji_piece};

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
    /// Who reacted to `mid`; `tab` is one emoji, `None` all of them.
    Reactions {
        mid: String,
        tab: Option<String>,
    },
    /// "Remind me about this message".
    Remind {
        mid: String,
        rid: String,
        tmid: Option<String>,
        picker: crate::later::when::WhenPicker,
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

/// The reminder dialog: a line of the message and the time picker.
pub fn remind<'a>(
    excerpt: String,
    picker: &'a crate::later::when::WhenPicker,
    ctx: &ViewCtx<'a>,
) -> Element<'a, Message> {
    let now = chrono::Local::now();
    dialog(
        t!("room_timeline.remind_title").into_owned(),
        column![
            text(excerpt).size(TEXT_SMALL).style(style::text_secondary),
            picker
                .view(now, ctx.prefs.morning_hour(), ctx.prefs.clock_24h)
                .map(Message::Remind),
        ]
        .spacing(SPACE_3),
        vec![
            secondary_button(
                t!("room_timeline.cancel").into_owned(),
                Some(Message::CloseDialog),
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

/// Who reacted: one tab for all reactions and one per emoji, then the users
/// (a click opens their profile).
pub fn reactions<'a>(
    groups: &[ReactionGroup],
    tab: Option<&str>,
    ctx: &ViewCtx<'a>,
) -> Element<'a, Message> {
    let real = common::use_real_names(ctx);
    let rc = RenderCtx {
        interactive: false,
        ..RenderCtx::from_view(ctx, &EMPTY)
    };
    // A tab whose emoji is gone falls back to all.
    let tab = tab.filter(|t| groups.iter().any(|g| g.emoji == *t));
    let close = Message::CloseDialog;
    if groups.is_empty() {
        return dialog(
            t!("room_timeline.reactions_title").into_owned(),
            text(t!("room_timeline.reactions_empty").into_owned())
                .size(TEXT_BODY)
                .style(style::text_hint),
            Vec::new(),
            Some(close),
        );
    }
    let mut tabs: Vec<Element<'a, Message>> = vec![
        button(
            text(
                t!(
                    "room_timeline.reactions_all",
                    count = reactions::total(groups)
                )
                .into_owned(),
            )
            .size(TEXT_CAPTION)
            .font(font_semibold()),
        )
        .padding([3, 10])
        .style(style::button_reaction(tab.is_none()))
        .on_press(Message::ReactionsTab(None))
        .into(),
    ];
    for g in groups {
        tabs.push(reaction_pill(
            emoji_element::<Message>(emoji_piece(&g.emoji, &rc), 16.0),
            u32::try_from(g.users.len()).unwrap_or(u32::MAX),
            tab == Some(g.emoji.as_str()),
            Some(Message::ReactionsTab(Some(g.emoji.clone()))),
        ));
    }
    let rows: Vec<(reactions::Reactor, Vec<String>)> = match tab {
        None => reactions::by_user(groups),
        Some(t) => groups
            .iter()
            .filter(|g| g.emoji == t)
            .flat_map(|g| g.users.iter().map(|u| (u.clone(), Vec::new())))
            .collect(),
    };
    let mut list = column![].spacing(2).width(Length::Fill);
    for (u, emojis) in rows {
        let shown = sanitize_bidi(&common::user_display(
            ctx.workspace,
            real,
            &u.username,
            u.name.as_deref(),
        ))
        .into_owned();
        let secondary = if real && u.name.as_deref().is_some_and(|n| !n.trim().is_empty()) {
            format!("@{}", u.username)
        } else {
            u.name.clone().unwrap_or_default()
        };
        // Without a second line the name is centred with the avatar.
        let avatar = common::user_avatar(ctx, &u.username, &shown, 28.0);
        let mut names = column![ellipsis(shown).size(TEXT_BODY).font(font_semibold())];
        if !secondary.trim().is_empty() {
            names = names.push(
                ellipsis(sanitize_bidi(&secondary).into_owned())
                    .size(TEXT_SMALL)
                    .style(|theme: &iced::Theme| style::text_secondary(theme).color),
            );
        }
        let mut line = row![avatar, names.width(Length::Fill)]
            .spacing(SPACE_2)
            .align_y(Alignment::Center);
        if !emojis.is_empty() {
            let mut used = row![].spacing(SPACE_1).align_y(Alignment::Center);
            for e in &emojis {
                used = used.push(emoji_element::<Message>(emoji_piece(e, &rc), 16.0));
            }
            line = line.push(used);
        }
        list = list.push(
            button(line)
                .padding([SPACE_1, SPACE_2])
                .width(Length::Fill)
                .style(style::button_list_item(false))
                .on_press(Message::OpenUser(u.username.clone())),
        );
    }
    dialog(
        t!("room_timeline.reactions_title").into_owned(),
        column![
            row(tabs).spacing(SPACE_1).wrap().vertical_spacing(SPACE_1),
            container(scrollable(list).height(Length::Shrink))
                .max_height(360)
                .width(Length::Fill),
        ]
        .spacing(SPACE_3),
        Vec::new(),
        Some(close),
    )
}

static EMPTY: RenderState = RenderState::new();

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
            .font(font_semibold())
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
