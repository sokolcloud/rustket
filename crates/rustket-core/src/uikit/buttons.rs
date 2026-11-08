//! Which app action buttons to show where (docs/uikit.md §6.2, web
//! `useApplyButtonFilters`, RN `app/lib/apps/filters.ts`).

use rocketchat::models::{ActionButton, ButtonContext, RoomType};

use crate::permissions::has_permission;
use crate::workspace::{RoomEntry, Workspace};

/// `category` filter of a menu.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ButtonCategory {
    /// Menus that show buttons without `category` or with `default`.
    #[default]
    Default,
    /// The separate "AI actions" menu.
    Ai,
}

/// `messageActionContext` values known to apps-engine.
pub const MESSAGE_CONTEXTS: &[&str] = &["message", "message-mobile", "threads", "starred"];

/// Where buttons are requested.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ButtonQuery<'a> {
    pub context: ButtonContext,
    /// The room of the menu (`None` for the user menu).
    pub rid: Option<&'a str>,
    pub category: ButtonCategory,
    /// Message lists only: `message` (main timeline), `threads`, `starred`, …
    pub message_context: Option<&'a str>,
}

impl<'a> ButtonQuery<'a> {
    pub fn room(rid: &'a str) -> Self {
        Self {
            context: ButtonContext::RoomAction,
            rid: Some(rid),
            category: ButtonCategory::Default,
            message_context: None,
        }
    }

    pub fn message(rid: &'a str, list: &'a str) -> Self {
        Self {
            context: ButtonContext::MessageAction,
            rid: Some(rid),
            category: ButtonCategory::Default,
            message_context: Some(list),
        }
    }

    pub fn composer(rid: &'a str) -> Self {
        Self {
            context: ButtonContext::MessageBoxAction,
            rid: Some(rid),
            category: ButtonCategory::Default,
            message_context: None,
        }
    }

    pub fn user_menu() -> Self {
        Self {
            context: ButtonContext::UserDropdownAction,
            rid: None,
            category: ButtonCategory::Default,
            message_context: None,
        }
    }

    pub fn ai(mut self) -> Self {
        self.category = ButtonCategory::Ai;
        self
    }
}

fn room_type(entry: &RoomEntry) -> RoomType {
    entry.room_type()
}

/// One `RoomTypeFilter` against a room.
pub fn room_type_matches(filter: &str, entry: &RoomEntry) -> bool {
    let t = room_type(entry);
    let public = t == RoomType::Channel;
    let private = t == RoomType::Private;
    match filter {
        "public_channel" => public,
        "private_channel" => private,
        "public_team" => entry.is_team() && public,
        "private_team" => entry.is_team() && private,
        "public_discussion" => entry.is_discussion() && public,
        "private_discussion" => entry.is_discussion() && private,
        "direct" => t == RoomType::Direct,
        "direct_multiple" => {
            t == RoomType::Direct
                && entry
                    .room
                    .as_ref()
                    .and_then(|r| r.uids.as_ref())
                    .is_some_and(|u| u.len() > 2)
        }
        "livechat" => t == RoomType::Livechat,
        _ => false,
    }
}

/// Role ids of the user, global plus those of the subscription to `rid`.
fn has_role(ws: &Workspace, role: &str, rid: Option<&str>) -> bool {
    // Apps know custom roles by name, the workspace by id: accept both.
    let id = ws
        .roles()
        .values()
        .find(|r| r.name == role)
        .map_or(role, |r| r.id.as_str());
    let global = ws.me().and_then(|m| m.roles.as_deref()).unwrap_or_default();
    let room = rid
        .and_then(|r| ws.subscription(r))
        .and_then(|s| s.roles.as_deref())
        .unwrap_or_default();
    global
        .iter()
        .chain(room.iter())
        .any(|r| r == id || r == role)
}

/// The permission and role filters of `when` (`useApplyButtonAuthFilter`).
/// Permissions are global checks; roles are scoped to `rid` when given.
pub fn auth_allowed(ws: &Workspace, b: &ActionButton, rid: Option<&str>) -> bool {
    let Some(w) = &b.when else {
        return true;
    };
    let logged_in = ws.user_id().is_some();
    w.has_all_permissions
        .as_ref()
        .is_none_or(|p| p.iter().all(|p| has_permission(ws, p, None)))
        && w.has_one_permission
            .as_ref()
            .is_none_or(|p| p.iter().any(|p| has_permission(ws, p, None)))
        && w.has_all_roles
            .as_ref()
            .is_none_or(|r| logged_in && r.iter().all(|r| has_role(ws, r, rid)))
        && w.has_one_role
            .as_ref()
            .is_none_or(|r| logged_in && r.iter().any(|r| has_role(ws, r, rid)))
}

