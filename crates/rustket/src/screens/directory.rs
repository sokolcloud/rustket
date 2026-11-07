//! Directory of one server: channels, users and teams (`directory` endpoint:
//! type, text, sort, pagination). Click: channel → open (preview) / join,
//! user → DM, team → its main room. Owner: account agent. Strings:
//! `locales/directory.yml` (`directory.*`).

pub mod open;

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use iced::widget::{button, column, container, row, text};
use iced::{Alignment, Element, Length, Subscription, Task};
use rocketchat::RcDate;
use rocketchat::rest::Query;
use rust_i18n::t;
use rustket_core::room_list::Avatar;
use rustket_ui::icon::{self, Icon};
use rustket_ui::style;
use rustket_ui::theme::{
    FONT_MEDIUM, SPACE_1, SPACE_2, SPACE_3, SPACE_4, TEXT_BODY, TEXT_CAPTION, TEXT_SMALL,
};
use rustket_ui::widget::{
    AvatarShape, BannerKind, avatar, banner, empty_state, ghost_button, loading_placeholder,
    secondary_button,
};
use serde_json::{Value, json};

use crate::component::{Action, Ctx, Effect, ViewCtx};
use crate::error::Res;
use open::{Opened, Target};

pub const PAGE_SIZE: u64 = 25;
pub const SEARCH_ID: &str = "directory-search";
const DEBOUNCE: Duration = Duration::from_millis(300);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Hash)]
pub enum Tab {
    #[default]
    Channels,
    Users,
    Teams,
}

impl Tab {
    pub const ALL: [Tab; 3] = [Tab::Channels, Tab::Users, Tab::Teams];

    fn api_type(self) -> &'static str {
        match self {
            Tab::Channels => "channels",
            Tab::Users => "users",
            Tab::Teams => "teams",
        }
    }

    fn i18n_key(self) -> &'static str {
        match self {
            Tab::Channels => "directory.channels",
            Tab::Users => "directory.users",
            Tab::Teams => "directory.teams",
        }
    }

    /// Sort keys the server accepts for this type.
    pub fn sorts(self) -> &'static [Sort] {
        match self {
            Tab::Users => &[Sort::Name, Sort::Username, Sort::Created],
            _ => &[Sort::Name, Sort::Members, Sort::Created],
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Sort {
    Name,
    Username,
    Members,
    Created,
}

impl Sort {
    fn field(self) -> &'static str {
        match self {
            Sort::Name => "name",
            Sort::Username => "username",
            Sort::Members => "usersCount",
            Sort::Created => "createdAt",
        }
    }

    fn i18n_key(self) -> &'static str {
        match self {
            Sort::Name => "directory.sort_name",
            Sort::Username => "directory.sort_username",
            Sort::Members => "directory.sort_members",
            Sort::Created => "directory.sort_created",
        }
    }
}

/// One directory row (channel, team or user).
#[derive(Debug, Clone, PartialEq)]
pub struct Entry {
    pub id: String,
    pub kind: EntryKind,
    pub name: String,
    pub fname: Option<String>,
    pub username: Option<String>,
    pub topic: Option<String>,
    pub users_count: Option<u64>,
    pub rooms_count: Option<u64>,
    pub created: Option<RcDate>,
    pub avatar_etag: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryKind {
    Channel { private: bool },
    Team { private: bool },
    User,
}

impl Entry {
    pub fn parse(tab: Tab, v: &Value) -> Option<Self> {
        let s = |k: &str| v.get(k).and_then(Value::as_str).map(str::to_owned);
        let n = |k: &str| v.get(k).and_then(Value::as_u64);
        let date = |k: &str| {
            v.get(k)
                .and_then(|d| serde_json::from_value::<RcDate>(d.clone()).ok())
        };
        let id = s("_id")?;
        let private = s("t").as_deref() == Some("p");
        let kind = match tab {
            Tab::Users => EntryKind::User,
            Tab::Teams => EntryKind::Team { private },
            Tab::Channels if v.get("teamMain").and_then(Value::as_bool) == Some(true) => {
                EntryKind::Team { private }
            }
            Tab::Channels => EntryKind::Channel { private },
        };
        Some(Self {
            name: s("name")
                .or_else(|| s("username"))
                .unwrap_or_else(|| id.clone()),
            id,
            kind,
            fname: s("fname"),
            username: s("username"),
            topic: s("topic").filter(|t| !t.is_empty()),
            users_count: n("usersCount"),
            rooms_count: n("roomsCount"),
            created: date("createdAt").or_else(|| date("ts")),
            avatar_etag: s("avatarETag"),
        })
    }

