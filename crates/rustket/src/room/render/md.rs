//! `md` AST (`@rocket.chat/message-parser`) → iced widgets. Paragraph-level
//! content becomes `rich_text` spans; blocks become containers.

use std::cell::Cell;

use chrono::{DateTime, Local, TimeZone};
use iced::font::{Style as FontStyle, Weight};
use iced::widget::scrollable::{Direction, Scrollbar};
use iced::widget::text::{Span, Wrapping};
use iced::widget::{
    Column, Row, button, column, container, image, rich_text, row, scrollable, space, span, text,
};
use iced::{Alignment, Color, Element, Font, Length, Padding, border};
use rocketchat::RcDate;
use rocketchat::models::Message;
use rocketchat::models::md::{Block, CodeLine, Inline, ListItem, TableCell, TimestampValue};
use rust_i18n::t;
use rustket_core::emoji::ResolvedEmoji;
use rustket_ui::Tokens;
use rustket_ui::emoji::{EMOJI_FONT, segments};
use rustket_ui::icon::{self, Icon};
use rustket_ui::style;
use rustket_ui::theme::{
    DEFAULT_FONT, FONT_SEMIBOLD, MONOSPACE_FONT, RADIUS_SM, SPACE_1, SPACE_2, SPACE_3, TEXT_BODY,
    TEXT_CAPTION, TEXT_EMOJI_LARGE, TEXT_HEADLINE, TEXT_SMALL, TEXT_SUBTITLE, TEXT_TITLE,
};
use rustket_ui::widget::{TooltipPosition, divider, tooltip};
use serde_json::Value;

use super::linkify::{find_links, safe_url};
use super::{RenderCtx, RenderMsg, tokens};

type El = Element<'static, RenderMsg>;
type Sp = Span<'static, RenderMsg>;

/// Deepest block nesting rendered (quotes in quotes …); deeper content is
/// flattened to text.
const MAX_DEPTH: usize = 8;

pub(super) struct Cx<'r, 'a> {
    pub rc: &'r RenderCtx<'a>,
    pub mid: &'r str,
    pub msg: Option<&'r Message>,
    pub me: Option<&'r str>,
    pub t: &'static Tokens,
    spoiler: Cell<usize>,
}

impl<'r, 'a> Cx<'r, 'a> {
    pub fn new(rc: &'r RenderCtx<'a>, mid: &'r str, msg: Option<&'r Message>) -> Self {
        Self {
            rc,
            mid,
            msg,
            me: rc.workspace.username(),
            t: tokens(),
            spoiler: Cell::new(0),
        }
    }

    fn next_spoiler(&self) -> usize {
        let i = self.spoiler.get();
        self.spoiler.set(i + 1);
        i
    }
}

/// Inherited inline formatting.
#[derive(Clone, Default)]
struct Fmt {
    bold: bool,
    italic: bool,
    strike: bool,
    link: Option<RenderMsg>,
    color: Option<Color>,
    background: Option<Color>,
    size: Option<f32>,
}

impl Fmt {
    fn font(&self) -> Font {
        Font {
            weight: if self.bold {
                Weight::Bold
            } else {
                Weight::Normal
            },
            style: if self.italic {
                FontStyle::Italic
            } else {
                FontStyle::Normal
            },
            ..DEFAULT_FONT
        }
    }
}

/// Renders a whole `md` root.
pub(super) fn blocks(root: &[Block], mid: &str, msg: &Message, rc: &RenderCtx<'_>) -> El {
    let cx = Cx::new(rc, mid, Some(msg));
    block_list(root, &cx, 0)
}

/// Renders an `md` root that is not the message's own (attachment text).
pub(super) fn blocks_for(root: &[Block], cx: &Cx<'_, '_>) -> El {
    block_list(root, cx, 0)
}

/// Plain text with clickable URLs (messages without `md`).
pub(super) fn plain_paragraph(text: &str, mid: &str, rc: &RenderCtx<'_>) -> El {
    let cx = Cx::new(rc, mid, None);
    let mut spans = Vec::new();
    push_linkified(&mut spans, text, &Fmt::default(), &cx);
    paragraph(spans, TEXT_BODY)
}

fn block_list(list: &[Block], cx: &Cx<'_, '_>, depth: usize) -> El {
    let mut col: Column<'static, RenderMsg> = column![].spacing(SPACE_1).width(Length::Fill);
    for b in list {
        col = col.push(block(b, cx, depth));
    }
    col.into()
}

