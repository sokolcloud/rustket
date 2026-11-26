use iced::Element;
use iced::widget::Column;
use rustket_core::config::{AppConfig, IndicatorScheme};
use rustket_core::quiet::Quiet;
use rustket_ui::theme::Mode;

use super::*;
use crate::help::model::{Code, Family, HelpEnv};

/// A default config pinned to the Rustket indicator scheme.
fn rustket_cfg() -> AppConfig {
    let mut cfg = AppConfig::default();
    cfg.ui.indicator_scheme = IndicatorScheme::Rustket;
    cfg
}

fn texts(cfg: &AppConfig, env: &HelpEnv) -> Vec<String> {
    let d = Data {
        cfg,
        env,
        mode: Mode::Light,
        theme_name: "Rustket".into(),
    };
    let el: Element<'_, Message> = Column::with_children(view(&d)).into();
    crate::tree_probe::texts(el)
}

fn has(texts: &[String], needle: &str) -> bool {
    texts.iter().any(|t| t.contains(needle))
}

/// No text is an untranslated key.
fn no_raw_keys(texts: &[String]) {
    for t in texts {
        assert!(!t.contains("help."), "raw key: {t}");
    }
}

/// Every code is on the page; the page follows the settings.
#[test]
fn page_shows_the_codes_and_follows_the_settings() {
    rust_i18n::set_locale("en");
    let mut cfg = rustket_cfg();
    let env = HelpEnv::default();
    let base = texts(&cfg, &env);
    for c in Code::ALL {
        assert!(base.iter().any(|t| t == c.code()), "{} missing", c.code());
    }
    assert!(has(&base, "Computed for alice @ Dev"));
    assert!(has(&base, "Shown with the message text"));
    assert!(has(&base, "Not paused"));
    assert!(!has(&base, "desktop notifications are off in Settings"));
    // The legend says where each code uses a colour.
    assert!(base.iter().any(|t| t == "M5 · dot"));
    // The server's limit for large rooms (100 by default).
    assert!(has(&base, "In rooms with more than 100 members"));
    // A plain @mention there: Rustket shows the notification itself.
    assert!(has(
        &base,
        "sends none; Rustket shows the notification itself"
    ));
    // Toasts that are not message toasts say what they show.
    assert!(has(&base, "You missed N reminders"));
    assert!(has(&base, "only while Rustket's window is not focused"));
    assert!(has(&base, "Reminder · general"));
    // Q4 follows the account's preference (on by default).
    assert!(has(&base, "On: your account's “Mute the conversation"));
    let mut off_env = env.clone();
    off_env.account.as_mut().unwrap().prefs.mute_focused = false;
    let q4_off = texts(&cfg, &off_env);
    assert!(has(&q4_off, "the conversation on screen notifies too"));
    no_raw_keys(&q4_off);
    // Account sounds use the account's names.
    assert!(has(&base, "New message sound"));
    assert!(has(&base, "Chime ·"));
    no_raw_keys(&base);

    cfg.ui.desktop_notifications = false;
    let off = texts(&cfg, &env);
    assert!(has(&off, "desktop notifications are off in Settings"));
    assert_ne!(base, off);
    no_raw_keys(&off);

    // Paused by hand: the toasts and Q1 name it.
    let mut cfg = rustket_cfg();
    let until = env.now_ms + 3_600_000;
    cfg.pause.paused_until = Some(until);
    let paused_env = HelpEnv {
        quiet: Quiet {
            paused: true,
            until: Some(until),
            off_hours: false,
        },
        ..HelpEnv::default()
    };
    let paused = texts(&cfg, &paused_env);
    assert!(has(&paused, "Paused until"));
    assert!(has(&paused, "(Q1)"));
    assert!(!has(&paused, "Not paused"));
    no_raw_keys(&paused);

    // No account: the server part asks to sign in.
    let none = texts(
        &rustket_cfg(),
        &HelpEnv {
            account: None,
            ..HelpEnv::default()
        },
    );
    assert!(has(&none, "No account is signed in"));
    assert!(has(&none, "Sign in to see them"));
    // No sample names a real user.
    assert!(has(&none, "@you"));
    assert!(!has(&none, "@me") && !has(&none, "alice"));
    no_raw_keys(&none);

    // Threads section off (default): no "Threads:" part on the collapsed
    // header line; on: there is one.
    assert!(!has(&base, "· Threads: "));
    let mut cfg = rustket_cfg();
    cfg.ui.sidebar_threads = rustket_core::config::ThreadsMode::Unread;
    assert!(has(&texts(&cfg, &env), "· Threads: "));

    // D1's on-time line only with the pinned rows.
    assert!(has(&base, "Drafts & scheduled, while on time"));
    let mut cfg = rustket_cfg();
    cfg.ui.sidebar_pinned_views = false;
    assert!(!has(&texts(&cfg, &env), "while on time"));
}

