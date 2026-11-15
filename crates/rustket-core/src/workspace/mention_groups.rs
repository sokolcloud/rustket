//! The account's mention groups (`crate::mention_groups`): the blob the
//! service rooms hold, this device's pending edits and the effective groups.

use crate::mention_groups::{MentionGroupBlob, MentionGroups};
use crate::service_sync::SyncBlob;

use super::{Dirty, Workspace};

impl Workspace {
    /// The effective mention groups: the stored blob with this device's
    /// pending edits.
    pub fn mention_groups(&self) -> &MentionGroups {
        &self.mention_groups
    }

    /// The mention groups blob of the user's service rooms, as the
    /// subscriptions last showed it (live and from the offline cache).
    pub fn mention_group_blob(&self) -> &MentionGroupBlob {
        &self.mention_group_blob
    }

    /// The pending edits set with [`Self::set_mention_group_overlay`].
    pub fn mention_group_overlay(&self) -> &MentionGroupBlob {
        &self.mention_group_overlay
    }

    /// This device's mention group edits not known to be stored yet (shown
    /// right away).
    pub fn set_mention_group_overlay(&mut self, overlay: MentionGroupBlob) {
        if self.mention_group_overlay == overlay {
            return;
        }
        self.mention_group_overlay = overlay;
        self.mark(Dirty::MENTION_GROUPS);
        self.update_mention_groups();
    }

    pub(super) fn set_mention_group_blob(&mut self, blob: MentionGroupBlob) {
        if self.mention_group_blob == blob {
            return;
        }
        self.mention_group_blob = blob;
        self.mark(Dirty::MENTION_GROUPS);
        self.update_mention_groups();
    }

    fn update_mention_groups(&mut self) {
        let groups = self
            .mention_group_blob
            .merged_with(&self.mention_group_overlay)
            .view();
        if groups != self.mention_groups {
            self.mention_groups = groups;
            self.mark(Dirty::MENTION_GROUPS);
        }
    }
}