fn block(b: &Block, cx: &Cx<'_, '_>, depth: usize) -> El {
    if depth > MAX_DEPTH {
        let mut s = String::new();
        b.plain_text(&mut s);
        return paragraph(vec![span(s)], TEXT_BODY);
    }
    match b {
        Block::Paragraph { value } => paragraph(inline_spans(value, cx), TEXT_BODY),
        Block::BigEmoji { value } => big_emoji(value, cx),
        Block::Code { language, value } => code_block(language.as_deref(), value),
        Block::Heading { level, value } => {
            let size = match level {
                1 => TEXT_HEADLINE,
                2 => TEXT_TITLE,
                3 => TEXT_SUBTITLE,
                _ => TEXT_BODY,
            };
            let fmt = Fmt {
                bold: true,
                size: Some(size),
                ..Fmt::default()
            };
            let mut spans = Vec::new();
            for i in value {
                push_inline(&mut spans, i, &fmt, cx);
            }
            paragraph(spans, size)
        }
        Block::Quote { value } | Block::Blockquote { value } => {
            quote(block_list(value, cx, depth + 1))
        }
        Block::SpoilerBlock { value } => {
            let idx = cx.next_spoiler();
            if cx.rc.state.is_revealed(cx.mid, idx) {
                let inner = block_list(value, cx, depth + 1);
                container(inner)
                    .padding([SPACE_1, SPACE_2])
                    .style(style::container_inline_code)
                    .width(Length::Fill)
                    .into()
            } else {
                hidden_spoiler(cx.mid, idx)
            }
        }
        Block::UnorderedList { value } => list(value, cx, false),
        Block::OrderedList { value } => list(value, cx, true),
        Block::Tasks { value } => {
            let mut col = column![].spacing(2);
            for task in value {
                let mark = if task.status {
                    icon::sized(Icon::SquareCheckBig, 16.0).style(style::text_success)
                } else {
                    icon::sized(Icon::Square, 16.0).style(style::text_hint)
                };
                col = col.push(
                    row![
                        container(mark).padding(Padding::ZERO.top(2)),
                        paragraph(inline_spans(&task.value, cx), TEXT_BODY)
                    ]
                    .spacing(SPACE_2)
                    .align_y(Alignment::Start),
                );
            }
            col.into()
        }
        Block::ListItem { value, number } => {
            let marker = number.map_or_else(|| "•".to_owned(), |n| format!("{n}."));
            list_row(marker, inline_spans(value, cx))
        }
        Block::LineBreak => space().height(SPACE_1).into(),
        Block::HorizontalRule { .. } => container(divider()).padding([SPACE_1, 0.0]).into(),
        Block::Katex { value } => katex_block(value),
        Block::Table { value, .. } => {
            table(&value.header, value.rows.iter().map(|r| &r.value[..]), cx)
        }
        Block::Unknown(v) => {
            let mut s = String::new();
            unknown_text(v, &mut s, 0);
            if s.is_empty() {
                space().height(0).into()
            } else {
                paragraph(vec![span(s)], TEXT_BODY)
            }
        }
    }
}

/// A paragraph of spans that wraps to the available width.
fn paragraph(spans: Vec<Sp>, size: f32) -> El {
    rich_text(spans)
        .on_link_click(|m: RenderMsg| m)
        .size(size)
        .width(Length::Fill)
        .into()
}

fn inline_spans(list: &[Inline], cx: &Cx<'_, '_>) -> Vec<Sp> {
    let mut out = Vec::new();
    let fmt = Fmt::default();
    for i in list {
        push_inline(&mut out, i, &fmt, cx);
    }
    out
}

fn styled(text: String, fmt: &Fmt, emoji: bool) -> Sp {
    let mut s = span(text)
        .font(if emoji { EMOJI_FONT } else { fmt.font() })
        .strikethrough(fmt.strike)
        .underline(fmt.link.is_some() && fmt.background.is_none())
        .color_maybe(fmt.color)
        .link_maybe(fmt.link.clone());
    if let Some(size) = fmt.size {
        s = s.size(size);
    }
    if let Some(bg) = fmt.background {
        s = s.background(bg);
    }
    s
}

/// Text with emoji runs in the colour emoji font.
fn push_text(out: &mut Vec<Sp>, text: &str, fmt: &Fmt) {
    for (emoji, run) in segments(text) {
        out.push(styled(run.to_owned(), fmt, emoji));
    }
}

fn push_linkified(out: &mut Vec<Sp>, text: &str, fmt: &Fmt, cx: &Cx<'_, '_>) {
    let mut last = 0;
    for (a, b) in find_links(text) {
        if a > last {
            push_text(out, &text[last..a], fmt);
        }
        let url = &text[a..b];
        match safe_url(url) {
            Some(target) => {
                let f = Fmt {
                    link: Some(RenderMsg::OpenUrl(target)),
                    color: Some(cx.t.link),
                    ..fmt.clone()
                };
                push_text(out, url, &f);
            }
            None => push_text(out, url, fmt),
        }
        last = b;
    }
    if last < text.len() {
        push_text(out, &text[last..], fmt);
    }
}

