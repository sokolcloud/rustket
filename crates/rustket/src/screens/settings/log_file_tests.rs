//! The diagnostic log file switch and "Show log folder" (Privacy & storage
//! tab), rendered headless.

use rustket_core::config::AppConfig;
use rustket_core::policy::Policy;

use super::policies_tests::{Fixture, click_row_of, click_text, texts_of};
use super::*;

fn privacy(log_file: bool) -> (Settings, Fixture) {
    rust_i18n::set_locale("en");
    let mut user = AppConfig::default();
    user.ui.log_file = log_file;
    (
        Settings::with_autostart(None).with_tab(Tab::Privacy),
        Fixture::new(user, Policy::default()),
    )
}

#[test]
fn switch_and_folder_are_shown() {
    for on in [false, true] {
        let (s, f) = privacy(on);
        let texts = texts_of(&s, &f);
        for label in [
            "Keep a diagnostic log file",
            "Show log folder",
            "Takes effect after Rustket is restarted. The log holds no passwords, tokens or message text.",
        ] {
            assert!(texts.iter().any(|t| t == label), "{label}: {texts:?}");
        }
        if let Some(dir) = crate::logging::default_dir() {
            let dir = dir.display().to_string();
            assert!(texts.contains(&dir), "{dir}: {texts:?}");
        }
    }
}

#[test]
fn switch_sets_the_preference() {
    for on in [false, true] {
        let (mut s, mut f) = privacy(on);
        let sent = click_row_of(&s, &f, "Keep a diagnostic log file");
        assert!(
            sent.iter()
                .any(|m| matches!(m, Message::SetLogFile(v) if *v == !on)),
            "{sent:?}"
        );
        let mut ui_state = UiState::default();
        let mut ctx = SettingsCtx {
            config: &mut f.config,
            ui_state: &mut ui_state,
            policy: crate::policy::empty(),
        };
        let a = s.update(Message::SetLogFile(!on), &mut ctx);
        assert_eq!(f.config.ui.log_file, !on);
        assert!(matches!(a.effects.as_slice(), [Effect::SaveConfig]));
    }
}

#[test]
fn show_log_folder_opens_the_log_folder() {
    let (mut s, mut f) = privacy(false);
    let sent = click_text(&s, &f, "Show log folder");
    assert!(
        sent.iter().any(|m| matches!(m, Message::OpenLogFolder)),
        "{sent:?}"
    );
    let mut ui_state = UiState::default();
    let mut ctx = SettingsCtx {
        config: &mut f.config,
        ui_state: &mut ui_state,
        policy: crate::policy::empty(),
    };
    let a = s.update(Message::OpenLogFolder, &mut ctx);
    match (crate::logging::default_dir(), a.effects.as_slice()) {
        (Some(dir), [Effect::OpenLogFolder(path)]) => assert_eq!(path, &dir),
        (None, [Effect::Toast(BannerKind::Error, ..)]) => {}
        (dir, effects) => panic!("{dir:?}: {effects:?}"),
    }
    assert!(
        !f.config.ui.log_file,
        "the folder action changes no preference"
    );
}