/// The Russian page quotes with «…» outside and „…“ inside, never the
/// reverse, and never «…» in «…» (the bodies of real toasts carry their
/// own «…»). It switches the process-wide locale, so it runs alone:
/// `cargo test -p rustket --lib russian_quotes -- --ignored --test-threads=1`.
#[test]
#[ignore = "switches the process-wide locale"]
fn russian_quotes_do_not_nest() {
    rust_i18n::set_locale("ru");
    let mut cfg = rustket_cfg();
    let mut all = texts(&cfg, &HelpEnv::default());
    cfg.ui.notification_preview = false;
    all.extend(texts(&cfg, &HelpEnv::default()));
    all.extend(texts(
        &cfg,
        &HelpEnv {
            account: None,
            ..HelpEnv::default()
        },
    ));
    rust_i18n::set_locale("en");
    for t in &all {
        russian_quotes_well_formed(t);
    }
    // L2 and C1 toast bodies, as the page shows them.
    assert!(has(&all, "«Откройте „Позже“, чтобы их посмотреть»"));
    assert!(has(&all, "«Вам звонят»"));
}

/// «…» only at the top level, „…“ only inside «…».
fn russian_quotes_well_formed(t: &str) {
    let mut guillemets = 0u32;
    let mut lows = 0u32;
    for ch in t.chars() {
        match ch {
            '«' => {
                assert_eq!((guillemets, lows), (0, 0), "nested quotes: {t}");
                guillemets += 1;
            }
            '»' => guillemets = guillemets.saturating_sub(1),
            '„' => {
                assert!(guillemets > 0, "„…“ outside «…»: {t}");
                lows += 1;
            }
            '“' => lows = lows.saturating_sub(1),
            _ => {}
        }
    }
}

#[test]
fn russian_quote_order_is_checked() {
    russian_quotes_well_formed("Показывается как «Напоминания»: «Откройте „Позже“, чтобы»");
    for bad in [
        "Показывается как «Вам»: „Вам звонят“",
        "„Откройте «Позже», чтобы их посмотреть“",
        "«Откройте «Позже», чтобы»",
    ] {
        assert!(
            std::panic::catch_unwind(|| russian_quotes_well_formed(bad)).is_err(),
            "{bad}"
        );
    }
}

/// The sound lines and Q1 follow the platform: on Windows a shown toast
/// plays the Windows sound, and Q1 says what the system's Do Not Disturb
/// silences there.
#[test]
fn sound_texts_follow_the_platform() {
    use crate::desktop::sound::Platform;
    rust_i18n::set_locale("en");
    let cfg = rustket_cfg();
    let env = |platform| HelpEnv {
        platform,
        ..HelpEnv::default()
    };
    let chime = t!(
        "help.sound.message",
        name = account_sound_label(Some("chime"))
    )
    .into_owned();
    let windows = t!("help.sound.windows").into_owned();
    let exact = |texts: &[String], s: &str| texts.iter().any(|t| t == s);
    let linux = texts(&cfg, &env(Platform::Linux));
    assert!(exact(&linux, &chime));
    assert!(!exact(&linux, &windows));
    assert!(has(&linux, "With KDE Plasma or GNOME"));
    assert!(has(&linux, "For call rings, and on other desktops"));
    assert!(has(
        &linux,
        "It plays your account's New message sound, unless"
    ));
    assert!(
        !linux.iter().any(|t| t.contains("Windows")),
        "Windows text on Linux"
    );
    let win = texts(&cfg, &env(Platform::Windows));
    assert!(exact(&win, &windows));
    assert!(!exact(&win, &chime), "every toast plays the Windows sound");
    assert!(has(
        &win,
        "When Windows shows a message or reminder notification, it plays the Windows notification sound in place of the Rocket.Chat sound"
    ));
    assert!(has(&win, "Sounds without a notification and call rings"));
    assert!(has(
        &win,
        "Its Windows notification plays the Windows notification sound"
    ));
    assert!(!has(&win, "KDE Plasma"));
    no_raw_keys(&win);
    let mac = texts(&cfg, &env(Platform::MacOs));
    assert!(exact(&mac, &chime));
    assert!(has(
        &mac,
        "When macOS shows a message or reminder notification"
    ));
    assert!(
        !mac.iter().any(|t| t.contains("Windows")),
        "Windows text on macOS"
    );
    // Without desktop notifications the Rocket.Chat sound plays alone
    // (reminders still show their own toast).
    let mut off = cfg.clone();
    off.ui.desktop_notifications = false;
    let win_off = texts(&off, &env(Platform::Windows));
    assert!(exact(&win_off, &chime));
    for t in [&linux, &win, &mac, &win_off] {
        assert!(!t.iter().any(|x| x.contains("%{")), "placeholder left");
    }
}

