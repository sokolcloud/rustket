//! Administration screen state (docs/admin.md): section gating, the settings
//! editor, server i18n, and helpers. Plain data and pure functions; the REST calls
//! live in `rocketchat::rest::admin`.

pub mod invites;
pub mod mongo_filter;
pub mod server_i18n;
pub mod settings_editor;
pub mod timespan;

use rocketchat::models::admin::LicenseInfo;

use crate::permissions::{has_any_permission, has_permission};
use crate::workspace::Workspace;

/// A page of the admin screen, gated like the web's `admin/sidebarItems.ts`
/// (docs/admin.md §1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AdminSection {
    Workspace,
    Subscription,
    Engagement,
    Moderation,
    Rooms,
    Users,
    AiCenter,
    Invites,
    UserStatus,
    Permissions,
    DeviceManagement,
    EmailInboxes,
    Mailer,
    ThirdPartyLogin,
    Integrations,
    Import,
    Reports,
    Sounds,
    Emoji,
    Settings,
    Apps,
}

const SETTINGS_PERMISSIONS: &[&str] = &[
    "view-privileged-setting",
    "edit-privileged-setting",
    "manage-selected-settings",
];

impl AdminSection {
    /// In sidebar order.
    pub const ALL: [Self; 21] = [
        Self::Workspace,
        Self::Subscription,
        Self::Engagement,
        Self::Moderation,
        Self::Rooms,
        Self::Users,
        Self::AiCenter,
        Self::Invites,
        Self::UserStatus,
        Self::Permissions,
        Self::DeviceManagement,
        Self::EmailInboxes,
        Self::Mailer,
        Self::ThirdPartyLogin,
        Self::Integrations,
        Self::Import,
        Self::Reports,
        Self::Sounds,
        Self::Emoji,
        Self::Settings,
        Self::Apps,
    ];

    /// Any of these permissions shows the section.
    pub fn permissions(self) -> &'static [&'static str] {
        match self {
            Self::Workspace => &["view-statistics"],
            Self::Subscription => &["manage-cloud"],
            Self::Engagement => &["view-engagement-dashboard"],
            Self::Moderation => &["view-moderation-console"],
            Self::Rooms => &["view-room-administration"],
            Self::Users => &["view-user-administration"],
            Self::AiCenter | Self::Settings => SETTINGS_PERMISSIONS,
            Self::Invites => &["create-invite-links"],
            Self::UserStatus => &["manage-user-status"],
            Self::Permissions => &["access-permissions", "access-setting-permissions"],
            Self::DeviceManagement => &["view-device-management"],
            Self::EmailInboxes => &["manage-email-inbox"],
            Self::Mailer => &["access-mailer"],
            Self::ThirdPartyLogin => &["manage-oauth-apps"],
            Self::Integrations => &[
                "manage-outgoing-integrations",
                "manage-own-outgoing-integrations",
                "manage-incoming-integrations",
                "manage-own-incoming-integrations",
            ],
            Self::Import => &["run-import"],
            Self::Reports => &["view-logs"],
            Self::Sounds => &["manage-sounds"],
            Self::Emoji => &["manage-emoji"],
            Self::Apps => &["access-marketplace", "manage-apps"],
        }
    }

    /// EE module required for the section, if any.
    pub fn license_module(self) -> Option<&'static str> {
        match self {
            Self::Engagement => Some("engagement-dashboard"),
            Self::DeviceManagement => Some("device-management"),
            _ => None,
        }
    }

    /// Implemented natively; otherwise offer "Open in browser" ([`Self::web_path`]).
    pub fn is_native(self) -> bool {
        !matches!(self, Self::Subscription | Self::AiCenter | Self::Import)
    }

    /// Web route below the server URL for the browser fallback.
    pub fn web_path(self) -> &'static str {
        match self {
            Self::Workspace => "admin/info",
            Self::Subscription => "admin/subscription",
            Self::Engagement => "admin/engagement/users",
            Self::Moderation => "admin/moderation",
            Self::Rooms => "admin/rooms",
            Self::Users => "admin/users",
            Self::AiCenter => "admin/ai-center",
            Self::Invites => "admin/invites",
            Self::UserStatus => "admin/user-status",
            Self::Permissions => "admin/permissions",
            Self::DeviceManagement => "admin/device-management",
            Self::EmailInboxes => "admin/email-inboxes",
            Self::Mailer => "admin/mailer",
            Self::ThirdPartyLogin => "admin/third-party-login",
            Self::Integrations => "admin/integrations",
            Self::Import => "admin/import",
            Self::Reports => "admin/analytic-reports",
            Self::Sounds => "admin/sounds",
            Self::Emoji => "admin/emoji",
            Self::Settings => "admin/settings",
            Self::Apps => "marketplace/installed",
        }
    }

    /// Stable key, e.g. for i18n lookups of section titles (`admin.section.<key>`).
    pub fn key(self) -> &'static str {
        match self {
            Self::Workspace => "workspace",
            Self::Subscription => "subscription",
            Self::Engagement => "engagement",
            Self::Moderation => "moderation",
            Self::Rooms => "rooms",
            Self::Users => "users",
            Self::AiCenter => "ai_center",
            Self::Invites => "invites",
            Self::UserStatus => "user_status",
            Self::Permissions => "permissions",
            Self::DeviceManagement => "device_management",
            Self::EmailInboxes => "email_inboxes",
            Self::Mailer => "mailer",
            Self::ThirdPartyLogin => "third_party_login",
            Self::Integrations => "integrations",
            Self::Import => "import",
            Self::Reports => "reports",
            Self::Sounds => "sounds",
            Self::Emoji => "emoji",
            Self::Settings => "settings",
            Self::Apps => "apps",
        }
    }

    /// The user has one of the section's permissions.
    pub fn is_permitted(self, ws: &Workspace) -> bool {
        has_any_permission(ws, self.permissions(), None)
    }
}

