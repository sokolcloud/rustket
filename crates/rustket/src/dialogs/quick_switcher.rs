//! Quick switcher (Ctrl+K): local rooms (fuzzy on name/fname, unread first)
//! plus `spotlight` (users and rooms the user has not joined), with debounced
//! queries and stale responses dropped. Up/Down select, Enter opens.
//! Owner: account agent. Strings: `locales/quick_switcher.yml`
//! (`quick_switcher.*`).

use std::time::Duration;

use iced::widget::{button, column, container, row, text, text_input};
use iced::{Alignment, Element, Length, Subscription, Task};
use rocketchat::models::{RoomType, SpotlightResult};
use rust_i18n::t;
use rustket_core::Workspace;
use rustket_core::room_list::{self, Avatar, BadgeVariant, RoomKind, RoomListOptions};
use rustket_ui::icon::{self, Icon};
use rustket_ui::style;
use rustket_ui::theme::{SPACE_1, SPACE_2, SPACE_3, TEXT_BODY, TEXT_CAPTION, TEXT_SMALL};
use rustket_ui::widget::{AvatarShape, BadgeKind, avatar, badge, dialog, kbd, spinner};

use crate::component::{Action, Ctx, Effect, ViewCtx};
use crate::error::Res;
use crate::screens::directory::open::{self, Opened, Target};

pub const INPUT_ID: &str = "quick-switcher";
const DEBOUNCE: Duration = Duration::from_millis(250);
const MAX_LOCAL: usize = 8;
const MAX_TOTAL: usize = 12;

#[derive(Debug, Clone)]
pub enum Message {
    Query(String),
    Fire(u64),
    Spotlight(u64, String, Res<SpotlightResult>),
    Up,
    Down,
    Select(usize),
    Open(usize),
    Submit,
    Opened(Box<Res<Opened>>),
    Close,
}

/// One result row.
#[derive(Debug, Clone, PartialEq)]
pub enum Item {
    /// A room in the sidebar.
    Local {
        rid: String,
        name: String,
        kind: RoomKind,
        avatar: Avatar,
        unread: u64,
        mention: bool,
    },
    /// A user without a DM yet (from `spotlight`).
    User { username: String, name: String },
    /// A room the user has not joined (from `spotlight`).
    Room {
        rid: String,
        name: String,
        private: bool,
    },
}

impl Item {
    fn target(&self) -> Target {
        match self {
            Item::Local { rid, .. } | Item::Room { rid, .. } => Target::Room(rid.clone()),
            Item::User { username, .. } => Target::User(username.clone()),
        }
    }
}

#[derive(Debug, Default)]
pub struct QuickSwitcher {
    pub query: String,
    pub selected: usize,
    /// Bumped by every query change.
    pub seq: u64,
    /// The latest spotlight result and the query it was for.
    pub remote: Option<(String, SpotlightResult)>,
    pub searching: bool,
    pub busy: bool,
    pub error: Option<String>,
}

/// Fuzzy score of `candidate` for `query` (lowercase); 0 = no match.
pub fn score(query: &str, candidate: &str) -> u32 {
    if query.is_empty() {
        return 1;
    }
    let c = candidate.to_lowercase();
    if c == query {
        return 1000;
    }
    if c.starts_with(query) {
        return 800 - c.len().min(200) as u32;
    }
    if let Some(i) = c.find(query) {
        let boundary = c[..i]
            .chars()
            .next_back()
            .is_some_and(|p| matches!(p, '-' | '_' | '.' | ' ' | '/'));
        return if boundary { 600 } else { 400 } - i.min(100) as u32;
    }
    // Subsequence: every query char in order; tighter spans score higher.
    let mut chars = c.char_indices();
    let mut first = None;
    let mut last = 0;
    for q in query.chars() {
        match chars.by_ref().find(|(_, ch)| *ch == q) {
            Some((i, _)) => {
                first.get_or_insert(i);
                last = i;
            }
            None => return 0,
        }
    }
    let span = last - first.unwrap_or(0) + 1;
    (200usize.saturating_sub(span * 4)).max(10) as u32
}

