//! The Dock tile badge (macOS): the number of mentions (a DM counts as one);
//! nothing for `@all` alone or quiet unread (only mentions are loud).

use rustket_core::room_list::WorkspaceSignal;

/// The badge text for the unread signal over all accounts ("99+" above 99).
pub fn badge_label(signal: &WorkspaceSignal) -> Option<String> {
    (signal.mentions > 0)
        .then(|| rustket_ui::widget::badge_label(crate::format::count_u32(signal.mentions)))
}

/// Sets (or clears) the Dock badge. Main thread only (iced `update`).
#[cfg(target_os = "macos")]
pub fn apply(label: Option<&str>) {
    use objc2::MainThreadMarker;
    use objc2_app_kit::NSApplication;
    use objc2_foundation::NSString;
    let Some(mtm) = MainThreadMarker::new() else {
        tracing::debug!("dock badge: not on the main thread");
        return;
    };
    let label = label.map(NSString::from_str);
    NSApplication::sharedApplication(mtm)
        .dockTile()
        .setBadgeLabel(label.as_deref());
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustket_core::room_list::BadgeVariant;

    #[test]
    fn badge_counts_mentions_only() {
        let sig = |mentions, strongest, rooms| WorkspaceSignal {
            mentions,
            strongest,
            rooms,
        };
        assert_eq!(badge_label(&sig(0, None, 0)), None);
        assert_eq!(badge_label(&sig(0, Some(BadgeVariant::Unread), 3)), None);
        assert_eq!(badge_label(&sig(0, Some(BadgeVariant::Thread), 2)), None);
        assert_eq!(
            badge_label(&sig(0, Some(BadgeVariant::GroupMention), 1)),
            None
        );
        assert_eq!(
            badge_label(&sig(3, Some(BadgeVariant::Mention), 5)).as_deref(),
            Some("3")
        );
        assert_eq!(
            badge_label(&sig(250, Some(BadgeVariant::Mention), 5)).as_deref(),
            Some("99+")
        );
    }
}
