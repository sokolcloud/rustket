//! Syncs the account's mention groups (`rustket_core::mention_groups`) with
//! its hidden service room: one key of the generic service sync
//! ([`super::service_sync`]). Edits go into a pending overlay the composer
//! and the Settings use at once; the app persists it in the config
//! (`pending_service`) until the subscriptions show it stored.

use rustket_core::mention_groups::MentionGroupBlob;
use rustket_core::{Dirty, Workspace};

use super::service_sync::{BlobSync, ServiceKey};

/// The mention groups as a synced key.
#[derive(Debug, Default)]
pub struct MentionGroupsKey;

impl ServiceKey for MentionGroupsKey {
    type Blob = MentionGroupBlob;
    const DIRTY: Dirty = Dirty::MENTION_GROUPS;
    const LOG: &'static str = "mention groups";

    fn stored(ws: &Workspace) -> &MentionGroupBlob {
        ws.mention_group_blob()
    }

    fn overlay(ws: &Workspace) -> &MentionGroupBlob {
        ws.mention_group_overlay()
    }

    fn set_overlay(ws: &mut Workspace, overlay: MentionGroupBlob) {
        ws.set_mention_group_overlay(overlay);
    }
}

/// Per-server mention group sync (`ServerState::mention_groups`).
pub type MentionGroupSync = BlobSync<MentionGroupsKey>;

