//! Shared pieces of the admin pages: the update context, paged server-side
//! lists (search with debounce, sort, pages), table/form helpers and error
//! texts.

use std::time::Duration;

use chrono::{Local, TimeZone};
use iced::widget::{button, column, container, row, space, text, text_input, toggler};
use iced::{Alignment, Element, Length, Task};
use rocketchat::RcDate;
use rocketchat::models::Paginated;
use rocketchat::models::admin::LicenseInfo;
use rocketchat::rest::admin::{Page as RestPage, Sort};
use rust_i18n::t;
use rustket_core::admin::server_i18n::ServerI18n;
use rustket_ui::icon::{self, Icon};
use rustket_ui::style;
use rustket_ui::theme::{
    FONT_MEDIUM, FONT_SEMIBOLD, SPACE_1, SPACE_2, SPACE_3, SPACE_4, TEXT_BODY, TEXT_CAPTION,
    TEXT_SMALL, TEXT_SUBTITLE,
};
use rustket_ui::widget::{
    BannerKind, banner, dialog, divider, ghost_button, icon_button, loading_placeholder,
    primary_button, secondary_button,
};

use super::Message;
use super::guard::{Done, Op};
use crate::component::{Ctx, Effect};
use crate::error::{Failure, Res};

/// Search debounce of server-side lists (the web uses 400–500 ms).
pub const DEBOUNCE: Duration = Duration::from_millis(400);
pub const PAGE_SIZE: u64 = 25;

/// A confirmation dialog; `yes` is sent when confirmed.
#[derive(Debug, Clone)]
pub struct Confirm {
    pub title: String,
    pub body: String,
    pub danger: bool,
    pub yes_label: String,
    pub yes: Box<Message>,
}

impl Confirm {
    pub fn danger(title: String, body: String, yes: Message) -> Self {
        Self {
            title,
            body,
            danger: true,
            yes_label: t!("admin.delete").into_owned(),
            yes: Box::new(yes),
        }
    }

    pub fn ask(title: String, body: String, yes_label: String, yes: Message) -> Self {
        Self {
            title,
            body,
            danger: false,
            yes_label,
            yes: Box::new(yes),
        }
    }
}

/// What a page's `update` gets: the server context, admin-wide data, and
/// outputs (effects, a confirmation to ask).
pub struct Cx<'a, 'b> {
    pub ctx: &'a mut Ctx<'b>,
    pub i18n: &'a ServerI18n,
    pub license: Option<&'a LicenseInfo>,
    pub effects: Vec<Effect>,
    pub confirm: Option<Confirm>,
}

impl Cx<'_, '_> {
    pub fn ok(&mut self, body: String) {
        self.effects.push(Effect::Toast(
            BannerKind::Success,
            t!("admin.done").into_owned(),
            body,
        ));
    }

    pub fn fail(&mut self, f: &Failure) {
        let body = err_text(f, self.i18n);
        self.effects.push(Effect::Toast(
            BannerKind::Error,
            t!("admin.failed").into_owned(),
            body,
        ));
    }

    /// Toasts a failed write; `true` when it succeeded.
    pub fn check<T>(&mut self, r: &Res<T>) -> bool {
        match r {
            Ok(_) => true,
            Err(f) => {
                self.fail(f);
                false
            }
        }
    }

    pub fn ask(&mut self, c: Confirm) {
        self.confirm = Some(c);
    }

    /// A 2FA-capable call (the admin screen asks for a code when needed).
    pub fn guarded(&self, op: Op) -> Task<Message> {
        super::guard::run(self.ctx, op, None)
    }

    pub fn site_url(&self) -> String {
        self.ctx
            .workspace
            .settings()
            .str("Site_Url")
            .filter(|s| !s.is_empty())
            .map(|s| s.trim_end_matches('/').to_owned())
            .unwrap_or_else(|| self.ctx.server.as_str().trim_end_matches('/').to_owned())
    }
}

/// Server error text: the server's translation of its error key when it has
/// one (e.g. `LDAP_disabled`), else the generic text.
pub fn err_text(f: &Failure, i18n: &ServerI18n) -> String {
    use rocketchat::Error as E;
    if let E::Api {
        error, error_type, ..
    } = f.error()
    {
        let mut keys: Vec<&str> = Vec::new();
        if let Some(e) = error.as_deref() {
            keys.push(e);
            // "Message [error-key]" carries the key in brackets.
            if let Some(k) = e.rsplit_once('[').and_then(|(_, r)| r.strip_suffix(']')) {
                keys.push(k);
            }
        }
        if let Some(t) = error_type.as_deref() {
            keys.push(t);
        }
        for k in keys {
            if let Some(s) = i18n.t(k) {
                return strip_tags(s);
            }
        }
        if let Some(e) = error.as_deref()
            && matches!(
                e,
                "error-not-allowed" | "error-not-authorized" | "[not_authorized]"
            )
        {
            return t!("admin.not_allowed").into_owned();
        }
    }
    f.user_text()
}

