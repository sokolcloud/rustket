//! Autocomplete popup above the composer: `@` users, `#` rooms, `:` emoji and
//! `/` commands. Matching is local (room members, rooms of the workspace,
//! emoji tables, cached commands); server results are merged in as they
//! arrive and stale ones are dropped by sequence number.

use iced::widget::{button, column, container, row, scrollable, text};
use iced::{Alignment, Element, Length};
use rocketchat::models::{RoomSummary, RoomType, SlashCommand, UserStatus, UserSummary};
use rust_i18n::t;
use rustket_core::Workspace;
use rustket_core::emoji::ResolvedEmoji;
use rustket_core::permissions;
use rustket_core::room_list::Avatar;
use rustket_ui::theme::{SPACE_1, SPACE_2, TEXT_CAPTION, TEXT_SMALL};
use rustket_ui::widget::{AvatarShape, avatar, emoji_text};
use rustket_ui::{Icon, icon, style};

use super::text::{Trigger, TriggerKind};
use crate::room::ViewCtx;

/// Maximum entries in the popup.
pub const MAX_ITEMS: usize = 10;

#[derive(Debug, Clone, PartialEq)]
pub enum Item {
    User {
        username: String,
        name: Option<String>,
        status: UserStatus,
    },
    /// `@all` / `@here`.
    Special(&'static str),
    Room {
        name: String,
        t: RoomType,
    },
    Emoji {
        key: String,
        matched: String,
        emoji: ResolvedEmoji,
    },
    Command {
        command: String,
        params: Option<String>,
        description: Option<String>,
    },
}

impl Item {
    /// Text that replaces the trigger token.
    pub fn replacement(&self) -> String {
        match self {
            Self::User { username, .. } => format!("@{username}"),
            Self::Special(s) => format!("@{s}"),
            Self::Room { name, .. } => format!("#{name}"),
            Self::Emoji { key, .. } => key.clone(),
            Self::Command { command, .. } => format!("/{command}"),
        }
    }

    fn identity(&self) -> String {
        self.replacement().to_lowercase()
    }
}

/// The open popup.
#[derive(Debug, Clone)]
pub struct Popup {
    pub trigger: Trigger,
    pub items: Vec<Item>,
    pub selected: usize,
    /// Sequence of the server query this popup waits for.
    pub seq: u64,
    /// Items from the server for `trigger.query`, merged after local ones.
    pub remote: Vec<Item>,
}

impl Popup {
    pub fn new(trigger: Trigger, seq: u64) -> Self {
        Self {
            trigger,
            items: Vec::new(),
            selected: 0,
            seq,
            remote: Vec::new(),
        }
    }

    pub fn move_selection(&mut self, delta: i32) {
        let n = self.items.len();
        if n == 0 {
            return;
        }
        let cur = self.selected as i64 + i64::from(delta);
        self.selected = cur.rem_euclid(n as i64) as usize;
    }

    pub fn current(&self) -> Option<&Item> {
        self.items.get(self.selected)
    }

