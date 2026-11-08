//! UIKit and apps-engine protocol: decoding of captured and synthetic fixtures
//! (`fixtures/uikit/`), interaction serialisation and the REST calls against an
//! in-process HTTP server.
#![allow(clippy::unwrap_used)]

mod common;

use std::time::Duration;

use rocketchat::models::{
    ActionButton, AppInfo, AppLogs, AppSetting, AppStatus, AppsCount, AppsStreamEvent, Banner,
    BannerSurface, ButtonContext, Message,
};
use rocketchat::session::{DataKind, DataMessage, SessionEvent, decode_event};
use rocketchat::uikit::{
    BlockActionPayload, CalloutVariant, Container, ContextualBarView, Element, InteractionResponse,
    LayoutBlock, ModalView, RawView, ServerInteraction, Slot, Surface, TextKind, UserInteraction,
    decode_blocks, interaction::MAX_SUBMIT_BODY,
};
use rocketchat::{Credentials, Error, RestClient, ServerUrl, TlsConfig};
use serde_json::{Value, json};

fn fixture(name: &str) -> Value {
    let path = format!(
        "{}/tests/fixtures/uikit/{name}.json",
        env!("CARGO_MANIFEST_DIR")
    );
    serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap()
}

fn data_message(name: &str) -> DataMessage {
    let v = fixture(name);
    DataMessage {
        kind: DataKind::Changed,
        collection: v["collection"].as_str().unwrap().to_owned(),
        id: v["id"].as_str().unwrap().to_owned(),
        fields: v["fields"].as_object().cloned(),
        cleared: Vec::new(),
        before: None,
    }
}

fn server_interaction(name: &str) -> ServerInteraction {
    ServerInteraction::from_value(fixture(name))
}

// --- blocks -----------------------------------------------------------------------

#[test]
fn alice_block_set_decodes_every_kind() {
    let blocks = decode_blocks(&fixture("alice_blocks"));
    let kinds: Vec<&str> = blocks.iter().map(LayoutBlock::type_name).collect();
    assert_eq!(
        kinds,
        [
            "section",
            "divider",
            "image",
            "context",
            "actions",
            "callout",
            "preview",
            "conditional",
            "conditional",
            "info_card",
            "input",
            "unknown"
        ]
    );
    let LayoutBlock::Section {
        meta,
        text,
        fields,
        accessory,
    } = &blocks[0]
    else {
        panic!()
    };
    assert_eq!(meta.app_id.as_deref(), Some("fake-app-id"));
    assert_eq!(text.as_ref().unwrap().kind, TextKind::Mrkdwn);
    assert_eq!(fields.len(), 2);
    let Element::Overflow { a, options } = accessory.as_deref().unwrap() else {
        panic!()
    };
    assert_eq!(a.action_id, "ovf");
    assert_eq!(options[1].url.as_deref(), Some("https://example.com"));
    // Inheritance: the accessory gets the block's appId/blockId.
    let inherited = blocks[0].inherited_elements();
    let a = inherited[0].actionable().unwrap();
    assert_eq!(a.app_id.as_deref(), Some("fake-app-id"));
    assert_eq!(a.block_id.as_deref(), Some("blk_section"));

    let LayoutBlock::Actions { elements, .. } = &blocks[4] else {
        panic!()
    };
    let types: Vec<&str> = elements.iter().map(Element::type_name).collect();
    assert_eq!(
        types,
        [
            "button",
            "button",
            "static_select",
            "multi_static_select",
            "datepicker",
            "time_picker",
            "linear_scale",
            "checkbox",
            "radio_button",
            "toggle_switch"
        ]
    );
    let Element::Button { a, style, .. } = &elements[0] else {
        panic!()
    };
    assert_eq!(style.as_deref(), Some("primary"));
    assert_eq!(a.confirm.as_ref().unwrap().style.as_deref(), Some("danger"));
    assert_eq!(elements[1].url(), Some("https://rocket.chat"));
    assert_eq!(elements[2].initial_value(), Some(json!("b")));
    assert_eq!(elements[3].initial_value(), None);
    assert_eq!(elements[6].initial_value(), Some(json!(3)));
    assert_eq!(elements[7].initial_value(), Some(json!(["c1"])));
    // toggle_switch without initialOptions: no initial value (empty selection).
    assert!(matches!(
        &elements[9],
        Element::ToggleSwitch { initial: None, .. }
    ));
    assert!(elements.iter().all(|e| e.allowed_in(Slot::Actions)));

    let LayoutBlock::Callout { variant, .. } = &blocks[5] else {
        panic!()
    };
    assert_eq!(*variant, CalloutVariant::Warning);
    let LayoutBlock::Preview(p) = &blocks[6] else {
        panic!()
    };
    assert!(p.thumb.is_some() && !p.shows_cover());
    assert_eq!(p.footer.as_ref().unwrap().len(), 1);
    let LayoutBlock::InfoCard { rows, .. } = &blocks[9] else {
        panic!()
    };
    assert!(matches!(
        rows[0].action.as_deref(),
        Some(Element::IconButton { label: Some(l), .. }) if l == "Info"
    ));

    // Message surface: conditionals resolved for rocket.chat, unknown dropped.
    let shown = Surface::Message.renderable(&blocks);
    let shown_kinds: Vec<&str> = shown.iter().map(LayoutBlock::type_name).collect();
    assert_eq!(
        shown_kinds,
        [
            "section",
            "divider",
            "image",
            "context",
            "actions",
            "callout",
            "preview",
            "section",
            "info_card",
            "input"
        ]
    );
    let LayoutBlock::Section { text, .. } = &shown[7] else {
        panic!()
    };
    assert_eq!(text.as_ref().unwrap().text, "rocket.chat only");
    // Banners render no callout; contextual bars no info card.
    assert!(!Surface::Banner.allows(&blocks[5]));
    assert!(!Surface::ContextualBar.allows(&blocks[9]));
}

