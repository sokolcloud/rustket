//! Policies tab (docs/policy.md section 10) and the "managed" notice shown at
//! the top of every tab. The content is `crate::policy::page`'s model of the
//! policy; this file lays it out. Strings: `locales/policy.yml` (`policy.*`).

use iced::widget::text::Wrapping;
use iced::widget::{Column, button, column, container, row, text};
use iced::{Alignment, Element, Length};
use rust_i18n::t;
use rustket_core::policy::{
    Blocking, CoerceError, InvalidReason, Policy, ReportRow, RowStatus, SourceId, SourceInfo,
    display_text,
};
use rustket_ui::icon::{self, Icon};
use rustket_ui::style;
use rustket_ui::theme::{
    SPACE_1, SPACE_2, SPACE_3, SPACE_4, TEXT_BODY, TEXT_CAPTION, font_mono, font_semibold,
};
use rustket_ui::widget::{focus_ring, secondary_button};

use super::widgets::{caption, section};
use super::{Message, Tab};
use crate::policy::page::{self, Page};

/// What `--dump-policy` is called with (shown, never run): the running
/// executable's own path (the AppImage when running from one), resolved
/// once. Windows release builds have no console, so the report goes to a
/// file there.
pub(crate) fn dump_command() -> &'static str {
    static COMMAND: std::sync::LazyLock<String> = std::sync::LazyLock::new(|| {
        let exe = crate::desktop::autostart::executable()
            .or_else(|_| std::env::current_exe())
            .unwrap_or_else(|_| std::path::PathBuf::from(DEFAULT_EXE));
        dump_command_for(&exe, cfg!(windows))
    });
    &COMMAND
}

/// The executable named when its path cannot be found.
const DEFAULT_EXE: &str = if cfg!(windows) {
    "rustket.exe"
} else {
    "rustket"
};

/// The `--dump-policy` command line for `exe`. A path with a character
/// outside `[A-Za-z0-9_./:-]` (and `\` on Windows) is quoted: for
/// PowerShell in single quotes (a quote, ASCII or U+2018 to U+201B,
/// doubled) after the call operator, which a quoted command needs; for a
/// POSIX shell in single quotes (a quote written as `'\''`). `windows`
/// also adds the output file.
pub(super) fn dump_command_for(exe: &std::path::Path, windows: bool) -> String {
    let path = exe.to_string_lossy();
    let safe = |c: char| {
        c.is_ascii_alphanumeric()
            || matches!(c, '_' | '.' | '/' | ':' | '-')
            || (windows && c == '\\')
    };
    let path = if path.chars().all(safe) {
        path.into_owned()
    } else if windows {
        let mut quoted = String::with_capacity(path.len() + 2);
        for c in path.chars() {
            // PowerShell takes the typographic single quotes for `'` too.
            if matches!(c, '\'' | '\u{2018}' | '\u{2019}' | '\u{201A}' | '\u{201B}') {
                quoted.push(c);
            }
            quoted.push(c);
        }
        format!("& '{quoted}'")
    } else {
        format!("'{}'", path.replace('\'', r"'\''"))
    };
    if windows {
        format!("{path} --dump-policy policy.json")
    } else {
        format!("{path} --dump-policy")
    }
}

/// "Managed by your organization", or by the organization's name.
pub(crate) fn managed_text(p: &Policy) -> String {
    match p.organization_name() {
        Some(name) => t!("policy.managed_by", name = name).into_owned(),
        None => t!("policy.managed").into_owned(),
    }
}

/// Whether the top notice is shown: a key is applied or its strict value
/// is in force.
pub(crate) fn has_notice(p: &Policy) -> bool {
    !p.has_no_settings()
}

/// The notice at the top of every tab while the policy sets something:
/// "Details" opens the Policies tab (when it is offered), "Contact support"
/// the support link.
pub(super) fn notice<'a>(p: &Policy, details: bool) -> Option<Element<'a, Message>> {
    if !has_notice(p) {
        return None;
    }
    let label = match p.organization_name() {
        Some(name) => t!("policy.notice_by", name = name).into_owned(),
        None => t!("policy.notice").into_owned(),
    };
    let mut content = row![
        icon::sized(Icon::Lock, 16.0),
        text(label)
            .size(TEXT_BODY)
            .style(style::text_primary)
            .wrapping(Wrapping::WordOrGlyph)
            .width(Length::Fill),
    ]
    .spacing(SPACE_2)
    .align_y(Alignment::Center);
    if let Some(url) = p.support_url() {
        content = content.push(link_button(
            t!("policy.contact_support").into_owned(),
            Message::OpenUrl(url.to_owned()),
        ));
    }
    if details {
        content = content.push(secondary_button(
            t!("policy.details"),
            Some(Message::Tab(Tab::Policies)),
        ));
    }
    Some(
        container(content)
            .padding([SPACE_2, SPACE_3])
            .width(Length::Fill)
            .style(style::container_banner_info)
            .into(),
    )
}