/// Messages of the mention group sync.
pub type Msg = super::service_sync::Msg<MentionGroupBlob>;

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use std::time::Instant;

    use rocketchat::models::{MENTION_GROUPS_KEY, ServiceFields, User};
    use rustket_core::mention_groups::{MentionGroupEdit, MentionGroupError};
    use rustket_core::service_sync::SyncBlob;
    use rustket_core::workspace::ConnectionState;

    use super::super::service_sync::{
        COALESCE, Next, Outcome, Pushed, Status, Syncing, key_checks,
    };
    use super::*;

    fn ws() -> Workspace {
        let mut ws = Workspace::new();
        ws.apply_me(User {
            id: "me".into(),
            username: Some("me".into()),
            ..Default::default()
        });
        ws.apply_connection(ConnectionState::Connected);
        ws
    }

    fn synced() -> MentionGroupSync {
        let mut s = MentionGroupSync::default();
        s.follow_account(Some("me"), |_| MentionGroupBlob::default());
        s
    }

    fn upsert(id: &str, name: &str) -> MentionGroupEdit {
        MentionGroupEdit::Upsert {
            id: id.into(),
            name: name.into(),
            members: vec!["alice".into(), "bob".into()],
        }
    }

    #[test]
    fn edits_show_at_once_and_go_out_with_the_server_push() {
        let mut ws = ws();
        let mut s = synced();
        let t0 = Instant::now();
        assert_eq!(
            s.edit(&mut ws, &upsert("g1", "backend"), "dev", t0),
            Ok(true)
        );
        assert_eq!(
            ws.mention_groups().get("backend").unwrap().members,
            ["alice", "bob"]
        );
        assert_eq!(s.status(), Status::Pending);
        assert_eq!(s.plan(&ws, t0), Next::Wait(t0 + COALESCE));
        assert_eq!(
            s.edit(&mut ws, &upsert("g2", "backend"), "dev", t0),
            Err(MentionGroupError::Duplicate)
        );
        Syncing::start(&mut s);
        assert!(s.in_flight);
        let written = s.overlay().clone();
        let mut f = ServiceFields::new("me");
        f.set(MENTION_GROUPS_KEY, written.to_value_at(0));
        let o = Outcome::for_tests(vec![(MENTION_GROUPS_KEY, s.epoch)], vec![f]);
        let (epoch, res) = s.finish(&o).unwrap();
        let p: Pushed<MentionGroupBlob> = res.unwrap();
        assert_eq!(p.written, written);
        assert!(s.update(Msg::Pushed { epoch, res: Ok(p) }, t0).is_empty());
        assert!(!s.in_flight);
        // Signed out: edits are not taken.
        let mut off = MentionGroupSync::default();
        assert_eq!(off.edit(&mut ws, &upsert("g3", "qa"), "dev", t0), Ok(false));
    }

    #[test]
    fn the_key_takes_part_in_seed_checks() {
        let keys: Vec<&str> = key_checks().iter().map(|k| k.key()).collect();
        assert!(keys.contains(&MENTION_GROUPS_KEY), "{keys:?}");
        assert!(keys.contains(&rocketchat::models::FLAGS_KEY), "{keys:?}");
    }

    /// The groups go out in the server's one combined push, next to the
    /// flags, and both learn their result from it.
    #[test]
    fn the_server_push_carries_flags_and_groups() {
        use rocketchat::models::FLAGS_KEY;
        use rustket_core::flags::{FlagBlob, FlagColor, FlagEdit};

        let entry = rustket_core::config::ServerEntry::new("http://localhost:3999", "t");
        let mut state = super::super::ServerState::from_entry(&entry).unwrap();
        state.workspace = ws();
        state
            .flags
            .follow_account(Some("me"), |_| FlagBlob::default());
        state
            .mention_groups
            .follow_account(Some("me"), |_| MentionGroupBlob::default());
        let t0 = Instant::now();
        let flag = FlagEdit::Room {
            rid: "r1".into(),
            color: Some(FlagColor::Red),
        };
        assert_eq!(
            state.flags.edit(&mut state.workspace, &flag, "dev", t0),
            Ok(true)
        );
        let group = upsert("g1", "backend");
        assert_eq!(
            state
                .mention_groups
                .edit(&mut state.workspace, &group, "dev", t0),
            Ok(true)
        );
        assert!(state.service_parts(t0).is_none(), "still coalescing");
        let parts = state.service_parts(t0 + COALESCE).unwrap();
        let keys: Vec<(&'static str, u64)> = parts.iter().map(|p| p.key()).collect();
        assert_eq!(
            keys.iter().map(|k| k.0).collect::<Vec<_>>(),
            [FLAGS_KEY, MENTION_GROUPS_KEY]
        );
        assert!(state.flags.in_flight && state.mention_groups.in_flight);
        // The push stored both: each key gets its part of the one outcome.
        let mut f = ServiceFields::new("me");
        f.set(FLAGS_KEY, state.flags.overlay().to_value_at(0));
        f.set(
            MENTION_GROUPS_KEY,
            state.mention_groups.overlay().to_value_at(0),
        );
        let o = Outcome::for_tests(keys, vec![f]);
        let pushed = state.service_pushed(&o, t0 + COALESCE);
        let (
            Some(super::super::flags::Msg::Pushed {
                epoch: fe,
                res: Ok(fp),
            }),
            Some(Msg::Pushed {
                epoch: ge,
                res: Ok(gp),
            }),
        ) = (pushed.flags, pushed.mention_groups)
        else {
            panic!("both keys stored");
        };
        assert_eq!(&fp.written, state.flags.overlay());
        assert_eq!(&gp.written, state.mention_groups.overlay());
        assert_eq!(gp.stored.groups, state.mention_groups.overlay().groups);
        // Both learn the room the push used.
        assert_eq!(state.flags.created.as_deref(), Some("svc"));
        assert_eq!(state.mention_groups.created.as_deref(), Some("svc"));
        let flags = super::super::flags::Msg::Pushed {
            epoch: fe,
            res: Ok(fp),
        };
        let groups = Msg::Pushed {
            epoch: ge,
            res: Ok(gp),
        };
        assert!(state.flags.update(flags, t0 + COALESCE).is_empty());
        assert!(
            state
                .mention_groups
                .update(groups, t0 + COALESCE)
                .is_empty()
        );
        assert!(!state.flags.in_flight && !state.mention_groups.in_flight);
        assert_eq!(
            state.mention_groups.confirmed.groups,
            state.mention_groups.overlay().groups
        );
    }
}