    /// Replaces the items with `local` followed by the remote ones not
    /// already listed, keeping the selection on the same entry if possible.
    pub fn set_items(&mut self, local: Vec<Item>) {
        let keep = self.current().map(Item::identity);
        let mut items = local;
        for r in &self.remote {
            if !items.iter().any(|i| i.identity() == r.identity()) {
                items.push(r.clone());
            }
        }
        items.truncate(MAX_ITEMS);
        self.selected = keep
            .and_then(|k| items.iter().position(|i| i.identity() == k))
            .unwrap_or(0);
        self.items = items;
    }
}

/// Lowercase match rank of `query` in `hay`: 0 prefix, 1 word prefix,
/// 2 substring.
fn rank(hay: &str, query: &str) -> Option<u8> {
    if query.is_empty() {
        return Some(0);
    }
    let h = hay.to_lowercase();
    if h.starts_with(query) {
        Some(0)
    } else if h
        .split(|c: char| !c.is_alphanumeric())
        .any(|w| w.starts_with(query))
    {
        Some(1)
    } else if h.contains(query) {
        Some(2)
    } else {
        None
    }
}

/// `@` candidates: `@all`/`@here` (when permitted) and room members by
/// username or name; the current user is left out.
pub fn match_users(ws: &Workspace, rid: &str, members: &[UserSummary], query: &str) -> Vec<Item> {
    let q = query.to_lowercase();
    let mut out = Vec::new();
    for (special, allowed) in [
        ("all", permissions::can_mention_all(ws, rid)),
        ("here", permissions::can_mention_here(ws, rid)),
    ] {
        if allowed && special.starts_with(&q) {
            out.push(Item::Special(special));
        }
    }
    let me = ws.username();
    let mut ranked: Vec<(u8, usize, Item)> = members
        .iter()
        .enumerate()
        .filter_map(|(i, m)| {
            let username = m.username.as_deref()?;
            if Some(username) == me {
                return None;
            }
            let r = rank(username, &q)
                .into_iter()
                .chain(m.name.as_deref().and_then(|n| rank(n, &q)))
                .min()?;
            let status = ws
                .presence()
                .get(&m.id)
                .map(|p| p.status)
                .or(m.status)
                .unwrap_or_default();
            Some((
                r,
                i,
                Item::User {
                    username: username.to_owned(),
                    name: m.name.clone(),
                    status,
                },
            ))
        })
        .collect();
    ranked.sort_by_key(|(r, i, _)| (*r, *i));
    out.extend(ranked.into_iter().map(|(_, _, it)| it));
    out.truncate(MAX_ITEMS);
    out
}

/// Server users (`users.autocomplete`) as items.
pub fn remote_users(ws: &Workspace, list: Vec<UserSummary>) -> Vec<Item> {
    let me = ws.username();
    list.into_iter()
        .filter_map(|u| {
            let username = u.username?;
            (Some(username.as_str()) != me).then_some(Item::User {
                username,
                name: u.name,
                status: u.status.unwrap_or_default(),
            })
        })
        .collect()
}

/// `#` candidates among the workspace's channels and private groups.
pub fn match_rooms(ws: &Workspace, query: &str) -> Vec<Item> {
    let q = query.to_lowercase();
    let mut ranked: Vec<(u8, String, RoomType)> = ws
        .rooms()
        .values()
        .filter(|e| matches!(e.room_type(), RoomType::Channel | RoomType::Private))
        .filter(|e| !e.is_discussion())
        .filter_map(|e| {
            let name = e.name();
            if name.is_empty() {
                return None;
            }
            rank(name, &q).map(|r| (r, name.to_owned(), e.room_type()))
        })
        .collect();
    ranked.sort_by(|a, b| (a.0, &a.1).cmp(&(b.0, &b.1)));
    ranked
        .into_iter()
        .take(MAX_ITEMS)
        .map(|(_, name, t)| Item::Room { name, t })
        .collect()
}

/// Server rooms (`rooms.autocomplete.channelAndPrivate`) as items.
pub fn remote_rooms(list: Vec<RoomSummary>) -> Vec<Item> {
    list.into_iter()
        .filter_map(|r| {
            Some(Item::Room {
                name: r.name?,
                t: r.t,
            })
        })
        .collect()
}

/// `:` candidates from the emoji tables (custom emoji included).
pub fn match_emoji(ws: &Workspace, query: &str) -> Vec<Item> {
    ws.emoji()
        .search(query, MAX_ITEMS)
        .into_iter()
        .map(|m| Item::Emoji {
            key: m.key,
            matched: m.matched,
            emoji: m.emoji,
        })
        .collect()
}

/// `/` candidates by command prefix, limited to commands the user may run in
/// room `rid` (the web client's `hasAtLeastOnePermission(command.permission)`).
pub fn match_commands(
    ws: &Workspace,
    rid: &str,
    commands: &[SlashCommand],
    query: &str,
) -> Vec<Item> {
    let q = query.to_lowercase();
    let mut list: Vec<&SlashCommand> = commands
        .iter()
        .filter(|c| c.command.to_lowercase().starts_with(&q))
        .filter(|c| may_run(ws, rid, c))
        .collect();
    list.sort_by(|a, b| a.command.cmp(&b.command));
    list.into_iter()
        .take(MAX_ITEMS)
        .map(|c| Item::Command {
            command: c.command.clone(),
            params: c.params.clone().filter(|p| !p.is_empty()),
            description: c.description.clone().filter(|d| !d.is_empty()),
        })
        .collect()
}

/// Whether the kind asks the server too.
pub fn wants_remote(kind: TriggerKind) -> bool {
    matches!(kind, TriggerKind::User | TriggerKind::Room)
}

/// Whether the user holds one of the command's permissions. Permissions not
/// loaded yet do not hide anything (the server checks again on run).
fn may_run(ws: &Workspace, rid: &str, c: &SlashCommand) -> bool {
    let perms: Vec<&str> = match &c.permission {
        Some(serde_json::Value::String(p)) => vec![p.as_str()],
        Some(serde_json::Value::Array(a)) => a.iter().filter_map(|v| v.as_str()).collect(),
        _ => Vec::new(),
    };
    perms.is_empty()
        || perms.iter().all(|p| ws.permission_roles(p).is_none())
        || permissions::has_any_permission(ws, &perms, Some(rid))
}

/// Command descriptions/params are mostly i18n keys of the server
/// (`Slash_Gimme_Description`, `your_message_optional`): known keys are
/// translated (`room_composer.cmd.<key>`), unknown keys are hidden, prose
/// is shown as is.
fn command_text(s: &str) -> Option<String> {
    let key = format!("room_composer.cmd.{s}");
    let translated = crate::i18n::tr_dyn(&key, "");
    if !translated.is_empty() {
        return Some(translated);
    }
    let looks_like_key =
        !s.contains(' ') && (s.contains('_') || s.chars().all(char::is_alphanumeric));
    let is_hint = s.starts_with('#') || s.starts_with('@');
    (is_hint || !looks_like_key).then(|| s.to_owned())
}

pub fn view<'a, M: Clone + 'a>(
    popup: &'a Popup,
    ctx: &ViewCtx<'a>,
    on_select: impl Fn(usize) -> M,
) -> Element<'a, M> {
    let title = match popup.trigger.kind {
        TriggerKind::User => t!("room_composer.ac.people"),
        TriggerKind::Room => t!("room_composer.ac.rooms"),
        TriggerKind::Emoji => t!("room_composer.ac.emoji"),
        TriggerKind::Command => t!("room_composer.ac.commands"),
    };
    let mut list = column![
        container(text(title).size(TEXT_CAPTION).style(style::text_secondary))
            .padding([SPACE_1, SPACE_2])
    ]
    .spacing(0);
    if popup.items.is_empty() {
        list = list.push(
            container(
                text(t!("room_composer.ac.no_results"))
                    .size(TEXT_SMALL)
                    .style(style::text_hint),
            )
            .padding([SPACE_1, SPACE_2]),
        );
    }
    for (i, item) in popup.items.iter().enumerate() {
        let content = item_view(item, ctx);
        list = list.push(
            button(content)
                .width(Length::Fill)
                .padding([SPACE_1, SPACE_2])
                .style(style::button_list_item(i == popup.selected))
                .on_press(on_select(i)),
        );
    }
    container(
        scrollable(list.padding(SPACE_1))
            .height(Length::Shrink)
            .style(style::scrollable),
    )
    .width(Length::Fixed(420.0))
    .max_height(360.0)
    .style(style::container_popover)
    .into()
}

