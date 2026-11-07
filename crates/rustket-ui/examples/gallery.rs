//! Visual gallery of the Rustket design system.
//!
//! `cargo run -p rustket-ui --example gallery`
//!
//! Environment switches for screenshots: `GALLERY_MODE=dark`, `GALLERY_MODAL=1`,
//! `GALLERY_TOASTS=1`, `GALLERY_MENU=1` (popover open), `GALLERY_CONDENSED=1`,
//! `GALLERY_TOP=1` (timeline scrolled to the top).

use std::collections::HashSet;
use std::time::{Duration, Instant};

use iced::widget::{
    Column, button, checkbox, column, container, pick_list, row, scrollable, space, text,
    text_input, toggler,
};
use iced::{Alignment, Element, Length, Subscription, Task, Theme, keyboard};

use rustket_ui::icon::{self, Icon};
use rustket_ui::style;
use rustket_ui::theme::{self, *};
use rustket_ui::widget::*;

pub fn main() -> iced::Result {
    iced::application(Gallery::new, Gallery::update, Gallery::view)
        .settings(rustket_ui::settings())
        .title("Rustket UI gallery")
        .theme(Gallery::theme)
        .subscription(Gallery::subscription)
        .window_size((1440.0, 900.0))
        .run()
}

#[derive(Debug, Clone)]
enum Message {
    ToggleMode,
    SelectRoom(usize),
    ToggleSection(&'static str),
    ResizeSidebar(f32),
    ShowModal(bool),
    ModalName(String),
    ModalPassword(String),
    ModalSubmit,
    AddToast,
    CloseToast(u64),
    Tick,
    Composer(String),
    Send,
    PopoverOpen(bool),
    MenuAction(&'static str),
    React(usize),
    Density(bool),
    Notify(bool),
    Plan(Plan),
    Workspace(usize),
    Noop,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Plan {
    Activity,
    Name,
    Unread,
}

impl std::fmt::Display for Plan {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Plan::Activity => "By activity",
            Plan::Name => "By name",
            Plan::Unread => "Unread first",
        })
    }
}

struct Room {
    name: &'static str,
    preview: &'static str,
    time: &'static str,
    unread: u32,
    mentions: u32,
    group: u32,
    threads: bool,
    muted: bool,
    presence: Option<Presence>,
    icon: Option<Icon>,
    section: &'static str,
    typing: bool,
}

const fn room(
    name: &'static str,
    preview: &'static str,
    time: &'static str,
    section: &'static str,
) -> Room {
    Room {
        name,
        preview,
        time,
        unread: 0,
        mentions: 0,
        group: 0,
        threads: false,
        muted: false,
        presence: None,
        icon: Some(Icon::Hash),
        section,
        typing: false,
    }
}

fn rooms() -> Vec<Room> {
    vec![
        Room {
            unread: 3,
            mentions: 1,
            ..room(
                "general",
                "Иван: Привет всем! 👋 Релиз в пятницу",
                "10:42",
                "Favorites",
            )
        },
        Room {
            icon: Some(Icon::Lock),
            unread: 12,
            ..room(
                "release-planning",
                "Anna: Let's freeze the branch tomorrow",
                "10:15",
                "Favorites",
            )
        },
        Room {
            icon: None,
            presence: Some(Presence::Online),
            typing: true,
            ..room(
                "Alice Johnson",
                "Sure, sending the logs now",
                "09:58",
                "Favorites",
            )
        },
        Room {
            icon: Some(Icon::Users),
            unread: 128,
            group: 1,
            ..room(
                "Platform Team",
                "@all standup moved to 11:00",
                "09:30",
                "Teams",
            )
        },
        Room {
            icon: Some(Icon::MessagesSquare),
            threads: true,
            ..room(
                "Q3 roadmap discussion",
                "Bob: Added the KPIs section 📊",
                "Yesterday",
                "Discussions",
            )
        },
        Room {
            muted: true,
            unread: 5,
            ..room(
                "random",
                "Cat pictures incoming 🐱🔥",
                "Yesterday",
                "Channels",
            )
        },
        room(
            "a-very-long-channel-name-that-will-definitely-not-fit-into-the-sidebar",
            "Long names get clipped instead of wrapping",
            "Mon",
            "Channels",
        ),
        Room {
            unread: 2,
            ..room("design", "Новые иконки в Figma 🎨", "Mon", "Channels")
        },
        Room {
            icon: Some(Icon::Lock),
            ..room(
                "security-incidents",
                "No incidents this week ✅",
                "Sun",
                "Channels",
            )
        },
        Room {
            icon: None,
            presence: Some(Presence::Away),
            ..room(
                "Борис Петров",
                "Ок, посмотрю вечером",
                "Sun",
                "Direct messages",
            )
        },
        Room {
            icon: None,
            presence: Some(Presence::Busy),
            unread: 1,
            mentions: 1,
            ..room(
                "Carol Danvers",
                "Can you review my PR?",
                "Sat",
                "Direct messages",
            )
        },
        Room {
            icon: None,
            presence: Some(Presence::Offline),
            ..room(
                "rocket.cat",
                "Welcome to Rocket.Chat! 🚀",
                "Fri",
                "Direct messages",
            )
        },
        Room {
            icon: None,
            presence: Some(Presence::Online),
            ..room("Dmitry K", "👍", "Thu", "Direct messages")
        },
    ]
}

