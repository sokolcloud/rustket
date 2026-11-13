//! The configurable sidebar: a user group ("Work") taking channels, private
//! groups, discussions and DMs in a custom order; the Threads section in
//! Unread and kept mode (unread, mention and kept rows); the context menu of
//! the Threads header; a header drag in progress; the Sidebar settings card
//! (en, ru). Rooms are throwaway (in the ledger); the layout is reset after.
//! `RUSTKET_SNAPSHOT_SIDEBAR`: only these after the shell shots.

use std::path::Path;
use std::time::{Duration, Instant};

use iced::advanced::renderer::Headless;
use iced::advanced::widget::{Id, Operation};
use iced::{Event, Pixels, Point, Rectangle, Size, Vector, mouse};
use iced_runtime::user_interface::{Cache, UserInterface};
use rocketchat::ServerUrl;
use rustket::message::Message;
use rustket::workspace::sidebar::{self, HeaderAct, LIST_ID, ThreadAct};
use rustket_core::ServerId;
use rustket_core::config::{GroupType, SectionId, ThreadsMode};
use rustket_core::room_list::SectionKind;
use rustket_core::sidebar_layout::LayoutEdit;
use serde_json::{Value, json};

use super::polish::{Step, shoot_steps};
use super::{Driver, Ledger, NoClip, SIZE, write_png};

/// `sidebar.rs`: header height.
const HEADER_H: f32 = 27.0;

fn row_h(d: &Driver) -> f32 {
    match d.app.config.ui.density {
        rustket_core::config::Density::Condensed => 32.0,
        rustket_core::config::Density::Extended => 56.0,
    }
}

/// Top of the room list's content (screen coordinates).
fn list_top(d: &Driver) -> f32 {
    struct ListTop(Option<f32>);
    impl Operation for ListTop {
        fn traverse(&mut self, operate: &mut dyn FnMut(&mut dyn Operation)) {
            operate(self);
        }
        fn scrollable(
            &mut self,
            id: Option<&Id>,
            _bounds: Rectangle,
            content: Rectangle,
            _translation: Vector,
            _state: &mut dyn iced::advanced::widget::operation::Scrollable,
        ) {
            if id == Some(&Id::new(LIST_ID)) {
                self.0 = Some(content.y);
            }
        }
    }
    let mut renderer = renderer();
    let mut ui = UserInterface::build(d.app.view(), SIZE, Cache::default(), &mut renderer);
    let mut probe = ListTop(None);
    ui.operate(&renderer, &mut probe);
    probe.0.expect("sidebar list")
}

/// Screen y of the centre of `id`'s header and the list's x centre.
fn header_at(d: &Driver, sid: &ServerId, id: &SectionId) -> Point {
    let top = list_top(d);
    let rh = row_h(d);
    let collapsed = &d.app.ui_state.collapsed_sections;
    let mut y = top;
    for s in d.app.servers[sid]
        .sidebar
        .sections
        .iter()
        .filter(|s| !s.is(SectionKind::Omnichannel))
    {
        if &s.id == id {
            return Point::new(200.0, y + HEADER_H / 2.0);
        }
        y += HEADER_H;
        if !collapsed.contains(s.id.collapse_key().as_ref()) {
            y += s.len() as f32 * rh;
        }
    }
    panic!("no header {id:?}");
}

fn renderer() -> iced::Renderer {
    iced::Renderer::Secondary(iced_tiny_skia::Renderer::new(
        rustket_ui::theme::font_regular(),
        Pixels(rustket_ui::theme::TEXT_BODY),
    ))
}

/// Feeds mouse `events` (each with the cursor at its point) to one UI and
/// draws it with the cursor at the last point.
fn shoot_mouse(d: &Driver, dir: &Path, name: &str, events: &[(Point, mouse::Event)]) {
    let size = SIZE;
    let mut renderer = renderer();
    let theme = d.app.theme();
    let t = rustket_ui::tokens(&theme);
    let mut ui = UserInterface::build(d.app.view(), size, Cache::default(), &mut renderer);
    let mut msgs = Vec::new();
    let redraw = || Event::Window(iced::window::Event::RedrawRequested(Instant::now()));
    let mut cursor = mouse::Cursor::Available(Point::new(-10.0, -10.0));
    let _ = ui.update(&[redraw()], cursor, &mut renderer, &mut NoClip, &mut msgs);
    for (at, e) in events {
        cursor = mouse::Cursor::Available(*at);
        let _ = ui.update(
            &[Event::Mouse(*e)],
            cursor,
            &mut renderer,
            &mut NoClip,
            &mut msgs,
        );
    }
    let _ = ui.update(&[redraw()], cursor, &mut renderer, &mut NoClip, &mut msgs);
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
    write_png(&path, size.width as u32, size.height as u32, &rgba);
    println!("wrote {}", path.display());
}

fn themed(d: &mut Driver, f: impl Fn(&mut Driver, &str)) {
    use rustket_core::config::Theme;
    for (theme, suffix) in [(Theme::Light, "light"), (Theme::Dark, "dark")] {
        d.app.config.ui.theme = theme;
        f(d, suffix);
    }
}

