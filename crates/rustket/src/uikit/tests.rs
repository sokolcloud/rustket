#![allow(clippy::unwrap_used)]

use std::time::Instant;

use rocketchat::RcDate;
use rocketchat::models::{ActionButton, ButtonContext, Message as RcMessage, UserRef};
use rocketchat::uikit::{InteractionResponse, ServerInteraction, decode_blocks};
use rustket_core::Workspace;
use rustket_core::uikit::{InteractionResult, Origin, OriginKind};
use rustket_ui::widget::MenuEntry;
use serde_json::{Value, json};

use super::controller::{self, Ctx, Effect, Out};
use super::{
    ActionRequest, Local, Msg, SessionHook, SurfaceKey, Typed, UiKitMsg, buttons, commands,
};
use crate::error::Failure;

const MENTION: &str =
    include_str!("../../../rocketchat/tests/fixtures/uikit/ephemeral_mention.json");
const ALICE: &str = include_str!("../../../rocketchat/tests/fixtures/uikit/alice_blocks.json");
const MODAL: &str =
    include_str!("../../../rocketchat/tests/fixtures/uikit/synthetic_modal_open.json");
const BAR: &str =
    include_str!("../../../rocketchat/tests/fixtures/uikit/synthetic_contextual_bar_open.json");
const ERRORS: &str =
    include_str!("../../../rocketchat/tests/fixtures/uikit/synthetic_errors_object.json");

struct Fx {
    ws: Workspace,
    local: Local,
}

impl Fx {
    fn new() -> Self {
        let mut ws = Workspace::new();
        ws.open_room("r1");
        Self {
            ws,
            local: Local::default(),
        }
    }

    fn ctx(&mut self) -> Ctx<'_> {
        Ctx {
            ws: &mut self.ws,
            local: &mut self.local,
            rest: None,
            active_rid: Some("r1"),
        }
    }

    fn ui(&mut self, msg: UiKitMsg) -> Out {
        controller::update(&mut self.ctx(), Msg::Ui { rid: None, msg })
    }

    /// Opens a view as the HTTP answer of a fresh trigger.
    fn answer(&mut self, origin: Origin, mut si: Value) -> Out {
        let t = self
            .ws
            .uikit_mut()
            .begin(Some("app1"), origin, Instant::now());
        si["triggerId"] = Value::String(t.clone());
        controller::update(
            &mut self.ctx(),
            Msg::Finished {
                trigger: t,
                result: InteractionResult::Response(InteractionResponse::Interaction(
                    ServerInteraction::from_value(si),
                )),
            },
        )
    }

    fn triggers(&self) -> usize {
        self.ws.uikit().triggers().len()
    }
}

fn message(id: &str, user: &str, blocks: Value) -> RcMessage {
    RcMessage {
        id: id.into(),
        rid: "r1".into(),
        msg: "fallback".into(),
        ts: RcDate::from_millis(1_000),
        u: UserRef {
            id: user.into(),
            username: Some(user.into()),
            name: None,
        },
        blocks: Some(blocks),
        ..Default::default()
    }
}

fn mention_message() -> RcMessage {
    let v: Value = serde_json::from_str(MENTION).unwrap();
    let mut m: RcMessage = serde_json::from_value(v["fields"]["args"][0].clone()).unwrap();
    m.rid = "r1".into();
    m
}

fn req(
    surface: SurfaceKey,
    app: Option<&str>,
    block: &str,
    action: &str,
    value: Option<Value>,
) -> ActionRequest {
    ActionRequest {
        surface,
        app_id: app.map(str::to_owned),
        block_id: block.into(),
        action_id: action.into(),
        value,
        url: None,
        in_video_conf: false,
    }
}

fn msg_surface(mid: &str) -> SurfaceKey {
    SurfaceKey::Message {
        rid: "r1".into(),
        mid: mid.into(),
        tmid: None,
    }
}

fn json_of(i: &rocketchat::uikit::UserInteraction) -> Value {
    let mut v = serde_json::to_value(i).unwrap();
    v["triggerId"] = json!("T");
    v
}