/// Drops HTML-ish markup from server strings (`<br/>`, `<b>`, …).
pub fn strip_tags(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut in_tag = false;
    let mut rest = s;
    while let Some(c) = rest.chars().next() {
        rest = &rest[c.len_utf8()..];
        match c {
            '<' => {
                in_tag = true;
                // Line breaks and list items become new lines.
                let lower = rest.to_ascii_lowercase();
                if lower.starts_with("br") || lower.starts_with("li") || lower.starts_with("/p") {
                    out.push('\n');
                }
            }
            '>' if in_tag => in_tag = false,
            _ if !in_tag => out.push(c),
            _ => {}
        }
    }
    out.trim().to_owned()
}

pub fn date(d: Option<RcDate>) -> String {
    d.and_then(|d| Local.timestamp_millis_opt(d.timestamp_millis()).single())
        .map(|l| l.format("%Y-%m-%d %H:%M").to_string())
        .unwrap_or_else(|| "—".into())
}

pub fn size(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut v = bytes as f64;
    let mut u = 0;
    while v >= 1024.0 && u + 1 < UNITS.len() {
        v /= 1024.0;
        u += 1;
    }
    if u == 0 {
        format!("{bytes} B")
    } else {
        format!("{v:.1} {}", UNITS[u])
    }
}

pub fn num(n: Option<u64>) -> String {
    n.map(|n| n.to_string()).unwrap_or_else(|| "—".into())
}

// --- server-side paged lists --------------------------------------------------

