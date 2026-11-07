//! Rich timeline for the snapshot example: a throwaway channel with every
//! markdown construct, code, quotes, an uploaded image and file, reactions,
//! a thread, a discussion, edits, pins and system messages; then tall
//! light/dark shots, the hover toolbar, the lightbox, the report dialog and a
//! failed outgoing message.

use std::path::Path;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use iced::advanced::renderer::Headless as _;
use iced::{Pixels, Size, mouse};
use iced_runtime::user_interface::{Cache, UserInterface};

use rocketchat::RestClient;
use rocketchat::models::{Message as RcMessage, UserRef};
use rocketchat::rest::UploadRequest;
use rustket::message::Message;
use rustket::room;
use rustket::room::render::RenderMsg;
use rustket::workspace::sidebar;
use rustket_core::ServerId;
use serde_json::{Value, json};

use super::{Driver, Ledger};

/// The rich channel's name and rooms deleted with `rooms.delete`
/// (discussions), kept here so `snapshot.rs` only calls in.
static STATE: Mutex<(String, Vec<String>)> = Mutex::new((String::new(), Vec::new()));

/// Deletes the discussions created by [`setup`] (the channel itself is in
/// the shared ledger).
pub async fn teardown(server: &rocketchat::ServerUrl) {
    let rooms = std::mem::take(&mut STATE.lock().expect("state").1);
    if rooms.is_empty() {
        return;
    }
    let admin = super::admin(server).await;
    for rid in rooms {
        match admin.rooms_delete(&rid).await {
            Ok(()) => println!("deleted room {rid}"),
            Err(e) => eprintln!("rooms.delete {rid}: {e}"),
        }
    }
}

/// Renders at `size`, optionally with the mouse at `hover`.
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
    let mut msgs = Vec::new();
    let mut events = vec![iced::Event::Window(iced::window::Event::RedrawRequested(
        Instant::now(),
    ))];
    if hover.is_some() {
        events.push(iced::Event::Mouse(mouse::Event::CursorMoved {
            position: point,
        }));
    }
    let _ = ui.update(
        &events,
        cursor,
        &mut renderer,
        &mut super::NoClip,
        &mut msgs,
    );
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

const TALL: iced::Size = iced::Size::new(1280.0, 3000.0);

async fn post(c: &RestClient, rid: &str, body: Value) -> String {
    let mut body = body;
    body["roomId"] = json!(rid);
    let v: Value = c
        .post("chat.postMessage", &body)
        .await
        .unwrap_or_else(|e| panic!("post {body}: {e}"));
    v["message"]["_id"].as_str().expect("message id").to_owned()
}

async fn text(c: &RestClient, rid: &str, text: &str) -> String {
    post(c, rid, json!({ "text": text })).await
}

/// A small gradient PNG.
fn png_bytes(w: u32, h: u32) -> Vec<u8> {
    let mut data = Vec::with_capacity((w * h * 4) as usize);
    for y in 0..h {
        for x in 0..w {
            let r = (x * 255 / w) as u8;
            let g = (y * 255 / h) as u8;
            data.extend_from_slice(&[r, g, 200, 255]);
        }
    }
    let mut out = Vec::new();
    {
        let mut enc = png::Encoder::new(&mut out, w, h);
        enc.set_color(png::ColorType::Rgba);
        enc.set_depth(png::BitDepth::Eight);
        let mut writer = enc.write_header().expect("png header");
        writer.write_image_data(&data).expect("png data");
    }
    out
}