#[test]
fn core_app_actions_need_a_server_ephemeral_from_rocket_cat() {
    let mut fx = Fx::new();
    let m = mention_message();
    let mid = m.id.clone();
    // Persisted copy (forged by a member): `mention-core` stays disabled.
    fx.ws.apply_message(m.clone());
    let r = req(
        msg_surface(&mid),
        Some("mention-core"),
        "vSxru3XTbnFh8HJrc",
        "dismiss",
        Some(json!("[]")),
    );
    assert!(controller::interaction(&mut fx.ws, &r, Instant::now()).is_none());
    // The same message as a server ephemeral: allowed, payload like the web.
    fx.ws.apply_ephemeral_message(m);
    let (app, i) = controller::interaction(&mut fx.ws, &r, Instant::now()).unwrap();
    assert_eq!(app, "mention-core");
    assert_eq!(
        json_of(&i),
        json!({"type": "blockAction", "actionId": "dismiss",
               "payload": {"blockId": "vSxru3XTbnFh8HJrc", "value": "[]"},
               "container": {"type": "message", "id": mid},
               "mid": mid, "rid": "r1", "triggerId": "T"})
    );
}

#[test]
fn message_change_emits_and_tracks_loading() {
    let mut fx = Fx::new();
    let blocks: Value = serde_json::from_str(ALICE).unwrap();
    fx.ws.apply_message(message("m1", "alice", blocks));
    let s = msg_surface("m1");
    fx.ws.uikit_mut().scratch_mut().open = Some(s.element("sel"));
    let out = fx.ui(UiKitMsg::Change {
        req: req(
            s.clone(),
            Some("fake-app-id"),
            "blk_actions",
            "sel",
            Some(json!("a")),
        ),
        emit: true,
        confirm: None,
    });
    assert!(out.effects.is_empty());
    let sc = fx.ws.uikit().scratch();
    assert!(sc.open.is_none(), "choosing closes the select");
    assert_eq!(sc.values.get(&s.element("sel")), Some(&json!("a")));
    assert!(sc.is_loading(&s.element("sel")));
    assert_eq!(fx.ws.uikit().busy(), 1);
    // The answer settles the element.
    let trigger = sc.loading[&s.element("sel")].clone();
    controller::update(
        &mut fx.ctx(),
        Msg::Finished {
            trigger,
            result: InteractionResult::Failed("500".into()),
        },
    );
    assert!(!fx.ws.uikit().scratch().is_loading(&s.element("sel")));
    assert_eq!(fx.ws.uikit().busy(), 0);
}

#[test]
fn http_errors_and_timeouts_toast() {
    let mut fx = Fx::new();
    let t = fx.ws.uikit_mut().begin(
        Some("app1"),
        Origin::new(OriginKind::MessageBlock),
        Instant::now(),
    );
    let out = controller::update(
        &mut fx.ctx(),
        Msg::Finished {
            trigger: t,
            result: InteractionResult::Failed("Cannot read properties".into()),
        },
    );
    assert!(matches!(out.effects.as_slice(), [Effect::Toast(..)]));
    let t = fx.ws.uikit_mut().begin(
        Some("app1"),
        Origin::new(OriginKind::MessageBlock),
        Instant::now(),
    );
    let out = controller::update(
        &mut fx.ctx(),
        Msg::Finished {
            trigger: t,
            result: InteractionResult::Unauthorized,
        },
    );
    assert_eq!(out.effects, vec![Effect::Relogin]);
}

#[test]
fn url_buttons_open_and_emit() {
    let mut fx = Fx::new();
    let blocks: Value = serde_json::from_str(ALICE).unwrap();
    fx.ws.apply_message(message("m1", "alice", blocks));
    let mut r = req(
        msg_surface("m1"),
        Some("fake-app-id"),
        "blk_actions",
        "btn_url",
        None,
    );
    r.url = Some("https://rocket.chat".into());
    let out = fx.ui(UiKitMsg::Change {
        req: r,
        emit: true,
        confirm: None,
    });
    assert_eq!(
        out.effects,
        vec![Effect::OpenUrl("https://rocket.chat".into())]
    );
    assert_eq!(fx.triggers(), 1);
}

#[test]
fn confirmation_comes_first() {
    let mut fx = Fx::new();
    let blocks: Value = serde_json::from_str(ALICE).unwrap();
    let decoded = decode_blocks(&blocks);
    let confirm = decoded
        .iter()
        .flat_map(|b| b.inherited_elements())
        .find_map(|e| e.actionable().and_then(|a| a.confirm.clone()))
        .unwrap();
    fx.ws.apply_message(message("m1", "alice", blocks));
    let r = req(
        msg_surface("m1"),
        Some("fake-app-id"),
        "blk_actions",
        "btn_primary",
        Some(json!("v1")),
    );
    fx.ui(UiKitMsg::Change {
        req: r,
        emit: true,
        confirm: Some(Box::new(confirm)),
    });
    assert!(fx.ws.uikit().scratch().confirm.is_some());
    assert_eq!(fx.triggers(), 0, "nothing sent before the answer");
    fx.ui(UiKitMsg::Confirmed(false));
    assert!(fx.ws.uikit().scratch().confirm.is_none());
    assert_eq!(fx.triggers(), 0);
}

