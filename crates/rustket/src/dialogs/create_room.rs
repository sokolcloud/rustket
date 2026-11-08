//! Create a channel, private group, team, direct message (1:1 or group) or
//! discussion (`channels.create`, `groups.create`, `teams.create`,
//! `im.create`, `rooms.createDiscussion`), gated by the `create-*` /
//! `start-discussion` permissions; opens the new room on success.
//! Owner: account agent. Strings: `locales/create_room.yml` (`create_room.*`).

pub mod names;

use std::time::Duration;

use iced::widget::{button, column, container, row, text, toggler};
use iced::{Alignment, Element, Length, Subscription, Task};
use rocketchat::RestClient;
use rocketchat::models::{RoomSummary, RoomType, UserSummary};
use rocketchat::rest::{CreateRoom as CreateParams, Query};
use rust_i18n::t;
use rustket_core::Workspace;
use rustket_core::permissions;
use rustket_core::room_list::Avatar;
use rustket_ui::icon::{self, Icon};
use rustket_ui::style;
use rustket_ui::theme::{SPACE_1, SPACE_2, SPACE_3, TEXT_BODY, TEXT_CAPTION, TEXT_SMALL};
use rustket_ui::widget::{
    AvatarShape, avatar, dialog, primary_button, secondary_button, text_field,
};
use serde_json::{Value, json};

use crate::component::{Action, Ctx, Effect, ViewCtx};
use crate::error::Res;
use crate::screens::directory::open::{self, Opened, Target};
use names::NameError;

const DEBOUNCE: Duration = Duration::from_millis(300);
const MAX_SUGGESTIONS: usize = 6;
pub const NAME_ID: &str = "create-room-name";

/// What to create.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Hash)]
pub enum Kind {
    #[default]
    Channel,
    Group,
    Team,
    DirectMessage,
    Discussion,
}

impl Kind {
    /// Kinds offered as tabs (`Group` is the channel tab with "private" on).
    pub const TABS: [Kind; 4] = [
        Kind::Channel,
        Kind::Team,
        Kind::DirectMessage,
        Kind::Discussion,
    ];

    fn tab(self) -> Kind {
        match self {
            Kind::Group => Kind::Channel,
            k => k,
        }
    }

    fn i18n_key(self) -> &'static str {
        match self {
            Kind::Channel | Kind::Group => "create_room.kind_channel",
            Kind::Team => "create_room.kind_team",
            Kind::DirectMessage => "create_room.kind_dm",
            Kind::Discussion => "create_room.kind_discussion",
        }
    }

    fn help_key(self) -> &'static str {
        match self {
            Kind::Channel => "create_room.help_channel",
            Kind::Group => "create_room.help_group",
            Kind::Team => "create_room.help_team",
            Kind::DirectMessage => "create_room.help_dm",
            Kind::Discussion => "create_room.help_discussion",
        }
    }

    fn icon(self) -> Icon {
        match self {
            Kind::Channel => Icon::Hash,
            Kind::Group => Icon::Lock,
            Kind::Team => Icon::Users,
            Kind::DirectMessage => Icon::AtSign,
            Kind::Discussion => Icon::MessagesSquare,
        }
    }
}

/// Whether the user may create `kind`.
pub fn allowed(ws: &Workspace, kind: Kind) -> bool {
    match kind {
        Kind::Channel => permissions::can_create_channel(ws),
        Kind::Group => permissions::can_create_group(ws),
        Kind::Team => permissions::can_create_team(ws),
        Kind::DirectMessage => permissions::can_create_dm(ws),
        Kind::Discussion => permissions::can_create_discussion(ws, None),
    }
}

/// A tab is offered when any of its kinds is allowed.
fn tab_allowed(ws: &Workspace, tab: Kind) -> bool {
    match tab {
        Kind::Channel | Kind::Group => allowed(ws, Kind::Channel) || allowed(ws, Kind::Group),
        k => allowed(ws, k),
    }
}

#[derive(Debug, Clone)]
pub enum Message {
    Kind(Kind),
    Name(String),
    Topic(String),
    Private(bool),
    ReadOnly(bool),
    Broadcast(bool),
    FirstMessage(String),
    CheckName(u64),
    NameChecked(u64, String, Res<bool>),
    MemberQuery(String),
    SearchMembers(u64),
    Members(u64, Res<Vec<UserSummary>>),
    AddMember(String),
    RemoveMember(String),
    ParentQuery(String),
    SearchParents(u64),
    Parents(u64, Res<Vec<RoomSummary>>),
    Parent(String, String),
    ClearParent,
    Submit,
    Created(Box<Res<Opened>>),
    Close,
}

