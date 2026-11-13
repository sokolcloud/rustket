//! Custom window title bar (client-side decorations, `UiPrefs::system_title_bar`
//! off): the workspace switcher of the left rail, the window title on a
//! draggable area, the settings gear and the window controls. Workspace
//! clicks are reported as `rail::Message`s so they take the rail's path
//! (navigation guards included). Strings: `locales/titlebar.yml`.

use std::path::Path;
use std::time::{Duration, Instant};

use iced::widget::scrollable::{Direction as ScrollDirection, Scrollbar};
use iced::widget::{Row, column, container, mouse_area, row, scrollable, space, text};
use iced::window::Direction;
use iced::{Alignment, Element, Length, Point, Subscription, mouse};
use rust_i18n::t;
use rustket_core::config::{AppConfig, CONFIG_FILE};
use rustket_ui::icon::Icon;
use rustket_ui::style;
use rustket_ui::theme::{SPACE_1, SPACE_2, TEXT_SMALL};
use rustket_ui::widget::{
    MenuEntry, MenuItem, TITLE_BAR_HEIGHT, TITLE_BAR_ITEM, WindowControlLabels, context_menu,
    divider, title_bar_button, title_bar_workspace, window_controls,
};

use super::rail::{self, RailEntry};

/// Two presses on the empty title bar within this time toggle maximize.
pub const DOUBLE_CLICK: Duration = Duration::from_millis(400);
/// Pointer travel (logical px) after a press that starts a window move.
pub const DRAG_THRESHOLD: f32 = 4.0;
/// Room for about a dozen workspaces; more scroll horizontally.
pub const WORKSPACES_MAX_WIDTH: f32 = TITLE_BAR_ITEM * 12.0;
/// Empty bar after the workspaces that always stays free to drag by.
pub const DRAG_RESERVE: f32 = 48.0;
/// Smallest window size: the window controls, the gear, a few workspaces and
/// the drag reserve fit in the title bar.
pub const MIN_WINDOW_SIZE: iced::Size = iced::Size::new(480.0, 400.0);

/// The saved `system_title_bar` preference, read without side effects like
/// `renderer::saved_hardware_acceleration`; any error yields the default.
pub fn saved_system_title_bar(dir: Option<&Path>) -> bool {
    dir.and_then(|d| std::fs::read_to_string(d.join(CONFIG_FILE)).ok())
        .and_then(|text| AppConfig::parse(&text).ok())
        .unwrap_or_default()
        .ui
        .system_title_bar
}

#[derive(Debug, Clone)]
pub enum Message {
    /// A workspace, add-workspace or settings click.
    Rail(rail::Message),
    /// Left button pressed on empty title-bar space.
    Press,
    /// Pointer moved anywhere in the window while pressed ([`drag_events`]).
    Move(Point),
    /// Left button released anywhere in the window while pressed.
    Release,
    /// A press on a workspace item that is inert under a modal.
    Inert,
    Minimize,
    ToggleMaximize,
    Close,
    /// Pressed on a resize edge or corner of the window.
    Resize(Direction),
    /// The window was resized (maximize state may have changed).
    Resized,
    Maximized(bool),
}

/// A window-manager request.
#[derive(Debug, Clone, Copy)]
pub enum WindowOp {
    Drag,
    ToggleMaximize,
    Minimize,
    Resize(Direction),
    /// Through the app's close-request path (close to tray, config flush,
    /// unsaved-changes guard).
    Close,
}

/// What the app does after [`TitleBar::update`].
#[derive(Debug, Clone)]
pub enum Outcome {
    None,
    Rail(rail::Message),
    Window(WindowOp),
    /// Read the window's maximize state (answer: `Message::Maximized`).
    QueryMaximized,
}

/// Pointer state of the empty title-bar area and the window's maximize state.
#[derive(Debug, Default)]
pub struct TitleBar {
    /// Pressed and not yet moving the window: where the pointer was first
    /// seen after the press.
    pressed: Option<Option<Point>>,
    last_press: Option<Instant>,
    pub maximized: bool,
}

impl TitleBar {
    /// Pointer moves are only listened to while a press may become a drag.
    pub fn tracking(&self) -> bool {
        self.pressed.is_some()
    }