/// The account's desktop preference says whether it is stored on the
/// subscriptions or only the server default `/me` merges in.
#[test]
fn account_desktop_preference_shows_whether_it_is_stored() {
    rust_i18n::set_locale("en");
    let cfg = rustket_cfg();
    let mut env = HelpEnv::default();
    let not_stored = "All messages (server default) · ";
    assert!(!env.account.as_ref().unwrap().desktop_stored);
    assert!(has(&texts(&cfg, &env), not_stored));
    env.account.as_mut().unwrap().desktop_stored = true;
    let stored = texts(&cfg, &env);
    assert!(!has(&stored, "(server default)"));
    assert!(stored.iter().any(|t| t.starts_with("All messages · ")));
    // No value at all: the server default, named as such.
    let a = env.account.as_mut().unwrap();
    a.desktop_stored = false;
    a.prefs.desktop = None;
    a.server.default_desktop = Some(DesktopPref::Mentions);
    assert!(has(&texts(&cfg, &env), "Server default (Mentions)"));
}

/// K3: Rustket's own notification for a plain @mention; "you have no
/// highlight words" only where one would make the server notify; otherwise
/// the large-room reason.
#[test]
fn large_room_line_names_highlights_only_where_they_help() {
    rust_i18n::set_locale("en");
    let cfg = rustket_cfg();
    let count = |env: &HelpEnv, needle: &str| {
        texts(&cfg, env)
            .iter()
            .filter(|t| t.contains(needle))
            .count()
    };
    let counts = |env: &HelpEnv| {
        (
            count(env, "sends none; Rustket shows the notification itself"),
            count(env, "sends none (you have no highlight words)"),
            count(env, "sends none: the room has more than 100 members"),
        )
    };
    // DMs (M2) are never large rooms: no line. Preference not stored: M1
    // gets Rustket's fallback, M3 and M5 stay silent.
    let mut env = HelpEnv::default();
    assert_eq!(counts(&env), (1, 0, 2));
    // Stored "all": a highlight word would help M3 and M5.
    env.account.as_mut().unwrap().desktop_stored = true;
    assert_eq!(counts(&env), (1, 2, 0));
    // With highlight words and the preference not stored, the server
    // notifies M1; M3 and M5 stay silent and the line does not blame the
    // words.
    let a = env.account.as_mut().unwrap();
    a.desktop_stored = false;
    a.prefs.highlights = vec!["rust".into()];
    assert_eq!(counts(&env), (0, 0, 2));
}

/// In the narrowest settings page every text stays inside the page (the R
/// matrix columns shrink).
#[test]
fn narrow_page_fits() {
    use iced::advanced::widget::{Id, Operation};
    use iced::{Rectangle, Size};
    use iced_runtime::user_interface::{Cache, UserInterface};

    struct Bounds(Vec<(String, Rectangle)>);
    impl Operation for Bounds {
        fn traverse(&mut self, operate: &mut dyn FnMut(&mut dyn Operation)) {
            operate(self);
        }
        fn text(&mut self, _: Option<&Id>, bounds: Rectangle, text: &str) {
            self.0.push((text.to_owned(), bounds));
        }
    }

    rust_i18n::set_locale("en");
    let cfg = rustket_cfg();
    let env = HelpEnv::default();
    let d = Data {
        cfg: &cfg,
        env: &env,
        mode: Mode::Light,
        theme_name: "Rustket".into(),
    };
    let width = 380.0;
    let mut renderer = iced::Renderer::Secondary(iced_tiny_skia::Renderer::new(
        rustket_ui::theme::font_regular(),
        iced::Pixels(rustket_ui::theme::TEXT_BODY),
    ));
    let mut ui = UserInterface::build(
        Column::with_children(view(&d)),
        Size::new(width, 100_000.0),
        Cache::default(),
        &mut renderer,
    );
    let mut b = Bounds(Vec::new());
    ui.operate(&renderer, &mut b);
    let r4: Vec<_> = b.0.iter().filter(|(t, _)| t == "R4").collect();
    assert!(r4.len() >= 2, "R4 heading and column");
    for (t, r) in &b.0 {
        assert!(r.x + r.width <= width + 0.5, "{t} sticks out: {r:?}");
    }
    for (_, r) in r4 {
        assert!(r.width >= 10.0, "R4 squeezed: {r:?}");
    }
}

