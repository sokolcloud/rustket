#![allow(clippy::unwrap_used)]

use std::time::{Duration, Instant};

use rocketchat::models::{
    ActionButton, AppStatus, AppsStreamEvent, Banner, ButtonContext, Message,
};
use rocketchat::uikit::{InteractionResponse, ServerInteraction, UserInteraction};
use serde_json::{Value, json};

use super::*;
use crate::test_util::{fixture, msg};

fn si(name: &str, trigger: &str) -> ServerInteraction {
    let mut v = fixture(&format!("uikit/{name}.json"));
    v["triggerId"] = json!(trigger);
    ServerInteraction::from_value(v)
}

fn resp(name: &str, trigger: &str) -> InteractionResult {
    InteractionResult::Response(InteractionResponse::Interaction(si(name, trigger)))
}

fn cat_message() -> Message {
    let mut m = msg("1794088390781", "r1", 1000, "rocket.cat");
    m.blocks = Some(json!([]));
    m
}

/// A store with the modal `view-1` of `app1` open (opened by a slash command).
fn with_modal(now: Instant) -> UiKitStore {
    let mut s = UiKitStore::new();
    let t = s.slash_command_trigger(Some("app1"), "r1", None, now);
    let ch = s.apply(si("synthetic_modal_open", &t), now);
    s.finish_command(&t, now);
    assert_eq!(ch, vec![UiKitChange::ModalOpened("view-1".into())]);
    s
}

fn submit(s: &mut UiKitStore, now: Instant) -> String {
    let (app, i) = s.view_submit("view-1", now).unwrap();
    assert_eq!(app, "app1");
    i.trigger_id().to_owned()
}

#[test]
fn close_decision_table() {
    use HttpOutcome::*;
    let k = OriginKind::ViewSubmit;
    for keep in [
        "errors",
        "modal.update",
        "contextual_bar.update",
        "modal.open",
    ] {
        for o in [Timeout, Failed, Empty, Body] {
            assert!(!close_decision(k, Some(keep), o), "{keep} {o:?}");
        }
    }
    assert!(close_decision(k, Some("modal.close"), Empty));
    assert!(close_decision(k, Some("contextual_bar.open"), Failed));
    assert!(close_decision(k, None, Body));
    assert!(close_decision(k, None, Timeout));
    assert!(!close_decision(k, None, Empty));
    assert!(!close_decision(k, None, Failed));
    assert!(!close_decision(OriginKind::ViewClosed, None, Body));
    assert!(!close_decision(OriginKind::ViewBlock, None, Body));
}

#[test]
fn modal_open_initial_state_and_submit_payload() {
    let now = Instant::now();
    let mut s = with_modal(now);
    let m = s.modal().unwrap();
    assert_eq!(m.app_id, "app1");
    assert_eq!(
        m.state.to_state_json(),
        json!({"b_q": {"question": "Lunch?"}})
    );
    s.view_state_mut("view-1")
        .unwrap()
        .toggle("opts", Some("b_opts"), "anon");
    let (_, i) = s.view_submit("view-1", now).unwrap();
    let v = serde_json::to_value(&i).unwrap();
    assert_eq!(
        v["payload"]["view"]["state"],
        json!({"b_q": {"question": "Lunch?"}, "b_opts": {"opts": ["anon"]}})
    );
    assert_eq!(v["payload"]["view"]["customField"]["kept"], true);
    // Double submit is blocked while the first is in flight.
    assert!(s.view_submit("view-1", now).is_none());
    assert_eq!(s.busy(), 1);
}

