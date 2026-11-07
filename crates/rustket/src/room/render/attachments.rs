//! Attachments (images, files, audio/video, quotes, integration cards) and
//! link previews.

use chrono::Local;
use iced::widget::{Column, button, column, container, image, progress_bar, row, space, text};
use iced::{Alignment, Color, ContentFit, Element, Length, Padding, Theme, border};
use rocketchat::models::{AttachmentField, Message, MessageAttachment, MessageUrl};
use rust_i18n::t;
use rustket_ui::icon::{self, Icon};
use rustket_ui::style;
use rustket_ui::theme::{
    FONT_BOLD, FONT_SEMIBOLD, RADIUS_MD, RADIUS_SM, SPACE_1, SPACE_2, SPACE_3, TEXT_BODY,
    TEXT_CAPTION, TEXT_SMALL, tokens,
};
use rustket_ui::widget::{AvatarShape, AvatarSource, TooltipPosition, avatar, ellipsis, tooltip};

use super::md::{Cx, blocks_for, format_date_with, quote};
use super::preview::preview_handle;
use super::{FileRef, ImageRef, RenderCtx, RenderMsg, Transfer, safe_url};

type El = Element<'static, RenderMsg>;

fn clean(s: &str) -> String {
    rustket_ui::widget::sanitize_bidi(s).into_owned()
}

/// Largest image box in the timeline.
const MAX_W: f32 = 360.0;
const MAX_H: f32 = 300.0;
/// Deepest nesting of quoted attachments.
const MAX_NESTING: usize = 3;
/// Index offset of link previews in the collapse state.
const PREVIEW_BASE: usize = 10_000;

pub(super) fn attachments(msg: &Message, rc: &RenderCtx<'_>) -> Vec<El> {
    let Some(list) = &msg.attachments else {
        return Vec::new();
    };
    let cx = Cx::new(rc, &msg.id, Some(msg));
    let mut counter = 0;
    list.iter()
        .map(|a| attachment(a, msg, &cx, &mut counter, 0))
        .collect()
}

fn attachment(
    a: &MessageAttachment,
    msg: &Message,
    cx: &Cx<'_, '_>,
    counter: &mut usize,
    depth: usize,
) -> El {
    let idx = *counter;
    *counter += 1;
    if a.kind.as_deref() == Some("removed-file") {
        return text(t!("room_render.file_removed").into_owned())
            .size(TEXT_SMALL)
            .style(style::text_hint)
            .into();
    }
    if a.is_quote() {
        return quote_card(a, msg, cx, counter, depth);
    }
    let collapsed = a.collapsed.unwrap_or(false) != cx.rc.state.is_toggled(cx.mid, idx);
    let mut col: Column<'static, RenderMsg> = column![].spacing(SPACE_1);
    let is_file = a.kind.as_deref() == Some("file");
    // Upload messages: description above the media.
    if is_file && let Some(d) = description(a, cx) {
        col = col.push(d);
    }
    if a.image_url.is_some() {
        if is_file {
            col = col.push(media_title(a, idx, collapsed, cx));
        }
        if !collapsed {
            col = col.push(image_attachment(a, cx));
        }
        // 8.x stores an upload's description as `image_alt` (markdown text).
        if is_file
            && a.description.is_none()
            && a.description_md.is_none()
            && let Some(alt) = a
                .image_alt
                .as_deref()
                .filter(|t| !t.trim().is_empty() && Some(*t) != a.title.as_deref())
        {
            col = col.push(super::md::plain_paragraph(alt, cx.mid, cx.rc));
        }
        return col.into();
    }
    if a.audio_url.is_some() || a.video_url.is_some() || (is_file && a.title_link.is_some()) {
        col = col.push(file_card(a, msg, cx));
        return col.into();
    }
    col = col.push(generic_card(a, msg, cx, counter, depth, idx, collapsed));
    col.into()
}

fn description(a: &MessageAttachment, cx: &Cx<'_, '_>) -> Option<El> {
    match (&a.description_md, &a.description) {
        (Some(md), _) if !md.is_empty() => Some(blocks_for(md, cx)),
        (_, Some(d)) if !d.trim().is_empty() => Some(super::md::plain_paragraph(d, cx.mid, cx.rc)),
        _ => None,
    }
}

