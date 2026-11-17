//! Rendering of the global search view: the query field, the filter
//! builder (chips with pickers) and the results grouped by room.

use std::ops::Range;
use std::time::Instant;

use chrono::{DateTime, Local};
use iced::widget::{
    button, column, container, lazy, mouse_area, rich_text, row, scrollable, space, span, text,
    text_input,
};
use iced::{Alignment, Element, Length};
use rocketchat::ServerUrl;
use rocketchat::models::{Message as RcMessage, RoomType};
use rust_i18n::t;
use rustket_core::config::UiPrefs;
use rustket_core::room_list::{self, Avatar};
use rustket_core::search::{Backend, Has, Origin, format_date};
use rustket_core::{ServerId, Workspace};
use rustket_ui::icon::{self, Icon};
use rustket_ui::style;
use rustket_ui::theme::{
    SPACE_1, SPACE_2, SPACE_3, SPACE_4, TEXT_BODY, TEXT_CAPTION, TEXT_SMALL, font_semibold,
};
use rustket_ui::widget::{
    AvatarShape, MessageRowProps, Placement, TooltipPosition, avatar, empty_state, header_bar,
    icon_button, icon_button_with, message_row, popover, sanitize_bidi, secondary_button, spinner,
    tooltip,
};

use super::{Msg, PICKER_ID, Picker, Provider, QUERY_ID, SearchState, SearchView};
use crate::images::Images;

/// Characters of a result's snippet.
const SNIPPET_CHARS: usize = 220;

/// What the view reads besides its state.
pub struct Ctx<'a> {
    pub server_id: &'a ServerId,
    pub server: &'a ServerUrl,
    pub workspace: &'a Workspace,
    pub images: &'a Images,
    pub prefs: &'a UiPrefs,
    pub state: &'a SearchState,
    /// The session is connected (server requests run).
    pub online: bool,
    pub now: Instant,
    pub local_now: DateTime<Local>,
}

impl Ctx<'_> {
    fn use_real(&self) -> bool {
        self.workspace.use_real_names(self.prefs.use_real_names)
    }

    fn room_name(&self, rid: &str) -> String {
        self.workspace
            .room(rid)
            .map(|e| room_list::display_name(self.workspace, e, self.use_real()))
            .unwrap_or_default()
    }
}

/// A builder chip: a toggle or a set filter.
fn chip_button<'a>(
    icon_: Icon,
    label: String,
    active: bool,
    on: Option<Msg>,
) -> iced::widget::Button<'a, Msg> {
    button(
        row![
            icon::sized(icon_, 13.0),
            text(label).size(TEXT_SMALL).wrapping(text::Wrapping::None)
        ]
        .spacing(SPACE_1)
        .align_y(Alignment::Center),
    )
    .padding([3.0, SPACE_2])
    .style(style::button_reaction(active))
    .on_press_maybe(on)
}

/// A set filter with its remove button.
fn filter_chip<'a>(icon_: Icon, label: String, remove: Msg) -> Element<'a, Msg> {
    row![
        chip_button(icon_, label, true, None),
        icon_button_with(
            Icon::X,
            Some(remove),
            t!("search.remove_filter"),
            false,
            22.0
        ),
    ]
    .spacing(0)
    .align_y(Alignment::Center)
    .into()
}

/// The text with its matches highlighted.
fn highlighted<'a>(s: &str, ranges: &[Range<usize>]) -> Element<'a, Msg> {
    let t = crate::room::render::tokens();
    let mut spans: Vec<iced::advanced::text::Span<'a, Msg>> = Vec::new();
    let mut at = 0;
    for r in ranges {
        if r.start < at
            || r.end > s.len()
            || !s.is_char_boundary(r.start)
            || !s.is_char_boundary(r.end)
        {
            continue;
        }
        if r.start > at {
            spans.push(span(s[at..r.start].to_owned()));
        }
        spans.push(
            span(s[r.clone()].to_owned())
                .font(font_semibold())
                .color(t.text_primary)
                .background(t.highlight_bg),
        );
        at = r.end;
    }
    if at < s.len() {
        spans.push(span(s[at..].to_owned()));
    }
    rich_text(spans).size(TEXT_BODY).width(Length::Fill).into()
}