#[test]
fn submit_answered_with_errors_keeps_the_modal() {
    let now = Instant::now();
    let mut s = with_modal(now);
    let t = submit(&mut s, now);
    let ch = s.finish(&t, resp("synthetic_errors_object", &t), now);
    assert_eq!(ch, vec![UiKitChange::ErrorsSet("view-1".into())]);
    let m = s.modal().unwrap();
    assert_eq!(m.errors["question"], "Question is required");
    assert!(!m.submitting);
    assert_eq!(s.busy(), 0);
    // A second submit is possible again; an update clears the errors and
    // keeps the values, adding the initial values of new fields.
    s.view_state_mut("view-1")
        .unwrap()
        .set("question", Some("b_q"), json!("Dinner?"));
    let t2 = submit(&mut s, now);
    let ch = s.finish(&t2, resp("synthetic_modal_update", &t2), now);
    assert_eq!(ch, vec![UiKitChange::ModalUpdated("view-1".into())]);
    let m = s.modal().unwrap();
    assert!(m.errors.is_empty());
    assert_eq!(m.view.view.title.text, "Create poll (2)");
    assert_eq!(m.state.get("question"), Some(&json!("Dinner?")));
    assert_eq!(m.state.get("choice3"), Some(&json!("Pizza")));
}

#[test]
fn submit_closes_on_ack_but_not_on_empty_or_error() {
    let now = Instant::now();
    let mut s = with_modal(now);
    let t = submit(&mut s, now);
    let ch = s.finish(
        &t,
        InteractionResult::Response(InteractionResponse::Empty),
        now,
    );
    assert!(ch.is_empty());
    assert!(s.modal().is_some_and(|m| !m.submitting));

    let t = submit(&mut s, now);
    let ch = s.finish(&t, InteractionResult::Failed("500".into()), now);
    assert_eq!(
        ch,
        vec![UiKitChange::Toast(UiKitToast::AppError("500".into()))]
    );
    assert!(s.modal().is_some());

    let t = submit(&mut s, now);
    let ch = s.finish(
        &t,
        InteractionResult::Response(InteractionResponse::Ack),
        now,
    );
    assert_eq!(ch, vec![UiKitChange::ModalClosed("view-1".into())]);
    assert!(s.modal().is_none());
}

#[test]
fn submit_timeout_closes_and_toasts() {
    let now = Instant::now();
    let mut s = with_modal(now);
    let t = submit(&mut s, now);
    let ch = s.finish(&t, InteractionResult::Timeout, now + Duration::from_secs(5));
    assert_eq!(
        ch,
        vec![
            UiKitChange::Toast(UiKitToast::Timeout),
            UiKitChange::ModalClosed("view-1".into())
        ]
    );
}

#[test]
fn pushed_update_decides_before_the_http_answer() {
    // RN rule: a submit handled by pushing modal.update and answering
    // {success:true} keeps the updated modal open.
    let now = Instant::now();
    let mut s = with_modal(now);
    let t = submit(&mut s, now);
    let ch = s.apply(si("synthetic_modal_update", &t), now);
    assert_eq!(ch, vec![UiKitChange::ModalUpdated("view-1".into())]);
    let ch = s.finish(
        &t,
        InteractionResult::Response(InteractionResponse::Ack),
        now,
    );
    assert!(ch.is_empty());
    assert!(s.modal().is_some());
    // A pushed modal.close (or anything else) closes it even on an empty body.
    let t = submit(&mut s, now);
    let mut close = fixture("uikit/synthetic_modal_close_viewid.json");
    close["triggerId"] = json!(t);
    close["viewId"] = json!("view-1");
    close["appId"] = json!("app1");
    let ch = s.apply(ServerInteraction::from_value(close), now);
    assert_eq!(ch, vec![UiKitChange::ModalClosed("view-1".into())]);
    let ch = s.finish(
        &t,
        InteractionResult::Response(InteractionResponse::Empty),
        now,
    );
    assert!(ch.is_empty());
}

#[test]
fn modal_open_answer_reusing_the_submitted_id_is_not_closed() {
    let now = Instant::now();
    let mut s = with_modal(now);
    s.view_state_mut("view-1")
        .unwrap()
        .set("question", Some("b_q"), json!("changed"));
    let t = submit(&mut s, now);
    let ch = s.finish(&t, resp("synthetic_modal_open", &t), now);
    assert_eq!(ch, vec![UiKitChange::ModalOpened("view-1".into())]);
    let m = s.modal().unwrap();
    // A fresh modal with fresh state.
    assert_eq!(m.state.get("question"), Some(&json!("Lunch?")));
    assert!(!m.submitting);
}

