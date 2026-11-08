//! Sidebar of the active server: user header with the account menu, search,
//! collapsible sections of rooms. Strings: `locales/sidebar.yml`,
//! `locales/preview.yml`, `locales/typing.yml`.

use std::collections::BTreeSet;
use std::time::Instant;

use iced::widget::{Column, column, container, row, scrollable, text_input};
use iced::{Alignment, Element, Length};
use rocketchat::ServerUrl;
use rocketchat::models::UserStatus;
use rust_i18n::t;
use rustket_core::config::{Density, UiPrefs};
use rustket_core::room_list::{
    self, Avatar, BadgeVariant, Preview, PreviewKind, RoomKind, RoomListItem, RoomListOptions,
    Section, SectionKind,
};
use rustket_core::typing::TypingLabel;
use rustket_core::{ServerId, Workspace};
use rustket_ui::icon::{self, Icon};
use rustket_ui::style;
use rustket_ui::theme::{SPACE_1, SPACE_2, SPACE_3, TEXT_BODY};
use rustket_ui::widget::{
    Presence, RoomListItemProps, empty_state, icon_button, room_list_item, section_header,
};

use crate::format::{count_u32, short_time};
use crate::i18n::tr_dyn;
use crate::images::Images;

pub const SEARCH_INPUT_ID: &str = "sidebar-search";

#[derive(Debug, Clone)]
pub enum Message {
    Filter(String),
    ClearFilter,
    Toggle(SectionKind),
    Open(String),
    /// The status menu in the header (`workspace/status_menu.rs`).
    Status(super::status_menu::Message),
    Directory,
    Create,
    /// Omnichannel groups and buttons (`crate::omnichannel::sidebar`).
    Omni(crate::omnichannel::sidebar::Msg),
}

#[derive(Debug, Clone)]
pub enum Out {
    None,
    OpenRoom(String),
    /// Collapsed state of a section changed (`SectionKind::i18n_key`).
    ToggleSection(String),
    Status(super::status_menu::Message),
    Directory,
    Create,
    Omni(crate::omnichannel::sidebar::Msg),
}

pub struct SidebarCtx<'a> {
    pub server_id: &'a ServerId,
    pub server: &'a ServerUrl,
    pub workspace: &'a Workspace,
    pub images: &'a Images,
    pub prefs: &'a UiPrefs,
    pub collapsed: &'a BTreeSet<String>,
    pub active_room: Option<&'a str>,
    pub now: Instant,
}

#[derive(Default)]
pub struct Sidebar {
    pub filter: String,
    pub sections: Vec<Section>,
    /// Revision of the inputs the sections were computed from.
    computed_for: Option<(String, u64, u64, u64, u64)>,
}

pub fn presence(s: UserStatus) -> Presence {
    match s {
        UserStatus::Online => Presence::Online,
        UserStatus::Away => Presence::Away,
        UserStatus::Busy => Presence::Busy,
        UserStatus::Offline => Presence::Offline,
        _ => Presence::Unknown,
    }
}

impl Sidebar {
    pub fn update(&mut self, msg: Message) -> Out {
        match msg {
            Message::Filter(s) => {
                self.filter = s;
                Out::None
            }
            Message::ClearFilter => {
                self.filter.clear();
                Out::None
            }
            Message::Toggle(kind) => Out::ToggleSection(kind.i18n_key().to_owned()),
            Message::Open(rid) => Out::OpenRoom(rid),
            Message::Status(super::status_menu::Message::Omni(
                crate::omnichannel::status::Msg::ContactCenter,
            )) => Out::Omni(crate::omnichannel::sidebar::Msg::ContactCenter),
            Message::Status(m) => Out::Status(m),
            Message::Directory => Out::Directory,
            Message::Create => Out::Create,
            Message::Omni(crate::omnichannel::sidebar::Msg::Toggle(key)) => Out::ToggleSection(key),
            Message::Omni(m) => Out::Omni(m),
        }
    }

    /// Recomputes the sections when the workspace or the filter changed and asks
    /// the image service for the avatars of all rows.
    pub fn refresh(
        &mut self,
        ws: &Workspace,
        prefs: &UiPrefs,
        images: &Images,
        sid: &ServerId,
        server: &ServerUrl,
        prefetch: usize,
    ) {
        use rustket_core::Dirty;
        let key = (
            self.filter.clone(),
            ws.revision(Dirty::ROOMS),
            ws.revision(Dirty::SETTINGS),
            ws.revision(Dirty::PRESENCE),
            ws.revision(Dirty::DRAFTS) ^ ws.revision(Dirty::ME).rotate_left(32),
        );
        if self.computed_for.as_ref() == Some(&key) {
            return;
        }
        let opts = RoomListOptions::from_prefs(prefs, ws);
        self.sections = room_list::compute(ws, &opts, &self.filter);
        self.computed_for = Some(key);
        // Prefetch the first rows' avatars (and the user's own); the rest are
        // requested when they are shown.
        if let Some(me) = ws.username() {
            images.want(crate::images::ImageRequest {
                server_id: sid.clone(),
                url: Images::avatar_url(
                    server,
                    &Avatar::User {
                        username: me.to_owned(),
                    },
                ),
            });
        }
        for item in self
            .sections
            .iter()
            .flat_map(|s| s.items.iter())
            .take(prefetch)
        {
            images.want(crate::images::ImageRequest {
                server_id: sid.clone(),
                url: Images::avatar_url(server, &item.avatar),
            });
        }
    }