    pub fn title(&self) -> &str {
        match self.kind {
            EntryKind::User => self.fname.as_deref().unwrap_or(&self.name),
            _ => self
                .fname
                .as_deref()
                .filter(|f| !f.is_empty())
                .unwrap_or(&self.name),
        }
    }

    /// What clicking the row opens.
    pub fn target(&self) -> Target {
        match self.kind {
            EntryKind::User => Target::User(self.username.clone().unwrap_or(self.name.clone())),
            _ => Target::Room(self.id.clone()),
        }
    }
}

/// One page of results.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Page {
    pub rows: Vec<Entry>,
    pub offset: u64,
    pub total: u64,
}

/// The query a page was loaded for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Request {
    pub tab: Tab,
    pub text: String,
    pub sort: Sort,
    pub ascending: bool,
    pub offset: u64,
}

impl Request {
    pub fn query(&self) -> Query {
        let mut sort = serde_json::Map::new();
        sort.insert(
            self.sort.field().to_owned(),
            json!(if self.ascending { 1 } else { -1 }),
        );
        Query::new()
            .push("type", self.tab.api_type())
            .opt("text", Some(self.text.trim()).filter(|t| !t.is_empty()))
            .push("sort", Value::Object(sort))
            .push("count", PAGE_SIZE)
            .push("offset", self.offset)
    }
}

pub async fn fetch(rest: rocketchat::RestClient, req: Request) -> rocketchat::Result<Page> {
    let v: Value = rest.get("directory", &req.query()).await?;
    let rows = v
        .get("result")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(|r| Entry::parse(req.tab, r)).collect())
        .unwrap_or_default();
    Ok(Page {
        rows,
        offset: v
            .get("offset")
            .and_then(Value::as_u64)
            .unwrap_or(req.offset),
        total: v.get("total").and_then(Value::as_u64).unwrap_or(0),
    })
}

#[derive(Debug, Clone)]
pub enum Message {
    Init,
    Tab(Tab),
    Text(String),
    Fire(u64),
    Loaded(u64, Res<Page>),
    Sort(Sort),
    Prev,
    Next,
    Open(Entry),
    Join(String),
    Opened(Box<Res<Opened>>),
    Close,
}

static INSTANCES: AtomicU64 = AtomicU64::new(1);

#[derive(Debug)]
pub struct Directory {
    instance: u64,
    pub tab: Tab,
    pub text: String,
    pub sort: Sort,
    pub ascending: bool,
    pub offset: u64,
    pub page: Option<Page>,
    pub loading: bool,
    pub error: Option<String>,
    /// Bumped by every query change; older responses are dropped.
    pub seq: u64,
    /// Row being opened/joined.
    pub busy: Option<String>,
}

#[allow(clippy::new_without_default)]
impl Directory {
    pub fn new() -> Self {
        Self {
            instance: INSTANCES.fetch_add(1, Ordering::Relaxed),
            tab: Tab::Channels,
            text: String::new(),
            sort: Sort::Name,
            ascending: true,
            offset: 0,
            page: None,
            loading: false,
            error: None,
            seq: 0,
            busy: None,
        }
    }

    pub fn request(&self) -> Request {
        Request {
            tab: self.tab,
            text: self.text.clone(),
            sort: self.sort,
            ascending: self.ascending,
            offset: self.offset,
        }
    }

    /// Loads the current request now (tagged with a new sequence number).
    fn load(&mut self, ctx: &Ctx<'_>) -> Action<Message> {
        self.seq += 1;
        self.loading = true;
        let seq = self.seq;
        let req = self.request();
        Action::task(ctx.rest_task(
            move |rest| fetch(rest, req),
            move |r| Message::Loaded(seq, r),
        ))
    }