#[test]
fn triggers_gate_server_interactions() {
    let now = Instant::now();
    let mut s = with_modal(now);
    // Unknown trigger (e.g. a push meant for another device): ignored.
    assert!(
        s.apply(si("synthetic_errors_object", "someone-else"), now)
            .is_empty()
    );
    assert!(s.modal().unwrap().errors.is_empty());
    // Two interactions for one trigger: only the first applies.
    let t = submit(&mut s, now);
    assert_eq!(s.apply(si("synthetic_errors_object", &t), now).len(), 1);
    assert!(s.apply(si("synthetic_errors_array", &t), now).is_empty());
    assert_eq!(s.modal().unwrap().errors.len(), 1);
    // Expired trigger: 5 s after the HTTP call settled.
    let (_, i) = s
        .view_block_action("view-1", None, "b_act", "add", Some(json!("add")), now)
        .unwrap();
    let t = i.trigger_id().to_owned();
    s.finish(
        &t,
        InteractionResult::Response(InteractionResponse::Ack),
        now,
    );
    assert!(
        s.apply(
            si("synthetic_modal_update", &t),
            now + Duration::from_secs(6)
        )
        .is_empty()
    );
    // Built-in slash command triggers never accept pushes.
    let b = s.slash_command_trigger(None, "r1", None, now);
    assert!(s.apply(si("synthetic_modal_open", &b), now).is_empty());
}

#[test]
fn updates_and_errors_for_views_that_are_not_open_are_ignored() {
    let now = Instant::now();
    let mut s = UiKitStore::new();
    let t = s.slash_command_trigger(Some("app1"), "r1", None, now);
    assert!(s.apply(si("synthetic_modal_update", &t), now).is_empty());
    let t = s.slash_command_trigger(Some("app1"), "r1", None, now);
    assert!(s.apply(si("synthetic_errors_object", &t), now).is_empty());
    assert!(s.modal().is_none());
}

#[test]
fn view_block_actions_and_close() {
    let now = Instant::now();
    let mut s = with_modal(now);
    let (app, i) = s
        .view_block_action("view-1", None, "b_act", "add", Some(json!("add")), now)
        .unwrap();
    assert_eq!(app, "app1");
    let v = serde_json::to_value(&i).unwrap();
    assert_eq!(v["container"], json!({"type": "view", "id": "view-1"}));
    assert!(v.get("rid").is_none());
    assert!(
        s.view_block_action("nope", None, "b", "a", None, now)
            .is_none()
    );

    let (app, i, ch) = s.view_closed("view-1", true, now).unwrap();
    assert_eq!(app, "app1");
    assert_eq!(ch, UiKitChange::ModalClosed("view-1".into()));
    assert!(s.modal().is_none());
    let v = serde_json::to_value(&i).unwrap();
    assert_eq!(v["payload"]["isCleared"], true);
    assert_eq!(v["payload"]["view"]["state"]["b_q"]["question"], "Lunch?");
}

#[test]
fn modal_open_replaces_and_modal_close_matches() {
    let now = Instant::now();
    let mut s = with_modal(now);
    let t = s.slash_command_trigger(Some("videoconf-core"), "r1", None, now);
    let ch = s.apply(si("synthetic_videoconf_info", &t), now);
    assert_eq!(ch, vec![UiKitChange::ModalOpened("callid123-info".into())]);
    assert_eq!(s.modal().unwrap().app_id, "videoconf-core");
    // modal.close with another view id: nothing.
    let t = s.slash_command_trigger(Some("x"), "r1", None, now);
    assert!(
        s.apply(si("synthetic_modal_close_viewid", &t), now)
            .is_empty()
    );
    // modal.close without a view id closes the app's own modal.
    let t = s.slash_command_trigger(Some("x"), "r1", None, now);
    let close = ServerInteraction::from_value(
        json!({"type": "modal.close", "triggerId": t, "appId": "videoconf-core"}),
    );
    assert_eq!(
        s.apply(close, now),
        vec![UiKitChange::ModalClosed("callid123-info".into())]
    );
}