fn push_inline(out: &mut Vec<Sp>, node: &Inline, fmt: &Fmt, cx: &Cx<'_, '_>) {
    let t = cx.t;
    match node {
        Inline::PlainText { value } => push_text(out, value, fmt),
        Inline::Bold { value } => {
            let f = Fmt {
                bold: true,
                ..fmt.clone()
            };
            value.iter().for_each(|i| push_inline(out, i, &f, cx));
        }
        Inline::Italic { value } => {
            let f = Fmt {
                italic: true,
                ..fmt.clone()
            };
            value.iter().for_each(|i| push_inline(out, i, &f, cx));
        }
        Inline::Strike { value } => {
            let f = Fmt {
                strike: true,
                ..fmt.clone()
            };
            value.iter().for_each(|i| push_inline(out, i, &f, cx));
        }
        Inline::Spoiler { value } => {
            let idx = cx.next_spoiler();
            let f = if cx.rc.state.is_revealed(cx.mid, idx) {
                Fmt {
                    background: Some(t.surface_sunken),
                    link: Some(RenderMsg::ToggleSpoiler(cx.mid.to_owned(), idx)),
                    ..fmt.clone()
                }
            } else {
                Fmt {
                    color: Some(t.text_secondary),
                    background: Some(t.text_secondary),
                    link: Some(RenderMsg::ToggleSpoiler(cx.mid.to_owned(), idx)),
                    ..fmt.clone()
                }
            };
            value.iter().for_each(|i| push_inline(out, i, &f, cx));
        }
        Inline::InlineCode { value } => out.push(code_span(&value.value, t, fmt)),
        Inline::InlineKatex { value } => out.push(code_span(value, t, fmt)),
        Inline::Link { value } => {
            let src = &value.src.value;
            let label = value.label.as_slice();
            // `[ ](permalink)` of a quote: the quote attachment shows it.
            let quoted = cx
                .msg
                .and_then(|m| m.attachments.as_ref())
                .is_some_and(|list| {
                    list.iter()
                        .any(|a| a.message_link.as_deref() == Some(src.as_str()))
                });
            let blank = label.iter().all(|i| {
                let mut s = String::new();
                i.plain_text(&mut s);
                s.trim().is_empty()
            });
            if quoted && blank {
                return;
            }
            let target = safe_url(src);
            let f = Fmt {
                link: target.clone().map(RenderMsg::OpenUrl),
                color: target.is_some().then_some(t.link).or(fmt.color),
                ..fmt.clone()
            };
            let before = out.len();
            for i in label {
                push_inline(out, i, &f, cx);
            }
            let empty = out[before..].iter().all(|s| s.text.trim().is_empty());
            if empty {
                out.truncate(before);
                push_text(out, src, &f);
            }
        }
        Inline::Image { value } => {
            let src = &value.src.value;
            let target = safe_url(src);
            let f = Fmt {
                link: target.clone().map(RenderMsg::OpenUrl),
                color: target.is_some().then_some(t.link).or(fmt.color),
                ..fmt.clone()
            };
            let mut label = String::new();
            value
                .label
                .as_slice()
                .iter()
                .for_each(|i| i.plain_text(&mut label));
            let shown = if label.trim().is_empty() {
                src.clone()
            } else {
                label
            };
            push_text(out, &format!("🖼 {shown}"), &f);
        }
        Inline::MentionUser { value } => out.push(mention_user(&value.value, cx, fmt)),
        Inline::MentionChannel { value } => {
            let name = value.value.as_str();
            let target = cx
                .msg
                .and_then(|m| m.channels.as_ref())
                .and_then(|c| c.iter().find(|c| c.name.as_deref() == Some(name)))
                .map_or_else(|| name.to_owned(), |c| c.id.clone());
            out.push(
                span(format!("#{name}"))
                    .font(Font {
                        weight: Weight::Semibold,
                        ..fmt.font()
                    })
                    .color(t.link)
                    .link(RenderMsg::OpenRoom(target)),
            );
        }
        Inline::Emoji(e) => {
            let code = e
                .short_code
                .clone()
                .or_else(|| e.value.as_ref().map(|v| v.value.clone()));
            match (&e.unicode, code) {
                (Some(u), _) if !u.is_empty() => out.push(styled(u.clone(), fmt, true)),
                (_, Some(code)) => match emoji_piece(&code, cx.rc) {
                    EmojiPiece::Unicode(u) => out.push(styled(u, fmt, true)),
                    EmojiPiece::Image(_, name) | EmojiPiece::Text(name) => {
                        out.push(styled(name, fmt, false));
                    }
                },
                _ => {}
            }
        }
        Inline::Color { value } => {
            let c = Color::from_rgba8(
                value.r,
                value.g,
                value.b,
                if value.a > 0.0 { value.a as f32 } else { 1.0 },
            );
            out.push(span("■ ".to_owned()).color(c));
            push_text(
                out,
                &format!("#{:02x}{:02x}{:02x}", value.r, value.g, value.b),
                fmt,
            );
        }
        Inline::Timestamp { value, .. } => {
            let label = format_timestamp(value, cx.rc.clock_24h);
            out.push(
                span(label)
                    .font(FONT_SEMIBOLD)
                    .background(t.surface_sunken)
                    .border(border::rounded(RADIUS_SM))
                    .padding([0, 3]),
            );
        }
        Inline::Unknown(v) => {
            let mut s = String::new();
            unknown_text(v, &mut s, 0);
            push_text(out, &s, fmt);
        }
    }
}

