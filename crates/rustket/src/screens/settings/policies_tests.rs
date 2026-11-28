//! The policy in the Settings: locked switches, the managed notice and the
//! Policies tab, rendered headless.

use iced::advanced::widget::{Id, Operation};
use iced::{Point, Rectangle, Size, mouse};
use iced_runtime::user_interface::{Cache, UserInterface};
use rustket_core::config::AppConfig;
use rustket_core::policy::{
    Policy, RawValue, Snapshot, SourceError, SourceErrorKind, SourceId, Trust, TrustProblem,
    TrustSubject, resolve,
};
use rustket_core::secrets::SecretBackend;

use super::*;

/// What a `SettingsView` borrows.
pub(super) struct Fixture {
    pub config: AppConfig,
    pub themes: crate::themes::Themes,
    pub locales: crate::locales::Locales,
    pub policy: Policy,
}

impl Fixture {
    /// The user's preferences under `policy`, as the app keeps them (the
    /// effective copy).
    pub fn new(user: AppConfig, policy: Policy) -> Self {
        let mut config = user;
        config.ui = policy.apply(&config.ui);
        Fixture {
            config,
            themes: Default::default(),
            locales: Default::default(),
            policy,
        }
    }

    pub fn view(&self) -> SettingsView<'_> {
        SettingsView {
            config: &self.config,
            themes: &self.themes,
            locales: &self.locales,
            handler_registered: true,
            notifications_blocked: false,
            secret_backend: Some(SecretBackend::Keyring),
            secrets_system: true,
            secrets_switching: false,
            tray: TraySupport {
                available: true,
                can_hide: true,
            },
            offline_cache: None,
            flags: Vec::new(),
            star_kept_busy: false,
            mention_groups: Vec::new(),
            mode: rustket_ui::theme::Mode::Light,
            help: None,
            policy: &self.policy,
            #[cfg(feature = "self-update")]
            updater: crate::updater::view::SettingsData {
                method: "Downloads from the project page".into(),
                status: String::new(),
                last_checked: "Never checked".into(),
                busy: false,
                install: None,
                release_page: false,
                check: true,
                managed: None,
            },
        }
    }
}

/// A policy of one full-trust test source, resolved for this build.
pub(super) fn policy(values: &[(&str, RawValue)]) -> Policy {
    let mut snap = Snapshot::new(SourceId::Test("admin".into()), Trust::Full);
    for (name, value) in values {
        snap = snap.with(name, value.clone());
    }
    resolve(&[snap], &crate::policy::context())
}

/// A mixed report: applied, overridden, invalid with dropped items, an
/// unknown key, and a limited-trust file with trust problems.
pub(super) fn mixed_policy() -> Policy {
    let high = Snapshot::new(SourceId::LinuxFile("50-company.json".into()), Trust::Full)
        .with("NotificationPreview", RawValue::Bool(false))
        .with("OrganizationName", RawValue::String("Example Corp".into()))
        .with(
            "SupportUrl",
            RawValue::String("https://help.example.com/chat".into()),
        )
        .with("RequireHttps", RawValue::Bool(true))
        .with(
            "AllowedServers",
            RawValue::List(vec![
                RawValue::String("chat.example.com".into()),
                RawValue::Integer(7),
                RawValue::String("ftp://files.example.com".into()),
                RawValue::String("*x.example.com".into()),
                RawValue::String(format!(
                    "https://{}.example.com",
                    "very-long-name".repeat(6)
                )),
            ]),
        )
        .with("AllowUserServers", RawValue::Integer(3))
        .with("RequireHtps", RawValue::Bool(true));
    let mut low = Snapshot::new(SourceId::LinuxFile("90-local.json".into()), Trust::Limited)
        .with("RequireHttps", RawValue::Bool(false))
        .with(
            "SupportUrl",
            RawValue::String("https://elsewhere.example".into()),
        )
        .with(
            "ManagedServers",
            RawValue::List(vec![RawValue::String("https://rogue.example".into())]),
        );
    low.trust_problems = vec![
        TrustProblem::WrongOwner {
            subject: TrustSubject::File,
            found: 1000,
            expected: 0,
        },
        TrustProblem::Writable {
            subject: TrustSubject::File,
        },
    ];
    resolve(&[high, low], &crate::policy::context())
}