#[test]
fn contextual_bars_open_in_the_origin_room() {
    let now = Instant::now();
    let mut s = UiKitStore::new();
    let button = ActionButton {
        app_id: "app1".into(),
        action_id: "room-polls".into(),
        context: ButtonContext::RoomAction,
        label_i18n: "room_polls".into(),
        ..Default::default()
    };
    let (_, i) = s.action_button(&button, Some("r1"), Some("t1"), Some("m1"), None, now);
    let v = serde_json::to_value(&i).unwrap();
    assert_eq!(
        v,
        json!({"type": "actionButton", "actionId": "room-polls", "rid": "r1",
               "payload": {"context": "roomAction"}, "triggerId": i.trigger_id()})
    );
    let t = i.trigger_id().to_owned();
    let ch = s.finish(&t, resp("synthetic_contextual_bar_open", &t), now);
    assert_eq!(
        ch,
        vec![UiKitChange::BarOpened {
            rid: Some("r1".into()),
            view_id: "bar-1".into()
        }]
    );
    assert_eq!(s.bar_for_room("r1").unwrap().id(), "bar-1");
    // Bar interactions carry the room.
    let (_, i) = s
        .view_block_action("bar-1", None, "tabs", "t1", None, now)
        .unwrap();
    let v = serde_json::to_value(&i).unwrap();
    assert_eq!(v["rid"], "r1");
    let t = i.trigger_id().to_owned();
    let ch = s.apply(si("synthetic_contextual_bar_close", &t), now);
    assert_eq!(
        ch,
        vec![UiKitChange::BarClosed {
            rid: Some("r1".into()),
            view_id: "bar-1".into()
        }]
    );
    // User-menu buttons have no room: the bar opens without one.
    let menu = ActionButton {
        context: ButtonContext::UserDropdownAction,
        ..button
    };
    let (_, i) = s.action_button(&menu, Some("r1"), None, None, None, now);
    assert!(serde_json::to_value(&i).unwrap().get("rid").is_none());
    let t = i.trigger_id().to_owned();
    s.apply(si("synthetic_contextual_bar_open", &t), now);
    assert_eq!(s.bar("bar-1").unwrap().rid, None);
    // Closing the room view drops its panels silently.
    assert!(s.close_room_bars("r1").is_empty());
}

#[test]
fn composer_buttons_send_the_text() {
    let now = Instant::now();
    let mut s = UiKitStore::new();
    let b = ActionButton {
        app_id: "app1".into(),
        action_id: "create-poll".into(),
        context: ButtonContext::MessageBoxAction,
        ..Default::default()
    };
    let (_, i) = s.action_button(&b, Some("r1"), Some("t1"), Some("m1"), None, now);
    let v = serde_json::to_value(&i).unwrap();
    assert_eq!(
        v["payload"],
        json!({"context": "messageBoxAction", "message": ""})
    );
    assert_eq!(v["tmid"], "t1");
    assert!(v.get("mid").is_none());
}

#[test]
fn message_block_actions_follow_the_trust_rule() {
    let now = Instant::now();
    let mut s = UiKitStore::new();
    let m = cat_message();
    assert!(
        s.message_block_action(
            &m,
            false,
            false,
            Some("mention-core"),
            "b",
            "dismiss",
            None,
            now
        )
        .is_none()
    );
    let (app, i) = s
        .message_block_action(
            &m,
            true,
            false,
            Some("mention-core"),
            "b",
            "dismiss",
            Some(json!("[]")),
            now,
        )
        .unwrap();
    assert_eq!(app, "mention-core");
    let UserInteraction::BlockAction {
        mid,
        rid,
        container,
        ..
    } = &i
    else {
        panic!()
    };
    assert_eq!(mid.as_deref(), Some("1794088390781"));
    assert_eq!(rid.as_deref(), Some("r1"));
    assert_eq!(
        *container,
        rocketchat::uikit::Container::Message("1794088390781".into())
    );
    // No resolvable app id.
    assert!(
        s.message_block_action(&m, true, false, None, "b", "a", None, now)
            .is_none()
    );
    assert_eq!(s.busy(), 1);
    // A message action whose answer is a modal opens it.
    let t = i.trigger_id().to_owned();
    s.finish(&t, resp("synthetic_modal_open", &t), now);
    assert!(s.modal().is_some());
    assert_eq!(s.busy(), 0);
}

