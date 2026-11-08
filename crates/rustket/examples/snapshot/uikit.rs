//! UIKit shots (no app can be installed on the dev server, so everything is
//! synthetic and injected into the workspace / store; nothing is created on
//! the server): a message with every block kind, the message menu and the
//! room header menu with app buttons, a contextual bar, a banner and a modal
//! with inputs and a validation error. Light and dark.
//!
//! `RUSTKET_SNAPSHOT_ONLY=uikit`: only these (plus the integration shots).

use std::path::Path;
use std::time::{Duration, Instant};

use iced::advanced::renderer::Headless as _;
use iced::{Pixels, Size, mouse};
use iced_runtime::user_interface::{Cache, UserInterface};
use rocketchat::RcDate;
use rocketchat::models::{ActionButton, ButtonContext, Message as RcMessage, UserRef};
use rocketchat::uikit::{InteractionResponse, ServerInteraction};
use rustket::message::Message;
use rustket::room;
use rustket::uikit::{Msg, SurfaceKey, UiKitMsg};
use rustket_core::ServerId;
use rustket_core::config::Theme;
use rustket_core::uikit::{InteractionResult, Origin, OriginKind};
use serde_json::{Value, json};

use super::Driver;

const ALICE_BLOCKS: &str =
    include_str!("../../../rocketchat/tests/fixtures/uikit/alice_blocks.json");
const TALL: Size = Size::new(1280.0, 1500.0);

fn shoot_with(d: &Driver, dir: &Path, name: &str, size: Size, hover: Option<iced::Point>) {
    let mut renderer = iced::Renderer::Secondary(iced_tiny_skia::Renderer::new(
        rustket_ui::theme::DEFAULT_FONT,
        Pixels(rustket_ui::theme::TEXT_BODY),
    ));
    let theme = d.app.theme();
    let t = rustket_ui::tokens(&theme);
    let mut ui = UserInterface::build(d.app.view(), size, Cache::default(), &mut renderer);
    let point = hover.unwrap_or(iced::Point::new(-10.0, -10.0));
    let cursor = mouse::Cursor::Available(point);
    let mut events = vec![iced::Event::Window(iced::window::Event::RedrawRequested(
        Instant::now(),
    ))];
    if hover.is_some() {
        events.push(iced::Event::Mouse(mouse::Event::CursorMoved {
            position: point,
        }));
    }
    let mut msgs = Vec::new();
    let _ = ui.update(
        &events,
        cursor,
        &mut renderer,
        &mut super::NoClip,
        &mut msgs,
    );
    if hover.is_some() {
        // Rebuild once so widgets shown on hover (the toolbar and its open
        // menu) take part in layout and overlays.
        let cache = ui.into_cache();
        ui = UserInterface::build(d.app.view(), size, cache, &mut renderer);
        let redraw = [iced::Event::Window(iced::window::Event::RedrawRequested(
            Instant::now(),
        ))];
        let _ = ui.update(
            &redraw,
            cursor,
            &mut renderer,
            &mut super::NoClip,
            &mut msgs,
        );
    }
    ui.draw(
        &mut renderer,
        &theme,
        &iced::advanced::renderer::Style {
            text_color: t.text_primary,
        },
        cursor,
    );
    let rgba = renderer.screenshot(
        Size::new(size.width as u32, size.height as u32),
        1.0,
        t.surface_room,
    );
    let path = dir.join(format!("{name}.png"));
    super::write_png(&path, size.width as u32, size.height as u32, &rgba);
    println!("wrote {}", path.display());
}

fn both(d: &mut Driver, out: &Path, name: &str, size: Size, hover: Option<iced::Point>) {
    d.app.config.ui.theme = Theme::Light;
    shoot_with(d, out, &format!("{name}-light"), size, hover);
    d.app.config.ui.theme = Theme::Dark;
    shoot_with(d, out, &format!("{name}-dark"), size, hover);
    d.app.config.ui.theme = Theme::Light;
}

fn bot_message(rid: &str, id: &str, text: &str, blocks: Option<Value>, ms: i64) -> RcMessage {
    RcMessage {
        id: id.into(),
        rid: rid.into(),
        msg: text.into(),
        ts: RcDate::from_millis(ms),
        updated_at: RcDate::from_millis(ms),
        u: UserRef {
            id: "poll.bot".into(),
            username: Some("poll.bot".into()),
            name: Some("Poll".into()),
        },
        blocks,
        ..Default::default()
    }
}

