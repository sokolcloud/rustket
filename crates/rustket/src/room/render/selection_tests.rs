//! Text selection in rendered message lists: the selection lives in the
//! widget tree of each message body (one scope per message), so it must
//! survive what rebuilds rows around it.

#![allow(clippy::unwrap_used)]

use iced::advanced::widget::Tree;
use iced::{Pixels, Size};
use iced_runtime::user_interface::{Cache, UserInterface};
use rocketchat::models::{
    Message as RcMessage, Room, RoomType, Subscription as Sub, User, UserRef,
};
use rocketchat::{RcDate, ServerUrl};
use rustket_core::sync::InitialData;
use rustket_core::workspace::Changes;
use rustket_core::{ServerId, Workspace};
use rustket_ui::widget::selectable::{Copied, SelectAll, scope};

use crate::images::Images;
use crate::room::timeline::Timeline;
use crate::room::{UpdateCtx, ViewCtx};
use rustket_core::config::UiPrefs;

fn workspace() -> Workspace {
    let mut ws = Workspace::new();
    ws.apply_initial(InitialData {
        me: User {
            id: "me".into(),
            username: Some("me".into()),
            ..Default::default()
        },
        subscriptions: vec![Sub {
            id: "s1".into(),
            rid: "R1".into(),
            name: "general".into(),
            t: RoomType::Channel,
            u: UserRef {
                id: "me".into(),
                username: Some("me".into()),
                name: None,
            },
            open: true,
            ..Default::default()
        }],
        rooms: vec![Room {
            id: "R1".into(),
            t: RoomType::Channel,
            name: Some("general".into()),
            ..Default::default()
        }],
        ..Default::default()
    });
    ws
}

fn message(id: &str, text: &str, ms: i64) -> RcMessage {
    RcMessage {
        id: id.into(),
        rid: "R1".into(),
        msg: text.into(),
        ts: RcDate::from_millis(ms),
        u: UserRef {
            id: "bob".into(),
            username: Some("bob".into()),
            name: None,
        },
        ..Default::default()
    }
}

/// A new message arriving (row rebuilt around the kept ones by the keyed,
/// `lazy` list) keeps the selection of the messages already shown, and every
/// scope keeps its own state object.
#[test]
fn selection_survives_a_new_message() {
    let mut ws = workspace();
    let sid = ServerId("s".into());
    let url = ServerUrl::parse("http://chat.test").unwrap();
    let images = Images::new(None);
    let prefs = UiPrefs::default();
    ws.open_room("R1");
    let cursor = ws.begin_load_older("R1").unwrap();
    ws.finish_load_older(
        "R1",
        cursor,
        Ok(vec![
            message("b", "second message", 2_000_000),
            message("a", "first message", 1_000_000),
        ]),
    );
    let mut tl = Timeline::new("R1");
    fn view_ctx<'a>(
        ws: &'a Workspace,
        sid: &'a ServerId,
        url: &'a ServerUrl,
        images: &'a Images,
        prefs: &'a UiPrefs,
    ) -> ViewCtx<'a> {
        ViewCtx {
            server_id: sid,
            server: url,
            rid: "R1",
            workspace: ws,
            images,
            prefs,
            connected: true,
            panel: None,
            now: std::time::Instant::now(),
        }
    }
    let mut renderer = iced::Renderer::Secondary(iced_tiny_skia::Renderer::new(
        rustket_ui::theme::font_regular(),
        Pixels(rustket_ui::theme::TEXT_BODY),
    ));
    let size = Size::new(900.0, 700.0);
    let copied = |ui: &mut UserInterface<'_, _, _, _>, r: &iced::Renderer| {
        let mut c = Copied::default();
        ui.operate(r, &mut c);
        c.texts
    };

    let ctx = view_ctx(&ws, &sid, &url, &images, &prefs);
    let mut ui = UserInterface::build(tl.view(&ctx), size, Cache::default(), &mut renderer);
    ui.operate(&renderer, &mut SelectAll);
    assert_eq!(
        copied(&mut ui, &renderer),
        ["first message", "second message"]
    );
    let cache = ui.into_cache();

    // The tree before the new message, to compare state objects.
    let scope_tag = {
        let e: iced::Element<'_, ()> = scope(iced::widget::space()).into();
        e.as_widget().tag()
    };
    let mut probe = Tree::new(tl.view(&ctx).as_widget());

    ws.apply_message(message("c", "third message", 3_000_000));
    let mut changes = Changes::default();
    changes.timelines.insert("R1".into());
    {
        let mut uctx = UpdateCtx {
            server_id: &sid,
            server: &url,
            rid: "R1",
            workspace: &mut ws,
            session: None,
            images: &images,
            prefs: &prefs,
        };
        let _ = tl.on_changes(&changes, &mut uctx);
    }
    let ctx = view_ctx(&ws, &sid, &url, &images, &prefs);
    let view = tl.view(&ctx);
    crate::tree_probe::assert_kept(&mut probe, &view, scope_tag, "new message");
    assert_eq!(crate::tree_probe::states(&probe, scope_tag).len(), 3);

    let mut ui = UserInterface::build(view, size, cache, &mut renderer);
    assert_eq!(
        copied(&mut ui, &renderer),
        ["first message", "second message", ""],
        "the selections stayed, the new message has none"
    );
}