const SECTIONS: [&str; 5] = [
    "Favorites",
    "Teams",
    "Discussions",
    "Channels",
    "Direct messages",
];

#[derive(Clone)]
struct Msg {
    author: &'static str,
    time: &'static str,
    body: &'static str,
    reactions: Vec<(&'static str, u32, bool)>,
    role: Option<&'static str>,
}

struct Gallery {
    mode: theme::Mode,
    rooms: Vec<Room>,
    selected: usize,
    collapsed: HashSet<&'static str>,
    sidebar_width: f32,
    show_modal: bool,
    modal_name: String,
    modal_password: String,
    toasts: Vec<Toast>,
    toast_born: Vec<(u64, Instant)>,
    next_toast: u64,
    composer: String,
    popover: bool,
    messages: Vec<Msg>,
    condensed: bool,
    notify: bool,
    plan: Plan,
    workspace: usize,
}

fn env_flag(name: &str) -> bool {
    std::env::var(name).is_ok_and(|v| v != "0" && !v.is_empty())
}

impl Gallery {
    fn new() -> (Self, Task<Message>) {
        let mode = if std::env::var("GALLERY_MODE").is_ok_and(|v| v == "dark") {
            theme::Mode::Dark
        } else {
            theme::Mode::Light
        };
        let mut g = Gallery {
            mode,
            rooms: rooms(),
            selected: 0,
            collapsed: HashSet::new(),
            sidebar_width: SIDEBAR_WIDTH,
            show_modal: env_flag("GALLERY_MODAL"),
            modal_name: "Иван".into(),
            modal_password: String::new(),
            toasts: Vec::new(),
            toast_born: Vec::new(),
            next_toast: 0,
            composer: String::new(),
            popover: env_flag("GALLERY_MENU"),
            messages: vec![
                Msg {
                    author: "Иван Петров",
                    time: "10:02",
                    body: "Всем привет! Сегодня выкатываем новую версию 🎉",
                    reactions: vec![("🎉", 4, true), ("👍", 2, false)],
                    role: Some("Admin"),
                },
                Msg {
                    author: "Alice Johnson",
                    time: "10:05",
                    body: "Great news! The migration finished without errors. Ёжик в тумане — съешь же ещё этих мягких французских булок.",
                    reactions: vec![("🔥", 3, false)],
                    role: None,
                },
                Msg {
                    author: "rocket.cat",
                    time: "10:06",
                    body: "Build #1287 passed ✅ — deploy to staging started.",
                    reactions: vec![],
                    role: Some("Bot"),
                },
                Msg {
                    author: "Dmitry K",
                    time: "10:12",
                    body: "👍🎉🔥",
                    reactions: vec![],
                    role: None,
                },
                Msg {
                    author: "Carol Danvers",
                    time: "10:40",
                    body: "@ivan can you take a look at the release notes before lunch? 🙏",
                    reactions: vec![("👀", 1, false), ("✅", 2, true)],
                    role: Some("Owner"),
                },
            ],
            condensed: env_flag("GALLERY_CONDENSED"),
            notify: true,
            plan: Plan::Activity,
            workspace: 0,
        };
        let mut task = Task::none();
        if env_flag("GALLERY_TOASTS") {
            for _ in 0..3 {
                task = Task::batch([task, Task::done(Message::AddToast)]);
            }
        }
        if g.show_modal {
            g.modal_password = "secret".into();
        }
        (g, task)
    }

    fn theme(&self) -> Theme {
        theme::theme(self.mode)
    }

    fn subscription(&self) -> Subscription<Message> {
        let keys = keyboard::listen().filter_map(|event| match event {
            keyboard::Event::KeyPressed {
                key: keyboard::Key::Named(keyboard::key::Named::Escape),
                ..
            } => Some(Message::ShowModal(false)),
            _ => None,
        });
        if self.toasts.is_empty() || env_flag("GALLERY_TOASTS") {
            keys
        } else {
            Subscription::batch([
                keys,
                iced::time::every(Duration::from_millis(500)).map(|_| Message::Tick),
            ])
        }
    }