/// File name line above an image with a collapse toggle.
fn media_title(a: &MessageAttachment, idx: usize, collapsed: bool, cx: &Cx<'_, '_>) -> El {
    let name = clean(a.title.as_deref().unwrap_or_default());
    let mut r = row![
        ellipsis(name)
            .size(TEXT_CAPTION)
            .style(|theme: &Theme| Some(tokens(theme).text_secondary))
            .width(Length::Shrink),
    ]
    .spacing(SPACE_1)
    .align_y(Alignment::Center);
    if let Some(size) = a.image_size.or(a.size) {
        r = r.push(
            text(format!("({})", human_size(size)))
                .size(TEXT_CAPTION)
                .style(style::text_hint),
        );
    }
    r = r.push(collapse_button(cx.mid, idx, collapsed));
    r.into()
}

fn collapse_button(mid: &str, idx: usize, collapsed: bool) -> El {
    let (i, label) = if collapsed {
        (Icon::ChevronRight, t!("room_render.expand"))
    } else {
        (Icon::ChevronDown, t!("room_render.collapse"))
    };
    tooltip(
        button(icon::sized(i, 14.0))
            .padding(2)
            .style(style::button_icon)
            .on_press(RenderMsg::ToggleCollapsed(mid.to_owned(), idx)),
        label.into_owned(),
        TooltipPosition::Top,
    )
}

/// Fits `w`×`h` into the timeline image box.
pub(crate) fn fit_size(w: f32, h: f32, max_w: f32, max_h: f32) -> (f32, f32) {
    if w <= 0.0 || h <= 0.0 {
        return (max_w.min(240.0), max_h.min(180.0));
    }
    let scale = (max_w / w).min(max_h / h).min(1.0);
    ((w * scale).max(16.0), (h * scale).max(16.0))
}

