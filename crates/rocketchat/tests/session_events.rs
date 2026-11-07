//! Decoding of realtime stream messages. Fixtures in `fixtures/ddp/` were captured
//! from the 8.9 dev server; `synthetic_*` ones follow `streams.ts` for events a
//! test cannot trigger without changing global server state.
#![allow(clippy::unwrap_used)]

use rocketchat::models::{RoomType, UserStatus};
use rocketchat::session::{ChangeKind, SessionEvent, decode_event, stream_params};
use rocketchat::session::{DataKind, DataMessage};
use serde_json::{Value, json};

fn load(name: &str) -> DataMessage {
    let path = format!(
        "{}/tests/fixtures/ddp/{name}.json",
        env!("CARGO_MANIFEST_DIR")
    );
    let v: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    DataMessage {
        kind: DataKind::Changed,
        collection: v["collection"].as_str().unwrap().to_owned(),
        id: v["id"].as_str().unwrap().to_owned(),
        fields: v["fields"].as_object().cloned(),
        cleared: Vec::new(),
        before: None,
    }
}

fn decode(name: &str) -> SessionEvent {
    decode_event(&load(name))
}

const RID: &str = "6aef60d5ff2338a3f2a2c250";

#[test]
fn stream_params_shape() {
    assert_eq!(
        stream_params("__my_messages__"),
        vec![
            json!("__my_messages__"),
            json!({"useCollection": false, "args": []})
        ]
    );
    assert_eq!(
        serde_json::to_string(&stream_params("x")).unwrap(),
        r#"["x",{"args":[],"useCollection":false}]"#
    );
}

#[test]
fn my_messages() {
    let SessionEvent::Message {
        message: m,
        room_participant: true,
    } = decode("my_messages_new")
    else {
        panic!()
    };
    assert_eq!(m.rid, RID);
    assert!(m.msg.starts_with("hello @cap-a"));
    assert!(m.md.is_some());
    assert!(m.edited_at.is_none());

    let SessionEvent::Message {
        message: m,
        room_participant: true,
    } = decode("my_messages_edited")
    else {
        panic!()
    };
    assert_eq!(m.msg, "edited text");
    assert!(m.edited_at.is_some());
    assert_eq!(
        m.edited_by.as_ref().unwrap().username.as_deref(),
        Some("cap-b-1794072787233")
    );

    let SessionEvent::Message {
        message: m,
        room_participant: true,
    } = decode("my_messages_system_ul")
    else {
        panic!()
    };
    assert_eq!(m.t.as_deref(), Some("ul"));
}

#[test]
fn subscriptions_changed() {
    let SessionEvent::SubscriptionChanged { kind, sub } = decode("subscriptions_changed_inserted")
    else {
        panic!()
    };
    assert_eq!(kind, ChangeKind::Inserted);
    assert_eq!(sub.rid, RID);
    assert_eq!(sub.t, RoomType::Channel);

    let SessionEvent::SubscriptionChanged { kind, sub } = decode("subscriptions_changed_updated")
    else {
        panic!()
    };
    assert_eq!(kind, ChangeKind::Updated);
    assert!(sub.alert);

    for name in [
        "subscriptions_changed_removed",
        "subscriptions_changed_removed_dm",
    ] {
        let SessionEvent::SubscriptionChanged { kind, sub } = decode(name) else {
            panic!("{name}")
        };
        assert_eq!(kind, ChangeKind::Removed);
        assert!(!sub.rid.is_empty());
    }

    let SessionEvent::SubscriptionChanged { kind, sub } =
        decode("synthetic_subscriptions_changed_removed_partial")
    else {
        panic!()
    };
    assert_eq!(kind, ChangeKind::Removed);
    assert_eq!(sub.id, "subid1");
    assert_eq!(sub.rid, RID);
    assert_eq!(sub.u.username.as_deref(), Some("cap-a"));
}

#[test]
fn rooms_changed() {
    let SessionEvent::RoomChanged { kind, room } = decode("rooms_changed_inserted") else {
        panic!()
    };
    assert_eq!(kind, ChangeKind::Inserted);
    assert_eq!(room.id, RID);
    let SessionEvent::RoomChanged { kind, room } = decode("rooms_changed_inserted_dm") else {
        panic!()
    };
    assert_eq!(kind, ChangeKind::Inserted);
    assert_eq!(room.t, RoomType::Direct);
    assert_eq!(room.uids.as_ref().unwrap().len(), 2);
    let SessionEvent::RoomChanged { kind, room } = decode("rooms_changed_updated") else {
        panic!()
    };
    assert_eq!(kind, ChangeKind::Updated);
    assert!(room.last_message.is_some());
    let SessionEvent::RoomChanged { kind, room } =
        decode("synthetic_rooms_changed_removed_partial")
    else {
        panic!()
    };
    assert_eq!(kind, ChangeKind::Removed);
    assert_eq!(room.id, RID);
}