    pub fn room_count(&self) -> usize {
        self.sections.iter().map(|s| s.items.len()).sum()
    }

    /// `status`: the status menu (avatar trigger and popover) for the header.
    pub fn view<'a>(
        &'a self,
        ctx: &SidebarCtx<'a>,
        status: Element<'a, super::status_menu::Message>,
    ) -> Element<'a, Message> {
        let header = self.header(status);
        let search = container(
            row![
                icon::sized(Icon::Search, 16.0).style(style::text_hint),
                text_input(&t!("sidebar.search"), &self.filter)
                    .id(SEARCH_INPUT_ID)
                    .on_input(Message::Filter)
                    .style(style::text_input_bare)
                    .size(TEXT_BODY)
                    .padding([6, 0]),
            ]
            .spacing(SPACE_2)
            .align_y(Alignment::Center)
            .padding([0.0, SPACE_2]),
        )
        .style(|theme: &iced::Theme| {
            let t = rustket_ui::theme::tokens(theme);
            container::Style {
                background: Some(t.surface_sunken.into()),
                border: iced::border::rounded(rustket_ui::theme::RADIUS_MD)
                    .width(1)
                    .color(t.border),
                ..container::Style::default()
            }
        })
        .padding([0.0, SPACE_1]);
        let queue = crate::omnichannel::sidebar::queue_group(ctx, &self.filter);
        let body: Element<'a, Message> = if self.sections.is_empty() && queue.is_none() {
            if self.filter.trim().is_empty() {
                empty_state(Icon::Inbox, t!("sidebar.no_rooms"), String::new())
            } else {
                empty_state(Icon::Search, t!("sidebar.no_results"), String::new())
            }
        } else {
            let mut list = Column::new().spacing(0).padding([0.0, SPACE_2]);
            // Omnichannel groups first (queue, in progress, on hold).
            let row = |item: &'a RoomListItem| self.row(item, ctx);
            list = crate::omnichannel::sidebar::push_groups(
                list,
                queue,
                &self.sections,
                ctx,
                &self.filter,
                &row,
            );
            for section in &self.sections {
                if section.kind == SectionKind::Omnichannel {
                    continue;
                }
                let collapsed = self.filter.trim().is_empty()
                    && ctx.collapsed.contains(section.kind.i18n_key());
                list = list.push(section_header(
                    t!(section.kind.i18n_key()).into_owned(),
                    collapsed,
                    Some(section.items.len() as u32),
                    Message::Toggle(section.kind),
                ));
                if !collapsed {
                    for item in &section.items {
                        list = list.push(self.row(item, ctx));
                    }
                }
            }
            scrollable(list)
                .height(Length::Fill)
                .style(style::scrollable)
                .into()
        };
        container(
            column![header, container(search).padding([0.0, SPACE_3]), body]
                .spacing(SPACE_2)
                .height(Length::Fill),
        )
        .height(Length::Fill)
        .width(Length::Fill)
        .style(style::container_sidebar)
        .into()
    }

    fn header<'a>(
        &'a self,
        status: Element<'a, super::status_menu::Message>,
    ) -> Element<'a, Message> {
        let menu = status.map(Message::Status);
        container(
            row![
                container(menu).width(Length::Fill),
                icon_button(
                    Icon::Globe,
                    Some(Message::Directory),
                    t!("sidebar.directory")
                ),
                icon_button(Icon::SquarePen, Some(Message::Create), t!("sidebar.create")),
            ]
            .spacing(SPACE_1)
            .align_y(Alignment::Center),
        )
        .padding([SPACE_2, SPACE_2])
        .into()
    }

    fn row<'a>(&'a self, item: &'a RoomListItem, ctx: &SidebarCtx<'a>) -> Element<'a, Message> {
        let src = ctx
            .images
            .avatar(ctx.server_id, ctx.server, &item.avatar, &item.display_name);
        let mut p = RoomListItemProps::new(
            src,
            item.display_name.clone(),
            Message::Open(item.rid.clone()),
        );
        p.density = match ctx.prefs.density {
            Density::Extended => rustket_ui::widget::Density::Extended,
            Density::Condensed => rustket_ui::widget::Density::Condensed,
        };
        p.subtitle = item.preview.as_ref().map(preview_text);
        p.time = item
            .last_activity
            .map(|ts| short_time(ts, chrono::Local::now(), ctx.prefs.clock_24h));
        let b = &item.badge;
        p.unread = count_u32(b.count);
        p.user_mentions = if b.variant == BadgeVariant::Mention {
            count_u32(b.mentions)
        } else {
            0
        };
        p.group_mentions = if b.variant == BadgeVariant::GroupMention {
            count_u32(b.group_mentions)
        } else {
            0
        };
        p.thread_unread = b.threads > 0;
        if b.dot && p.unread == 0 {
            // `alert` without a counter: show a dot via thread/unread styling.
            p.thread_unread = true;
        }
        p.is_muted = item.muted;
        p.is_selected = ctx.active_room == Some(item.rid.as_str());
        p.presence = item.dm_status.map(presence);
        p.icon = Some(room_icon(item.kind));
        crate::omnichannel::sidebar::decorate(&mut p, item, ctx);
        if let Some(label) = ctx.workspace.typing_label(&item.rid, None, ctx.now) {
            p.typing = true;
            p.typing_label = typing_text(&label);
        }
        room_list_item(p)
    }
}