#[test]
fn tolerant_decoding() {
    let blocks = decode_blocks(&json!([
        {"type": "actions", "elements": [
            {"type": "button"},
            {"type": "image"},
            {"type": "future_element", "x": 1},
            {"type": "static_select", "options": [{"value": "ok"}, {"text": "no value"}, 7]},
            "not an object"
        ]},
        {"type": "section", "text": 42, "fields": 7},
        {"type": "image"},
        {"type": "input", "label": {"type": "plain_text", "text": "L"}},
        {"type": "conditional", "render": [{"type": "conditional", "render": []}]},
        {"no_type": true},
        null
    ]));
    assert_eq!(blocks.len(), 7);
    let LayoutBlock::Actions { elements, .. } = &blocks[0] else {
        panic!()
    };
    assert!(matches!(&elements[0], Element::Button { text, .. } if text.text.is_empty()));
    assert!(matches!(elements[1], Element::Unknown(_)));
    assert!(matches!(elements[2], Element::Unknown(_)));
    let Element::StaticSelect { options, .. } = &elements[3] else {
        panic!()
    };
    assert_eq!(options.len(), 1);
    assert!(matches!(elements[4], Element::Unknown(_)));
    assert!(
        matches!(&blocks[1], LayoutBlock::Section { text: None, fields, .. } if fields.is_empty())
    );
    assert!(matches!(blocks[2], LayoutBlock::Unknown(_)));
    assert!(matches!(blocks[3], LayoutBlock::Unknown(_)));
    // Nested conditionals are not resolved.
    let LayoutBlock::Conditional { render, .. } = &blocks[4] else {
        panic!()
    };
    assert!(matches!(render[0], LayoutBlock::Unknown(_)));
    assert!(matches!(blocks[5], LayoutBlock::Unknown(_)));
    assert!(decode_blocks(&json!({"type": "section"})).is_empty());
}