/// A policy with both blocking states.
pub(super) fn blocking_policy() -> Policy {
    let broken = Snapshot::unreadable(
        SourceId::LinuxFile("20-broken.json".into()),
        SourceError::new(SourceErrorKind::InvalidJson)
            .with_detail("expected `,` or `}` at line 3 column 5"),
    );
    let min = Snapshot::new(SourceId::LinuxFile("10-version.json".into()), Trust::Full)
        .with("MinimumVersion", RawValue::String("99.0.0".into()))
        .with("OrganizationName", RawValue::String("Example Corp".into()));
    resolve(&[broken, min], &crate::policy::context())
}

fn ui<'a>(
    el: Element<'a, Message>,
    size: Size,
) -> (
    UserInterface<'a, Message, iced::Theme, iced::Renderer>,
    iced::Renderer,
) {
    let mut renderer = iced::Renderer::Secondary(iced_tiny_skia::Renderer::new(
        rustket_ui::theme::font_regular(),
        iced::Pixels(rustket_ui::theme::TEXT_BODY),
    ));
    let ui = UserInterface::build(el, size, Cache::default(), &mut renderer);
    (ui, renderer)
}

/// The texts of `el` with their bounds, laid out at `size`.
fn text_bounds(
    ui: &mut UserInterface<'_, Message, iced::Theme, iced::Renderer>,
    renderer: &iced::Renderer,
) -> Vec<(String, Rectangle)> {
    struct Texts(Vec<(String, Rectangle)>);
    impl Operation for Texts {
        fn traverse(&mut self, operate: &mut dyn FnMut(&mut dyn Operation)) {
            operate(self);
        }
        fn text(&mut self, _: Option<&Id>, bounds: Rectangle, text: &str) {
            self.0.push((text.to_owned(), bounds));
        }
    }
    let mut texts = Texts(Vec::new());
    ui.operate(renderer, &mut texts);
    texts.0
}

const SIZE: Size = Size::new(1280.0, 3000.0);

/// The page as the app shows it, and its texts.
pub(super) fn texts_of(s: &Settings, f: &Fixture) -> Vec<String> {
    crate::tree_probe::texts(s.view(f.view()))
}

/// Clicks the centre of the text `label` in the page; the messages sent.
pub(super) fn click_text(s: &Settings, f: &Fixture, label: &str) -> Vec<Message> {
    let (mut ui, mut renderer) = ui(s.view(f.view()), SIZE);
    let texts = text_bounds(&mut ui, &renderer);
    let at = texts
        .iter()
        .find(|(t, _)| t == label)
        .map(|(_, b)| b.center())
        .unwrap_or_else(|| panic!("no {label:?} in {texts:?}"));
    click(&mut ui, &mut renderer, at)
}

fn click(
    ui: &mut UserInterface<'_, Message, iced::Theme, iced::Renderer>,
    renderer: &mut iced::Renderer,
    at: Point,
) -> Vec<Message> {
    let mut out = Vec::new();
    let _ = ui.update(
        &[
            iced::Event::Mouse(mouse::Event::CursorMoved { position: at }),
            iced::Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)),
            iced::Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)),
        ],
        mouse::Cursor::Available(at),
        renderer,
        &mut iced::advanced::clipboard::Null,
        &mut out,
    );
    out
}

/// The messages clicks anywhere right of the text `label`, within its row
/// (the label, the help or managed line, and the switch beside them), send.
pub(super) fn click_row_of(s: &Settings, f: &Fixture, label: &str) -> Vec<Message> {
    let (mut ui, mut renderer) = ui(s.view(f.view()), SIZE);
    let texts = text_bounds(&mut ui, &renderer);
    let (_, at) = texts
        .iter()
        .find(|(t, _)| t == label)
        .unwrap_or_else(|| panic!("no {label:?}"))
        .clone();
    let mut sent = Vec::new();
    let mut y = at.y;
    while y < at.y + 2.0 * at.height {
        let mut x = at.x + at.width;
        while x < SIZE.width {
            sent.extend(click(&mut ui, &mut renderer, Point::new(x, y)));
            x += 6.0;
        }
        y += 4.0;
    }
    sent
}