/// The rows for `query`: matching sidebar rooms (unread first, then score,
/// then activity), then spotlight users without a DM and rooms not joined.
pub fn results(
    ws: &Workspace,
    opts: &RoomListOptions,
    query: &str,
    remote: Option<&SpotlightResult>,
) -> Vec<Item> {
    let q = query.trim().trim_start_matches(['@', '#']).to_lowercase();
    let mut local: Vec<(bool, u32, Option<rocketchat::RcDate>, Item)> =
        room_list::compute(ws, opts, "")
            .into_iter()
            .flat_map(|s| s.items)
            .filter_map(|i| {
                let s = [Some(i.display_name.as_str()), Some(i.name.as_str())]
                    .into_iter()
                    .flatten()
                    .chain(ws.room(&i.rid).and_then(|e| e.fname()))
                    .map(|c| score(&q, c))
                    .max()
                    .unwrap_or(0);
                (s > 0).then(|| {
                    let unread = i.badge.count > 0 || i.badge.dot || i.badge.highlighted;
                    (
                        unread,
                        s,
                        i.last_activity,
                        Item::Local {
                            mention: i.badge.variant == BadgeVariant::Mention,
                            unread: i.badge.count,
                            rid: i.rid,
                            name: i.display_name,
                            kind: i.kind,
                            avatar: i.avatar,
                        },
                    )
                })
            })
            .collect();
    local.sort_by(|a, b| b.0.cmp(&a.0).then(b.1.cmp(&a.1)).then(b.2.cmp(&a.2)));
    let mut out: Vec<Item> = local
        .into_iter()
        .take(MAX_LOCAL)
        .map(|(_, _, _, i)| i)
        .collect();
    if let Some(r) = remote {
        let me = ws.username().unwrap_or_default();
        for u in &r.users {
            let Some(username) = u.username.as_deref() else {
                continue;
            };
            if username == me || open::local_dm(ws, username).is_some() {
                continue;
            }
            out.push(Item::User {
                username: username.to_owned(),
                name: u.name.clone().unwrap_or_else(|| username.to_owned()),
            });
        }
        for room in &r.rooms {
            if ws.subscription(&room.id).is_some() || room.t == RoomType::Direct {
                continue;
            }
            out.push(Item::Room {
                rid: room.id.clone(),
                name: room
                    .fname
                    .clone()
                    .or_else(|| room.name.clone())
                    .unwrap_or_else(|| room.id.clone()),
                private: room.t == RoomType::Private,
            });
        }
    }
    out.truncate(MAX_TOTAL);
    out
}

fn options(ctx_ws: &Workspace, prefs: &rustket_core::config::UiPrefs) -> RoomListOptions {
    let mut o = RoomListOptions::from_prefs(prefs, ctx_ws);
    o.unread_on_top = false;
    o
}

impl QuickSwitcher {
    pub fn new() -> Self {
        Self::default()
    }

    /// Focuses the search field (run when the dialog opens).
    pub fn focus() -> Task<Message> {
        iced::widget::operation::focus(INPUT_ID)
    }

    fn items(&self, ws: &Workspace, prefs: &rustket_core::config::UiPrefs) -> Vec<Item> {
        let remote = self
            .remote
            .as_ref()
            .filter(|(q, _)| *q == self.query.trim())
            .map(|(_, r)| r);
        results(ws, &options(ws, prefs), &self.query, remote)
    }

    pub fn update(&mut self, msg: Message, ctx: &mut Ctx<'_>) -> Action<Message> {
        match msg {
            Message::Query(q) => {
                self.query = q;
                self.selected = 0;
                self.seq += 1;
                self.error = None;
                if self.query.trim().is_empty() {
                    self.searching = false;
                    return Action::none();
                }
                self.searching = true;
                let seq = self.seq;
                Action::task(Task::perform(tokio::time::sleep(DEBOUNCE), move |_| {
                    Message::Fire(seq)
                }))
            }
            Message::Fire(seq) => {
                if seq != self.seq {
                    return Action::none();
                }
                let q = self.query.trim().to_owned();
                let q2 = q.clone();
                Action::task(ctx.rest_task(
                    move |rest| async move { rest.spotlight(&q).await },
                    move |r| Message::Spotlight(seq, q2, r),
                ))
            }
            Message::Spotlight(seq, q, r) => {
                if seq != self.seq {
                    return Action::none();
                }
                self.searching = false;
                match r {
                    Ok(res) => self.remote = Some((q, res)),
                    Err(e) => self.error = Some(e.user_text()),
                }
                Action::none()
            }
            Message::Up => {
                self.selected = self.selected.saturating_sub(1);
                Action::none()
            }
            Message::Down => {
                let n = self.items(ctx.workspace, &ctx.config.ui).len();
                if self.selected + 1 < n {
                    self.selected += 1;
                }
                Action::none()
            }
            Message::Select(i) => {
                self.selected = i;
                Action::none()
            }
            Message::Submit => self.open(self.selected, ctx),
            Message::Open(i) => self.open(i, ctx),
            Message::Opened(r) => {
                let r = *r;
                self.busy = false;
                match open::finish(ctx.workspace, r) {
                    Ok(a) => a.with(Effect::Close),
                    Err(e) => {
                        self.error = Some(e);
                        Action::none()
                    }
                }
            }
            Message::Close => Action::effect(Effect::Close),
        }
    }

