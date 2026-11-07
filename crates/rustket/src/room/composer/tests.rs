#![allow(clippy::unwrap_used)]

use std::sync::Arc;

use iced::keyboard::{self, Key, Modifiers, key};
use iced::widget::text_editor::{Action as EditorAction, Binding, Edit, KeyPress, Status};
use rocketchat::models::{
    Message as RcMessage, Room, RoomType, SlashCommand, Subscription as Sub, User, UserRef,
};
use rocketchat::{RcDate, ServerUrl};
use rustket_core::config::UiPrefs;
use rustket_core::sync::InitialData;
use rustket_core::timeline::DeliveryState;
use rustket_core::{ServerId, Workspace};

use super::*;
use crate::error::Failure;
use crate::images::Images;

struct Env {
    sid: ServerId,
    server: ServerUrl,
    ws: Workspace,
    images: Images,
    prefs: UiPrefs,
}

impl Env {
    fn new(sid: &str) -> Self {
        let mut ws = Workspace::new();
        ws.apply_initial(InitialData {
            me: User {
                id: "me".into(),
                username: Some("me".into()),
                roles: Some(vec!["user".into()]),
                ..Default::default()
            },
            subscriptions: vec![sub("R1", "general", RoomType::Channel)],
            rooms: vec![
                room("R1", "general", RoomType::Channel),
                room("R2", "random", RoomType::Channel),
            ],
            ..Default::default()
        });
        ws.open_room("R1");
        Self {
            sid: ServerId(sid.into()),
            server: ServerUrl::parse("http://chat.test").unwrap(),
            ws,
            images: Images::new(None),
            prefs: UiPrefs::default(),
        }
    }

    fn ctx(&mut self) -> UpdateCtx<'_> {
        UpdateCtx {
            server_id: &self.sid,
            server: &self.server,
            rid: "R1",
            workspace: &mut self.ws,
            session: None,
            images: &self.images,
            prefs: &self.prefs,
        }
    }
}

fn sub(rid: &str, name: &str, t: RoomType) -> Sub {
    Sub {
        id: format!("sub-{rid}"),
        rid: rid.into(),
        name: name.into(),
        t,
        u: UserRef {
            id: "me".into(),
            username: Some("me".into()),
            name: None,
        },
        open: true,
        ..Default::default()
    }
}

fn room(rid: &str, name: &str, t: RoomType) -> Room {
    Room {
        id: rid.into(),
        t,
        name: Some(name.into()),
        ..Default::default()
    }
}

fn msg(id: &str, uid: &str, text: &str) -> RcMessage {
    RcMessage {
        id: id.into(),
        rid: "R1".into(),
        msg: text.into(),
        ts: Some(RcDate::now()),
        updated_at: Some(RcDate::now()),
        u: UserRef {
            id: uid.into(),
            username: Some(uid.into()),
            name: None,
        },
        ..Default::default()
    }
}

fn type_text(c: &mut Composer, env: &mut Env, s: &str) {
    for ch in s.chars() {
        let edit = if ch == '\n' {
            Edit::Enter
        } else {
            Edit::Insert(ch)
        };
        let _ = c.update(Message::Editor(EditorAction::Edit(edit)), &mut env.ctx());
    }
}

fn press(key: Key, modifiers: Modifiers, k: Keys) -> Option<Binding<Message>> {
    key_binding(
        KeyPress {
            key: key.clone(),
            modified_key: key,
            physical_key: keyboard::key::Physical::Unidentified(
                keyboard::key::NativeCode::Unidentified,
            ),
            modifiers,
            text: None,
            status: Status::Focused { is_hovered: false },
        },
        k,
    )
}

fn custom(b: Option<Binding<Message>>) -> Option<String> {
    match b {
        Some(Binding::Custom(m)) => Some(format!("{m:?}")),
        _ => None,
    }
}