fn notifications(policy: Policy) -> (Settings, Fixture) {
    let mut user = AppConfig::default();
    user.ui.notification_preview = true;
    (
        Settings::with_autostart(None).with_tab(Tab::Notifications),
        Fixture::new(user, policy),
    )
}

/// Without a policy the tabs are exactly those of before.
#[test]
fn no_policies_tab_without_a_policy() {
    rust_i18n::set_locale("en");
    let empty = Policy::default();
    assert_eq!(
        Tab::offered(&empty),
        [
            Tab::General,
            Tab::Appearance,
            Tab::Notifications,
            Tab::Sidebar,
            Tab::Mentions,
            Tab::Privacy,
            Tab::Help,
            Tab::About,
        ]
    );
    let (s, f) = notifications(empty);
    let texts = texts_of(&s, &f);
    assert!(!texts.iter().any(|t| t == "Policies"), "{texts:?}");
    assert!(!texts.iter().any(|t| t.contains("managed")), "{texts:?}");
    // With a policy to show, the tab sits before Help.
    let offered = Tab::offered(&mixed_policy());
    assert_eq!(offered.len(), 9);
    assert_eq!(offered[6], Tab::Policies);
}

/// The selected Policies tab falls back to General once the policy is gone.
#[test]
fn policies_tab_without_content_shows_general() {
    let s = Settings::with_autostart(None).with_tab(Tab::Policies);
    assert_eq!(s.shown_tab(&Policy::default()), Tab::General);
    assert_eq!(s.shown_tab(&mixed_policy()), Tab::Policies);
}

/// A locked switch is disabled, keeps the forced value and says who manages
/// it; an unlocked one is unchanged.
#[test]
fn locked_switch_is_disabled_with_the_managed_line() {
    rust_i18n::set_locale("en");
    let label = "Show message text";
    let (s, free) = notifications(Policy::default());
    assert!(free.config.ui.notification_preview);
    let texts = texts_of(&s, &free);
    assert!(texts.iter().any(|t| t == label));
    assert!(!texts.iter().any(|t| t.starts_with("Managed by")));
    let sent = click_row_of(&s, &free, label);
    assert!(
        !sent.is_empty() && sent.iter().all(|m| matches!(m, Message::SetPreview(false))),
        "{sent:?}"
    );

    let (s, locked) = notifications(policy(&[("NotificationPreview", RawValue::Bool(false))]));
    assert!(
        !locked.config.ui.notification_preview,
        "the switch shows the forced value"
    );
    let texts = texts_of(&s, &locked);
    assert!(
        texts.iter().any(|t| t == "Managed by your organization"),
        "{texts:?}"
    );
    let sent = click_row_of(&s, &locked, label);
    assert!(sent.is_empty(), "a locked switch sent {sent:?}");
    // Only the forced switch: the sound switch stays free.
    let managed = texts.iter().filter(|t| t.starts_with("Managed by")).count();
    assert_eq!(managed, 1);

    // With the organization's name.
    let (s, named) = notifications(policy(&[
        ("NotificationPreview", RawValue::Bool(true)),
        ("OrganizationName", RawValue::String("Example Corp".into())),
    ]));
    assert!(named.config.ui.notification_preview);
    let texts = texts_of(&s, &named);
    assert!(
        texts.iter().any(|t| t == "Managed by Example Corp"),
        "{texts:?}"
    );
}