    /// Window-wide pointer moves and the release while [`Self::tracking`]: a
    /// quick flick leaves the 40 px bar before the drag threshold is reached,
    /// and the pointer stays grabbed by the window meanwhile.
    pub fn subscription(&self) -> Subscription<Message> {
        if !self.tracking() {
            return Subscription::none();
        }
        iced::event::listen_with(|event, _status, _id| match event {
            iced::Event::Mouse(mouse::Event::CursorMoved { position }) => {
                Some(Message::Move(position))
            }
            iced::Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)) => {
                Some(Message::Release)
            }
            _ => None,
        })
    }

    pub fn update(&mut self, msg: Message, now: Instant) -> Outcome {
        match msg {
            Message::Rail(m) => Outcome::Rail(m),
            Message::Press => {
                let double = self
                    .last_press
                    .is_some_and(|t| now.saturating_duration_since(t) <= DOUBLE_CLICK);
                if double {
                    self.last_press = None;
                    self.pressed = None;
                    Outcome::Window(WindowOp::ToggleMaximize)
                } else {
                    self.last_press = Some(now);
                    self.pressed = Some(None);
                    Outcome::None
                }
            }
            Message::Move(p) => match self.pressed {
                Some(None) => {
                    self.pressed = Some(Some(p));
                    Outcome::None
                }
                Some(Some(origin)) if origin.distance(p) >= DRAG_THRESHOLD => {
                    // A move is not the first click of a double click.
                    self.pressed = None;
                    self.last_press = None;
                    Outcome::Window(WindowOp::Drag)
                }
                _ => Outcome::None,
            },
            Message::Release => {
                self.pressed = None;
                Outcome::None
            }
            Message::Inert => Outcome::None,
            Message::Minimize => Outcome::Window(WindowOp::Minimize),
            Message::ToggleMaximize => Outcome::Window(WindowOp::ToggleMaximize),
            Message::Close => Outcome::Window(WindowOp::Close),
            Message::Resize(_) if self.maximized => Outcome::None,
            Message::Resize(d) => Outcome::Window(WindowOp::Resize(d)),
            Message::Resized => Outcome::QueryMaximized,
            Message::Maximized(m) => {
                self.maximized = m;
                Outcome::None
            }
        }
    }
}

/// What the title bar shows.
pub struct View {
    pub entries: Vec<RailEntry>,
    /// Add-workspace and settings buttons (not before the first server).
    pub workspace_buttons: bool,
    pub settings_active: bool,
    pub title: String,
    pub maximized: bool,
    /// `false` while a modal covers the window: workspace, add and settings
    /// buttons are inert (their presses do nothing, not even start a drag);
    /// dragging and the window controls still work.
    pub interactive: bool,
}