#[test]
fn enter_sends_shift_enter_breaks_the_line() {
    let k = Keys::default();
    let enter = Key::Named(key::Named::Enter);
    assert_eq!(
        custom(press(enter.clone(), Modifiers::empty(), k)).as_deref(),
        Some("Send")
    );
    assert!(matches!(
        press(enter.clone(), Modifiers::SHIFT, k),
        Some(Binding::Enter)
    ));
    let popup = Keys { popup: true, ..k };
    assert_eq!(
        custom(press(enter, Modifiers::empty(), popup)).as_deref(),
        Some("AcConfirm")
    );
    assert_eq!(
        custom(press(
            Key::Named(key::Named::Tab),
            Modifiers::empty(),
            popup
        ))
        .as_deref(),
        Some("AcConfirm")
    );
    assert_eq!(
        custom(press(
            Key::Named(key::Named::ArrowDown),
            Modifiers::empty(),
            popup
        ))
        .as_deref(),
        Some("AcMove(1)")
    );
}

#[test]
fn up_edits_last_only_when_empty_and_escape_cancels() {
    let up = Key::Named(key::Named::ArrowUp);
    let empty = Keys {
        empty: true,
        ..Keys::default()
    };
    assert_eq!(
        custom(press(up.clone(), Modifiers::empty(), empty)).as_deref(),
        Some("EditLast")
    );
    assert!(matches!(
        press(up, Modifiers::empty(), Keys::default()),
        Some(Binding::Move(_))
    ));
    let esc = Key::Named(key::Named::Escape);
    assert!(matches!(
        press(esc.clone(), Modifiers::empty(), Keys::default()),
        Some(Binding::Unfocus)
    ));
    let editing = Keys {
        cancellable: true,
        ..Keys::default()
    };
    assert_eq!(
        custom(press(esc, Modifiers::empty(), editing)).as_deref(),
        Some("Cancel")
    );
}

#[test]
fn ctrl_v_pastes_and_checks_for_an_image() {
    let b = press(Key::Character("v".into()), Modifiers::CTRL, Keys::default());
    match b {
        Some(Binding::Sequence(v)) => {
            assert!(matches!(v[0], Binding::Paste));
            assert!(matches!(&v[1], Binding::Custom(Message::PasteCheck)));
        }
        other => panic!("unexpected {other:?}"),
    }
    assert_eq!(
        custom(press(
            Key::Character("b".into()),
            Modifiers::CTRL,
            Keys::default()
        ))
        .as_deref(),
        Some("Format(Bold)")
    );
}

#[test]
fn unfocused_editor_has_no_bindings() {
    let b = key_binding(
        KeyPress {
            key: Key::Named(key::Named::Enter),
            modified_key: Key::Named(key::Named::Enter),
            physical_key: keyboard::key::Physical::Unidentified(
                keyboard::key::NativeCode::Unidentified,
            ),
            modifiers: Modifiers::empty(),
            text: None,
            status: Status::Active,
        },
        Keys::default(),
    );
    assert!(b.is_none());
}

#[test]
fn send_goes_through_the_outbox_and_retry() {
    let mut env = Env::new("t-send");
    let mut c = Composer::new("R1");
    type_text(&mut c, &mut env, "hello\nworld  ");
    assert_eq!(c.text(), "hello\nworld  ");
    let _ = c.update(Message::Send, &mut env.ctx());
    assert_eq!(c.text(), "");
    let pending = env.ws.pending_messages("R1");
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].message.msg, "hello\nworld");
    let id = pending[0].message.id.clone();
    assert!(env.ws.timeline("R1").unwrap().contains(&id));

    // Failure marks it failed; retry sends it again; the server echo confirms it.
    let err = Failure::from(rocketchat::Error::Protocol("boom".into()));
    let _ = c.update(Message::Sent(id.clone(), Err(err)), &mut env.ctx());
    assert!(matches!(
        env.ws.pending_messages("R1")[0].state,
        DeliveryState::Failed { .. }
    ));
    let _ = c.update(Message::Retry(id.clone()), &mut env.ctx());
    assert_eq!(
        env.ws.pending_messages("R1")[0].state,
        DeliveryState::Sending
    );
    let mut echo = msg(&id, "me", "hello\nworld");
    echo.updated_at = Some(RcDate::now());
    let _ = c.update(
        Message::Sent(id.clone(), Ok(Box::new(echo))),
        &mut env.ctx(),
    );
    assert!(env.ws.pending_messages("R1").is_empty());
    assert!(
        !env.ws
            .timeline("R1")
            .unwrap()
            .get(&id)
            .unwrap()
            .is_pending()
    );
}