fn link_button<'a>(label: String, msg: Message) -> Element<'a, Message> {
    focus_ring(
        button(text(label).size(TEXT_BODY).style(style::text_link))
            .padding(0)
            .style(style::button_link)
            .on_press(msg.clone()),
        Some(msg),
    )
    .into()
}

/// Translates a key computed from a core code.
fn tr(key: &str) -> String {
    t!(key).into_owned()
}

fn wrapped<'a>(s: String) -> iced::widget::Text<'a> {
    text(s).size(TEXT_BODY).wrapping(Wrapping::WordOrGlyph)
}

fn secondary<'a, M: 'a>(s: String) -> Element<'a, M> {
    caption(s).wrapping(Wrapping::WordOrGlyph).into()
}

/// A source's label: translated by kind, with the file name for files.
pub(crate) fn source_label(id: &SourceId) -> String {
    match page::source_label(id) {
        (key, Some(name)) => t!(key, name = display_text(name)).into_owned(),
        (key, None) => tr(key),
    }
}

/// A source's raw id, on one line.
fn source_id<'a>(id: &SourceId) -> iced::widget::Text<'a> {
    text(display_text(&id.to_string()))
        .size(TEXT_CAPTION)
        .font(font_mono())
        .wrapping(Wrapping::WordOrGlyph)
}

/// The tab's cards.
pub(super) fn view<'a>(p: &Page<'_>) -> Vec<Element<'a, Message>> {
    let mut cards = vec![caption(t!("policy.intro").into_owned()).into()];
    if !p.blocking.is_empty() {
        cards.push(blocking_box(p.blocking));
    }
    cards.push(rows_card(&p.rows));
    if !p.sources.is_empty() {
        cards.push(sources_card(p.sources));
    }
    cards.push(section(
        t!("policy.report_title").into_owned(),
        vec![
            row![
                caption(t!("policy.dump_hint", command = dump_command()).into_owned())
                    .width(Length::Fill),
                secondary_button(t!("policy.copy_report"), Some(Message::CopyPolicyReport)),
            ]
            .spacing(SPACE_3)
            .align_y(Alignment::Center)
            .into(),
        ],
    ));
    cards
}

/// The blocking states, prominent.
fn blocking_box<'a>(blocking: &[Blocking]) -> Element<'a, Message> {
    let mut col = column![
        row![
            icon::sized(Icon::CircleAlert, 18.0),
            text(t!("policy.blocking_title").into_owned())
                .size(TEXT_BODY)
                .font(font_semibold()),
        ]
        .spacing(SPACE_2)
        .align_y(Alignment::Center)
    ]
    .spacing(SPACE_3);
    for b in blocking {
        let item: Element<'a, Message> = match b {
            Blocking::SourceUnreadable { source, error } => {
                let mut c = column![
                    wrapped(t!("policy.blocked_unreadable").into_owned()).font(font_semibold()),
                    wrapped(format!(
                        "{} · {}",
                        source_label(source),
                        tr(&page::error_key(error.kind))
                    )),
                    source_id(source),
                ]
                .spacing(SPACE_1);
                if let Some(detail) = &error.detail {
                    c = c.push(secondary(detail.clone()));
                }
                c.into()
            }
            Blocking::BelowMinimumVersion { required, client } => column![
                wrapped(t!("policy.blocked_version").into_owned()).font(font_semibold()),
                wrapped(
                    t!(
                        "policy.versions",
                        client = client.clone(),
                        required = required.to_string()
                    )
                    .into_owned()
                ),
            ]
            .spacing(SPACE_1)
            .into(),
        };
        col = col.push(item);
    }
    container(col)
        .padding(SPACE_4)
        .width(Length::Fill)
        .style(style::container_banner_error)
        .into()
}

/// Column widths of the rows table.
const KEY_PORTION: u16 = 3;
const VALUE_PORTION: u16 = 4;
const SOURCE_PORTION: u16 = 3;
const STATUS_PORTION: u16 = 3;

fn table_row<'a>(cells: [Element<'a, Message>; 4]) -> Element<'a, Message> {
    let [k, v, s, st] = cells;
    row![
        container(k).width(Length::FillPortion(KEY_PORTION)),
        container(v).width(Length::FillPortion(VALUE_PORTION)),
        container(s).width(Length::FillPortion(SOURCE_PORTION)),
        container(st).width(Length::FillPortion(STATUS_PORTION)),
    ]
    .spacing(SPACE_3)
    .into()
}