#[test]
fn modal_values_toggles_text_and_submit() {
    let mut fx = Fx::new();
    let out = fx.answer(
        Origin::new(OriginKind::ActionButton(ButtonContext::RoomAction)),
        serde_json::from_str(MODAL).unwrap(),
    );
    let surface = SurfaceKey::Modal("view-1".into());
    // The first single-line input gets the focus.
    assert_eq!(
        out.effects,
        vec![Effect::Focus(surface.element("question"))]
    );

    // Checkbox in an input block: stored as the whole array, nothing sent.
    fx.ui(UiKitMsg::Toggle {
        req: req(
            surface.clone(),
            None,
            "b_opts",
            "opts",
            Some(json!(["anon"])),
        ),
        option: "anon".into(),
        emit: false,
    });
    // A toggle in an actions block emits only the toggled value.
    let before = fx.triggers();
    fx.ui(UiKitMsg::Toggle {
        req: req(
            surface.clone(),
            None,
            "b_act",
            "chk",
            Some(json!(["x", "y"])),
        ),
        option: "y".into(),
        emit: true,
    });
    assert_eq!(fx.triggers(), before + 1);

    // Typing stores the text; `on_character_entered` is debounced.
    let mut t = req(
        surface.clone(),
        None,
        "b_q",
        "question",
        Some(json!("Pizza?")),
    );
    fx.ui(UiKitMsg::Text {
        req: t.clone(),
        dispatch: false,
    });
    let before = fx.triggers();
    t.value = Some(json!("Pizza??"));
    fx.ui(UiKitMsg::Text {
        req: t.clone(),
        dispatch: true,
    });
    assert_eq!(fx.triggers(), before, "debounced");
    let key = surface.element("question");
    let stamp = fx.ws.uikit().scratch().current_stamp(&key).unwrap();
    controller::update(
        &mut fx.ctx(),
        Msg::Debounced {
            req: t.clone(),
            stamp: stamp - 1,
        },
    );
    assert_eq!(fx.triggers(), before, "a stale timer sends nothing");
    controller::update(&mut fx.ctx(), Msg::Debounced { req: t, stamp });
    assert_eq!(fx.triggers(), before + 1);

    // Dates are stored once valid.
    let d = req(surface.clone(), None, "b_d", "date", None);
    fx.ui(UiKitMsg::Typing {
        req: d.clone(),
        text: "2026-13-01".into(),
        emit: false,
        kind: Typed::Date,
    });
    fx.ui(UiKitMsg::Typing {
        req: d,
        text: "2026-11-30".into(),
        emit: false,
        kind: Typed::Date,
    });

    // A partial date or a cleared field holds no value (not the last one).
    let d = req(surface.clone(), None, "b_d", "date", None);
    let date = |fx: &Fx| fx.ws.uikit().modal().unwrap().state.get("date").cloned();
    fx.ui(UiKitMsg::Typing {
        req: d.clone(),
        text: "2026-11-3".into(),
        emit: false,
        kind: Typed::Date,
    });
    assert_eq!(date(&fx), Some(json!("")));
    fx.ui(UiKitMsg::Typing {
        req: d.clone(),
        text: String::new(),
        emit: false,
        kind: Typed::Date,
    });
    assert_eq!(date(&fx), Some(json!("")));
    fx.ui(UiKitMsg::Typing {
        req: d.clone(),
        text: "2026-11-30".into(),
        emit: false,
        kind: Typed::Date,
    });
    // An emitting time field sends a change once, not per invalid keystroke.
    let tm = req(surface.clone(), None, "b_t", "time", None);
    let before = fx.triggers();
    for text in ["1", "12", "12:3"] {
        fx.ui(UiKitMsg::Typing {
            req: tm.clone(),
            text: text.into(),
            emit: true,
            kind: Typed::Time,
        });
    }
    assert_eq!(fx.triggers(), before, "an empty field stays empty");
    assert!(
        fx.ws.uikit().modal().unwrap().state.get("time").is_none(),
        "nothing is stored for a field that never had a value"
    );
    fx.ui(UiKitMsg::Typing {
        req: tm.clone(),
        text: "12:30".into(),
        emit: true,
        kind: Typed::Time,
    });
    assert_eq!(fx.triggers(), before + 1);
    // Breaking a valid value clears it exactly once.
    for text in ["12:", "1"] {
        fx.ui(UiKitMsg::Typing {
            req: tm.clone(),
            text: text.into(),
            emit: true,
            kind: Typed::Time,
        });
    }
    assert_eq!(fx.triggers(), before + 2, "one clear for a valid value");
    fx.ui(UiKitMsg::Typing {
        req: tm,
        text: "12:30".into(),
        emit: true,
        kind: Typed::Time,
    });
    assert_eq!(fx.triggers(), before + 3);

    let state = fx.ws.uikit().modal().unwrap().state.to_state_json();
    assert_eq!(state["b_q"]["question"], json!("Pizza??"));
    assert_eq!(state["b_t"]["time"], json!("12:30"));
    assert_eq!(state["b_opts"]["opts"], json!(["anon"]));
    assert_eq!(state["b_act"]["chk"], json!(["x", "y"]));
    assert_eq!(state["b_d"]["date"], json!("2026-11-30"));

    // Submit (the store call the controller makes, to learn the trigger);
    // a second submit while it runs sends nothing.
    let (_, submit) = fx
        .ws
        .uikit_mut()
        .view_submit(
            rustket_core::uikit::ViewTarget::Modal,
            "view-1",
            Instant::now(),
        )
        .unwrap();
    let trigger = submit.trigger_id().to_owned();
    assert!(fx.ws.uikit().modal().unwrap().submitting);
    let before = fx.triggers();
    fx.ui(UiKitMsg::Submit(surface.clone()));
    assert_eq!(fx.triggers(), before, "second submit blocked");
    // The app answers the submit with validation errors: the view stays open,
    // can be submitted again, and the errored input gets the focus.
    let mut errors: Value = serde_json::from_str(ERRORS).unwrap();
    errors["triggerId"] = json!(trigger);
    let out = controller::update(
        &mut fx.ctx(),
        Msg::Finished {
            trigger,
            result: InteractionResult::Response(InteractionResponse::Interaction(
                ServerInteraction::from_value(errors),
            )),
        },
    );
    assert_eq!(
        out.effects,
        vec![Effect::Focus(surface.element("question"))]
    );
    let m = fx.ws.uikit().modal().expect("still open");
    assert!(!m.submitting);
    assert_eq!(
        m.errors.get("question").map(String::as_str),
        Some("Question is required")
    );
}