fn code_span(code: &str, t: &Tokens, fmt: &Fmt) -> Sp {
    let mut s = span(code.to_owned())
        .font(MONOSPACE_FONT)
        .color(t.code_fg)
        .background(t.code_bg)
        .border(border::rounded(RADIUS_SM))
        .padding([0, 3])
        .strikethrough(fmt.strike)
        .link_maybe(fmt.link.clone());
    if let Some(size) = fmt.size {
        s = s.size(size);
    }
    s
}

fn mention_user(name: &str, cx: &Cx<'_, '_>, fmt: &Fmt) -> Sp {
    let t = cx.t;
    let special = name == "all" || name == "here";
    let me = special || cx.me == Some(name);
    let shown = if cx.rc.use_real_names && !special {
        cx.msg
            .and_then(|m| m.mentions.as_ref())
            .and_then(|l| l.iter().find(|m| m.username.as_deref() == Some(name)))
            .and_then(|m| m.name.clone())
            .filter(|n| !n.trim().is_empty())
            .unwrap_or_else(|| name.to_owned())
    } else {
        name.to_owned()
    };
    let (bg, fg) = if me {
        (t.mention_me_bg, t.mention_me_fg)
    } else {
        (t.mention_bg, t.mention_fg)
    };
    let mut s = span(format!("@{}", rustket_ui::widget::sanitize_bidi(&shown)))
        .font(Font {
            weight: Weight::Semibold,
            ..fmt.font()
        })
        .color(fg)
        .background(bg)
        .border(border::rounded(RADIUS_SM))
        .padding([0, 2]);
    if !special {
        s = s.link(RenderMsg::OpenUser(name.to_owned()));
    }
    s
}

/// How to show an emoji code (`smile`, `:smile:`, custom names).
#[derive(Debug, Clone)]
pub enum EmojiPiece {
    Unicode(String),
    /// Custom emoji image (loaded) and its `:name:` fallback text.
    Image(image::Handle, String),
    /// Unknown or not yet loaded: `:name:`.
    Text(String),
}

pub fn emoji_piece(code: &str, rc: &RenderCtx<'_>) -> EmojiPiece {
    let code = code.trim().trim_matches(':');
    let fallback = format!(":{code}:");
    match rc.workspace.emoji().resolve(code) {
        Some(ResolvedEmoji::Unicode(u)) => EmojiPiece::Unicode(u.to_owned()),
        Some(custom @ ResolvedEmoji::Custom { .. }) => {
            let url = custom.custom_url(rc.server).unwrap_or_default();
            match rc.images.get(rc.server_id, &url) {
                Some(l) => EmojiPiece::Image(l.handle.clone(), fallback),
                None => EmojiPiece::Text(fallback),
            }
        }
        None => match rustket_core::emoji::to_unicode(code) {
            Some(u) => EmojiPiece::Unicode(u.to_owned()),
            None => EmojiPiece::Text(fallback),
        },
    }
}

/// An emoji piece as a widget of `size` px.
pub fn emoji_element<M: 'static>(piece: EmojiPiece, size: f32) -> Element<'static, M> {
    match piece {
        EmojiPiece::Unicode(u) => rustket_ui::widget::emoji_text(u, size).into(),
        EmojiPiece::Image(h, name) => tooltip(
            image(h)
                .width(size)
                .height(size)
                .content_fit(iced::ContentFit::Contain),
            name,
            TooltipPosition::Top,
        ),
        EmojiPiece::Text(name) => text(name).size(size * 0.6).into(),
    }
}