#[test]
fn unknown_interactions_toast() {
    let now = Instant::now();
    let mut s = UiKitStore::new();
    let t = s.slash_command_trigger(Some("app1"), "r1", None, now);
    assert_eq!(
        s.apply(si("synthetic_unknown_interaction", &t), now),
        vec![UiKitChange::Toast(UiKitToast::Unsupported(
            "toast.show".into()
        ))]
    );
}

#[test]
fn banners_from_rest_and_interactions() {
    let now = Instant::now();
    let mut s = UiKitStore::new();
    let banners: Vec<Banner> = rocketchat::models::lenient::vec_from_value(
        fixture("uikit/synthetic_banners.json")["banners"].clone(),
    );
    s.set_banners(banners);
    // The inactive one is skipped; the modal announcement is kept apart.
    assert_eq!(s.banners().len(), 2);
    let b = s.current_banner().unwrap();
    assert_eq!(b.app_id, CLOUD_ANNOUNCEMENTS_APP);
    assert_eq!(b.view_id(), "ban1-view");
    let title = match b.view.view.title.as_ref().unwrap() {
        rocketchat::uikit::StrOrText::Text(t) => t.clone(),
        _ => panic!(),
    };
    assert_eq!(s.translations().text(&title, None, "en"), "Cloud news");
    let modal = s.banners().iter().find(|b| b.modal.is_some()).unwrap();
    assert_eq!(modal.view_id(), "announcement-1");

    // banner.open / banner.close interactions.
    let t = s.slash_command_trigger(Some("banner-core"), "r1", None, now);
    assert_eq!(
        s.apply(si("synthetic_banner_open", &t), now),
        vec![UiKitChange::BannerOpened("banner-1".into())]
    );
    let t = s.slash_command_trigger(Some("banner-core"), "r1", None, now);
    assert_eq!(
        s.apply(si("synthetic_banner_close", &t), now),
        vec![UiKitChange::BannerClosed("banner-1".into())]
    );

    // Closing a banner locally builds its viewClosed.
    let (app, i, ch) = s.banner_closed("ban1-view", now).unwrap();
    assert_eq!(app, CLOUD_ANNOUNCEMENTS_APP);
    assert_eq!(ch, UiKitChange::BannerClosed("ban1-view".into()));
    let v = serde_json::to_value(&i).unwrap();
    assert_eq!(v["payload"]["view"]["id"], "ban1-view");
    assert_eq!(v["payload"]["isCleared"], true);
    assert!(s.current_banner().is_none());
}

#[test]
fn apps_stream_followups() {
    let mut s = UiKitStore::new();
    s.set_buttons(vec![ActionButton {
        app_id: "app1".into(),
        ..Default::default()
    }]);
    assert!(
        s.apply_apps_event(&AppsStreamEvent::ActionsChanged)
            .action_buttons
    );
    assert!(
        s.apply_apps_event(&AppsStreamEvent::CommandAdded("poll".into()))
            .commands
    );
    assert!(
        s.apply_apps_event(&AppsStreamEvent::Added("x".into()))
            .languages
    );
    let f = s.apply_apps_event(&AppsStreamEvent::StatusUpdate {
        app_id: "app1".into(),
        status: AppStatus::ManuallyDisabled,
    });
    assert!(f.admin && !f.action_buttons);
    assert!(s.buttons().is_empty());
    assert_eq!(
        s.apply_apps_event(&AppsStreamEvent::Unknown {
            key: "x".into(),
            args: Value::Null
        }),
        AppsFollowup::default()
    );
}

#[test]
fn interaction_results_from_rest_errors() {
    assert_eq!(
        InteractionResult::from(Err(rocketchat::Error::Timeout("app interaction"))),
        InteractionResult::Timeout
    );
    assert!(matches!(
        InteractionResult::from(Err(rocketchat::Error::Status {
            status: 413,
            body: String::new()
        })),
        InteractionResult::Failed(_)
    ));
    assert_eq!(
        InteractionResult::from(Ok(InteractionResponse::Ack)),
        InteractionResult::Response(InteractionResponse::Ack)
    );
}

// --- the per-message ephemeral flag (§3.2) ----------------------------------------