pub fn room_icon(kind: RoomKind) -> Icon {
    match kind {
        RoomKind::Channel => Icon::Hash,
        RoomKind::Private => Icon::Lock,
        RoomKind::Direct => Icon::AtSign,
        RoomKind::GroupDirect => Icon::Users,
        RoomKind::Discussion => Icon::MessagesSquare,
        RoomKind::Team { .. } => Icon::Users,
        RoomKind::Omnichannel => Icon::MessageCircle,
    }
}

/// Last-message preview line: "You: …", "bob: …", "📷 Image" …
pub fn preview_text(p: &Preview) -> String {
    let body = match p.kind {
        PreviewKind::Text => p.text.clone().unwrap_or_default(),
        PreviewKind::System => {
            let arg = p.text.clone().unwrap_or_default();
            let s = rust_i18n::t!(p.i18n_key.as_str(), text = arg).into_owned();
            if s.ends_with(p.i18n_key.as_str()) {
                t!("preview.system.generic").into_owned()
            } else {
                s
            }
        }
        _ => {
            let label = tr_dyn(&p.i18n_key, &t!("preview.attachment"));
            match p.text.as_deref() {
                Some(name) if !name.is_empty() => format!("{label}: {name}"),
                _ => label,
            }
        }
    };
    if p.own {
        t!("preview.you", text = body).into_owned()
    } else if let Some(author) = &p.author {
        t!("preview.author", author = author, text = body).into_owned()
    } else {
        body
    }
}

/// "bob is typing…", "bob and alice are typing…", "a, b, c and 2 others are typing…".
pub fn typing_text(l: &TypingLabel) -> String {
    let names = match (l.usernames.as_slice(), l.others()) {
        ([one], 0) => one.clone(),
        (list, 0) if list.len() > 1 => {
            let (last, rest) = list
                .split_last()
                .map_or((String::new(), &[][..]), |(l, r)| (l.clone(), r));
            t!("typing.and", first = rest.join(", "), last = last).into_owned()
        }
        (list, others) => {
            t!("typing.and_others", first = list.join(", "), count = others).into_owned()
        }
    };
    let key = l.activity.i18n_key();
    if l.count > 1 {
        rust_i18n::t!(format!("{key}_many"), names = names).into_owned()
    } else {
        rust_i18n::t!(key, names = names).into_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustket_core::typing::Activity;

    #[test]
    fn typing_labels() {
        rust_i18n::set_locale("en");
        let l = |names: &[&str], count: usize| TypingLabel {
            usernames: names.iter().map(|s| (*s).to_owned()).collect(),
            count,
            activity: Activity::Typing,
        };
        assert_eq!(typing_text(&l(&["bob"], 1)), "bob is typing…");
        assert_eq!(
            typing_text(&l(&["bob", "eve"], 2)),
            "bob and eve are typing…"
        );
        assert_eq!(
            typing_text(&l(&["a", "b", "c"], 5)),
            "a, b, c and 2 others are typing…"
        );
    }

    #[test]
    fn previews() {
        rust_i18n::set_locale("en");
        let p = Preview {
            kind: PreviewKind::Text,
            i18n_key: "preview.text".into(),
            own: true,
            author: None,
            text: Some("hello".into()),
            system_type: None,
            ts: None,
        };
        assert_eq!(preview_text(&p), "You: hello");
        let p = Preview {
            kind: PreviewKind::Image,
            i18n_key: "preview.image".into(),
            own: false,
            author: Some("bob".into()),
            text: Some("cat.png".into()),
            system_type: None,
            ts: None,
        };
        assert_eq!(preview_text(&p), "bob: Image: cat.png");
        let p = Preview {
            kind: PreviewKind::System,
            i18n_key: "preview.system.unknown-type".into(),
            own: false,
            author: None,
            text: None,
            system_type: Some("unknown-type".into()),
            ts: None,
        };
        assert_eq!(preview_text(&p), "System message");
    }
}