/// `LaunchAtLogin` locks the launch-at-login switch to the forced value with
/// the managed line, also while the state loads and while it waits for the
/// user's approval; `SetAutostart` is refused. Without the key, or where
/// launch at login is unsupported, the switch is as before.
#[test]
fn launch_at_login_switch_is_locked() {
    rust_i18n::set_locale("en");
    let label = "Launch at login";
    let home = tempfile::tempdir().unwrap();
    let at = crate::desktop::autostart::Autostart::LaunchAgent {
        home: home.path().to_owned(),
    };
    let general = |state: Option<crate::desktop::autostart::State>| {
        let mut s = Settings::with_autostart(Some(at.clone())).with_tab(Tab::General);
        s.autostart = state.map(crate::desktop::autostart::State::is_on);
        s.autostart_approval = state == Some(crate::desktop::autostart::State::NeedsApproval);
        s
    };
    use crate::desktop::autostart::State;

    // Free: the switch sends its message.
    let free = Fixture::new(AppConfig::default(), Policy::default());
    let s = general(Some(State::Off));
    assert!(
        !texts_of(&s, &free)
            .iter()
            .any(|t| t.starts_with("Managed by"))
    );
    let sent = click_row_of(&s, &free, label);
    assert!(
        !sent.is_empty()
            && sent
                .iter()
                .all(|m| matches!(m, Message::SetAutostart(true))),
        "{sent:?}"
    );

    for state in [
        None,
        Some(State::Off),
        Some(State::On),
        Some(State::NeedsApproval),
    ] {
        let forced = Fixture::new(
            AppConfig::default(),
            policy(&[("LaunchAtLogin", RawValue::Bool(true))]),
        );
        let s = general(state);
        let texts = texts_of(&s, &forced);
        assert!(
            texts.iter().any(|t| t == "Managed by your organization"),
            "{state:?}: {texts:?}"
        );
        assert_eq!(
            texts
                .iter()
                .any(|t| t.starts_with("Waiting for your approval")),
            state == Some(State::NeedsApproval),
            "{state:?}"
        );
        assert!(click_row_of(&s, &forced, label).is_empty(), "{state:?}");
    }

    // The handler refuses while the key is set.
    let mut s = general(Some(State::Off));
    let mut cfg = AppConfig::default();
    let mut ui_state = crate::ui_state::UiState::default();
    let forced = policy(&[("LaunchAtLogin", RawValue::Bool(false))]);
    let mut ctx = SettingsCtx {
        config: &mut cfg,
        ui_state: &mut ui_state,
        policy: &forced,
    };
    let a = s.update(Message::SetAutostart(true), &mut ctx);
    assert!(a.effects.is_empty() && a.task.units() == 0 && !s.autostart_busy);
    // The key gone: the switch works again.
    let mut ctx = SettingsCtx {
        config: &mut cfg,
        ui_state: &mut ui_state,
        policy: crate::policy::empty(),
    };
    let a = s.update(Message::SetAutostart(true), &mut ctx);
    assert!(a.task.units() > 0 && s.autostart_busy);

    // Unsupported: nothing is shown for the key (the Mac App Store flavor
    // leaves the switch out).
    if cfg!(feature = "mas") {
        return;
    }
    let forced = Fixture::new(
        AppConfig::default(),
        policy(&[("LaunchAtLogin", RawValue::Bool(true))]),
    );
    let s = Settings::with_autostart(None).with_tab(Tab::General);
    let texts = texts_of(&s, &forced);
    assert!(texts.iter().any(|t| t == "Not available"), "{texts:?}");
    assert_eq!(
        texts.iter().filter(|t| t.starts_with("Managed by")).count(),
        0,
        "{texts:?}"
    );
}