    fn update(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::ToggleMode => self.mode = self.mode.toggle(),
            Message::SelectRoom(i) => self.selected = i,
            Message::ToggleSection(s) => {
                if !self.collapsed.remove(s) {
                    self.collapsed.insert(s);
                }
            }
            Message::ResizeSidebar(w) => self.sidebar_width = w,
            Message::ShowModal(v) => self.show_modal = v,
            Message::ModalName(v) => self.modal_name = v,
            Message::ModalPassword(v) => self.modal_password = v,
            Message::ModalSubmit => self.show_modal = false,
            Message::AddToast => {
                let id = self.next_toast;
                self.next_toast += 1;
                let (kind, title, body) = match id % 4 {
                    0 => (
                        BannerKind::Success,
                        "Message sent",
                        "Your message was delivered to #general.",
                    ),
                    1 => (
                        BannerKind::Error,
                        "Upload failed",
                        "Файл слишком большой (макс. 100 МБ).",
                    ),
                    2 => (
                        BannerKind::Warning,
                        "Connection unstable",
                        "Reconnecting to chat.example.com…",
                    ),
                    _ => (BannerKind::Info, "New version available", ""),
                };
                self.toasts.push(Toast {
                    id,
                    kind,
                    title: title.into(),
                    body: body.into(),
                });
                self.toast_born.push((id, Instant::now()));
            }
            Message::CloseToast(id) => {
                self.toasts.retain(|t| t.id != id);
                self.toast_born.retain(|(t, _)| *t != id);
            }
            Message::Tick => {
                let expired: Vec<u64> = self
                    .toast_born
                    .iter()
                    .filter(|(_, born)| born.elapsed() > Duration::from_secs(5))
                    .map(|(id, _)| *id)
                    .collect();
                for id in expired {
                    self.toasts.retain(|t| t.id != id);
                    self.toast_born.retain(|(t, _)| *t != id);
                }
            }
            Message::Composer(v) => self.composer = v,
            Message::Send => {
                if !self.composer.trim().is_empty() {
                    self.messages.push(Msg {
                        author: "You",
                        time: "now",
                        body: Box::leak(std::mem::take(&mut self.composer).into_boxed_str()),
                        reactions: vec![],
                        role: None,
                    });
                }
            }
            Message::PopoverOpen(v) => self.popover = v,
            Message::MenuAction(action) => {
                self.popover = false;
                let id = self.next_toast;
                self.next_toast += 1;
                self.toasts.push(Toast {
                    id,
                    kind: BannerKind::Info,
                    title: format!("Menu action: {action}"),
                    body: String::new(),
                });
                self.toast_born.push((id, Instant::now()));
            }
            Message::React(i) => {
                if let Some(m) = self.messages.get_mut(i)
                    && let Some(r) = m.reactions.first_mut()
                {
                    r.2 = !r.2;
                    r.1 = if r.2 { r.1 + 1 } else { r.1.saturating_sub(1) };
                }
            }
            Message::Density(v) => self.condensed = v,
            Message::Notify(v) => self.notify = v,
            Message::Plan(p) => self.plan = p,
            Message::Workspace(i) => self.workspace = i,
            Message::Noop => {}
        }
        Task::none()
    }