#[test]
fn closing_a_modal_sends_view_closed_and_forgets_its_widgets() {
    let mut fx = Fx::new();
    fx.answer(
        Origin::new(OriginKind::ActionButton(ButtonContext::RoomAction)),
        serde_json::from_str(MODAL).unwrap(),
    );
    let surface = SurfaceKey::Modal("view-1".into());
    fx.ws.uikit_mut().scratch_mut().open = Some(surface.element("x"));
    let before = fx.triggers();
    fx.ui(UiKitMsg::Close {
        surface,
        cleared: true,
    });
    assert!(fx.ws.uikit().modal().is_none());
    assert!(fx.ws.uikit().scratch().open.is_none());
    assert_eq!(fx.triggers(), before + 1, "viewClosed sent");
}

#[test]
fn contextual_bars_go_to_the_origin_or_active_room() {
    let mut fx = Fx::new();
    let out = fx.answer(
        Origin::new(OriginKind::ActionButton(ButtonContext::RoomAction)).rid(Some("r1")),
        serde_json::from_str(BAR).unwrap(),
    );
    assert_eq!(
        out.effects,
        vec![Effect::ShowBar {
            rid: "r1".into(),
            view_id: "bar-1".into()
        }]
    );
    // Without a room (user menu) it is placed in the active room.
    let mut bar: Value = serde_json::from_str(BAR).unwrap();
    bar["view"]["id"] = json!("bar-2");
    let out = fx.answer(
        Origin::new(OriginKind::ActionButton(ButtonContext::UserDropdownAction)),
        bar,
    );
    assert!(out.effects.contains(&Effect::ShowBar {
        rid: "r1".into(),
        view_id: "bar-2".into()
    }));
    assert_eq!(
        fx.ws.uikit().bar("bar-2").unwrap().rid.as_deref(),
        Some("r1")
    );
    // The X closes it locally and hides the panel.
    let out = fx.ui(UiKitMsg::Close {
        surface: SurfaceKey::Bar("bar-2".into()),
        cleared: true,
    });
    assert!(out.effects.contains(&Effect::HideBar {
        rid: "r1".into(),
        view_id: "bar-2".into()
    }));
}