/// Every lockable switch the build shows carries the line while forced; the
/// flavors hide what they hide.
#[test]
fn every_lockable_switch_shows_the_line() {
    rust_i18n::set_locale("en");
    let p = policy(&[
        ("HardwareAcceleration", RawValue::Bool(false)),
        ("AllowSecretsFile", RawValue::Bool(false)),
        ("UpdateMode", RawValue::String("notify".into())),
    ]);
    let f = Fixture::new(AppConfig::default(), p);
    let count = |tab: Tab| {
        let s = Settings::with_autostart(None).with_tab(tab);
        texts_of(&s, &f)
            .iter()
            .filter(|t| *t == "Managed by your organization")
            .count()
    };
    let store = usize::from(!cfg!(feature = "mas"));
    assert_eq!(count(Tab::Appearance), store, "renderer switch");
    assert_eq!(count(Tab::Privacy), store, "secrets switch");
    let updater = if cfg!(feature = "self-update") { 2 } else { 0 };
    assert_eq!(count(Tab::About), updater, "updater switches");
    let s = Settings::with_autostart(None).with_tab(Tab::Appearance);
    let texts = texts_of(&s, &f);
    assert_eq!(
        texts.iter().any(|t| t == "Hardware acceleration (GPU)"),
        !cfg!(feature = "mas")
    );
}

/// A memory-only store the policy forced says so, not "as you chose".
#[test]
fn secret_store_forced_to_memory_names_the_organization() {
    rust_i18n::set_locale("en");
    let s = Settings::with_autostart(None).with_tab(Tab::Privacy);
    let texts = |p: Policy, memory_only: bool| {
        let mut user = AppConfig::default();
        user.ui.secrets_memory_only = memory_only;
        let f = Fixture::new(user, p);
        let mut v = f.view();
        v.secret_backend = Some(SecretBackend::MemoryByPolicy);
        crate::tree_probe::texts(s.view(v))
    };
    let chosen = texts(Policy::default(), true);
    assert!(
        chosen.iter().any(|t| t.contains("As you chose")),
        "{chosen:?}"
    );
    assert!(!chosen.iter().any(|t| t.contains("Your organization")));
    if cfg!(feature = "mas") {
        // AllowSecretsFile does not apply to that flavor.
        return;
    }
    let forced = texts(
        policy(&[("AllowSecretsFile", RawValue::Bool(false))]),
        false,
    );
    assert!(
        forced
            .iter()
            .any(|t| t.contains("Your organization does not allow storing")),
        "{forced:?}"
    );
    assert!(
        !forced.iter().any(|t| t.contains("As you chose")),
        "{forced:?}"
    );
}

/// The notice shows on every tab while the policy sets something, and only
/// then; "Details" opens the Policies tab, "Contact support" the link.
#[test]
fn notice_and_its_actions() {
    rust_i18n::set_locale("en");
    let notice = "Some settings are managed by your organization";
    let (s, f) = notifications(Policy::default());
    assert!(!texts_of(&s, &f).iter().any(|t| t.contains("Some settings")));
    // A policy that sets nothing (an unknown key only): no notice, but the
    // Policies tab reports it.
    let typo = policy(&[("RequireHtps", RawValue::Bool(true))]);
    assert!(Tab::offered(&typo).contains(&Tab::Policies));
    let (s, f) = notifications(typo);
    assert!(!texts_of(&s, &f).iter().any(|t| t.contains("Some settings")));

    let p = policy(&[("NotificationPreview", RawValue::Bool(false))]);
    let f = Fixture::new(AppConfig::default(), p);
    for tab in Tab::offered(&f.policy) {
        let s = Settings::with_autostart(None).with_tab(tab);
        let texts = texts_of(&s, &f);
        assert_eq!(
            texts.iter().filter(|t| *t == notice).count(),
            1,
            "{tab:?}: {texts:?}"
        );
        assert!(
            !texts.iter().any(|t| t == "Contact support"),
            "no support URL"
        );
    }
    let mut s = Settings::with_autostart(None).with_tab(Tab::Notifications);
    let sent = click_text(&s, &f, "Details");
    assert!(
        matches!(sent.as_slice(), [Message::Tab(Tab::Policies)]),
        "{sent:?}"
    );
    let mut cfg = f.config.clone();
    let mut ui_state = UiState::default();
    let mut ctx = SettingsCtx {
        config: &mut cfg,
        ui_state: &mut ui_state,
        policy: &f.policy,
    };
    for m in sent {
        let _ = s.update(m, &mut ctx);
    }
    assert_eq!(s.tab, Tab::Policies);
    assert_eq!(s.shown_tab(&f.policy), Tab::Policies);
    let texts = texts_of(&s, &f);
    assert!(texts.iter().any(|t| t == "Policy settings"), "{texts:?}");
    assert!(
        !texts.iter().any(|t| t == "Details"),
        "no Details on its own tab"
    );

    // With the name and the support link.
    let f = Fixture::new(AppConfig::default(), mixed_policy());
    let s = Settings::with_autostart(None).with_tab(Tab::General);
    let texts = texts_of(&s, &f);
    assert!(
        texts
            .iter()
            .any(|t| t == "Some settings are managed by Example Corp"),
        "{texts:?}"
    );
    let sent = click_text(&s, &f, "Contact support");
    assert!(
        matches!(sent.as_slice(), [Message::OpenUrl(u)] if u == "https://help.example.com/chat"),
        "{sent:?}"
    );
}