#[test]
fn size_guard_and_caps() {
    let mut deep = json!("x");
    for _ in 0..40 {
        deep = json!([deep]);
    }
    assert!(decode_blocks(&json!([{"type": "section", "fields": deep}])).is_empty());
    let many: Vec<Value> = (0..80).map(|_| json!({"type": "divider"})).collect();
    assert_eq!(decode_blocks(&Value::Array(many)).len(), 50);
    let msg: Message = serde_json::from_value(json!({
        "_id": "m", "rid": "r", "msg": "fallback", "blocks": []
    }))
    .unwrap();
    assert!(!msg.has_blocks());
    let msg2 = Message {
        blocks: Some(fixture("alice_blocks")),
        ..msg
    };
    assert!(msg2.has_blocks());
    assert_eq!(msg2.uikit_blocks().len(), 12);
}

#[test]
fn ephemeral_mention_message() {
    let SessionEvent::EphemeralMessage(m) = decode_event(&data_message("ephemeral_mention")) else {
        panic!()
    };
    assert_eq!(m.u.id, "rocket.cat");
    let blocks = m.uikit_blocks();
    let LayoutBlock::Actions { meta, elements } = &blocks[1] else {
        panic!()
    };
    assert_eq!(meta.app_id.as_deref(), Some("mention-core"));
    let ids: Vec<&str> = elements.iter().filter_map(Element::action_id).collect();
    assert_eq!(ids, ["add-users", "dismiss", "share-message"]);
    let a = elements[1].inherited(meta);
    let a = a.actionable().unwrap();
    assert_eq!(a.block_id.as_deref(), Some("vSxru3XTbnFh8HJrc"));
}

// --- interactions -----------------------------------------------------------------

#[test]
fn user_interactions_serialise_like_the_web() {
    let i = UserInteraction::BlockAction {
        action_id: "dismiss".into(),
        payload: BlockActionPayload {
            block_id: "vSxru3XTbnFh8HJrc".into(),
            value: Some(json!("[{}]")),
        },
        container: Container::Message("1794088390781".into()),
        mid: Some("1794088390781".into()),
        tmid: None,
        rid: Some("6aef9dc6ff2338a3f2a2ea1a".into()),
        trigger_id: "T".into(),
    };
    assert_eq!(
        serde_json::to_value(&i).unwrap(),
        json!({"type": "blockAction", "actionId": "dismiss",
               "payload": {"blockId": "vSxru3XTbnFh8HJrc", "value": "[{}]"},
               "container": {"type": "message", "id": "1794088390781"},
               "mid": "1794088390781", "rid": "6aef9dc6ff2338a3f2a2ea1a", "triggerId": "T"})
    );
    let url = UserInteraction::BlockAction {
        action_id: "btn_url".into(),
        payload: BlockActionPayload {
            block_id: "b".into(),
            value: None,
        },
        container: Container::View("v".into()),
        mid: None,
        tmid: None,
        rid: None,
        trigger_id: "T".into(),
    };
    assert_eq!(
        serde_json::to_value(&url).unwrap(),
        json!({"type": "blockAction", "actionId": "btn_url", "payload": {"blockId": "b"},
               "container": {"type": "view", "id": "v"}, "triggerId": "T"})
    );

    let raw = fixture("synthetic_modal_open")["view"].clone();
    let state = json!({"b_q": {"question": "Lunch?"}});
    let submit =
        UserInteraction::view_submit(&raw, state.clone(), "view-1", "app1", None, "T".into());
    let v = serde_json::to_value(&submit).unwrap();
    assert_eq!(v["type"], "viewSubmit");
    assert_eq!(v["viewId"], "view-1");
    assert_eq!(v["payload"]["view"]["state"], state);
    // Unknown view fields are echoed.
    assert_eq!(v["payload"]["view"]["customField"]["kept"], true);
    assert!(v.get("rid").is_none());

    let closed = UserInteraction::view_closed(
        &raw,
        json!({}),
        "view-1",
        false,
        Some("r".into()),
        "T".into(),
    );
    let v = serde_json::to_value(&closed).unwrap();
    assert_eq!(v["payload"]["viewId"], "view-1");
    assert_eq!(v["payload"]["isCleared"], false);
    assert_eq!(v["rid"], "r");

    let banner = UserInteraction::banner_closed(
        &json!({"viewId": "b1", "appId": "banner-core"}),
        json!({}),
        "b1",
        "T".into(),
    );
    let v = serde_json::to_value(&banner).unwrap();
    assert_eq!(v["payload"]["view"]["id"], "b1");
    assert_eq!(v["payload"]["view"]["viewId"], "b1");
    assert_eq!(v["payload"]["isCleared"], true);

    let button = UserInteraction::ActionButton {
        action_id: "create-poll".into(),
        payload: rocketchat::uikit::ActionButtonPayload {
            context: ButtonContext::MessageBoxAction,
            message: Some(String::new()),
        },
        mid: None,
        tmid: Some("t".into()),
        rid: Some("r".into()),
        trigger_id: "T".into(),
    };
    assert_eq!(
        serde_json::to_value(&button).unwrap(),
        json!({"type": "actionButton", "actionId": "create-poll", "rid": "r", "tmid": "t",
               "payload": {"context": "messageBoxAction", "message": ""}, "triggerId": "T"})
    );
}

