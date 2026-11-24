//! Left rail: one icon per server with unread badges, add-server and settings
//! buttons. Under the Rocket.Chat scheme the badges and tooltips follow the
//! desktop app's workspace tabs. Strings: `locales/rail.yml`.

use iced::widget::{Column, column, container, scrollable, space, stack};
use iced::{Alignment, Element, Length, Theme, border};
use rust_i18n::t;
use rustket_core::ServerId;
use rustket_core::indicators::RcSignal;
use rustket_core::room_list::WorkspaceSignal;
use rustket_ui::icon::{self, Icon};
use rustket_ui::style;
use rustket_ui::theme::{IndicatorLook, RAIL_WIDTH, SPACE_2, SPACE_3, indicator_look, tokens};
use rustket_ui::widget::{
    AvatarSource, BadgeKind, RailIcon, WorkspaceBadge, add_server_button, icon_button,
    workspace_rail_item_with,
};

use crate::room::render::plural;

#[derive(Debug, Clone)]
pub enum Message {
    Select(ServerId),
    Add,
    Settings,
}

pub struct RailEntry {
    pub id: ServerId,
    pub name: String,
    pub avatar: AvatarSource,
    /// The account's unread signal (`room_list::workspace_signal`).
    pub signal: WorkspaceSignal,
    /// The account's Rocket.Chat signal (`indicators::rc_workspace_signal`).
    pub rc: RcSignal,
    pub selected: bool,
    /// Logged out (token rejected, signed out by the server, never signed
    /// in): nothing arrives from this server until the user signs in.
    pub signed_out: bool,
}

/// The badge of a workspace item (rail or title bar): a red counter of the
/// mentions, else a dot in the colour of the strongest counted unread row
/// (orange `@all`, unread blue, thread slate), else nothing.
pub fn badge(signal: WorkspaceSignal) -> Option<(u32, BadgeKind)> {
    if signal.mentions > 0 {
        Some((
            crate::format::count_u32(signal.mentions),
            BadgeKind::Mention,
        ))
    } else {
        signal
            .strongest
            .map(|k| (0, crate::workspace::sidebar::widget_kind(k)))
    }
}

/// The badge of a workspace item under the Rocket.Chat scheme, the desktop
/// app's tab badge: a ghost counter of the account's unread (`99+`), else a
/// ghost bullet for an alert, else nothing.
pub fn badge_rc(rc: RcSignal) -> WorkspaceBadge {
    if rc.unread > 0 {
        WorkspaceBadge::Ghost(u32::try_from(rc.unread).unwrap_or(u32::MAX))
    } else if rc.bullet {
        WorkspaceBadge::GhostBullet
    } else {
        WorkspaceBadge::None
    }
}

/// Tooltip text of a workspace item (rail or title bar).
pub fn label(name: String, signed_out: bool) -> String {
    if signed_out {
        t!("rail.signed_out", name = name).into_owned()
    } else {
        name
    }
}

/// [`label`] with the desktop app's unread suffix (Rocket.Chat scheme):
/// " — N messages" (the full number), or " — Unread messages" for the
/// bullet.
pub fn label_rc(name: String, signed_out: bool, rc: RcSignal) -> String {
    let name = label(name, signed_out);
    if rc.unread > 0 {
        let key = format!("rail.rc_suffix_count_{}", plural(rc.unread));
        t!(&key, name = name, count = rc.unread).into_owned()
    } else if rc.bullet {
        t!("rail.rc_suffix_alert", name = name).into_owned()
    } else {
        name
    }
}

/// The badge and tooltip of `e` in `look`.
pub fn entry_badge_label(e: &RailEntry, look: IndicatorLook) -> (WorkspaceBadge, String) {
    match look {
        IndicatorLook::Rustket => (badge(e.signal).into(), label(e.name.clone(), e.signed_out)),
        IndicatorLook::RocketChat => (badge_rc(e.rc), label_rc(e.name.clone(), e.signed_out, e.rc)),
    }
}

