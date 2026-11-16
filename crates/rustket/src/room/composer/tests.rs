#![allow(clippy::unwrap_used)]

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

/// Runs a task to completion and returns the messages it produced.
fn outputs<M: Send + 'static>(task: Task<M>) -> Vec<M> {
    use futures::StreamExt;
    let Some(stream) = iced_runtime::task::into_stream(task) else {
        return Vec::new();
    };
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(
        stream
            .filter_map(|a| async move {
                match a {
                    iced_runtime::Action::Output(m) => Some(m),
                    _ => None,
                }
            })
            .collect::<Vec<_>>(),
    )
}

fn names(ms: &[Message]) -> Vec<String> {
    ms.iter()
        .map(|m| {
            let s = format!("{m:?}");
            s.split(['(', ' ']).next().unwrap_or_default().to_owned()
        })
        .collect()
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
    // Nothing to cancel: a captured unfocus (the right panel stays open).
    assert_eq!(
        custom(press(esc.clone(), Modifiers::empty(), Keys::default())).as_deref(),
        Some("Leave")
    );
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
fn ctrl_v_checks_for_files_and_images_before_pasting_text() {
    let b = press(
        Key::Character("v".into()),
        Modifiers::COMMAND,
        Keys::default(),
    );
    assert!(matches!(b, Some(Binding::Custom(Message::PasteCheck))));
    assert_eq!(
        custom(press(
            Key::Character("b".into()),
            Modifiers::COMMAND,
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
    let _ = c.intent(ComposeIntent::Retry(id.clone()), &mut env.ctx());
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
    let _ = c.intent(ComposeIntent::Discard(id), &mut env.ctx());
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

/// Escape with nothing to cancel: an empty thread composer closes the
/// thread (like the web); one holding a draft, or the room composer, only
/// gives up the focus.
#[test]
fn escape_closes_only_an_empty_thread() {
    let mut env = Env::new("t-leave");
    env.ws.open_thread("R1", "T1");
    let mut c = Composer::for_thread("R1", "T1");
    let a = c.update(Message::Leave, &mut env.ctx());
    assert!(matches!(a.effects.as_slice(), [Effect::ClosePanel]));
    type_text(&mut c, &mut env, "draft");
    let a = c.update(Message::Leave, &mut env.ctx());
    assert!(a.effects.is_empty());
    let mut room = Composer::new("R1");
    let a = room.update(Message::Leave, &mut env.ctx());
    assert!(a.effects.is_empty());
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

/// Unknown commands are not posted unless the server allows it
/// (`Message_AllowUnrecognizedSlashCommand`, default off, like the web).
#[test]
fn slash_commands_run_known_and_refuse_unknown() {
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
    let a = c.update(Message::Send, &mut env.ctx());
    assert!(
        env.ws.pending_messages("R1").is_empty(),
        "commands are not messages"
    );
    assert_eq!(c.text(), "");
    // `commands.run` is handed to the UIKit controller (app level).
    assert!(runs_command(&a.effects, "leave"));
    type_text(&mut c, &mut env, "/remnd me about card 4111");
    let a = c.update(Message::Send, &mut env.ctx());
    assert!(env.ws.pending_messages("R1").is_empty(), "not posted");
    assert_eq!(c.text(), "/remnd me about card 4111", "kept for a fix");
    assert!(
        a.effects
            .iter()
            .any(|e| matches!(e, Effect::Toast(_, title, _) if title.contains("/remnd")))
    );
    c.content = text_editor::Content::new();
    env.ws.apply_setting(
        rustket_core::Change::Updated,
        rocketchat::models::Setting {
            id: "Message_AllowUnrecognizedSlashCommand".into(),
            value: serde_json::json!(true),
            ..Default::default()
        },
    );
    type_text(&mut c, &mut env, "/shrug me");
    let a = c.update(Message::Send, &mut env.ctx());
    assert_eq!(env.ws.pending_messages("R1")[0].message.msg, "/shrug me");
    assert!(names(&outputs(a.task)).contains(&"Sent".to_owned()));
}

#[test]
fn uncached_commands_keep_the_text_until_the_list_is_known() {
    let mut env = Env::new("t-slash-uncached");
    let mut c = Composer::new("R1");
    type_text(&mut c, &mut env, "/leave");
    let _ = c.update(Message::AcClose, &mut env.ctx());
    // Without a session the list cannot load: the text stays, nothing is sent.
    let a = c.update(Message::Send, &mut env.ctx());
    assert_eq!(a.effects.len(), 1);
    assert_eq!(c.text(), "/leave");
    assert!(env.ws.pending_messages("R1").is_empty());

    // A failed load does not retry on its own and keeps the text.
    let err = Failure::from(rocketchat::Error::Protocol("down".into()));
    let a = c.update(
        Message::CommandsLoaded(Err(err), Some("/leave".into())),
        &mut env.ctx(),
    );
    assert_eq!(a.effects.len(), 1);
    assert!(outputs(a.task).is_empty(), "no new request");
    assert_eq!(c.text(), "/leave");
    assert!(commands_recently_failed(&env.sid));

    // An empty list (Apps-Engine still loading) is not cached either.
    let a = c.update(
        Message::CommandsLoaded(Ok(Vec::new()), Some("/leave".into())),
        &mut env.ctx(),
    );
    assert_eq!(a.effects.len(), 1);
    assert!(cached_commands(&env.sid).is_none());

    // Once the list arrives the waiting text is run as a command.
    let list = vec![SlashCommand {
        command: "leave".into(),
        ..Default::default()
    }];
    let a = c.update(
        Message::CommandsLoaded(Ok(list), Some("/leave".into())),
        &mut env.ctx(),
    );
    assert_eq!(c.text(), "");
    assert!(env.ws.pending_messages("R1").is_empty());
    assert!(runs_command(&a.effects, "leave"));
}

fn runs_command(effects: &[Effect], name: &str) -> bool {
    effects.iter().any(|e| {
        matches!(e, Effect::UiKit(m)
            if matches!(&**m, crate::uikit::UiKitMsg::Command { command, .. } if command == name))
    })
}

#[test]
fn send_results_of_a_closed_room_are_applied_on_reopen() {
    let mut env = Env::new("t-outbox-closed");
    let mut c = Composer::new("R1");
    type_text(&mut c, &mut env, "one");
    let a = c.update(Message::Send, &mut env.ctx());
    let id = env.ws.pending_messages("R1")[0].message.id.clone();
    assert!(outbox_in_flight(&env.sid, &id));
    // The room closes before the result arrives; the task still finishes
    // (the failure is recorded) but its message is dropped.
    c.on_close(&mut env.ws);
    drop(c);
    let _ = outputs(a.task);
    assert_eq!(
        env.ws.pending_messages("R1")[0].state,
        DeliveryState::Sending
    );
    let mut reopened = Composer::new("R1");
    let _ = reopened.on_open(&mut env.ctx());
    assert!(matches!(
        env.ws.pending_messages("R1")[0].state,
        DeliveryState::Failed { .. }
    ));

    // A message stuck in Sending with no request at all becomes failed too.
    let m = env.ws.new_local_message("R1", "orphan", None, false);
    let orphan = m.id.clone();
    env.ws.add_pending(m);
    let mut again = Composer::new("R1");
    let _ = again.on_open(&mut env.ctx());
    let state = env
        .ws
        .pending_messages("R1")
        .into_iter()
        .find(|i| i.message.id == orphan)
        .unwrap()
        .state
        .clone();
    assert!(matches!(state, DeliveryState::Failed { .. }));
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
fn encrypted_room_without_e2e_settings_sends_nothing() {
    // `settings.public` failed during the load: E2E state unknown.
    let mut env = Env::new("t-e2e-unknown");
    let mut r = room("R1", "general", RoomType::Private);
    r.encrypted = Some(true);
    env.ws.apply_room(rustket_core::Change::Updated, r);
    env.ws.apply_message(msg("M1", "me", "plain before"));
    let mut c = Composer::new("R1");
    type_text(&mut c, &mut env, "secret");
    let a = c.update(Message::Send, &mut env.ctx());
    assert_eq!(c.text(), "secret", "the text stays");
    assert!(env.ws.pending_messages("R1").is_empty());
    assert_eq!(a.effects.len(), 1);
    // Edits are refused before they start.
    let a = c.intent(ComposeIntent::Edit("M1".into()), &mut env.ctx());
    assert!(!c.is_editing());
    assert_eq!(a.effects.len(), 1);
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

/// A message not shown decrypted is never quoted (any caller, e.g. the
/// inbox's Reply): its server text would show as the quote.
#[test]
fn undecrypted_messages_are_not_quoted() {
    let mut env = Env::new("t-quote-e2e");
    let mut m = msg("M9", "other", "server plaintext");
    m.t = Some("e2e".into());
    env.ws.apply_message(m);
    let mut c = Composer::new("R1");
    let a = c.intent(ComposeIntent::Quote("M9".into()), &mut env.ctx());
    assert!(c.quoted().is_empty());
    assert!(matches!(a.effects.as_slice(), [Effect::Toast(..)]));
    // Fetched (a room not open yet, e.g. an inbox Reply): decrypted first,
    // and still undecrypted here (no E2E), so not quoted, with a toast.
    let mut f = msg("M10", "other", "server plaintext");
    f.t = Some("e2e".into());
    let a = c.update(
        Message::Fetched(FetchFor::Quote, Ok(Box::new(f))),
        &mut env.ctx(),
    );
    assert!(c.quoted().is_empty());
    assert!(matches!(a.effects.as_slice(), [Effect::Toast(..)]));
    let a = c.update(
        Message::Fetched(FetchFor::Quote, Ok(Box::new(msg("M11", "other", "plain")))),
        &mut env.ctx(),
    );
    assert_eq!(c.quoted().len(), 1);
    assert!(a.effects.is_empty());
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
fn formatting_uses_double_click_word_selection() {
    let mut env = Env::new("t-format-word");
    let mut c = Composer::new("R1");
    type_text(&mut c, &mut env, "hello world");
    let _ = c.update(
        Message::Editor(EditorAction::Move(text_editor::Motion::Left)),
        &mut env.ctx(),
    );
    let _ = c.update(
        Message::Editor(EditorAction::Move(text_editor::Motion::Left)),
        &mut env.ctx(),
    );
    let _ = c.update(Message::Editor(EditorAction::SelectWord), &mut env.ctx());
    let _ = c.update(Message::Format(Format::Bold), &mut env.ctx());
    assert_eq!(c.text(), "hello *world*");
    let _ = c.update(Message::Editor(EditorAction::SelectLine), &mut env.ctx());
    let _ = c.update(Message::Format(Format::Strike), &mut env.ctx());
    assert_eq!(c.text(), "~hello *world*~");
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
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("notes.txt");
    std::fs::write(&path, b"hello").unwrap();
    let mut c = Composer::new("R1");
    let a = c.update(Message::FilesDropped(vec![path]), &mut env.ctx());
    assert!(a.effects.is_empty());
    // Dropped paths are read off the UI thread, then shown in the dialog.
    for m in outputs(a.task) {
        let _ = c.update(m, &mut env.ctx());
    }
    assert!(c.is_upload_dialog_open());
    let d = c.uploads.dialog.as_ref().unwrap();
    assert_eq!(d.name, "notes.txt");
    assert_eq!(d.current().unwrap().size, 5);

    // Upload without a connection keeps the file in the dialog.
    let a = c.update(Message::Upload(upload::Message::Send), &mut env.ctx());
    assert_eq!(a.effects.len(), 1);
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

#[test]
fn escape_with_an_empty_popup_cancels_the_edit() {
    let mut env = Env::new("t-esc-empty");
    env.ws.apply_message(msg("M1", "me", "first"));
    let mut c = Composer::new("R1");
    let _ = c.update(Message::EditLast, &mut env.ctx());
    type_text(&mut c, &mut env, " :zzzzqq");
    assert!(c.popup.is_some() && c.popup_items().is_empty());
    let _ = c.update(Message::Cancel, &mut env.ctx());
    assert!(!c.is_editing());
}

/// Review: Up right after sending edited the previous own message (the
/// just-sent one is still pending and cannot be edited yet).
#[test]
fn up_right_after_sending_does_not_edit_an_older_message() {
    let mut env = Env::new("t-up-pending");
    // Older by a second: a pending message in the same millisecond may sort
    // before it.
    let mut old = msg("OLD", "me", "older message");
    old.ts = RcDate::from_millis(RcDate::now().timestamp_millis() - 1000);
    env.ws.apply_message(old);
    let mut c = Composer::new("R1");
    type_text(&mut c, &mut env, "helo wrld");
    let _ = c.update(Message::Send, &mut env.ctx());
    let pending = env.ws.pending_messages("R1")[0].message.clone();
    let _ = c.update(Message::EditLast, &mut env.ctx());
    assert_eq!(c.editing_mid(), None, "not the older message");
    // Delivered: now it is the one edited.
    let mut sent = pending.clone();
    sent.temp = None;
    sent.ts = Some(RcDate::now());
    sent.updated_at = Some(RcDate::now());
    env.ws.apply_message(sent);
    let _ = c.update(Message::EditLast, &mut env.ctx());
    assert_eq!(c.editing_mid(), Some(pending.id.as_str()));
}

/// Review: a failed (never delivered) own message blocked Up for good.
#[test]
fn up_skips_a_failed_message() {
    let mut env = Env::new("t-up-failed");
    env.ws.apply_message(msg("OLD", "me", "older message"));
    let mut c = Composer::new("R1");
    type_text(&mut c, &mut env, "helo wrld");
    let _ = c.update(Message::Send, &mut env.ctx());
    let pending = env.ws.pending_messages("R1")[0].message.clone();
    env.ws.mark_pending_failed("R1", None, &pending.id, "boom");
    let _ = c.update(Message::EditLast, &mut env.ctx());
    assert_eq!(c.editing_mid(), Some("OLD"));
}

#[test]
fn up_skips_thread_replies_in_the_room_composer() {
    let mut env = Env::new("t-up-tshow");
    env.ws.apply_message(msg("M1", "me", "room message"));
    let mut reply = msg("M2", "me", "thread reply");
    reply.tmid = Some("M1".into());
    reply.tshow = Some(true);
    env.ws.apply_message(reply);
    let mut c = Composer::new("R1");
    let _ = c.update(Message::EditLast, &mut env.ctx());
    assert_eq!(c.editing_mid(), Some("M1"));
}

#[test]
fn emptied_edit_asks_to_delete_and_quotes_need_text() {
    let mut env = Env::new("t-edit-empty");
    env.ws.apply_message(msg("M1", "me", "x"));
    env.ws.apply_message(msg("M9", "other", "quoted"));
    let mut c = Composer::new("R1");
    let _ = c.update(Message::EditLast, &mut env.ctx());
    let _ = c.update(Message::Editor(EditorAction::SelectAll), &mut env.ctx());
    let _ = c.update(
        Message::Editor(EditorAction::Edit(Edit::Backspace)),
        &mut env.ctx(),
    );
    let _ = c.update(Message::Send, &mut env.ctx());
    assert_eq!(c.confirm_delete.as_deref(), Some("M1"));
    let _ = c.update(Message::Cancel, &mut env.ctx());
    assert!(c.confirm_delete.is_none() && c.is_editing());
    let _ = c.update(Message::Send, &mut env.ctx());
    let a = c.update(Message::DeleteConfirmed, &mut env.ctx());
    assert!(!c.is_editing());
    assert!(names(&outputs(a.task)).contains(&"Deleted".to_owned()));

    let _ = c.intent(ComposeIntent::Quote("M9".into()), &mut env.ctx());
    let _ = c.update(Message::Send, &mut env.ctx());
    assert!(
        env.ws.pending_messages("R1").is_empty(),
        "quote-only refused"
    );
    assert_eq!(c.quoted(), ["M9"]);
}

#[test]
fn length_limit_counts_utf16_of_the_final_text() {
    let mut env = Env::new("t-utf16");
    env.ws.apply_setting(
        rustket_core::Change::Updated,
        rocketchat::models::Setting {
            id: "Message_MaxAllowedSize".into(),
            value: serde_json::json!(45),
            ..Default::default()
        },
    );
    env.ws.apply_message(msg("M9", "other", "quoted"));
    let mut c = Composer::new("R1");
    // 23 emoji = 23 chars but 46 UTF-16 units.
    type_text(&mut c, &mut env, &"\u{1F600}".repeat(23));
    let a = c.update(Message::Send, &mut env.ctx());
    assert_eq!(a.effects.len(), 1);
    assert!(env.ws.pending_messages("R1").is_empty());
    // Short text, but the quote link makes it too long.
    let _ = c.update(Message::Editor(EditorAction::SelectAll), &mut env.ctx());
    let _ = c.update(
        Message::Editor(EditorAction::Edit(Edit::Backspace)),
        &mut env.ctx(),
    );
    type_text(&mut c, &mut env, "ok");
    let _ = c.intent(ComposeIntent::Quote("M9".into()), &mut env.ctx());
    let a = c.update(Message::Send, &mut env.ctx());
    assert_eq!(a.effects.len(), 1);
    assert!(env.ws.pending_messages("R1").is_empty());
    assert_eq!(c.text(), "ok");
}

#[test]
fn react_follows_can_react_in_blocked_rooms() {
    let mut env = Env::new("t-react-ro");
    let mut ro = room("R1", "general", RoomType::Channel);
    ro.ro = Some(true);
    env.ws.apply_room(rustket_core::Change::Updated, ro);
    assert!(permissions::post_block(&env.ws, "R1").is_some());
    let mut c = Composer::new("R1");
    let _ = c.intent(ComposeIntent::React("M1".into()), &mut env.ctx());
    assert!(!c.is_picker_open(), "read-only without reactWhenReadOnly");

    let mut archived = room("R1", "general", RoomType::Channel);
    archived.archived = Some(true);
    env.ws.apply_room(rustket_core::Change::Updated, archived);
    let _ = c.intent(ComposeIntent::React("M1".into()), &mut env.ctx());
    assert!(c.is_picker_open(), "archived rooms accept reactions");
}

#[test]
fn join_code_rooms_need_a_code() {
    let mut env = Env::new("t-join-code");
    let mut r = room("R2", "secret", RoomType::Channel);
    r.join_code_required = Some(true);
    env.ws.apply_room(rustket_core::Change::Updated, r);
    assert!(needs_join_code(&env.ws, "R2"));
    let mut c = Composer::new("R2");
    let mut ctx = env.ctx();
    ctx.rid = "R2";
    let a = c.update(Message::Join, &mut ctx);
    assert!(outputs(a.task).is_empty(), "no request without a code");
    let _ = c.update(Message::JoinCode("1234".into()), &mut ctx);
    let a = c.update(Message::Join, &mut ctx);
    assert!(names(&outputs(a.task)).contains(&"Joined".to_owned()));
}

#[test]
fn private_discussions_are_joinable() {
    let mut env = Env::new("t-join-discussion");
    let mut d = room("D1", "talk", RoomType::Private);
    d.prid = Some("P1".into());
    env.ws.apply_room(rustket_core::Change::Updated, d);
    env.ws.apply_room(
        rustket_core::Change::Updated,
        room("G1", "group", RoomType::Private),
    );
    assert!(can_join(&env.ws, "D1"));
    assert!(can_join(&env.ws, "R2"));
    assert!(!can_join(&env.ws, "G1"));
    let mut c = Composer::new("D1");
    let mut ctx = env.ctx();
    ctx.rid = "D1";
    let a = c.update(Message::Join, &mut ctx);
    assert!(names(&outputs(a.task)).contains(&"Joined".to_owned()));
}

#[test]
fn leaving_stops_typing_once() {
    let mut env = Env::new("t-leave");
    let mut c = Composer::for_thread("R1", "T1");
    type_text(&mut c, &mut env, "hel");
    assert!(c.typing.is_active());
    // Without a session nothing can be sent, but the indicator is over.
    let _ = c.leave(None);
    assert!(!c.typing.is_active());
    assert_eq!(c.leave(None).units(), 0);
}

fn view_ctx(env: &Env) -> ViewCtx<'_> {
    ViewCtx {
        server_id: &env.sid,
        server: &env.server,
        rid: "R1",
        workspace: &env.ws,
        images: &env.images,
        prefs: &env.prefs,
        connected: true,
        panel: None,
        now: std::time::Instant::now(),
    }
}

/// The typing label, the edit bar and quote chips were children of the
/// column before the editor: each one appearing or disappearing moved the
/// editor to another position and rebuilt its tree (focus lost while
/// typing, e.g. when someone else started typing).
#[test]
fn editor_keeps_its_tree_when_bars_and_popups_come_and_go() {
    use crate::tree_probe::{assert_kept, text_editor_tag};
    use iced::advanced::widget::Tree;
    use std::time::{Duration, Instant};
    let mut env = Env::new("t-tree");
    env.ws.apply_message(msg("M1", "me", "first"));
    let mut c = Composer::new("R1");
    let ed = text_editor_tag();
    let mut tree = Tree::new(c.view(&view_ctx(&env)).as_widget());
    type Change = dyn Fn(&mut Composer, &mut Env);
    let steps: [(&str, &Change); 6] = [
        ("someone types", &|_, env| {
            env.ws.apply_user_activity_at(
                "R1",
                "bob",
                &["user-typing".into()],
                None,
                Instant::now(),
            );
        }),
        ("edit bar", &|c, env| {
            let _ = c.update(Message::EditLast, &mut env.ctx());
            assert!(c.is_editing());
        }),
        ("popup opens", &|c, env| {
            type_text(c, env, " :thumbsu");
            assert!(!c.popup_items().is_empty());
        }),
        ("popup closes", &|c, env| {
            let _ = c.update(Message::Cancel, &mut env.ctx());
            assert!(c.popup_items().is_empty());
        }),
        ("edit cancelled", &|c, env| {
            let _ = c.update(Message::CancelEdit, &mut env.ctx());
            assert!(!c.is_editing());
        }),
        ("typing stops", &|_, env| {
            env.ws
                .expire_typing(Instant::now() + Duration::from_secs(3600));
        }),
    ];
    for (step, change) in steps {
        change(&mut c, &mut env);
        assert_kept(&mut tree, &c.view(&view_ctx(&env)), ed, step);
    }
}

/// The E2E "sent unencrypted" hint shares the bars slot: E2E being switched
/// on for the room (or the room key arriving) must not rebuild the editor.
#[test]
fn editor_keeps_its_tree_when_the_e2e_hint_comes_and_goes() {
    use crate::tree_probe::{assert_kept, text_editor_tag};
    use iced::advanced::widget::Tree;
    let mut env = Env::new("t-tree-e2e");
    let set = |env: &mut Env, id: &str, v: bool| {
        env.ws.apply_setting(
            rustket_core::Change::Updated,
            rocketchat::models::Setting {
                id: id.into(),
                value: serde_json::json!(v),
                ..Default::default()
            },
        );
    };
    let encrypted = |env: &mut Env, on: bool| {
        let mut r = room("R1", "general", RoomType::Channel);
        r.encrypted = Some(on);
        env.ws.apply_room(rustket_core::Change::Updated, r);
    };
    let mut c = Composer::new("R1");
    type_text(&mut c, &mut env, "hello");
    let ed = text_editor_tag();
    let mut tree = Tree::new(c.view(&view_ctx(&env)).as_widget());
    set(&mut env, "E2E_Enable", true);
    set(&mut env, "E2E_Allow_Unencrypted_Messages", true);
    encrypted(&mut env, true);
    assert!(crate::e2e::room::composer_hint::<()>(&env.ws, "R1").is_some());
    assert_kept(&mut tree, &c.view(&view_ctx(&env)), ed, "hint appears");
    encrypted(&mut env, false);
    assert!(crate::e2e::room::composer_hint::<()>(&env.ws, "R1").is_none());
    assert_kept(&mut tree, &c.view(&view_ctx(&env)), ed, "hint goes");
}

fn press_with(
    key: Key,
    physical: keyboard::key::Physical,
    modifiers: Modifiers,
    k: Keys,
) -> Option<Binding<Message>> {
    key_binding(
        KeyPress {
            key: key.clone(),
            modified_key: key,
            physical_key: physical,
            modifiers,
            text: None,
            status: Status::Focused { is_hovered: false },
        },
        k,
    )
}

#[test]
fn slack_formatting_chords_beat_copy_and_cut() {
    let cs = Modifiers::COMMAND | Modifiers::SHIFT;
    let k = Keys::default();
    for (c, want) in [
        ("x", "Format(Strike)"),
        ("c", "Format(Code)"),
        ("u", "Format(Link)"),
        ("9", "Format(Quote)"),
    ] {
        assert_eq!(
            custom(press(Key::Character(c.into()), cs, k)).as_deref(),
            Some(want),
            "{c}"
        );
    }
    assert_eq!(
        custom(press(Key::Character("c".into()), cs | Modifiers::ALT, k)).as_deref(),
        Some("Format(CodeBlock)")
    );
    // Plain Ctrl+C still copies.
    assert!(matches!(
        press(Key::Character("c".into()), Modifiers::COMMAND, k),
        Some(Binding::Copy)
    ));
    // Russian layout: Cyrillic `с` on the C key.
    let on_c = keyboard::key::Physical::Code(keyboard::key::Code::KeyC);
    assert_eq!(
        custom(press_with(Key::Character("с".into()), on_c, cs, k)).as_deref(),
        Some("Format(Code)")
    );
}

#[test]
fn app_wide_chords_are_swallowed_by_the_composer() {
    let popup = Keys {
        popup: true,
        cancellable: true,
        ..Keys::default()
    };
    let swallowed =
        |b: Option<Binding<Message>>| matches!(b, Some(Binding::Sequence(v)) if v.is_empty());
    // Shift+Esc asks for "Mark all as read" instead of closing the popup
    // (the app listener skips captured Shift+Esc).
    assert_eq!(
        custom(press(
            Key::Named(key::Named::Escape),
            Modifiers::SHIFT,
            popup
        ))
        .as_deref(),
        Some("AskReadAll")
    );
    assert!(swallowed(press(
        Key::Character("/".into()),
        Modifiers::COMMAND,
        Keys::default()
    )));
    assert!(swallowed(press(
        Key::Character("k".into()),
        Modifiers::COMMAND,
        Keys::default()
    )));
    assert!(swallowed(press(
        Key::Character(",".into()),
        Modifiers::COMMAND,
        Keys::default()
    )));
    // The composer reacts to its own timeline.
    assert_eq!(
        custom(press(
            Key::Character("\\".into()),
            Modifiers::COMMAND | Modifiers::SHIFT,
            Keys::default()
        ))
        .as_deref(),
        Some("ReactLast")
    );
    // All unreads, Activity and Later work from the composer too.
    for c in ["a", "m", "s"] {
        assert!(swallowed(press(
            Key::Character(c.into()),
            Modifiers::COMMAND | Modifiers::SHIFT,
            Keys::default()
        )));
    }
    // Other chords still reach the editor.
    assert!(!swallowed(press(
        Key::Character("d".into()),
        Modifiers::COMMAND | Modifiers::SHIFT,
        Keys::default()
    )));
}

#[test]
fn react_last_targets_the_newest_message_of_room_or_thread() {
    let mut env = Env::new("t-react-last");
    let mut c = Composer::new("R1");
    let _ = c.update(Message::ReactLast, &mut env.ctx());
    assert!(!c.is_picker_open(), "nothing to react to");
    env.ws.apply_message(msg("M1", "other", "first"));
    env.ws.apply_message(msg("M2", "me", "second"));
    let mut reply = msg("M3", "other", "reply shown in the channel");
    reply.tmid = Some("M1".into());
    reply.tshow = Some(true);
    env.ws.apply_message(reply.clone());
    let _ = c.update(Message::ReactLast, &mut env.ctx());
    assert_eq!(c.react_target(), Some("M2"), "thread replies are skipped");

    env.ws.open_thread("R1", "M1");
    env.ws.apply_message(reply);
    let mut t = Composer::for_thread("R1", "M1");
    let _ = t.update(Message::ReactLast, &mut env.ctx());
    assert_eq!(t.react_target(), Some("M3"));
}

#[test]
fn shift_escape_asks_the_app_for_mark_all_read() {
    let mut env = Env::new("t-ask-read-all");
    for mut c in [Composer::new("R1"), Composer::for_thread("R1", "M1")] {
        let a = c.update(Message::AskReadAll, &mut env.ctx());
        assert!(matches!(a.effects.as_slice(), [Effect::AskReadAll]));
    }
}

/// `R1` with `users` members (the sender included) and the mention
/// permissions given to the `user` role.
fn broad_env(name: &str, users: Option<u64>, perms: &[&str]) -> Env {
    use rocketchat::models::Permission;
    let mut env = Env::new(name);
    let mut r = room("R1", "general", RoomType::Channel);
    r.users_count = users;
    env.ws.apply_room(rustket_core::Change::Updated, r);
    for p in perms {
        env.ws.apply_permission(
            rustket_core::Change::Updated,
            Permission {
                id: (*p).into(),
                roles: vec!["user".into()],
                updated_at: None,
            },
        );
    }
    env
}

#[test]
fn broad_mentions_ask_before_sending() {
    let mut env = broad_env("t-broad", Some(11), &["mention-all", "mention-here"]);
    let mut c = Composer::new("R1");
    type_text(&mut c, &mut env, "release is out @all");
    let a = c.update(Message::Send, &mut env.ctx());
    assert!(a.effects.is_empty());
    assert!(
        env.ws.pending_messages("R1").is_empty(),
        "waits for the answer"
    );
    let ask = c.confirm_broad.clone().unwrap();
    assert!(ask.all && !ask.here);
    assert_eq!(ask.members, Some(10));
    assert_eq!(ask.room, "#general");
    assert!(c.is_confirm_open());
    // Cancel keeps the text; Escape does the same.
    let _ = c.update(Message::BroadCancelled, &mut env.ctx());
    assert!(c.confirm_broad.is_none());
    assert_eq!(c.text(), "release is out @all");
    let _ = c.update(Message::Send, &mut env.ctx());
    let _ = c.update(Message::Cancel, &mut env.ctx());
    assert!(c.confirm_broad.is_none());
    assert_eq!(c.text(), "release is out @all");
    // Confirmed: sent once, and the next one asks again.
    let _ = c.update(Message::Send, &mut env.ctx());
    let _ = c.update(Message::BroadConfirmed, &mut env.ctx());
    let p = env.ws.pending_messages("R1");
    assert_eq!(p.len(), 1);
    assert_eq!(p[0].message.msg, "release is out @all");
    assert!(!c.broad_ok);
    type_text(&mut c, &mut env, "@here standup");
    let a = c.update(Message::Send, &mut env.ctx());
    let ask = c.confirm_broad.clone().unwrap();
    assert!(ask.here && !ask.all);
    // The online count comes in for this dialog only (the sender excluded).
    assert!(names(&outputs(a.task)).contains(&"OnlineCount".to_owned()));
    let _ = c.update(Message::OnlineCount(ask.seq + 1, Ok(9)), &mut env.ctx());
    assert_eq!(c.confirm_broad.as_ref().unwrap().online, None);
    let _ = c.update(Message::OnlineCount(ask.seq, Ok(4)), &mut env.ctx());
    assert_eq!(c.confirm_broad.as_ref().unwrap().online, Some(3));
}

#[test]
fn broad_mentions_send_directly_when_not_needed() {
    let sends = |env: &mut Env, text: &str| {
        let mut c = Composer::new("R1");
        type_text(&mut c, env, text);
        let _ = c.update(Message::Send, &mut env.ctx());
        c.confirm_broad.is_none() && c.text().is_empty()
    };
    // Fewer than five other members.
    let mut env = broad_env("t-broad-small", Some(5), &["mention-all"]);
    assert!(sends(&mut env, "@all hi"));
    // No permission: the server would refuse the mention anyway.
    let mut env = broad_env("t-broad-perm", Some(50), &[]);
    assert!(sends(&mut env, "@all hi"));
    // Inside code, or another word.
    let mut env = broad_env("t-broad-code", Some(50), &["mention-all"]);
    assert!(sends(&mut env, "`@all` and @allison"));
    // Switched off in the settings.
    let mut env = broad_env("t-broad-off", Some(50), &["mention-all"]);
    env.prefs.confirm_broad_mentions = false;
    assert!(sends(&mut env, "@all hi"));
    // An unknown member count asks.
    let mut env = broad_env("t-broad-unknown", None, &["mention-all"]);
    assert!(!sends(&mut env, "@all hi"));
    // Edits never ask (the server notifies new messages only).
    let mut env = broad_env("t-broad-edit", Some(50), &["mention-all"]);
    env.ws.apply_message(msg("M1", "me", "hello"));
    let mut c = Composer::new("R1");
    let _ = c.update(Message::EditLast, &mut env.ctx());
    type_text(&mut c, &mut env, " @all");
    let _ = c.update(Message::Send, &mut env.ctx());
    assert!(c.confirm_broad.is_none() && !c.is_editing());
}

#[test]
fn direct_messages_never_ask_about_broad_mentions() {
    let mut env = broad_env("t-broad-dm", Some(50), &["mention-all"]);
    let mut r = room("R1", "bob", RoomType::Direct);
    r.users_count = Some(50);
    env.ws.apply_room(rustket_core::Change::Updated, r);
    env.ws.apply_subscription(
        rustket_core::Change::Updated,
        sub("R1", "bob", RoomType::Direct),
    );
    assert!(broad::broad_check(&env.ws, "R1", "@all").is_none());
}

#[test]
fn upload_message_with_broad_mentions_asks_first() {
    let mut env = broad_env("t-broad-upload", Some(20), &["mention-all"]);
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("notes.txt");
    std::fs::write(&path, b"hello").unwrap();
    let mut c = Composer::new("R1");
    let a = c.update(Message::FilesDropped(vec![path]), &mut env.ctx());
    for m in outputs(a.task) {
        let _ = c.update(m, &mut env.ctx());
    }
    let _ = c.update(
        Message::Upload(upload::Message::Text("@all minutes".into())),
        &mut env.ctx(),
    );
    let a = c.update(Message::Upload(upload::Message::Send), &mut env.ctx());
    assert!(a.effects.is_empty(), "asks before the connection check");
    let d = c.uploads.dialog.as_ref().unwrap();
    assert!(d.confirm.as_ref().is_some_and(|c| c.all));
    assert!(matches!(
        c.uploads.dismiss(),
        upload::Message::BroadCancelled
    ));
    let _ = c.update(
        Message::Upload(upload::Message::BroadCancelled),
        &mut env.ctx(),
    );
    let d = c.uploads.dialog.as_ref().unwrap();
    assert!(d.confirm.is_none() && d.message == "@all minutes");
    // Confirmed: goes on to the upload (refused here: not connected).
    let _ = c.update(Message::Upload(upload::Message::Send), &mut env.ctx());
    let a = c.update(
        Message::Upload(upload::Message::BroadConfirmed),
        &mut env.ctx(),
    );
    assert_eq!(a.effects.len(), 1, "the not-connected toast");
    assert!(c.uploads.dialog.as_ref().unwrap().confirm.is_none());
}

#[test]
fn forwarded_quotes_link_their_room_and_need_no_comment() {
    let mut env = Env::new("t-forward");
    let refs = vec![
        crate::room::ForwardRef {
            rid: "R2".into(),
            mid: "M1".into(),
            author: "bob".into(),
            excerpt: "first".into(),
        },
        crate::room::ForwardRef {
            rid: "R2".into(),
            mid: "M2".into(),
            author: "bob".into(),
            excerpt: "second".into(),
        },
    ];
    let mut c = Composer::new("R1");
    assert!(!c.can_send());
    let _ = c.intent(ComposeIntent::Forward(refs.clone()), &mut env.ctx());
    let _ = c.intent(ComposeIntent::Forward(refs), &mut env.ctx());
    assert!(c.can_send(), "the Send button works without a comment");
    assert_eq!(c.quoted(), ["M1", "M2"], "deduplicated");
    assert!(c.quotes.iter().all(|q| q.forwarded && q.rid == "R2"));
    let _ = c.update(Message::Send, &mut env.ctx());
    let p = env.ws.pending_messages("R1");
    assert_eq!(
        p[0].message.msg,
        "[ ](http://chat.test/channel/random?msg=M1)\n[ ](http://chat.test/channel/random?msg=M2)"
    );
    assert!(c.quoted().is_empty());
    // A plain quote still needs a comment.
    env.ws.apply_message(msg("M9", "other", "quoted"));
    let _ = c.intent(ComposeIntent::Quote("M9".into()), &mut env.ctx());
    assert!(!c.can_send());
    let _ = c.update(Message::Send, &mut env.ctx());
    assert_eq!(env.ws.pending_messages("R1").len(), 1);
    assert_eq!(c.quotes[0].rid, "R1");
    assert!(!c.quotes[0].forwarded);
}

#[test]
fn dm_partner_local_time_is_cached_per_server() {
    let mut env = Env::new("t-peer");
    let mut r = room("D1", "bob", RoomType::Direct);
    r.usernames = Some(vec!["me".into(), "bob".into()]);
    env.ws.apply_room(rustket_core::Change::Updated, r);
    env.ws.apply_subscription(
        rustket_core::Change::Updated,
        sub("D1", "bob", RoomType::Direct),
    );
    let mut c = Composer::new("D1");
    let a = {
        let mut ctx = env.ctx();
        ctx.rid = "D1";
        c.on_open(&mut ctx)
    };
    assert_eq!(
        c.peer.as_ref().map(|(u, i)| (u.as_str(), i.is_none())),
        Some(("bob", true))
    );
    assert!(names(&outputs(a.task)).contains(&"PeerLoaded".to_owned()));
    // Twelve hours away from here always differs by more than 30 minutes.
    let mine = f64::from(chrono::Local::now().offset().local_minus_utc()) / 3600.0;
    let far = if mine > 0.0 { mine - 12.0 } else { mine + 12.0 };
    let user = User {
        id: "u-bob".into(),
        username: Some("bob".into()),
        name: Some("Bob B".into()),
        utc_offset: Some(far),
        ..Default::default()
    };
    let _ = c.update(
        Message::PeerLoaded("bob".into(), Ok(Box::new(user))),
        &mut env.ctx(),
    );
    let (u, info, clock) = c.peer_clock().unwrap();
    assert_eq!((u, info.name.as_deref()), ("bob", Some("Bob B")));
    assert_eq!(clock.offset_secs, (far * 3600.0).round() as i32);
    // Another composer of the same server uses the cache.
    let mut c2 = Composer::new("D1");
    let a = {
        let mut ctx = env.ctx();
        ctx.rid = "D1";
        c2.on_open(&mut ctx)
    };
    assert!(c2.peer_clock().is_some());
    assert!(!names(&outputs(a.task)).contains(&"PeerLoaded".to_owned()));
    // Thread composers and channels have no hint.
    let mut t = Composer::for_thread("D1", "T1");
    let mut ctx = env.ctx();
    ctx.rid = "D1";
    let _ = t.on_open(&mut ctx);
    assert!(t.peer.is_none());
    let mut ch = Composer::new("R1");
    let _ = ch.on_open(&mut env.ctx());
    assert!(ch.peer.is_none());
}

/// The account's mention groups, as pending edits the workspace shows.
fn with_groups(env: &mut Env, groups: &[(&str, &[&str])]) {
    use rustket_core::mention_groups::{MentionGroupBlob, MentionGroupEdit};
    let mut b = MentionGroupBlob::default();
    for (i, (name, members)) in groups.iter().enumerate() {
        let e = MentionGroupEdit::Upsert {
            id: format!("g{i}"),
            name: (*name).into(),
            members: members.iter().map(|m| (*m).to_owned()).collect(),
        };
        b.edit(&MentionGroupBlob::default(), &e, 1, "dev").unwrap();
    }
    env.ws.set_mention_group_overlay(b);
}

#[test]
fn mention_groups_expand_when_sent() {
    let mut env = Env::new("t-groups");
    with_groups(&mut env, &[("backend", &["alice", "bob", "me"])]);
    let mut c = Composer::new("R1");
    type_text(&mut c, &mut env, "ping @backend");
    let a = c.update(Message::Send, &mut env.ctx());
    assert!(a.effects.is_empty());
    let p = env.ws.pending_messages("R1");
    assert_eq!(
        p[0].message.msg, "ping @backend (@alice @bob)",
        "me left out"
    );
    assert!(c.text().is_empty());
    // Thread replies too.
    let mut t = Composer::for_thread("R1", "T1");
    type_text(&mut t, &mut env, "@backend look");
    let _ = t.update(Message::Send, &mut env.ctx());
    let p = env.ws.pending_messages("R1");
    assert!(p.iter().any(|i| i.message.tmid.as_deref() == Some("T1")
        && i.message.msg == "@backend (@alice @bob) look"));
    // Slash commands keep their parameters as typed.
    store_commands(
        &env.sid,
        vec![SlashCommand {
            command: "leave".into(),
            ..Default::default()
        }],
    );
    env.ws.apply_setting(
        rustket_core::Change::Updated,
        rocketchat::models::Setting {
            id: "Message_AllowUnrecognizedSlashCommand".into(),
            value: serde_json::json!(true),
            ..Default::default()
        },
    );
    type_text(&mut c, &mut env, "/invite @backend");
    let _ = c.update(Message::Send, &mut env.ctx());
    let p = env.ws.pending_messages("R1");
    assert!(p.iter().any(|i| i.message.msg == "/invite @backend"));
}

#[test]
fn a_room_member_named_like_a_group_wins() {
    let mut env = Env::new("t-groups-user");
    with_groups(&mut env, &[("backend", &["alice"])]);
    assert!(expand_groups(&env.ws, &env.sid, "R1", "@backend").is_some());
    store_members(
        &env.sid,
        "R1",
        vec![rocketchat::models::UserSummary {
            id: "u1".into(),
            username: Some("backend".into()),
            ..Default::default()
        }],
    );
    assert!(expand_groups(&env.ws, &env.sid, "R1", "@backend").is_none());
}

#[test]
fn edits_expand_groups_once_and_say_nobody_is_notified() {
    let mut env = Env::new("t-groups-edit");
    with_groups(&mut env, &[("backend", &["alice", "bob"])]);
    env.ws.apply_message(msg("M1", "me", "hello"));
    let mut c = Composer::new("R1");
    let _ = c.update(Message::EditLast, &mut env.ctx());
    type_text(&mut c, &mut env, " @backend");
    let a = c.update(Message::Send, &mut env.ctx());
    assert!(!c.is_editing());
    assert!(matches!(
        a.effects.as_slice(),
        [Effect::Toast(BannerKind::Info, _, _)]
    ));
    // Saving an already expanded message unchanged is no edit at all.
    let mut env = Env::new("t-groups-edit-same");
    with_groups(&mut env, &[("backend", &["alice", "bob"])]);
    env.ws
        .apply_message(msg("M2", "me", "hi @backend (@alice @bob)"));
    let mut c = Composer::new("R1");
    let _ = c.update(Message::EditLast, &mut env.ctx());
    assert!(c.is_editing());
    let a = c.update(Message::Send, &mut env.ctx());
    assert!(a.effects.is_empty() && !c.is_editing());
    assert!(names(&outputs(a.task)).iter().all(|n| n != "Updated"));
    // Nor is one whose group was never expanded (sent from another client).
    let mut env = Env::new("t-groups-edit-plain");
    with_groups(&mut env, &[("backend", &["alice", "bob"])]);
    env.ws.apply_message(msg("M3", "me", "hi @backend"));
    let mut c = Composer::new("R1");
    let _ = c.update(Message::EditLast, &mut env.ctx());
    assert!(c.is_editing());
    let a = c.update(Message::Send, &mut env.ctx());
    assert!(a.effects.is_empty() && !c.is_editing());
    assert!(names(&outputs(a.task)).iter().all(|n| n != "Updated"));
}

#[test]
fn broad_confirmation_sees_and_keeps_the_expanded_text() {
    let mut env = broad_env("t-groups-broad", Some(20), &["mention-all"]);
    with_groups(&mut env, &[("backend", &["alice", "bob"])]);
    let mut c = Composer::new("R1");
    type_text(&mut c, &mut env, "@all @backend ship it");
    let _ = c.update(Message::Send, &mut env.ctx());
    assert!(c.confirm_broad.is_some());
    assert_eq!(c.text(), "@all @backend ship it", "the editor keeps @group");
    let _ = c.update(Message::BroadConfirmed, &mut env.ctx());
    let p = env.ws.pending_messages("R1");
    assert_eq!(p.len(), 1);
    assert_eq!(p[0].message.msg, "@all @backend (@alice @bob) ship it");
}

#[test]
fn upload_captions_expand_groups_before_the_broad_check() {
    let mut env = broad_env("t-groups-upload", Some(20), &["mention-all"]);
    with_groups(&mut env, &[("backend", &["alice", "bob"])]);
    assert_eq!(
        upload::caption(&env.ws, &env.sid, "R1", " @all @backend "),
        "@all @backend (@alice @bob)"
    );
    assert_eq!(
        upload::caption(&env.ws, &env.sid, "R1", " minutes "),
        "minutes"
    );
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("notes.txt");
    std::fs::write(&path, b"hello").unwrap();
    let mut c = Composer::new("R1");
    let a = c.update(Message::FilesDropped(vec![path]), &mut env.ctx());
    for m in outputs(a.task) {
        let _ = c.update(m, &mut env.ctx());
    }
    let _ = c.update(
        Message::Upload(upload::Message::Text("@all @backend".into())),
        &mut env.ctx(),
    );
    let a = c.update(Message::Upload(upload::Message::Send), &mut env.ctx());
    assert!(a.effects.is_empty());
    let d = c.uploads.dialog.as_ref().unwrap();
    assert!(d.confirm.as_ref().is_some_and(|c| c.all));
    assert_eq!(d.message, "@all @backend", "the dialog keeps @group");
    // Confirmed: sent again, expanded again to the same caption (refused
    // here: not connected, and the dialog keeps the typed text).
    let a = c.update(
        Message::Upload(upload::Message::BroadConfirmed),
        &mut env.ctx(),
    );
    assert_eq!(a.effects.len(), 1, "the not-connected toast");
    let d = c.uploads.dialog.as_ref().unwrap();
    assert!(d.confirm.is_none());
    assert_eq!(d.message, "@all @backend");
}

#[test]
fn the_people_popup_offers_groups_and_their_settings() {
    let mut env = Env::new("t-groups-popup");
    let mut c = Composer::new("R1");
    type_text(&mut c, &mut env, "@");
    assert!(matches!(
        c.popup_items().last(),
        Some(Item::ManageGroups { create: true })
    ));
    let _ = c.update(Message::AcClose, &mut env.ctx());
    c.content = text_editor::Content::new();
    with_groups(&mut env, &[("backend", &["alice", "bob"])]);
    type_text(&mut c, &mut env, "hi @ba");
    let items = c.popup_items().to_vec();
    assert!(matches!(&items[0], Item::Group { name, .. } if name == "backend"));
    assert!(matches!(
        items.last(),
        Some(Item::ManageGroups { create: false })
    ));
    let _ = c.update(Message::AcConfirm, &mut env.ctx());
    assert_eq!(c.text(), "hi @backend ");
    type_text(&mut c, &mut env, "@b");
    let last = c.popup_items().len() - 1;
    let a = c.update(Message::AcSelect(last), &mut env.ctx());
    assert!(matches!(
        a.effects.as_slice(),
        [Effect::ManageMentionGroups]
    ));
    assert!(c.popup.is_none());
    assert_eq!(c.text(), "hi @backend @b", "nothing inserted");
}