#[test]
fn discard_removes_a_failed_message_and_blank_text_is_not_sent() {
    let mut env = Env::new("t-discard");
    let mut c = Composer::new("R1");
    type_text(&mut c, &mut env, "   ");
    let _ = c.update(Message::Send, &mut env.ctx());
    assert!(env.ws.pending_messages("R1").is_empty());
    type_text(&mut c, &mut env, "x");
    let _ = c.update(Message::Send, &mut env.ctx());
    let id = env.ws.pending_messages("R1")[0].message.id.clone();
    let _ = c.update(Message::Discard(id), &mut env.ctx());
    assert!(env.ws.pending_messages("R1").is_empty());
}

#[test]
fn too_long_message_is_kept() {
    let mut env = Env::new("t-long");
    env.ws.apply_setting(
        rustket_core::Change::Updated,
        rocketchat::models::Setting {
            id: "Message_MaxAllowedSize".into(),
            value: serde_json::json!(3),
            ..Default::default()
        },
    );
    let mut c = Composer::new("R1");
    type_text(&mut c, &mut env, "abcd");
    let a = c.update(Message::Send, &mut env.ctx());
    assert!(matches!(
        a.effects.as_slice(),
        [Effect::Toast(BannerKind::Error, _, _)]
    ));
    assert_eq!(c.text(), "abcd");
    assert!(env.ws.pending_messages("R1").is_empty());
}

#[test]
fn thread_composer_sends_with_tmid() {
    let mut env = Env::new("t-thread");
    env.ws.open_thread("R1", "T1");
    let mut c = Composer::for_thread("R1", "T1");
    type_text(&mut c, &mut env, "reply");
    let _ = c.update(Message::Send, &mut env.ctx());
    let p = env.ws.pending_messages("R1");
    assert_eq!(p[0].message.tmid.as_deref(), Some("T1"));
}

#[test]
fn room_autocomplete_inserts_the_name() {
    let mut env = Env::new("t-rooms");
    let mut c = Composer::new("R1");
    type_text(&mut c, &mut env, "see #ran");
    let items = c.popup_items().to_vec();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].replacement(), "#random");
    let _ = c.update(Message::AcConfirm, &mut env.ctx());
    assert_eq!(c.text(), "see #random ");
    assert!(c.popup_items().is_empty());
}

#[test]
fn emoji_autocomplete_and_escape_closes_the_popup() {
    let mut env = Env::new("t-emoji");
    let mut c = Composer::new("R1");
    type_text(&mut c, &mut env, ":thumbsu");
    assert!(!c.popup_items().is_empty());
    let _ = c.update(Message::Cancel, &mut env.ctx());
    assert!(c.popup_items().is_empty());
    type_text(&mut c, &mut env, "p");
    let _ = c.update(Message::AcSelect(0), &mut env.ctx());
    assert_eq!(c.text(), ":thumbsup: ");
    // One character is too short for emoji completion.
    type_text(&mut c, &mut env, ":t");
    assert!(c.popup_items().is_empty());
}

#[test]
fn user_autocomplete_uses_cached_members_and_drops_stale_results() {
    let mut env = Env::new("t-users");
    store_members(
        &env.sid,
        "R1",
        vec![
            UserSummary {
                id: "u1".into(),
                username: Some("alice".into()),
                name: Some("Alice".into()),
                ..Default::default()
            },
            UserSummary {
                id: "u2".into(),
                username: Some("bob".into()),
                ..Default::default()
            },
        ],
    );
    let mut c = Composer::new("R1");
    type_text(&mut c, &mut env, "@a");
    let names: Vec<String> = c.popup_items().iter().map(Item::replacement).collect();
    assert_eq!(names, ["@alice"]);
    let seq_a = c.popup.as_ref().unwrap().seq;
    type_text(&mut c, &mut env, "l");
    // A response for the older "@a" query is ignored.
    let stale = vec![UserSummary {
        id: "u3".into(),
        username: Some("alfred".into()),
        ..Default::default()
    }];
    let _ = c.update(Message::AcUsers(seq_a, Ok(stale.clone())), &mut env.ctx());
    assert_eq!(c.popup_items().len(), 1);
    let seq = c.popup.as_ref().unwrap().seq;
    let _ = c.update(Message::AcUsers(seq, Ok(stale)), &mut env.ctx());
    let names: Vec<String> = c.popup_items().iter().map(Item::replacement).collect();
    assert_eq!(names, ["@alice", "@alfred"]);
    let _ = c.update(Message::AcMove(1), &mut env.ctx());
    let _ = c.update(Message::AcConfirm, &mut env.ctx());
    assert_eq!(c.text(), "@alfred ");
}