fn edit(d: &mut Driver, sid: &ServerId, e: LayoutEdit) {
    d.send(Message::Sidebar(
        sid.clone(),
        sidebar::Message::Header(HeaderAct::Layout(e)),
    ));
}

/// Rooms and threads: a channel with three threads started by the user (the
/// peer replies to two, once mentioning the user; the third only has the
/// user's own reply), a private group and a discussion. Returns the tmids.
async fn seed(
    server: &ServerUrl,
    user: &(String, String, String),
    ledger: &Ledger,
) -> (String, [String; 3]) {
    let sfx = user.1.trim_start_matches("rk-snap-").to_owned();
    let peer_name = format!("rk-peer-{sfx}");
    let admin = super::admin(server).await;
    let mut rid_of = Vec::new();
    for (ep, key, name) in [
        (
            "channels.create",
            "channel",
            format!("rk-snap-releases-{sfx}"),
        ),
        ("groups.create", "group", format!("rk-snap-infra-{sfx}")),
    ] {
        let v: Value = admin
            .post(ep, &json!({"name": name, "members": [user.1, peer_name]}))
            .await
            .expect(ep);
        let rid = v[key]["_id"].as_str().expect("rid").to_owned();
        ledger.lock().expect("ledger").rooms.push(rid.clone());
        rid_of.push(rid);
    }
    let _ = admin.logout().await;
    let (chan, group) = (rid_of[0].clone(), rid_of[1].clone());

    let me = super::rest(server).await;
    me.login_password(&user.1, &user.2, None)
        .await
        .expect("user login");
    let peer = super::rest(server).await;
    peer.login_password(&peer_name, &user.2, None)
        .await
        .expect("peer login");
    let post = |c: &rocketchat::RestClient, body: Value| {
        let c = c.clone();
        async move {
            let v: Value = c.post("chat.postMessage", &body).await.expect("post");
            v["message"]["_id"].as_str().expect("mid").to_owned()
        }
    };
    let disc: Value = peer
        .post(
            "rooms.createDiscussion",
            &json!({"prid": chan, "t_name": "Q4 roadmap", "users": [user.1]}),
        )
        .await
        .expect("discussion");
    let drid = disc["discussion"]["_id"].as_str().expect("drid").to_owned();
    ledger.lock().expect("ledger").rooms.push(drid.clone());
    post(
        &peer,
        json!({"roomId": drid, "text": "First draft of the roadmap is up."}),
    )
    .await;

    let t1 = post(
        &me,
        json!({"roomId": chan, "text": "Release checklist for 0.3"}),
    )
    .await;
    let t2 = post(
        &me,
        json!({"roomId": group, "text": "Flaky CI runner on Windows"}),
    )
    .await;
    let t3 = post(
        &me,
        json!({"roomId": chan, "text": "Feedback on the new sidebar layout"}),
    )
    .await;
    post(
        &me,
        json!({"roomId": chan, "tmid": t3, "text": "Collecting notes here."}),
    )
    .await;
    post(
        &peer,
        json!({"roomId": chan, "tmid": t1, "text": "Changelog is updated."}),
    )
    .await;
    post(
        &peer,
        json!({"roomId": group, "tmid": t2, "text": format!("@{} can you check the runner logs?", user.1)}),
    )
    .await;
    post(
        &peer,
        json!({"roomId": group, "text": "Standup moved to 10:30."}),
    )
    .await;
    me.post_ok(
        "rooms.favorite",
        &json!({"roomId": group, "favorite": true}),
    )
    .await
    .expect("favorite");
    let _ = me.logout().await;
    let _ = peer.logout().await;
    (chan, [t1, t2, t3])
}