#[derive(Debug, Clone)]
pub enum ListMsg {
    Search(String),
    Debounced(u64),
    Prev,
    Next,
    Sort(&'static str),
    Reload,
}

/// What the page must do after [`List::update`].
pub enum Step {
    None,
    /// Wait for the debounce (map the task into the page's message).
    Wait(Task<ListMsg>),
    Load,
}

/// One page of a server-side list with its query.
#[derive(Debug, Clone)]
pub struct List<T> {
    pub query: String,
    pub offset: u64,
    pub page_size: u64,
    pub total: u64,
    pub items: Vec<T>,
    pub loading: bool,
    pub loaded: bool,
    pub error: Option<String>,
    /// Sort field and direction.
    pub sort: Option<(&'static str, bool)>,
    stamp: u64,
    request: u64,
}

impl<T> Default for List<T> {
    fn default() -> Self {
        Self::new(None)
    }
}

impl<T> List<T> {
    pub fn new(sort: Option<(&'static str, bool)>) -> Self {
        Self {
            query: String::new(),
            offset: 0,
            page_size: PAGE_SIZE,
            total: 0,
            items: Vec::new(),
            loading: false,
            loaded: false,
            error: None,
            sort,
            stamp: 0,
            request: 0,
        }
    }

    pub fn update(&mut self, m: ListMsg) -> Step {
        match m {
            ListMsg::Search(q) => {
                self.query = q;
                self.stamp += 1;
                let stamp = self.stamp;
                Step::Wait(Task::perform(tokio::time::sleep(DEBOUNCE), move |_| {
                    ListMsg::Debounced(stamp)
                }))
            }
            ListMsg::Debounced(s) if s == self.stamp => {
                self.offset = 0;
                Step::Load
            }
            ListMsg::Debounced(_) => Step::None,
            ListMsg::Prev if self.offset > 0 => {
                self.offset = self.offset.saturating_sub(self.page_size);
                Step::Load
            }
            ListMsg::Next if self.offset + self.page_size < self.total => {
                self.offset += self.page_size;
                Step::Load
            }
            ListMsg::Prev | ListMsg::Next => Step::None,
            ListMsg::Sort(field) => {
                self.sort = Some(match self.sort {
                    Some((f, asc)) if f == field => (field, !asc),
                    _ => (field, true),
                });
                self.offset = 0;
                Step::Load
            }
            ListMsg::Reload => Step::Load,
        }
    }

    /// Starts a load; pass the returned id back to [`Self::finish`].
    pub fn begin(&mut self) -> u64 {
        self.request += 1;
        self.loading = true;
        self.request
    }

    pub fn rest_page(&self) -> RestPage {
        let p = RestPage::new(self.offset, self.page_size);
        match self.sort {
            Some((f, asc)) => p.sorted(if asc { Sort::asc(f) } else { Sort::desc(f) }),
            None => p,
        }
    }

    /// Applies a page; returns true when a deletion emptied the last page and
    /// the caller must fetch again (the offset already stepped back).
    #[must_use]
    pub fn finish(&mut self, request: u64, r: Result<Paginated<T>, String>) -> bool {
        if request != self.request {
            return false;
        }
        self.loading = false;
        self.loaded = true;
        match r {
            Ok(p) => {
                self.total = p.total;
                self.items = p.items;
                self.error = None;
                // A deletion emptied the last page: step back.
                if self.items.is_empty() && self.offset > 0 && self.offset >= self.total {
                    self.offset = self.total.saturating_sub(1) / self.page_size * self.page_size;
                    return true;
                }
            }
            Err(e) => self.error = Some(e),
        }
        false
    }

    /// Fills the list from a complete, client-side array (no paging server-side).
    pub fn set_all(&mut self, items: Vec<T>) {
        self.loading = false;
        self.loaded = true;
        self.error = None;
        self.total = items.len() as u64;
        self.page_size = self.page_size.max(self.total);
        self.items = items;
    }

    pub fn search_term(&self) -> Option<String> {
        let q = self.query.trim();
        (!q.is_empty()).then(|| q.to_owned())
    }
}

/// Search field with a refresh button.
pub fn search_bar<'a, M: Clone + 'a>(
    value: &'a str,
    placeholder: String,
    on_input: impl Fn(String) -> M + 'a,
    reload: M,
) -> Element<'a, M> {
    row![
        text_input(&placeholder, value)
            .on_input(on_input)
            .padding([6, 10])
            .size(TEXT_BODY)
            .style(style::text_input)
            .width(Length::Fill),
        icon_button(Icon::RefreshCw, Some(reload), t!("admin.refresh")),
    ]
    .spacing(SPACE_2)
    .align_y(Alignment::Center)
    .into()
}

/// A table column: label, width portion, optional server sort field.
pub struct Col {
    pub label: String,
    pub portion: u16,
    pub sort: Option<&'static str>,
}

pub fn col(label: impl Into<String>, portion: u16) -> Col {
    Col {
        label: label.into(),
        portion,
        sort: None,
    }
}

pub fn sortable(label: impl Into<String>, portion: u16, field: &'static str) -> Col {
    Col {
        label: label.into(),
        portion,
        sort: Some(field),
    }
}

/// Table of `rows` (each row's cells match `cols`); clicking a row sends its
/// message. Sortable headers send `on_sort(field)`.
pub fn table<'a, M: Clone + 'a>(
    cols: Vec<Col>,
    sort: Option<(&'static str, bool)>,
    on_sort: impl Fn(&'static str) -> M,
    rows: Vec<(Vec<Element<'a, M>>, Option<M>)>,
) -> Element<'a, M> {
    let mut header = row![].spacing(SPACE_3).padding([SPACE_2, SPACE_3]);
    let portions: Vec<u16> = cols.iter().map(|c| c.portion).collect();
    for c in cols {
        let mut label = row![
            text(c.label)
                .size(TEXT_CAPTION)
                .font(FONT_SEMIBOLD)
                .style(style::text_secondary)
        ]
        .spacing(SPACE_1)
        .align_y(Alignment::Center);
        if let (Some(f), Some((sf, asc))) = (c.sort, sort)
            && f == sf
        {
            label = label.push(
                icon::sized(
                    if asc {
                        Icon::ChevronUp
                    } else {
                        Icon::ChevronDown
                    },
                    12.0,
                )
                .style(style::text_secondary),
            );
        }
        let cell: Element<'a, M> = match c.sort {
            Some(f) => button(label)
                .padding(0)
                .style(style::button_bare)
                .on_press(on_sort(f))
                .into(),
            None => label.into(),
        };
        header = header.push(container(cell).width(Length::FillPortion(c.portion)));
    }
    let mut body = column![header, divider()];
    for (cells, on_press) in rows {
        let mut r = row![].spacing(SPACE_3).align_y(Alignment::Center);
        for (i, cell) in cells.into_iter().enumerate() {
            let p = portions.get(i).copied().unwrap_or(1);
            r = r.push(container(cell).width(Length::FillPortion(p)).clip(true));
        }
        let line: Element<'a, M> = match on_press {
            Some(m) => button(r)
                .padding([SPACE_2, SPACE_3])
                .width(Length::Fill)
                .style(style::button_list_item(false))
                .on_press(m)
                .into(),
            None => container(r)
                .padding([SPACE_2, SPACE_3])
                .width(Length::Fill)
                .into(),
        };
        body = body.push(line).push(divider());
    }
    container(body)
        .width(Length::Fill)
        .style(style::container_card)
        .into()
}

/// The state rows of a list: loading, error, empty, or the table and pager.
pub fn list_view<'a, T, M: Clone + 'a>(
    list: &'a List<T>,
    table: impl FnOnce() -> Element<'a, M>,
    to_msg: impl Fn(ListMsg) -> M + Clone + 'a,
    empty: String,
) -> Element<'a, M> {
    if !list.loaded && list.loading {
        return container(loading_placeholder(None)).height(200).into();
    }
    let mut col = column![].spacing(SPACE_3);
    if let Some(e) = &list.error {
        col = col.push(banner(
            BannerKind::Error,
            e.clone(),
            Some((t!("admin.retry").into_owned(), to_msg(ListMsg::Reload))),
        ));
    }
    if list.items.is_empty() {
        if list.error.is_none() {
            col = col.push(
                container(text(empty).size(TEXT_BODY).style(style::text_secondary))
                    .padding(SPACE_4),
            );
        }
        return col.into();
    }
    col = col.push(table());
    col = col.push(pager(list, to_msg));
    col.into()
}

pub fn pager<'a, T, M: Clone + 'a>(
    list: &List<T>,
    to_msg: impl Fn(ListMsg) -> M,
) -> Element<'a, M> {
    let from = if list.total == 0 { 0 } else { list.offset + 1 };
    let to = (list.offset + list.items.len() as u64).min(list.total.max(from));
    row![
        text(t!("admin.range", from = from, to = to, total = list.total))
            .size(TEXT_SMALL)
            .style(style::text_secondary)
            .width(Length::Fill),
        icon_button(
            Icon::ChevronLeft,
            (list.offset > 0).then(|| to_msg(ListMsg::Prev)),
            t!("admin.prev")
        ),
        icon_button(
            Icon::ChevronRight,
            (list.offset + list.page_size < list.total).then(|| to_msg(ListMsg::Next)),
            t!("admin.next")
        ),
    ]
    .spacing(SPACE_2)
    .align_y(Alignment::Center)
    .into()
}