mod ephemeral {
    use super::*;
    use crate::workspace::Workspace;

    fn mention_message() -> Message {
        let v = fixture("uikit/ephemeral_mention.json");
        serde_json::from_value(v["fields"]["args"][0].clone()).unwrap()
    }

    fn shown(ws: &Workspace, rid: &str, id: &str) -> Message {
        ws.timeline(rid).unwrap().get(id).unwrap().message.clone()
    }

    #[test]
    fn ephemeral_flag_lifecycle() {
        let mut ws = Workspace::new();
        let m = mention_message();
        let rid = m.rid.clone();
        // Room not open: nothing stored, nothing flagged.
        ws.apply_ephemeral_message(m.clone());
        assert!(!ws.is_ephemeral(&rid, &m.id));

        ws.open_room(&rid);
        ws.apply_ephemeral_message(m.clone());
        assert!(ws.is_ephemeral(&rid, &m.id));
        let s = shown(&ws, &rid, &m.id);
        assert!(actions_allowed(
            &s,
            ws.is_ephemeral(&rid, &m.id),
            false,
            Some("mention-core")
        ));

        // The dismiss replacement: same `_id`, new ts, no blocks; still ephemeral.
        let mut repl = m.clone();
        repl.blocks = None;
        repl.msg = "You mentioned @bob, but they're not in this room.".into();
        repl.ts = rocketchat::RcDate::from_millis(1_794_088_399_000);
        repl.updated_at = repl.ts;
        ws.apply_ephemeral_message(repl);
        assert!(ws.is_ephemeral(&rid, &m.id));
        assert!(shown(&ws, &rid, &m.id).blocks.is_none());

        // A persisted message reusing the id (client-chosen) drops the flag.
        let mut forged = m.clone();
        forged.u.id = "alice".into();
        forged.updated_at = rocketchat::RcDate::from_millis(1_794_088_400_000);
        ws.apply_message(forged);
        assert!(!ws.is_ephemeral(&rid, &m.id));
        let s = shown(&ws, &rid, &m.id);
        assert!(!actions_allowed(
            &s,
            ws.is_ephemeral(&rid, &m.id),
            false,
            Some("mention-core")
        ));
    }

    #[test]
    fn history_deletion_and_close_drop_the_flag() {
        let mut ws = Workspace::new();
        let m = mention_message();
        let rid = m.rid.clone();
        ws.open_room(&rid);
        ws.apply_ephemeral_message(m.clone());
        assert!(ws.is_ephemeral(&rid, &m.id));
        // A history page containing a persisted copy.
        let cursor = ws.begin_load_older(&rid).unwrap();
        let mut persisted = m.clone();
        persisted.u.id = "mallory".into();
        ws.finish_load_older(&rid, cursor, Ok(vec![persisted]));
        assert!(!ws.is_ephemeral(&rid, &m.id));

        ws.apply_ephemeral_message(mention_message());
        assert!(ws.is_ephemeral(&rid, &m.id));
        ws.apply_message_deleted(&rid, &m.id);
        assert!(!ws.is_ephemeral(&rid, &m.id));

        ws.apply_ephemeral_message(mention_message());
        assert!(ws.is_ephemeral(&rid, &m.id));
        ws.close_room(&rid);
        assert!(!ws.is_ephemeral(&rid, &m.id));
    }

    #[test]
    fn forged_message_fields_never_set_the_flag() {
        let mut ws = Workspace::new();
        let mut m = mention_message();
        m.id = "forged1".into();
        m.t = Some("videoconf".into());
        let rid = m.rid.clone();
        ws.open_room(&rid);
        ws.apply_message(m.clone());
        assert!(!ws.is_ephemeral(&rid, "forged1"));
        assert!(!actions_allowed(&m, false, false, Some("mention-core")));
    }

    #[test]
    fn uikit_store_lives_in_the_workspace() {
        let mut ws = Workspace::new();
        let now = Instant::now();
        let t = ws
            .uikit_mut()
            .slash_command_trigger(Some("app1"), "r1", None, now);
        ws.uikit_mut().apply(si("synthetic_modal_open", &t), now);
        assert!(ws.uikit().modal().is_some());
    }
}