#[test]
fn tabs_are_selected_locally_and_emitted() {
    let mut fx = Fx::new();
    fx.answer(
        Origin::new(OriginKind::ActionButton(ButtonContext::RoomAction)).rid(Some("r1")),
        serde_json::from_str(BAR).unwrap(),
    );
    let s = SurfaceKey::Bar("bar-1".into());
    let before = fx.triggers();
    fx.ui(UiKitMsg::Change {
        req: req(s.clone(), None, "tabs", "t2", None),
        emit: true,
        confirm: None,
    });
    assert_eq!(fx.triggers(), before + 1);
    assert_eq!(
        fx.ws
            .uikit()
            .scratch()
            .tabs
            .get(&format!("{}#tabstabs", s.prefix())),
        Some(&"t2".to_owned())
    );
}

#[test]
fn people_select_keeps_labels_of_chosen_suggestions() {
    let mut fx = Fx::new();
    fx.answer(
        Origin::new(OriginKind::ActionButton(ButtonContext::RoomAction)),
        serde_json::from_str(MODAL).unwrap(),
    );
    let s = SurfaceKey::Modal("view-1".into());
    let key = s.element("who");
    fx.ws
        .uikit_mut()
        .scratch_mut()
        .texts
        .insert(key.clone(), "bo".into());
    controller::update(
        &mut fx.ctx(),
        Msg::Suggestions {
            key: key.clone(),
            term: "bo".into(),
            result: Ok(vec![super::Suggestion {
                value: "bob".into(),
                label: "Bob Builder".into(),
                detail: Some("(bob)".into()),
                avatar: None,
            }]),
        },
    );
    assert_eq!(fx.ws.uikit().scratch().open.as_deref(), Some(key.as_str()));
    fx.ui(UiKitMsg::Change {
        req: req(s, None, "b_who", "who", Some(json!(["bob"]))),
        emit: false,
        confirm: None,
    });
    let sc = fx.ws.uikit().scratch();
    assert_eq!(
        sc.labels.get(&format!("{key}|bob")).map(String::as_str),
        Some("Bob Builder")
    );
    assert!(sc.open.is_none());
    let state = fx.ws.uikit().modal().unwrap().state.to_state_json();
    assert_eq!(state["b_who"]["who"], json!(["bob"]));
    // A failed autocomplete keeps the old list.
    let one = || {
        vec![super::Suggestion {
            value: "carol".into(),
            label: "Carol".into(),
            detail: None,
            avatar: None,
        }]
    };
    fx.ws
        .uikit_mut()
        .scratch_mut()
        .texts
        .insert(key.clone(), "ca".into());
    controller::update(
        &mut fx.ctx(),
        Msg::Suggestions {
            key: key.clone(),
            term: "ca".into(),
            result: Ok(one()),
        },
    );
    controller::update(
        &mut fx.ctx(),
        Msg::Suggestions {
            key: key.clone(),
            term: "ca".into(),
            result: Err(Failure::from(rocketchat::Error::Protocol("x".into()))),
        },
    );
    assert_eq!(fx.ws.uikit().scratch().suggestions.get(&key), Some(&one()));
}

#[test]
fn stale_suggestions_are_ignored() {
    let mut fx = Fx::new();
    fx.answer(
        Origin::new(OriginKind::ActionButton(ButtonContext::RoomAction)),
        serde_json::from_str(MODAL).unwrap(),
    );
    let key = SurfaceKey::Modal("view-1".into()).element("who");
    let list = |v: &str| {
        Ok(vec![super::Suggestion {
            value: v.into(),
            label: v.into(),
            detail: None,
            avatar: None,
        }])
    };
    fx.ws
        .uikit_mut()
        .scratch_mut()
        .texts
        .insert(key.clone(), "bobby".into());
    // The answer for an older query arrives after the field changed.
    controller::update(
        &mut fx.ctx(),
        Msg::Suggestions {
            key: key.clone(),
            term: "bob".into(),
            result: list("bob"),
        },
    );
    assert!(!fx.ws.uikit().scratch().suggestions.contains_key(&key));
    assert!(fx.ws.uikit().scratch().open.is_none());
    // Another popover stays open when the right answer arrives.
    fx.ws.uikit_mut().scratch_mut().open = Some("other".into());
    controller::update(
        &mut fx.ctx(),
        Msg::Suggestions {
            key: key.clone(),
            term: "bobby".into(),
            result: list("bobby"),
        },
    );
    let sc = fx.ws.uikit().scratch();
    assert!(sc.suggestions.contains_key(&key));
    assert_eq!(sc.open.as_deref(), Some("other"));
}