/// The keys chosen at runtime (code and family titles, triggers, plurals).
#[test]
fn runtime_keys_are_translated() {
    let entries = crate::i18n::tests::locale_entries();
    let mut keys: Vec<String> = Code::ALL
        .iter()
        .flat_map(|c| [c.title_key(), c.trigger_key()])
        .chain(Family::ALL.iter().map(|f| f.title_key()))
        .map(str::to_owned)
        .collect();
    keys.extend(["one", "few", "many"].map(|p| format!("help.usage_{p}")));
    keys.push(Tab::Help.i18n_key().to_owned());
    for key in keys {
        let texts = entries
            .get(key.as_str())
            .unwrap_or_else(|| panic!("{key} missing"));
        for loc in crate::i18n::tests::BUNDLED_COMPLETE {
            assert!(
                texts.get(*loc).is_some_and(|t| !t.is_empty()),
                "{key}: missing {loc}"
            );
        }
    }
}

/// The samples are inert: the only Tab stops are the jump buttons (to
/// other tabs, each to its own row's tab) and the shortcuts button, and a
/// click anywhere on the page sends nothing else.
#[test]
fn samples_are_inert() {
    use iced::advanced::widget::operation::Focusable;
    use iced::advanced::widget::{Id, Operation};
    use iced::{Point, Rectangle, Size, mouse};
    use iced_runtime::user_interface::{Cache, UserInterface};

    struct Stops(Vec<Rectangle>, Vec<(String, Rectangle)>);
    impl Operation for Stops {
        fn traverse(&mut self, operate: &mut dyn FnMut(&mut dyn Operation)) {
            operate(self);
        }
        fn focusable(&mut self, _: Option<&Id>, bounds: Rectangle, _: &mut dyn Focusable) {
            self.0.push(bounds);
        }
        fn text(&mut self, _: Option<&Id>, bounds: Rectangle, text: &str) {
            self.1.push((text.to_owned(), bounds));
        }
    }

    rust_i18n::set_locale("en");
    let cfg = rustket_cfg();
    // The left rail: its workspace sample is a button in the app.
    let env = HelpEnv {
        custom_chrome: false,
        ..HelpEnv::default()
    };
    let mut with_threads = cfg.clone();
    with_threads.ui.sidebar_threads = rustket_core::config::ThreadsMode::Unread;
    // The label of each row with an Open button and the tab it opens.
    let q2 = format!("Q2 · {}", t!(Code::Q2.title_key()));
    let rows: Vec<(String, Tab)> = [
        ("settings.notifications_enable", Tab::Notifications),
        ("settings.notifications_preview", Tab::Notifications),
        ("settings.notifications_sound", Tab::Notifications),
        ("settings.threads_mode", Tab::Sidebar),
        ("settings.unread_on_top", Tab::Sidebar),
        ("settings.pinned_views", Tab::Sidebar),
        ("settings.theme", Tab::Appearance),
    ]
    .into_iter()
    .map(|(k, tab)| (t!(k).into_owned(), tab))
    .chain([(q2, Tab::Notifications)])
    .collect();
    let allowed = |m: &Message| {
        matches!(
            m,
            Message::Tab(Tab::Notifications | Tab::Sidebar | Tab::Appearance)
                | Message::ShowShortcuts
        )
    };
    for cfg in [&cfg, &with_threads] {
        let d = Data {
            cfg,
            env: &env,
            mode: Mode::Light,
            theme_name: "Rustket".into(),
        };
        let opens = texts(cfg, &env).iter().filter(|t| *t == "Open").count();
        let mut renderer = iced::Renderer::Secondary(iced_tiny_skia::Renderer::new(
            rustket_ui::theme::font_regular(),
            iced::Pixels(rustket_ui::theme::TEXT_BODY),
        ));
        let width = 800.0;
        let mut ui = UserInterface::build(
            Column::with_children(view(&d)),
            Size::new(width, 100_000.0),
            Cache::default(),
            &mut renderer,
        );
        let mut stops = Stops(Vec::new(), Vec::new());
        ui.operate(&renderer, &mut stops);
        assert_eq!(stops.0.len(), opens + 1, "Tab stops");
        assert_eq!(opens, rows.len(), "Open buttons");
        // The row label nearest to a button (by vertical centre).
        let row_of = |b: &Rectangle| {
            let cy = b.center_y();
            rows.iter()
                .flat_map(|(label, tab)| {
                    stops
                        .1
                        .iter()
                        .filter(move |(t, r)| t == label && r.x < b.x)
                        .map(move |(_, r)| ((r.center_y() - cy).abs(), label, *tab))
                })
                .min_by(|a, b| a.0.total_cmp(&b.0))
        };
        let mut click = |at: Point| {
            let mut out = Vec::new();
            let _ = ui.update(
                &[
                    iced::Event::Mouse(mouse::Event::CursorMoved { position: at }),
                    iced::Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)),
                    iced::Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)),
                ],
                mouse::Cursor::Available(at),
                &mut renderer,
                &mut iced::advanced::clipboard::Null,
                &mut out,
            );
            out
        };
        let mut seen = Vec::new();
        for b in &stops.0 {
            let row = row_of(b);
            let out = click(b.center());
            assert!(out.len() == 1 && allowed(&out[0]), "{out:?}");
            if let Message::Tab(tab) = &out[0] {
                let (dist, label, want) = row.unwrap();
                assert!(dist < 40.0, "{label}: no row at {b:?}");
                assert_eq!(*tab, want, "Open in row {label}");
                seen.push(label.clone());
            }
        }
        seen.sort();
        seen.dedup();
        assert_eq!(seen.len(), rows.len(), "every row's own button");
        // Every sample row is taller than the step (the Threads one has
        // all the kinds of samples).
        if cfg.ui.sidebar_threads == rustket_core::config::ThreadsMode::Off {
            continue;
        }
        let height = stops.0.iter().map(|b| b.y + b.height).fold(0.0, f32::max);
        let mut y = 2.0;
        while y < height {
            let mut x = 4.0;
            while x < width {
                for m in click(Point::new(x, y)) {
                    assert!(allowed(&m), "{m:?} at {x},{y}");
                }
                x += 40.0;
            }
            y += 20.0;
        }
    }
}