pub fn view<'a>(v: View) -> Element<'a, Message> {
    let on = |m: rail::Message| v.interactive.then_some(Message::Rail(m));
    // Inert items swallow their presses instead of passing them to the bar.
    let guard = |e: Element<'a, Message>| -> Element<'a, Message> {
        let area = mouse_area(e);
        if v.interactive {
            area.into()
        } else {
            area.on_press(Message::Inert).into()
        }
    };
    let items = v.entries.into_iter().map(|e| {
        let badge = rail::badge(e.unread);
        title_bar_workspace(
            &e.avatar,
            rail::label(e.name, e.signed_out),
            badge,
            e.selected,
            e.signed_out,
            on(rail::Message::Select(e.id)),
        )
    });
    let mut list = Row::with_children(items).align_y(Alignment::Center);
    if v.workspace_buttons {
        list = list.push(
            container(title_bar_button(
                Icon::Plus,
                t!("rail.add_server"),
                false,
                on(rail::Message::Add),
            ))
            .padding(iced::Padding::ZERO.left(SPACE_1)),
        );
    }
    let workspaces = container(guard(
        scrollable(list)
            .direction(ScrollDirection::Horizontal(
                Scrollbar::new().width(3).scroller_width(3).margin(0),
            ))
            .style(style::scrollable)
            .into(),
    ))
    .padding(iced::Padding::ZERO.right(DRAG_RESERVE))
    .max_width(WORKSPACES_MAX_WIDTH + DRAG_RESERVE);
    let labels = || WindowControlLabels {
        minimize: t!("titlebar.minimize").into_owned(),
        maximize: t!("titlebar.maximize").into_owned(),
        restore: t!("titlebar.restore").into_owned(),
        close: t!("titlebar.close").into_owned(),
    };
    // The window menu (winit's system menu is a no-op on X11).
    let menu_labels = labels();
    let window_menu = vec![
        MenuEntry::from(MenuItem::new(menu_labels.minimize, Message::Minimize).icon(Icon::Minus)),
        MenuItem::new(
            if v.maximized {
                menu_labels.restore
            } else {
                menu_labels.maximize
            },
            Message::ToggleMaximize,
        )
        .icon(if v.maximized {
            Icon::Copy
        } else {
            Icon::Square
        })
        .into(),
        MenuEntry::Separator,
        MenuItem::new(menu_labels.close, Message::Close)
            .icon(Icon::X)
            .into(),
    ];
    let title = context_menu(
        container(
            text(v.title)
                .size(TEXT_SMALL)
                .style(style::text_secondary)
                .wrapping(text::Wrapping::None),
        )
        .center_x(Length::Fill)
        .center_y(Length::Fill)
        .clip(true),
        window_menu,
    );
    // Workspaces and title share what the controls leave: the workspaces
    // scroll instead of pushing the controls off a narrow window, and
    // `DRAG_RESERVE` after them stays free to move the window by.
    let middle = row![workspaces, title]
        .width(Length::Fill)
        .height(Length::Fill)
        .align_y(Alignment::Center);
    let gear: Element<'a, Message> = if v.workspace_buttons {
        guard(title_bar_button(
            Icon::Settings,
            t!("rail.settings"),
            v.settings_active,
            on(rail::Message::Settings),
        ))
    } else {
        guard(space().into())
    };
    let controls = window_controls(
        v.maximized,
        labels(),
        Message::Minimize,
        Message::ToggleMaximize,
        Message::Close,
    );
    let bar = row![
        space().width(SPACE_1),
        middle,
        gear,
        space().width(SPACE_2),
        controls,
    ]
    .height(TITLE_BAR_HEIGHT)
    .align_y(Alignment::Center);
    // Buttons capture their presses; the rest of the bar moves the window.
    let area = mouse_area(bar).on_press(Message::Press);
    container(column![area, divider()])
        .width(Length::Fill)
        .style(style::container_rail)
        .into()
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use rustket_core::ServerId;
    use rustket_ui::widget::AvatarSource;

    fn op(o: Outcome) -> Option<&'static str> {
        match o {
            Outcome::Window(WindowOp::Drag) => Some("drag"),
            Outcome::Window(WindowOp::ToggleMaximize) => Some("max"),
            Outcome::Window(WindowOp::Minimize) => Some("min"),
            Outcome::Window(WindowOp::Resize(_)) => Some("resize"),
            Outcome::Window(WindowOp::Close) => Some("close"),
            Outcome::QueryMaximized => Some("query"),
            Outcome::Rail(_) => Some("rail"),
            Outcome::None => None,
        }
    }

    #[test]
    fn click_without_motion_does_not_drag() {
        let mut tb = TitleBar::default();
        let t0 = Instant::now();
        assert_eq!(op(tb.update(Message::Press, t0)), None);
        assert!(tb.tracking());
        assert_eq!(
            op(tb.update(Message::Move(Point::new(10.0, 10.0)), t0)),
            None
        );
        assert_eq!(
            op(tb.update(Message::Move(Point::new(12.0, 11.0)), t0)),
            None
        );
        assert_eq!(op(tb.update(Message::Release, t0)), None);
        assert!(!tb.tracking());
        // Motion after the release does nothing.
        assert_eq!(
            op(tb.update(Message::Move(Point::new(90.0, 10.0)), t0)),
            None
        );
    }

    #[test]
    fn press_and_move_drags_once() {
        let mut tb = TitleBar::default();
        let t0 = Instant::now();
        tb.update(Message::Press, t0);
        tb.update(Message::Move(Point::new(10.0, 10.0)), t0);
        assert_eq!(
            op(tb.update(Message::Move(Point::new(15.0, 10.0)), t0)),
            Some("drag")
        );
        // The window manager owns the pointer now; no second drag, and the
        // release may never arrive.
        assert!(!tb.tracking());
        assert_eq!(
            op(tb.update(Message::Move(Point::new(40.0, 10.0)), t0)),
            None
        );
        // A press right after a drag is a first click, not a double click.
        assert_eq!(
            op(tb.update(Message::Press, t0 + Duration::from_millis(100))),
            None
        );
    }

    #[test]
    fn double_click_toggles_maximize_and_slow_clicks_do_not() {
        let mut tb = TitleBar::default();
        let t0 = Instant::now();
        tb.update(Message::Press, t0);
        tb.update(Message::Release, t0);
        assert_eq!(
            op(tb.update(Message::Press, t0 + Duration::from_millis(250))),
            Some("max")
        );
        assert!(!tb.tracking());
        tb.update(Message::Release, t0);
        // A third quick press starts over.
        assert_eq!(
            op(tb.update(Message::Press, t0 + Duration::from_millis(400))),
            None
        );
        tb.update(Message::Release, t0);
        assert_eq!(
            op(tb.update(Message::Press, t0 + Duration::from_millis(1000))),
            None,
            "600 ms apart"
        );
    }

    /// Moves come window-wide while pressed, so a flick off the bar (here to
    /// y = 3, then above the window) still starts the move.
    #[test]
    fn a_flick_off_the_bar_drags() {
        let mut tb = TitleBar::default();
        let t0 = Instant::now();
        assert!(!tb.tracking());
        tb.update(Message::Press, t0);
        assert!(tb.tracking());
        tb.update(Message::Move(Point::new(511.0, 11.0)), t0);
        assert_eq!(
            op(tb.update(Message::Move(Point::new(511.0, -20.0)), t0)),
            Some("drag")
        );
        assert!(!tb.tracking());
        // An inert item's press is neither a click nor a drag.
        assert_eq!(op(tb.update(Message::Inert, t0)), None);
        assert!(!tb.tracking());
    }

    #[test]
    fn maximize_state_and_resize_edges() {
        let mut tb = TitleBar::default();
        let t0 = Instant::now();
        assert_eq!(
            op(tb.update(Message::Resize(Direction::East), t0)),
            Some("resize")
        );
        assert_eq!(op(tb.update(Message::Resized, t0)), Some("query"));
        tb.update(Message::Maximized(true), t0);
        assert!(tb.maximized);
        assert_eq!(op(tb.update(Message::Resize(Direction::East), t0)), None);
        tb.update(Message::Maximized(false), t0);
        assert_eq!(
            op(tb.update(Message::Resize(Direction::South), t0)),
            Some("resize")
        );
        assert_eq!(op(tb.update(Message::Close, t0)), Some("close"));
        assert_eq!(op(tb.update(Message::Minimize, t0)), Some("min"));
        assert_eq!(op(tb.update(Message::ToggleMaximize, t0)), Some("max"));
    }

    #[test]
    fn saved_preference_is_read_without_side_effects() {
        let dir = tempfile::tempdir().unwrap();
        assert!(!saved_system_title_bar(None));
        assert!(!saved_system_title_bar(Some(dir.path())));
        let path = dir.path().join(CONFIG_FILE);
        std::fs::write(&path, "[ui]\nsystem_title_bar = true\n").unwrap();
        assert!(saved_system_title_bar(Some(dir.path())));
        std::fs::write(&path, "not toml [").unwrap();
        assert!(!saved_system_title_bar(Some(dir.path())));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "not toml [");
    }

    fn entry(n: u8, unread: (u64, u64), signed_out: bool) -> RailEntry {
        RailEntry {
            id: ServerId(format!("s{n}")),
            name: format!("Server {n}"),
            avatar: AvatarSource::initials("Server", "https://example.org"),
            unread,
            selected: n == 0,
            signed_out,
        }
    }

    /// Badges, the signed-out marker, a modal (inert items), maximizing and
    /// workspaces coming and going keep the workspace row's scroll state.
    #[test]
    fn title_bar_keeps_its_scroll_state() {
        use crate::tree_probe::{assert_kept, scrollable_tag};
        use iced::advanced::widget::Tree;
        let view_of = |entries, interactive, maximized| -> Element<'static, Message> {
            view(View {
                entries,
                workspace_buttons: true,
                settings_active: false,
                title: "Rustket".into(),
                maximized,
                interactive,
            })
        };
        let a = view_of(
            vec![entry(0, (0, 0), false), entry(1, (0, 0), false)],
            true,
            false,
        );
        let b = view_of(
            vec![entry(0, (3, 2), false), entry(1, (1, 0), true)],
            false,
            true,
        );
        let c = view_of(vec![entry(0, (0, 0), false)], true, false);
        let mut tree = Tree::new(a.as_widget());
        assert_kept(&mut tree, &b, scrollable_tag(), "badges, modal, maximized");
        assert_kept(&mut tree, &c, scrollable_tag(), "workspace removed");
        assert_kept(&mut tree, &a, scrollable_tag(), "workspace added");
    }

    // --- in the app ------------------------------------------------------------

    use crate::app::{App, Flags, Modal, Screen, SecretsMode};
    use crate::message::Message as AppMsg;
    use rocketchat::models::{Room, RoomType, Subscription as Sub, User, UserRef};
    use rustket_core::config::ServerEntry;
    use rustket_core::sync::InitialData;
    use rustket_core::{Change, Workspace};

    fn sub(rid: &str, unread: u64, mentions: u64) -> Sub {
        Sub {
            id: format!("sub-{rid}"),
            rid: rid.into(),
            name: rid.to_lowercase(),
            t: RoomType::Channel,
            u: UserRef {
                id: "me".into(),
                username: Some("me".into()),
                name: None,
            },
            open: true,
            unread,
            user_mentions: mentions,
            ..Default::default()
        }
    }

    fn workspace() -> Workspace {
        let mut ws = Workspace::new();
        ws.apply_initial(InitialData {
            me: User {
                id: "me".into(),
                username: Some("me".into()),
                ..Default::default()
            },
            subscriptions: vec![sub("R1", 0, 0)],
            rooms: vec![Room {
                id: "R1".into(),
                t: RoomType::Channel,
                name: Some("r1".into()),
                ..Default::default()
            }],
            ..Default::default()
        });
        ws
    }

    /// An app with signed-in server "One" showing room R1, custom title bar.
    fn app() -> App {
        let (mut app, _) = App::new(Flags {
            ephemeral: true,
            secrets: SecretsMode::Memory,
            open_urls: false,
            ..Flags::default()
        });
        assert!(app.custom_chrome(), "custom title bar by default");
        let sid = add_server(&mut app, "One", 0, true);
        let s = app.servers.get_mut(&sid).unwrap();
        let mut ctx = crate::room::UpdateCtx {
            server_id: &s.id,
            server: &s.url,
            rid: "R1",
            workspace: &mut s.workspace,
            session: None,
            images: &app.images,
            prefs: &app.config.ui,
        };
        let (rv, _) = crate::room::RoomView::open(&mut ctx);
        for (i, text) in ["one", "two"].iter().enumerate() {
            s.workspace.apply_message(rocketchat::models::Message {
                id: format!("m{i}"),
                rid: "R1".into(),
                msg: (*text).into(),
                ts: rocketchat::RcDate::from_millis(1_000_000 + i as i64 * 1000),
                u: UserRef {
                    id: "alice".into(),
                    username: Some("alice".into()),
                    name: None,
                },
                ..Default::default()
            });
        }
        s.rooms.insert("R1".into(), rv);
        s.active_room = Some("R1".into());
        app.active = Some(sid);
        app.screen = Screen::Server;
        app
    }

    fn add_server(app: &mut App, name: &str, port: u16, signed_in: bool) -> ServerId {
        let entry = ServerEntry::new(format!("http://localhost:{}", 3900 + port), name);
        let sid = app.config.add_server(entry.clone());
        let mut state = super::super::ServerState::from_entry(&entry).unwrap();
        state.workspace = workspace();
        state.restoring = false;
        if signed_in {
            state.credentials = Some(rocketchat::Credentials {
                user_id: "me".into(),
                auth_token: "tok".into(),
            });
        }
        app.servers.insert(sid.clone(), state);
        sid
    }

    /// Servers, badges and the signed-out marker changing in the title bar,
    /// a modal covering the window and the window being maximized keep the
    /// state of every scrollable (title bar, sidebar, timeline) and of the
    /// composer.
    #[test]
    fn room_trees_survive_title_bar_changes() {
        use crate::tree_probe::{
            assert_first_kept, assert_kept, scrollable_tag, states, text_editor_tag,
        };
        use iced::advanced::widget::Tree;
        let mut app = app();
        let (sc, ed) = (scrollable_tag(), text_editor_tag());
        let mut tree = Tree::new(app.view().as_widget());
        // The title bar's workspace row, the room list and the timeline (at
        // least).
        let base = states(&tree, sc).len();
        assert!(base >= 3, "{:?}", states(&tree, sc));
        let first = app.active.clone().unwrap();
        type Step = (&'static str, Box<dyn Fn(&mut App)>);
        let steps: Vec<Step> = vec![
            (
                "server added",
                Box::new(|a| {
                    add_server(a, "Two", 1, true);
                }),
            ),
            (
                "signed-out server added",
                Box::new(|a| {
                    add_server(a, "Three", 2, false);
                }),
            ),
            (
                "badges",
                Box::new(move |a| {
                    let ws = &mut a.servers.get_mut(&first).unwrap().workspace;
                    ws.apply_subscription(Change::Updated, sub("R1", 4, 2));
                }),
            ),
            (
                "dragging",
                Box::new(|a| {
                    let _ = a.update(AppMsg::TitleBar(Message::Press));
                }),
            ),
            (
                "modal",
                Box::new(|a| {
                    let sid = a.active.clone().unwrap();
                    a.modals.push(Modal::ConfirmRemove(sid));
                }),
            ),
            (
                "maximized",
                Box::new(|a| {
                    a.modals.clear();
                    let _ = a.update(AppMsg::TitleBar(Message::Maximized(true)));
                }),
            ),
            (
                "server removed",
                Box::new(|a| {
                    let active = a.active.clone();
                    let last = a
                        .servers
                        .keys()
                        .rfind(|k| Some(*k) != active.as_ref())
                        .cloned()
                        .unwrap();
                    a.servers.remove(&last);
                    a.config.servers.retain(|s| s.id != last);
                }),
            ),
        ];
        for (step, change) in steps {
            change(&mut app);
            let view = app.view();
            // The dialog's own scrollable (last, in the layer) comes and goes.
            assert_first_kept(&mut tree, &view, sc, base, step);
            assert_kept(&mut tree, &view, ed, step);
        }
    }

    struct NoClip;
    impl iced::advanced::Clipboard for NoClip {
        fn read(&self, _: iced::advanced::clipboard::Kind) -> Option<String> {
            None
        }
        fn write(&mut self, _: iced::advanced::clipboard::Kind, _: String) {}
    }

    /// Messages of a left click at `at` in a `width` x 700 window.
    fn click(app: &App, width: f32, at: Point) -> Vec<String> {
        use iced_runtime::user_interface::{Cache, UserInterface};
        let mut r = iced::Renderer::Secondary(iced_tiny_skia::Renderer::new(
            rustket_ui::theme::font_regular(),
            iced::Pixels(rustket_ui::theme::TEXT_BODY),
        ));
        let mut ui = UserInterface::build(
            app.view(),
            iced::Size::new(width, 700.0),
            Cache::default(),
            &mut r,
        );
        let mut out = Vec::new();
        let _ = ui.update(
            &[
                iced::Event::Mouse(iced::mouse::Event::CursorMoved { position: at }),
                iced::Event::Mouse(iced::mouse::Event::ButtonPressed(iced::mouse::Button::Left)),
                iced::Event::Mouse(iced::mouse::Event::ButtonReleased(
                    iced::mouse::Button::Left,
                )),
            ],
            iced::mouse::Cursor::Available(at),
            &mut r,
            &mut NoClip,
            &mut out,
        );
        out.iter().map(|m| format!("{m:?}")).collect()
    }

    /// At 1024 px the window controls, the gear and the workspaces are where
    /// they belong, the empty middle moves the window, and under a modal the
    /// window controls, the drag area and the edges work while the
    /// workspaces are inert.
    #[test]
    fn layout_at_1024() {
        let mut app = app();
        add_server(&mut app, "Two", 1, true);
        add_server(&mut app, "Three", 2, false);
        let w = 1024.0;
        let y = TITLE_BAR_HEIGHT / 2.0;
        let has = |out: &[String], what: &str| out.iter().any(|m| m.contains(what));
        let close = click(&app, w, Point::new(w - 1.0 - 22.0, y));
        assert_eq!(close, ["TitleBar(Close)"]);
        let max = click(&app, w, Point::new(w - 1.0 - 44.0 - 22.0, y));
        assert_eq!(max, ["TitleBar(ToggleMaximize)"]);
        let gear = click(&app, w, Point::new(w - 1.0 - 132.0 - 8.0 - 16.0, y));
        assert_eq!(gear, ["TitleBar(Rail(Settings))"]);
        let ws = click(&app, w, Point::new(1.0 + 4.0 + TITLE_BAR_ITEM * 1.5, y));
        assert!(has(&ws, "TitleBar(Rail(Select("), "{ws:?}");
        let add = click(
            &app,
            w,
            Point::new(1.0 + 4.0 + TITLE_BAR_ITEM * 3.0 + 20.0, y),
        );
        assert_eq!(add, ["TitleBar(Rail(Add))"]);
        let middle = click(&app, w, Point::new(w / 2.0, y));
        assert_eq!(middle, ["TitleBar(Press)"]);
        let edge = click(&app, w, Point::new(w / 2.0, 1.0));
        assert_eq!(edge, ["TitleBar(Resize(North))"]);
        // Below the bar: the room, not the title bar.
        let room = click(&app, w, Point::new(w / 2.0, 300.0));
        assert!(!has(&room, "TitleBar"), "{room:?}");

        let sid = app.active.clone().unwrap();
        app.modals.push(Modal::ConfirmRemove(sid));
        let ws = click(&app, w, Point::new(1.0 + 4.0 + TITLE_BAR_ITEM * 1.5, y));
        assert_eq!(ws, ["TitleBar(Inert)"], "inert under a modal");
        let close = click(&app, w, Point::new(w - 1.0 - 22.0, y));
        assert_eq!(close, ["TitleBar(Close)"]);
        let max = click(&app, w, Point::new(w - 1.0 - 44.0 - 22.0, y));
        assert_eq!(max, ["TitleBar(ToggleMaximize)"]);
        let middle = click(&app, w, Point::new(w / 2.0, y));
        assert_eq!(middle, ["TitleBar(Press)"]);
        let edge = click(&app, w, Point::new(w - 1.0, 400.0));
        assert_eq!(edge, ["TitleBar(Resize(East))"]);
        // The backdrop still dismisses.
        let backdrop = click(&app, w, Point::new(40.0, 600.0));
        assert_eq!(backdrop, ["CloseModal"]);
    }

    /// With many workspaces in a narrow window the workspaces scroll: the
    /// window controls stay in place and some bar is left to drag by.
    #[test]
    fn narrow_window_keeps_the_controls() {
        let mut app = app();
        for i in 1..14 {
            add_server(&mut app, &format!("S{i}"), i, true);
        }
        let w = MIN_WINDOW_SIZE.width;
        let y = TITLE_BAR_HEIGHT / 2.0;
        let close = click(&app, w, Point::new(w - 1.0 - 22.0, y));
        assert_eq!(close, ["TitleBar(Close)"]);
        let min = click(&app, w, Point::new(w - 1.0 - 132.0 + 22.0, y));
        assert_eq!(min, ["TitleBar(Minimize)"]);
        let gear = click(&app, w, Point::new(w - 1.0 - 132.0 - 8.0 - 16.0, y));
        assert_eq!(gear, ["TitleBar(Rail(Settings))"]);
        let drag = click(
            &app,
            w,
            Point::new(w - 1.0 - 132.0 - 8.0 - 32.0 - DRAG_RESERVE / 2.0, y),
        );
        assert_eq!(drag, ["TitleBar(Press)"]);
    }

    /// With the system title bar the left rail is back and the custom title
    /// bar is gone.
    #[test]
    fn system_title_bar_uses_the_rail() {
        let mut app = app();
        app.config.ui.system_title_bar = true;
        let _ = app.update(AppMsg::Settings(
            crate::screens::settings::Message::SetSystemTitleBar(true),
        ));
        assert!(!app.custom_chrome());
        // Decorations are only toggled on a difference: a repeated change
        // (or a reload with the same value) does not flip them back.
        let _ = app.update(AppMsg::Settings(
            crate::screens::settings::Message::SetSystemTitleBar(true),
        ));
        assert!(!app.custom_chrome());
        let close = click(&app, 1024.0, Point::new(1024.0 - 22.0, 20.0));
        assert!(!close.iter().any(|m| m.contains("TitleBar")), "{close:?}");
        let rail = click(&app, 1024.0, Point::new(32.0, 30.0));
        assert!(rail.iter().any(|m| m.contains("Rail(Select")), "{rail:?}");
    }
}