fn rows_card<'a>(rows: &[&ReportRow]) -> Element<'a, Message> {
    let title = t!("policy.rows_title").into_owned();
    if rows.is_empty() {
        return section(
            title,
            vec![caption(t!("policy.no_rows").into_owned()).into()],
        );
    }
    let head = |key: &str| -> Element<'a, Message> {
        text(tr(key))
            .size(TEXT_CAPTION)
            .font(font_semibold())
            .style(style::text_secondary)
            .into()
    };
    let mut items = vec![table_row([
        head("policy.col_key"),
        head("policy.col_value"),
        head("policy.col_source"),
        head("policy.col_status"),
    ])];
    items.extend(rows.iter().map(|r| report_row(r)));
    section(title, items)
}

fn report_row<'a>(r: &ReportRow) -> Element<'a, Message> {
    let mut status = Column::new()
        .spacing(SPACE_1)
        .push(wrapped(tr(&page::status_key(&r.status))));
    if let RowStatus::Invalid {
        reason,
        strict_applied,
    } = &r.status
    {
        status = status.push(secondary(reason_text(reason)));
        status = status.push(secondary(if *strict_applied {
            t!("policy.strict_applied").into_owned()
        } else {
            t!("policy.strict_not_applied").into_owned()
        }));
    }
    let main = table_row([
        wrapped(display_text(&r.key)).font(font_semibold()).into(),
        text(r.value.clone())
            .size(TEXT_CAPTION)
            .font(font_mono())
            .wrapping(Wrapping::WordOrGlyph)
            .into(),
        wrapped(source_label(&r.source)).into(),
        status.into(),
    ]);
    if r.dropped.is_empty() {
        return main;
    }
    let mut dropped = column![].spacing(SPACE_1);
    for d in &r.dropped {
        dropped = dropped.push(secondary(
            t!(
                "policy.dropped_item",
                position = d.position,
                item = d.item.as_str(),
                reason = tr(&page::drop_key(d))
            )
            .into_owned(),
        ));
    }
    if r.dropped_omitted > 0 {
        dropped = dropped.push(secondary(
            t!("policy.dropped_more", count = r.dropped_omitted).into_owned(),
        ));
    }
    column![main, container(dropped).padding([0.0, SPACE_4])]
        .spacing(SPACE_2)
        .into()
}

/// An invalid value's reason, translated by its code.
fn reason_text(reason: &InvalidReason) -> String {
    let (max, variants) = match reason {
        InvalidReason::TooLong { max } => (max.to_string(), String::new()),
        InvalidReason::Coerce(CoerceError::UnknownVariant { variants, .. }) => {
            (String::new(), variants.join(", "))
        }
        _ => (String::new(), String::new()),
    };
    t!(&page::reason_key(reason), max = max, variants = variants).into_owned()
}

/// Every source with its trust level, trust problems and error (the
/// Policies tab, and "Details" of the blocking screen).
pub(crate) fn sources_card<'a, M: 'a>(sources: &[SourceInfo]) -> Element<'a, M> {
    let items = sources
        .iter()
        .map(|s| {
            let mut c = column![
                row![
                    wrapped(source_label(&s.id))
                        .font(font_semibold())
                        .width(Length::Fill),
                    text(tr(&page::trust_key(s.trust)))
                        .size(TEXT_CAPTION)
                        .style(style::text_secondary),
                ]
                .spacing(SPACE_3)
                .align_y(Alignment::Center),
                source_id(&s.id).style(style::text_secondary),
            ]
            .spacing(SPACE_1);
            for problem in &s.trust_problems {
                let (found, expected) = match problem {
                    rustket_core::policy::TrustProblem::WrongOwner {
                        found, expected, ..
                    } => (found.to_string(), expected.to_string()),
                    _ => (String::new(), String::new()),
                };
                c = c.push(
                    row![
                        icon::sized(Icon::TriangleAlert, TEXT_CAPTION),
                        wrapped(
                            t!(
                                &page::trust_problem_key(problem),
                                found = found,
                                expected = expected
                            )
                            .into_owned()
                        )
                        .size(TEXT_CAPTION),
                    ]
                    .spacing(SPACE_1)
                    .align_y(Alignment::Center),
                );
                if let rustket_core::policy::TrustProblem::NotChecked {
                    detail: Some(d), ..
                } = problem
                {
                    c = c.push(secondary(d.clone()));
                }
            }
            if let Some(e) = &s.error {
                c = c.push(
                    text(
                        t!(
                            "policy.source_unreadable",
                            error = tr(&page::error_key(e.kind))
                        )
                        .into_owned(),
                    )
                    .size(TEXT_CAPTION)
                    .style(style::text_danger)
                    .wrapping(Wrapping::WordOrGlyph),
                );
                if let Some(d) = &e.detail {
                    c = c.push(secondary(d.clone()));
                }
            }
            c.into()
        })
        .collect();
    section(t!("policy.sources_title").into_owned(), items)
}