/// The Policies page lists the blocking states, the rows with their
/// statuses, the sources and the report button, all translated.
#[test]
fn policies_page_content() {
    rust_i18n::set_locale("en");
    let f = Fixture::new(AppConfig::default(), mixed_policy());
    let s = Settings::with_autostart(None).with_tab(Tab::Policies);
    let texts = texts_of(&s, &f);
    let has = |needle: &str| texts.iter().any(|t| t.contains(needle));
    for needle in [
        "Policy settings",
        "NotificationPreview",
        "applied",
        "overridden",
        "invalid",
        "unknown key",
        "ignored: source has limited trust",
        "strict value in force",
        "a number other than 0 or 1",
        "Item 2 (7): not text",
        "Item 3 (\"ftp://files.example.com\"): unsupported scheme (use http or https)",
        "Item 4 (\"*x.example.com\"): ",
        "File 50-company.json",
        "File 90-local.json",
        "full trust",
        "limited trust",
        "the file is owned by user 1000, expected 0",
        "the file is writable by group or others",
        "Copy report",
        policies::dump_command(),
    ] {
        assert!(has(needle), "missing {needle:?} in {texts:?}");
    }
    for t in &texts {
        assert!(!t.starts_with("policy."), "raw key: {t}");
    }
    assert!(!has("Problems that block connections"));

    let f = Fixture::new(AppConfig::default(), blocking_policy());
    let texts = texts_of(&s, &f);
    let has = |needle: &str| texts.iter().any(|t| t.contains(needle));
    for needle in [
        "Problems that block connections",
        "A policy source cannot be read",
        "File 20-broken.json · not valid JSON",
        "linux:20-broken.json",
        "expected `,` or `}` at line 3 column 5",
        "This version is older than the required minimum",
        "required: 99.0.0 or later",
        "Error: not valid JSON",
    ] {
        assert!(has(needle), "missing {needle:?} in {texts:?}");
    }
}

/// Key names and file names that would span lines or reorder the text
/// around them are shown escaped.
#[test]
fn names_are_shown_on_one_line() {
    rust_i18n::set_locale("en");
    let snap = Snapshot::new(
        SourceId::LinuxFile("a\u{202E}nosj.json".into()),
        Trust::Full,
    )
    .with("Foo\nBar", RawValue::Bool(true));
    let mut broken = Snapshot::unreadable(
        SourceId::LinuxFile("b\u{2028}.json".into()),
        SourceErrorKind::InvalidJson,
    );
    broken.trust_problems = vec![TrustProblem::DirectoryChanged];
    let p = resolve(&[snap, broken], &crate::policy::context());
    let f = Fixture::new(AppConfig::default(), p);
    let s = Settings::with_autostart(None).with_tab(Tab::Policies);
    let texts = texts_of(&s, &f);
    let has = |needle: &str| texts.iter().any(|t| t.contains(needle));
    for needle in [
        "Foo\\u000aBar",
        "File a\\u202enosj.json",
        "linux:a\\u202enosj.json",
        "File b\\u2028.json",
        "linux:b\\u2028.json",
    ] {
        assert!(has(needle), "missing {needle:?} in {texts:?}");
    }
    for t in &texts {
        assert!(
            !t.contains(['\n', '\u{202E}', '\u{2028}']),
            "unescaped: {t:?}"
        );
    }
}