#[test]
fn debounce_timers_do_not_cancel_each_other() {
    let mut fx = Fx::new();
    fx.answer(
        Origin::new(OriginKind::ActionButton(ButtonContext::RoomAction)),
        serde_json::from_str(MODAL).unwrap(),
    );
    let surface = SurfaceKey::Modal("view-1".into());
    let a = req(surface.clone(), None, "b_q", "question", Some(json!("A")));
    let b = req(surface.clone(), None, "b_x", "other", Some(json!("B")));
    fx.ui(UiKitMsg::Text {
        req: a.clone(),
        dispatch: true,
    });
    let sa = fx
        .ws
        .uikit()
        .scratch()
        .current_stamp(&surface.element("question"))
        .unwrap();
    fx.ui(UiKitMsg::Text {
        req: b,
        dispatch: true,
    });
    // An `actions/changed` refresh starts its own timer as well.
    controller::session(
        &mut fx.ctx(),
        SessionHook::Apps(Box::new(
            rocketchat::models::AppsStreamEvent::ActionsChanged,
        )),
    );
    let before = fx.triggers();
    controller::update(&mut fx.ctx(), Msg::Debounced { req: a, stamp: sa });
    assert_eq!(fx.triggers(), before + 1, "A's value is still sent");
}

#[test]
fn closing_a_room_drops_its_bars() {
    let mut fx = Fx::new();
    fx.answer(
        Origin::new(OriginKind::ActionButton(ButtonContext::RoomAction)).rid(Some("r1")),
        serde_json::from_str(BAR).unwrap(),
    );
    let s = SurfaceKey::Bar("bar-1".into());
    fx.ws
        .uikit_mut()
        .scratch_mut()
        .texts
        .insert(s.element("x"), "typed".into());
    controller::room_closed(&mut fx.ws, "r1");
    assert!(fx.ws.uikit().bar("bar-1").is_none());
    assert!(fx.ws.uikit().scratch().texts.is_empty());
}

#[test]
fn commands_run_at_app_level() {
    let mut fx = Fx::new();
    // Without a session the request fails at once; the result still settles
    // the trigger without any room view.
    let out = fx.ui(commands::request(
        "r1",
        None,
        "poll".into(),
        "x".into(),
        Some("app1".into()),
    ));
    assert!(out.effects.is_empty());
    assert_eq!(fx.ws.uikit().busy(), 1);
}

fn button(ctx: ButtonContext, action: &str) -> ActionButton {
    ActionButton {
        app_id: "app1".into(),
        action_id: action.into(),
        context: ctx,
        label_i18n: format!("label-{action}"),
        ..Default::default()
    }
}

fn labels<M>(v: &[MenuEntry<M>]) -> Vec<String> {
    v.iter()
        .filter_map(|e| match e {
            MenuEntry::Item(i) => Some(i.label.clone()),
            MenuEntry::Header(h) => Some(format!("# {h}")),
            MenuEntry::Separator => None,
        })
        .collect()
}

#[test]
fn app_buttons_in_menus() {
    let mut fx = Fx::new();
    let mut ai = button(ButtonContext::RoomAction, "ai");
    ai.category = Some("ai".into());
    fx.ws.uikit_mut().set_buttons(vec![
        button(ButtonContext::RoomAction, "room"),
        ai,
        button(ButtonContext::MessageAction, "msg"),
        button(ButtonContext::MessageBoxAction, "box"),
        button(ButtonContext::UserDropdownAction, "user"),
    ]);
    let ws = &fx.ws;
    assert_eq!(
        labels(&buttons::room_entries(ws, "r1", |m| m)),
        ["# Apps", "label-room", "# AI actions", "label-ai"]
    );
    let m = message("m1", "alice", json!([]));
    assert_eq!(
        labels(&buttons::message_entries(
            ws,
            &m,
            "message",
            |m| m,
            UiKitMsg::Popover(None)
        )),
        ["# Apps", "label-msg"]
    );
    // Not in pinned / search lists.
    assert!(buttons::message_entries(ws, &m, "pinned", |m| m, UiKitMsg::Popover(None)).is_empty());
    let mut enc = m.clone();
    enc.t = Some("e2e".into());
    let e = buttons::message_entries(ws, &enc, "message", |m| m, UiKitMsg::Popover(None));
    assert!(
        matches!(e.last(), Some(MenuEntry::Item(i)) if i.disabled),
        "E2E: Unavailable"
    );
    let composer = buttons::composer_entries(ws, "r1", Some("t1"), "draft", |m| m);
    assert_eq!(labels(&composer), ["label-box"]);
    assert_eq!(buttons::user_menu(ws).len(), 1);

    // The composer button sends the composer text and the thread.
    let Some(MenuEntry::Item(item)) = composer.into_iter().next() else {
        panic!("item expected");
    };
    let UiKitMsg::Button {
        button,
        rid,
        tmid,
        mid,
        message,
    } = item.on_select
    else {
        panic!("button expected");
    };
    let (_, i) = fx.ws.uikit_mut().action_button(
        &button,
        rid.as_deref(),
        tmid.as_deref(),
        mid.as_deref(),
        message.as_deref(),
        Instant::now(),
    );
    assert_eq!(
        json_of(&i),
        json!({"type": "actionButton", "actionId": "box", "rid": "r1", "tmid": "t1",
               "payload": {"context": "messageBoxAction", "message": "draft"}, "triggerId": "T"})
    );
}