/// A small "signed out" emblem on the bottom-right corner of the face, or
/// an empty placeholder when `shown` is false.
fn signed_out_marker(item: Element<'_, Message>, shown: bool) -> Element<'_, Message> {
    if !shown {
        return stack![item, iced::widget::space()].clip(false).into();
    }
    let emblem = container(icon::sized(Icon::LogOut, 10.0))
        .center(16)
        .style(|theme: &Theme| {
            let t = tokens(theme);
            container::Style {
                text_color: Some(t.on_danger),
                background: Some(t.danger.into()),
                border: border::rounded(8).width(2).color(t.surface_rail),
                ..container::Style::default()
            }
        });
    stack![
        item,
        container(emblem)
            .width(Length::Fill)
            .height(Length::Fill)
            .align_x(Alignment::End)
            .align_y(Alignment::End)
            .padding(iced::Padding::ZERO.right(6.0).bottom(-2.0)),
    ]
    .clip(false)
    .into()
}

pub fn view<'a>(entries: Vec<RailEntry>) -> Element<'a, Message> {
    let look = indicator_look();
    let items = entries.into_iter().map(|e| {
        let (badge, name) = entry_badge_label(&e, look);
        let item = workspace_rail_item_with(
            RailIcon::Avatar(e.avatar),
            name,
            badge,
            e.selected,
            Message::Select(e.id),
        );
        // Always wrapped, so the item keeps its widget state when the
        // server signs out or back in.
        signed_out_marker(item, e.signed_out)
    });
    let list = Column::with_children(items)
        .push(add_server_button(t!("rail.add_server"), Message::Add))
        .spacing(SPACE_3)
        .align_x(Alignment::Start)
        // Inside the scrollable so the badges drawn above the first item
        // (offset -4 px) are not clipped by its viewport.
        .padding(iced::Padding::ZERO.top(SPACE_2));
    container(
        column![
            scrollable(list)
                .height(Length::Fill)
                .style(style::scrollable),
            container(icon_button(
                Icon::Settings,
                Some(Message::Settings),
                t!("rail.settings")
            ))
            .center_x(Length::Fill),
            space().height(SPACE_2),
        ]
        .spacing(SPACE_2)
        .padding(iced::Padding::ZERO.top(SPACE_3 - SPACE_2).bottom(SPACE_3)),
    )
    .width(RAIL_WIDTH)
    .height(Length::Fill)
    .style(style::container_rail)
    .into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustket_core::room_list::BadgeVariant;

    fn signal(mentions: u64, strongest: Option<BadgeVariant>) -> WorkspaceSignal {
        WorkspaceSignal {
            mentions,
            strongest,
            rooms: u64::from(strongest.is_some()),
        }
    }

    /// Items 3/4/7: a red count of mentions, else a dot in the strongest
    /// counted colour, else nothing.
    #[test]
    fn workspace_badge() {
        assert_eq!(badge(WorkspaceSignal::default()), None);
        assert_eq!(
            badge(signal(3, Some(BadgeVariant::Mention))),
            Some((3, BadgeKind::Mention))
        );
        assert_eq!(
            badge(signal(0, Some(BadgeVariant::GroupMention))),
            Some((0, BadgeKind::GroupMention))
        );
        assert_eq!(
            badge(signal(0, Some(BadgeVariant::Unread))),
            Some((0, BadgeKind::Unread))
        );
        assert_eq!(
            badge(signal(0, Some(BadgeVariant::Thread))),
            Some((0, BadgeKind::Thread))
        );
    }

    /// Rocket.Chat: a ghost counter (`99+` in the widget), else the ghost
    /// bullet, else nothing; the tooltip gets the desktop app's suffix with
    /// the full number.
    #[test]
    fn rc_workspace_badge_and_tooltip() {
        rust_i18n::set_locale("en");
        let rc = |unread, bullet| RcSignal { unread, bullet };
        assert_eq!(badge_rc(rc(0, false)), WorkspaceBadge::None);
        assert_eq!(badge_rc(rc(0, true)), WorkspaceBadge::GhostBullet);
        assert_eq!(badge_rc(rc(5, true)), WorkspaceBadge::Ghost(5));
        assert_eq!(badge_rc(rc(150, false)), WorkspaceBadge::Ghost(150));
        assert_eq!(
            badge_rc(rc(u64::MAX, false)),
            WorkspaceBadge::Ghost(u32::MAX)
        );
        assert_eq!(label_rc("Acme".into(), false, rc(0, false)), "Acme");
        assert_eq!(
            label_rc("Acme".into(), false, rc(1, false)),
            "Acme — 1 message"
        );
        assert_eq!(
            label_rc("Acme".into(), false, rc(150, false)),
            "Acme — 150 messages"
        );
        assert_eq!(
            label_rc("Acme".into(), false, rc(0, true)),
            "Acme — Unread messages"
        );
        assert_eq!(
            label_rc("Acme".into(), true, rc(2, false)),
            "Acme (signed out) — 2 messages"
        );
        // Russian plural without switching the process-wide locale.
        assert_eq!(crate::room::render::plural_for("ru", 5), "many");
        assert_eq!(
            t!(
                "rail.rc_suffix_count_many",
                locale = "ru",
                name = "Acme",
                count = 5
            ),
            "Acme — 5 сообщений"
        );
    }

    /// Each look reads its own signal.
    #[test]
    fn entry_badge_follows_the_look() {
        rust_i18n::set_locale("en");
        let e = RailEntry {
            id: ServerId("s".into()),
            name: "Acme".into(),
            avatar: AvatarSource::initials("Acme", "https://acme.example"),
            signal: signal(2, Some(BadgeVariant::Mention)),
            rc: RcSignal {
                unread: 7,
                bullet: false,
            },
            selected: false,
            signed_out: false,
        };
        assert_eq!(
            entry_badge_label(&e, IndicatorLook::Rustket),
            (WorkspaceBadge::Kind(2, BadgeKind::Mention), "Acme".into())
        );
        assert_eq!(
            entry_badge_label(&e, IndicatorLook::RocketChat),
            (WorkspaceBadge::Ghost(7), "Acme — 7 messages".into())
        );
    }
}