/// Union of the section permissions without `access-marketplace` (every user has
/// it): any of them shows the administration entry.
pub const ADMIN_PERMISSIONS: &[&str] = &[
    "view-statistics",
    "manage-cloud",
    "view-engagement-dashboard",
    "view-moderation-console",
    "view-room-administration",
    "view-user-administration",
    "view-privileged-setting",
    "edit-privileged-setting",
    "manage-selected-settings",
    "create-invite-links",
    "manage-user-status",
    "access-permissions",
    "access-setting-permissions",
    "view-device-management",
    "manage-email-inbox",
    "access-mailer",
    "manage-oauth-apps",
    "manage-outgoing-integrations",
    "manage-own-outgoing-integrations",
    "manage-incoming-integrations",
    "manage-own-incoming-integrations",
    "run-import",
    "view-logs",
    "manage-sounds",
    "manage-emoji",
    "manage-apps",
];

/// Whether the administration entry is shown.
pub fn can_view_admin(ws: &Workspace) -> bool {
    has_any_permission(ws, ADMIN_PERMISSIONS, None)
}

/// A section the user may open; `premium_locked` sections need an EE module the
/// workspace lacks (show disabled with a "Premium" tag, or hide).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VisibleSection {
    pub section: AdminSection,
    pub premium_locked: bool,
}

/// Sections in sidebar order. `Apps` is listed only inside an admin screen that is
/// shown at all (its `access-marketplace` permission alone does not open it).
pub fn visible_sections(ws: &Workspace, license: Option<&LicenseInfo>) -> Vec<VisibleSection> {
    if !can_view_admin(ws) {
        return Vec::new();
    }
    AdminSection::ALL
        .into_iter()
        .filter(|s| s.is_permitted(ws))
        .map(|section| VisibleSection {
            section,
            premium_locked: section
                .license_module()
                .is_some_and(|m| !license.is_some_and(|l| l.has_module(m))),
        })
        .collect()
}

/// Settings editor permissions: any of view/edit privileged, manage selected.
pub fn can_manage_settings(ws: &Workspace) -> bool {
    has_any_permission(ws, SETTINGS_PERMISSIONS, None)
}