fn item_view<'a, M: 'a>(item: &'a Item, ctx: &ViewCtx<'a>) -> Element<'a, M> {
    match item {
        Item::User {
            username,
            name,
            status,
        } => {
            let label = name.as_deref().unwrap_or(username);
            let src = ctx.images.avatar(
                ctx.server_id,
                ctx.server,
                &Avatar::User {
                    username: username.clone(),
                },
                label,
            );
            row![
                avatar(src, 24.0, AvatarShape::Rounded)
                    .presence(Some(crate::workspace::sidebar::presence(*status))),
                text(label).size(TEXT_SMALL),
                text(format!("@{username}"))
                    .size(TEXT_CAPTION)
                    .style(style::text_secondary),
            ]
            .spacing(SPACE_2)
            .align_y(Alignment::Center)
            .into()
        }
        Item::Special(s) => {
            let hint = if *s == "all" {
                t!("room_composer.ac.all_hint")
            } else {
                t!("room_composer.ac.here_hint")
            };
            row![
                container(icon(Icon::Users).size(16.0)).center_x(24.0),
                text(format!("@{s}")).size(TEXT_SMALL),
                text(hint).size(TEXT_CAPTION).style(style::text_secondary),
            ]
            .spacing(SPACE_2)
            .align_y(Alignment::Center)
            .into()
        }
        Item::Room { name, t } => row![
            container(
                icon(if *t == RoomType::Private {
                    Icon::Lock
                } else {
                    Icon::Hash
                })
                .size(16.0)
            )
            .center_x(24.0),
            text(name.as_str()).size(TEXT_SMALL),
        ]
        .spacing(SPACE_2)
        .align_y(Alignment::Center)
        .into(),
        Item::Emoji {
            key,
            matched: _,
            emoji,
        } => row![
            container(emoji_glyph(emoji, ctx, 20.0)).center_x(24.0),
            text(key.as_str()).size(TEXT_SMALL),
        ]
        .spacing(SPACE_2)
        .align_y(Alignment::Center)
        .into(),
        Item::Command {
            command,
            params,
            description,
        } => {
            let mut head = row![text(format!("/{command}")).size(TEXT_SMALL)]
                .spacing(SPACE_2)
                .align_y(Alignment::Center);
            if let Some(p) = params.as_deref().and_then(command_text) {
                head = head.push(text(p).size(TEXT_CAPTION).style(style::text_hint));
            }
            let mut col = column![head].spacing(0);
            if let Some(d) = description.as_deref().and_then(command_text) {
                col = col.push(text(d).size(TEXT_CAPTION).style(style::text_secondary));
            }
            col.into()
        }
    }
}