/// The refresh tick runs only on the Help tab while the window is shown.
#[test]
fn help_tick_only_while_shown() {
    use iced::advanced::subscription::into_recipes;
    let mut s = super::super::Settings::with_autostart(None);
    for tab in Tab::ALL {
        s.tab = tab;
        for visible in [true, false] {
            let n = into_recipes(s.subscription(visible)).len();
            assert_eq!(
                n,
                usize::from(tab == Tab::Help && visible),
                "{tab:?} {visible}"
            );
        }
    }
}

const SHOT_WIDTH: f32 = 800.0;

fn load_fonts() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        if let Ok(mut fonts) = iced::advanced::graphics::text::font_system().write() {
            for f in rustket_ui::fonts() {
                fonts.load_font(f);
            }
        }
    });
}

/// The tab body in a frame of the page's width (no scrollable).
fn page<'a>(d: &Data<'_>) -> Element<'a, Message> {
    use iced::Length;
    use iced::widget::container;
    use rustket_ui::theme::SPACE_6;
    container(
        container(
            Column::with_children(view(d))
                .spacing(SPACE_6)
                .max_width(720),
        )
        .center_x(Length::Fill),
    )
    .padding(SPACE_6)
    .width(Length::Fixed(SHOT_WIDTH))
    .into()
}

/// Renders the Help tab (light and dark, English and Russian) into PNG files
/// in `$RUSTKET_SHOT_DIR` (skipped when unset). It switches the process-wide
/// locale: run it alone (`-- --ignored screenshots`).
#[test]
#[ignore = "writes PNG files; run with RUSTKET_SHOT_DIR set"]
fn screenshots() {
    use iced::advanced::layout::Limits;
    use iced::advanced::renderer::{Headless, Style};
    use iced::advanced::widget::Tree;
    use iced::{Size, mouse};
    use iced_runtime::user_interface::{Cache, UserInterface};

    /// Puts the process-wide locale back to English, also on a panic
    /// (other tests of this binary assert English text).
    struct EnglishAfter;
    impl Drop for EnglishAfter {
        fn drop(&mut self) {
            rust_i18n::set_locale("en");
        }
    }

    let Some(dir) = std::env::var_os("RUSTKET_SHOT_DIR").map(std::path::PathBuf::from) else {
        eprintln!("skipped: set RUSTKET_SHOT_DIR to write the screenshots");
        return;
    };
    std::fs::create_dir_all(&dir).unwrap();
    let _english = EnglishAfter;
    load_fonts();
    let cfg = rustket_cfg();
    let env = HelpEnv::default();
    for lang in ["en", "ru"] {
        rust_i18n::set_locale(lang);
        for (mode, name) in [(Mode::Light, "light"), (Mode::Dark, "dark")] {
            let d = Data {
                cfg: &cfg,
                env: &env,
                mode,
                theme_name: t!("settings.theme_builtin").into_owned(),
            };
            let theme = rustket_ui::theme(mode);
            let tokens = rustket_ui::theme::active_tokens(mode);
            let mut renderer = iced::Renderer::Secondary(iced_tiny_skia::Renderer::new(
                rustket_ui::theme::font_regular(),
                iced::Pixels(rustket_ui::theme::TEXT_BODY),
            ));
            // The height the whole page needs.
            let mut el = page(&d);
            let mut tree = Tree::new(&el);
            let node = el.as_widget_mut().layout(
                &mut tree,
                &renderer,
                &Limits::new(Size::ZERO, Size::new(SHOT_WIDTH, f32::INFINITY)),
            );
            let height = node.size().height.ceil().max(1.0);
            let size = Size::new(SHOT_WIDTH, height);
            let mut ui = UserInterface::build(page(&d), size, Cache::default(), &mut renderer);
            // Buttons take their status on the first redraw.
            let _ = ui.update(
                &[iced::Event::Window(iced::window::Event::RedrawRequested(
                    std::time::Instant::now(),
                ))],
                mouse::Cursor::Unavailable,
                &mut renderer,
                &mut iced::advanced::clipboard::Null,
                &mut Vec::new(),
            );
            ui.draw(
                &mut renderer,
                &theme,
                &Style {
                    text_color: tokens.text_primary,
                },
                mouse::Cursor::Unavailable,
            );
            let (w, h) = (SHOT_WIDTH as u32, height as u32);
            let rgba = renderer.screenshot(Size::new(w, h), 1.0, tokens.surface_room);
            assert_eq!(rgba.len(), (w * h * 4) as usize);
            let (pixels, _) = rgba.as_chunks::<4>();
            let distinct = pixels.iter().filter(|p| **p != pixels[0]).count();
            assert!(distinct > 1000, "{lang}/{name}: blank");
            let mut out = Vec::new();
            {
                let mut enc = png::Encoder::new(&mut out, w, h);
                enc.set_color(png::ColorType::Rgba);
                enc.set_depth(png::BitDepth::Eight);
                let mut wr = enc.write_header().unwrap();
                wr.write_image_data(&rgba).unwrap();
            }
            std::fs::write(dir.join(format!("help-{name}-{lang}.png")), out).unwrap();
        }
    }
}

