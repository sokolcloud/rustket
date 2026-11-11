//! Accessibility polish (phase 13): keyboard focus rings on the sidebar,
//! the status menu and a dialog, the Storage settings, and the room at
//! 1024 px with the thread panel open. Focus is set with the same widget
//! operations the app runs for Tab / F6, on a UI kept across the events.
//! `RUSTKET_SNAPSHOT_POLISH`: only these after the room shots.

use std::path::Path;
use std::time::{Duration, Instant};

use iced::advanced::renderer::Headless;
use iced::{Event, Pixels, Size, keyboard, mouse};
use iced_runtime::user_interface::{Cache, UserInterface};
use rocketchat::ServerUrl;
use rustket::message::Message;
use rustket_core::ServerId;
use rustket_core::config::Theme;
use serde_json::{Value, json};

use super::{Driver, NoClip, SIZE, write_png};

/// What to do to the UI before drawing it.
pub(super) enum Step {
    /// A widget operation task (`focus`, `focus_cycle`, `focus_region`).
    Task(iced::Task<Message>),
    Key(keyboard::key::Named),
}

fn key(named: keyboard::key::Named) -> Event {
    let k = keyboard::Key::Named(named);
    Event::Keyboard(keyboard::Event::KeyPressed {
        key: k.clone(),
        modified_key: k,
        physical_key: keyboard::key::Physical::Unidentified(
            keyboard::key::NativeCode::Unidentified,
        ),
        location: keyboard::Location::Standard,
        modifiers: keyboard::Modifiers::empty(),
        text: None,
        repeat: false,
    })
}