/// Delivers a synthetic server interaction as the HTTP answer of a trigger
/// issued for `origin` (the real path: `UiKitStore::finish`).
fn answer(d: &mut Driver, sid: &ServerId, origin: Origin, mut si: Value) {
    let trigger = d
        .app
        .servers
        .get_mut(sid)
        .expect("server")
        .workspace
        .uikit_mut()
        .begin(Some("app1"), origin, Instant::now());
    si["triggerId"] = Value::String(trigger.clone());
    let result = InteractionResult::Response(InteractionResponse::Interaction(
        ServerInteraction::from_value(si),
    ));
    let epoch = d.app.servers[sid].generation;
    d.send(Message::UiKit(
        sid.clone(),
        epoch,
        Msg::Finished { trigger, result },
    ));
}

fn buttons() -> Vec<ActionButton> {
    let b = |ctx: ButtonContext, action: &str, label: &str, variant: Option<&str>| ActionButton {
        app_id: "app1".into(),
        action_id: action.into(),
        context: ctx,
        label_i18n: label.into(),
        variant: variant.map(str::to_owned),
        ..Default::default()
    };
    vec![
        b(
            ButtonContext::MessageAction,
            "quote-poll",
            "Create poll from message",
            None,
        ),
        b(
            ButtonContext::MessageAction,
            "report-poll",
            "Report to moderators",
            Some("danger"),
        ),
        b(
            ButtonContext::RoomAction,
            "room-polls",
            "Polls of this room",
            None,
        ),
        b(
            ButtonContext::MessageBoxAction,
            "create-poll",
            "Create poll",
            None,
        ),
        b(
            ButtonContext::UserDropdownAction,
            "my-polls",
            "My polls",
            None,
        ),
    ]
}

fn modal_open() -> Value {
    json!({
        "type": "modal.open", "appId": "app1",
        "view": {
            "appId": "app1", "type": "modal", "id": "poll-modal", "showIcon": true,
            "title": {"type": "plain_text", "text": "Create poll"},
            "submit": {"type": "button", "actionId": "submit", "text": {"type": "plain_text", "text": "Create"}},
            "close": {"type": "button", "actionId": "close", "text": {"type": "plain_text", "text": "Cancel"}},
            "blocks": [
                {"type": "section", "blockId": "intro", "text": {"type": "mrkdwn", "text": "Ask the room a *question*. Votes are visible to everyone in #general."}},
                {"type": "input", "blockId": "b_q", "label": {"type": "plain_text", "text": "Question"},
                 "element": {"type": "plain_text_input", "actionId": "question", "placeholder": {"type": "plain_text", "text": "What should we ask?"}}},
                {"type": "input", "blockId": "b_desc", "label": {"type": "plain_text", "text": "Description"},
                 "hint": {"type": "plain_text", "text": "Shown below the question"},
                 "element": {"type": "plain_text_input", "actionId": "desc", "multiline": true, "initialValue": "Pick one option.\nThe poll closes on Friday."}},
                {"type": "input", "blockId": "b_kind", "label": {"type": "plain_text", "text": "Kind"},
                 "element": {"type": "static_select", "actionId": "kind", "initialValue": "single",
                  "placeholder": {"type": "plain_text", "text": "Choose"},
                  "options": [{"text": {"type": "plain_text", "text": "Single choice"}, "value": "single"},
                              {"text": {"type": "plain_text", "text": "Multiple choice"}, "value": "multi"}]}},
                {"type": "input", "blockId": "b_close", "label": {"type": "plain_text", "text": "Closes on"},
                 "element": {"type": "datepicker", "actionId": "closes", "initialDate": "2026-11-13"}},
                {"type": "input", "blockId": "b_who", "label": {"type": "plain_text", "text": "Notify"},
                 "element": {"type": "multi_users_select", "actionId": "who", "placeholder": {"type": "plain_text", "text": "Search users"}}},
                {"type": "input", "blockId": "b_vis", "label": {"type": "plain_text", "text": "Visibility"},
                 "element": {"type": "radio_button", "actionId": "vis", "initialOption": {"text": {"type": "plain_text", "text": "Public"}, "value": "public"},
                  "options": [{"text": {"type": "plain_text", "text": "Public"}, "value": "public"},
                              {"text": {"type": "mrkdwn", "text": "*Anonymous*"}, "value": "anon"}]}},
                {"type": "input", "blockId": "b_opts", "label": {"type": "plain_text", "text": "Options"},
                 "element": {"type": "toggle_switch", "actionId": "opts",
                  "initialOptions": [{"text": {"type": "plain_text", "text": "Allow comments"}, "value": "comments"}],
                  "options": [{"text": {"type": "plain_text", "text": "Allow comments"}, "value": "comments"},
                              {"text": {"type": "plain_text", "text": "Show results live"}, "value": "live"}]}},
                {"type": "input", "blockId": "b_scale", "label": {"type": "plain_text", "text": "Importance"},
                 "element": {"type": "linear_scale", "actionId": "imp", "minValue": 1, "maxValue": 5,
                  "preLabel": {"type": "plain_text", "text": "Low"}, "postLabel": {"type": "plain_text", "text": "High"}}}
            ]
        }
    })
}