pub async fn setup(
    admin: &RestClient,
    user: &RestClient,
    other: &RestClient,
    ledger: &Ledger,
    suffix: &str,
    [user_name, other_name, tfa_name]: [&str; 3],
) {
    let name = format!("rk-snap-rich-{suffix}");
    let name = name.as_str();
    STATE.lock().expect("state").0 = name.to_owned();
    let v: Value = admin
        .post(
            "channels.create",
            &json!({"name": name, "members": [user_name, other_name]}),
        )
        .await
        .expect("channels.create");
    let rid = v["channel"]["_id"].as_str().expect("rid").to_owned();
    ledger.lock().expect("ledger").rooms.push(rid.clone());
    let _: Value = admin
        .post(
            "channels.setTopic",
            &json!({"roomId": rid, "topic": "Rendering showcase"}),
        )
        .await
        .expect("topic");
    let _: Value = admin
        .post(
            "channels.invite",
            &json!({"roomId": rid, "username": tfa_name}),
        )
        .await
        .expect("invite");

    let first = text(user, &rid, "Morning! Posting the release checklist below.").await;
    text(
        other,
        &rid,
        "# Release 2.4\n## Highlights\nThis has **bold**, _italic_, ~strike~, `inline code`, a [link](https://rust-lang.org) and *__nested bold italic__*.",
    )
    .await;
    text(
        other,
        &rid,
        &format!("@{user_name} please check, and @all heads up. See #{name} and https://www.rust-lang.org/learn :tada: 🚀"),
    )
    .await;
    text(
        other,
        &rid,
        "```rust\nfn main() {\n    let xs: Vec<u64> = (1..=10).map(|x| x * x).collect();\n    println!(\"sum of squares = {}  // a deliberately long line to show horizontal scrolling of code blocks\", xs.iter().sum::<u64>());\n}\n```",
    )
    .await;
    text(
        other,
        &rid,
        "> Quoted wisdom: measure twice, cut once.\n> Second line of the quote.",
    )
    .await;
    text(
        other,
        &rid,
        "Todo:\n- [x] write the changelog\n- [ ] tag the release\n\nSteps:\n1. build\n2. test\n3. ship\n\n- bullet one\n- bullet two",
    )
    .await;
    text(
        other,
        &rid,
        "| Platform | Status |\n|:---|---:|\n| Linux | ✅ |\n| macOS | ⏳ |",
    )
    .await;
    text(
        other,
        &rid,
        "Spoiler: ||the cake is a lie|| and timestamp <t:1767225600:f> and math \\(e^{i\\pi}+1=0\\)\n\n---\nafter the rule",
    )
    .await;
    text(user, &rid, "🎉🎉🎉").await;
    // Upload: image and file (with descriptions).
    let mut img = UploadRequest::new("gradient.png", "image/png", png_bytes(480, 300));
    img.description = Some("A **gradient** test image".into());
    other.upload_file(&rid, img).await.expect("upload image");
    let mut file = UploadRequest::new(
        "release-notes.txt",
        "text/plain",
        "Release notes\n\n- faster\n- smaller\n"
            .repeat(40)
            .into_bytes(),
    );
    file.description = Some("Release notes attached".into());
    other.upload_file(&rid, file).await.expect("upload file");
    // Integration-style message: attachment with colour bar and fields
    // (alias/emoji avatars need `message-impersonate`, which admins lack).
    post(
        admin,
        &rid,
        json!({
            "text": "Deployment finished",
            "attachments": [{
                "color": "#158D65",
                "title": "staging-42",
                "title_link": "https://example.com/deploys/42",
                "pretext": "Pipeline for <https://example.com/mr/7|MR !7> finished",
                "text": "All checks **passed**, see `make test` and [the log](https://example.com/log)",
                "fields": [
                    {"short": true, "title": "Duration", "value": "3m 12s"},
                    {"short": true, "title": "Commit", "value": "_a1b2c3d_"}
                ]
            }]
        }),
    )
    .await;
    // Reactions.
    let reacted = text(other, &rid, "Who is in for lunch?").await;
    for (c, e) in [
        (other, ":thumbsup:"),
        (user, ":thumbsup:"),
        (user, ":pizza:"),
    ] {
        c.post_ok("chat.react", &json!({"messageId": reacted, "emoji": e}))
            .await
            .expect("react");
    }
    // Thread with replies.
    let parent = text(other, &rid, "Thread: which date works for the retro?").await;
    for (c, t) in [(user, "Thursday works for me"), (other, "Thursday it is")] {
        post(c, &rid, json!({"text": t, "tmid": parent})).await;
    }
    // Discussion.
    let disc = other
        .rooms_create_discussion(&rid, "Retro agenda", Some("Let's collect topics"), &[])
        .await
        .expect("discussion");
    STATE.lock().expect("state").1.push(disc.id.clone());
    // Edit, pin, star, quote.
    let edited = text(other, &rid, "Deploy at 15:00").await;
    let _: Value = other
        .post(
            "chat.update",
            &json!({"roomId": rid, "msgId": edited, "text": "Deploy at **16:00** (moved)"}),
        )
        .await
        .expect("edit");
    let _: Value = admin
        .post("chat.pinMessage", &json!({"messageId": first}))
        .await
        .expect("pin");
    user.post_ok("chat.starMessage", &json!({"messageId": reacted}))
        .await
        .expect("star");
    let link = format!(
        "{}/channel/{name}?msg={first}",
        user.server().as_str().trim_end_matches('/')
    );
    text(other, &rid, &format!("[ ]({link}) Agreed with this one")).await;
    let _: Value = admin
        .post(
            "channels.setDescription",
            &json!({"roomId": rid, "description": "Where we test rendering"}),
        )
        .await
        .expect("description");
    text(other, &rid, "Last message from the peer.").await;
}

fn rich_rid(d: &Driver, sid: &ServerId, name: &str) -> String {
    d.app.servers[sid]
        .workspace
        .rooms()
        .values()
        .find(|e| e.name() == name)
        .map(|e| e.rid().to_owned())
        .expect("rich channel in sidebar")
}

fn timeline_msg(d: &Driver, sid: &ServerId, rid: &str, m: room::timeline::Message) -> Message {
    let epoch = d.app.servers[sid].generation;
    Message::Room(
        sid.clone(),
        epoch,
        rid.to_owned(),
        room::Message::Timeline(m),
    )
}