#[test]
fn notification() {
    let SessionEvent::Notification(n) = decode("notification") else {
        panic!()
    };
    assert!(n.title.contains("cap-room"));
    assert!(!n.text.is_empty());
    assert_eq!(n.payload.rid, RID);
    assert_eq!(n.payload.room_type, Some(RoomType::Channel));
    assert_eq!(n.payload.name.as_deref(), Some("cap-room-1794072787233"));
    assert_eq!(
        n.payload.sender.as_ref().unwrap().username.as_deref(),
        Some("cap-b-1794072787233")
    );
    assert!(n.payload.message.msg.starts_with("hello"));
    assert!(!n.payload.id.is_empty());
}

#[test]
fn room_events() {
    assert_eq!(
        decode("user_activity_typing"),
        SessionEvent::UserActivity {
            rid: RID.into(),
            username: "cap-b-1794072787233".into(),
            activities: vec!["user-typing".into()],
            tmid: None,
        }
    );
    assert!(matches!(
        decode("user_activity_stop"),
        SessionEvent::UserActivity { activities, .. } if activities.is_empty()
    ));
    assert_eq!(
        decode("synthetic_user_activity_thread"),
        SessionEvent::UserActivity {
            rid: RID.into(),
            username: "bob".into(),
            activities: vec!["user-typing".into(), "user-uploading".into()],
            tmid: Some("t1".into()),
        }
    );
    assert_eq!(
        decode("synthetic_typing_legacy"),
        SessionEvent::UserActivity {
            rid: RID.into(),
            username: "bob".into(),
            activities: vec!["user-typing".into()],
            tmid: None,
        }
    );
    assert_eq!(
        decode("delete_message"),
        SessionEvent::MessageDeleted {
            rid: RID.into(),
            id: "iPET8t9XYwBx5qXfp".into()
        }
    );
    let SessionEvent::MessagesDeletedBulk(b) = decode("delete_message_bulk") else {
        panic!()
    };
    assert_eq!(b.rid, RID);
    assert!(b.users.is_empty());
    assert!(b.ids.is_none());
    assert!(!b.exclude_pinned);
    let inside = rocketchat::RcDate::from_millis(1_794_072_800_000).unwrap();
    let outside = rocketchat::RcDate::from_millis(946_684_800_000).unwrap();
    assert!(b.ts.contains(inside));
    assert!(!b.ts.contains(outside), "$gt is exclusive");

    let SessionEvent::MessagesRead { rid, until, tmid } = decode("synthetic_messages_read") else {
        panic!()
    };
    assert_eq!(rid, RID);
    assert_eq!(until.unwrap().timestamp_millis(), 1_794_072_800_000);
    assert_eq!(tmid.as_deref(), Some("t1"));
    assert_eq!(
        decode("synthetic_videoconf_room"),
        SessionEvent::RoomVideoConference {
            rid: RID.into(),
            call_id: "call1".into()
        }
    );
}

#[test]
fn user_status() {
    assert_eq!(
        decode("user_status_online"),
        SessionEvent::UserStatus {
            uid: "F3QsssYnr8Ardq4Js".into(),
            username: "cap-b-1794072787233".into(),
            status: UserStatus::Online,
            status_text: Some(String::new()),
        }
    );
    assert_eq!(
        decode("user_status_away"),
        SessionEvent::UserStatus {
            uid: "dNmNNcy3eQMw2BTCi".into(),
            username: "cap-a-1794072787233".into(),
            status: UserStatus::Away,
            status_text: None,
        }
    );
    assert!(matches!(
        decode("user_status_offline_text"),
        SessionEvent::UserStatus { status: UserStatus::Offline, status_text: Some(t), .. } if t == "capturing"
    ));
}