    pub fn update(&mut self, msg: Message, ctx: &mut Ctx<'_>) -> Action<Message> {
        match msg {
            Message::Init => {
                let focus = iced::widget::operation::focus(SEARCH_ID);
                let a = self.load(ctx);
                Action::task(Task::batch([a.task, focus]))
            }
            Message::Tab(t) => {
                if t == self.tab {
                    return Action::none();
                }
                self.tab = t;
                if !t.sorts().contains(&self.sort) {
                    self.sort = Sort::Name;
                    self.ascending = true;
                }
                self.offset = 0;
                self.page = None;
                self.load(ctx)
            }
            Message::Text(s) => {
                self.text = s;
                self.offset = 0;
                self.seq += 1;
                self.loading = true;
                let seq = self.seq;
                Action::task(Task::perform(tokio::time::sleep(DEBOUNCE), move |_| {
                    Message::Fire(seq)
                }))
            }
            Message::Fire(seq) => {
                if seq != self.seq {
                    return Action::none();
                }
                self.load(ctx)
            }
            Message::Loaded(seq, r) => {
                if seq != self.seq {
                    return Action::none();
                }
                self.loading = false;
                match r {
                    Ok(p) => {
                        self.error = None;
                        self.page = Some(p);
                    }
                    Err(e) => self.error = Some(e.user_text()),
                }
                Action::none()
            }
            Message::Sort(s) => {
                if s == self.sort {
                    self.ascending = !self.ascending;
                } else {
                    self.sort = s;
                    // Counts and dates: largest/newest first.
                    self.ascending = matches!(s, Sort::Name | Sort::Username);
                }
                self.offset = 0;
                self.load(ctx)
            }
            Message::Prev => {
                if self.offset == 0 {
                    return Action::none();
                }
                self.offset = self.offset.saturating_sub(PAGE_SIZE);
                self.load(ctx)
            }
            Message::Next => {
                let total = self.page.as_ref().map_or(0, |p| p.total);
                if self.offset + PAGE_SIZE >= total {
                    return Action::none();
                }
                self.offset += PAGE_SIZE;
                self.load(ctx)
            }
            Message::Open(e) => {
                if self.busy.is_some() {
                    return Action::none();
                }
                let a = open::open(ctx, e.target(), |r| Message::Opened(Box::new(r)));
                if a.effects.is_empty() {
                    self.busy = Some(e.id);
                }
                a
            }
            Message::Join(rid) => {
                if self.busy.is_some() {
                    return Action::none();
                }
                let a = open::open(ctx, Target::Join(rid.clone()), |r| {
                    Message::Opened(Box::new(r))
                });
                if a.effects.is_empty() {
                    self.busy = Some(rid);
                }
                a
            }
            Message::Opened(r) => {
                let r = *r;
                self.busy = None;
                match open::finish(ctx.workspace, r) {
                    Ok(a) => a,
                    Err(e) => Action::effect(Effect::Toast(
                        BannerKind::Error,
                        t!("directory.open_failed").into_owned(),
                        e,
                    )),
                }
            }
            Message::Close => Action::effect(Effect::Close),
        }
    }

    pub fn subscription(&self) -> Subscription<Message> {
        Subscription::run_with(self.instance, |_| {
            use iced::futures::StreamExt;
            iced::futures::stream::once(async { Message::Init })
                .chain(iced::futures::stream::pending())
        })
    }