fn category_matches(b: &ActionButton, category: ButtonCategory) -> bool {
    match category {
        ButtonCategory::Default => matches!(b.category.as_deref(), None | Some("default")),
        ButtonCategory::Ai => b.category.as_deref() == Some("ai"),
    }
}

/// Whether `b` shows for `q`.
pub fn button_visible(ws: &Workspace, b: &ActionButton, q: &ButtonQuery<'_>) -> bool {
    if b.context != q.context {
        return false;
    }
    match q.context {
        // The user menu applies only the auth filter, without a room.
        ButtonContext::UserDropdownAction => auth_allowed(ws, b, None),
        ButtonContext::RoomAction
        | ButtonContext::MessageAction
        | ButtonContext::MessageBoxAction => {
            // `ai` composer buttons are not shown anywhere.
            if q.context == ButtonContext::MessageBoxAction && q.category == ButtonCategory::Ai {
                return false;
            }
            if q.context == ButtonContext::MessageAction {
                let Some(list) = q.message_context.filter(|l| MESSAGE_CONTEXTS.contains(l)) else {
                    return false;
                };
                let listed = b
                    .when
                    .as_ref()
                    .and_then(|w| w.message_action_context.as_ref());
                if listed.is_some_and(|l| !l.iter().any(|c| c == list)) {
                    return false;
                }
            }
            let room_ok = match b.when.as_ref().and_then(|w| w.room_types.as_ref()) {
                None => true,
                Some(types) => q
                    .rid
                    .and_then(|r| ws.room(r))
                    .is_some_and(|e| types.iter().any(|t| room_type_matches(t, e))),
            };
            room_ok && auth_allowed(ws, b, q.rid) && category_matches(b, q.category)
        }
        ButtonContext::RoomSideBarAction | ButtonContext::Unknown => false,
    }
}