    fn view(&self) -> Element<'_, Message> {
        let body = row![
            self.rail(),
            split(
                self.sidebar(),
                self.room(),
                self.sidebar_width,
                SIDEBAR_MIN_WIDTH,
                SIDEBAR_MAX_WIDTH,
                Message::ResizeSidebar
            )
        ]
        .height(Length::Fill);
        // Both wrappers are always present so the base keeps its state; toasts go
        // on top of everything, including the modal.
        let base = modal(
            body,
            self.show_modal.then(|| self.dialog()),
            Some(Message::ShowModal(false)),
        );
        toasts(base, &self.toasts, Message::CloseToast)
    }

    fn rail(&self) -> Element<'_, Message> {
        let ws = [
            ("Acme Corp", "acme", Some((7, BadgeKind::Mention))),
            ("Open Source Community", "oss", Some((0, BadgeKind::Unread))),
            ("Рабочий сервер", "work", None),
        ];
        let mut col = column![]
            .spacing(SPACE_3)
            .align_x(Alignment::Center)
            .padding([SPACE_3, 0.0]);
        for (i, (name, key, unread)) in ws.into_iter().enumerate() {
            col = col.push(workspace_rail_item(
                RailIcon::Avatar(AvatarSource::initials(name, key)),
                name,
                unread,
                i == self.workspace,
                Message::Workspace(i),
            ));
        }
        col = col.push(add_server_button("Add workspace", Message::Noop));
        col = col.push(space().height(Length::Fill));
        col = col.push(icon_button(
            if self.mode == theme::Mode::Light {
                Icon::Moon
            } else {
                Icon::Sun
            },
            Some(Message::ToggleMode),
            "Toggle theme",
        ));
        container(col)
            .width(RAIL_WIDTH)
            .height(Length::Fill)
            .style(style::container_rail)
            .into()
    }

    fn sidebar(&self) -> Element<'_, Message> {
        let me = avatar(
            AvatarSource::initials("Иван Петров", "ivan"),
            28.0,
            AvatarShape::Rounded,
        )
        .presence(Some(Presence::Online))
        .ring_with(|t| t.surface_sidebar);
        let top = row![
            me,
            space().width(Length::Fill),
            icon_button(Icon::House, Some(Message::Noop), "Home"),
            icon_button(Icon::Search, Some(Message::Noop), "Search"),
            icon_button(Icon::Users, Some(Message::Noop), "Directory"),
            popover_menu(
                icon_button(
                    Icon::Ellipsis,
                    Some(Message::PopoverOpen(!self.popover)),
                    "More"
                ),
                &[
                    MenuEntry::Header("Display".into()),
                    MenuItem::new("Condensed view", Message::Density(!self.condensed))
                        .icon(Icon::List)
                        .into(),
                    MenuItem::new("Sort by activity", Message::MenuAction("sort"))
                        .icon(Icon::Funnel)
                        .into(),
                    MenuEntry::Separator,
                    MenuItem::new("Preferences", Message::MenuAction("prefs"))
                        .icon(Icon::Settings)
                        .hint("Ctrl+,")
                        .into(),
                    MenuItem::new("Administration", Message::MenuAction("admin"))
                        .icon(Icon::Shield)
                        .disabled(true)
                        .into(),
                    MenuEntry::Separator,
                    MenuItem::new("Log out", Message::MenuAction("logout"))
                        .icon(Icon::LogOut)
                        .danger(true)
                        .into(),
                ],
                self.popover,
                Message::PopoverOpen(false),
                Placement::BelowEnd,
            ),
            icon_button_with(
                Icon::Pencil,
                Some(Message::ShowModal(true)),
                "Create new",
                false,
                32.0
            ),
        ]
        .spacing(2)
        .align_y(Alignment::Center)
        .padding([SPACE_2, SPACE_3]);

        let mut list = Column::new().spacing(1).padding([0.0, SPACE_2]);
        for section in SECTIONS {
            let items: Vec<(usize, &Room)> = self
                .rooms
                .iter()
                .enumerate()
                .filter(|(_, r)| r.section == section)
                .collect();
            let collapsed = self.collapsed.contains(section);
            list = list.push(section_header(
                section,
                collapsed,
                Some(items.len() as u32),
                Message::ToggleSection(section),
            ));
            if collapsed {
                continue;
            }
            for (i, r) in items {
                let mut p = RoomListItemProps::new(
                    AvatarSource::initials(r.name, r.name),
                    r.name,
                    Message::SelectRoom(i),
                );
                p.subtitle = Some(r.preview.into());
                p.time = Some(r.time.into());
                p.unread = r.unread;
                p.user_mentions = r.mentions;
                p.group_mentions = r.group;
                p.thread_unread = r.threads;
                p.is_muted = r.muted;
                p.is_selected = i == self.selected;
                p.presence = r.presence;
                p.icon = r.icon;
                p.typing = r.typing;
                p.typing_label = "typing…".into();
                p.density = if self.condensed {
                    Density::Condensed
                } else {
                    Density::Extended
                };
                let entries = vec![
                    MenuItem::new("Mark as read", Message::MenuAction("read"))
                        .icon(Icon::CheckCheck)
                        .into(),
                    MenuItem::new("Favorite", Message::MenuAction("fav"))
                        .icon(Icon::Star)
                        .into(),
                    MenuItem::new("Hide", Message::MenuAction("hide"))
                        .icon(Icon::EyeOff)
                        .into(),
                    MenuEntry::Separator,
                    MenuItem::new("Leave", Message::MenuAction("leave"))
                        .icon(Icon::LogOut)
                        .danger(true)
                        .into(),
                ];
                list = list.push(context_menu(room_list_item(p), entries));
            }
            list = list.push(space().height(SPACE_2));
        }

        column![
            top,
            status_strip(
                BannerKind::Warning,
                "Reconnecting…",
                Some(spinner(12.0)),
                Some(("Retry now".into(), Message::Noop))
            ),
            scrollable(list)
                .height(Length::Fill)
                .style(style::scrollable),
        ]
        .height(Length::Fill)
        .apply_style(style::container_sidebar)
    }

    fn room(&self) -> Element<'_, Message> {
        let r = &self.rooms[self.selected.min(self.rooms.len() - 1)];
        let header = header_bar(
            Some(
                avatar(
                    AvatarSource::initials(r.name, r.name),
                    32.0,
                    AvatarShape::Rounded,
                )
                .presence(r.presence)
                .into(),
            ),
            r.name,
            Some("Company-wide announcements and work-based matters · 1 248 members".into()),
            vec![
                icon_button(Icon::Phone, Some(Message::Noop), "Call"),
                icon_button(Icon::Video, Some(Message::Noop), "Video call"),
                icon_button(Icon::MessageCircle, Some(Message::Noop), "Threads"),
                icon_button(Icon::Info, Some(Message::Noop), "Room info"),
                icon_button(Icon::Search, Some(Message::Noop), "Search messages"),
                icon_button_with(Icon::Users, Some(Message::Noop), "Members", true, 32.0),
                icon_button(Icon::EllipsisVertical, Some(Message::Noop), "Kebab menu"),
            ],
        );

        let mut timeline = Column::new().spacing(0).padding([SPACE_2, 0.0]);
        timeline = timeline.push(date_separator("Yesterday"));
        timeline = timeline.push(system_message_row(
            Icon::User,
            rustket_ui::emoji::text_with_emoji("Anna Lee joined the channel 👋").size(TEXT_SMALL),
            "18:05",
        ));
        let bob = Msg {
            author: "Bob Smith",
            time: "18:12",
            body: "Here's the config we discussed:",
            reactions: vec![],
            role: None,
        };
        timeline = timeline.push(message_row(
            &bob,
            usize::MAX,
            Opts {
                extra: Some(code_block(
                    "[server]\nurl = \"https://chat.example.com\"\nreconnect = true",
                )),
                edited: true,
                ..Opts::default()
            },
        ));
        timeline = timeline.push(message_row(
            &Msg {
                body: "And the deploy script lives in ops/ — grouped messages hide the header; hover to see the time.",
                time: "18:13",
                ..bob.clone()
            },
            usize::MAX,
            Opts {
                sequential: true,
                pinned: true,
                thread: Some((
                    "4 replies",
                    "Last reply 19:02",
                    vec!["Anna Lee", "Иван Петров", "Carol Danvers"],
                )),
                ..Opts::default()
            },
        ));
        timeline = timeline.push(message_row(
            &Msg {
                author: "Anna Lee",
                time: "18:30",
                body: "",
                reactions: vec![("😂", 6, false)],
                role: None,
            },
            usize::MAX,
            Opts {
                extra: Some(quote("Bob Smith", "Here's the config we discussed:")),
                starred: true,
                discussion: true,
                ..Opts::default()
            },
        ));
        timeline = timeline.push(date_separator("Today"));
        for (i, m) in self.messages.iter().enumerate() {
            if i == 3 {
                timeline = timeline.push(unread_separator("Unread messages"));
            }
            timeline = timeline.push(message_row(
                m,
                i,
                Opts {
                    mention: m.body.contains("@ivan"),
                    ..Opts::default()
                },
            ));
        }
        let me = Msg {
            author: "Иван Петров",
            time: "10:44",
            body: "Сейчас посмотрю 👀",
            reactions: vec![],
            role: None,
        };
        timeline = timeline.push(message_row(
            &me,
            usize::MAX,
            Opts {
                own: true,
                ..Opts::default()
            },
        ));
        timeline = timeline.push(message_row(
            &Msg {
                body: "Отправляю файл… (pending)",
                time: "10:45",
                ..me.clone()
            },
            usize::MAX,
            Opts {
                own: true,
                sequential: true,
                pending: true,
                ..Opts::default()
            },
        ));
        timeline = timeline.push(message_row(
            &Msg {
                body: "This one failed to send",
                time: "10:45",
                ..me.clone()
            },
            usize::MAX,
            Opts {
                own: true,
                sequential: true,
                failed: true,
                ..Opts::default()
            },
        ));

        let composer = container(
            row![
                icon_button(Icon::Plus, Some(Message::Noop), "Add"),
                text_input("Message #general", &self.composer)
                    .on_input(Message::Composer)
                    .on_submit(Message::Send)
                    .style(style::text_input_bare)
                    .size(TEXT_BODY)
                    .padding([8, 4]),
                icon_button(Icon::Smile, Some(Message::Noop), "Emoji"),
                icon_button(Icon::AtSign, Some(Message::Noop), "Mention"),
                icon_button(Icon::Paperclip, Some(Message::Noop), "Upload file"),
                icon_button(Icon::Mic, Some(Message::Noop), "Audio message"),
                button(icon::sized(Icon::Send, 16.0).center())
                    .width(32)
                    .height(32)
                    .padding(0)
                    .style(style::button_primary)
                    .on_press_maybe((!self.composer.is_empty()).then_some(Message::Send)),
            ]
            .spacing(2)
            .align_y(Alignment::Center),
        )
        .padding([4, 6])
        .style(style::container_composer(false));
        let typing = row![
            text("Alice Johnson is typing…")
                .size(TEXT_CAPTION)
                .style(style::text_hint)
        ]
        .padding([2.0, SPACE_1]);

        let main = column![
            header,
            banner(
                BannerKind::Info,
                "This room is read-only for guests. Ask an admin to grant you write access.",
                Some(("Request access".into(), Message::Noop))
            )
            .apply_padding(),
            {
                let timeline = scrollable(timeline).height(Length::Fill);
                if env_flag("GALLERY_TOP") {
                    timeline
                } else {
                    timeline.anchor_bottom()
                }
                .style(style::scrollable)
            },
            container(column![composer, typing])
                .padding(iced::Padding::from([0.0, SPACE_4]).bottom(SPACE_2)),
        ]
        .height(Length::Fill);

        row![
            container(main)
                .width(Length::Fill)
                .style(style::container_room),
            vertical_divider(),
            self.kit_panel(),
        ]
        .into()
    }

    fn kit_panel(&self) -> Element<'_, Message> {
        let title = |s: &'static str| {
            text(s)
                .size(TEXT_CAPTION)
                .font(FONT_SEMIBOLD)
                .style(style::text_secondary)
        };
        let badges = row![
            badge(3, BadgeKind::Unread),
            badge(12, BadgeKind::Mention),
            badge(150, BadgeKind::GroupMention),
            badge(1, BadgeKind::Thread),
            badge(5, BadgeKind::Muted),
            dot(BadgeKind::Unread, 8.0),
        ]
        .spacing(SPACE_2)
        .align_y(Alignment::Center);
        let presences = row![
            presence_dot(Presence::Online, 10.0),
            text("Online").size(TEXT_CAPTION),
            presence_dot(Presence::Away, 10.0),
            text("Away").size(TEXT_CAPTION),
            presence_dot(Presence::Busy, 10.0),
            text("Busy").size(TEXT_CAPTION),
            presence_dot(Presence::Offline, 10.0),
            text("Offline").size(TEXT_CAPTION),
        ]
        .spacing(SPACE_1)
        .align_y(Alignment::Center);
        let avatars = row![
            avatar(
                AvatarSource::initials("Alice Johnson", "alice"),
                40.0,
                AvatarShape::Rounded
            )
            .presence(Some(Presence::Online))
            .ring_with(|t| t.surface_room),
            avatar(
                AvatarSource::initials("Иван Петров", "ivan"),
                40.0,
                AvatarShape::Circle
            )
            .presence(Some(Presence::Busy))
            .ring_with(|t| t.surface_room),
            avatar(
                AvatarSource::initials("🚀", "rocket"),
                40.0,
                AvatarShape::Rounded
            ),
            avatar(
                AvatarSource::initials("", "nobody"),
                40.0,
                AvatarShape::Rounded
            )
            .badge(badge(4, BadgeKind::Mention)),
            avatar(
                AvatarSource::initials("bob", "bob"),
                24.0,
                AvatarShape::Rounded
            ),
        ]
        .spacing(SPACE_3)
        .align_y(Alignment::Center);
        let buttons = column![
            row![
                primary_button("Save", Some(Message::Noop)),
                secondary_button("Cancel", Some(Message::Noop)),
                danger_button("Delete", Some(Message::Noop)).icon(Icon::Trash2),
            ]
            .spacing(SPACE_2),
            row![
                primary_button("Signing in", Some(Message::Noop)).loading(true),
                ghost_button("Ghost", Some(Message::Noop)),
                secondary_button("Disabled", None),
            ]
            .spacing(SPACE_2),
            row![
                primary_button("Small", Some(Message::Noop))
                    .size(ButtonSize::Small)
                    .icon(Icon::Check),
                secondary_button("Copy link", Some(Message::Noop))
                    .size(ButtonSize::Small)
                    .icon(Icon::Link),
                icon_button(Icon::Star, Some(Message::Noop), "Star"),
                icon_button(Icon::Pin, None, "Pin (disabled)"),
            ]
            .spacing(SPACE_2)
            .align_y(Alignment::Center),
        ]
        .spacing(SPACE_2);
        let chips = row![
            chip("Admin", ChipKind::Primary),
            chip("Owner", ChipKind::Success),
            chip("Bot", ChipKind::Neutral),
            chip("Banned", ChipKind::Danger),
            kbd("Ctrl"),
            kbd("K"),
        ]
        .spacing(SPACE_1)
        .align_y(Alignment::Center);
        let form = column![
            text_field("Username", &self.modal_name, Message::ModalName)
                .placeholder("e.g. alice")
                .help("Your @handle on this server"),
            text_field("Password", &self.modal_password, Message::ModalPassword)
                .password(true)
                .error(Some("Incorrect password".into())),
            pick_list(
                [Plan::Activity, Plan::Name, Plan::Unread],
                Some(self.plan),
                Message::Plan
            )
            .style(style::pick_list)
            .menu_style(style::menu)
            .width(Length::Fill)
            .padding([8, 12]),
            checkbox(self.condensed)
                .label("Condensed sidebar")
                .on_toggle(Message::Density)
                .style(style::checkbox),
            toggler(self.notify)
                .label("Desktop notifications")
                .on_toggle(Message::Notify)
                .style(style::toggler),
        ]
        .spacing(SPACE_3);
        let banners = column![
            banner(
                BannerKind::Error,
                "Certificate for chat.example.com is not trusted",
                Some(("Details".into(), Message::Noop))
            ),
            banner::<Message>(
                BannerKind::Success,
                "Two-factor authentication enabled",
                None
            ),
            banner::<Message>(BannerKind::Warning, "You are offline", None),
        ]
        .spacing(SPACE_2);
        let actions = row![
            secondary_button("Show modal", Some(Message::ShowModal(true))),
            secondary_button("Toast", Some(Message::AddToast)).icon(Icon::Bell),
        ]
        .spacing(SPACE_2);
        let content = column![
            title("AVATARS"),
            avatars,
            title("BADGES & PRESENCE"),
            badges,
            presences,
            title("BUTTONS"),
            buttons,
            title("TAGS"),
            chips,
            title("FORM"),
            form,
            title("BANNERS"),
            banners,
            title("OVERLAYS"),
            actions,
            title("MENU"),
            menu_panel(&[
                MenuItem::new("Reply in thread", Message::Noop)
                    .icon(Icon::MessageCircle)
                    .into(),
                MenuItem::new("Copy link", Message::Noop)
                    .icon(Icon::Link)
                    .hint("Ctrl+L")
                    .into(),
                MenuEntry::Separator,
                MenuItem::new("Delete", Message::Noop)
                    .icon(Icon::Trash2)
                    .danger(true)
                    .into(),
            ]),
            title("EMPTY STATE"),
            container(empty_state(
                Icon::Inbox,
                "No threads yet",
                "Threads started in this room will appear here."
            ))
            .height(200),
        ]
        .spacing(SPACE_3)
        .padding(SPACE_4);
        column![
            header_bar(
                Some(icon::sized(Icon::Users, 18.0).into()),
                "Components",
                None,
                vec![icon_button(Icon::X, Some(Message::Noop), "Close")]
            ),
            scrollable(content)
                .height(Length::Fill)
                .style(style::scrollable),
        ]
        .width(360)
        .apply_style(style::container_room)
    }

    fn dialog(&self) -> Element<'_, Message> {
        dialog(
            "Create channel",
            column![
                text(
                    "Channels are where your team communicates. Каналы — место для общения команды."
                )
                .size(TEXT_BODY)
                .style(style::text_secondary),
                text_field("Name", &self.modal_name, Message::ModalName)
                    .placeholder("e.g. marketing")
                    .on_submit(Message::ModalSubmit),
                text_field("Password", &self.modal_password, Message::ModalPassword)
                    .password(true)
                    .help("Optional: required to join"),
                toggler(true).label("Private").style(style::toggler),
            ]
            .spacing(SPACE_4),
            vec![
                secondary_button("Cancel", Some(Message::ShowModal(false))).into(),
                primary_button("Create", Some(Message::ModalSubmit)).into(),
            ],
            Some(Message::ShowModal(false)),
        )
    }
}