fn image_attachment(a: &MessageAttachment, cx: &Cx<'_, '_>) -> El {
    let rc = cx.rc;
    let thumb = rc
        .server
        .absolute(a.image_url.as_deref().unwrap_or_default());
    // The lightbox shows the original upload (`title_link`) when there is one.
    let full = a
        .title_link
        .as_deref()
        .filter(|_| a.kind.as_deref() == Some("file"))
        .map_or_else(|| thumb.clone(), |l| rc.server.absolute(l));
    let loaded = rc.images.get(rc.server_id, &thumb);
    let (w, h) = match (a.image_dimensions, loaded) {
        (Some(d), _) if d.width > 0 && d.height > 0 => (d.width as f32, d.height as f32),
        (_, Some(l)) => (l.width as f32, l.height as f32),
        _ => (0.0, 0.0),
    };
    let (bw, bh) = fit_size(w, h, MAX_W, MAX_H);
    let content: El = match (
        loaded,
        a.image_preview
            .as_deref()
            .and_then(|p| preview_handle(cx.mid, p)),
    ) {
        (Some(l), _) => image(l.handle.clone())
            .width(bw)
            .height(bh)
            .content_fit(ContentFit::Contain)
            .border_radius(RADIUS_MD)
            .into(),
        (None, Some(p)) => image(p)
            .width(bw)
            .height(bh)
            .content_fit(ContentFit::Cover)
            .opacity(0.7_f32)
            .border_radius(RADIUS_MD)
            .into(),
        (None, None) => container(icon::sized(Icon::Image, 32.0).style(style::text_hint))
            .center_x(bw)
            .center_y(bh)
            .style(|theme: &Theme| container::Style {
                background: Some(tokens(theme).surface_sunken.into()),
                border: border::rounded(RADIUS_MD),
                ..container::Style::default()
            })
            .into(),
    };
    let title = a
        .title
        .clone()
        .or_else(|| a.image_alt.clone())
        .unwrap_or_default();
    button(content)
        .padding(0)
        .style(style::button_bare)
        .on_press(RenderMsg::OpenImage(ImageRef {
            mid: cx.mid.to_owned(),
            url: full,
            title,
        }))
        .into()
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Media {
    Audio,
    Video,
    File,
}

/// Icon for a file by MIME type / extension.
pub(crate) fn file_icon(mime: Option<&str>, name: &str) -> Icon {
    let mime = mime.unwrap_or_default();
    let ext = name
        .rsplit_once('.')
        .map(|(_, e)| e.to_ascii_lowercase())
        .unwrap_or_default();
    if mime.starts_with("image/") {
        Icon::FileImage
    } else if mime.starts_with("audio/") {
        Icon::FileMusic
    } else if mime.starts_with("video/") {
        Icon::FileVideo
    } else if mime.contains("zip")
        || mime.contains("compressed")
        || mime.contains("tar")
        || matches!(
            ext.as_str(),
            "zip" | "gz" | "tgz" | "7z" | "rar" | "xz" | "bz2" | "zst"
        )
    {
        Icon::FileArchive
    } else if mime.contains("spreadsheet")
        || mime.contains("excel")
        || matches!(ext.as_str(), "csv" | "xls" | "xlsx" | "ods")
    {
        Icon::FileSpreadsheet
    } else if mime.starts_with("text/")
        || mime == "application/pdf"
        || mime.contains("document")
        || matches!(
            ext.as_str(),
            "txt" | "md" | "pdf" | "doc" | "docx" | "odt" | "rtf"
        )
    {
        Icon::FileText
    } else if mime.contains("json")
        || mime.contains("javascript")
        || mime.contains("xml")
        || matches!(
            ext.as_str(),
            "rs" | "js"
                | "ts"
                | "py"
                | "c"
                | "h"
                | "cpp"
                | "go"
                | "java"
                | "sh"
                | "toml"
                | "yml"
                | "yaml"
        )
    {
        Icon::FileCode
    } else {
        Icon::File
    }
}

pub(crate) fn human_size(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut v = bytes as f64;
    let mut u = 0;
    while v >= 1024.0 && u < UNITS.len() - 1 {
        v /= 1024.0;
        u += 1;
    }
    if u == 0 {
        format!("{bytes} B")
    } else if v < 10.0 {
        format!("{v:.1} {}", UNITS[u])
    } else {
        format!("{v:.0} {}", UNITS[u])
    }
}

fn file_card(a: &MessageAttachment, msg: &Message, cx: &Cx<'_, '_>) -> El {
    let rc = cx.rc;
    let (media, url, mime, size) = if let Some(u) = &a.audio_url {
        (Media::Audio, u.clone(), a.audio_type.clone(), a.audio_size)
    } else if let Some(u) = &a.video_url {
        (Media::Video, u.clone(), a.video_type.clone(), a.video_size)
    } else {
        (
            Media::File,
            a.title_link.clone().unwrap_or_default(),
            a.format
                .clone()
                .or_else(|| msg.file.as_ref().and_then(|f| f.mime.clone())),
            a.size.or_else(|| msg.file.as_ref().and_then(|f| f.size)),
        )
    };
    let url = rc.server.absolute(&url);
    let name = a
        .title
        .clone()
        .or_else(|| msg.file.as_ref().map(|f| f.name.clone()))
        .unwrap_or_else(|| t!("room_render.file").into_owned());
    let file = FileRef {
        mid: cx.mid.to_owned(),
        url: url.clone(),
        name: name.clone(),
        size,
    };
    let glyph = match media {
        Media::Audio => Icon::FileMusic,
        Media::Video => Icon::FileVideo,
        Media::File => file_icon(mime.as_deref(), &name),
    };
    let mut info = column![
        ellipsis(clean(&name))
            .size(TEXT_BODY)
            .font(FONT_SEMIBOLD)
            .width(Length::Fill)
    ]
    .spacing(2)
    .width(Length::Fill);
    let mut meta = Vec::new();
    if let Some(s) = size {
        meta.push(human_size(s));
    }
    if let Some(ext) = name.rsplit_once('.').map(|(_, e)| e.to_ascii_uppercase()) {
        meta.push(ext);
    }
    let transfer = rc.state.transfer(cx.mid, &url);
    match transfer {
        Some(Transfer::Running { received, total }) => {
            let total = total.or(size).unwrap_or(0);
            let frac = if total > 0 {
                (*received as f32 / total as f32).clamp(0.0, 1.0)
            } else {
                0.0
            };
            info = info.push(progress_bar(0.0..=1.0, frac).girth(4));
            info = info.push(
                text(
                    t!(
                        "room_render.downloading",
                        received = human_size(*received),
                        total = if total > 0 {
                            human_size(total)
                        } else {
                            "?".into()
                        }
                    )
                    .into_owned(),
                )
                .size(TEXT_CAPTION)
                .style(style::text_hint),
            );
        }
        Some(Transfer::Failed(e)) => {
            info = info.push(
                text(t!("room_render.download_failed", error = e.as_str()).into_owned())
                    .size(TEXT_CAPTION)
                    .style(style::text_danger),
            );
        }
        _ => {
            info = info.push(
                text(meta.join(" · "))
                    .size(TEXT_CAPTION)
                    .style(style::text_hint),
            );
        }
    }
    let mut actions = row![].spacing(SPACE_1).align_y(Alignment::Center);
    if media != Media::File {
        actions = actions.push(small_icon_button(
            Icon::Play,
            t!("room_render.play").into_owned(),
            (!matches!(transfer, Some(Transfer::Running { .. })))
                .then(|| RenderMsg::Play(file.clone())),
        ));
    }
    match transfer {
        Some(Transfer::Done(path)) => {
            actions = actions.push(small_icon_button(
                Icon::FolderOpen,
                t!("room_render.open_file").into_owned(),
                Some(RenderMsg::OpenFile(path.clone())),
            ));
        }
        Some(Transfer::Running { .. }) => {}
        _ => {
            actions = actions.push(small_icon_button(
                Icon::Download,
                t!("room_render.download").into_owned(),
                Some(RenderMsg::Download(file.clone())),
            ));
        }
    }
    let card = row![
        container(icon::sized(glyph, 24.0).style(style::text_secondary))
            .center_x(40)
            .center_y(40)
            .style(|theme: &Theme| container::Style {
                background: Some(tokens(theme).surface_sunken.into()),
                border: border::rounded(RADIUS_SM),
                ..container::Style::default()
            }),
        info,
        actions,
    ]
    .spacing(SPACE_3)
    .align_y(Alignment::Center);
    let mut col = column![
        container(card)
            .padding(SPACE_2)
            .max_width(420)
            .style(style::container_card)
    ]
    .spacing(SPACE_1);
    if a.kind.as_deref() != Some("file")
        && let Some(d) = description(a, cx)
    {
        col = col.push(d);
    }
    col.into()
}

fn small_icon_button(i: Icon, label: String, msg: Option<RenderMsg>) -> El {
    tooltip(
        button(container(icon::sized(i, 16.0)).center(28))
            .padding(0)
            .style(style::button_icon)
            .on_press_maybe(msg),
        label,
        TooltipPosition::Top,
    )
}

fn small_avatar(url: Option<&str>, name: &str, cx: &Cx<'_, '_>) -> El {
    let rc = cx.rc;
    let handle = url
        .filter(|u| !u.is_empty())
        .map(|u| rc.server.absolute(u))
        .and_then(|u| rc.images.get(rc.server_id, &u).map(|l| l.handle.clone()));
    avatar(
        AvatarSource::initials(name, name).with_image(handle),
        20.0,
        AvatarShape::Rounded,
    )
    .into()
}

/// A quoted message (`message_link`), possibly with nested quotes.
fn quote_card(
    a: &MessageAttachment,
    msg: &Message,
    cx: &Cx<'_, '_>,
    counter: &mut usize,
    depth: usize,
) -> El {
    let author = clean(a.author_name.as_deref().unwrap_or_default());
    let mut header = row![small_avatar(a.author_icon.as_deref(), &author, cx)]
        .spacing(SPACE_2)
        .align_y(Alignment::Center);
    header = header.push(
        ellipsis(author)
            .size(TEXT_SMALL)
            .font(FONT_BOLD)
            .width(Length::Shrink),
    );
    if let Some(ts) = a.ts {
        header = header.push(
            text(format_date_with(ts, "f", cx.rc.clock_24h, Local::now()))
                .size(TEXT_CAPTION)
                .style(style::text_hint),
        );
    }
    let mut col = column![header].spacing(SPACE_1);
    match (&a.md, &a.text) {
        (Some(md), _) if !md.is_empty() => col = col.push(blocks_for(md, cx)),
        (_, Some(t)) if !t.trim().is_empty() => {
            col = col.push(super::md::plain_paragraph(t, cx.mid, cx.rc));
        }
        _ => {}
    }
    if let Some(nested) = &a.attachments {
        for n in nested {
            if depth + 1 >= MAX_NESTING {
                break;
            }
            col = col.push(attachment(n, msg, cx, counter, depth + 1));
        }
    }
    if let Some(link) = a.message_link.as_deref().and_then(safe_url) {
        col = col.push(
            button(
                row![
                    icon::sized(Icon::CornerUpLeft, 12.0),
                    text(t!("room_render.jump_to_message").into_owned()).size(TEXT_CAPTION)
                ]
                .spacing(SPACE_1)
                .align_y(Alignment::Center),
            )
            .padding(0)
            .style(style::button_link)
            .on_press(RenderMsg::OpenUrl(link)),
        );
    }
    container(quote(col.into())).max_width(560).into()
}

/// Colour bar colour of an integration attachment.
pub(crate) fn parse_color(s: &str) -> Option<Color> {
    let s = s.trim();
    match s.to_ascii_lowercase().as_str() {
        "good" | "green" => return Some(Color::from_rgb8(0x15, 0x8d, 0x65)),
        "warning" | "yellow" => return Some(Color::from_rgb8(0xf3, 0xbe, 0x08)),
        "danger" | "red" => return Some(Color::from_rgb8(0xec, 0x0d, 0x2a)),
        "blue" => return Some(Color::from_rgb8(0x15, 0x6f, 0xf5)),
        _ => {}
    }
    let hex = s.strip_prefix('#')?;
    let expand = |c: u8| c * 17;
    let v = |i: usize, len: usize| u8::from_str_radix(hex.get(i..i + len)?, 16).ok();
    match hex.len() {
        3 => Some(Color::from_rgb8(
            expand(v(0, 1)?),
            expand(v(1, 1)?),
            expand(v(2, 1)?),
        )),
        6 | 8 => Some(Color::from_rgb8(v(0, 2)?, v(2, 2)?, v(4, 2)?)),
        _ => None,
    }
}

fn bar(color: Option<Color>) -> El {
    container(space().width(4))
        .width(4)
        .height(Length::Fill)
        .style(move |theme: &Theme| container::Style {
            background: Some(color.unwrap_or(tokens(theme).border_strong).into()),
            border: border::rounded(RADIUS_SM),
            ..container::Style::default()
        })
        .into()
}

/// Integration / bot attachment card.
fn generic_card(
    a: &MessageAttachment,
    msg: &Message,
    cx: &Cx<'_, '_>,
    counter: &mut usize,
    depth: usize,
    idx: usize,
    collapsed: bool,
) -> El {
    let rc = cx.rc;
    let mut col = column![].spacing(SPACE_1).width(Length::Fill);
    if let Some(p) = a.pretext.as_deref().filter(|p| !p.trim().is_empty()) {
        col = col.push(super::md::plain_paragraph(p, cx.mid, rc));
    }
    if let Some(name) = a.author_name.as_deref().filter(|n| !n.is_empty()) {
        let name = clean(name);
        let mut r = row![small_avatar(a.author_icon.as_deref(), &name, cx)]
            .spacing(SPACE_2)
            .align_y(Alignment::Center);
        let label = text(name).size(TEXT_SMALL).font(FONT_SEMIBOLD);
        r = match a.author_link.as_deref().and_then(safe_url) {
            Some(l) => r.push(
                button(label)
                    .padding(0)
                    .style(style::button_link)
                    .on_press(RenderMsg::OpenUrl(l)),
            ),
            None => r.push(label),
        };
        col = col.push(r);
    }
    let has_title = a.title.as_deref().is_some_and(|t| !t.is_empty());
    if let Some(title) = a.title.as_deref().filter(|t| !t.is_empty()) {
        let title_el: El = match a
            .title_link
            .as_deref()
            .map(|l| rc.server.absolute(l))
            .and_then(|l| safe_url(&l))
        {
            Some(l) => button(text(clean(title)).size(TEXT_BODY).font(FONT_BOLD))
                .padding(0)
                .style(style::button_link)
                .on_press(RenderMsg::OpenUrl(l))
                .into(),
            None => text(clean(title)).size(TEXT_BODY).font(FONT_BOLD).into(),
        };
        col = col.push(
            row![title_el, collapse_button(cx.mid, idx, collapsed)]
                .spacing(SPACE_1)
                .align_y(Alignment::Center),
        );
    }
    if !(collapsed && has_title) {
        match (&a.md, &a.text) {
            (Some(md), _) if !md.is_empty() => col = col.push(blocks_for(md, cx)),
            (_, Some(t)) if !t.trim().is_empty() => {
                col = col.push(super::md::plain_paragraph(t, cx.mid, rc));
            }
            _ => {}
        }
        if let Some(fields) = &a.fields {
            col = col.push(fields_grid(fields));
        }
        if a.image_url.is_some() {
            col = col.push(image_attachment(a, cx));
        } else if let Some(thumb) = a.thumb_url.as_deref() {
            let url = rc.server.absolute(thumb);
            if let Some(l) = rc.images.get(rc.server_id, &url) {
                col = col.push(
                    image(l.handle.clone())
                        .width(80)
                        .height(80)
                        .content_fit(ContentFit::Cover),
                );
            }
        }
        if let Some(nested) = &a.attachments {
            for n in nested {
                if depth + 1 >= MAX_NESTING {
                    break;
                }
                col = col.push(attachment(n, msg, cx, counter, depth + 1));
            }
        }
    }
    let color = a.color.as_deref().and_then(parse_color);
    row![
        bar(color),
        container(col)
            .padding(Padding::ZERO.left(SPACE_2))
            .width(Length::Fill)
    ]
    .height(Length::Shrink)
    .into()
}

fn field_value(v: &serde_json::Value) -> String {
    match v {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Null => String::new(),
        other => other.to_string(),
    }
}

fn fields_grid(fields: &[AttachmentField]) -> El {
    let cell = |f: &AttachmentField| -> El {
        column![
            text(clean(&f.title)).size(TEXT_SMALL).font(FONT_BOLD),
            text(field_value(&f.value)).size(TEXT_SMALL),
        ]
        .spacing(2)
        .width(Length::FillPortion(1))
        .into()
    };
    let mut col = column![].spacing(SPACE_2);
    let mut i = 0;
    while i < fields.len() {
        let f = &fields[i];
        if f.short == Some(true) && fields.get(i + 1).is_some_and(|n| n.short == Some(true)) {
            col = col.push(row![cell(f), cell(&fields[i + 1])].spacing(SPACE_3));
            i += 2;
        } else {
            col = col.push(cell(f));
            i += 1;
        }
    }
    col.into()
}

/// Link previews from `urls[].meta` (skipping `ignoreParse`).
pub(super) fn link_previews(msg: &Message, rc: &RenderCtx<'_>) -> Vec<El> {
    let Some(urls) = &msg.urls else {
        return Vec::new();
    };
    let cx = Cx::new(rc, &msg.id, Some(msg));
    urls.iter()
        .enumerate()
        .filter(|(_, u)| u.ignore_parse != Some(true))
        .filter_map(|(i, u)| link_preview(u, PREVIEW_BASE + i, &cx))
        .collect()
}

/// Title, description, image and site name of a link preview.
#[derive(Debug, Default, PartialEq)]
pub(crate) struct PreviewData {
    pub title: Option<String>,
    pub description: Option<String>,
    pub image: Option<String>,
    pub site: Option<String>,
}

pub(crate) fn preview_data(u: &MessageUrl) -> Option<PreviewData> {
    let pick = |keys: &[&str]| {
        keys.iter()
            .find_map(|k| u.meta_str(k).map(str::trim).filter(|s| !s.is_empty()))
            .map(str::to_owned)
    };
    let mut d = PreviewData {
        title: pick(&["ogTitle", "twitterTitle", "oembedTitle", "pageTitle"]),
        description: pick(&[
            "ogDescription",
            "twitterDescription",
            "oembedDescription",
            "description",
        ]),
        image: pick(&[
            "ogImage",
            "twitterImage",
            "oembedThumbnailUrl",
            "ogImageUrl",
            "twitterImageSrc",
        ]),
        site: pick(&["ogSiteName", "oembedProviderName"]),
    };
    let is_image = u
        .headers
        .as_ref()
        .and_then(|h| h.content_type.as_deref())
        .is_some_and(|c| c.starts_with("image/"));
    if d.image.is_none() && is_image {
        d.image = Some(u.url.clone());
    }
    if d.title.is_none() && d.description.is_none() && d.image.is_none() {
        return None;
    }
    if d.site.is_none() {
        d.site = url::Url::parse(&u.url)
            .ok()
            .and_then(|p| p.host_str().map(str::to_owned));
    }
    Some(d)
}

fn link_preview(u: &MessageUrl, idx: usize, cx: &Cx<'_, '_>) -> Option<El> {
    let d = preview_data(u)?;
    let rc = cx.rc;
    let target = safe_url(&u.url);
    let collapsed = rc.state.is_toggled(cx.mid, idx);
    let mut head = row![].spacing(SPACE_1).align_y(Alignment::Center);
    if let Some(site) = &d.site {
        head = head.push(text(clean(site)).size(TEXT_CAPTION).style(style::text_hint));
    }
    head = head.push(collapse_button(cx.mid, idx, collapsed));
    let mut col = column![head].spacing(SPACE_1).width(Length::Fill);
    if !collapsed {
        if let Some(title) = &d.title {
            let label: iced::widget::Text<'static> =
                text(clean(title)).size(TEXT_BODY).font(FONT_SEMIBOLD);
            let title_el: El = match &target {
                Some(l) => button(label)
                    .padding(0)
                    .style(style::button_link)
                    .on_press(RenderMsg::OpenUrl(l.clone()))
                    .into(),
                None => label.into(),
            };
            col = col.push(title_el);
        }
        if let Some(desc) = &d.description {
            let short: String = desc.chars().take(300).collect();
            col = col.push(text(short).size(TEXT_SMALL).style(style::text_secondary));
        }
        if let Some(img) = d.image.as_deref().and_then(safe_url)
            && let Some(l) = rc.images.get(rc.server_id, &img)
        {
            let (w, h) = fit_size(l.width as f32, l.height as f32, MAX_W, 200.0);
            let pic = image(l.handle.clone())
                .width(w)
                .height(h)
                .content_fit(ContentFit::Contain)
                .border_radius(RADIUS_SM);
            let pic_el: El = match &target {
                Some(t) => button(pic)
                    .padding(0)
                    .style(style::button_bare)
                    .on_press(RenderMsg::OpenUrl(t.clone()))
                    .into(),
                None => pic.into(),
            };
            col = col.push(pic_el);
        }
    }
    Some(
        container(
            row![
                bar(None),
                container(col)
                    .padding(Padding::ZERO.left(SPACE_2))
                    .width(Length::Fill)
            ]
            .height(Length::Shrink),
        )
        .max_width(480)
        .into(),
    )
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn sizes_and_fit() {
        assert_eq!(human_size(512), "512 B");
        assert_eq!(human_size(1536), "1.5 KB");
        assert_eq!(human_size(25 * 1024 * 1024), "25 MB");
        assert_eq!(fit_size(1600.0, 1200.0, 360.0, 300.0), (360.0, 270.0));
        assert_eq!(fit_size(100.0, 50.0, 360.0, 300.0), (100.0, 50.0));
        assert_eq!(fit_size(0.0, 0.0, 360.0, 300.0), (240.0, 180.0));
    }

    #[test]
    fn icons_and_colors() {
        assert_eq!(
            char::from(file_icon(Some("application/zip"), "a.zip")),
            char::from(Icon::FileArchive)
        );
        assert_eq!(
            char::from(file_icon(None, "notes.md")),
            char::from(Icon::FileText)
        );
        assert_eq!(
            char::from(file_icon(Some("audio/ogg"), "x")),
            char::from(Icon::FileMusic)
        );
        assert_eq!(
            char::from(file_icon(None, "main.rs")),
            char::from(Icon::FileCode)
        );
        assert_eq!(char::from(file_icon(None, "blob")), char::from(Icon::File));
        assert_eq!(parse_color("#ff0000"), Some(Color::from_rgb8(255, 0, 0)));
        assert_eq!(parse_color("#0f0"), Some(Color::from_rgb8(0, 255, 0)));
        assert!(parse_color("good").is_some());
        assert_eq!(parse_color("nope"), None);
    }

    #[test]
    fn preview_meta() {
        let u: MessageUrl = serde_json::from_value(serde_json::json!({
            "url": "https://rust-lang.org/learn",
            "meta": {"pageTitle": "Learn", "ogDescription": "Docs", "ogImage": "https://rust-lang.org/i.png"}
        }))
        .unwrap();
        let d = preview_data(&u).unwrap();
        assert_eq!(d.title.as_deref(), Some("Learn"));
        assert_eq!(d.description.as_deref(), Some("Docs"));
        assert_eq!(d.site.as_deref(), Some("rust-lang.org"));
        let empty: MessageUrl =
            serde_json::from_value(serde_json::json!({"url": "https://x.y", "meta": {}})).unwrap();
        assert_eq!(preview_data(&empty), None);
    }
}
