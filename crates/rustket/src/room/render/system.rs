//! System messages (`t`): localised text and icon for every Rocket.Chat 8.9
//! message type (`packages/message-types` registrations plus the legacy
//! role/discussion types); unknown types get a generic text.

use rocketchat::models::Message;
use rust_i18n::t;
use rustket_ui::icon::Icon;
use rustket_ui::widget::sanitize_bidi;

/// Author of a system message as shown in its text.
fn actor(m: &Message, real_names: bool) -> String {
    let name = if real_names {
        m.u.name.as_deref().filter(|n| !n.trim().is_empty())
    } else {
        None
    };
    sanitize_bidi(name.or(m.u.username.as_deref()).unwrap_or_default()).into_owned()
}

/// The localised text of system message `m`.
pub fn system_text(m: &Message, real_names: bool) -> String {
    system_text_in(m, real_names, &rust_i18n::locale())
}

/// [`system_text`] in locale `loc`.
pub fn system_text_in(m: &Message, real_names: bool, loc: &str) -> String {
    let user = actor(m, real_names);
    let msg = sanitize_bidi(&m.msg);
    let role = sanitize_bidi(m.role.as_deref().unwrap_or_default());
    let none = || t!("room_render.sys.none", locale = loc).into_owned();
    let or_none = |s: &str| {
        if s.trim().is_empty() {
            none()
        } else {
            s.to_owned()
        }
    };
    let kind = m.t.as_deref().unwrap_or_default();
    let s = match kind {
        "uj" => t!("room_render.sys.uj", locale = loc, user = user),
        "ujt" => t!("room_render.sys.ujt", locale = loc, user = user),
        "ut" => t!("room_render.sys.ut", locale = loc, user = user),
        "ul" => t!("room_render.sys.ul", locale = loc, user = user),
        "ult" => t!("room_render.sys.ult", locale = loc, user = user),
        "wm" => t!("room_render.sys.wm", locale = loc, user = user),
        "r" => t!("room_render.sys.r", locale = loc, user = user, name = msg),
        "au" => t!(
            "room_render.sys.au",
            locale = loc,
            user = user,
            target = msg
        ),
        "ui" => t!(
            "room_render.sys.ui",
            locale = loc,
            user = user,
            target = msg
        ),
        "uir" => t!("room_render.sys.uir", locale = loc, user = user),
        "ru" => t!(
            "room_render.sys.ru",
            locale = loc,
            user = user,
            target = msg
        ),
        "added-user-to-team" => t!(
            "room_render.sys.added_user_to_team",
            locale = loc,
            user = user,
            target = msg
        ),
        "removed-user-from-team" => {
            t!(
                "room_render.sys.removed_user_from_team",
                locale = loc,
                user = user,
                target = msg
            )
        }
        "user-converted-to-team" => t!(
            "room_render.sys.converted_to_team",
            locale = loc,
            user = user,
            name = msg
        ),
        "user-converted-to-channel" => {
            t!(
                "room_render.sys.converted_to_channel",
                locale = loc,
                user = user,
                name = msg
            )
        }
        "user-removed-room-from-team" => {
            t!(
                "room_render.sys.removed_room_from_team",
                locale = loc,
                user = user,
                name = msg
            )
        }
        "user-deleted-room-from-team" => {
            t!(
                "room_render.sys.deleted_room_from_team",
                locale = loc,
                user = user,
                name = msg
            )
        }
        "user-added-room-to-team" => t!(
            "room_render.sys.added_room_to_team",
            locale = loc,
            user = user,
            name = msg
        ),
        "rm" => t!("room_render.sys.rm", locale = loc),
        "user-muted" => t!(
            "room_render.sys.user_muted",
            locale = loc,
            user = user,
            target = msg
        ),
        "user-unmuted" => t!(
            "room_render.sys.user_unmuted",
            locale = loc,
            user = user,
            target = msg
        ),
        "mute_unmute" => t!(
            "room_render.sys.mute_unmute",
            locale = loc,
            user = user,
            target = msg
        ),
        "user-banned" => t!(
            "room_render.sys.user_banned",
            locale = loc,
            user = user,
            target = msg
        ),
        "user-unbanned" => t!(
            "room_render.sys.user_unbanned",
            locale = loc,
            user = user,
            target = msg
        ),
        "subscription-role-added" => {
            t!(
                "room_render.sys.role_added",
                locale = loc,
                user = user,
                target = msg,
                role = role
            )
        }
        "subscription-role-removed" => {
            t!(
                "room_render.sys.role_removed",
                locale = loc,
                user = user,
                target = msg,
                role = role
            )
        }
        "new-owner" => t!(
            "room_render.sys.role_added",
            locale = loc,
            user = user,
            target = msg,
            role = t!("room_render.role_owner", locale = loc)
        ),
        "owner-removed" => t!(
            "room_render.sys.role_removed",
            locale = loc,
            user = user,
            target = msg,
            role = t!("room_render.role_owner", locale = loc)
        ),
        "new-moderator" => t!(
            "room_render.sys.role_added",
            locale = loc,
            user = user,
            target = msg,
            role = t!("room_render.role_moderator", locale = loc)
        ),
        "moderator-removed" => t!(
            "room_render.sys.role_removed",
            locale = loc,
            user = user,
            target = msg,
            role = t!("room_render.role_moderator", locale = loc)
        ),
        "new-leader" => t!(
            "room_render.sys.role_added",
            locale = loc,
            user = user,
            target = msg,
            role = t!("room_render.role_leader", locale = loc)
        ),
        "leader-removed" => t!(
            "room_render.sys.role_removed",
            locale = loc,
            user = user,
            target = msg,
            role = t!("room_render.role_leader", locale = loc)
        ),
        "room-archived" => t!("room_render.sys.room_archived", locale = loc, user = user),
        "room-unarchived" => t!("room_render.sys.room_unarchived", locale = loc, user = user),
        "room-removed-read-only" => t!(
            "room_render.sys.room_removed_read_only",
            locale = loc,
            user = user
        ),
        "room-set-read-only" => t!(
            "room_render.sys.room_set_read_only",
            locale = loc,
            user = user
        ),
        "room-allowed-reacting" => t!(
            "room_render.sys.room_allowed_reacting",
            locale = loc,
            user = user
        ),
        "room-disallowed-reacting" => t!(
            "room_render.sys.room_disallowed_reacting",
            locale = loc,
            user = user
        ),
        "room_changed_privacy" => {
            let kind = match m.msg.as_str() {
                "public" | "c" => t!("room_render.sys.privacy_public", locale = loc),
                "private" | "p" => t!("room_render.sys.privacy_private", locale = loc),
                other => other.to_owned().into(),
            };
            t!(
                "room_render.sys.room_changed_privacy",
                locale = loc,
                user = user,
                kind = kind
            )
        }
        "room_changed_topic" => t!(
            "room_render.sys.room_changed_topic",
            locale = loc,
            user = user,
            value = or_none(&msg)
        ),
        "room_changed_avatar" => t!(
            "room_render.sys.room_changed_avatar",
            locale = loc,
            user = user
        ),
        "room_changed_announcement" => {
            t!(
                "room_render.sys.room_changed_announcement",
                locale = loc,
                user = user,
                value = or_none(&msg)
            )
        }
        "room_changed_description" => {
            t!(
                "room_render.sys.room_changed_description",
                locale = loc,
                user = user,
                value = or_none(&msg)
            )
        }
        "room_e2e_enabled" => t!("room_render.sys.e2e_enabled", user = user),
        "room_e2e_disabled" => t!("room_render.sys.e2e_disabled", user = user),
        "message_pinned" | "message_pinned_e2e" => {
            t!("room_render.sys.message_pinned", locale = loc, user = user)
        }
        "discussion-created" => t!(
            "room_render.sys.discussion_created",
            locale = loc,
            user = user,
            name = msg
        ),
        "videoconf" => t!("room_render.sys.videoconf", locale = loc, user = user),
        "command" => t!(
            "room_render.sys.command",
            locale = loc,
            user = user,
            command = msg
        ),
        "abac-removed-user-from-room" => {
            t!("room_render.sys.abac_removed", locale = loc, user = user)
        }
        "livechat-started" => t!("room_render.sys.livechat_started", locale = loc),
        "livechat-close" => {
            if msg.trim().is_empty() {
                t!("room_render.sys.livechat_close", locale = loc)
            } else {
                t!(
                    "room_render.sys.livechat_close_comment",
                    locale = loc,
                    comment = msg
                )
            }
        }
        "livechat_video_call" => t!("room_render.sys.livechat_video_call", locale = loc),
        "livechat_navigation_history" => t!("room_render.sys.livechat_navigation", locale = loc),
        "livechat_transfer_history" | "livechat_transfer_history_fallback" => {
            t!("room_render.sys.livechat_transfer", locale = loc)
        }
        "livechat_transcript_history" => t!("room_render.sys.livechat_transcript", locale = loc),
        "omnichannel_placed_chat_on_hold" => t!("room_render.sys.on_hold", locale = loc),
        "omnichannel_on_hold_chat_resumed" => t!("room_render.sys.on_hold_resumed", locale = loc),
        "omnichannel_priority_change_history" => {
            t!("room_render.sys.priority_changed", locale = loc)
        }
        "omnichannel_sla_change_history" => t!("room_render.sys.sla_changed", locale = loc),
        _ => t!(
            "room_render.sys.unknown",
            locale = loc,
            user = user,
            kind = sanitize_bidi(kind)
        ),
    };
    s.into_owned()
}

