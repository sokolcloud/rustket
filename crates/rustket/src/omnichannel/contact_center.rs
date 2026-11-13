//! Contact Center (`docs/omnichannel.md` §9.3, web `/omnichannel-directory`):
//! contacts (`omnichannel/contacts.search`) and chats (`livechat/rooms`,
//! restricted to the user's own chats without `view-livechat-rooms`).
//! Shown in place of the room area; a chat opens like any room (closed
//! chats read-only).

use iced::widget::{Column, column, container, row, scrollable, text};
use iced::{Alignment, Element, Length, Task};
use rocketchat::RestClient;
use rocketchat::models::{Contact, Paginated, Room};
use rocketchat::rest::{LivechatPage, LivechatRoomsQuery};
use rust_i18n::t;
use rustket_core::Workspace;
use rustket_core::omnichannel::permissions as perm;
use rustket_core::permissions::has_permission;
use rustket_ui::icon::{self, Icon};
use rustket_ui::style;
use rustket_ui::theme::{
    HEADER_HEIGHT, SPACE_1, SPACE_2, SPACE_3, SPACE_4, TEXT_BODY, TEXT_CAPTION, TEXT_SUBTITLE,
    font_semibold,
};
use rustket_ui::widget::{ChipKind, chip, divider, ellipsis, icon_button};

use crate::error::{Failure, Res};
use crate::room::panel::common::{filter_pills, list_row, load_more, search_field};

const PAGE: u64 = 25;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Tab {
    #[default]
    Contacts,
    Chats,
}

#[derive(Debug, Clone)]
pub enum Msg {
    Tab(Tab),
    Query(String),
    Search(u64),
    Contacts(u64, Res<Paginated<Contact>>),
    Chats(u64, Res<Paginated<Room>>),
    More,
    Retry,
    OpenChat(String),
    /// Show a contact's past chats (`contacts.history`): id, name.
    OpenContact(String, String),
    /// Back from a contact's chats to the contact list.
    Back,
    Close,
}

/// Requests to the app.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Out {
    Close,
    /// Open chat `rid` (fetched into the workspace first when needed).
    OpenChat(String),
}

#[derive(Default)]
pub struct ContactCenter {
    pub tab: Tab,
    query: String,
    seq: u64,
    loading: bool,
    error: Option<String>,
    pub contacts: Vec<Contact>,
    pub chats: Vec<Room>,
    total: u64,
    /// The contact whose chats are listed (id, name).
    pub contact: Option<(String, String)>,
}

fn run<T: Send + 'static>(
    rest: Option<RestClient>,
    call: impl FnOnce(
        RestClient,
    ) -> std::pin::Pin<Box<dyn Future<Output = rocketchat::Result<T>> + Send>>
    + Send
    + 'static,
    map: impl FnOnce(Res<T>) -> Msg + Send + 'static,
) -> Task<Msg> {
    Task::perform(
        async move {
            match rest {
                Some(r) => call(r).await.map_err(Failure::from),
                None => Err(Failure::from(rocketchat::Error::Unauthorized {
                    message: Some("not connected".into()),
                    code: None,
                })),
            }
        },
        map,
    )
}

impl ContactCenter {
    pub fn new() -> Self {
        Self::default()
    }

    fn len(&self) -> u64 {
        match (self.tab, &self.contact) {
            (Tab::Contacts, None) => self.contacts.len() as u64,
            _ => self.chats.len() as u64,
        }
    }

    /// Loads the page at `offset` of the current tab.
    pub fn load(&mut self, rest: Option<RestClient>, ws: &Workspace, offset: u64) -> Task<Msg> {
        self.loading = true;
        self.error = None;
        self.seq += 1;
        let seq = self.seq;
        let page = LivechatPage::new(offset, PAGE);
        let text = Some(self.query.trim().to_owned()).filter(|s| !s.is_empty());
        if let Some((id, _)) = &self.contact {
            let id = id.clone();
            return run(
                rest,
                move |r| {
                    Box::pin(async move { r.omnichannel_contact_history(&id, None, page).await })
                },
                move |r| Msg::Chats(seq, r),
            );
        }
        match self.tab {
            Tab::Contacts => run(
                rest,
                move |r| {
                    Box::pin(async move {
                        r.omnichannel_contacts_search(text.as_deref(), None, page)
                            .await
                    })
                },
                move |r| Msg::Contacts(seq, r),
            ),
            Tab::Chats => {
                let mut q = LivechatRoomsQuery {
                    room_name: text,
                    page,
                    ..Default::default()
                };
                // Without `view-livechat-rooms` only the own chats may be listed.
                if !has_permission(ws, perm::VIEW_LIVECHAT_ROOMS, None)
                    && let Some(me) = ws.user_id()
                {
                    q.agents = vec![me.to_owned()];
                }
                run(
                    rest,
                    move |r| Box::pin(async move { r.livechat_rooms(&q).await }),
                    move |r| Msg::Chats(seq, r),
                )
            }
        }
    }