/// Renders, lets the wanted images load, renders again.
async fn shoot_loaded(d: &mut Driver, out: &Path, name: &str, size: iced::Size) {
    for _ in 0..3 {
        shoot_with(d, out, name, size, None);
        d.send(Message::FlushImages);
        d.settle(Duration::from_millis(1500)).await;
    }
    shoot_with(d, out, name, size, None);
}

pub async fn shoot(d: &mut Driver, out: &Path, sid: &ServerId) {
    let name = STATE.lock().expect("state").0.clone();
    let rid = rich_rid(d, sid, &name);
    d.send(Message::Sidebar(
        sid.clone(),
        sidebar::Message::Open(rid.clone()),
    ));
    d.pump_until("rich room open", Duration::from_secs(20), |a| {
        a.servers[sid]
            .rooms
            .get(&rid)
            .is_some_and(|r| r.loaded && r.has_watch())
    })
    .await;
    d.settle(Duration::from_secs(2)).await;
    d.app.config.ui.theme = rustket_core::config::Theme::Light;
    shoot_loaded(d, out, "10-timeline-light", TALL).await;
    d.app.config.ui.theme = rustket_core::config::Theme::Dark;
    shoot_loaded(d, out, "11-timeline-dark", TALL).await;
    d.app.config.ui.theme = rustket_core::config::Theme::Light;

    // Hover toolbar over the last message (bottom of the timeline).
    shoot_with(
        d,
        out,
        "12-timeline-hover",
        super::SIZE,
        Some(iced::Point::new(800.0, 650.0)),
    );

    // Lightbox of the uploaded image.
    let image = d.app.servers[sid]
        .workspace
        .timeline(&rid)
        .and_then(|t| {
            t.messages().find_map(|m| {
                m.attachments
                    .as_ref()?
                    .iter()
                    .find(|a| a.image_url.is_some())
                    .map(|a| (m.id.clone(), a.clone()))
            })
        })
        .expect("image message");
    let server = d.app.servers[sid].url.clone();
    let url = server.absolute(image.1.title_link.as_deref().unwrap_or_default());
    let m = timeline_msg(
        d,
        sid,
        &rid,
        room::timeline::Message::Render(RenderMsg::OpenImage(room::render::ImageRef {
            mid: image.0.clone(),
            url,
            title: image.1.title.clone().unwrap_or_default(),
            mime: image.1.image_type.clone(),
        })),
    );
    d.send(m);
    shoot_loaded(d, out, "13-lightbox", super::SIZE).await;
    let m = timeline_msg(d, sid, &rid, room::timeline::Message::CloseDialog);
    d.send(m);

    // Report dialog on a peer message.
    let ws = &d.app.servers[sid].workspace;
    let me = ws.user_id().unwrap_or_default().to_owned();
    let target = ws
        .timeline(&rid)
        .and_then(|t| {
            t.messages()
                .rev()
                .find(|m| m.t.is_none() && m.u.id != me && m.alias.is_none())
                .map(|m| m.id.clone())
        })
        .expect("peer message");
    let m = timeline_msg(
        d,
        sid,
        &rid,
        room::timeline::Message::Act(target, room::timeline::Act::Report),
    );
    d.send(m);
    let m = timeline_msg(
        d,
        sid,
        &rid,
        room::timeline::Message::ReportText("Spam link".into()),
    );
    d.send(m);
    d.app.config.ui.theme = rustket_core::config::Theme::Dark;
    d.shoot(out, "14-report-dialog-dark");
    let m = timeline_msg(d, sid, &rid, room::timeline::Message::CloseDialog);
    d.send(m);
    d.app.config.ui.theme = rustket_core::config::Theme::Light;

    // A failed and a pending outgoing message.
    {
        let s = d.app.servers.get_mut(sid).expect("server");
        let me = s.workspace.me().cloned().expect("me");
        let mk = |id: &str, text: &str| RcMessage {
            id: id.into(),
            rid: rid.clone(),
            msg: text.into(),
            ts: rocketchat::RcDate::from_millis(chrono::Utc::now().timestamp_millis()),
            u: UserRef {
                id: me.id.clone(),
                username: me.username.clone(),
                name: me.name.clone(),
            },
            ..Default::default()
        };
        s.workspace
            .add_pending(mk("rk-failed-1", "This one could not be sent"));
        s.workspace
            .mark_pending_failed(&rid, None, "rk-failed-1", "offline");
        s.workspace
            .add_pending(mk("rk-pending-1", "And this one is still sending"));
    }
    d.settle(Duration::from_millis(300)).await;
    d.shoot(out, "15-outbox");
    {
        let s = d.app.servers.get_mut(sid).expect("server");
        s.workspace.discard_pending(&rid, None, "rk-failed-1");
        s.workspace.discard_pending(&rid, None, "rk-pending-1");
    }
}