#[test]
fn notify_logged_events() {
    assert_eq!(
        decode("update_avatar"),
        SessionEvent::AvatarUpdated {
            username: Some("cap-b-1794072787233".into()),
            rid: None,
            etag: None
        }
    );
    assert_eq!(
        decode("synthetic_update_avatar_room"),
        SessionEvent::AvatarUpdated {
            username: None,
            rid: Some(RID.into()),
            etag: Some("abc123".into())
        }
    );
    let SessionEvent::RolesChanged(r) = decode("roles_change_added") else {
        panic!()
    };
    assert_eq!(r.kind, "added");
    assert_eq!(r.role, "bot");
    assert_eq!(r.user.as_ref().unwrap().id, "F3QsssYnr8Ardq4Js");
    assert!(r.scope.is_none());

    let SessionEvent::CustomUserStatusUpdated(s) = decode("update_custom_user_status") else {
        panic!()
    };
    assert_eq!(s.status_type, UserStatus::Busy);
    let SessionEvent::CustomUserStatusDeleted(s) = decode("delete_custom_user_status") else {
        panic!()
    };
    assert!(s.name.starts_with("capstatus"));

    assert_eq!(
        decode("users_deleted"),
        SessionEvent::UserDeleted {
            uid: "F3QsssYnr8Ardq4Js".into(),
            message_erasure_type: Some("Delete".into()),
            replace_by_user: None
        }
    );
    assert!(matches!(
        decode("synthetic_users_deleted_unlink"),
        SessionEvent::UserDeleted {
            replace_by_user: Some(_),
            ..
        }
    ));
    assert_eq!(
        decode("synthetic_users_name_changed"),
        SessionEvent::UserNameChanged {
            uid: "F3QsssYnr8Ardq4Js".into(),
            username: Some("bob".into()),
            name: Some("Bob Renamed".into())
        }
    );
    let SessionEvent::EmojiCustomUpdated(e) = decode("synthetic_update_emoji_custom") else {
        panic!()
    };
    assert_eq!(
        (e.name.as_str(), e.extension.as_str()),
        ("party_parrot", "gif")
    );
    assert!(matches!(
        decode("synthetic_delete_emoji_custom"),
        SessionEvent::EmojiCustomDeleted(e) if e.id == "e1"
    ));
    let SessionEvent::PermissionChanged { kind, permission } =
        decode("synthetic_permissions_changed")
    else {
        panic!()
    };
    assert_eq!(kind, ChangeKind::Updated);
    assert_eq!(permission.id, "create-c");
    assert_eq!(permission.roles.len(), 3);
    assert!(matches!(
        decode("synthetic_private_settings_changed"),
        SessionEvent::SettingChanged { public: false, setting, .. } if setting.id == "SMTP_Host"
    ));
    assert_eq!(
        decode("synthetic_banner_changed"),
        SessionEvent::BannerChanged {
            banner_id: "b1".into()
        }
    );
}

#[test]
fn notify_all_and_roles() {
    let SessionEvent::SettingChanged {
        kind,
        setting,
        public,
    } = decode("synthetic_public_settings_changed")
    else {
        panic!()
    };
    assert_eq!(kind, ChangeKind::Updated);
    assert!(public);
    assert_eq!(setting.as_str(), Some("Rocket.Chat"));
    assert!(matches!(
        decode("synthetic_update_custom_sound"),
        SessionEvent::CustomSoundUpdated(s) if s.name == "ding" && s.extension == "mp3"
    ));
    assert!(matches!(
        decode("synthetic_delete_custom_sound"),
        SessionEvent::CustomSoundDeleted(s) if s.id == "s1"
    ));
    let SessionEvent::RoleChanged { kind, role } = decode("synthetic_roles_stream") else {
        panic!()
    };
    assert_eq!(kind, ChangeKind::Updated);
    assert_eq!(role.id, "support");
    assert_eq!(role.scope.as_deref(), Some("Users"));
}

#[test]
fn notify_user_misc() {
    let SessionEvent::UserData(v) = decode("user_data") else {
        panic!()
    };
    assert_eq!(v["type"], "updated");
    assert_eq!(v["diff"]["status"], "away");
    assert_eq!(
        decode("synthetic_e2e_key_request"),
        SessionEvent::E2EKeyRequest {
            rid: RID.into(),
            key_id: "keyid1".into()
        }
    );
    assert_eq!(
        decode("synthetic_force_logout"),
        SessionEvent::ForceLogout { session_id: None }
    );
    assert!(matches!(
        decode("synthetic_ui_interaction"),
        SessionEvent::UiInteraction(v) if v["type"] == "modal.open"
    ));
    assert!(matches!(
        decode("synthetic_video_conference"),
        SessionEvent::VideoConference(v) if v["params"]["callId"] == "call1"
    ));
    assert!(matches!(
        decode("synthetic_banners"),
        SessionEvent::Banner(v) if v["_id"] == "b1"
    ));
    let SessionEvent::EphemeralMessage(m) = decode("synthetic_ephemeral_message") else {
        panic!()
    };
    assert_eq!(m.id, "eph1");
}

#[test]
fn unknown_and_malformed_events() {
    for name in [
        "synthetic_unknown_event",
        "synthetic_malformed_user_status",
        "synthetic_malformed_message",
    ] {
        let msg = load(name);
        match decode_event(&msg) {
            SessionEvent::Unknown {
                collection,
                event_name,
                args,
            } => {
                assert_eq!(collection, msg.collection);
                assert_eq!(Some(event_name.as_str()), msg.event_name());
                assert!(args.is_array());
            }
            other => panic!("{name}: {other:?}"),
        }
    }
    // No fields at all, foreign collection.
    let msg = DataMessage {
        kind: DataKind::Added,
        collection: "users".into(),
        id: "x".into(),
        fields: None,
        cleared: Vec::new(),
        before: None,
    };
    assert!(matches!(decode_event(&msg), SessionEvent::Unknown { .. }));
}