    pub fn view<'a>(&'a self, ctx: &ViewCtx<'a>) -> Element<'a, Message> {
        let tabs = row(Tab::ALL.iter().map(|&tab| {
            button(text(t!(tab.i18n_key()).into_owned()).size(TEXT_BODY))
                .padding([6, 12])
                .style(if tab == self.tab {
                    style::button_primary
                } else {
                    style::button_secondary
                })
                .on_press(Message::Tab(tab))
                .into()
        }))
        .spacing(SPACE_2);
        let search = iced::widget::text_input(&t!("directory.search"), &self.text)
            .id(SEARCH_ID)
            .on_input(Message::Text)
            .style(style::text_input)
            .size(TEXT_BODY)
            .padding([8, 12])
            .width(Length::Fill);
        let sorts = row(self.tab.sorts().iter().map(|&s| {
            let active = s == self.sort;
            let mut label = t!(s.i18n_key()).into_owned();
            if active {
                label.push_str(if self.ascending { " ↑" } else { " ↓" });
            }
            button(text(label).size(TEXT_SMALL))
                .padding([4, 10])
                .style(if active {
                    style::button_icon_active
                } else {
                    style::button_ghost
                })
                .on_press(Message::Sort(s))
                .into()
        }))
        .spacing(SPACE_1)
        .align_y(Alignment::Center);
        let toolbar = row![
            search,
            text(t!("directory.sort_by"))
                .size(TEXT_SMALL)
                .style(style::text_secondary),
            sorts
        ]
        .spacing(SPACE_3)
        .align_y(Alignment::Center);

        let mut body = column![tabs, toolbar].spacing(SPACE_4);
        if let Some(e) = &self.error {
            body = body.push(banner(BannerKind::Error, e.clone(), None));
        }
        let list: Element<'a, Message> = match &self.page {
            None => loading_placeholder(None),
            Some(p) if p.rows.is_empty() => container(empty_state(
                Icon::SearchX,
                t!("directory.empty"),
                t!("directory.empty_hint"),
            ))
            .height(260)
            .into(),
            Some(p) => {
                let mut col = column![].spacing(SPACE_1);
                for e in &p.rows {
                    col = col.push(self.row_view(e, ctx));
                }
                col.into()
            }
        };
        body = body.push(list);
        if let Some(p) = &self.page
            && p.total > 0
        {
            let first = (self.offset + 1).min(p.total);
            let last = (self.offset + p.rows.len() as u64).min(p.total);
            body = body.push(
                row![
                    text(t!(
                        "directory.range",
                        first = first,
                        last = last,
                        total = p.total
                    ))
                    .size(TEXT_SMALL)
                    .style(style::text_secondary)
                    .width(Length::Fill),
                    secondary_button(
                        t!("directory.prev"),
                        (self.offset > 0).then_some(Message::Prev)
                    )
                    .icon(Icon::ChevronLeft),
                    secondary_button(
                        t!("directory.next"),
                        (self.offset + PAGE_SIZE < p.total).then_some(Message::Next)
                    )
                    .icon(Icon::ChevronRight),
                ]
                .spacing(SPACE_2)
                .align_y(Alignment::Center),
            );
        }
        crate::screens::page::page(
            t!("directory.title").into_owned(),
            body.into(),
            Message::Close,
        )
    }

    fn row_view<'a>(&'a self, e: &'a Entry, ctx: &ViewCtx<'a>) -> Element<'a, Message> {
        let ws = ctx.workspace;
        let av = match e.kind {
            EntryKind::User => Avatar::User {
                username: e.username.clone().unwrap_or(e.name.clone()),
            },
            _ => Avatar::Room {
                rid: e.id.clone(),
                etag: e.avatar_etag.clone(),
            },
        };
        let src = ctx.images.avatar(ctx.server_id, ctx.server, &av, e.title());
        let kind_icon = match e.kind {
            EntryKind::Channel { private: false } => Icon::Hash,
            EntryKind::Channel { private: true } => Icon::Lock,
            EntryKind::Team { .. } => Icon::Users,
            EntryKind::User => Icon::AtSign,
        };
        let joined = ws.subscription(&e.id).is_some();
        let mut title = row![
            icon::sized(kind_icon, 14.0).style(style::text_secondary),
            text(e.title()).size(TEXT_BODY).font(FONT_MEDIUM),
        ]
        .spacing(SPACE_1)
        .align_y(Alignment::Center);
        if joined && e.kind != EntryKind::User {
            title = title.push(
                text(t!("directory.joined"))
                    .size(TEXT_CAPTION)
                    .style(style::text_success),
            );
        }
        let sub_line = match e.kind {
            EntryKind::User => e.username.as_ref().map(|u| format!("@{u}")),
            _ => e.topic.clone(),
        };
        let mut info = column![title].spacing(2);
        if let Some(s) = sub_line {
            info = info.push(
                text(s)
                    .size(TEXT_SMALL)
                    .style(style::text_secondary)
                    .wrapping(iced::widget::text::Wrapping::None),
            );
        }
        let mut meta = row![].spacing(SPACE_4).align_y(Alignment::Center);
        if let Some(n) = e.users_count {
            meta = meta.push(stat(
                Icon::User,
                t!("directory.members", n = n).into_owned(),
            ));
        }
        if let (EntryKind::Team { .. }, Some(n)) = (e.kind, e.rooms_count) {
            meta = meta.push(stat(
                Icon::MessagesSquare,
                t!("directory.rooms", n = n).into_owned(),
            ));
        }
        if let Some(d) = e.created {
            meta = meta.push(stat(Icon::Calendar, date_label(d)));
        }
        let busy = self.busy.as_deref() == Some(e.id.as_str());
        let mut line = row![
            avatar(src, 36.0, AvatarShape::Rounded),
            info.width(Length::Fill),
            meta
        ]
        .spacing(SPACE_3)
        .align_y(Alignment::Center);
        let joinable = !joined
            && matches!(
                e.kind,
                EntryKind::Channel { private: false } | EntryKind::Team { private: false }
            );
        if e.kind != EntryKind::User {
            // A fixed slot keeps the columns aligned with or without the button.
            let slot: Element<'a, Message> = if joinable {
                ghost_button(
                    t!("directory.join"),
                    self.busy.is_none().then(|| Message::Join(e.id.clone())),
                )
                .loading(busy)
                .into()
            } else {
                text("").into()
            };
            line = line.push(container(slot).width(84).align_x(Alignment::End));
        }
        button(line)
            .width(Length::Fill)
            .padding([SPACE_2, SPACE_3])
            .style(style::button_list_item(busy))
            .on_press_maybe(self.busy.is_none().then(|| Message::Open(e.clone())))
            .into()
    }
}