#[derive(Debug, Default)]
pub struct CreateRoom {
    pub kind: Kind,
    pub name: String,
    pub topic: String,
    pub read_only: bool,
    pub broadcast: bool,
    pub team_private: bool,
    pub first_message: String,
    pub name_seq: u64,
    /// `rooms.nameExists` answer for the current name.
    pub name_taken: Option<bool>,
    pub members: Vec<String>,
    pub member_query: String,
    pub member_seq: u64,
    pub suggestions: Vec<UserSummary>,
    /// Parent room of a discussion: (rid, name).
    pub parent: Option<(String, String)>,
    pub parent_query: String,
    pub parent_seq: u64,
    pub parent_suggestions: Vec<RoomSummary>,
    /// Opened from a room (header, panel, message action): a discussion
    /// only, no kind tabs.
    pub from_room: bool,
    /// Message the discussion starts from (`pmid`) and its excerpt.
    pub pmid: Option<(String, String)>,
    pub busy: bool,
    pub error: Option<String>,
}

impl CreateRoom {
    pub fn new(kind: Kind) -> Self {
        Self {
            kind,
            ..Default::default()
        }
    }

    /// Opens a discussion dialog with a preset parent room.
    pub fn discussion_in(rid: &str, name: &str) -> Self {
        Self {
            kind: Kind::Discussion,
            parent: Some((rid.to_owned(), name.to_owned())),
            from_room: true,
            ..Default::default()
        }
    }

    /// A discussion started from message `pmid` (`excerpt` is shown and,
    /// as in the web client, its first 140 characters prefill the name).
    pub fn from_message(mut self, pmid: &str, excerpt: &str) -> Self {
        self.pmid = Some((pmid.to_owned(), excerpt.to_owned()));
        if self.name.is_empty() {
            self.name = excerpt
                .chars()
                .take(140)
                .collect::<String>()
                .trim()
                .to_owned();
        }
        self
    }

    /// The kind actually used: the requested one if allowed, else the first
    /// allowed tab.
    pub fn effective_kind(&self, ws: &Workspace) -> Option<Kind> {
        // Opened from a room: a discussion in that room or nothing (the
        // permission may be granted through room roles only).
        if self.from_room {
            let ok = match &self.parent {
                Some((rid, _)) => can_discuss_in(ws, rid),
                None => allowed(ws, Kind::Discussion),
            };
            return ok.then_some(Kind::Discussion);
        }
        if allowed(ws, self.kind) {
            return Some(self.kind);
        }
        if self.kind == Kind::Channel && allowed(ws, Kind::Group) {
            return Some(Kind::Group);
        }
        if self.kind == Kind::Group && allowed(ws, Kind::Channel) {
            return Some(Kind::Channel);
        }
        Kind::TABS
            .into_iter()
            .chain([Kind::Group])
            .find(|k| allowed(ws, *k))
    }

    fn named(kind: Kind) -> bool {
        matches!(
            kind,
            Kind::Channel | Kind::Group | Kind::Team | Kind::Discussion
        )
    }

    /// Validation error of the name field (translated).
    pub fn name_error(&self, ws: &Workspace, kind: Kind) -> Option<String> {
        if self.name.is_empty() {
            return None;
        }
        if kind == Kind::Discussion {
            return None;
        }
        match names::check_room_name(ws, &self.name) {
            Err(NameError::Empty) => Some(t!("create_room.err_name_empty").into_owned()),
            Err(NameError::Invalid) => Some(t!("create_room.err_name_invalid").into_owned()),
            Ok(()) if self.name_taken == Some(true) => {
                Some(t!("create_room.err_name_taken").into_owned())
            }
            Ok(()) => None,
        }
    }

    /// Whether the form can be submitted.
    pub fn ready(&self, ws: &Workspace) -> bool {
        let Some(kind) = self.effective_kind(ws) else {
            return false;
        };
        if self.busy {
            return false;
        }
        match kind {
            Kind::DirectMessage => !self.members.is_empty(),
            Kind::Discussion => {
                !self.name.trim().is_empty()
                    && self
                        .parent
                        .as_ref()
                        .is_some_and(|(rid, _)| can_discuss_in(ws, rid))
            }
            _ => !self.name.trim().is_empty() && self.name_error(ws, kind).is_none(),
        }
    }