#[test]
fn every_fixture_decodes_without_panicking() {
    let dir = format!("{}/tests/fixtures/ddp", env!("CARGO_MANIFEST_DIR"));
    let mut n = 0;
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        let name = path.file_stem().unwrap().to_str().unwrap().to_owned();
        let ev = decode(&name);
        let unknown = matches!(ev, SessionEvent::Unknown { .. });
        assert_eq!(
            unknown,
            name.starts_with("synthetic_unknown") || name.starts_with("synthetic_malformed"),
            "{name}: {ev:?}"
        );
        n += 1;
    }
    assert!(n >= 40, "{n}");
}

#[test]
fn message_participation_and_md_depth() {
    let SessionEvent::Message {
        message,
        room_participant,
    } = decode("synthetic_my_messages_non_participant")
    else {
        panic!()
    };
    assert!(!room_participant);
    assert_eq!(message.id, "np1");

    // An md tree deeper than MAX_MD_DEPTH is dropped; the message survives.
    let SessionEvent::Message { message, .. } = decode("synthetic_my_messages_deep_md") else {
        panic!()
    };
    assert_eq!(message.id, "deep1");
    assert!(message.md.is_none());
    // Normal md is kept.
    let SessionEvent::Message { message, .. } = decode("my_messages_new") else {
        panic!()
    };
    assert!(message.md.is_some());
}

#[test]
fn user_status_uses_username_not_real_name() {
    assert_eq!(
        decode("synthetic_user_status_real_name"),
        SessionEvent::UserStatus {
            uid: "F3QsssYnr8Ardq4Js".into(),
            username: "bob".into(),
            status: UserStatus::Busy,
            status_text: Some("on a call".into()),
        }
    );
}

#[test]
fn bulk_delete_variants() {
    let SessionEvent::MessagesDeletedBulk(b) = decode("synthetic_delete_message_bulk_inclusive")
    else {
        panic!()
    };
    assert!(b.exclude_pinned);
    assert!(b.ignore_discussion);
    assert_eq!(b.users, vec!["bob".to_owned()]);
    assert_eq!(
        b.ids.as_deref(),
        Some(&["m1".to_owned(), "m2".to_owned()][..])
    );
    assert_eq!(b.files_only, None);
    let at = |ms| rocketchat::RcDate::from_millis(ms).unwrap();
    // $gte / $lte are inclusive.
    assert!(b.ts.contains(at(1_794_072_000_000)));
    assert!(b.ts.contains(at(1_794_072_800_000)));
    assert!(!b.ts.contains(at(1_794_071_999_999)));
    assert!(!b.ts.contains(at(1_794_072_800_001)));

    let SessionEvent::MessagesDeletedBulk(b) = decode("synthetic_delete_message_bulk_files_only")
    else {
        panic!()
    };
    assert_eq!(b.files_only, Some(true));
    assert_eq!(
        b.replace_file_attachments_with.as_ref().unwrap()["type"],
        "removed-file"
    );

    // The captured $gt/$lt shape: exclusive at both ends.
    let SessionEvent::MessagesDeletedBulk(b) = decode("delete_message_bulk") else {
        panic!()
    };
    assert!(b.ts.gt.is_some() && b.ts.lt.is_some() && b.ts.gte.is_none());
    assert!(!b.ts.contains(at(4_102_444_800_000)));
}

#[test]
fn more_room_and_user_events() {
    assert_eq!(
        decode("synthetic_messages_imported"),
        SessionEvent::MessagesImported { rid: RID.into() }
    );
    assert_eq!(
        decode("synthetic_user_activity_legacy_bool"),
        SessionEvent::UserActivity {
            rid: RID.into(),
            username: "bob".into(),
            activities: vec!["user-typing".into()],
            tmid: None,
        }
    );
    assert_eq!(
        decode("synthetic_force_logout_session"),
        SessionEvent::ForceLogout {
            session_id: Some("sess-123".into())
        }
    );
    let SessionEvent::UserData(v) = decode("synthetic_user_data_services") else {
        panic!()
    };
    assert_eq!(v["diff"]["username"], "alice2");
    assert!(v["diff"].get("services").is_none());
    assert!(v["diff"].get("services.resume.loginTokens").is_none());
    assert!(v["unset"].get("services.email2fa").is_none());

    let SessionEvent::RolesChanged(r) = decode("synthetic_roles_change_scoped") else {
        panic!()
    };
    assert_eq!(r.kind, "removed");
    assert_eq!(r.scope.as_deref(), Some(RID));
    let user = r.user.unwrap();
    assert_eq!(user.username.as_deref(), Some("bob"));
    assert!(user.name.is_none());
}