    pub fn update(
        &mut self,
        msg: Msg,
        rest: Option<RestClient>,
        ws: &Workspace,
    ) -> (Task<Msg>, Option<Out>) {
        let task = match msg {
            Msg::Tab(t) if t != self.tab => {
                self.tab = t;
                self.contact = None;
                self.contacts.clear();
                self.chats.clear();
                self.load(rest, ws, 0)
            }
            Msg::Tab(_) => Task::none(),
            Msg::Query(q) => {
                self.query = q;
                self.seq += 1;
                crate::room::panel::debounce(Msg::Search(self.seq))
            }
            Msg::Search(seq) if seq == self.seq => {
                self.contacts.clear();
                self.chats.clear();
                self.load(rest, ws, 0)
            }
            Msg::Search(_) => Task::none(),
            Msg::Contacts(seq, r) if seq == self.seq => {
                self.loading = false;
                match r {
                    Ok(p) => {
                        self.total = p.total;
                        self.contacts.extend(p.items);
                    }
                    Err(f) => self.error = Some(super::error_text(&f)),
                }
                Task::none()
            }
            Msg::Chats(seq, r) if seq == self.seq => {
                self.loading = false;
                match r {
                    Ok(p) => {
                        self.total = p.total;
                        self.chats.extend(p.items);
                    }
                    Err(f) => self.error = Some(super::error_text(&f)),
                }
                Task::none()
            }
            Msg::Contacts(..) | Msg::Chats(..) => Task::none(),
            Msg::More if !self.loading && self.len() < self.total => {
                let offset = self.len();
                self.load(rest, ws, offset)
            }
            Msg::More => Task::none(),
            Msg::Retry => {
                self.contacts.clear();
                self.chats.clear();
                self.load(rest, ws, 0)
            }
            Msg::OpenChat(rid) => return (Task::none(), Some(Out::OpenChat(rid))),
            Msg::OpenContact(id, name) => {
                self.contact = Some((id, name));
                self.chats.clear();
                self.load(rest, ws, 0)
            }
            Msg::Back => {
                self.contact = None;
                self.chats.clear();
                self.contacts.clear();
                self.load(rest, ws, 0)
            }
            Msg::Close => return (Task::none(), Some(Out::Close)),
        };
        (task, None)
    }