trait ApplyStyle<'a> {
    fn apply_style(self, f: fn(&Theme) -> container::Style) -> Element<'a, Message>;
    fn apply_padding(self) -> Element<'a, Message>;
}

impl<'a, T: Into<Element<'a, Message>>> ApplyStyle<'a> for T {
    fn apply_style(self, f: fn(&Theme) -> container::Style) -> Element<'a, Message> {
        container(self).height(Length::Fill).style(f).into()
    }

    fn apply_padding(self) -> Element<'a, Message> {
        container(self).padding([SPACE_2, SPACE_4]).into()
    }
}

fn code_block<'a>(code: &'a str) -> Element<'a, Message> {
    container(text(code).font(MONOSPACE_FONT).size(TEXT_SMALL))
        .padding([SPACE_2, SPACE_3])
        .width(Length::Fill)
        .style(style::container_code_block)
        .into()
}

fn quote<'a>(author: &'a str, body: &'a str) -> Element<'a, Message> {
    row![
        container(space().width(3))
            .height(Length::Fill)
            .style(style::container_quote_bar),
        container(
            column![
                text(author).size(TEXT_CAPTION).font(FONT_SEMIBOLD),
                text(body).size(TEXT_BODY).style(style::text_secondary)
            ]
            .spacing(2)
        )
        .padding([SPACE_2, SPACE_3])
        .width(Length::Fill)
        .style(style::container_quote),
    ]
    .height(Length::Shrink)
    .into()
}