#[test]
fn large_submits_use_the_compact_form() {
    let big = "x".repeat(MAX_SUBMIT_BODY);
    let raw = json!({"id": "v", "appId": "a", "blocks": [], "huge": big});
    let i = UserInteraction::view_submit(&raw, json!({"b": {"a": 1}}), "v", "a", None, "T".into());
    let v = serde_json::to_value(&i).unwrap();
    assert_eq!(
        v["payload"]["view"],
        json!({"id": "v", "appId": "a", "state": {"b": {"a": 1}}})
    );
    assert!(serde_json::to_vec(&i).unwrap().len() < 1000);
}

#[test]
fn server_interactions_decode() {
    let ServerInteraction::ModalOpen {
        trigger_id,
        app_id,
        view,
    } = server_interaction("synthetic_modal_open")
    else {
        panic!()
    };
    assert_eq!((trigger_id.as_str(), app_id.as_str()), ("TRIGGER", "app1"));
    assert_eq!(view.view.id, "view-1");
    assert!(view.view.show_icon);
    assert!(matches!(view.view.submit, Some(Element::Button { .. })));
    assert_eq!(view.view.blocks.len(), 3);
    assert_eq!(view.raw["customField"]["kept"], true);

    let ServerInteraction::Errors {
        view_id, errors, ..
    } = server_interaction("synthetic_errors_object")
    else {
        panic!()
    };
    assert_eq!(view_id, "view-1");
    assert_eq!(errors["question"], "Question is required");
    let ServerInteraction::Errors { errors, .. } = server_interaction("synthetic_errors_array")
    else {
        panic!()
    };
    assert_eq!(errors.len(), 2);
    assert_eq!(errors["opts"], "Pick one");

    let ServerInteraction::ContextualBarOpen { view, .. } =
        server_interaction("synthetic_contextual_bar_open")
    else {
        panic!()
    };
    let bar: &RawView<ContextualBarView> = &view;
    assert_eq!(bar.view.id, "bar-1");
    assert!(
        matches!(&bar.view.blocks[1], LayoutBlock::TabNavigation { tabs, .. } if tabs.len() == 2)
    );
    assert!(matches!(
        server_interaction("synthetic_contextual_bar_close"),
        ServerInteraction::ContextualBarClose { view_id, .. } if view_id == "bar-1"
    ));
    assert!(matches!(
        server_interaction("synthetic_modal_close_viewid"),
        ServerInteraction::ModalClose { view_id: Some(v), .. } if v == "announcement-1"
    ));
    let ServerInteraction::BannerOpen { view, .. } = server_interaction("synthetic_banner_open")
    else {
        panic!()
    };
    assert_eq!(view.view.view_id, "banner-1");
    assert_eq!(
        view.view.title.as_ref().unwrap().text(),
        "Maintenance tonight"
    );
    assert!(view.raw.get("type").is_none());
    assert!(matches!(
        server_interaction("synthetic_banner_close"),
        ServerInteraction::BannerClose { view_id, .. } if view_id == "banner-1"
    ));
    let ServerInteraction::ModalOpen { view, .. } = server_interaction("synthetic_videoconf_info")
    else {
        panic!()
    };
    let m: &RawView<ModalView> = &view;
    assert_eq!(m.view.id, "callid123-info");
    assert!(m.view.submit.is_none() && m.view.close.is_some());
    let u = server_interaction("synthetic_unknown_interaction");
    assert!(matches!(u, ServerInteraction::Unknown(_)));
    assert_eq!(u.trigger_id(), "TRIGGER");
    // A known type without a view is unknown too.
    assert!(matches!(
        ServerInteraction::from_value(json!({"type": "modal.open", "triggerId": "t"})),
        ServerInteraction::Unknown(_)
    ));
}