/// "and N more" when the core omitted dropped items.
#[test]
fn dropped_items_beyond_the_kept_ones() {
    rust_i18n::set_locale("en");
    let items: Vec<RawValue> = (0..55).map(RawValue::Integer).collect();
    let p = policy(&[("AllowedServers", RawValue::List(items))]);
    let f = Fixture::new(AppConfig::default(), p);
    let s = Settings::with_autostart(None).with_tab(Tab::Policies);
    let texts = texts_of(&s, &f);
    assert!(texts.iter().any(|t| t == "and 5 more"), "{texts:?}");
    assert_eq!(
        texts.iter().filter(|t| t.starts_with("Item ")).count(),
        rustket_core::policy::MAX_DROPPED_REPORTED
    );
}

/// "Copy report" puts the `--dump-policy` JSON on the clipboard and says so.
#[test]
fn copy_report() {
    rust_i18n::set_locale("en");
    let f = Fixture::new(AppConfig::default(), mixed_policy());
    let s = Settings::with_autostart(None).with_tab(Tab::Policies);
    let sent = click_text(&s, &f, "Copy report");
    assert!(
        matches!(sent.as_slice(), [Message::CopyPolicyReport]),
        "{sent:?}"
    );
    let mut s = s;
    let mut cfg = f.config.clone();
    let mut ui_state = UiState::default();
    let mut ctx = SettingsCtx {
        config: &mut cfg,
        ui_state: &mut ui_state,
        policy: &f.policy,
    };
    let a = s.update(Message::CopyPolicyReport, &mut ctx);
    assert_eq!(a.task.units(), 1, "the clipboard write");
    assert!(
        matches!(
            a.effects.as_slice(),
            [Effect::Toast(BannerKind::Success, title, _)] if title == "Report copied to the clipboard"
        ),
        "{:?}",
        a.effects
    );
}

const SHOT_WIDTH: f32 = 1280.0;

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

/// Renders the page of `s` at the shot width and `height` into a PNG.
fn shoot(s: &Settings, f: &Fixture, height: f32, path: &std::path::Path) {
    use iced::advanced::renderer::{Headless, Style};
    let mode = rustket_ui::theme::Mode::Light;
    let theme = rustket_ui::theme(mode);
    let tokens = rustket_ui::theme::active_tokens(mode);
    let size = Size::new(SHOT_WIDTH, height);
    let (mut ui, mut renderer) = ui(s.view(f.view()), size);
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
    let (pixels, _) = rgba.as_chunks::<4>();
    assert!(
        pixels.iter().filter(|p| **p != pixels[0]).count() > 1000,
        "blank"
    );
    let mut out = Vec::new();
    {
        let mut enc = png::Encoder::new(&mut out, w, h);
        enc.set_color(png::ColorType::Rgba);
        enc.set_depth(png::BitDepth::Eight);
        let mut wr = enc.write_header().unwrap();
        wr.write_image_data(&rgba).unwrap();
    }
    std::fs::write(path, out).unwrap();
}

/// The height the tab body needs at the shot width, plus the page frame.
fn page_height(s: &Settings, f: &Fixture) -> f32 {
    use iced::advanced::layout::Limits;
    use iced::advanced::widget::Tree;
    let renderer = iced::Renderer::Secondary(iced_tiny_skia::Renderer::new(
        rustket_ui::theme::font_regular(),
        iced::Pixels(rustket_ui::theme::TEXT_BODY),
    ));
    let v = f.view();
    let mut el: Element<'_, Message> = iced::widget::container(s.tab_body(&v))
        .padding(rustket_ui::theme::SPACE_6)
        .into();
    let mut tree = Tree::new(&el);
    let width = SHOT_WIDTH - super::super::page::NAV_WIDTH;
    let node = el.as_widget_mut().layout(
        &mut tree,
        &renderer,
        &Limits::new(Size::ZERO, Size::new(width, f32::INFINITY)),
    );
    (node.size().height + 90.0).ceil()
}