/// Icon shown next to a system message.
pub fn system_icon(kind: &str) -> Icon {
    match kind {
        "uj" | "ujt" | "ut" | "au" | "ui" | "added-user-to-team" => Icon::UserPlus,
        "ul" | "ult" | "ru" | "uir" | "removed-user-from-team" | "abac-removed-user-from-room" => {
            Icon::UserMinus
        }
        "wm" => Icon::Hand,
        "r"
        | "room_changed_topic"
        | "room_changed_announcement"
        | "room_changed_description"
        | "room_changed_avatar" => Icon::Pencil,
        "room_changed_privacy" => Icon::Shield,
        "rm" => Icon::Trash2,
        "user-muted" | "mute_unmute" => Icon::MicOff,
        "user-unmuted" => Icon::Mic,
        "user-banned" => Icon::Ban,
        "user-unbanned" => Icon::ShieldCheck,
        "subscription-role-added" | "new-owner" | "new-moderator" | "new-leader" => {
            Icon::ShieldCheck
        }
        "subscription-role-removed" | "owner-removed" | "moderator-removed" | "leader-removed" => {
            Icon::ShieldOff
        }
        "room-archived" => Icon::Archive,
        "room-unarchived" => Icon::ArchiveRestore,
        "room-set-read-only" => Icon::Lock,
        "room-removed-read-only" => Icon::LockOpen,
        "room-allowed-reacting" | "room-disallowed-reacting" => Icon::Smile,
        "room_e2e_enabled" => Icon::Lock,
        "room_e2e_disabled" => Icon::LockOpen,
        "message_pinned" | "message_pinned_e2e" => Icon::Pin,
        "discussion-created" => Icon::MessagesSquare,
        "videoconf" | "livechat_video_call" => Icon::Video,
        "command" => Icon::SquareSlash,
        "user-converted-to-team" | "user-added-room-to-team" => Icon::Users,
        "user-converted-to-channel"
        | "user-removed-room-from-team"
        | "user-deleted-room-from-team" => Icon::Hash,
        k if k.starts_with("livechat") || k.starts_with("omnichannel") => Icon::Headset,
        _ => Icon::Info,
    }
}