fn bar_open() -> Value {
    json!({
        "type": "contextual_bar.open", "appId": "app1",
        "view": {
            "appId": "app1", "type": "contextualBar", "id": "poll-bar",
            "title": {"type": "plain_text", "text": "Poll results"},
            "submit": {"type": "button", "actionId": "vote", "text": {"type": "plain_text", "text": "Vote"}},
            "close": {"type": "button", "actionId": "close", "text": {"type": "plain_text", "text": "Close"}},
            "blocks": [
                {"type": "tab_navigation", "blockId": "tabs", "tabs": [
                    {"type": "tab", "actionId": "t-open", "title": {"type": "plain_text", "text": "Open"}, "selected": true},
                    {"type": "tab", "actionId": "t-closed", "title": {"type": "plain_text", "text": "Closed"}},
                    {"type": "tab", "actionId": "t-mine", "title": {"type": "plain_text", "text": "Mine"}, "disabled": true}]},
                {"type": "section", "blockId": "q", "text": {"type": "mrkdwn", "text": "*Where do we go for lunch?*\n_12 votes_ · closes Friday"}},
                {"type": "context", "blockId": "c1", "elements": [{"type": "mrkdwn", "text": "Started by @poll.bot"}]},
                {"type": "divider"},
                {"type": "section", "blockId": "o1", "text": {"type": "mrkdwn", "text": "🍕 Pizza — *7*"},
                 "accessory": {"type": "button", "actionId": "vote-1", "value": "1", "text": {"type": "plain_text", "text": "Vote"}, "style": "primary"}},
                {"type": "section", "blockId": "o2", "text": {"type": "mrkdwn", "text": "🥗 Salad — *3*"},
                 "accessory": {"type": "button", "actionId": "vote-2", "value": "2", "text": {"type": "plain_text", "text": "Vote"}}},
                {"type": "section", "blockId": "o3", "text": {"type": "mrkdwn", "text": "🍜 Ramen — *2*"},
                 "accessory": {"type": "overflow", "actionId": "more", "options": [
                    {"text": {"type": "plain_text", "text": "Remove option"}, "value": "rm"}]}},
                {"type": "input", "blockId": "b_c", "label": {"type": "plain_text", "text": "Comment"},
                 "element": {"type": "plain_text_input", "actionId": "comment", "placeholder": {"type": "plain_text", "text": "Optional"}}},
                {"type": "callout", "blockId": "co", "variant": "info", "title": {"type": "plain_text", "text": "Tip"},
                 "text": {"type": "mrkdwn", "text": "You can change your vote until the poll closes."}}
            ]
        }
    })
}

fn banner_open() -> Value {
    json!({
        "type": "banner.open", "appId": "app1", "viewId": "maint-1", "variant": "warning",
        "title": "Scheduled maintenance tonight",
        "blocks": [
            {"type": "section", "blockId": "b1", "text": {"type": "mrkdwn", "text": "The server restarts at *22:00 UTC* for about 10 minutes."}},
            {"type": "actions", "blockId": "b2", "elements": [
                {"type": "button", "actionId": "details", "text": {"type": "plain_text", "text": "Details"}, "url": "https://example.com/maintenance"},
                {"type": "button", "actionId": "ok", "value": "ok", "text": {"type": "plain_text", "text": "Got it"}, "style": "primary"}]}
        ]
    })
}