/// Renders the review pictures (Notifications with a locked switch and the
/// notice; the Policies page with a mixed report and with a blocking state;
/// light theme, English and Russian, 1280 wide) into `$RUSTKET_SHOT_DIR`.
/// It switches the process-wide locale: run it alone
/// (`-- --ignored policy_screenshots`).
#[test]
#[ignore = "writes PNG files; run with RUSTKET_SHOT_DIR set"]
fn policy_screenshots() {
    /// Puts the process-wide locale back to English, also on a panic.
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
    let mut user = AppConfig::default();
    user.ui.notification_preview = true;
    let shots = [
        ("notifications-locked", Tab::Notifications, mixed_policy()),
        (
            "notifications-locked-on",
            Tab::Notifications,
            policy(&[
                ("NotificationPreview", RawValue::Bool(true)),
                (
                    "SupportUrl",
                    RawValue::String("https://help.example.com".into()),
                ),
            ]),
        ),
        ("policies-mixed", Tab::Policies, mixed_policy()),
        ("policies-blocking", Tab::Policies, blocking_policy()),
    ];
    for lang in ["en", "ru"] {
        rust_i18n::set_locale(lang);
        for (name, tab, policy) in &shots {
            let f = Fixture::new(user.clone(), policy.clone());
            let s = Settings::with_autostart(None).with_tab(*tab);
            let height = page_height(&s, &f).max(720.0);
            shoot(
                &s,
                &f,
                height,
                &dir.join(format!("{name}-light-{lang}.png")),
            );
        }
    }
}

/// The `--dump-policy` hint names the executable it is given, quoted when
/// its path has a character outside the safe set: single quotes for
/// PowerShell (after `&`, a quote doubled) and for a POSIX shell (a quote
/// as `'\''`); Windows writes to a file.
#[test]
fn dump_command_names_the_executable() {
    use std::path::Path;
    let unix = [
        ("/opt/rustket/rustket", "/opt/rustket/rustket"),
        (
            "/home/a/My Apps/Rustket.AppImage",
            "'/home/a/My Apps/Rustket.AppImage'",
        ),
        ("/home/o'neil/rustket", r"'/home/o'\''neil/rustket'"),
        ("/opt/a&b/rustket", "'/opt/a&b/rustket'"),
        ("/opt/rustket(1)/rustket", "'/opt/rustket(1)/rustket'"),
        ("/opt/$HOME/rustket", "'/opt/$HOME/rustket'"),
    ];
    for (exe, want) in unix {
        assert_eq!(
            policies::dump_command_for(Path::new(exe), false),
            format!("{want} --dump-policy"),
            "{exe}"
        );
    }
    let windows = [
        (
            r"C:\Users\a\Rustket\rustket.exe",
            r"C:\Users\a\Rustket\rustket.exe",
        ),
        (
            r"C:\Program Files\Rustket\rustket.exe",
            r"& 'C:\Program Files\Rustket\rustket.exe'",
        ),
        (
            r"C:\Users\o'neil\rustket.exe",
            r"& 'C:\Users\o''neil\rustket.exe'",
        ),
        (
            "C:\\o\u{2018}a\u{2019}b\u{201A}c\u{201B}d\\rustket.exe",
            "& 'C:\\o\u{2018}\u{2018}a\u{2019}\u{2019}b\u{201A}\u{201A}c\u{201B}\u{201B}d\\rustket.exe'",
        ),
        (r"C:\a&b\rustket.exe", r"& 'C:\a&b\rustket.exe'"),
        (
            r"C:\Program Files (x86)\rustket.exe",
            r"& 'C:\Program Files (x86)\rustket.exe'",
        ),
        (r"C:\$Recycle\rustket.exe", r"& 'C:\$Recycle\rustket.exe'"),
    ];
    for (exe, want) in windows {
        assert_eq!(
            policies::dump_command_for(Path::new(exe), true),
            format!("{want} --dump-policy policy.json"),
            "{exe}"
        );
    }
    assert!(policies::dump_command().contains("--dump-policy"));
}
