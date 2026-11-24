//! The app sets the process-wide indicator look from the config. A binary of
//! its own: the look is shared by every test in a process.

#![allow(clippy::unwrap_used)]

use rustket::app::{App, Flags, Screen, SecretsMode};
use rustket::component::Nav;
use rustket::message::{Go, Message};
use rustket::screens::settings::Message as Sm;
use rustket_core::config::{AppConfig, IndicatorScheme};
use rustket_ui::theme::{IndicatorLook, generation, indicator_look};

fn start(dir: &std::path::Path) -> App {
    App::new(Flags {
        config_dir: Some(dir.to_owned()),
        cache_dir: Some(dir.join("cache")),
        secrets: SecretsMode::Memory,
        ..Flags::default()
    })
    .0
}

#[tokio::test]
async fn the_look_follows_the_config_and_the_settings() {
    let dir = tempfile::tempdir().unwrap();
    let mut cfg = AppConfig::default();
    cfg.ui.indicator_scheme = IndicatorScheme::Rustket;
    cfg.save(dir.path()).unwrap();
    let mut app = start(dir.path());
    assert_eq!(indicator_look(), IndicatorLook::Rustket);

    let _ = app.update(Message::Go(Go::Nav(Nav::Settings)));
    assert!(matches!(app.screen, Screen::Settings(_)));
    let g = generation();
    let _ = app.update(Message::Settings(Sm::SetIndicatorScheme(
        IndicatorScheme::RocketChat,
    )));
    assert_eq!(app.config.ui.indicator_scheme, IndicatorScheme::RocketChat);
    assert_eq!(indicator_look(), IndicatorLook::RocketChat);
    assert!(generation() > g, "cached views repaint");
    let _ = app.update(Message::Settings(Sm::SetIndicatorScheme(
        IndicatorScheme::Rustket,
    )));
    assert_eq!(indicator_look(), IndicatorLook::Rustket);

    // A config without the key starts with Rocket.Chat's look.
    let fresh = tempfile::tempdir().unwrap();
    std::fs::write(
        fresh.path().join("config.toml"),
        "version = 1\n[ui]\ndensity = \"condensed\"\n",
    )
    .unwrap();
    let second = start(fresh.path());
    assert_eq!(
        second.config.ui.indicator_scheme,
        IndicatorScheme::RocketChat
    );
    assert_eq!(indicator_look(), IndicatorLook::RocketChat);
}