/// The buttons of `buttons` that show for `q`, in server order.
pub fn visible_buttons<'a>(
    ws: &Workspace,
    buttons: &'a [ActionButton],
    q: &ButtonQuery<'_>,
) -> Vec<&'a ActionButton> {
    buttons
        .iter()
        .filter(|b| button_visible(ws, b, q))
        .collect()
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::test_util::{me, room, sub};
    use crate::workspace::Change;
    use rocketchat::models::{ActionButtonWhen, Permission, Role};

    fn ws() -> Workspace {
        let mut ws = Workspace::new();
        ws.apply_me(me("me", "me"));
        ws.apply_subscription(Change::Inserted, sub("c1", "chan", RoomType::Channel));
        ws.apply_room(Change::Inserted, room("c1", "chan", RoomType::Channel));
        let mut p = sub("p1", "grp", RoomType::Private);
        p.roles = Some(vec!["owner".into()]);
        ws.apply_subscription(Change::Inserted, p);
        let mut team = room("p1", "grp", RoomType::Private);
        team.team_main = Some(true);
        ws.apply_room(Change::Inserted, team);
        ws.apply_subscription(Change::Inserted, sub("d1", "dm", RoomType::Direct));
        let mut dm = room("d1", "dm", RoomType::Direct);
        dm.uids = Some(vec!["a".into(), "b".into(), "c".into()]);
        ws.apply_room(Change::Inserted, dm);
        ws.apply_permission(
            Change::Inserted,
            Permission {
                id: "create-c".into(),
                roles: vec!["user".into()],
                ..Default::default()
            },
        );
        ws.apply_role(
            Change::Inserted,
            Role {
                id: "rnd123".into(),
                name: "Auditor".into(),
                ..Default::default()
            },
        );
        ws
    }

    fn button(ctx: ButtonContext, when: Option<ActionButtonWhen>) -> ActionButton {
        ActionButton {
            app_id: "app".into(),
            action_id: "act".into(),
            context: ctx,
            label_i18n: "l".into(),
            when,
            ..Default::default()
        }
    }

    fn types(t: &[&str]) -> Option<ActionButtonWhen> {
        Some(ActionButtonWhen {
            room_types: Some(t.iter().map(|s| (*s).to_owned()).collect()),
            ..Default::default()
        })
    }

    #[test]
    fn room_type_filters() {
        let ws = ws();
        let b = button(ButtonContext::RoomAction, types(&["private_team"]));
        assert!(button_visible(&ws, &b, &ButtonQuery::room("p1")));
        assert!(!button_visible(&ws, &b, &ButtonQuery::room("c1")));
        let dm = button(ButtonContext::RoomAction, types(&["direct_multiple"]));
        assert!(button_visible(&ws, &dm, &ButtonQuery::room("d1")));
        let any = button(ButtonContext::RoomAction, None);
        assert!(button_visible(&ws, &any, &ButtonQuery::room("c1")));
        assert!(!button_visible(&ws, &b, &ButtonQuery::room("unknown")));
        // Wrong context.
        assert!(!button_visible(&ws, &any, &ButtonQuery::composer("c1")));
    }

    #[test]
    fn permission_and_role_filters() {
        let ws = ws();
        let perm = button(
            ButtonContext::RoomAction,
            Some(ActionButtonWhen {
                has_one_permission: Some(vec!["create-c".into(), "nope".into()]),
                ..Default::default()
            }),
        );
        assert!(button_visible(&ws, &perm, &ButtonQuery::room("c1")));
        let all = button(
            ButtonContext::RoomAction,
            Some(ActionButtonWhen {
                has_all_permissions: Some(vec!["create-c".into(), "nope".into()]),
                ..Default::default()
            }),
        );
        assert!(!button_visible(&ws, &all, &ButtonQuery::room("c1")));
        // Room-scoped role: only where the subscription has it.
        let owner = button(
            ButtonContext::RoomAction,
            Some(ActionButtonWhen {
                has_one_role: Some(vec!["owner".into()]),
                ..Default::default()
            }),
        );
        assert!(button_visible(&ws, &owner, &ButtonQuery::room("p1")));
        assert!(!button_visible(&ws, &owner, &ButtonQuery::room("c1")));
        // The user menu has no room: room-scoped roles never match there.
        let owner_menu = ActionButton {
            context: ButtonContext::UserDropdownAction,
            ..owner
        };
        assert!(!button_visible(&ws, &owner_menu, &ButtonQuery::user_menu()));
        // Custom roles by name resolve to their id.
        let mut ws2 = ws.clone();
        let mut m = me("me", "me");
        m.roles = Some(vec!["user".into(), "rnd123".into()]);
        ws2.apply_me(m);
        let auditor = button(
            ButtonContext::UserDropdownAction,
            Some(ActionButtonWhen {
                has_all_roles: Some(vec!["Auditor".into(), "user".into()]),
                ..Default::default()
            }),
        );
        assert!(button_visible(&ws2, &auditor, &ButtonQuery::user_menu()));
        assert!(!button_visible(&ws, &auditor, &ButtonQuery::user_menu()));
    }

    #[test]
    fn categories_and_message_contexts() {
        let ws = ws();
        let mut ai = button(ButtonContext::RoomAction, None);
        ai.category = Some("ai".into());
        assert!(!button_visible(&ws, &ai, &ButtonQuery::room("c1")));
        assert!(button_visible(&ws, &ai, &ButtonQuery::room("c1").ai()));
        let mut ai_box = button(ButtonContext::MessageBoxAction, None);
        ai_box.category = Some("ai".into());
        assert!(!button_visible(&ws, &ai_box, &ButtonQuery::composer("c1")));
        assert!(!button_visible(
            &ws,
            &ai_box,
            &ButtonQuery::composer("c1").ai()
        ));
        // The user menu lists `ai` buttons too.
        let mut ai_user = button(ButtonContext::UserDropdownAction, None);
        ai_user.category = Some("ai".into());
        assert!(button_visible(&ws, &ai_user, &ButtonQuery::user_menu()));

        let msg = button(
            ButtonContext::MessageAction,
            Some(ActionButtonWhen {
                message_action_context: Some(vec!["threads".into()]),
                ..Default::default()
            }),
        );
        assert!(button_visible(
            &ws,
            &msg,
            &ButtonQuery::message("c1", "threads")
        ));
        assert!(!button_visible(
            &ws,
            &msg,
            &ButtonQuery::message("c1", "message")
        ));
        let every = button(ButtonContext::MessageAction, None);
        assert!(button_visible(
            &ws,
            &every,
            &ButtonQuery::message("c1", "starred")
        ));
        assert!(!button_visible(
            &ws,
            &every,
            &ButtonQuery::message("c1", "pinned")
        ));
        let list = [msg, every];
        assert_eq!(
            visible_buttons(&ws, &list, &ButtonQuery::message("c1", "message")).len(),
            1
        );
    }
}