/// On macOS the tray texts describe the monochrome menu bar glyph (a dot for
/// mentions and `@all`, a ring otherwise) and the Dock icon's number of
/// mentions; Linux and Windows keep the coloured tray dots.
#[test]
fn tray_texts_follow_the_platform() {
    use crate::desktop::sound::Platform;
    rust_i18n::set_locale("en");
    let cfg = rustket_cfg();
    let env = |platform| HelpEnv {
        platform,
        ..HelpEnv::default()
    };
    for platform in [Platform::Linux, Platform::Windows] {
        let t = texts(&cfg, &env(platform));
        assert!(has(&t, "tray icon with a red dot"), "{platform:?}");
        assert!(has(&t, "tray icon with an orange dot"), "{platform:?}");
        assert!(has(&t, "tray icon with a quiet dot"), "{platform:?}");
        assert!(has(&t, "the tray's red dot"), "{platform:?}");
        assert!(has(&t, "Status menu or tray"), "{platform:?}");
        assert!(
            !t.iter()
                .any(|x| x.contains("menu bar") || x.contains("Dock")),
            "{platform:?}: macOS text"
        );
    }
    let mac = texts(&cfg, &env(Platform::MacOs));
    for s in [
        "Menu bar icon",
        "Window title, menu bar and Dock",
        "menu bar icon with a dot · Dock icon with the number “1”",
        "menu bar icon with a ring · no number on the Dock icon",
        "the ring on the menu bar icon",
        "the dot on the menu bar icon, the number on the Dock icon",
        "the number on the Dock icon counts only mentions of you, your highlight words and direct messages",
        "Status menu or menu bar icon",
        "puts the dot on the menu bar icon and counts in the number on the Dock icon",
    ] {
        assert!(has(&mac, s), "missing on macOS: {s}");
    }
    for s in [
        "tray icon with",
        "the tray's",
        "orange dot of the workspace icon and the tray",
        "Tray icon",
        "Status menu or tray",
    ] {
        assert!(!has(&mac, s), "tray text on macOS: {s}");
    }
    // `@all` alone: a dot in the menu bar, no number on the Dock icon.
    assert!(has(
        &mac,
        "menu bar icon with a dot · no number on the Dock icon"
    ));
    no_raw_keys(&mac);
}

// --- the Rocket.Chat scheme -------------------------------------------------

fn rc_cfg() -> AppConfig {
    let mut cfg = AppConfig::default();
    cfg.ui.indicator_scheme = IndicatorScheme::RocketChat;
    cfg
}