pub async fn sidebar_shots(
    d: &mut Driver,
    server: &ServerUrl,
    out: &Path,
    sid: &ServerId,
    user: &(String, String, String),
    ledger: &Ledger,
) {
    let (chan, [t1, t2, t3]) = seed(server, user, ledger).await;
    let (s2, c2) = (sid.clone(), chan.clone());
    d.pump_until("seeded rooms", Duration::from_secs(20), move |a| {
        a.servers[&s2].workspace.subscription(&c2).is_some()
    })
    .await;
    d.settle(Duration::from_secs(1)).await;

    // (1) A "Work" group of channels, private groups, discussions and DMs,
    // first in the list; Favorites after it.
    edit(
        d,
        sid,
        LayoutEdit::AddGroup {
            name: "Work".into(),
            types: vec![
                GroupType::Channels,
                GroupType::PrivateGroups,
                GroupType::Discussions,
                GroupType::DirectMessages,
            ],
        },
    );
    let gid = d
        .app
        .config
        .ui
        .sidebar_groups
        .iter()
        .find(|g| g.name == "Work")
        .expect("group")
        .id
        .clone();
    let work = SectionId::group(gid.clone());
    let favorites = SectionId::Builtin(SectionKind::Favorites);
    let threads = SectionId::Builtin(SectionKind::Threads);
    edit(
        d,
        sid,
        LayoutEdit::MoveTo {
            id: favorites.clone(),
            anchor: work.clone(),
            after: true,
        },
    );
    d.settle(Duration::from_millis(500)).await;
    println!(
        "order: {:?}",
        d.app.servers[sid]
            .sidebar
            .sections
            .iter()
            .map(|s| (s.id.as_config_str(), s.len()))
            .collect::<Vec<_>>()
    );
    themed(d, |d, s| d.shoot(out, &format!("sidebar-work-group-{s}")));

    // (2) Threads in Unread and kept mode, between Work and Favorites; the
    // third thread kept.
    edit(d, sid, LayoutEdit::SetThreadsMode(ThreadsMode::UnreadKept));
    edit(
        d,
        sid,
        LayoutEdit::MoveTo {
            id: threads.clone(),
            anchor: work.clone(),
            after: true,
        },
    );
    d.send(Message::Sidebar(
        sid.clone(),
        sidebar::Message::ThreadAct {
            rid: chan.clone(),
            tmid: t3.clone(),
            act: ThreadAct::Keep,
        },
    ));
    let s2 = sid.clone();
    let want = [t1.clone(), t2.clone(), t3.clone()];
    d.pump_until("thread rows", Duration::from_secs(90), move |a| {
        a.servers[&s2]
            .sidebar
            .sections
            .iter()
            .find(|s| s.is(SectionKind::Threads))
            .is_some_and(|s| want.iter().all(|t| s.threads.iter().any(|r| &r.tmid == t)))
    })
    .await;
    // Parents arrive from the rate-limited loader.
    let started = Instant::now();
    let s2 = sid.clone();
    d.pump_until("thread titles", Duration::from_secs(90), move |a| {
        a.servers[&s2]
            .sidebar
            .sections
            .iter()
            .filter(|s| s.is(SectionKind::Threads))
            .flat_map(|s| &s.threads)
            .all(|r| !matches!(r.title, rustket_core::room_list::ThreadTitle::Loading))
    })
    .await;
    println!("thread titles loaded in {:?}", started.elapsed());
    d.settle(Duration::from_secs(1)).await;
    themed(d, |d, s| d.shoot(out, &format!("sidebar-threads-{s}")));

    // (3) The Threads header's context menu.
    let at = header_at(d, sid, &threads);
    let right = [
        (at, mouse::Event::CursorMoved { position: at }),
        (at, mouse::Event::ButtonPressed(mouse::Button::Right)),
        (at, mouse::Event::ButtonReleased(mouse::Button::Right)),
    ];
    themed(d, |d, s| {
        shoot_mouse(d, out, &format!("sidebar-threads-menu-{s}"), &right)
    });

    // (4) Work collapsed, Favorites dragged above Threads (not released).
    d.send(Message::Sidebar(
        sid.clone(),
        sidebar::Message::Toggle(work.clone()),
    ));
    let from = header_at(d, sid, &favorites);
    let to = Point::new(from.x + 12.0, header_at(d, sid, &threads).y - 4.0);
    let mid = Point::new(from.x + 6.0, (from.y + to.y) / 2.0);
    let drag = [
        (from, mouse::Event::CursorMoved { position: from }),
        (from, mouse::Event::ButtonPressed(mouse::Button::Left)),
        (mid, mouse::Event::CursorMoved { position: mid }),
        (to, mouse::Event::CursorMoved { position: to }),
    ];
    themed(d, |d, s| {
        shoot_mouse(d, out, &format!("sidebar-drag-{s}"), &drag)
    });
    d.send(Message::Sidebar(
        sid.clone(),
        sidebar::Message::Toggle(work.clone()),
    ));

    // (5) The Sidebar settings card with this layout, en and ru.
    d.send(Message::Sidebar(
        sid.clone(),
        sidebar::Message::Header(HeaderAct::OpenSettings),
    ));
    d.settle(Duration::from_millis(500)).await;
    let tall = Size::new(SIZE.width, 2000.0);
    let reveal = || {
        vec![Step::Task(rustket::screens::settings::reveal_sidebar::<
            Message,
        >())]
    };
    for locale in ["en", "ru"] {
        rust_i18n::set_locale(locale);
        themed(d, |d, s| {
            shoot_steps(
                d,
                out,
                &format!("sidebar-settings-{locale}-{s}"),
                tall,
                reveal(),
            )
        });
    }
    rust_i18n::set_locale("en");
    d.send(Message::Settings(
        rustket::screens::settings::Message::Close,
    ));

    // Back to the default layout for later shots.
    edit(d, sid, LayoutEdit::SetThreadsMode(ThreadsMode::Off));
    edit(d, sid, LayoutEdit::DeleteGroup { id: gid });
    edit(d, sid, LayoutEdit::Reset);
    d.send(Message::Sidebar(
        sid.clone(),
        sidebar::Message::ThreadAct {
            rid: chan,
            tmid: t3,
            act: ThreadAct::Unkeep,
        },
    ));
    d.settle(Duration::from_millis(300)).await;
}