fn big_emoji(list: &[Inline], cx: &Cx<'_, '_>) -> El {
    let mut r: Row<'static, RenderMsg> = row![].spacing(SPACE_1).align_y(Alignment::Center);
    for i in list {
        match i {
            Inline::Emoji(e) => {
                let piece = match (&e.unicode, &e.short_code, &e.value) {
                    (Some(u), _, _) if !u.is_empty() => EmojiPiece::Unicode(u.clone()),
                    (_, Some(c), _) => emoji_piece(c, cx.rc),
                    (_, _, Some(v)) => emoji_piece(&v.value, cx.rc),
                    _ => continue,
                };
                r = r.push(emoji_element(piece, TEXT_EMOJI_LARGE));
            }
            Inline::PlainText { value } if value.trim().is_empty() => {}
            other => {
                let mut spans = Vec::new();
                push_inline(&mut spans, other, &Fmt::default(), cx);
                r = r.push(
                    rich_text(spans)
                        .on_link_click(|m: RenderMsg| m)
                        .size(TEXT_EMOJI_LARGE),
                );
            }
        }
    }
    r.into()
}

fn code_block(language: Option<&str>, lines: &[CodeLine]) -> El {
    let code = lines
        .iter()
        .map(|l| l.value.value.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    let lang = language
        .filter(|l| !l.is_empty() && *l != "none")
        .unwrap_or_default();
    let header = row![
        text(lang.to_owned())
            .size(TEXT_CAPTION)
            .font(MONOSPACE_FONT)
            .style(style::text_hint)
            .width(Length::Fill),
        tooltip(
            button(
                row![
                    icon::sized(Icon::Copy, 12.0),
                    text(t!("room_render.copy").into_owned()).size(TEXT_CAPTION)
                ]
                .spacing(SPACE_1)
                .align_y(Alignment::Center)
            )
            .padding([2, 6])
            .style(style::button_ghost)
            .on_press(RenderMsg::Copy(code.clone())),
            t!("room_render.copy_code").into_owned(),
            TooltipPosition::Left,
        ),
    ]
    .align_y(Alignment::Center);
    let body = scrollable(
        container(
            text(code)
                .font(MONOSPACE_FONT)
                .size(TEXT_SMALL)
                .wrapping(Wrapping::None),
        )
        .padding(Padding::ZERO.bottom(SPACE_2)),
    )
    .direction(Direction::Horizontal(
        Scrollbar::new().width(4).scroller_width(4),
    ))
    .style(style::scrollable)
    .width(Length::Fill);
    container(column![header, body].spacing(SPACE_1))
        .padding([SPACE_2, SPACE_3])
        .width(Length::Fill)
        .style(style::container_code_block)
        .into()
}

fn katex_block(value: &str) -> El {
    container(
        scrollable(
            text(value.to_owned())
                .font(MONOSPACE_FONT)
                .size(TEXT_SMALL)
                .wrapping(Wrapping::None),
        )
        .direction(Direction::Horizontal(
            Scrollbar::new().width(4).scroller_width(4),
        ))
        .width(Length::Fill),
    )
    .padding([SPACE_2, SPACE_3])
    .width(Length::Fill)
    .style(style::container_code_block)
    .into()
}

pub(super) fn quote(inner: El) -> El {
    row![
        container(space().width(3))
            .width(3)
            .height(Length::Fill)
            .style(style::container_quote_bar),
        container(inner)
            .padding([SPACE_1, SPACE_2])
            .width(Length::Fill)
            .style(style::container_quote),
    ]
    .height(Length::Shrink)
    .into()
}

fn hidden_spoiler(mid: &str, idx: usize) -> El {
    button(
        row![
            icon::sized(Icon::EyeOff, 14.0),
            text(t!("room_render.spoiler").into_owned()).size(TEXT_SMALL)
        ]
        .spacing(SPACE_2)
        .align_y(Alignment::Center),
    )
    .padding([SPACE_1, SPACE_2])
    .style(style::button_secondary)
    .on_press(RenderMsg::ToggleSpoiler(mid.to_owned(), idx))
    .into()
}

fn list(items: &[ListItem], cx: &Cx<'_, '_>, ordered: bool) -> El {
    let mut col = column![].spacing(2);
    for (i, item) in items.iter().enumerate() {
        let marker = if ordered {
            format!("{}.", item.number.unwrap_or(i as u64 + 1))
        } else {
            "•".to_owned()
        };
        col = col.push(list_row(marker, inline_spans(&item.value, cx)));
    }
    col.into()
}

fn list_row(marker: String, spans: Vec<Sp>) -> El {
    row![
        container(text(marker).size(TEXT_BODY))
            .width(24)
            .align_x(Alignment::End),
        paragraph(spans, TEXT_BODY)
    ]
    .spacing(SPACE_2)
    .into()
}

fn table<'t>(
    header: &'t [TableCell],
    rows: impl Iterator<Item = &'t [TableCell]>,
    cx: &Cx<'_, '_>,
) -> El {
    let cell = |c: &TableCell, head: bool| -> El {
        let fmt = Fmt {
            bold: head,
            ..Fmt::default()
        };
        let mut spans = Vec::new();
        for i in &c.value {
            push_inline(&mut spans, i, &fmt, cx);
        }
        let align = match c.align.as_deref() {
            Some("center") => Alignment::Center,
            Some("right") => Alignment::End,
            _ => Alignment::Start,
        };
        container(
            rich_text(spans)
                .on_link_click(|m: RenderMsg| m)
                .size(TEXT_SMALL)
                .align_x(align)
                .width(Length::Fill),
        )
        .padding([SPACE_1, SPACE_2])
        .width(Length::FillPortion(1))
        .into()
    };
    let mut col = column![].spacing(0);
    if !header.is_empty() {
        col = col.push(
            container(Row::with_children(header.iter().map(|c| cell(c, true)))).style(
                |theme: &iced::Theme| container::Style {
                    background: Some(rustket_ui::tokens(theme).surface_sunken.into()),
                    ..container::Style::default()
                },
            ),
        );
    }
    for r in rows {
        col = col.push(divider());
        col = col.push(Row::with_children(r.iter().map(|c| cell(c, false))));
    }
    container(col)
        .width(Length::Fill)
        .style(|theme: &iced::Theme| {
            let t = rustket_ui::tokens(theme);
            container::Style {
                border: border::rounded(RADIUS_SM).width(1).color(t.border),
                ..container::Style::default()
            }
        })
        .into()
}