/// The page describes the Rocket.Chat scheme: its colour names and tokens,
/// full counters without dots, the highlighted name, RC's tray, window icon
/// and workspace badge, the feed without row highlights.
#[test]
fn rocketchat_page() {
    rust_i18n::set_locale("en");
    let cfg = rc_cfg();
    let env = HelpEnv::default();
    let rc = texts(&cfg, &env);
    no_raw_keys(&rc);
    assert!(!rc.iter().any(|t| t.contains("%{")), "placeholder left");
    for c in Code::ALL {
        assert!(rc.iter().any(|t| t == c.code()), "{} missing", c.code());
    }
    for s in [
        "Indicators follow Rocket.Chat's web client and desktop app",
        "My account: Unread Tray Icon Alert",
        // Legend: RC names, both shades, the RC caption.
        "red (danger)",
        "blue (primary)",
        "orange (warning)",
        "grey (secondary)",
        "rc_sidebar_danger #B43C4C · rc_danger #EC0D2A",
        "rc_sidebar_secondary #6C737A · rc_secondary #6C737A",
        "the sidebar always uses Rocket.Chat's sidebar shades",
        // Rows: M2 is grey, the name is highlighted, M5 has no counter.
        "counter 1, grey (secondary) · highlighted name (medium weight)",
        "nothing · highlighted name (medium weight)",
        "only mentions (yours, @all, @here); other messages: only the name is highlighted",
        "only highlights the room's name (no counter)",
        // Collapsed headers: the counter with the itemised tooltip.
        "counter 1, red (danger), bold title; tooltip “1 mention”",
        "counter 1, grey (secondary), bold title; tooltip “1 unread message”",
        // The header's Threads counter (T3 is orange there).
        "Threads button in the room header: counter 1, orange (warning)",
        // Window, tray, window icon, workspace badge.
        "Window title, tray and window icon",
        "window title “Rustket (1)” · tray icon with a red badge “1” · the same badge on the window and taskbar icon",
        "window title unchanged · tray icon with a red badge “•”",
        "grey counter 1",
        "grey dot",
        // The feed.
        "no row highlight",
        "your name in the mention colour, no background",
        "“all” in the @all colour, no background",
        "the word white on red",
        "the thread's reply button filled blue, with a dot, orange (warning)",
        // Codes with their own Rocket.Chat text.
        "it only highlights the room's name: its row shows no counter and no dot",
        "When the server counts every direct message (its default), a grey counter in its row",
        // The @all colour names its thread dots and the Threads counter.
        "the dot of a thread that has @all",
        "(plus one for each unread thread)",
        "the room's counter is blue, because threads go before @all",
        "The room no longer notifies, but its row counter",
    ] {
        assert!(has(&rc, s), "missing: {s}");
    }
    for s in [
        "quiet colour",
        "counter 1, mention colour",
        "bold name",
        "tray icon with a red dot",
        "row highlighted",
        "dot, grey",
        "badge_mention",
        "@-chip",
    ] {
        assert!(!has(&rc, s), "Rustket text under Rocket.Chat: {s}");
    }
    // The current scheme is named on both pages.
    assert!(rc.iter().any(|t| t == "Unread indicators"));
    assert!(rc.iter().any(|t| t == "Rocket.Chat"));
    let rk = texts(&rustket_cfg(), &env);
    assert!(rk.iter().any(|t| t == "Unread indicators"));
    assert!(!has(&rk, "Indicators follow Rocket.Chat's"));
    assert!(!has(&rk, "Unread Tray Icon Alert"));
    // Without an account: placeholders only, no raw keys.
    let none = texts(
        &cfg,
        &HelpEnv {
            account: None,
            ..HelpEnv::default()
        },
    );
    no_raw_keys(&none);
    assert!(!has(&none, "Unread Tray Icon Alert"));
    // The preference off: the bullet goes, the counted number stays.
    let mut off = env.clone();
    off.account.as_mut().unwrap().prefs.unread_alert = false;
    let off = texts(&cfg, &off);
    assert!(!has(&off, "red badge “•”"));
    assert!(has(&off, "tray icon with a red badge “1”"));
    no_raw_keys(&off);
}

/// The window icon's badge follows the desktop, not the tray: none on
/// Wayland, its own text without a tray.
#[test]
fn rocketchat_window_icon_texts() {
    rust_i18n::set_locale("en");
    let cfg = rc_cfg();
    let same = "the same badge on the window and taskbar icon";
    let wayland = texts(
        &cfg,
        &HelpEnv {
            window_icon: false,
            ..HelpEnv::default()
        },
    );
    assert!(has(&wayland, "tray icon with a red badge “1”"));
    assert!(!has(&wayland, same));
    assert!(!has(&wayland, "window and taskbar icon"));
    let no_tray = texts(
        &cfg,
        &HelpEnv {
            tray: false,
            ..HelpEnv::default()
        },
    );
    assert!(has(
        &no_tray,
        "no tray icon · window and taskbar icon with a red badge “1”"
    ));
    assert!(!has(&no_tray, same));
    no_raw_keys(&no_tray);
}