#[test]
fn slash_commands_run_known_and_send_unknown() {
    let mut env = Env::new("t-slash");
    store_commands(
        &env.sid,
        vec![SlashCommand {
            command: "leave".into(),
            ..Default::default()
        }],
    );
    let mut c = Composer::new("R1");
    type_text(&mut c, &mut env, "/lea");
    assert_eq!(c.popup_items()[0].replacement(), "/leave");
    let _ = c.update(Message::AcConfirm, &mut env.ctx());
    assert_eq!(c.text(), "/leave ");
    let _ = c.update(Message::Send, &mut env.ctx());
    assert!(
        env.ws.pending_messages("R1").is_empty(),
        "commands are not messages"
    );
    assert_eq!(c.text(), "");
    type_text(&mut c, &mut env, "/shrug me");
    let _ = c.update(Message::Send, &mut env.ctx());
    assert_eq!(env.ws.pending_messages("R1")[0].message.msg, "/shrug me");
}

#[test]
fn mention_and_insert_intents() {
    let mut env = Env::new("t-mention");
    let mut c = Composer::new("R1");
    type_text(&mut c, &mut env, "hi");
    let _ = c.intent(ComposeIntent::Mention("bob".into()), &mut env.ctx());
    assert_eq!(c.text(), "hi @bob ");
    let _ = c.intent(ComposeIntent::InsertText("x".into()), &mut env.ctx());
    assert_eq!(c.text(), "hi @bob x");
}

#[test]
fn edit_intent_and_up_edit_the_last_own_message() {
    let mut env = Env::new("t-edit");
    env.ws.apply_message(msg("M1", "me", "first"));
    env.ws.apply_message(msg("M2", "other", "theirs"));
    let mut c = Composer::new("R1");
    // Up in an empty composer picks the newest own message.
    let _ = c.update(Message::EditLast, &mut env.ctx());
    assert_eq!(c.editing_mid(), Some("M1"));
    assert_eq!(c.text(), "first");
    let _ = c.update(Message::Cancel, &mut env.ctx());
    assert!(!c.is_editing());
    assert_eq!(c.text(), "");

    // The draft is stashed while editing and restored after saving.
    type_text(&mut c, &mut env, "draft");
    let _ = c.intent(ComposeIntent::Edit("M1".into()), &mut env.ctx());
    assert_eq!(c.text(), "first");
    type_text(&mut c, &mut env, "!");
    let _ = c.update(Message::Send, &mut env.ctx());
    assert!(!c.is_editing());
    assert_eq!(c.text(), "draft");
    assert!(
        env.ws.pending_messages("R1").is_empty(),
        "edits use chat.update"
    );

    // Messages of others cannot be edited without permission.
    let a = c.intent(ComposeIntent::Edit("M2".into()), &mut env.ctx());
    assert!(!c.is_editing());
    assert_eq!(a.effects.len(), 1);
}

#[test]
fn quotes_prepend_permalinks() {
    let mut env = Env::new("t-quote");
    env.ws.apply_message(msg("M9", "other", "quoted text"));
    let mut c = Composer::new("R1");
    let _ = c.intent(ComposeIntent::Quote("M9".into()), &mut env.ctx());
    let _ = c.intent(ComposeIntent::Quote("M9".into()), &mut env.ctx());
    assert_eq!(c.quoted(), ["M9"]);
    type_text(&mut c, &mut env, "agreed");
    let _ = c.update(Message::Send, &mut env.ctx());
    let p = env.ws.pending_messages("R1");
    assert_eq!(
        p[0].message.msg,
        "[ ](http://chat.test/channel/general?msg=M9)\nagreed"
    );
    assert!(c.quoted().is_empty());
    // Escape drops quotes.
    let _ = c.intent(ComposeIntent::Quote("M9".into()), &mut env.ctx());
    let _ = c.update(Message::Cancel, &mut env.ctx());
    assert!(c.quoted().is_empty());
}