/// Best-effort text of an unknown node: its string `value`s, depth-limited.
pub(super) fn unknown_text(v: &Value, out: &mut String, depth: usize) {
    if depth > 6 || out.len() > 4096 {
        return;
    }
    match v {
        Value::String(s) => out.push_str(s),
        Value::Array(a) => a.iter().for_each(|c| unknown_text(c, out, depth + 1)),
        Value::Object(o) => {
            if let Some(c) = o.get("value") {
                unknown_text(c, out, depth + 1);
            }
        }
        _ => {}
    }
}

/// `<t:… :f>` timestamps in local time (`t T d D f F R`).
pub fn format_timestamp(v: &TimestampValue, clock_24h: bool) -> String {
    let Some(date) = v.date() else {
        return v.timestamp.clone();
    };
    format_date_with(
        date,
        v.format.as_deref().unwrap_or("f"),
        clock_24h,
        Local::now(),
    )
}

pub(super) fn format_date_with(
    date: RcDate,
    fmt: &str,
    clock_24h: bool,
    now: DateTime<Local>,
) -> String {
    let Some(local) = Local.timestamp_millis_opt(date.timestamp_millis()).single() else {
        return String::new();
    };
    let time = if clock_24h { "%H:%M" } else { "%-I:%M %p" };
    let time_long = if clock_24h {
        "%H:%M:%S"
    } else {
        "%-I:%M:%S %p"
    };
    match fmt {
        "t" => local.format(time).to_string(),
        "T" => local.format(time_long).to_string(),
        "d" => local.format("%d.%m.%Y").to_string(),
        "D" => long_date(&local),
        "F" => format!(
            "{}, {} {}",
            weekday(&local),
            long_date(&local),
            local.format(time)
        ),
        "R" => relative(local, now),
        _ => format!("{} {}", long_date(&local), local.format(time)),
    }
}

fn long_date(d: &DateTime<Local>) -> String {
    use chrono::Datelike;
    let month = t!(format!("room_render.month_{}", d.month0())).into_owned();
    t!(
        "room_render.long_date",
        day = d.day(),
        month = month,
        year = d.year()
    )
    .into_owned()
}

fn weekday(d: &DateTime<Local>) -> String {
    use chrono::Datelike;
    t!(format!(
        "room_render.weekday_{}",
        d.weekday().num_days_from_monday()
    ))
    .into_owned()
}