/// Like `Driver::shoot_at`, after running `steps` on the same UI.
pub(super) fn shoot_steps(d: &Driver, dir: &Path, name: &str, size: Size, steps: Vec<Step>) {
    use iced::futures::{FutureExt as _, StreamExt as _};
    let mut renderer = iced::Renderer::Secondary(iced_tiny_skia::Renderer::new(
        rustket_ui::theme::DEFAULT_FONT,
        Pixels(rustket_ui::theme::TEXT_BODY),
    ));
    let theme = d.app.theme();
    let t = rustket_ui::tokens(&theme);
    let mut ui = UserInterface::build(d.app.view(), size, Cache::default(), &mut renderer);
    let cursor = mouse::Cursor::Available(iced::Point::new(-10.0, -10.0));
    let mut msgs = Vec::new();
    let mut events = vec![Event::Window(iced::window::Event::RedrawRequested(
        Instant::now(),
    ))];
    for step in steps {
        match step {
            Step::Task(task) => {
                let _ = ui.update(&events, cursor, &mut renderer, &mut NoClip, &mut msgs);
                events.clear();
                let Some(mut stream) = iced_runtime::task::into_stream(task) else {
                    continue;
                };
                while let Some(Some(action)) = stream.next().now_or_never() {
                    if let iced_runtime::Action::Widget(op) = action {
                        let mut current = Some(op);
                        while let Some(mut op) = current.take() {
                            ui.operate(&renderer, op.as_mut());
                            if let iced::advanced::widget::operation::Outcome::Chain(next) =
                                op.finish()
                            {
                                current = Some(next);
                            }
                        }
                    }
                }
            }
            Step::Key(k) => events.push(key(k)),
        }
    }
    events.push(Event::Window(iced::window::Event::RedrawRequested(
        Instant::now(),
    )));
    let _ = ui.update(&events, cursor, &mut renderer, &mut NoClip, &mut msgs);
    let mut focused = Focused(Vec::new());
    ui.operate(&renderer, &mut focused);
    println!("{name}: focused {:?}", focused.0);
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

/// Collects the focused widgets (id and bounds) for the log.
struct Focused(Vec<(Option<iced::advanced::widget::Id>, iced::Rectangle)>);

impl iced::advanced::widget::Operation for Focused {
    fn traverse(&mut self, operate: &mut dyn FnMut(&mut dyn iced::advanced::widget::Operation)) {
        operate(self);
    }

    fn focusable(
        &mut self,
        id: Option<&iced::advanced::widget::Id>,
        bounds: iced::Rectangle,
        state: &mut dyn iced::advanced::widget::operation::Focusable,
    ) {
        if state.is_focused() {
            self.0.push((id.cloned(), bounds));
        }
    }
}

fn both(d: &mut Driver, out: &Path, name: &str, size: Size, steps: impl Fn() -> Vec<Step>) {
    for (theme, suffix) in [(Theme::Light, "light"), (Theme::Dark, "dark")] {
        d.app.config.ui.theme = theme;
        shoot_steps(d, out, &format!("{name}-{suffix}"), size, steps());
    }
}

pub async fn polish_shots(
    d: &mut Driver,
    server: &ServerUrl,
    out: &Path,
    sid: &ServerId,
    rid: &str,
    user: &(String, String, String),
) {
    use iced::keyboard::key::Named;
    use rustket::workspace::sidebar::SEARCH_INPUT_ID;
    use rustket_ui::widget::focus_cycle;
    let focus = |id: &'static str| Step::Task(iced::widget::operation::focus(id));

    // Sidebar: Tab from the search field lands on the first room row; the
    // next Tab on the second.
    both(d, out, "polish-focus-sidebar", SIZE, || {
        vec![
            focus(SEARCH_INPUT_ID),
            Step::Task(focus_cycle(true)),
            Step::Task(focus_cycle(true)),
        ]
    });
    // F6 from nowhere: the sidebar search; again: the next region with
    // something focusable (timeline, else the composer).
    let f6 = || {
        Step::Task(rustket_ui::widget::focus_region(
            rustket::focus::regions(Some((rid, None))),
            true,
        ))
    };
    both(d, out, "polish-f6-once", SIZE, || vec![f6()]);
    both(d, out, "polish-f6-twice", SIZE, || vec![f6(), f6()]);

    // Status menu: opened, Down twice highlights the second status.
    {
        use rustket::workspace::status_menu::Message as Sm;
        d.send(Message::StatusMenu(sid.clone(), Sm::Toggle(true)));
        d.settle(Duration::from_millis(300)).await;
        both(d, out, "polish-focus-status-menu", SIZE, || {
            vec![Step::Key(Named::ArrowDown), Step::Key(Named::ArrowDown)]
        });
        d.send(Message::StatusMenu(sid.clone(), Sm::Toggle(false)));
    }

    // Create-room dialog: focus stays inside it; Shift+Tab from nothing
    // lands on its last enabled button.
    d.send(Message::Sidebar(
        sid.clone(),
        rustket::workspace::sidebar::Message::Create,
    ));
    d.settle(Duration::from_millis(300)).await;
    both(d, out, "polish-focus-dialog", SIZE, || {
        vec![Step::Task(focus_cycle(false))]
    });
    d.send(Message::CloseModal);

    // Settings → Storage with the cache limit (scrolled to; the Sidebar
    // card above it is tall).
    d.send(Message::Rail(rustket::workspace::rail::Message::Settings));
    d.settle(Duration::from_millis(500)).await;
    let tall = Size::new(SIZE.width, 2000.0);
    both(d, out, "polish-settings-storage", tall, || {
        vec![Step::Task(rustket::screens::settings::reveal_storage::<
            Message,
        >())]
    });

    // Settings → Sidebar card with a group taking channels and private
    // groups (their sections dimmed), a sort override and the group editor
    // open; en and ru. Reset afterwards so later shots keep the default.
    {
        use rustket::screens::settings::SidebarMessage as Sm;
        use rustket_core::config::{GroupType, SectionId, SidebarSort};
        use rustket_core::room_list::SectionKind;
        use rustket_core::sidebar_layout::LayoutEdit;
        let msgs = [
            Sm::GroupNew,
            Sm::GroupName("Work".into()),
            Sm::GroupType(GroupType::Channels, true),
            Sm::GroupType(GroupType::PrivateGroups, true),
            Sm::GroupSave,
            Sm::Layout(LayoutEdit::SetSort {
                id: SectionId::Builtin(SectionKind::DirectMessages),
                sort: Some(SidebarSort::Alphabetical),
            }),
            Sm::GroupNew,
            Sm::GroupName("Teams and talks".into()),
            Sm::GroupType(GroupType::Teams, true),
            Sm::GroupType(GroupType::Channels, true),
        ];
        for m in msgs {
            d.send(Message::Settings(
                rustket::screens::settings::Message::Sidebar(m),
            ));
        }
        d.settle(Duration::from_millis(300)).await;
        let reveal = || {
            vec![Step::Task(rustket::screens::settings::reveal_sidebar::<
                Message,
            >())]
        };
        both(d, out, "polish-settings-sidebar", tall, reveal);
        rust_i18n::set_locale("ru");
        both(d, out, "polish-settings-sidebar-ru", tall, reveal);
        rust_i18n::set_locale("en");
        for m in [Sm::GroupCancel, Sm::Reset, Sm::ResetConfirm] {
            d.send(Message::Settings(
                rustket::screens::settings::Message::Sidebar(m),
            ));
        }
    }
    d.send(Message::Settings(
        rustket::screens::settings::Message::Close,
    ));

    // 1024 px with the thread panel open: room title and composer
    // placeholder must stay readable.
    let client = super::rest(server).await;
    client
        .login_password(&user.1, &user.2, None)
        .await
        .expect("user login");
    let v: Value = client
        .post(
            "chat.postMessage",
            &json!({"roomId": rid, "text": "Thread starter for the narrow layout"}),
        )
        .await
        .expect("post");
    let tmid = v["message"]["_id"].as_str().expect("mid").to_owned();
    let _: Value = client
        .post(
            "chat.postMessage",
            &json!({"roomId": rid, "tmid": tmid, "text": "A reply in the thread"}),
        )
        .await
        .expect("reply");
    let _ = client.logout().await;
    let t2 = tmid.clone();
    let r2 = rid.to_owned();
    let s2 = sid.clone();
    d.pump_until("thread parent", Duration::from_secs(15), move |a| {
        a.servers[&s2]
            .workspace
            .timeline(&r2)
            .is_some_and(|t| t.get(&t2).is_some())
    })
    .await;
    let epoch = d.app.servers[sid].generation;
    d.send(Message::Room(
        sid.clone(),
        epoch,
        rid.to_owned(),
        rustket::room::Message::Panel(rustket::room::panel::Message::Open(
            rustket::room::panel::Kind::Thread(tmid),
        )),
    ));
    d.settle(Duration::from_secs(2)).await;
    let narrow = Size::new(1024.0, SIZE.height);
    both(d, out, "polish-1024-thread", narrow, Vec::new);
    let epoch = d.app.servers[sid].generation;
    d.send(Message::Room(
        sid.clone(),
        epoch,
        rid.to_owned(),
        rustket::room::Message::Panel(rustket::room::panel::Message::Close),
    ));

    // A timeline longer than the window: F6 from the sidebar lands on the
    // newest messages on screen (the timeline does not scroll).
    let client = super::rest(server).await;
    client
        .login_password(&user.1, &user.2, None)
        .await
        .expect("user login");
    let mut last = String::new();
    for i in 0..30 {
        let v: Value = client
            .post(
                "chat.postMessage",
                &json!({"roomId": rid, "text": format!("Filler message {i} for a long timeline")}),
            )
            .await
            .expect("post");
        last = v["message"]["_id"].as_str().expect("mid").to_owned();
    }
    let _ = client.logout().await;
    let r2 = rid.to_owned();
    let s2 = sid.clone();
    d.pump_until("long timeline", Duration::from_secs(15), move |a| {
        a.servers[&s2]
            .workspace
            .timeline(&r2)
            .is_some_and(|t| t.get(&last).is_some())
    })
    .await;
    d.settle(Duration::from_secs(1)).await;
    both(d, out, "polish-f6-long-timeline", SIZE, || {
        vec![focus(SEARCH_INPUT_ID), f6()]
    });
}