#[derive(Default)]
struct Opts<'a> {
    extra: Option<Element<'a, Message>>,
    mention: bool,
    own: bool,
    sequential: bool,
    pending: bool,
    failed: bool,
    edited: bool,
    pinned: bool,
    starred: bool,
    thread: Option<(&'static str, &'static str, Vec<&'static str>)>,
    discussion: bool,
}

fn message_row<'a>(m: &Msg, index: usize, o: Opts<'a>) -> Element<'a, Message> {
    let author = m.author.to_owned();
    let body: Element<'a, Message> = if m.body.is_empty() {
        space().into()
    } else if rustket_ui::emoji::is_emoji_only(m.body) {
        emoji_text(m.body, TEXT_EMOJI_LARGE).into()
    } else {
        rustket_ui::emoji::text_with_emoji(m.body)
            .size(TEXT_BODY)
            .into()
    };
    let mut p = MessageRowProps::new(
        avatar(
            AvatarSource::initials(author.clone(), author.clone()),
            MESSAGE_AVATAR,
            AvatarShape::Rounded,
        ),
        author,
        m.time,
        body,
    );
    if let Some(role) = m.role {
        p.author_badges.push(chip(
            role,
            if role == "Bot" {
                ChipKind::Neutral
            } else {
                ChipKind::Primary
            },
        ));
    }
    if let Some(extra) = o.extra {
        p.attachments.push(extra);
    }
    if !m.reactions.is_empty() {
        let mut reactions = row![].spacing(SPACE_1).align_y(Alignment::Center);
        for (k, (emoji, count, mine)) in m.reactions.iter().enumerate() {
            let on_press = (k == 0 && index != usize::MAX).then_some(Message::React(index));
            reactions = reactions.push(reaction_pill(
                emoji_text(*emoji, 14.0),
                *count,
                *mine,
                on_press.or(Some(Message::Noop)),
            ));
        }
        reactions = reactions.push(icon_button_with(
            Icon::SmilePlus,
            Some(Message::Noop),
            "Add reaction",
            false,
            24.0,
        ));
        p.reactions = Some(reactions.into());
    }
    if let Some((count, last, people)) = o.thread {
        p.thread_summary = Some(thread_summary(
            count,
            last,
            people
                .into_iter()
                .map(|n| avatar(AvatarSource::initials(n, n), 20.0, AvatarShape::Rounded).into())
                .collect(),
            true,
            Message::MenuAction("open thread"),
        ));
    }
    if o.discussion {
        p.thread_summary = Some(discussion_summary(
            "12 messages",
            "Last message 20:15",
            Message::MenuAction("open discussion"),
        ));
    }
    p.is_sequential = o.sequential;
    p.is_mention_highlight = o.mention;
    p.is_own_highlight = o.own;
    p.is_pending = o.pending;
    p.is_failed = o.failed;
    p.edited = o.edited;
    p.pinned = o.pinned;
    p.starred = o.starred;
    if o.failed {
        p.retry = Some(("Retry".into(), Message::MenuAction("retry")));
    }
    p.actions_toolbar = Some(message_toolbar(vec![
        icon_button_with(
            Icon::SmilePlus,
            Some(Message::Noop),
            "Add reaction",
            false,
            28.0,
        ),
        icon_button_with(
            Icon::MessageCircle,
            Some(Message::Noop),
            "Reply in thread",
            false,
            28.0,
        ),
        icon_button_with(Icon::Star, Some(Message::Noop), "Star", false, 28.0),
        icon_button_with(
            Icon::EllipsisVertical,
            Some(Message::Noop),
            "More",
            false,
            28.0,
        ),
    ]));
    let entries = vec![
        MenuItem::new("Reply in thread", Message::MenuAction("reply"))
            .icon(Icon::MessageCircle)
            .into(),
        MenuItem::new("Quote", Message::MenuAction("quote"))
            .icon(Icon::Reply)
            .into(),
        MenuItem::new("Copy text", Message::MenuAction("copy"))
            .icon(Icon::Copy)
            .into(),
        MenuItem::new("Pin", Message::MenuAction("pin"))
            .icon(Icon::Pin)
            .into(),
        MenuEntry::Separator,
        MenuItem::new("Delete", Message::MenuAction("delete"))
            .icon(Icon::Trash2)
            .danger(true)
            .into(),
    ];
    context_menu(rustket_ui::widget::message_row(p), entries)
}