    fn contact_row<'a>(&self, c: &'a Contact) -> Element<'a, Msg> {
        let open = Msg::OpenContact(c.id.clone(), c.name.clone().unwrap_or_default());
        let name = c.name.clone().unwrap_or_default();
        let mut sub: Vec<String> = Vec::new();
        if let Some(e) = c.emails.first() {
            sub.push(e.address.clone());
        }
        if let Some(p) = c.phones.first() {
            sub.push(p.phone_number.clone());
        }
        let mut head = row![ellipsis(name.clone()).size(TEXT_BODY).font(font_semibold())]
            .spacing(SPACE_2)
            .align_y(Alignment::Center);
        if c.unknown {
            head = head.push(chip(
                t!("omnichannel.unknown_contact_chip").into_owned(),
                ChipKind::Warning,
            ));
        }
        list_row(
            row![
                rustket_ui::widget::avatar(
                    rustket_ui::widget::AvatarSource::initials(&name, &c.id),
                    32.0,
                    rustket_ui::widget::AvatarShape::Rounded,
                ),
                column![
                    head,
                    text(sub.join(" · "))
                        .size(TEXT_CAPTION)
                        .style(style::text_secondary)
                ]
                .spacing(SPACE_1)
                .width(Length::Fill),
                text(t!("omnichannel.channels_count", count = c.channels.len()).into_owned())
                    .size(TEXT_CAPTION)
                    .style(style::text_secondary),
            ]
            .spacing(SPACE_3)
            .align_y(Alignment::Center),
            Some(open),
        )
    }

    fn chat_row<'a>(&self, r: &'a Room, clock_24h: bool) -> Element<'a, Msg> {
        let open = r.open == Some(true);
        let status = if r.on_hold == Some(true) {
            chip(
                t!("omnichannel.status_on_hold").into_owned(),
                ChipKind::Warning,
            )
        } else if open && r.served_by_agent().is_none() {
            chip(
                t!("omnichannel.status_queued").into_owned(),
                ChipKind::Primary,
            )
        } else if open {
            chip(
                t!("omnichannel.status_open").into_owned(),
                ChipKind::Success,
            )
        } else {
            chip(
                t!("omnichannel.status_closed").into_owned(),
                ChipKind::Neutral,
            )
        };
        let agent = r
            .served_by_agent()
            .and_then(|s| s.username)
            .map(|u| format!("@{u}"))
            .unwrap_or_default();
        let when = r
            .closed_at
            .or(r.ts)
            .map(|ts| crate::format::short_time(ts, chrono::Local::now(), clock_24h))
            .unwrap_or_default();
        let src = r.omnichannel_source().map(|s| s.kind).unwrap_or_default();
        list_row(
            row![
                icon::sized(super::panel::info::source_icon(src), 16.0)
                    .style(style::text_secondary),
                column![
                    ellipsis(r.fname.clone().unwrap_or_default())
                        .size(TEXT_BODY)
                        .font(font_semibold()),
                    text(format!("{agent}  {when}"))
                        .size(TEXT_CAPTION)
                        .style(style::text_secondary),
                ]
                .width(Length::Fill),
                status,
            ]
            .spacing(SPACE_3)
            .align_y(Alignment::Center),
            Some(Msg::OpenChat(r.id.clone())),
        )
    }

    pub fn view(&self, clock_24h: bool) -> Element<'_, Msg> {
        let head = container(
            row![
                icon::sized(Icon::Contact, 18.0),
                text(t!("omnichannel.contact_center").into_owned())
                    .size(TEXT_SUBTITLE)
                    .font(font_semibold())
                    .width(Length::Fill),
                icon_button(Icon::X, Some(Msg::Close), t!("omnichannel.close_screen")),
            ]
            .spacing(SPACE_2)
            .align_y(Alignment::Center),
        )
        .height(HEADER_HEIGHT)
        .padding([0.0, SPACE_4])
        .center_y(HEADER_HEIGHT)
        .style(style::container_header);
        let placeholder = match self.tab {
            Tab::Contacts => t!("omnichannel.search_contacts"),
            Tab::Chats => t!("omnichannel.search_chats"),
        };
        let tools = if let Some((_, name)) = &self.contact {
            container(
                row![
                    icon_button(Icon::ChevronLeft, Some(Msg::Back), t!("omnichannel.back")),
                    column![
                        ellipsis(name.clone()).size(TEXT_BODY).font(font_semibold()),
                        text(t!("omnichannel.contact_chats").into_owned())
                            .size(TEXT_CAPTION)
                            .style(style::text_secondary),
                    ]
                    .width(Length::Fill),
                ]
                .spacing(SPACE_2)
                .align_y(Alignment::Center),
            )
            .padding([SPACE_3, SPACE_4])
        } else {
            container(
                column![
                    filter_pills(
                        &[
                            (Tab::Contacts, t!("omnichannel.tab_contacts").into_owned()),
                            (Tab::Chats, t!("omnichannel.tab_chats").into_owned()),
                        ],
                        self.tab,
                        Msg::Tab
                    ),
                    search_field(&self.query, placeholder.into_owned(), Msg::Query),
                ]
                .spacing(SPACE_2),
            )
            .padding([SPACE_3, SPACE_4])
        };
        let mut list: Column<'_, Msg> = column![].spacing(0);
        if let (Some(e), 0) = (&self.error, self.len()) {
            list = list.push(crate::room::panel::common::error_state(e, Msg::Retry));
        } else {
            match (self.tab, &self.contact) {
                (Tab::Contacts, None) => {
                    for c in &self.contacts {
                        list = list.push(self.contact_row(c));
                    }
                }
                _ => {
                    for r in &self.chats {
                        list = list.push(self.chat_row(r, clock_24h));
                    }
                }
            }
            if self.len() == 0 && !self.loading {
                list = list.push(crate::room::panel::common::empty(
                    Icon::Inbox,
                    t!("omnichannel.nothing_found").into_owned(),
                ));
            }
            list = list.push(load_more(self.loading, self.len() < self.total, Msg::More));
        }
        container(
            column![
                head,
                divider(),
                tools,
                divider(),
                scrollable(list)
                    .height(Length::Fill)
                    .style(style::scrollable),
            ]
            .height(Length::Fill),
        )
        .width(Length::Fill)
        .height(Length::Fill)
        .style(style::container_room)
        .into()
    }
}