    pub fn update(&mut self, msg: Message, ctx: &mut Ctx<'_>) -> Action<Message> {
        match msg {
            Message::Kind(k) => {
                let k = if k == Kind::Channel
                    && !allowed(ctx.workspace, Kind::Channel)
                    && allowed(ctx.workspace, Kind::Group)
                {
                    Kind::Group
                } else {
                    k
                };
                let current = self.effective_kind(ctx.workspace).unwrap_or(self.kind);
                if k.tab() != current.tab() {
                    self.kind = k;
                    self.error = None;
                    self.name_taken = None;
                    self.suggestions.clear();
                    self.member_query.clear();
                }
                Action::none()
            }
            Message::Private(p) => {
                if self.effective_kind(ctx.workspace) == Some(Kind::Team) {
                    self.kind = Kind::Team;
                    self.team_private = p;
                } else {
                    let want = if p { Kind::Group } else { Kind::Channel };
                    if allowed(ctx.workspace, want) {
                        self.kind = want;
                    }
                }
                Action::none()
            }
            Message::Name(s) => {
                self.name = s;
                self.name_taken = None;
                self.name_seq += 1;
                let kind = self.effective_kind(ctx.workspace);
                if kind == Some(Kind::Discussion)
                    || names::check_room_name(ctx.workspace, &self.name).is_err()
                {
                    return Action::none();
                }
                let seq = self.name_seq;
                Action::task(Task::perform(tokio::time::sleep(DEBOUNCE), move |_| {
                    Message::CheckName(seq)
                }))
            }
            Message::CheckName(seq) => {
                if seq != self.name_seq {
                    return Action::none();
                }
                let name = self.name.trim().to_owned();
                let n2 = name.clone();
                Action::task(ctx.rest_task(
                    move |rest| async move { name_exists(&rest, &name).await },
                    move |r| Message::NameChecked(seq, n2, r),
                ))
            }
            Message::NameChecked(seq, name, r) => {
                if seq == self.name_seq && name == self.name.trim() {
                    self.name_taken = r.ok();
                }
                Action::none()
            }
            Message::Topic(s) => {
                self.topic = s;
                Action::none()
            }
            Message::ReadOnly(b) => {
                self.read_only = b;
                Action::none()
            }
            Message::Broadcast(b) => {
                self.broadcast = b;
                if b {
                    self.read_only = true;
                }
                Action::none()
            }
            Message::FirstMessage(s) => {
                self.first_message = s;
                Action::none()
            }
            Message::MemberQuery(q) => {
                self.member_query = q;
                self.member_seq += 1;
                if self.member_query.trim().is_empty() {
                    self.suggestions.clear();
                    return Action::none();
                }
                let seq = self.member_seq;
                Action::task(Task::perform(tokio::time::sleep(DEBOUNCE), move |_| {
                    Message::SearchMembers(seq)
                }))
            }
            Message::SearchMembers(seq) => {
                if seq != self.member_seq {
                    return Action::none();
                }
                let term = self.member_query.trim().trim_start_matches('@').to_owned();
                Action::task(ctx.rest_task(
                    move |rest| async move { rest.users_autocomplete(&term).await },
                    move |r| Message::Members(seq, r),
                ))
            }
            Message::Members(seq, r) => {
                if seq == self.member_seq {
                    let me = ctx.workspace.username().unwrap_or_default().to_owned();
                    self.suggestions = r
                        .unwrap_or_default()
                        .into_iter()
                        .filter(|u| {
                            u.username
                                .as_deref()
                                .is_some_and(|n| n != me && !self.members.iter().any(|m| m == n))
                        })
                        .take(MAX_SUGGESTIONS)
                        .collect();
                }
                Action::none()
            }
            Message::AddMember(u) => {
                if !self.members.contains(&u) {
                    self.members.push(u);
                }
                self.member_query.clear();
                self.suggestions.clear();
                self.member_seq += 1;
                Action::none()
            }
            Message::RemoveMember(u) => {
                self.members.retain(|m| *m != u);
                Action::none()
            }
            Message::ParentQuery(q) => {
                self.parent_query = q;
                self.parent_seq += 1;
                if self.parent_query.trim().is_empty() {
                    self.parent_suggestions.clear();
                    return Action::none();
                }
                let seq = self.parent_seq;
                Action::task(Task::perform(tokio::time::sleep(DEBOUNCE), move |_| {
                    Message::SearchParents(seq)
                }))
            }
            Message::SearchParents(seq) => {
                if seq != self.parent_seq {
                    return Action::none();
                }
                let term = self.parent_query.trim().trim_start_matches('#').to_owned();
                Action::task(
                    ctx.rest_task(
                        move |rest| async move {
                            rest.rooms_autocomplete_channel_and_private(&term).await
                        },
                        move |r| Message::Parents(seq, r),
                    ),
                )
            }
            Message::Parents(seq, r) => {
                if seq == self.parent_seq {
                    let ws = &*ctx.workspace;
                    self.parent_suggestions = r
                        .unwrap_or_default()
                        .into_iter()
                        .filter(|room| room.prid.is_none() && can_discuss_in(ws, &room.id))
                        .take(MAX_SUGGESTIONS)
                        .collect();
                }
                Action::none()
            }
            Message::Parent(rid, name) => {
                self.parent = Some((rid, name));
                self.parent_query.clear();
                self.parent_suggestions.clear();
                self.parent_seq += 1;
                Action::none()
            }
            Message::ClearParent => {
                // The parent is locked when opened from a room.
                if self.from_room {
                    return Action::none();
                }
                self.parent = None;
                Action::none()
            }
            Message::Submit => {
                if !self.ready(ctx.workspace) {
                    return Action::none();
                }
                let Some(kind) = self.effective_kind(ctx.workspace) else {
                    return Action::none();
                };
                self.busy = true;
                self.error = None;
                let req = self.submit_request(kind, ctx.workspace);
                Action::task(ctx.rest_task(
                    move |rest| create(rest, req),
                    |r| Message::Created(Box::new(r)),
                ))
            }
            Message::Created(r) => {
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

    /// The discussion parent is end-to-end encrypted (the discussion inherits
    /// it and cannot get a first message).
    pub fn parent_encrypted(&self, ws: &Workspace) -> bool {
        self.parent
            .as_ref()
            .and_then(|(rid, _)| ws.room(rid))
            .is_some_and(|e| e.is_encrypted())
    }

    /// [`Self::request`] adjusted to the workspace: no first message in a
    /// discussion of an encrypted room (the server refuses it).
    pub fn submit_request(&self, kind: Kind, ws: &Workspace) -> Request {
        let mut req = self.request(kind);
        if let Request::Discussion { reply, .. } = &mut req
            && self.parent_encrypted(ws)
        {
            *reply = None;
        }
        req
    }

    /// The request for the current form.
    pub fn request(&self, kind: Kind) -> Request {
        let name = self.name.trim().to_owned();
        let topic = Some(self.topic.trim().to_owned()).filter(|t| !t.is_empty());
        match kind {
            Kind::Channel | Kind::Group => Request::Room {
                private: kind == Kind::Group,
                params: CreateParams {
                    name,
                    members: self.members.clone(),
                    read_only: self.read_only,
                    extra_data: Some(extra(topic, self.broadcast)),
                },
            },
            Kind::Team => Request::Team(json!({
                "name": name,
                "type": if self.team_private { 1 } else { 0 },
                "members": self.members,
                "room": { "readOnly": self.read_only, "extraData": extra(topic, self.broadcast) },
            })),
            Kind::DirectMessage => Request::Direct(self.members.clone()),
            Kind::Discussion => Request::Discussion {
                prid: self
                    .parent
                    .as_ref()
                    .map(|p| p.0.clone())
                    .unwrap_or_default(),
                name,
                reply: Some(self.first_message.trim().to_owned()).filter(|m| !m.is_empty()),
                users: self.members.clone(),
                pmid: self.pmid.as_ref().map(|p| p.0.clone()),
            },
        }
    }

    pub fn subscription(&self) -> Subscription<Message> {
        Subscription::none()
    }

    pub fn view<'a>(&'a self, ctx: &ViewCtx<'a>) -> Element<'a, Message> {
        let ws = ctx.workspace;
        let Some(kind) = self.effective_kind(ws) else {
            return dialog(
                t!("create_room.title"),
                text(t!("create_room.not_allowed")).size(TEXT_BODY),
                vec![secondary_button(t!("create_room.close"), Some(Message::Close)).into()],
                Some(Message::Close),
            );
        };
        let tabs = row(Kind::TABS
            .into_iter()
            .filter(|k| tab_allowed(ws, *k))
            .map(|k| {
                let active = k == kind.tab();
                button(
                    row![
                        icon::sized(k.icon(), 14.0),
                        text(t!(k.i18n_key()).into_owned()).size(TEXT_SMALL)
                    ]
                    .spacing(SPACE_1)
                    .align_y(Alignment::Center),
                )
                .padding([5, 10])
                .style(if active {
                    style::button_primary
                } else {
                    style::button_secondary
                })
                .on_press(Message::Kind(k))
                .into()
            }))
        .spacing(SPACE_1);
        let mut body = column![].spacing(SPACE_3);
        if !self.from_room {
            body = body.push(tabs);
        }
        body = body.push(
            text(t!(kind.help_key()))
                .size(TEXT_SMALL)
                .style(style::text_secondary),
        );

        if kind == Kind::Discussion {
            body = body.push(self.parent_picker(ctx));
            if let Some((_, excerpt)) = &self.pmid {
                body = body.push(
                    column![
                        text(t!("create_room.from_message")).size(TEXT_CAPTION),
                        container(
                            text(excerpt.as_str())
                                .size(TEXT_SMALL)
                                .style(style::text_secondary)
                        )
                        .padding([SPACE_1, SPACE_2])
                        .width(Length::Fill)
                        .style(style::container_card),
                    ]
                    .spacing(SPACE_1),
                );
            }
        }
        if Self::named(kind) {
            let label = if kind == Kind::Discussion {
                t!("create_room.discussion_name")
            } else {
                t!("create_room.name")
            };
            let mut f = text_field(label, &self.name, Message::Name)
                .id(NAME_ID)
                .placeholder(t!("create_room.name_placeholder"))
                .error(self.name_error(ws, kind))
                .on_submit(Message::Submit);
            if kind != Kind::Discussion && self.name_taken == Some(false) {
                f = f.help(t!("create_room.name_free"));
            }
            body = body.push(f);
        }
        if matches!(kind, Kind::Channel | Kind::Group | Kind::Team) {
            body = body.push(
                text_field(t!("create_room.topic"), &self.topic, Message::Topic)
                    .placeholder(t!("create_room.topic_placeholder")),
            );
        }
        body = body.push(self.member_picker(ctx, kind));
        if kind == Kind::Discussion {
            let encrypted = self.parent_encrypted(ws);
            let mut f = text_field(
                t!("create_room.first_message"),
                if encrypted { "" } else { &self.first_message },
                Message::FirstMessage,
            )
            .placeholder(t!("create_room.first_message_placeholder"))
            .disabled(encrypted);
            if encrypted {
                f = f.help(t!("create_room.first_message_encrypted"));
            }
            body = body.push(f);
        }
        if matches!(kind, Kind::Channel | Kind::Group | Kind::Team) {
            let private = match kind {
                Kind::Team => self.team_private,
                k => k == Kind::Group,
            };
            let can_toggle_private =
                kind == Kind::Team || (allowed(ws, Kind::Channel) && allowed(ws, Kind::Group));
            body = body.push(toggle_row(
                t!("create_room.private").into_owned(),
                t!("create_room.private_help").into_owned(),
                private,
                can_toggle_private.then_some(Message::Private as fn(bool) -> Message),
            ));
            body = body.push(toggle_row(
                t!("create_room.read_only").into_owned(),
                t!("create_room.read_only_help").into_owned(),
                self.read_only,
                (!self.broadcast).then_some(Message::ReadOnly as fn(bool) -> Message),
            ));
            body = body.push(toggle_row(
                t!("create_room.broadcast").into_owned(),
                t!("create_room.broadcast_help").into_owned(),
                self.broadcast,
                Some(Message::Broadcast as fn(bool) -> Message),
            ));
        }
        if let Some(e) = &self.error {
            body = body.push(text(e.as_str()).size(TEXT_SMALL).style(style::text_danger));
        }
        let title = if self.from_room && kind == Kind::Discussion {
            t!("create_room.title_discussion")
        } else {
            t!("create_room.title")
        };
        dialog(
            title,
            body,
            vec![
                secondary_button(t!("create_room.cancel"), Some(Message::Close)).into(),
                primary_button(
                    t!("create_room.create"),
                    self.ready(ws).then_some(Message::Submit),
                )
                .loading(self.busy)
                .into(),
            ],
            Some(Message::Close),
        )
    }

    fn member_picker<'a>(&'a self, ctx: &ViewCtx<'a>, kind: Kind) -> Element<'a, Message> {
        let label = if kind == Kind::DirectMessage {
            t!("create_room.dm_users")
        } else {
            t!("create_room.members")
        };
        let mut col = column![
            text_field(label, &self.member_query, Message::MemberQuery)
                .placeholder(t!("create_room.members_placeholder"))
        ]
        .spacing(SPACE_1);
        if !self.suggestions.is_empty() {
            let list = column(self.suggestions.iter().filter_map(|u| {
                let username = u.username.clone()?;
                let name = u.name.clone().unwrap_or_else(|| username.clone());
                let src = ctx.images.avatar(
                    ctx.server_id,
                    ctx.server,
                    &Avatar::User {
                        username: username.clone(),
                    },
                    &name,
                );
                Some(
                    button(
                        row![
                            avatar(src, 20.0, AvatarShape::Rounded),
                            text(name).size(TEXT_SMALL),
                            text(format!("@{username}"))
                                .size(TEXT_CAPTION)
                                .style(style::text_secondary),
                        ]
                        .spacing(SPACE_2)
                        .align_y(Alignment::Center),
                    )
                    .width(Length::Fill)
                    .padding([4, 8])
                    .style(style::button_list_item(false))
                    .on_press(Message::AddMember(username))
                    .into(),
                )
            }));
            col = col.push(
                container(list)
                    .padding(SPACE_1)
                    .width(Length::Fill)
                    .style(style::container_popover),
            );
        }
        if !self.members.is_empty() {
            col = col.push(
                row(self.members.iter().map(|m| {
                    button(
                        row![
                            text(m.as_str()).size(TEXT_SMALL),
                            icon::sized(Icon::X, 12.0)
                        ]
                        .spacing(SPACE_1)
                        .align_y(Alignment::Center),
                    )
                    .padding([3, 8])
                    .style(style::button_secondary)
                    .on_press(Message::RemoveMember(m.clone()))
                    .into()
                }))
                .spacing(SPACE_1)
                .wrap(),
            );
        }
        col.into()
    }

    fn parent_picker<'a>(&'a self, ctx: &ViewCtx<'a>) -> Element<'a, Message> {
        if let Some((rid, name)) = &self.parent {
            let ok = can_discuss_in(ctx.workspace, rid);
            let mut line = row![
                icon::sized(parent_icon(ctx.workspace, rid), 14.0).style(style::text_secondary),
                text(name.as_str()).size(TEXT_BODY).width(Length::Fill),
            ]
            .spacing(SPACE_2)
            .align_y(Alignment::Center);
            if !self.from_room {
                line = line.push(
                    button(icon::sized(Icon::X, 14.0))
                        .style(style::button_icon)
                        .on_press(Message::ClearParent),
                );
            }
            let mut col = column![text(t!("create_room.parent")).size(TEXT_BODY - 1.0), line]
                .spacing(SPACE_1);
            if !ok {
                col = col.push(
                    text(t!("create_room.parent_not_allowed"))
                        .size(TEXT_CAPTION)
                        .style(style::text_danger),
                );
            }
            return col.into();
        }
        let mut col = column![
            text_field(
                t!("create_room.parent"),
                &self.parent_query,
                Message::ParentQuery
            )
            .placeholder(t!("create_room.parent_placeholder"))
        ]
        .spacing(SPACE_1);
        if !self.parent_suggestions.is_empty() {
            let list = column(self.parent_suggestions.iter().map(|r| {
                let name = r
                    .fname
                    .clone()
                    .or_else(|| r.name.clone())
                    .unwrap_or_else(|| r.id.clone());
                button(
                    row![
                        icon::sized(
                            if r.t == RoomType::Private {
                                Icon::Lock
                            } else {
                                Icon::Hash
                            },
                            14.0
                        ),
                        text(name.clone()).size(TEXT_SMALL)
                    ]
                    .spacing(SPACE_2)
                    .align_y(Alignment::Center),
                )
                .width(Length::Fill)
                .padding([4, 8])
                .style(style::button_list_item(false))
                .on_press(Message::Parent(r.id.clone(), name))
                .into()
            }));
            col = col.push(
                container(list)
                    .padding(SPACE_1)
                    .width(Length::Fill)
                    .style(style::container_popover),
            );
        }
        col.into()
    }
}

/// Icon of a known parent room (`#` when unknown).
fn parent_icon(ws: &Workspace, rid: &str) -> Icon {
    match ws.room(rid) {
        Some(e) if e.is_encrypted() => Icon::Lock,
        Some(e) if e.room_type() == RoomType::Private => Icon::Lock,
        _ => Icon::Hash,
    }
}

/// `start-discussion` in the parent room (known rooms only; unknown rooms are
/// left to the server).
fn can_discuss_in(ws: &Workspace, rid: &str) -> bool {
    ws.room(rid).is_none() || permissions::can_create_discussion(ws, Some(rid))
}

fn extra(topic: Option<String>, broadcast: bool) -> Value {
    let mut m = serde_json::Map::new();
    if let Some(t) = topic {
        m.insert("topic".into(), Value::String(t));
    }
    if broadcast {
        m.insert("broadcast".into(), Value::Bool(true));
    }
    Value::Object(m)
}

fn toggle_row<'a>(
    label: String,
    help: String,
    value: bool,
    on: Option<fn(bool) -> Message>,
) -> Element<'a, Message> {
    row![
        column![
            text(label).size(TEXT_BODY),
            text(help).size(TEXT_CAPTION).style(style::text_hint)
        ]
        .spacing(2)
        .width(Length::Fill),
        toggler(value).on_toggle_maybe(on).style(style::toggler),
    ]
    .spacing(SPACE_2)
    .align_y(Alignment::Center)
    .into()
}

/// A creation request (pure data; see [`create`]).
#[derive(Debug, Clone)]
pub enum Request {
    Room {
        private: bool,
        params: CreateParams,
    },
    Team(Value),
    Direct(Vec<String>),
    Discussion {
        prid: String,
        name: String,
        reply: Option<String>,
        users: Vec<String>,
        /// Parent message (`pmid`).
        pmid: Option<String>,
    },
}

/// Creates the room and fetches it (with the subscription) for the workspace.
pub async fn create(rest: RestClient, req: Request) -> rocketchat::Result<Opened> {
    let rid = match req {
        Request::Room { private, params } => {
            let room = if private {
                rest.groups_create(&params).await?
            } else {
                rest.channels_create(&params).await?
            };
            room.id
        }
        Request::Team(body) => {
            let v: Value = rest.post("teams.create", &body).await?;
            v.pointer("/team/roomId")
                .and_then(Value::as_str)
                .map(str::to_owned)
                .ok_or_else(|| rocketchat::Error::Protocol("missing team.roomId".into()))?
        }
        Request::Direct(users) => return open::resolve(rest, Target::Users(users)).await,
        Request::Discussion {
            prid,
            name,
            reply,
            users,
            pmid,
        } => {
            let refs: Vec<&str> = users.iter().map(String::as_str).collect();
            rest.rooms_create_discussion_from(
                &prid,
                pmid.as_deref(),
                &name,
                reply.as_deref(),
                &refs,
            )
            .await?
            .id
        }
    };
    open::resolve(rest, Target::Room(rid)).await
}

/// `rooms.nameExists`.
pub async fn name_exists(rest: &RestClient, name: &str) -> rocketchat::Result<bool> {
    let v: Value = rest
        .get("rooms.nameExists", &Query::new().push("roomName", name))
        .await?;
    Ok(v.get("exists").and_then(Value::as_bool).unwrap_or(false))
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use rocketchat::models::{Permission, Setting, User};
    use rustket_core::Change;

    fn ws(perms: &[&str]) -> Workspace {
        let mut ws = Workspace::new();
        ws.apply_me(User {
            id: "me".into(),
            username: Some("me".into()),
            roles: Some(vec!["user".into()]),
            ..Default::default()
        });
        for p in perms {
            ws.apply_permission(
                Change::Inserted,
                Permission {
                    id: (*p).into(),
                    roles: vec!["user".into()],
                    updated_at: None,
                },
            );
        }
        ws.apply_setting(
            Change::Inserted,
            Setting {
                id: "Discussion_enabled".into(),
                value: json!(true),
                ..Default::default()
            },
        );
        ws
    }

    #[test]
    fn kinds_are_gated_by_permissions() {
        let w = ws(&["create-p", "create-d"]);
        assert_eq!(
            CreateRoom::new(Kind::Channel).effective_kind(&w),
            Some(Kind::Group)
        );
        assert_eq!(
            CreateRoom::new(Kind::Team).effective_kind(&w),
            Some(Kind::DirectMessage)
        );
        assert!(tab_allowed(&w, Kind::Channel));
        assert!(!tab_allowed(&w, Kind::Team));
        assert!(!tab_allowed(&w, Kind::Discussion));
        assert_eq!(
            CreateRoom::new(Kind::Channel).effective_kind(&ws(&[])),
            None
        );
        let w = ws(&["start-discussion"]);
        assert_eq!(
            CreateRoom::new(Kind::Discussion).effective_kind(&w),
            Some(Kind::Discussion)
        );
    }

    #[test]
    fn readiness_and_validation() {
        let w = ws(&[
            "create-c",
            "create-p",
            "create-d",
            "create-team",
            "start-discussion",
        ]);
        let mut c = CreateRoom::new(Kind::Channel);
        assert!(!c.ready(&w));
        c.name = "bad name".into();
        assert!(c.name_error(&w, Kind::Channel).is_some());
        assert!(!c.ready(&w));
        c.name = "good-name".into();
        assert!(c.ready(&w));
        c.name_taken = Some(true);
        assert!(!c.ready(&w));

        let mut d = CreateRoom::new(Kind::DirectMessage);
        assert!(!d.ready(&w));
        d.members.push("alice".into());
        assert!(d.ready(&w));

        let mut disc = CreateRoom::new(Kind::Discussion);
        disc.name = "Any name with spaces".into();
        assert!(disc.name_error(&w, Kind::Discussion).is_none());
        assert!(!disc.ready(&w), "needs a parent");
        disc.parent = Some(("unknown-room".into(), "x".into()));
        assert!(disc.ready(&w));
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

    #[test]
    fn private_toggle_follows_the_effective_kind() {
        // Only teams allowed: the channel dialog falls back to the team tab,
        // and its Private toggle sets the team type.
        let mut w = ws(&["create-team"]);
        let mut c = CreateRoom::new(Kind::Channel);
        with_ctx(&mut w, |ctx| {
            assert_eq!(c.effective_kind(ctx.workspace), Some(Kind::Team));
            let _ = c.update(Message::Private(true), ctx);
        });
        assert!(c.team_private);
        c.name = "rk-team".into();
        assert!(matches!(c.request(Kind::Team), Request::Team(v) if v["type"] == 1));
    }

    #[test]
    fn from_room_is_a_discussion_or_nothing() {
        use rocketchat::models::{Room, Subscription};
        // start-discussion only through the room's owner role.
        let mut w = ws(&["create-c"]);
        w.apply_permission(
            Change::Inserted,
            Permission {
                id: "start-discussion".into(),
                roles: vec!["owner".into()],
                updated_at: None,
            },
        );
        w.apply_room(
            Change::Inserted,
            Room {
                id: "R".into(),
                t: RoomType::Channel,
                ..Default::default()
            },
        );
        w.apply_subscription(
            Change::Inserted,
            Subscription {
                id: "sR".into(),
                rid: "R".into(),
                roles: Some(vec!["owner".into()]),
                ..Default::default()
            },
        );
        let mut c = CreateRoom::discussion_in("R", "room");
        assert_eq!(c.effective_kind(&w), Some(Kind::Discussion));
        assert_eq!(
            CreateRoom::new(Kind::Discussion).effective_kind(&w),
            Some(Kind::Channel)
        );
        // Without the room role: no fallback to a channel.
        w.apply_subscription(
            Change::Updated,
            Subscription {
                id: "sR".into(),
                rid: "R".into(),
                ..Default::default()
            },
        );
        assert_eq!(c.effective_kind(&w), None);
        assert!(!c.ready(&w));
        // The parent is locked.
        with_ctx(&mut w, |ctx| {
            let _ = c.update(Message::ClearParent, ctx);
        });
        assert_eq!(c.parent.as_ref().map(|p| p.0.as_str()), Some("R"));
    }

    #[test]
    fn from_message_prefills_the_name() {
        let long = "x".repeat(300);
        let c = CreateRoom::discussion_in("R", "room").from_message("m", &long);
        assert_eq!(c.name.chars().count(), 140);
        assert_eq!(c.pmid.as_ref().map(|p| p.0.as_str()), Some("m"));
    }

    #[test]
    fn encrypted_parent_has_no_first_message() {
        let mut w = ws(&["start-discussion"]);
        w.apply_room(
            Change::Inserted,
            rocketchat::models::Room {
                id: "E".into(),
                t: RoomType::Private,
                encrypted: Some(true),
                ..Default::default()
            },
        );
        let mut c = CreateRoom::discussion_in("E", "secret");
        c.name = "disc".into();
        c.first_message = "hello".into();
        assert!(c.parent_encrypted(&w));
        assert!(matches!(parent_icon(&w, "E"), Icon::Lock));
        assert!(matches!(parent_icon(&w, "unknown"), Icon::Hash));
        assert!(matches!(
            c.submit_request(Kind::Discussion, &w),
            Request::Discussion { reply: None, .. }
        ));
        w.apply_room(
            Change::Updated,
            rocketchat::models::Room {
                id: "E".into(),
                t: RoomType::Private,
                ..Default::default()
            },
        );
        assert!(matches!(
            c.submit_request(Kind::Discussion, &w),
            Request::Discussion { reply: Some(_), .. }
        ));
    }

    #[test]
    fn requests_carry_the_form() {
        let mut c = CreateRoom::new(Kind::Group);
        c.name = " rk-room ".into();
        c.topic = "Topic".into();
        c.members = vec!["alice".into()];
        c.broadcast = true;
        c.read_only = true;
        match c.request(Kind::Group) {
            Request::Room { private, params } => {
                assert!(private);
                let v = serde_json::to_value(&params).unwrap();
                assert_eq!(
                    v,
                    json!({"name": "rk-room", "members": ["alice"], "readOnly": true,
                           "extraData": {"topic": "Topic", "broadcast": true}})
                );
            }
            r => panic!("{r:?}"),
        }
        c.team_private = true;
        c.broadcast = false;
        c.read_only = false;
        match c.request(Kind::Team) {
            Request::Team(v) => assert_eq!(
                v,
                json!({"name": "rk-room", "type": 1, "members": ["alice"],
                       "room": {"readOnly": false, "extraData": {"topic": "Topic"}}})
            ),
            r => panic!("{r:?}"),
        }
        c.parent = Some(("P".into(), "parent".into()));
        c.first_message = " hi ".into();
        match c.request(Kind::Discussion) {
            Request::Discussion {
                prid, reply, users, ..
            } => {
                assert_eq!(prid, "P");
                assert_eq!(reply.as_deref(), Some("hi"));
                assert_eq!(users, vec!["alice"]);
            }
            r => panic!("{r:?}"),
        }
    }
}
