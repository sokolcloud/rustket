//! `md` AST (`@rocket.chat/message-parser`) → iced widgets. Paragraph-level
//! content becomes spans of selectable text blocks
//! (`rustket_ui::widget::selectable::rich`, a drop-in for `rich_text`); blocks
//! become containers. The message body around them is a selection scope
//! (`super::message_body_with`).

use std::cell::Cell;

use chrono::{DateTime, Local, TimeZone};
use iced::font::{Style as FontStyle, Weight};
use iced::widget::scrollable::{Direction, Scrollbar};
use iced::widget::text::{Span, Wrapping};
use iced::widget::{
    Column, Row, button, column, container, image, row, scrollable, space, span, text,
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
use rustket_ui::widget::selectable::rich;
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
    /// A dropped quote link left whitespace behind: trim the next text.
    trim_next: Cell<bool>,
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
            trim_next: Cell::new(false),
        }
    }

    /// Spoiler indices start at `base` (attachments use their own range so
    /// they never share an index with the message text).
    pub fn with_spoiler_base(self, base: usize) -> Self {
        self.spoiler.set(base);
        self
    }

    fn spoiler_shown(&self, idx: usize) -> bool {
        !self.rc.interactive || self.rc.state.is_revealed(self.mid, idx)
    }

    fn spoiler_toggle(&self, idx: usize) -> Option<RenderMsg> {
        self.rc
            .interactive
            .then(|| RenderMsg::ToggleSpoiler(self.mid.to_owned(), idx))
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

/// One paragraph of inline nodes at `size`, as wide as its text (UIKit
/// context items, which share a row).
pub(super) fn inline_paragraph(value: &[Inline], cx: &Cx<'_, '_>, size: f32) -> El {
    rich(inline_spans(value, cx))
        .on_link_click(|m: RenderMsg| m)
        .size(size)
        .into()
}

/// Plain text with clickable URLs (messages without `md`).
pub(super) fn plain_paragraph(text: &str, mid: &str, rc: &RenderCtx<'_>) -> El {
    let cx = Cx::new(rc, mid, None);
    let mut spans = Vec::new();
    push_linkified(&mut spans, text, &Fmt::default(), &cx);
    paragraph(spans, TEXT_BODY)
}

/// Markdown text without an `md` AST (attachment text, pretext, fields,
/// upload descriptions), rendered with [`mrkdwn_spans`].
pub(super) fn mrkdwn_paragraph(text: &str, cx: &Cx<'_, '_>, size: f32, bold: bool) -> El {
    let mut spans = Vec::new();
    let fmt = Fmt {
        bold,
        ..Fmt::default()
    };
    mrkdwn_spans(&mut spans, text, &fmt, cx, 0);
    paragraph(spans, size)
}

/// Longest text parsed for markup; longer texts are shown plain (linkified).
const MRKDWN_MAX_BYTES: usize = 64 * 1024;

/// Emphasis delimiters, longest first.
const DELIMS: [&str; 6] = ["**", "__", "~~", "*", "_", "~"];

/// The first of the sorted positions `v` at or after `from`.
fn first_from(v: &[usize], from: usize) -> Option<usize> {
    v.get(v.partition_point(|p| *p < from)).copied()
}

/// Whether one of the sorted positions `v` lies in `a..b`.
fn any_in(v: &[usize], a: usize, b: usize) -> bool {
    first_from(v, a).is_some_and(|p| p < b)
}

/// Positions of everything [`mrkdwn_spans`] looks for, computed once per
/// text so each candidate is checked in logarithmic time: the whole parse
/// stays linear whatever the input (hostile attachment texts must not
/// freeze the UI thread).
struct Marks {
    newlines: Vec<usize>,
    spaces: Vec<usize>,
    ticks: Vec<usize>,
    link_mids: Vec<usize>,
    parens: Vec<usize>,
    lts: Vec<usize>,
    gts: Vec<usize>,
    /// Per [`DELIMS`] entry: where it can close a span (no space right
    /// before it; for `_` no word character right after it; a single
    /// delimiter not doubled).
    closers: [Vec<usize>; 6],
}

impl Marks {
    fn new(text: &str) -> Self {
        let at = |pat: &str| text.match_indices(pat).map(|(i, _)| i).collect::<Vec<_>>();
        let closers = DELIMS.map(|d| {
            text.match_indices(d)
                .map(|(p, _)| p)
                .filter(|&p| {
                    let before_ok = text[..p]
                        .chars()
                        .next_back()
                        .is_some_and(|c| !c.is_whitespace());
                    let after = &text[p + d.len()..];
                    let after_ok = !d.starts_with('_') || !after.starts_with(char::is_alphanumeric);
                    let doubled = d.len() == 1 && after.starts_with(d);
                    before_ok && after_ok && !doubled
                })
                .collect()
        });
        Self {
            newlines: at("\n"),
            spaces: text
                .char_indices()
                .filter(|(_, c)| c.is_whitespace())
                .map(|(i, _)| i)
                .collect(),
            ticks: at("`"),
            link_mids: at("]("),
            parens: at(")"),
            lts: at("<"),
            gts: at(">"),
            closers,
        }
    }

    /// Byte index of the closing `DELIMS[d]` for an opening one ending at
    /// `from`: non-empty content on one line that does not start or end
    /// with a space, and (for `_`) no word character right after it.
    fn closing(&self, text: &str, from: usize, d: usize) -> Option<usize> {
        let rest = &text[from..];
        if rest.starts_with(char::is_whitespace) || rest.starts_with(DELIMS[d]) {
            return None;
        }
        let end = first_from(&self.closers[d], from + 1)?;
        (!any_in(&self.newlines, from, end)).then_some(end)
    }
}

/// A small markdown subset like the web's `MarkdownText` for attachments:
/// `**b**`/`*b*`, `__i__`/`_i_`, `~~s~~`/`~s~`, `` `code` ``,
/// `[label](url)`, `<url|label>`, bare URLs and line breaks.
fn mrkdwn_spans(out: &mut Vec<Sp>, text: &str, fmt: &Fmt, cx: &Cx<'_, '_>, depth: usize) {
    if depth > 4 || text.len() > MRKDWN_MAX_BYTES {
        push_linkified(out, text, fmt, cx);
        return;
    }
    let marks = Marks::new(text);
    let mut plain = 0;
    let mut i = 0;
    while i < text.len() {
        let rest = &text[i..];
        let prev_word = text[..i]
            .chars()
            .next_back()
            .is_some_and(char::is_alphanumeric);
        type Emit<'e> = Box<dyn FnOnce(&mut Vec<Sp>) + 'e>;
        let mut matched: Option<(usize, Emit<'_>)> = None;
        if rest.starts_with('`') {
            if let Some(end) = first_from(&marks.ticks, i + 1).filter(|e| *e > i + 1) {
                let code = text[i + 1..end].to_owned();
                let f = fmt.clone();
                let t = cx.t;
                matched = Some((
                    end + 1 - i,
                    Box::new(move |o| o.push(code_span(&code, t, &f))),
                ));
            }
        } else if rest.starts_with('[') {
            if let Some(mid) = first_from(&marks.link_mids, i)
                && let Some(close) = first_from(&marks.parens, mid + 2)
                && !any_in(&marks.newlines, i + 1, mid)
                && !any_in(&marks.spaces, mid + 2, close)
            {
                let label = &text[i + 1..mid];
                let src = &text[mid + 2..close];
                let target = safe_url(src);
                let f = Fmt {
                    link: target.clone().map(RenderMsg::OpenUrl),
                    color: target.is_some().then_some(cx.t.link).or(fmt.color),
                    ..fmt.clone()
                };
                let (label, src) = (label.to_owned(), src.to_owned());
                matched = Some((
                    close + 1 - i,
                    Box::new(move |o| {
                        if label.trim().is_empty() {
                            push_text(o, &src, &f);
                        } else {
                            mrkdwn_spans(o, &label, &f, cx, depth + 1);
                            push_link_host(o, &label, &src, &f, cx);
                        }
                    }),
                ));
            }
        } else if rest.starts_with('<') {
            // A target never contains `<`: each `>` closes at most one
            // candidate, which keeps the scan linear.
            if let Some(close) = first_from(&marks.gts, i)
                && !any_in(&marks.lts, i + 1, close)
            {
                let inner = &text[i + 1..close];
                let (src, label) = inner.split_once('|').unwrap_or((inner, inner));
                if let Some(target) = safe_url(src).filter(|_| !src.contains(char::is_whitespace)) {
                    let f = Fmt {
                        link: Some(RenderMsg::OpenUrl(target)),
                        color: Some(cx.t.link),
                        ..fmt.clone()
                    };
                    let (label, src) = (label.to_owned(), src.to_owned());
                    matched = Some((
                        close + 1 - i,
                        Box::new(move |o| {
                            push_text(o, &label, &f);
                            push_link_host(o, &label, &src, &f, cx);
                        }),
                    ));
                }
            }
        } else if !prev_word {
            for (d, delim) in DELIMS.into_iter().enumerate() {
                if !rest.starts_with(delim) {
                    continue;
                }
                if let Some(end) = marks.closing(text, i + delim.len(), d) {
                    let inner = text[i + delim.len()..end].to_owned();
                    let f = match delim.as_bytes()[0] {
                        b'*' => Fmt {
                            bold: true,
                            ..fmt.clone()
                        },
                        b'_' => Fmt {
                            italic: true,
                            ..fmt.clone()
                        },
                        _ => Fmt {
                            strike: true,
                            ..fmt.clone()
                        },
                    };
                    matched = Some((
                        end + delim.len() - i,
                        Box::new(move |o| mrkdwn_spans(o, &inner, &f, cx, depth + 1)),
                    ));
                }
                break;
            }
        }
        match matched {
            Some((len, emit)) => {
                if plain < i {
                    push_linkified(out, &text[plain..i], fmt, cx);
                }
                emit(out);
                i += len;
                plain = i;
            }
            None => i += rest.chars().next().map_or(1, char::len_utf8),
        }
    }
    if plain < text.len() {
        push_linkified(out, &text[plain..], fmt, cx);
    }
}

/// After a link whose label looks like a URL of another host, shows the
/// real target host so it is visible before clicking.
fn push_link_host(out: &mut Vec<Sp>, label: &str, src: &str, fmt: &Fmt, cx: &Cx<'_, '_>) {
    let host = |s: &str| {
        let s = s.trim();
        let with_scheme = if s.contains("://") {
            s.to_owned()
        } else {
            format!("https://{s}")
        };
        url::Url::parse(&with_scheme).ok().and_then(|u| {
            u.host_str()
                .map(|h| h.trim_start_matches("www.").to_ascii_lowercase())
        })
    };
    let label = label.trim();
    let looks_like_url = !label.contains(char::is_whitespace)
        && label.contains('.')
        && find_links(label).first() == Some(&(0, label.len()));
    if !looks_like_url {
        return;
    }
    if let (Some(shown), Some(real)) = (host(label), host(src))
        && shown != real
    {
        out.push(
            span(format!(" ({real})"))
                .font(fmt.font())
                .color(cx.t.text_hint),
        );
    }
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
        Block::Paragraph { value } => {
            cx.trim_next.set(false);
            paragraph(inline_spans(value, cx), TEXT_BODY)
        }
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
            if cx.spoiler_shown(idx) {
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
    rich(spans)
        .on_link_click(|m: RenderMsg| m)
        .size(size)
        .width(Length::Fill)
        .into()
}

/// Unstyled selectable text (code and formula blocks): one line per source
/// line, scrolled horizontally instead of wrapped.
fn code_text(code: String) -> El {
    rich::<RenderMsg, RenderMsg>(vec![span(code)])
        .font(MONOSPACE_FONT)
        .size(TEXT_SMALL)
        .wrapping(Wrapping::None)
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
    if text.is_empty() {
        return;
    }
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
        Inline::PlainText { value } => {
            if cx.trim_next.replace(false) {
                push_text(out, value.trim_start(), fmt);
            } else {
                push_text(out, value, fmt);
            }
        }
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
            let f = if cx.spoiler_shown(idx) {
                Fmt {
                    background: Some(t.surface_sunken),
                    link: cx.spoiler_toggle(idx),
                    ..fmt.clone()
                }
            } else {
                // Dimmer in dark mode, where a light block would glare.
                let cover = if t.mode == rustket_ui::theme::Mode::Dark {
                    t.border_strong
                } else {
                    t.text_secondary
                };
                Fmt {
                    color: Some(cover),
                    background: Some(cover),
                    link: cx.spoiler_toggle(idx),
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
                cx.trim_next.set(true);
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
            } else {
                let shown: String = out[before..].iter().map(|s| s.text.as_ref()).collect();
                push_link_host(out, &shown, src, &f, cx);
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
            out.push(
                span(format!("#{name}"))
                    .font(Font {
                        weight: Weight::Semibold,
                        ..fmt.font()
                    })
                    .color(t.link)
                    .link(RenderMsg::OpenRoom(name.to_owned())),
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
    // Copied on one line: only the first block starts a new line.
    let mut joiner = "\n";
    for i in list {
        match i {
            Inline::Emoji(e) => {
                let piece = match (&e.unicode, &e.short_code, &e.value) {
                    (Some(u), _, _) if !u.is_empty() => EmojiPiece::Unicode(u.clone()),
                    (_, Some(c), _) => emoji_piece(c, cx.rc),
                    (_, _, Some(v)) => emoji_piece(&v.value, cx.rc),
                    _ => continue,
                };
                r = r.push(match piece {
                    // Selectable like the rest of the body, on one line.
                    EmojiPiece::Unicode(u) => {
                        rich::<RenderMsg, RenderMsg>(vec![span(u).font(EMOJI_FONT)])
                            .size(TEXT_EMOJI_LARGE)
                            .line_height(iced::widget::text::LineHeight::Relative(1.2))
                            .wrapping(Wrapping::None)
                            .joiner(std::mem::take(&mut joiner))
                            .into()
                    }
                    piece => emoji_element(piece, TEXT_EMOJI_LARGE),
                });
            }
            Inline::PlainText { value } if value.trim().is_empty() => {}
            other => {
                let mut spans = Vec::new();
                push_inline(&mut spans, other, &Fmt::default(), cx);
                r = r.push(
                    rich(spans)
                        .on_link_click(|m: RenderMsg| m)
                        .size(TEXT_EMOJI_LARGE)
                        .joiner(std::mem::take(&mut joiner)),
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
    let body = scrollable(container(code_text(code)).padding(Padding::ZERO.bottom(SPACE_2)))
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
        scrollable(code_text(value.to_owned()))
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
    let item = rich(spans)
        .on_link_click(|m: RenderMsg| m)
        .size(TEXT_BODY)
        .width(Length::Fill)
        .copy_prefix(format!("{marker} "));
    row![
        container(text(marker).size(TEXT_BODY))
            .width(24)
            .align_x(Alignment::End),
        item
    ]
    .spacing(SPACE_2)
    .into()
}

fn table<'t>(
    header: &'t [TableCell],
    rows: impl Iterator<Item = &'t [TableCell]>,
    cx: &Cx<'_, '_>,
) -> El {
    let cell = |(i, c): (usize, &TableCell), head: bool| -> El {
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
            rich(spans)
                .on_link_click(|m: RenderMsg| m)
                .size(TEXT_SMALL)
                .align_x(align)
                .width(Length::Fill)
                .joiner(if i == 0 { "\n" } else { "\t" }),
        )
        .padding([SPACE_1, SPACE_2])
        .width(Length::FillPortion(1))
        .into()
    };
    let mut col = column![].spacing(0);
    if !header.is_empty() {
        col = col.push(
            container(Row::with_children(
                header.iter().enumerate().map(|c| cell(c, true)),
            ))
            .style(|theme: &iced::Theme| container::Style {
                background: Some(rustket_ui::tokens(theme).surface_sunken.into()),
                ..container::Style::default()
            }),
        );
    }
    for r in rows {
        col = col.push(divider());
        col = col.push(Row::with_children(
            r.iter().enumerate().map(|c| cell(c, false)),
        ));
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
        "d" => crate::format::full_date(&local),
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
    let dir = if secs > 0 { "in" } else { "ago" };
    t!(
        format!("room_render.rel_{dir}_{unit}_{}", super::plural(n)),
        n = n
    )
    .into_owned()
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
                interactive: true,
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
    fn mrkdwn_hostile_input_renders_quickly() {
        let fx = Fx::new();
        let rc = fx.rc();
        let cx = Cx::new(&rc, "m1", None);
        let inputs = [
            "*a ".repeat(20_000),
            " _a".repeat(20_000),
            "~~a ".repeat(15_000),
            "**a ".repeat(15_000),
            "*a _b ~c ".repeat(6_000),
            "[".repeat(60_000) + "](http://a)",
            "[a](".repeat(15_000),
            "<".repeat(60_000) + ">",
            "<http://a".repeat(6_000) + ">",
            "`".repeat(60_000),
            format!("*{}", "a\n".repeat(30_000)),
            format!("[{}](http://a b)", "x".repeat(60_000)),
        ];
        let start = std::time::Instant::now();
        for text in &inputs {
            let mut spans = Vec::new();
            mrkdwn_spans(&mut spans, text, &Fmt::default(), &cx, 0);
        }
        // The old per-opener scans took minutes here.
        assert!(
            start.elapsed() < std::time::Duration::from_secs(2),
            "{:?}",
            start.elapsed()
        );
        // Past the size cap the text is still shown, unformatted.
        let big = "*b* ".repeat(20_000);
        let mut spans = Vec::new();
        mrkdwn_spans(&mut spans, &big, &Fmt::default(), &cx, 0);
        let shown: String = spans.iter().map(|s| s.text.as_ref()).collect();
        assert_eq!(shown, big);
    }

    #[test]
    fn mrkdwn_subset() {
        let fx = Fx::new();
        let rc = fx.rc();
        let cx = Cx::new(&rc, "m1", None);
        let mut spans = Vec::new();
        mrkdwn_spans(
            &mut spans,
            "All **checks** _ok_ ~no~ `make` [log](https://ci.example/1) <https://x.example/mr|MR> snake_case 2*3*4",
            &Fmt::default(),
            &cx,
            0,
        );
        let texts: Vec<&str> = spans.iter().map(|s| s.text.as_ref()).collect();
        assert_eq!(
            texts,
            [
                "All ",
                "checks",
                " ",
                "ok",
                " ",
                "no",
                " ",
                "make",
                " ",
                "log",
                " ",
                "MR",
                " snake_case 2*3*4"
            ]
        );
        assert_eq!(spans[1].font.unwrap().weight, Weight::Bold);
        assert_eq!(spans[3].font.unwrap().style, FontStyle::Italic);
        assert!(spans[5].strikethrough);
        assert_eq!(spans[7].font, Some(MONOSPACE_FONT));
        assert_eq!(
            spans[9].link,
            Some(RenderMsg::OpenUrl("https://ci.example/1".into()))
        );
        assert_eq!(
            spans[11].link,
            Some(RenderMsg::OpenUrl("https://x.example/mr".into()))
        );
    }

    #[test]
    fn deceptive_link_shows_its_host() {
        let fx = Fx::new();
        let rc = fx.rc();
        let cx = Cx::new(&rc, "m1", None);
        let value = para(serde_json::json!([
            {"type": "LINK", "value": {"src": plain("https://evil.example/x"), "label": [plain("https://bank.example")]}},
            {"type": "LINK", "value": {"src": plain("https://www.rust-lang.org/learn"), "label": [plain("rust-lang.org")]}},
            {"type": "LINK", "value": {"src": plain("https://evil.example"), "label": [plain("Docs")]}},
        ]));
        let texts: Vec<String> = inline_spans(&value, &cx)
            .iter()
            .map(|s| s.text.to_string())
            .collect();
        assert_eq!(
            texts,
            [
                "https://bank.example",
                " (evil.example)",
                "rust-lang.org",
                "Docs"
            ]
        );
    }

    #[test]
    fn quote_link_space_is_trimmed() {
        let fx = Fx::new();
        let rc = fx.rc();
        let link = "https://chat.example.com/channel/x?msg=q1";
        let msg: Message = serde_json::from_value(serde_json::json!({
            "_id": "m1", "rid": "r", "msg": "", "u": {"_id": "u"},
            "attachments": [{"message_link": link, "text": "quoted"}]
        }))
        .unwrap();
        let cx = Cx::new(&rc, "m1", Some(&msg));
        let value = para(serde_json::json!([
            {"type": "LINK", "value": {"src": plain(link), "label": [plain(" ")]}},
            plain(" Agreed"),
        ]));
        let texts: Vec<String> = inline_spans(&value, &cx)
            .iter()
            .map(|s| s.text.to_string())
            .collect();
        assert_eq!(texts, ["Agreed"]);
    }

    #[test]
    fn stateless_bodies_reveal_spoilers() {
        let fx = Fx::new();
        let rc = RenderCtx {
            interactive: false,
            ..fx.rc()
        };
        let cx = Cx::new(&rc, "m1", None);
        let value = para(serde_json::json!([{"type": "SPOILER", "value": [plain("s")]}]));
        let spans = inline_spans(&value, &cx);
        assert_eq!(spans[0].link, None, "nobody would handle the toggle");
        assert_eq!(spans[0].color, None, "shown in the text colour");
        // Attachment spoilers have their own index range.
        let rc = fx.rc();
        let cx = Cx::new(&rc, "m1", None).with_spoiler_base(100);
        let spans = inline_spans(&value, &cx);
        assert_eq!(
            spans[0].link,
            Some(RenderMsg::ToggleSpoiler("m1".into(), 100))
        );
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

    /// What "select all, copy" gives for `el` (a message body with its scope).
    fn copied(el: Element<'_, RenderMsg>) -> String {
        use iced_runtime::user_interface::{Cache, UserInterface};
        use rustket_ui::widget::selectable::{Copied, SelectAll};
        let mut renderer = iced::Renderer::Secondary(iced_tiny_skia::Renderer::new(
            iced::Font::DEFAULT,
            iced::Pixels(14.0),
        ));
        let mut ui = UserInterface::build(
            el,
            iced::Size::new(600.0, 4000.0),
            Cache::default(),
            &mut renderer,
        );
        ui.operate(&renderer, &mut SelectAll);
        let mut c = Copied::default();
        ui.operate(&renderer, &mut c);
        assert_eq!(c.texts.len(), 1, "one scope per body");
        c.texts.remove(0)
    }

    fn body(md: serde_json::Value, fx: &Fx) -> String {
        rust_i18n::set_locale("en");
        let msg = Message {
            id: "m1".into(),
            md: Some(root(md)),
            ..Message::default()
        };
        copied(crate::room::render::message_body_with(&msg, &fx.rc()))
    }

    /// Copied text of every `md` node: markdown rendered to plain text, emoji
    /// as unicode, mentions as `@name`, links as their label (like a browser
    /// selection in the web client), list markers kept, table cells tabbed,
    /// hidden spoilers left out.
    #[test]
    fn copy_text_of_every_node() {
        let mut fx = Fx::new();
        let inline = body(
            serde_json::json!([{"type": "PARAGRAPH", "value": [
                plain("a "),
                {"type": "BOLD", "value": [plain("b"), {"type": "ITALIC", "value": [plain("bi")]}]},
                plain(" "),
                {"type": "STRIKE", "value": [plain("s")]},
                plain(" "),
                {"type": "INLINE_CODE", "value": plain("x()")},
                plain(" "),
                {"type": "LINK", "value": {"src": plain("https://rust-lang.org"), "label": [plain("Rust")]}},
                plain(" "),
                {"type": "LINK", "value": {"src": plain("https://example.org/x"), "label": []}},
                plain(" "),
                {"type": "MENTION_USER", "value": plain("bob")},
                plain(" "),
                {"type": "MENTION_CHANNEL", "value": plain("general")},
                plain(" "),
                {"type": "EMOJI", "value": plain("smile"), "shortCode": "smile"},
                {"type": "EMOJI", "value": null, "unicode": "🎉"},
                plain(" "),
                // Hidden: neither highlighted nor copied.
                {"type": "SPOILER", "value": [plain("secret")]},
                plain(" "),
                {"type": "INLINE_KATEX", "value": "x^2"},
                plain(" "),
                {"type": "TIMESTAMP", "value": {"timestamp": "1700000000", "format": "d"}},
                plain(" "),
                {"type": "COLOR", "value": {"r": 255, "g": 0, "b": 0, "a": 255}},
                plain(" "),
                {"type": "IMAGE", "value": {"src": plain("https://i.example/p.png"), "label": plain("pic")}},
                plain(" "),
                {"type": "MYSTERY", "value": [plain("unknown")]},
            ]}]),
            &fx,
        );
        let ts = format_timestamp(
            &TimestampValue {
                timestamp: "1700000000".into(),
                format: Some("d".into()),
            },
            true,
        );
        assert_eq!(
            inline,
            format!(
                "a bbi s x() Rust https://example.org/x @bob #general 😄🎉  x^2 {ts} ■ #ff0000 🖼 pic unknown"
            )
        );
        let blocks = body(
            serde_json::json!([
                {"type": "HEADING", "level": 1, "value": [plain("Title")]},
                {"type": "PARAGRAPH", "value": [plain("para")]},
                {"type": "CODE", "language": "rust", "value": [
                    {"type": "CODE_LINE", "value": plain("fn main() {")},
                    {"type": "CODE_LINE", "value": plain("}")}
                ]},
                {"type": "QUOTE", "value": [{"type": "PARAGRAPH", "value": [plain("quoted")]}]},
                {"type": "BLOCKQUOTE", "value": [{"type": "PARAGRAPH", "value": [plain("legacy")]}]},
                {"type": "UNORDERED_LIST", "value": [{"type": "LIST_ITEM", "value": [plain("u1")]}, {"type": "LIST_ITEM", "value": [plain("u2")]}]},
                {"type": "ORDERED_LIST", "value": [{"type": "LIST_ITEM", "number": 3, "value": [plain("o3")]}]},
                {"type": "TASKS", "value": [{"type": "TASK", "status": true, "value": [plain("done")]}]},
                // Hidden behind its button: not copied.
                {"type": "SPOILER_BLOCK", "value": [{"type": "PARAGRAPH", "value": [plain("hidden")]}]},
                {"type": "LINE_BREAK", "value": null},
                {"type": "HORIZONTAL_RULE"},
                {"type": "KATEX", "value": "e^{i\\pi}"},
                {"type": "TABLE", "value": {"header": [
                        {"type": "TABLE_CELL", "value": [plain("H1")]},
                        {"type": "TABLE_CELL", "value": [plain("H2")]}],
                    "rows": [{"type": "TABLE_ROW", "value": [
                        {"type": "TABLE_CELL", "value": [plain("c1")]},
                        {"type": "TABLE_CELL", "value": [plain("c2")]}]}]}},
                {"type": "BIG_EMOJI", "value": [
                    {"type": "EMOJI", "value": null, "unicode": "🎉"},
                    {"type": "EMOJI", "value": null, "unicode": "👍"}]},
                {"type": "HOLOGRAM", "value": [plain("fallback")]},
            ]),
            &fx,
        );
        assert_eq!(
            blocks,
            "Title\npara\nfn main() {\n}\nquoted\nlegacy\n• u1\n• u2\n3. o3\ndone\ne^{i\\pi}\nH1\tH2\nc1\tc2\n🎉👍\nfallback"
        );
        // Revealed, the spoiler is ordinary text.
        fx.state.handle(&RenderMsg::ToggleSpoiler("m1".into(), 0));
        let revealed = body(
            serde_json::json!([{"type": "PARAGRAPH", "value": [
                plain("a "), {"type": "SPOILER", "value": [plain("secret")]}, plain(" b")
            ]}]),
            &fx,
        );
        assert_eq!(revealed, "a secret b");
        // Messages without `md` and attachment text are selectable too.
        let msg: Message = serde_json::from_value(serde_json::json!({
            "_id": "m2", "rid": "r", "msg": "plain www.example.org\nnext line", "u": {"_id": "u"},
            "attachments": [{"text": "*bold* and `code`", "title": "T"}]
        }))
        .unwrap();
        let rc = fx.rc();
        assert_eq!(
            copied(crate::room::render::message_body_with(&msg, &rc)),
            "plain www.example.org\nnext line\nbold and code"
        );
    }

    #[test]
    fn timestamps() {
        rust_i18n::set_locale("en");
        let now = Local.with_ymd_and_hms(2026, 3, 12, 14, 5, 9).unwrap();
        let d = RcDate::from_millis(now.timestamp_millis()).unwrap();
        assert_eq!(format_date_with(d, "t", true, now), "14:05");
        assert_eq!(format_date_with(d, "t", false, now), "2:05 PM");
        assert_eq!(format_date_with(d, "T", true, now), "14:05:09");
        assert_eq!(format_date_with(d, "d", true, now), "Mar 12, 2026");
        assert_eq!(format_date_with(d, "D", true, now), "March 12, 2026");
        assert_eq!(format_date_with(d, "f", true, now), "March 12, 2026 14:05");
        assert_eq!(
            format_date_with(d, "F", true, now),
            "Thursday, March 12, 2026 14:05"
        );
        let later = RcDate::from_millis((now + Duration::hours(3)).timestamp_millis()).unwrap();
        assert_eq!(format_date_with(later, "R", true, now), "in 3 hours");
        let before = RcDate::from_millis((now - Duration::minutes(5)).timestamp_millis()).unwrap();
        assert_eq!(format_date_with(before, "R", true, now), "5 minutes ago");
        let one = RcDate::from_millis((now - Duration::minutes(1)).timestamp_millis()).unwrap();
        assert_eq!(format_date_with(one, "R", true, now), "1 minute ago");
    }

    #[test]
    fn unknown_text_extracts_values() {
        let v = serde_json::json!({"type": "NEW", "value": [{"type": "PLAIN_TEXT", "value": "a"}, {"value": "b"}, 3]});
        let mut s = String::new();
        unknown_text(&v, &mut s, 0);
        assert_eq!(s, "ab");
    }
}