#[test]
fn appended_sections_drop_a_leading_separator() {
    let mut entries: Vec<MenuEntry<u8>> = Vec::new();
    let apps = vec![MenuEntry::Separator, MenuEntry::Header("Apps".into())];
    buttons::append(&mut entries, &apps);
    assert!(matches!(entries.first(), Some(MenuEntry::Header(_))));
}

#[test]
fn failed_slash_commands_leave_a_local_message() {
    let mut fx = Fx::new();
    let t = fx
        .ws
        .uikit_mut()
        .slash_command_trigger(Some("app1"), "r1", None, Instant::now());
    assert_eq!(fx.ws.uikit().busy(), 1);
    let ran = commands::Ran {
        trigger: t,
        command: "poll".into(),
        rid: "r1".into(),
        tmid: None,
        result: Err(Failure::from(rocketchat::Error::Protocol("boom".into()))),
    };
    commands::finished(&mut fx.ws, &ran);
    assert_eq!(fx.ws.uikit().busy(), 0);
    let tl = fx.ws.timeline("r1").unwrap();
    let m = tl.messages().last().unwrap();
    assert_eq!(m.u.username.as_deref(), Some("rocket.cat"));
    assert!(m.msg.contains("/poll"), "{}", m.msg);
}

#[test]
fn apps_stream_and_banner_hooks() {
    let mut fx = Fx::new();
    let ev = rocketchat::models::AppsStreamEvent::ActionsChanged;
    assert!(fx.ws.uikit().scratch().current_stamp("#buttons").is_none());
    controller::session(&mut fx.ctx(), SessionHook::Apps(Box::new(ev)));
    assert!(
        fx.ws.uikit().scratch().current_stamp("#buttons").is_some(),
        "buttons refetch is debounced"
    );
    let out = controller::session(
        &mut fx.ctx(),
        SessionHook::Apps(Box::new(rocketchat::models::AppsStreamEvent::Removed(
            "app1".into(),
        ))),
    );
    assert_eq!(
        out.effects,
        vec![Effect::AppsChanged {
            commands: false,
            admin: true
        }]
    );
    // Without a session nothing is fetched.
    let out = controller::session(&mut fx.ctx(), SessionHook::Banners);
    assert!(out.effects.is_empty());
}

#[test]
fn session_dispatcher_routes_uikit_events() {
    use crate::session::{Followup, apply_event};
    use rocketchat::session::SessionEvent;
    let mut ws = Workspace::new();
    let si = ServerInteraction::from_value(serde_json::from_str(MODAL).unwrap());
    assert_eq!(
        apply_event(&mut ws, &SessionEvent::UiInteraction(Box::new(si.clone()))),
        Followup::UiKit(SessionHook::Interaction(Box::new(si)))
    );
    assert_eq!(
        apply_event(
            &mut ws,
            &SessionEvent::BannerChanged {
                banner_id: "b".into()
            }
        ),
        Followup::UiKit(SessionHook::Banners)
    );
    let apps = SessionEvent::Unknown {
        collection: "stream-apps".into(),
        event_name: "apps".into(),
        args: json!([["actions/changed", []]]),
    };
    assert!(matches!(
        apply_event(&mut ws, &apps),
        Followup::UiKit(SessionHook::Apps(_))
    ));
    let other = SessionEvent::Unknown {
        collection: "stream-x".into(),
        event_name: "y".into(),
        args: json!([]),
    };
    assert_eq!(apply_event(&mut ws, &other), Followup::None);
}

#[test]
fn pushes_without_a_known_trigger_are_ignored() {
    let mut fx = Fx::new();
    let mut si: Value = serde_json::from_str(MODAL).unwrap();
    si["triggerId"] = json!("someone-elses");
    let out = controller::session(
        &mut fx.ctx(),
        SessionHook::Interaction(Box::new(ServerInteraction::from_value(si))),
    );
    assert!(out.effects.is_empty());
    assert!(fx.ws.uikit().modal().is_none());
}