fn stat<'a>(i: Icon, label: String) -> Element<'a, Message> {
    row![
        icon::sized(i, 13.0).style(style::text_hint),
        text(label).size(TEXT_CAPTION).style(style::text_secondary)
    ]
    .spacing(SPACE_1)
    .align_y(Alignment::Center)
    .into()
}

/// Local calendar date of a timestamp (`2026-11-08`).
pub fn date_label(d: RcDate) -> String {
    use chrono::TimeZone;
    chrono::Local
        .timestamp_millis_opt(d.timestamp_millis())
        .single()
        .map(|l| l.format("%Y-%m-%d").to_string())
        .unwrap_or_default()
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn parses_rows_of_every_type() {
        let ch = json!({"_id":"r1","fname":"rk-acct-ch","name":"rk-acct-ch","t":"c","usersCount":2,
            "ts":"2026-11-07T21:48:45.593Z","topic":"Hello topic","default":false});
        let e = Entry::parse(Tab::Channels, &ch).unwrap();
        assert_eq!(e.kind, EntryKind::Channel { private: false });
        assert_eq!(e.topic.as_deref(), Some("Hello topic"));
        assert_eq!(e.users_count, Some(2));
        assert!(e.created.is_some());
        assert_eq!(e.target(), Target::Room("r1".into()));

        let team =
            json!({"_id":"r2","name":"t","t":"p","teamMain":true,"teamId":"T","roomsCount":3});
        let e = Entry::parse(Tab::Teams, &team).unwrap();
        assert_eq!(e.kind, EntryKind::Team { private: true });
        assert_eq!(e.rooms_count, Some(3));

        let user = json!({"_id":"u1","createdAt":"2026-11-07T15:55:00.509Z","name":"Rocket.Cat","username":"rocket.cat"});
        let e = Entry::parse(Tab::Users, &user).unwrap();
        assert_eq!(e.kind, EntryKind::User);
        assert_eq!(e.title(), "Rocket.Cat");
        assert_eq!(e.target(), Target::User("rocket.cat".into()));
        assert!(Entry::parse(Tab::Users, &json!({"name": "x"})).is_none());
    }

    #[test]
    fn query_has_type_text_sort_and_paging() {
        let req = Request {
            tab: Tab::Users,
            text: " ali ".into(),
            sort: Sort::Created,
            ascending: false,
            offset: 25,
        };
        let q = req.query().apply("directory");
        assert!(q.contains("type=users"), "{q}");
        assert!(q.contains("text=ali"), "{q}");
        assert!(q.contains("sort=%7B%22createdAt%22%3A-1%7D"), "{q}");
        assert!(q.contains("count=25") && q.contains("offset=25"), "{q}");
        let req = Request {
            text: "  ".into(),
            ..req
        };
        assert!(!req.query().apply("d").contains("text="));
    }
}