#[test]
fn react_intent_opens_the_picker_and_reports_the_emoji() {
    let mut env = Env::new("t-react");
    let mut c = Composer::new("R1");
    let _ = c.intent(ComposeIntent::React("M1".into()), &mut env.ctx());
    assert!(c.is_picker_open());
    let a = c.update(
        Message::Picker(emoji_picker::Message::Pick(":tada:".into())),
        &mut env.ctx(),
    );
    assert!(!c.is_picker_open());
    assert!(
        a.effects
            .iter()
            .any(|e| matches!(e, Effect::RecentEmoji(k) if k.starts_with(":tada")))
    );
    assert_eq!(c.text(), "", "reactions do not touch the text");
}

#[test]
fn picker_inserts_into_the_text() {
    let mut env = Env::new("t-pick");
    let mut c = Composer::new("R1");
    type_text(&mut c, &mut env, "ok");
    let _ = c.update(Message::TogglePicker, &mut env.ctx());
    assert!(c.is_picker_open());
    let _ = c.update(
        Message::Picker(emoji_picker::Message::Pick(":heart:".into())),
        &mut env.ctx(),
    );
    assert_eq!(c.text(), "ok :heart: ");
}

#[test]
fn formatting_wraps_the_selection() {
    let mut env = Env::new("t-format");
    let mut c = Composer::new("R1");
    type_text(&mut c, &mut env, "word");
    let _ = c.update(Message::Editor(EditorAction::SelectAll), &mut env.ctx());
    let _ = c.update(Message::Format(Format::Bold), &mut env.ctx());
    assert_eq!(c.text(), "*word*");
    let _ = c.update(Message::Format(Format::Bold), &mut env.ctx());
    assert_eq!(c.text(), "word");
}

#[test]
fn drafts_are_debounced_and_restored() {
    let mut env = Env::new("t-draft");
    let mut c = Composer::new("R1");
    type_text(&mut c, &mut env, "unfinished");
    let stamp = c.draft_stamp;
    let _ = c.update(Message::DraftTick(stamp - 1), &mut env.ctx());
    assert_eq!(env.ws.draft("R1", None), None, "stale tick ignored");
    let _ = c.update(Message::DraftTick(stamp), &mut env.ctx());
    assert_eq!(env.ws.draft("R1", None), Some("unfinished"));

    let mut reopened = Composer::new("R1");
    let _ = reopened.on_open(&mut env.ctx());
    assert_eq!(reopened.text(), "unfinished");
    // Sending clears the draft.
    let _ = reopened.update(Message::Send, &mut env.ctx());
    assert_eq!(env.ws.draft("R1", None), None);

    // Closing stores the text right away.
    let mut c2 = Composer::new("R1");
    type_text(&mut c2, &mut env, "later");
    c2.on_close(&mut env.ws);
    assert_eq!(env.ws.draft("R1", None), Some("later"));
}

#[test]
fn files_dropped_open_the_dialog_only_where_uploads_are_allowed() {
    let mut env = Env::new("t-drop");
    let mut c = Composer::new("R1");
    let a = c.update(
        Message::FilesDropped(vec!["/nonexistent".into()]),
        &mut env.ctx(),
    );
    assert!(a.effects.is_empty());
    let _ = c.update(
        Message::Upload(upload::Message::Loaded(vec![upload::PendingFile {
            name: "a.txt".into(),
            mime: "text/plain".into(),
            size: 3,
            source: upload::Source::Bytes(Arc::new(vec![1, 2, 3])),
            thumb: None,
            error: None,
        }])),
        &mut env.ctx(),
    );
    assert!(c.is_upload_dialog_open());
    let _ = c.update(Message::Upload(upload::Message::Close), &mut env.ctx());
    assert!(!c.is_upload_dialog_open());

    // A room the user is not subscribed to refuses uploads.
    let mut c = Composer::new("R2");
    let mut ctx = env.ctx();
    ctx.rid = "R2";
    let a = c.update(Message::FilesDropped(vec!["/x".into()]), &mut ctx);
    assert_eq!(a.effects.len(), 1);
}