impl SearchView {
    pub fn view<'a>(&'a self, c: &Ctx<'a>) -> Element<'a, Msg> {
        let header = header_bar(
            Some(icon::sized(Icon::TextSearch, 18.0).into()),
            t!("search.title").into_owned(),
            None,
            vec![icon_button(Icon::X, Some(Msg::Close), t!("search.close"))],
        );
        let mut field = row![
            icon::sized(Icon::Search, 16.0).style(style::text_secondary),
            text_input(&t!("search.placeholder"), &self.query)
                .id(QUERY_ID)
                .on_input(Msg::Query)
                .on_submit(Msg::Submit)
                .size(TEXT_BODY)
                .padding([8, 10])
                .style(style::text_input),
        ]
        .spacing(SPACE_2)
        .align_y(Alignment::Center);
        if !self.query.is_empty() {
            field = field.push(icon_button_with(
                Icon::X,
                Some(Msg::Clear),
                t!("search.clear"),
                false,
                28.0,
            ));
        }
        let top = column![field, self.builder(c), self.status(c)]
            .spacing(SPACE_2)
            .padding([SPACE_3, SPACE_4]);
        let body = self.results(c);
        container(
            column![header, top, body]
                .width(Length::Fill)
                .height(Length::Fill),
        )
        .style(style::container_room)
        .width(Length::Fill)
        .height(Length::Fill)
        .into()
    }

    /// The filter builder: set filters as removable chips, then the
    /// pickers and toggles.
    fn builder<'a>(&'a self, c: &Ctx<'a>) -> Element<'a, Msg> {
        let q = self.parsed();
        let mut chips: Vec<Element<'a, Msg>> = Vec::new();
        for u in &q.from {
            chips.push(filter_chip(
                Icon::User,
                t!("search.from_chip", user = u.as_str()).into_owned(),
                Msg::RemoveFrom(u.clone()),
            ));
        }
        for r in &q.rooms {
            chips.push(filter_chip(
                Icon::Hash,
                t!("search.in_chip", room = r.as_str()).into_owned(),
                Msg::RemoveRoom(r.clone()),
            ));
        }
        if let Some(d) = q.after {
            chips.push(filter_chip(
                Icon::CalendarArrowUp,
                t!("search.after_chip", date = format_date(d).as_str()).into_owned(),
                Msg::ClearDate(Picker::After),
            ));
        }
        if let Some(d) = q.before {
            chips.push(filter_chip(
                Icon::Calendar,
                t!("search.before_chip", date = format_date(d).as_str()).into_owned(),
                Msg::ClearDate(Picker::Before),
            ));
        }
        chips.push(self.picker_chip(Picker::From, t!("search.from")));
        chips.push(self.picker_chip(Picker::In, t!("search.in")));
        if q.after.is_none() {
            chips.push(self.picker_chip(Picker::After, t!("search.after")));
        }
        if q.before.is_none() {
            chips.push(self.picker_chip(Picker::Before, t!("search.before")));
        }
        for (h, ic, key) in [
            (Has::Link, Icon::Link, "search.has_link"),
            (Has::Star, Icon::Star, "search.has_star"),
            (Has::Pin, Icon::Pin, "search.has_pin"),
        ] {
            chips.push(
                chip_button(ic, t!(key).into_owned(), q.has(h), Some(Msg::ToggleHas(h))).into(),
            );
        }
        let me = c.workspace.username().unwrap_or_default();
        let mentions_me = !me.is_empty() && q.mentions.iter().any(|m| m.eq_ignore_ascii_case(me));
        chips.push(
            chip_button(
                Icon::AtSign,
                t!("search.mentions_me").into_owned(),
                mentions_me,
                Some(Msg::ToggleMentionsMe),
            )
            .into(),
        );
        let help = tooltip(
            icon::sized(Icon::Info, 14.0).style(style::text_hint),
            t!("search.operators_help").into_owned(),
            TooltipPosition::Bottom,
        );
        chips.push(container(help).padding([0.0, SPACE_1]).into());
        row(chips)
            .spacing(SPACE_1)
            .wrap()
            .vertical_spacing(SPACE_1)
            .into()
    }