/// Page title row with optional buttons on the right.
pub fn title_row<'a, M: Clone + 'a>(title: String, buttons: Vec<Element<'a, M>>) -> Element<'a, M> {
    row![
        text(title)
            .size(TEXT_SUBTITLE)
            .font(FONT_SEMIBOLD)
            .width(Length::Fill)
    ]
    .extend(buttons)
    .spacing(SPACE_2)
    .align_y(Alignment::Center)
    .into()
}

/// Label above a control (with an optional hint below).
pub fn field<'a, M: 'a>(
    label: String,
    hint: Option<String>,
    control: impl Into<Element<'a, M>>,
) -> Element<'a, M> {
    let mut col = column![
        text(label).size(TEXT_SMALL).font(FONT_MEDIUM),
        control.into()
    ]
    .spacing(SPACE_1)
    .width(Length::Fill);
    if let Some(h) = hint {
        col = col.push(text(h).size(TEXT_CAPTION).style(style::text_secondary));
    }
    col.into()
}

pub fn input<'a, M: Clone + 'a>(
    value: &'a str,
    on_input: Option<impl Fn(String) -> M + 'a>,
) -> iced::widget::TextInput<'a, M> {
    let i = text_input("", value)
        .padding([6, 10])
        .size(TEXT_BODY)
        .style(style::text_input);
    match on_input {
        Some(f) => i.on_input(f),
        None => i,
    }
}

/// Text field with label.
pub fn text_row<'a, M: Clone + 'a>(
    label: String,
    value: &'a str,
    on_input: impl Fn(String) -> M + 'a,
) -> Element<'a, M> {
    field(label, None, input(value, Some(on_input)))
}