    fn open(&mut self, i: usize, ctx: &mut Ctx<'_>) -> Action<Message> {
        if self.busy {
            return Action::none();
        }
        let Some(item) = self.items(ctx.workspace, &ctx.config.ui).into_iter().nth(i) else {
            return Action::none();
        };
        self.selected = i;
        let a = open::open(ctx, item.target(), |r| Message::Opened(Box::new(r)));
        if a.effects.is_empty() {
            self.busy = true;
            a
        } else {
            a.with(Effect::Close)
        }
    }

    pub fn subscription(&self) -> Subscription<Message> {
        iced::event::listen_with(|event, _status, _window| match event {
            iced::Event::Keyboard(iced::keyboard::Event::KeyPressed {
                key: iced::keyboard::Key::Named(named),
                ..
            }) => match named {
                iced::keyboard::key::Named::ArrowUp => Some(Message::Up),
                iced::keyboard::key::Named::ArrowDown => Some(Message::Down),
                _ => None,
            },
            _ => None,
        })
    }

    pub fn view<'a>(&'a self, ctx: &ViewCtx<'a>) -> Element<'a, Message> {
        let items = self.items(ctx.workspace, &ctx.config.ui);
        let mut list = column![].spacing(2);
        let mut last_group = None;
        for (i, item) in items.iter().enumerate() {
            let group = match item {
                Item::Local { .. } => 0,
                _ => 1,
            };
            if last_group != Some(group) && group == 1 {
                list = list.push(
                    container(
                        text(t!("quick_switcher.more_results"))
                            .size(TEXT_CAPTION)
                            .style(style::text_hint),
                    )
                    .padding([SPACE_1, SPACE_2]),
                );
            }
            last_group = Some(group);
            list = list.push(self.row(i, item, ctx));
        }
        if items.is_empty() && !self.searching {
            list = list.push(
                container(
                    text(t!("quick_switcher.no_results"))
                        .size(TEXT_SMALL)
                        .style(style::text_secondary),
                )
                .padding(SPACE_3),
            );
        }
        let mut input_row = row![
            text_input(&t!("quick_switcher.placeholder"), &self.query)
                .id(INPUT_ID)
                .on_input(Message::Query)
                .on_submit(Message::Submit)
                .style(style::text_input)
                .size(TEXT_BODY)
                .padding([8, 10])
                .width(Length::Fill)
        ]
        .spacing(SPACE_2)
        .align_y(Alignment::Center);
        if self.searching || self.busy {
            input_row = input_row.push(spinner(16.0));
        }
        let mut body = column![input_row].spacing(SPACE_2);
        if let Some(e) = &self.error {
            body = body.push(text(e.as_str()).size(TEXT_SMALL).style(style::text_danger));
        }
        body = body.push(list);
        body = body.push(
            row![
                kbd("↑↓"),
                text(t!("quick_switcher.hint_navigate"))
                    .size(TEXT_CAPTION)
                    .style(style::text_hint),
                kbd("Enter"),
                text(t!("quick_switcher.hint_open"))
                    .size(TEXT_CAPTION)
                    .style(style::text_hint),
                kbd("Esc"),
                text(t!("quick_switcher.hint_close"))
                    .size(TEXT_CAPTION)
                    .style(style::text_hint),
            ]
            .spacing(SPACE_1)
            .align_y(Alignment::Center),
        );
        dialog(
            t!("quick_switcher.title"),
            body,
            Vec::new(),
            Some(Message::Close),
        )
    }