fn errors(view_id: &str) -> Value {
    json!({"type": "errors", "appId": "app1", "viewId": view_id,
           "errors": {"question": "Please enter a question", "imp": "Pick a value"}})
}

fn room_msg(d: &Driver, sid: &ServerId, rid: &str, m: room::Message) -> Message {
    Message::Room(
        sid.clone(),
        d.app.servers[sid].generation,
        rid.to_owned(),
        m,
    )
}

pub async fn shoot(d: &mut Driver, out: &Path, sid: &ServerId, rid: &str) {
    if std::env::var("RUSTKET_SNAPSHOT_ONLY").is_ok_and(|v| v != "uikit") {
        return;
    }
    d.send(Message::Sidebar(
        sid.clone(),
        rustket::workspace::sidebar::Message::Open(rid.to_owned()),
    ));
    let (s2, r2) = (sid.clone(), rid.to_owned());
    d.pump_until("uikit room", Duration::from_secs(20), move |a| {
        a.servers[&s2].rooms.get(&r2).is_some_and(|r| r.loaded)
    })
    .await;

    // A message with every block kind (alice's live block set) plus a small
    // message below it for the hover / menu shots.
    let now = chrono::Utc::now().timestamp_millis();
    let blocks: Value = serde_json::from_str(ALICE_BLOCKS).expect("fixture");
    {
        let ws = &mut d.app.servers.get_mut(sid).expect("server").workspace;
        ws.apply_message(bot_message(
            rid,
            "rk-uikit-blocks",
            "fallback text",
            Some(blocks),
            now,
        ));
        ws.uikit_mut().set_buttons(buttons());
    }
    // Warm-up frame: requests the block images.
    shoot_with(d, out, "u0-warmup", TALL, None);
    d.send(Message::FlushImages);
    d.settle(Duration::from_secs(3)).await;
    both(d, out, "u1-message-blocks", TALL, None);

    // A select opened inside the message and a loading button.
    {
        let ws = &mut d.app.servers.get_mut(sid).expect("server").workspace;
        let surface = SurfaceKey::Message {
            rid: rid.to_owned(),
            mid: "rk-uikit-blocks".into(),
            tmid: None,
        };
        let s = ws.uikit_mut().scratch_mut();
        s.open = Some(surface.element("sel"));
        s.loading.insert(surface.element("btn_primary"), "T".into());
    }
    both(d, out, "u2-message-select", TALL, None);
    // The date picker's calendar, with an app request in flight (the
    // floating "Loading" pill).
    let busy = {
        let ws = &mut d.app.servers.get_mut(sid).expect("server").workspace;
        let surface = SurfaceKey::Message {
            rid: rid.to_owned(),
            mid: "rk-uikit-blocks".into(),
            tmid: None,
        };
        let s = ws.uikit_mut().scratch_mut();
        s.loading.clear();
        s.open = Some(format!("{}#cal", surface.element("date")));
        ws.uikit_mut().begin(
            Some("app1"),
            Origin::new(OriginKind::SlashCommand),
            Instant::now(),
        )
    };
    both(d, out, "u2b-calendar-busy", TALL, None);
    {
        let ws = &mut d.app.servers.get_mut(sid).expect("server").workspace;
        ws.uikit_mut().finish_command(&busy, Instant::now());
        let s = ws.uikit_mut().scratch_mut();
        s.open = None;
        s.loading.clear();
    }

    // Message menu with the app buttons ("Apps" section).
    {
        let ws = &mut d.app.servers.get_mut(sid).expect("server").workspace;
        ws.apply_message(bot_message(
            rid,
            "rk-uikit-plain",
            "Lunch poll is open, vote in the panel",
            None,
            now + 1,
        ));
    }
    d.settle(Duration::from_millis(500)).await;
    let m = room_msg(
        d,
        sid,
        rid,
        room::Message::Timeline(room::timeline::Message::Item(
            room::timeline::ItemMsg::Menu(Some("rk-uikit-plain".into())),
        )),
    );
    d.send(m);
    both(
        d,
        out,
        "u3-message-menu",
        rustket_ui_size(),
        Some(iced::Point::new(800.0, 688.0)),
    );
    let m = room_msg(
        d,
        sid,
        rid,
        room::Message::Timeline(room::timeline::Message::Item(
            room::timeline::ItemMsg::Menu(None),
        )),
    );
    d.send(m);

    // Room header menu with the "Apps" section.
    let m = room_msg(
        d,
        sid,
        rid,
        room::Message::Header(room::header::Message::ToggleMenu),
    );
    d.send(m);
    both(d, out, "u4-header-menu", rustket_ui_size(), None);
    let m = room_msg(
        d,
        sid,
        rid,
        room::Message::Header(room::header::Message::CloseMenu),
    );
    d.send(m);

    // Composer app menu.
    let key = rustket::uikit::buttons::composer_menu_key(rid, None);
    let epoch = d.app.servers[sid].generation;
    d.send(Message::UiKit(
        sid.clone(),
        epoch,
        Msg::Ui {
            rid: Some(rid.to_owned()),
            msg: UiKitMsg::Popover(Some(key)),
        },
    ));
    both(d, out, "u5-composer-apps", rustket_ui_size(), None);
    let epoch = d.app.servers[sid].generation;
    d.send(Message::UiKit(
        sid.clone(),
        epoch,
        Msg::Ui {
            rid: None,
            msg: UiKitMsg::Popover(None),
        },
    ));

    // Contextual bar (from a room action button) and a banner.
    answer(
        d,
        sid,
        Origin::new(OriginKind::ActionButton(ButtonContext::RoomAction)).rid(Some(rid)),
        bar_open(),
    );
    d.settle(Duration::from_millis(500)).await;
    assert_eq!(
        d.app.servers[sid].rooms[rid].panel.kind(),
        Some(&room::panel::Kind::App("poll-bar".into())),
        "contextual_bar.open must open the app panel"
    );
    answer(
        d,
        sid,
        Origin::new(OriginKind::SlashCommand).rid(Some(rid)),
        banner_open(),
    );
    d.settle(Duration::from_millis(300)).await;
    both(d, out, "u6-bar-banner", rustket_ui_size(), None);

    // Modal with inputs and validation errors.
    answer(
        d,
        sid,
        Origin::new(OriginKind::ActionButton(ButtonContext::MessageBoxAction)).rid(Some(rid)),
        modal_open(),
    );
    answer(
        d,
        sid,
        Origin::new(OriginKind::ViewSubmit).view("poll-modal"),
        errors("poll-modal"),
    );
    d.settle(Duration::from_millis(300)).await;
    assert!(
        d.app.servers[sid]
            .workspace
            .uikit()
            .modal()
            .is_some_and(|m| m.errors.len() == 2),
        "errors must reach the modal"
    );
    both(d, out, "u7-modal-errors", Size::new(1280.0, 1100.0), None);
    // Date text being typed (plain border) and text that cannot become a
    // date (error border).
    let closes = SurfaceKey::Modal("poll-modal".into()).element("closes");
    for (text, name) in [
        ("2026-1", "u7b-modal-date-typing"),
        ("2026-13", "u7c-modal-date-invalid"),
    ] {
        let ws = &mut d.app.servers.get_mut(sid).expect("server").workspace;
        ws.uikit_mut()
            .scratch_mut()
            .texts
            .insert(closes.clone(), text.into());
        shoot_with(d, out, name, Size::new(1280.0, 1100.0), None);
    }

    // Clean up locally (nothing exists on the server).
    answer(
        d,
        sid,
        Origin::new(OriginKind::SlashCommand),
        json!({"type": "banner.close", "appId": "app1", "viewId": "maint-1"}),
    );
    let epoch = d.app.servers[sid].generation;
    d.send(Message::UiKit(
        sid.clone(),
        epoch,
        Msg::Ui {
            rid: None,
            msg: UiKitMsg::Close {
                surface: SurfaceKey::Modal("poll-modal".into()),
                cleared: true,
            },
        },
    ));
    let ws = &mut d.app.servers.get_mut(sid).expect("server").workspace;
    let _ = ws.uikit_mut().close_room_bars(rid);
    ws.uikit_mut().set_buttons(Vec::new());
    ws.apply_message_deleted(rid, "rk-uikit-blocks");
    ws.apply_message_deleted(rid, "rk-uikit-plain");
    d.send(room_msg(
        d,
        sid,
        rid,
        room::Message::Panel(room::panel::Message::Close),
    ));
    d.settle(Duration::from_millis(300)).await;
}

fn rustket_ui_size() -> Size {
    super::SIZE
}