/// Under Rocket.Chat the macOS texts name the menu bar count and the Dock's
/// full number or "•".
#[test]
fn rocketchat_macos_texts() {
    use crate::desktop::sound::Platform;
    rust_i18n::set_locale("en");
    let env = HelpEnv {
        platform: Platform::MacOs,
        ..HelpEnv::default()
    };
    let mac = texts(&rc_cfg(), &env);
    no_raw_keys(&mac);
    for s in [
        "window title “Rustket (1)” · menu bar icon with a dot and “1” next to it · Dock icon with the number “1”",
        "menu bar icon with a dot · “•” on the Dock icon",
    ] {
        assert!(has(&mac, s), "missing on macOS: {s}");
    }
    assert!(!has(&mac, "tray icon"));
    assert!(!has(&mac, "taskbar"));
}

/// The Rocket.Chat page keeps every text inside the narrowest settings
/// page and its only extra Tab stop is the Unread indicators row's Open
/// button (to Appearance).
#[test]
fn rocketchat_page_fits_and_opens_appearance() {
    use iced::advanced::widget::operation::Focusable;
    use iced::advanced::widget::{Id, Operation};
    use iced::{Rectangle, Size};
    use iced_runtime::user_interface::{Cache, UserInterface};

    #[derive(Default)]
    struct Probe(Vec<Rectangle>, Vec<(String, Rectangle)>);
    impl Operation for Probe {
        fn traverse(&mut self, operate: &mut dyn FnMut(&mut dyn Operation)) {
            operate(self);
        }
        fn focusable(&mut self, _: Option<&Id>, bounds: Rectangle, _: &mut dyn Focusable) {
            self.0.push(bounds);
        }
        fn text(&mut self, _: Option<&Id>, bounds: Rectangle, text: &str) {
            self.1.push((text.to_owned(), bounds));
        }
    }

    rust_i18n::set_locale("en");
    let probe = |cfg: &AppConfig, width: f32| {
        let env = HelpEnv::default();
        let d = Data {
            cfg,
            env: &env,
            mode: Mode::Light,
            theme_name: "Rustket".into(),
        };
        let mut renderer = iced::Renderer::Secondary(iced_tiny_skia::Renderer::new(
            rustket_ui::theme::font_regular(),
            iced::Pixels(rustket_ui::theme::TEXT_BODY),
        ));
        let mut ui = UserInterface::build(
            Column::with_children(view(&d)),
            Size::new(width, 100_000.0),
            Cache::default(),
            &mut renderer,
        );
        let mut p = Probe::default();
        ui.operate(&renderer, &mut p);
        p
    };
    let width = 380.0;
    let rc = probe(&rc_cfg(), width);
    for (t, r) in &rc.1 {
        assert!(r.x + r.width <= width + 0.5, "{t} sticks out: {r:?}");
    }
    let rk = probe(&rustket_cfg(), width);
    assert_eq!(rc.0.len(), rk.0.len() + 1, "Tab stops");
}

/// The Rocket.Chat trigger keys chosen at runtime.
#[test]
fn rocketchat_runtime_keys_are_translated() {
    let entries = crate::i18n::tests::locale_entries();
    for c in [
        Code::M2,
        Code::M4,
        Code::M5,
        Code::T1,
        Code::T2,
        Code::T3,
        Code::R1,
        Code::R3,
        Code::R4,
    ] {
        let key = format!("{}_rc", c.trigger_key());
        let texts = entries
            .get(key.as_str())
            .unwrap_or_else(|| panic!("{key} missing"));
        for loc in crate::i18n::tests::BUNDLED_COMPLETE {
            assert!(
                texts.get(*loc).is_some_and(|t| !t.is_empty()),
                "{key}: missing {loc}"
            );
        }
    }
}

/// Review: Q4 told the old read rule (bottom of the feed) although by
/// default a room also waits for its first unread message; with
/// `ui.mark_read_on_open` the old rule holds.
#[test]
fn q4_follows_the_read_setting() {
    rust_i18n::set_locale("en");
    let mut cfg = rustket_cfg();
    let env = HelpEnv::default();
    let first = "first unread message has been on screen";
    assert!(!cfg.ui.mark_read_on_open);
    assert!(has(&texts(&cfg, &env), first));
    cfg.ui.mark_read_on_open = true;
    let on_open = texts(&cfg, &env);
    assert!(!has(&on_open, first));
    assert!(has(
        &on_open,
        "the feed is at the bottom and no app dialog covers it"
    ));
}

#[test]
fn q4_mentions_esc_only_while_it_marks_read() {
    rust_i18n::set_locale("en");
    let mut cfg = rustket_cfg();
    let env = HelpEnv::default();
    let esc = "and Esc mark it read at once";
    assert!(cfg.ui.escape_marks_read);
    assert!(has(&texts(&cfg, &env), esc));
    cfg.ui.escape_marks_read = false;
    let no_esc = texts(&cfg, &env);
    assert!(!has(&no_esc, esc));
    assert!(has(&no_esc, "first unread message has been on screen"));
    assert!(has(
        &no_esc,
        "Mark as read on the unread bar marks it read at once"
    ));
}