#[test]
fn interaction_responses() {
    assert_eq!(
        InteractionResponse::from_body(b"").unwrap(),
        InteractionResponse::Empty
    );
    assert_eq!(
        InteractionResponse::from_body(b" \n").unwrap(),
        InteractionResponse::Empty
    );
    assert_eq!(
        InteractionResponse::from_body(b"{}").unwrap(),
        InteractionResponse::Ack
    );
    assert_eq!(
        InteractionResponse::from_body(br#"{"success":false}"#).unwrap(),
        InteractionResponse::Ack
    );
    let errors = serde_json::to_vec(&fixture("synthetic_errors_object")).unwrap();
    assert!(matches!(
        InteractionResponse::from_body(&errors).unwrap(),
        InteractionResponse::Interaction(ServerInteraction::Errors { .. })
    ));
    assert!(InteractionResponse::from_body(b"<pre>Payload Too Large</pre>").is_err());
}

#[test]
fn session_events() {
    let SessionEvent::UiInteraction(si) =
        decode_event(&data_message("synthetic_ui_interaction_errors"))
    else {
        panic!()
    };
    assert!(matches!(*si, ServerInteraction::Errors { ref view_id, .. } if view_id == "view-1"));
    assert_eq!(si.trigger_id(), "tr2");

    let ev = decode_event(&data_message("synthetic_stream_apps_status"));
    assert_eq!(
        AppsStreamEvent::from_session_event(&ev),
        Some(AppsStreamEvent::StatusUpdate {
            app_id: "app1".into(),
            status: AppStatus::ManuallyDisabled
        })
    );
    let ev = decode_event(&data_message("synthetic_stream_apps_actions"));
    assert_eq!(
        AppsStreamEvent::from_session_event(&ev),
        Some(AppsStreamEvent::ActionsChanged)
    );
    assert_eq!(
        AppsStreamEvent::decode(&[json!(["command/removed", ["poll"]])]),
        Some(AppsStreamEvent::CommandRemoved("poll".into()))
    );
    assert!(matches!(
        AppsStreamEvent::decode(&[json!(["app/future", [1]])]),
        Some(AppsStreamEvent::Unknown { key, .. }) if key == "app/future"
    ));
}

// --- apps models ------------------------------------------------------------------

#[test]
fn apps_models() {
    let buttons: Vec<ActionButton> =
        rocketchat::models::lenient::vec_from_value(fixture("synthetic_action_buttons"));
    assert_eq!(buttons.len(), 4);
    assert_eq!(buttons[0].context, ButtonContext::MessageBoxAction);
    assert!(buttons[1].is_danger());
    let when = buttons[1].when.as_ref().unwrap();
    assert_eq!(when.room_types.as_ref().unwrap().len(), 2);
    assert!(buttons[2].is_ai());
    assert_eq!(buttons[3].context, ButtonContext::Unknown);
    let live: Vec<ActionButton> =
        rocketchat::models::lenient::vec_from_value(fixture("action_buttons"));
    assert!(live.is_empty());

    let count: AppsCount = serde_json::from_value(fixture("apps_count")).unwrap();
    assert_eq!(count.max_marketplace_apps, 5);

    let apps: Vec<AppInfo> = rocketchat::models::lenient::vec_from_value(
        fixture("synthetic_apps_installed")["apps"].clone(),
    );
    assert_eq!(apps.len(), 2);
    assert_eq!(apps[0].name, "Poll");
    assert!(apps[0].status.is_enabled());
    assert_eq!(apps[0].author.name, "Rocket.Chat");
    assert_eq!(apps[0].permissions.as_ref().unwrap()[0].name, "ui.interact");
    assert!(apps[1].status.is_disabled());
    assert!(apps[1].implements.is_empty());

    let settings: Vec<AppSetting> = fixture("synthetic_app_settings")["settings"]
        .as_object()
        .unwrap()
        .values()
        .map(|v| serde_json::from_value(v.clone()).unwrap())
        .collect();
    let mode = settings.iter().find(|s| s.id == "mode").unwrap();
    assert_eq!(mode.effective_value(), &json!("a"));
    assert_eq!(mode.values.as_ref().unwrap()[1].i18n_label, "mode_b");
    let key = settings.iter().find(|s| s.id == "api_key").unwrap();
    // Unknown fields round-trip.
    let back = serde_json::to_value(key).unwrap();
    assert_eq!(back["createdAt"], "2026-01-01T00:00:00.000Z");
    assert_eq!(back["type"], "password");

    let logs: AppLogs = serde_json::from_value(fixture("synthetic_app_logs")).unwrap();
    assert_eq!(logs.logs[0].entries.len(), 2);
    assert_eq!(logs.logs[0].total_time, Some(42.0));
    let live_logs: AppLogs = serde_json::from_value(fixture("apps_logs")).unwrap();
    assert!(live_logs.logs.is_empty());

    let banners: Vec<Banner> = rocketchat::models::lenient::vec_from_value(
        fixture("synthetic_banners")["banners"].clone(),
    );
    assert_eq!(banners.len(), 3);
    assert_eq!(banners[1].surface, BannerSurface::Modal);
    assert_eq!(banners[0].dictionary_map()["en"]["title_key"], "Cloud news");
}

// --- REST against a mock server ---------------------------------------------------

fn mock_client(port: u16) -> RestClient {
    let tls = TlsConfig::new().build().unwrap();
    let c = RestClient::new(
        ServerUrl::parse(&format!("http://127.0.0.1:{port}")).unwrap(),
        &tls,
    )
    .unwrap();
    c.set_credentials(Some(Credentials::new("uid", "tok")));
    c
}

fn dismiss() -> UserInteraction {
    UserInteraction::BlockAction {
        action_id: "dismiss".into(),
        payload: BlockActionPayload {
            block_id: "b".into(),
            value: None,
        },
        container: Container::Message("m".into()),
        mid: Some("m".into()),
        tmid: None,
        rid: Some("r".into()),
        trigger_id: "T".into(),
    }
}

#[tokio::test]
async fn ui_interaction_over_http() {
    let errors = serde_json::to_string(&fixture("synthetic_errors_object")).unwrap();
    let unknown = serde_json::to_string(&fixture("error_unknown_app")).unwrap();
    let (port, log) = common::serve(move |req| {
        let first = req.lines().next().unwrap_or_default().to_owned();
        Some(if first.contains("/empty ") {
            common::http_response("200 OK", &[], "")
        } else if first.contains("/ack ") {
            common::json_ok("{}")
        } else if first.contains("/errors ") {
            common::json_ok(&errors)
        } else if first.contains("/big ") {
            common::http_response(
                "413 Payload Too Large",
                &[("Content-Type", "text/html")],
                "<pre>Payload Too Large</pre>",
            )
        } else if first.contains("/slow ") {
            return None;
        } else {
            common::http_response(
                "500 Internal Server Error",
                &[("Content-Type", "application/json")],
                &unknown,
            )
        })
    })
    .await;
    let c = mock_client(port);
    let i = dismiss();
    assert_eq!(
        c.ui_interaction("empty", &i).await.unwrap(),
        InteractionResponse::Empty
    );
    assert_eq!(
        c.ui_interaction("ack", &i).await.unwrap(),
        InteractionResponse::Ack
    );
    assert!(matches!(
        c.ui_interaction("errors", &i).await.unwrap(),
        InteractionResponse::Interaction(ServerInteraction::Errors { .. })
    ));
    let e = c.ui_interaction("fake-app", &i).await.unwrap_err();
    assert!(matches!(e, Error::Api { status: 500, .. }), "{e:?}");
    assert!(e.to_string().contains("Cannot read properties"));
    let e = c.ui_interaction("big", &i).await.unwrap_err();
    assert!(matches!(e, Error::Status { status: 413, .. }), "{e:?}");
    let e = c
        .ui_interaction_with_timeout("slow", &i, Duration::from_millis(300))
        .await
        .unwrap_err();
    assert!(matches!(e, Error::Timeout(_)), "{e:?}");
    assert!(c.ui_interaction("a/b", &i).await.is_err());

    let reqs = log.lock().unwrap().clone();
    assert!(reqs[0].starts_with("POST /api/apps/ui.interaction/empty HTTP/1.1"));
    let lower = reqs[0].to_ascii_lowercase();
    assert!(lower.contains("x-user-id: uid") && lower.contains("x-auth-token: tok"));
    assert_eq!(common::body_json(&reqs[0])["actionId"], "dismiss");
}

#[tokio::test]
async fn apps_endpoints_over_http() {
    let (port, log) = common::serve(|req| {
        let first = req.lines().next().unwrap_or_default().to_owned();
        let body = if first.contains("/api/apps/actionButtons") {
            serde_json::to_string(&fixture("synthetic_action_buttons")).unwrap()
        } else if first.contains("/api/apps/languages") {
            serde_json::to_string(&fixture("synthetic_apps_languages")).unwrap()
        } else if first.contains("/api/apps/installed") {
            serde_json::to_string(&fixture("synthetic_apps_installed")).unwrap()
        } else if first.contains("/settings") {
            serde_json::to_string(&fixture("synthetic_app_settings")).unwrap()
        } else if first.contains("/logs") {
            serde_json::to_string(&fixture("synthetic_app_logs")).unwrap()
        } else if first.contains("/status") {
            r#"{"status":"manually_disabled","success":true}"#.to_owned()
        } else if first.contains("/api/v1/banners") {
            serde_json::to_string(&fixture("synthetic_banners")).unwrap()
        } else {
            r#"{"success":true}"#.to_owned()
        };
        Some(common::json_ok(&body))
    })
    .await;
    let c = mock_client(port);
    assert_eq!(c.action_buttons().await.unwrap().len(), 4);
    let langs = c.apps_languages().await.unwrap();
    assert_eq!(langs[0].languages["pt-BR"]["create_poll"], "Criar enquete");
    assert_eq!(c.installed_apps().await.unwrap().len(), 2);
    assert_eq!(c.app_settings("poll").await.unwrap().len(), 3);
    let logs = c
        .app_logs(
            "poll",
            &rocketchat::models::AppLogsQuery {
                count: Some(10),
                log_level: Some(2),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(logs.total, 1);
    assert_eq!(
        c.set_app_status("poll", false).await.unwrap(),
        AppStatus::ManuallyDisabled
    );
    assert_eq!(c.banners().await.unwrap().len(), 3);
    c.dismiss_banner("ban1").await.unwrap();
    assert_eq!(
        c.app_icon_url("poll").unwrap(),
        format!("http://127.0.0.1:{port}/api/apps/poll/icon")
    );

    let reqs = log.lock().unwrap().clone();
    let firsts: Vec<String> = reqs
        .iter()
        .map(|r| r.lines().next().unwrap_or_default().to_owned())
        .collect();
    assert!(
        firsts.contains(&"GET /api/apps/installed?includeClusterStatus=true HTTP/1.1".to_owned())
    );
    assert!(firsts.contains(&"GET /api/apps/poll/logs?count=10&logLevel=2 HTTP/1.1".to_owned()));
    assert!(firsts.contains(&"GET /api/v1/banners?platform=web HTTP/1.1".to_owned()));
    let status = reqs
        .iter()
        .find(|r| r.starts_with("POST /api/apps/poll/status"))
        .unwrap();
    assert_eq!(
        common::body_json(status),
        json!({"status": "manually_disabled"})
    );
    let dismiss = reqs
        .iter()
        .find(|r| r.starts_with("POST /api/v1/banners.dismiss"))
        .unwrap();
    assert_eq!(common::body_json(dismiss), json!({"bannerId": "ban1"}));
}