fn relative(d: DateTime<Local>, now: DateTime<Local>) -> String {
    let secs = (d - now).num_seconds();
    let (n, unit) = match secs.unsigned_abs() {
        s if s < 60 => return t!("room_render.rel_now").into_owned(),
        s if s < 3600 => (s / 60, "minutes"),
        s if s < 86_400 => (s / 3600, "hours"),
        s => (s / 86_400, "days"),
    };
    if secs > 0 {
        t!(format!("room_render.rel_in_{unit}"), n = n).into_owned()
    } else {
        t!(format!("room_render.rel_ago_{unit}"), n = n).into_owned()
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::images::Images;
    use crate::room::render::RenderState;
    use chrono::Duration;
    use rocketchat::ServerUrl;
    use rocketchat::models::md::Root;
    use rustket_core::{ServerId, Workspace};

    struct Fx {
        server_id: ServerId,
        server: ServerUrl,
        ws: Workspace,
        images: Images,
        state: RenderState,
    }

    impl Fx {
        fn new() -> Self {
            Self {
                server_id: ServerId("s".into()),
                server: ServerUrl::parse("https://chat.example.com").unwrap(),
                ws: Workspace::new(),
                images: Images::new(None),
                state: RenderState::new(),
            }
        }

        fn rc(&self) -> RenderCtx<'_> {
            RenderCtx {
                server_id: &self.server_id,
                server: &self.server,
                workspace: &self.ws,
                images: &self.images,
                state: &self.state,
                clock_24h: true,
                use_real_names: false,
            }
        }
    }

    fn root(json: serde_json::Value) -> Root {
        serde_json::from_value(json).unwrap()
    }

    fn para(json: serde_json::Value) -> Vec<Inline> {
        match root(serde_json::json!([{"type": "PARAGRAPH", "value": json}])).remove(0) {
            Block::Paragraph { value } => value,
            _ => unreachable!(),
        }
    }

    fn plain(v: &str) -> serde_json::Value {
        serde_json::json!({"type": "PLAIN_TEXT", "value": v})
    }

    #[test]
    fn inline_formatting_becomes_spans() {
        let fx = Fx::new();
        let rc = fx.rc();
        let cx = Cx::new(&rc, "m1", None);
        let value = para(serde_json::json!([
            plain("a "),
            {"type": "BOLD", "value": [plain("b"), {"type": "ITALIC", "value": [plain("bi")]}]},
            {"type": "STRIKE", "value": [plain("s")]},
            {"type": "INLINE_CODE", "value": plain("x()")},
            {"type": "LINK", "value": {"src": plain("https://rust-lang.org"), "label": [plain("Rust")]}},
            {"type": "LINK", "value": {"src": plain("javascript:alert(1)"), "label": [plain("evil")]}},
            {"type": "EMOJI", "value": plain("smile"), "shortCode": "smile"},
            {"type": "INLINE_KATEX", "value": "x^2"},
        ]));
        let spans = inline_spans(&value, &cx);
        let texts: Vec<&str> = spans.iter().map(|s| s.text.as_ref()).collect();
        assert_eq!(
            texts,
            ["a ", "b", "bi", "s", "x()", "Rust", "evil", "😄", "x^2"]
        );
        assert_eq!(spans[1].font.unwrap().weight, Weight::Bold);
        assert_eq!(spans[2].font.unwrap().style, FontStyle::Italic);
        assert_eq!(spans[2].font.unwrap().weight, Weight::Bold);
        assert!(spans[3].strikethrough);
        assert_eq!(spans[4].font, Some(MONOSPACE_FONT));
        assert!(spans[4].highlight.is_some(), "inline code has a background");
        assert_eq!(
            spans[5].link,
            Some(RenderMsg::OpenUrl("https://rust-lang.org".into()))
        );
        assert!(spans[5].underline);
        assert_eq!(spans[6].link, None, "unsafe schemes are not clickable");
        assert_eq!(spans[7].font, Some(EMOJI_FONT));
        assert_eq!(spans[8].font, Some(MONOSPACE_FONT));
    }

    #[test]
    fn mentions_and_spoilers() {
        let mut fx = Fx::new();
        let rc = fx.rc();
        let cx = Cx::new(&rc, "m1", None);
        let value = para(serde_json::json!([
            {"type": "MENTION_USER", "value": plain("bob")},
            {"type": "MENTION_USER", "value": plain("all")},
            {"type": "MENTION_CHANNEL", "value": plain("general")},
            {"type": "SPOILER", "value": [plain("secret")]},
            {"type": "SPOILER", "value": [plain("second")]},
        ]));
        let spans = inline_spans(&value, &cx);
        assert_eq!(spans[0].text.as_ref(), "@bob");
        assert_eq!(spans[0].link, Some(RenderMsg::OpenUser("bob".into())));
        assert_eq!(spans[1].text.as_ref(), "@all");
        assert_eq!(spans[1].link, None, "group mentions are not links");
        assert_eq!(spans[2].text.as_ref(), "#general");
        assert_eq!(spans[2].link, Some(RenderMsg::OpenRoom("general".into())));
        assert_eq!(
            spans[3].link,
            Some(RenderMsg::ToggleSpoiler("m1".into(), 0))
        );
        assert_eq!(
            spans[4].link,
            Some(RenderMsg::ToggleSpoiler("m1".into(), 1))
        );
        // Hidden: text drawn in its background colour.
        assert_eq!(spans[3].color, Some(tokens().text_secondary));
        fx.state.handle(&RenderMsg::ToggleSpoiler("m1".into(), 0));
        let rc = fx.rc();
        let cx = Cx::new(&rc, "m1", None);
        let spans = inline_spans(&value, &cx);
        assert_eq!(
            spans[3].color, None,
            "revealed spoiler uses the text colour"
        );
        assert_eq!(spans[4].color, Some(tokens().text_secondary));
    }

    #[test]
    fn plain_fallback_linkifies() {
        let fx = Fx::new();
        let rc = fx.rc();
        let cx = Cx::new(&rc, "m1", None);
        let mut spans = Vec::new();
        push_linkified(
            &mut spans,
            "see www.example.org, or 👍",
            &Fmt::default(),
            &cx,
        );
        let texts: Vec<&str> = spans.iter().map(|s| s.text.as_ref()).collect();
        assert_eq!(texts, ["see ", "www.example.org", ", or ", "👍"]);
        assert_eq!(
            spans[1].link,
            Some(RenderMsg::OpenUrl("https://www.example.org".into()))
        );
        assert_eq!(spans[3].font, Some(EMOJI_FONT));
    }

    /// Every block kind (and unknown nodes) renders and lays out.
    #[test]
    fn every_block_renders() {
        use iced::advanced::renderer::Headless;
        use iced_runtime::user_interface::{Cache, UserInterface};
        let fx = Fx::new();
        let rc = fx.rc();
        let md = root(serde_json::json!([
            {"type": "PARAGRAPH", "value": [plain("p")]},
            {"type": "HEADING", "level": 2, "value": [plain("h")]},
            {"type": "CODE", "language": "rust", "value": [{"type": "CODE_LINE", "value": plain("fn main() {}")}]},
            {"type": "QUOTE", "value": [{"type": "PARAGRAPH", "value": [plain("q")]}]},
            {"type": "UNORDERED_LIST", "value": [{"type": "LIST_ITEM", "value": [plain("u")]}]},
            {"type": "ORDERED_LIST", "value": [{"type": "LIST_ITEM", "number": 3, "value": [plain("o")]}]},
            {"type": "TASKS", "value": [{"type": "TASK", "status": true, "value": [plain("t")]}]},
            {"type": "SPOILER_BLOCK", "value": [{"type": "PARAGRAPH", "value": [plain("s")]}]},
            {"type": "BIG_EMOJI", "value": [{"type": "EMOJI", "value": null, "unicode": "🎉"}]},
            {"type": "LINE_BREAK", "value": null},
            {"type": "HORIZONTAL_RULE"},
            {"type": "KATEX", "value": "e^{i\\pi}"},
            {"type": "TABLE", "value": {"header": [{"type": "TABLE_CELL", "value": [plain("H")]}],
                "rows": [{"type": "TABLE_ROW", "value": [{"type": "TABLE_CELL", "value": [plain("c")]}]}]}},
            {"type": "HOLOGRAM", "value": [plain("fallback")]},
        ]));
        assert_eq!(md.len(), 14);
        let msg = Message {
            id: "m1".into(),
            ..Message::default()
        };
        let el = blocks(&md, "m1", &msg, &rc);
        let mut renderer = iced::Renderer::Secondary(iced_tiny_skia::Renderer::new(
            iced::Font::DEFAULT,
            iced::Pixels(14.0),
        ));
        let ui = UserInterface::build(
            el,
            iced::Size::new(600.0, 2000.0),
            Cache::default(),
            &mut renderer,
        );
        drop(ui);
        let _ = renderer.screenshot(iced::Size::new(1, 1), 1.0, iced::Color::WHITE);
    }

    #[test]
    fn timestamps() {
        rust_i18n::set_locale("en");
        let now = Local.with_ymd_and_hms(2026, 3, 12, 14, 5, 9).unwrap();
        let d = RcDate::from_millis(now.timestamp_millis()).unwrap();
        assert_eq!(format_date_with(d, "t", true, now), "14:05");
        assert_eq!(format_date_with(d, "t", false, now), "2:05 PM");
        assert_eq!(format_date_with(d, "T", true, now), "14:05:09");
        assert_eq!(format_date_with(d, "d", true, now), "12.03.2026");
        assert_eq!(format_date_with(d, "D", true, now), "12 March 2026");
        assert_eq!(format_date_with(d, "f", true, now), "12 March 2026 14:05");
        assert_eq!(
            format_date_with(d, "F", true, now),
            "Thursday, 12 March 2026 14:05"
        );
        let later = RcDate::from_millis((now + Duration::hours(3)).timestamp_millis()).unwrap();
        assert_eq!(format_date_with(later, "R", true, now), "in 3 hours");
        let before = RcDate::from_millis((now - Duration::minutes(5)).timestamp_millis()).unwrap();
        assert_eq!(format_date_with(before, "R", true, now), "5 minutes ago");
    }

    #[test]
    fn unknown_text_extracts_values() {
        let v = serde_json::json!({"type": "NEW", "value": [{"type": "PLAIN_TEXT", "value": "a"}, {"value": "b"}, 3]});
        let mut s = String::new();
        unknown_text(&v, &mut s, 0);
        assert_eq!(s, "ab");
    }
}