/// Create user is disabled while the seats cap is reached.
pub fn can_create_users(ws: &Workspace, license: Option<&LicenseInfo>) -> bool {
    has_permission(ws, "create-user", None)
        && !license.is_some_and(|l| l.is_prevented("activeUsers"))
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::test_util::{fixture, me};
    use crate::workspace::Change;
    use rocketchat::models::{Permission, UpdatedSince};

    fn ws_with(roles: &[&str], perms: &UpdatedSince<Permission>) -> Workspace {
        let mut ws = Workspace::new();
        let mut m = me("me", "me");
        m.roles = Some(roles.iter().map(|r| (*r).to_owned()).collect());
        ws.apply_me(m);
        for p in &perms.update {
            ws.apply_permission(Change::Inserted, p.clone());
        }
        ws
    }

    fn live_permissions() -> UpdatedSince<Permission> {
        serde_json::from_value(fixture("admin/permissions_list_all.json")).unwrap()
    }

    #[test]
    fn admin_sees_everything_but_premium_is_locked() {
        let perms = live_permissions();
        let ws = ws_with(&["admin"], &perms);
        assert!(can_view_admin(&ws));
        let license: LicenseInfo =
            serde_json::from_value(fixture("admin/licenses_info.json")["license"].clone()).unwrap();
        let v = visible_sections(&ws, Some(&license));
        let sections: Vec<AdminSection> = v.iter().map(|s| s.section).collect();
        for s in [
            AdminSection::Workspace,
            AdminSection::Users,
            AdminSection::Rooms,
            AdminSection::Settings,
            AdminSection::Permissions,
            AdminSection::Integrations,
            AdminSection::Moderation,
            AdminSection::Apps,
        ] {
            assert!(sections.contains(&s), "{s:?}");
        }
        // Engagement needs an EE module the CE license lacks.
        let engagement = v
            .iter()
            .find(|s| s.section == AdminSection::Engagement)
            .unwrap();
        assert!(engagement.premium_locked);
        assert!(
            v.iter()
                .all(|s| !s.premium_locked || s.section.license_module().is_some())
        );
        assert!(can_manage_settings(&ws));
        assert!(can_create_users(&ws, Some(&license)));
    }

    #[test]
    fn plain_user_sees_nothing() {
        let perms = live_permissions();
        let ws = ws_with(&["user"], &perms);
        // `access-marketplace` belongs to `user` but must not open the admin entry.
        assert!(has_permission(&ws, "access-marketplace", None));
        assert!(!can_view_admin(&ws));
        assert!(visible_sections(&ws, None).is_empty());
        assert!(!crate::permissions::can_view_admin(&ws));
    }

    #[test]
    fn moderator_sees_only_invites() {
        let mut ws = Workspace::new();
        let mut m = me("me", "me");
        m.roles = Some(vec!["user".into(), "bot".into()]);
        ws.apply_me(m);
        ws.apply_permission(
            Change::Inserted,
            Permission {
                id: "create-invite-links".into(),
                roles: vec!["bot".into()],
                updated_at: None,
            },
        );
        let v = visible_sections(&ws, None);
        assert_eq!(
            v,
            vec![VisibleSection {
                section: AdminSection::Invites,
                premium_locked: false
            }]
        );
    }

    #[test]
    fn premium_lock() {
        let mut ws = Workspace::new();
        let mut m = me("me", "me");
        m.roles = Some(vec!["admin".into()]);
        ws.apply_me(m);
        ws.apply_permission(
            Change::Inserted,
            Permission {
                id: "view-engagement-dashboard".into(),
                roles: vec!["admin".into()],
                updated_at: None,
            },
        );
        let v = visible_sections(&ws, None);
        assert_eq!(v.len(), 1);
        assert!(v[0].premium_locked);
        let license = LicenseInfo {
            active_modules: vec!["engagement-dashboard".into()],
            ..LicenseInfo::default()
        };
        assert!(!visible_sections(&ws, Some(&license))[0].premium_locked);
    }

    #[test]
    fn permission_table_is_consistent() {
        for s in AdminSection::ALL {
            for p in s.permissions() {
                assert!(
                    ADMIN_PERMISSIONS.contains(p) || *p == "access-marketplace",
                    "{s:?} {p}"
                );
            }
            assert!(!s.web_path().is_empty());
        }
        assert!(!ADMIN_PERMISSIONS.contains(&"access-marketplace"));
        assert!(!AdminSection::Import.is_native());
    }
}