/// Whether `kind` is hidden by `Hide_System_Messages` or the room's `sysMes`.
pub fn is_hidden(kind: &str, global: &[String], room: Option<&serde_json::Value>) -> bool {
    if global.iter().any(|h| h == kind) {
        return true;
    }
    match room {
        Some(serde_json::Value::Array(a)) => a.iter().any(|v| v.as_str() == Some(kind)),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rocketchat::models::UserRef;

    fn sys(t: &str, msg: &str) -> Message {
        Message {
            id: "m".into(),
            rid: "r".into(),
            t: Some(t.into()),
            msg: msg.into(),
            u: UserRef {
                id: "u1".into(),
                username: Some("alice".into()),
                name: Some("Alice Liddell".into()),
            },
            ..Message::default()
        }
    }

    fn en(m: &Message, rn: bool) -> String {
        system_text_in(m, rn, "en")
    }

    #[test]
    fn texts_en() {
        assert_eq!(en(&sys("uj", ""), false), "alice joined the channel");
        assert_eq!(en(&sys("uj", ""), true), "Alice Liddell joined the channel");
        assert_eq!(en(&sys("au", "bob"), false), "alice added bob");
        assert_eq!(
            en(&sys("r", "general"), false),
            "alice changed the room name to general"
        );
        assert_eq!(
            en(&sys("room_changed_topic", ""), false),
            "alice changed the room topic to (none)"
        );
        let mut role = sys("subscription-role-added", "bob");
        role.role = Some("moderator".into());
        assert_eq!(en(&role, false), "alice set bob as moderator");
        assert_eq!(en(&sys("rm", ""), false), "Message removed");
        assert_eq!(
            en(&sys("brand-new-type", ""), false),
            "alice: system message (brand-new-type)"
        );
        assert_eq!(
            en(&sys("room_changed_privacy", "private"), false),
            "alice changed the room type to private"
        );
    }

    #[test]
    fn texts_ru_and_every_type_translated() {
        assert_eq!(
            system_text_in(&sys("uj", ""), false, "ru"),
            "alice присоединился к каналу"
        );
        const TYPES: &[&str] = &[
            "e2e",
            "uj",
            "ui",
            "uir",
            "ul",
            "ru",
            "au",
            "mute_unmute",
            "r",
            "ut",
            "wm",
            "rm",
            "subscription-role-added",
            "subscription-role-removed",
            "room-archived",
            "room-unarchived",
            "room_changed_privacy",
            "room_changed_description",
            "room_changed_announcement",
            "room_changed_avatar",
            "room_changed_topic",
            "room_e2e_enabled",
            "room_e2e_disabled",
            "user-muted",
            "user-unmuted",
            "user-banned",
            "user-unbanned",
            "room-removed-read-only",
            "room-set-read-only",
            "room-allowed-reacting",
            "room-disallowed-reacting",
            "command",
            "videoconf",
            "message_pinned",
            "message_pinned_e2e",
            "new-moderator",
            "moderator-removed",
            "new-owner",
            "owner-removed",
            "new-leader",
            "leader-removed",
            "discussion-created",
            "abac-removed-user-from-room",
            "removed-user-from-team",
            "added-user-to-team",
            "ult",
            "user-converted-to-team",
            "user-converted-to-channel",
            "user-removed-room-from-team",
            "user-deleted-room-from-team",
            "user-added-room-to-team",
            "ujt",
            "livechat_navigation_history",
            "livechat_transfer_history",
            "livechat_transcript_history",
            "livechat_video_call",
            "livechat_transfer_history_fallback",
            "livechat-close",
            "livechat-started",
            "omnichannel_priority_change_history",
            "omnichannel_sla_change_history",
            "omnichannel_placed_chat_on_hold",
            "omnichannel_on_hold_chat_resumed",
        ];
        for locale in ["en", "ru"] {
            for kind in TYPES {
                let s = system_text_in(&sys(kind, "x"), false, locale);
                assert!(!s.contains("room_render."), "{locale} {kind}: {s}");
                assert!(!s.contains("%{"), "{locale} {kind}: {s}");
                if *kind != "e2e" {
                    assert!(
                        !s.contains("system message ("),
                        "{locale} {kind} unmapped: {s}"
                    );
                }
            }
        }
    }

    #[test]
    fn hidden_types() {
        let global = vec!["uj".to_owned()];
        assert!(is_hidden("uj", &global, None));
        assert!(!is_hidden("ul", &global, None));
        let room = serde_json::json!(["ul"]);
        assert!(is_hidden("ul", &global, Some(&room)));
        assert!(!is_hidden("ul", &global, Some(&serde_json::json!(true))));
    }
}