    fn row<'a>(&'a self, i: usize, item: &Item, ctx: &ViewCtx<'a>) -> Element<'a, Message> {
        let (av, name, kind_icon, hint): (Avatar, String, Icon, Option<String>) = match item {
            Item::Local {
                name, kind, avatar, ..
            } => (avatar.clone(), name.clone(), kind_icon(*kind), None),
            Item::User { username, name } => (
                Avatar::User {
                    username: username.clone(),
                },
                if name == username {
                    name.clone()
                } else {
                    format!("{name} (@{username})")
                },
                Icon::AtSign,
                Some(t!("quick_switcher.start_dm").into_owned()),
            ),
            Item::Room { rid, name, private } => (
                Avatar::Room {
                    rid: rid.clone(),
                    etag: None,
                },
                name.clone(),
                if *private { Icon::Lock } else { Icon::Hash },
                Some(t!("quick_switcher.not_joined").into_owned()),
            ),
        };
        let src = ctx.images.avatar(ctx.server_id, ctx.server, &av, &name);
        let mut line = row![
            avatar(src, 24.0, AvatarShape::Rounded),
            icon::sized(kind_icon, 14.0).style(style::text_secondary),
            text(name)
                .size(TEXT_BODY)
                .wrapping(iced::widget::text::Wrapping::None)
                .width(Length::Fill),
        ]
        .spacing(SPACE_2)
        .align_y(Alignment::Center);
        if let Some(h) = hint {
            line = line.push(text(h).size(TEXT_CAPTION).style(style::text_hint));
        }
        if let Item::Local {
            unread, mention, ..
        } = item
            && *unread > 0
        {
            line = line.push(badge(
                crate::format::count_u32(*unread),
                if *mention {
                    BadgeKind::Mention
                } else {
                    BadgeKind::Unread
                },
            ));
        }
        button(line)
            .width(Length::Fill)
            .padding([6, 8])
            .style(style::button_list_item(i == self.selected))
            .on_press(Message::Open(i))
            .into()
    }
}