/// A Unicode emoji glyph or a custom emoji image (placeholder until loaded).
pub fn emoji_glyph<'a, M: 'a>(
    emoji: &ResolvedEmoji,
    ctx: &ViewCtx<'a>,
    size: f32,
) -> Element<'a, M> {
    emoji_glyph_with(emoji, ctx.server_id, ctx.server, ctx.images, size)
}

pub fn emoji_glyph_with<'a, M: 'a>(
    emoji: &ResolvedEmoji,
    server_id: &rustket_core::ServerId,
    server: &rocketchat::ServerUrl,
    images: &crate::images::Images,
    size: f32,
) -> Element<'a, M> {
    match emoji {
        ResolvedEmoji::Unicode(u) => emoji_text(*u, size).into(),
        custom => {
            let handle = custom
                .custom_url(server)
                .and_then(|url| images.get(server_id, &url))
                .map(|l| l.handle.clone());
            match handle {
                Some(h) => iced::widget::image(h)
                    .width(Length::Fixed(size))
                    .height(Length::Fixed(size))
                    .into(),
                None => container(text(""))
                    .width(Length::Fixed(size))
                    .height(Length::Fixed(size))
                    .into(),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use rocketchat::models::{Permission, User};
    use rustket_core::Change;
    use rustket_core::sync::InitialData;

    fn member(id: &str, username: &str, name: &str) -> UserSummary {
        UserSummary {
            id: id.into(),
            username: Some(username.into()),
            name: Some(name.into()),
            ..Default::default()
        }
    }

    fn ws_with(roles: &[&str]) -> Workspace {
        let mut ws = Workspace::new();
        ws.apply_initial(InitialData {
            me: User {
                id: "me".into(),
                username: Some("me".into()),
                roles: Some(roles.iter().map(|r| (*r).to_owned()).collect()),
                ..Default::default()
            },
            ..Default::default()
        });
        for p in [
            permissions::ids::MENTION_ALL,
            permissions::ids::MENTION_HERE,
        ] {
            ws.apply_permission(
                Change::Updated,
                Permission {
                    id: p.into(),
                    roles: vec!["admin".into()],
                    updated_at: None,
                },
            );
        }
        ws
    }

    #[test]
    fn users_rank_prefix_then_word_then_substring() {
        let ws = ws_with(&["user"]);
        let members = vec![
            member("1", "zed", "Bob Alpha"),
            member("2", "alice", "Alice A"),
            member("3", "malina", "M"),
            member("4", "me", "Me"),
        ];
        let items = match_users(&ws, "r", &members, "al");
        let names: Vec<String> = items.iter().map(Item::replacement).collect();
        assert_eq!(names, ["@alice", "@zed", "@malina"]);
        // Empty query lists everyone except me, no @all without permission.
        let all = match_users(&ws, "r", &members, "");
        assert_eq!(all.len(), 3);
    }

    #[test]
    fn all_and_here_need_permissions() {
        let ws = ws_with(&["admin"]);
        let items = match_users(&ws, "r", &[], "");
        assert_eq!(items, [Item::Special("all"), Item::Special("here")]);
        let items = match_users(&ws, "r", &[], "he");
        assert_eq!(items, [Item::Special("here")]);
        let ws = ws_with(&["user"]);
        assert!(match_users(&ws, "r", &[], "a").is_empty());
    }

    #[test]
    fn remote_items_merge_without_duplicates() {
        let trigger = super::super::text::trigger_at("@a", 2).unwrap();
        let mut p = Popup::new(trigger, 1);
        p.set_items(vec![Item::Special("all")]);
        p.move_selection(1);
        assert_eq!(p.selected, 0, "single item wraps to itself");
        p.remote = vec![
            Item::User {
                username: "ann".into(),
                name: None,
                status: UserStatus::Online,
            },
            Item::Special("all"),
        ];
        p.set_items(vec![Item::Special("all")]);
        assert_eq!(p.items.len(), 2);
        p.move_selection(1);
        assert_eq!(p.current().unwrap().replacement(), "@ann");
        // Selection follows the entry when the list changes.
        p.set_items(vec![]);
        assert_eq!(p.current().unwrap().replacement(), "@ann");
        p.move_selection(-1);
        assert_eq!(p.current().unwrap().replacement(), "@all");
    }

    #[test]
    fn commands_by_prefix() {
        let cmds = vec![
            SlashCommand {
                command: "leave".into(),
                ..Default::default()
            },
            SlashCommand {
                command: "lennyface".into(),
                description: Some("Slash_LennyFace_Description".into()),
                ..Default::default()
            },
            SlashCommand {
                command: "invite".into(),
                ..Default::default()
            },
        ];
        let ws = Workspace::new();
        let items = match_commands(&ws, "r", &cmds, "le");
        let names: Vec<String> = items.iter().map(Item::replacement).collect();
        assert_eq!(names, ["/leave", "/lennyface"]);
        rust_i18n::set_locale("en");
        assert_eq!(
            command_text("Slash_Shrug_Description").as_deref(),
            Some("Displays ¯\\_(ツ)_/¯ after your message")
        );
        assert_eq!(
            command_text("your_message_optional").as_deref(),
            Some("your message (optional)")
        );
        assert_eq!(command_text("Some_Unknown_Key"), None);
        assert_eq!(command_text("#channel"), Some("#channel".into()));
        assert_eq!(command_text("Invite a user"), Some("Invite a user".into()));
    }

    #[test]
    fn emoji_items() {
        let ws = Workspace::new();
        let items = match_emoji(&ws, "thumbsu");
        assert!(matches!(&items[0], Item::Emoji { key, .. } if key == ":thumbsup:"));
    }
}