/// A bar answering an action of a room whose view has closed since opens in
/// the room shown; with no room shown it is dropped with a notice.
#[test]
fn bar_for_a_closed_room_opens_in_the_active_room() {
    let mut fx = Fx::new();
    let out = fx.answer(
        Origin::new(OriginKind::ActionButton(ButtonContext::RoomAction)).rid(Some("r0")),
        serde_json::from_str(BAR).unwrap(),
    );
    assert!(out.effects.contains(&Effect::ShowBar {
        rid: "r1".into(),
        view_id: "bar-1".into()
    }));
    assert_eq!(
        fx.ws.uikit().bar("bar-1").unwrap().rid.as_deref(),
        Some("r1")
    );

    let mut fx = Fx::new();
    let t = fx.ws.uikit_mut().begin(
        Some("app1"),
        Origin::new(OriginKind::ActionButton(ButtonContext::RoomAction)).rid(Some("r0")),
        Instant::now(),
    );
    let mut si: Value = serde_json::from_str(BAR).unwrap();
    si["triggerId"] = Value::String(t.clone());
    let mut ctx = Ctx {
        ws: &mut fx.ws,
        local: &mut fx.local,
        rest: None,
        active_rid: None,
    };
    let out = controller::update(
        &mut ctx,
        Msg::Finished {
            trigger: t,
            result: InteractionResult::Response(InteractionResponse::Interaction(
                ServerInteraction::from_value(si),
            )),
        },
    );
    assert!(out.effects.iter().any(|e| matches!(e, Effect::Toast(..))));
    assert!(fx.ws.uikit().bar("bar-1").is_none());
}

/// A pending `on_character_entered` timer of a modal replaced by another
/// instance with the same view id sends nothing into the new instance.
#[test]
fn debounce_does_not_survive_modal_replacement() {
    let mut fx = Fx::new();
    let origin = || Origin::new(OriginKind::ActionButton(ButtonContext::RoomAction));
    fx.answer(origin(), serde_json::from_str(MODAL).unwrap());
    let surface = SurfaceKey::Modal("view-1".into());
    let a = req(surface.clone(), None, "b_q", "question", Some(json!("old")));
    fx.ui(UiKitMsg::Text {
        req: a.clone(),
        dispatch: true,
    });
    let stamp = fx
        .ws
        .uikit()
        .scratch()
        .current_stamp(&surface.element("question"))
        .unwrap();
    fx.answer(origin(), serde_json::from_str(MODAL).unwrap());
    let before = fx.triggers();
    controller::update(&mut fx.ctx(), Msg::Debounced { req: a, stamp });
    assert_eq!(fx.triggers(), before, "stale text not sent");
}

/// The same for a contextual bar reopened in place (same id, same room): its
/// pending timer and typed texts belong to the old instance.
#[test]
fn debounce_does_not_survive_bar_replacement() {
    let mut fx = Fx::new();
    let origin =
        || Origin::new(OriginKind::ActionButton(ButtonContext::RoomAction)).rid(Some("r1"));
    fx.answer(origin(), serde_json::from_str(BAR).unwrap());
    let surface = SurfaceKey::Bar("bar-1".into());
    let a = req(surface.clone(), None, "b_q", "question", Some(json!("old")));
    fx.ui(UiKitMsg::Text {
        req: a.clone(),
        dispatch: true,
    });
    let key = surface.element("question");
    let stamp = fx.ws.uikit().scratch().current_stamp(&key).unwrap();
    fx.answer(origin(), serde_json::from_str(BAR).unwrap());
    assert!(fx.ws.uikit().bar("bar-1").is_some());
    assert!(!fx.ws.uikit().scratch().texts.contains_key(&key));
    let before = fx.triggers();
    controller::update(&mut fx.ctx(), Msg::Debounced { req: a, stamp });
    assert_eq!(fx.triggers(), before, "stale text not sent");
}

/// Date/time text is marked as an error only when it cannot become valid.
#[test]
fn partial_date_and_time_are_not_errors() {
    use super::elements::typed_plausible;
    for t in [
        "",
        "2",
        "2026-",
        "2026-1",
        "2026-04-3",
        "2026-04-30",
        "2026-02-0",
    ] {
        assert!(typed_plausible(t, Typed::Date), "{t}");
    }
    for t in ["2026-13", "2026-02-3", "2026-04-31", "x", "2026-04-300"] {
        assert!(!typed_plausible(t, Typed::Date), "{t}");
    }
    for t in ["1", "2", "12:", "12:3", "23:59"] {
        assert!(typed_plausible(t, Typed::Time), "{t}");
    }
    for t in ["25", "12:6", "9:30", "12:300"] {
        assert!(!typed_plausible(t, Typed::Time), "{t}");
    }
}