fn kind_icon(k: RoomKind) -> Icon {
    match k {
        RoomKind::Channel => Icon::Hash,
        RoomKind::Private => Icon::Lock,
        RoomKind::Direct => Icon::AtSign,
        RoomKind::GroupDirect => Icon::Users,
        RoomKind::Discussion => Icon::MessagesSquare,
        RoomKind::Team { .. } => Icon::Users,
        RoomKind::Omnichannel => Icon::Headset,
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use rocketchat::models::{Room, RoomSummary, Subscription, User, UserRef, UserSummary};
    use rustket_core::Change;

    fn add(ws: &mut Workspace, rid: &str, name: &str, t: RoomType, unread: u64) {
        ws.apply_room(
            Change::Updated,
            Room {
                id: rid.into(),
                t,
                name: Some(name.into()),
                ..Default::default()
            },
        );
        ws.apply_subscription(
            Change::Updated,
            Subscription {
                id: format!("s-{rid}"),
                rid: rid.into(),
                name: name.into(),
                t,
                open: true,
                unread,
                alert: unread > 0,
                u: UserRef {
                    id: "me".into(),
                    username: Some("me".into()),
                    name: None,
                },
                ..Default::default()
            },
        );
    }

    fn ws() -> Workspace {
        let mut ws = Workspace::new();
        ws.apply_me(User {
            id: "me".into(),
            username: Some("me".into()),
            ..Default::default()
        });
        add(&mut ws, "r1", "dev-public", RoomType::Channel, 0);
        add(&mut ws, "r2", "random", RoomType::Channel, 0);
        add(&mut ws, "r3", "devops", RoomType::Private, 3);
        add(&mut ws, "d1", "bob", RoomType::Direct, 0);
        ws
    }

    fn rids(items: &[Item]) -> Vec<String> {
        items
            .iter()
            .map(|i| match i {
                Item::Local { rid, .. } | Item::Room { rid, .. } => rid.clone(),
                Item::User { username, .. } => format!("@{username}"),
            })
            .collect()
    }

    #[test]
    fn fuzzy_scores() {
        assert!(score("dev", "dev") > score("dev", "devops"));
        assert!(score("dev", "devops") > score("dev", "my-dev"));
        assert!(score("dev", "my-dev") > score("dev", "mydev"));
        assert!(score("dev", "mydev") > score("dev", "d-e-v"));
        assert!(score("dpb", "dev-public") > 0);
        assert_eq!(score("xyz", "dev-public"), 0);
        assert_eq!(score("", "anything"), 1);
    }

    #[test]
    fn unread_rooms_come_first() {
        let ws = ws();
        let opts = RoomListOptions::default();
        let r = results(&ws, &opts, "dev", None);
        assert_eq!(rids(&r), vec!["r3", "r1"]);
        let r = results(&ws, &opts, "#rand", None);
        assert_eq!(rids(&r), vec!["r2"]);
        let all = results(&ws, &opts, "", None);
        assert_eq!(all.len(), 4);
        assert_eq!(rids(&all)[0], "r3");
    }

    #[test]
    fn spotlight_adds_people_and_rooms_not_in_the_sidebar() {
        let ws = ws();
        let remote = SpotlightResult {
            users: vec![
                UserSummary {
                    id: "u-bob".into(),
                    username: Some("bob".into()),
                    ..Default::default()
                },
                UserSummary {
                    id: "u-me".into(),
                    username: Some("me".into()),
                    ..Default::default()
                },
                UserSummary {
                    id: "u-dev".into(),
                    username: Some("devon".into()),
                    name: Some("Devon".into()),
                    ..Default::default()
                },
            ],
            rooms: vec![
                RoomSummary {
                    id: "r1".into(),
                    t: RoomType::Channel,
                    name: Some("dev-public".into()),
                    ..Default::default()
                },
                RoomSummary {
                    id: "r9".into(),
                    t: RoomType::Channel,
                    name: Some("dev-other".into()),
                    ..Default::default()
                },
            ],
        };
        let r = results(&ws, &RoomListOptions::default(), "dev", Some(&remote));
        assert_eq!(rids(&r), vec!["r3", "r1", "@devon", "r9"]);
        assert_eq!(r[3].target(), Target::Room("r9".into()));
        assert_eq!(r[2].target(), Target::User("devon".into()));
    }

    fn with_ctx<R>(ws: &mut Workspace, f: impl FnOnce(&mut Ctx<'_>) -> R) -> R {
        let sid = rustket_core::ServerId("s".into());
        let url = rocketchat::ServerUrl::parse("http://localhost:3000").unwrap();
        let images = crate::images::Images::new(None);
        let config = rustket_core::AppConfig::default();
        let mut ctx = Ctx {
            server_id: &sid,
            server: &url,
            workspace: ws,
            session: None,
            images: &images,
            config: &config,
        };
        f(&mut ctx)
    }

    fn spotlight_with(username: &str) -> SpotlightResult {
        SpotlightResult {
            users: vec![UserSummary {
                id: format!("u-{username}"),
                username: Some(username.into()),
                ..Default::default()
            }],
            rooms: vec![],
        }
    }

    #[tokio::test]
    async fn stale_spotlight_responses_are_dropped() {
        let mut ws = ws();
        let mut qs = QuickSwitcher::new();
        with_ctx(&mut ws, |ctx| {
            let _ = qs.update(Message::Query("de".into()), ctx);
            let first = qs.seq;
            let _ = qs.update(Message::Query("dev".into()), ctx);
            assert!(qs.searching);
            // The answer for "de" arrives after "dev" was typed: dropped.
            let _ = qs.update(
                Message::Spotlight(first, "de".into(), Ok(spotlight_with("deirdre"))),
                ctx,
            );
            assert!(qs.remote.is_none() && qs.searching);
            // A debounce timer of an older query does nothing.
            assert!(qs.update(Message::Fire(first), ctx).effects.is_empty());
            let seq = qs.seq;
            let _ = qs.update(
                Message::Spotlight(seq, "dev".into(), Ok(spotlight_with("devon"))),
                ctx,
            );
            assert!(!qs.searching);
            let items = qs.items(ctx.workspace, &ctx.config.ui);
            assert!(
                items
                    .iter()
                    .any(|i| matches!(i, Item::User { username, .. } if username == "devon"))
            );
        });
    }

    #[tokio::test]
    async fn keyboard_selection_and_open() {
        let mut ws = ws();
        let mut qs = QuickSwitcher::new();
        with_ctx(&mut ws, |ctx| {
            let _ = qs.update(Message::Query("dev".into()), ctx);
            let _ = qs.update(Message::Up, ctx);
            assert_eq!(qs.selected, 0);
            let _ = qs.update(Message::Down, ctx);
            let _ = qs.update(Message::Down, ctx);
            assert_eq!(qs.selected, 1, "two local matches");
            let a = qs.update(Message::Submit, ctx);
            assert!(matches!(&a.effects[..], [Effect::OpenRoom(r), Effect::Close] if r == "r1"));
            // A user without a DM needs `im.create` first (a task, no effect yet).
            qs.remote = Some(("dev".into(), spotlight_with("devon")));
            let n = qs.items(ctx.workspace, &ctx.config.ui).len();
            let a = qs.update(Message::Open(n - 1), ctx);
            assert!(a.effects.is_empty() && qs.busy);
        });
    }
}