    /// A picker trigger with its popover.
    fn picker_chip<'a>(&'a self, p: Picker, label: std::borrow::Cow<'_, str>) -> Element<'a, Msg> {
        let open = self.picker == Some(p);
        let trigger = chip_button(
            Icon::Plus,
            label.into_owned(),
            open,
            Some(Msg::Pick(Some(p))),
        );
        let popup: Element<'a, Msg> = if open {
            self.picker_popup(p)
        } else {
            space().into()
        };
        popover(trigger, popup, open, Placement::BelowStart, Msg::Pick(None))
    }

    fn picker_popup<'a>(&'a self, p: Picker) -> Element<'a, Msg> {
        let placeholder = match p {
            Picker::From => t!("search.from_placeholder"),
            Picker::In => t!("search.in_placeholder"),
            Picker::After | Picker::Before => t!("search.date_placeholder"),
        };
        let input = text_input(&placeholder, &self.picker_input)
            .id(PICKER_ID)
            .on_input(Msg::PickerInput)
            .on_submit(Msg::PickerSubmit)
            .size(TEXT_SMALL)
            .padding([6, 8])
            .style(style::text_input);
        let mut col = column![input].spacing(SPACE_1).width(260);
        let item = |label: String, secondary: String, m: Msg| -> Element<'a, Msg> {
            let mut r = row![text(label).size(TEXT_SMALL).font(font_semibold())]
                .spacing(SPACE_2)
                .align_y(Alignment::Center);
            if !secondary.is_empty() {
                r = r.push(
                    text(secondary)
                        .size(TEXT_CAPTION)
                        .style(style::text_secondary)
                        .wrapping(text::Wrapping::None),
                );
            }
            button(r)
                .width(Length::Fill)
                .padding([4.0, SPACE_2])
                .style(style::button_menu_item(false))
                .on_press(m)
                .into()
        };
        match p {
            Picker::From => {
                for (u, name) in &self.suggestions {
                    let shown = sanitize_bidi(name).into_owned();
                    col = col.push(item(format!("@{u}"), shown, Msg::AddFrom(u.clone())));
                }
            }
            Picker::In => {
                for (name, shown) in &self.suggestions {
                    let secondary = if shown == name {
                        String::new()
                    } else {
                        sanitize_bidi(shown).into_owned()
                    };
                    col = col.push(item(
                        format!("#{name}"),
                        secondary,
                        Msg::AddRoom(name.clone()),
                    ));
                }
            }
            Picker::After => {
                for (days, key) in [
                    (7, "search.last_7_days"),
                    (30, "search.last_30_days"),
                    (365, "search.last_year"),
                ] {
                    col = col.push(item(
                        t!(key).into_owned(),
                        String::new(),
                        Msg::AfterDaysAgo(days),
                    ));
                }
                col = col.push(
                    text(t!("search.after_hint"))
                        .size(TEXT_CAPTION)
                        .style(style::text_hint),
                );
            }
            Picker::Before => {
                col = col.push(
                    text(t!("search.before_hint"))
                        .size(TEXT_CAPTION)
                        .style(style::text_hint),
                );
            }
        }
        container(col)
            .padding(SPACE_2)
            .style(style::container_popover)
            .into()
    }

    /// Progress and notes under the builder.
    fn status<'a>(&'a self, c: &Ctx<'a>) -> Element<'a, Msg> {
        let line = |s: String| -> Element<'a, Msg> {
            text(s).size(TEXT_CAPTION).style(style::text_hint).into()
        };
        let mut parts: Vec<Element<'a, Msg>> = Vec::new();
        let Some(run) = &self.run else {
            if matches!(c.state.provider, Provider::Loading { .. }) {
                parts.push(spinner(12.0));
            }
            return row(parts)
                .spacing(SPACE_2)
                .align_y(Alignment::Center)
                .into();
        };
        let (done, total) = run.progress();
        let running = !run.is_finished() || self.cache_pending;
        if running {
            parts.push(spinner(12.0));
        }
        if self.parsed().regex_unsupported() {
            parts.push(line(t!("search.regex_slash").into_owned()));
        } else if run.stopped {
            parts.push(line(t!("search.server_error").into_owned()));
        } else if !c.online && !run.is_finished() {
            parts.push(line(t!("search.offline").into_owned()));
        } else if run.backend == Backend::Global && total > 0 {
            parts.push(line(if run.is_finished() {
                t!("search.server_wide_done").into_owned()
            } else {
                t!("search.server_wide").into_owned()
            }));
        } else if total > 0 {
            parts.push(line(
                t!("search.progress", done = done, total = total).into_owned(),
            ));
        }
        if let Some(until) = c
            .state
            .pacing
            .paused_until(c.now)
            .filter(|_| !run.is_finished())
        {
            let secs = until.saturating_duration_since(c.now).as_secs().max(1);
            parts.push(line(t!("search.paused", seconds = secs).into_owned()));
        }
        if run.encrypted > 0 {
            parts.push(tooltip(
                line(t!("search.encrypted_local", count = run.encrypted).into_owned()),
                t!("search.encrypted_help").into_owned(),
                TooltipPosition::Bottom,
            ));
        }
        if !run.failed.is_empty() && !run.stopped {
            parts.push(line(
                t!("search.failed_rooms", count = run.failed.len()).into_owned(),
            ));
        }
        row(parts)
            .spacing(SPACE_3)
            .align_y(Alignment::Center)
            .wrap()
            .into()
    }

    fn results<'a>(&'a self, c: &Ctx<'a>) -> Element<'a, Msg> {
        let q = self.parsed();
        let centered =
            |e: Element<'a, Msg>| -> Element<'a, Msg> { container(e).center(Length::Fill).into() };
        let Some(run) = &self.run else {
            return centered(empty_state(
                Icon::TextSearch,
                t!("search.hint_title"),
                t!("search.hint_body"),
            ));
        };
        if run.is_empty() {
            if run.is_finished() && !self.cache_pending {
                if run.stopped {
                    // The server never answered: not "nothing found".
                    return centered(
                        column![
                            empty_state(
                                Icon::SearchX,
                                t!("search.server_error"),
                                t!("search.server_error_body"),
                            ),
                            secondary_button(t!("search.retry"), Some(Msg::Submit)),
                        ]
                        .spacing(SPACE_3)
                        .align_x(Alignment::Center)
                        .into(),
                    );
                }
                let body = if q.regex_unsupported() {
                    t!("search.regex_slash")
                } else if !q.rooms.is_empty() && run.progress().1 == 0 && run.encrypted == 0 {
                    t!("search.no_room")
                } else {
                    t!("search.no_results_body")
                };
                return centered(empty_state(Icon::SearchX, t!("search.no_results"), body));
            }
            return centered(spinner(24.0));
        }
        let words: std::rc::Rc<[String]> = q.words().into();
        let groups = self.groups();
        let mut col = column![].spacing(0);
        let mut rows = 0usize;
        let total: usize = groups.iter().map(|g| g.1.len()).sum();
        'groups: for (rid, mids) in groups.iter() {
            // No header without a row under it at the page's end.
            if rows >= self.shown {
                break;
            }
            col = col.push(self.group_header(c, rid, mids.len()));
            for mid in mids {
                if rows >= self.shown {
                    break 'groups;
                }
                let Some(h) = run.hit(mid) else {
                    continue;
                };
                col = col.push(self.hit_row(c, &h.message, h.origin, &words));
                rows += 1;
            }
        }
        let left = total.saturating_sub(self.shown);
        if left > 0 {
            col = col.push(
                container(secondary_button(
                    t!("search.show_more", count = left.min(super::PAGE)),
                    Some(Msg::ShowMore),
                ))
                .center_x(Length::Fill)
                .padding(SPACE_2),
            );
        }
        scrollable(col).height(Length::Fill).into()
    }

    fn group_header<'a>(&'a self, c: &Ctx<'a>, rid: &str, n: usize) -> Element<'a, Msg> {
        let glyph = match c.workspace.room(rid).map(|e| e.room_type()) {
            Some(RoomType::Direct) => Icon::AtSign,
            Some(RoomType::Private) => Icon::Lock,
            _ => Icon::Hash,
        };
        container(
            row![
                icon::sized(glyph, 14.0).style(style::text_secondary),
                text(sanitize_bidi(&c.room_name(rid)).into_owned())
                    .size(TEXT_SMALL)
                    .font(font_semibold()),
                text(n.to_string())
                    .size(TEXT_CAPTION)
                    .style(style::text_hint),
            ]
            .spacing(SPACE_2)
            .align_y(Alignment::Center),
        )
        .padding([SPACE_2, SPACE_4])
        .width(Length::Fill)
        .into()
    }

    fn hit_row<'a>(
        &'a self,
        c: &Ctx<'a>,
        m: &'a RcMessage,
        origin: Origin,
        words: &std::rc::Rc<[String]>,
    ) -> Element<'a, Msg> {
        let words = words.clone();
        let username = m.u.username.clone().unwrap_or_default();
        let name = m
            .alias
            .clone()
            .filter(|a| !a.is_empty())
            .unwrap_or_else(|| {
                crate::room::panel::common::user_display(
                    c.workspace,
                    c.use_real(),
                    &username,
                    m.u.name.as_deref(),
                )
            });
        let time = m
            .ts
            .map(|d| crate::format::moment(d.timestamp_millis(), c.local_now, c.prefs.clock_24h))
            .unwrap_or_default();
        let src = c.images.avatar(
            c.server_id,
            c.server,
            &Avatar::User {
                username: username.clone(),
            },
            &name,
        );
        // Rows are rebuilt only when what they show changes (many rows,
        // views on every update).
        let key = (
            self.run.as_ref().map_or(0, |r| r.id),
            m.id.clone(),
            origin == Origin::Local,
            super::text_tag(&m.msg),
            name.clone(),
            time.clone(),
            (
                crate::room::render::mode() == rustket_ui::theme::Mode::Dark,
                rustket_ui::theme::generation(),
            ),
            crate::i18n::cache_tag(),
            c.images.generation(c.server_id),
        );
        lazy(key, move |_| {
            let snip = self.snippet(m, &words, SNIPPET_CHARS);
            let mut body = column![highlighted(&snip.0, &snip.1)].spacing(2);
            let mut notes: Vec<Element<'static, Msg>> = Vec::new();
            if m.tmid.is_some() {
                notes.push(note(
                    Icon::MessageCircle,
                    t!("search.in_thread").into_owned(),
                ));
            }
            if origin == Origin::Local {
                notes.push(note(Icon::HardDrive, t!("search.from_cache").into_owned()));
            }
            if !notes.is_empty() {
                body = body.push(row(notes).spacing(SPACE_3));
            }
            let mut p = MessageRowProps::new(
                avatar(src.clone(), 32.0, AvatarShape::Rounded),
                sanitize_bidi(&name).into_owned(),
                time.clone(),
                body,
            );
            p.actions_toolbar = Some(rustket_ui::widget::message_toolbar(vec![icon_button_with(
                Icon::ArrowUpRight,
                Some(Msg::Open(m.id.clone())),
                t!("search.open"),
                false,
                28.0,
            )]));
            Element::from(
                mouse_area(message_row(p))
                    .interaction(iced::mouse::Interaction::Pointer)
                    .on_press(Msg::Open(m.id.clone())),
            )
        })
        .into()
    }
}

fn note<'a>(i: Icon, s: String) -> Element<'a, Msg> {
    row![
        icon::sized(i, 11.0).style(style::text_hint),
        text(s).size(TEXT_CAPTION).style(style::text_hint),
    ]
    .spacing(SPACE_1)
    .align_y(Alignment::Center)
    .into()
}