/// Label with a switch on the right.
pub fn switch<'a, M: Clone + 'a>(
    label: String,
    value: bool,
    on_toggle: Option<impl Fn(bool) -> M + 'a>,
) -> Element<'a, M> {
    row![
        text(label).size(TEXT_BODY).width(Length::Fill),
        toggler(value)
            .size(20)
            .style(style::toggler)
            .on_toggle_maybe(on_toggle),
    ]
    .spacing(SPACE_3)
    .align_y(Alignment::Center)
    .into()
}

/// Key/value line of a details card.
pub fn kv<'a, M: 'a>(key: String, value: String) -> Element<'a, M> {
    row![
        text(key)
            .size(TEXT_SMALL)
            .style(style::text_secondary)
            .width(Length::FillPortion(2)),
        text(value).size(TEXT_SMALL).width(Length::FillPortion(3)),
    ]
    .spacing(SPACE_3)
    .into()
}

/// A titled card.
pub fn card<'a, M: 'a>(title: String, body: impl Into<Element<'a, M>>) -> Element<'a, M> {
    container(
        column![text(title).size(TEXT_BODY).font(FONT_SEMIBOLD), body.into()].spacing(SPACE_3),
    )
    .padding(SPACE_4)
    .width(Length::Fill)
    .style(style::container_card)
    .into()
}

pub fn small<'a, M: 'a>(s: impl Into<String>) -> Element<'a, M> {
    text(s.into()).size(TEXT_SMALL).into()
}

pub fn muted<'a, M: 'a>(s: impl Into<String>) -> Element<'a, M> {
    text(s.into())
        .size(TEXT_SMALL)
        .style(style::text_secondary)
        .into()
}

pub fn yes_no<'a, M: 'a>(b: bool) -> Element<'a, M> {
    muted(yes_no_text(b))
}

pub fn yes_no_text(b: bool) -> String {
    if b { t!("admin.yes") } else { t!("admin.no") }.into_owned()
}

/// A form dialog with Cancel / Save (and optional extra left actions).
pub fn form_dialog<'a, M: Clone + 'a>(
    title: String,
    body: Element<'a, M>,
    error: Option<&'a str>,
    busy: bool,
    save: Option<M>,
    cancel: M,
    extra: Vec<Element<'a, M>>,
) -> Element<'a, M> {
    let mut col = column![body].spacing(SPACE_3);
    if let Some(e) = error {
        col = col.push(banner(BannerKind::Error, e.to_owned(), None));
    }
    let mut actions = extra;
    actions.push(secondary_button(t!("admin.cancel"), Some(cancel.clone())).into());
    actions.push(
        primary_button(t!("admin.save"), if busy { None } else { save })
            .loading(busy)
            .into(),
    );
    dialog(title, col, actions, Some(cancel))
}

/// Header of a sub page with a back button.
pub fn back_row<'a, M: Clone + 'a>(
    title: String,
    back: M,
    buttons: Vec<Element<'a, M>>,
) -> Element<'a, M> {
    row![
        ghost_button(t!("admin.back"), Some(back)).icon(Icon::ArrowLeft),
        text(title)
            .size(TEXT_SUBTITLE)
            .font(FONT_SEMIBOLD)
            .width(Length::Fill),
    ]
    .extend(buttons)
    .spacing(SPACE_2)
    .align_y(Alignment::Center)
    .into()
}

/// A selectable-looking read-only value (secrets, URLs) in monospace.
/// A disabled input showing `value` (for disabled selects and the like).
pub fn disabled_value<'a, M: Clone + 'a>(value: String) -> Element<'a, M> {
    text_input("", &value)
        .padding([6, 10])
        .size(TEXT_BODY)
        .style(style::text_input)
        .into()
}

pub fn readonly<'a, M: 'a>(value: String) -> Element<'a, M> {
    container(
        text(value)
            .size(TEXT_SMALL)
            .font(rustket_ui::theme::MONOSPACE_FONT),
    )
    .padding([6, 10])
    .width(Length::Fill)
    .style(style::container_code_block)
    .into()
}

pub fn gap<'a, M: 'a>() -> Element<'a, M> {
    space().height(SPACE_2).into()
}

/// Files picked with the system dialog.
#[derive(Debug, Clone)]
pub struct Picked {
    pub name: String,
    pub mime: String,
    pub data: std::sync::Arc<Vec<u8>>,
}

impl Picked {
    pub fn upload(&self) -> rocketchat::rest::admin::AdminUpload {
        rocketchat::rest::admin::AdminUpload::new(
            self.name.clone(),
            self.mime.clone(),
            self.data.as_ref().clone(),
        )
    }
}

/// MIME type from a file name (emoji take their extension from it).
pub fn mime_for(name: &str) -> &'static str {
    let ext = name
        .rsplit_once('.')
        .map(|(_, e)| e.to_ascii_lowercase())
        .unwrap_or_default();
    match ext.as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "svg" => "image/svg+xml",
        "webp" => "image/webp",
        "ico" => "image/x-icon",
        "bmp" => "image/bmp",
        "mp3" => "audio/mpeg",
        "wav" => "audio/wav",
        _ => "application/octet-stream",
    }
}

/// Opens the file dialog (filtered by extensions) and reads the file, at most
/// `max` bytes.
pub fn pick_file<M: Send + 'static>(
    exts: Vec<String>,
    max: u64,
    map: impl FnOnce(Result<Option<Picked>, String>) -> M + Send + 'static,
) -> Task<M> {
    Task::perform(
        async move {
            let mut d = rfd::AsyncFileDialog::new();
            if !exts.is_empty() {
                d = d.add_filter("", &exts);
            }
            let Some(f) = d.pick_file().await else {
                return Ok(None);
            };
            let path = f.path().to_owned();
            let name = f.file_name();
            let meta = tokio::fs::metadata(&path)
                .await
                .map_err(|e| e.to_string())?;
            if meta.len() > max {
                return Err(t!("admin.file_too_large", max = size(max)).into_owned());
            }
            let data = tokio::fs::read(&path).await.map_err(|e| e.to_string())?;
            Ok(Some(Picked {
                mime: mime_for(&name).to_owned(),
                name,
                data: std::sync::Arc::new(data),
            }))
        },
        map,
    )
}

/// The result of a guarded call for page `P`: `Some` when it was this op.
pub fn done_of<T>(r: &Res<Done>, f: impl Fn(&Done) -> Option<T>) -> Option<Result<T, Failure>> {
    match r {
        Ok(d) => f(d).map(Ok),
        Err(e) => Some(Err(e.clone())),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::sync::Arc;

    #[tokio::test]
    async fn list_paging_and_sort() {
        let mut l: List<u32> = List::new(Some(("name", true)));
        assert!(matches!(l.update(ListMsg::Next), Step::None));
        let r = l.begin();
        let _ = l.finish(
            r,
            Ok(Paginated {
                items: (0..25).collect(),
                count: 25,
                offset: 0,
                total: 60,
                raw_len: 25,
                dropped: 0,
            }),
        );
        assert!(matches!(l.update(ListMsg::Next), Step::Load));
        assert_eq!(l.offset, 25);
        assert!(matches!(l.update(ListMsg::Sort("name")), Step::Load));
        assert_eq!(l.sort, Some(("name", false)));
        assert_eq!(l.offset, 0);
        // A stale response is ignored.
        let old = l.begin();
        let new = l.begin();
        let _ = l.finish(old, Err("x".into()));
        assert!(l.error.is_none());
        let _ = l.finish(new, Err("boom".into()));
        assert_eq!(l.error.as_deref(), Some("boom"));
        // Debounce: only the latest stamp loads.
        let _ = l.update(ListMsg::Search("a".into()));
        let _ = l.update(ListMsg::Search("ab".into()));
        assert!(matches!(l.update(ListMsg::Debounced(1)), Step::None));
        assert!(matches!(l.update(ListMsg::Debounced(2)), Step::Load));
        assert_eq!(l.search_term().as_deref(), Some("ab"));
    }

    #[test]
    fn server_error_keys_are_translated() {
        let en: HashMap<String, String> =
            [("LDAP_disabled".to_owned(), "LDAP is <b>off</b>".to_owned())].into();
        let en = Arc::new(en);
        let i18n = ServerI18n::new("en", en.clone(), en);
        let f = Failure::from(rocketchat::Error::Api {
            status: 400,
            error: Some("LDAP_disabled".into()),
            error_type: None,
            message: None,
            details: None,
        });
        assert_eq!(err_text(&f, &i18n), "LDAP is off");
        let f = Failure::from(rocketchat::Error::Api {
            status: 400,
            error: Some("error-not-allowed".into()),
            error_type: None,
            message: None,
            details: None,
        });
        assert_eq!(err_text(&f, &i18n), t!("admin.not_allowed"));
    }

    #[test]
    fn tags_and_sizes() {
        assert_eq!(strip_tags("a<br/>b <b>c</b>"), "a\nb c");
        assert_eq!(size(512), "512 B");
        assert_eq!(size(2048), "2.0 KB");
        assert_eq!(mime_for("x.PNG"), "image/png");
    }
}
