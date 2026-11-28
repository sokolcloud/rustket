// Release builds on Windows open no console window.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

use rustket::app::{App, Flags};
use rustket::renderer;
use rustket::single_instance::{self, Instance, ListenerSlot};

/// Restarts the process with the CPU renderer when hardware acceleration is
/// off (by policy, else by preference). Returns only when no restart is
/// needed or it failed (the error). Never in the Mac App Store flavor: a
/// sandboxed exec of itself is not supported there (and the switch is
/// hidden).
fn software_reexec(policy: &rustket_core::policy::Policy) -> Option<std::io::Error> {
    let backend_set = std::env::var_os(renderer::BACKEND_VAR).is_some();
    let marker_set = std::env::var_os(renderer::REEXEC_MARKER).is_some();
    let restart = renderer::software_restart_wanted(
        cfg!(feature = "mas"),
        backend_set,
        marker_set,
        policy.hardware_acceleration(),
        || {
            let dir = rustket_core::config::AppConfig::default_dir().ok();
            renderer::saved_hardware_acceleration(dir.as_deref())
        },
    );
    if !restart {
        return None;
    }
    let exe = match std::env::current_exe() {
        Ok(exe) => exe,
        Err(e) => return Some(e),
    };
    let mut args = std::env::args_os();
    let arg0 = args.next();
    let mut cmd = std::process::Command::new(exe);
    cmd.args(args)
        .env(renderer::BACKEND_VAR, renderer::SOFTWARE_BACKEND)
        .env(renderer::REEXEC_MARKER, "1");
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        if let Some(arg0) = arg0 {
            cmd.arg0(arg0);
        }
        // Replaces this process; returns only on failure.
        Some(cmd.exec())
    }
    #[cfg(not(unix))]
    {
        let _ = arg0;
        match cmd.status() {
            Ok(status) => std::process::exit(status.code().unwrap_or(1)),
            Err(e) => Some(e),
        }
    }
}

/// Linux: the application id (`WM_CLASS`, Wayland `app_id`). macOS: the
/// traffic lights over the content (a transparent, untitled title bar and a
/// full-size content view); the app draws its title row under them.
fn platform_specific() -> iced::window::settings::PlatformSpecific {
    #[cfg(target_os = "linux")]
    {
        iced::window::settings::PlatformSpecific {
            application_id: rustket::APP_ID.into(),
            ..Default::default()
        }
    }
    #[cfg(target_os = "macos")]
    {
        iced::window::settings::PlatformSpecific {
            title_hidden: true,
            titlebar_transparent: true,
            fullsize_content_view: true,
        }
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    iced::window::settings::PlatformSpecific::default()
}

fn main() -> iced::Result {
    // `--help`/`--version` print and exit before any GUI or re-exec work.
    rust_i18n::set_locale(&rustket::i18n::system());
    match rustket::cli::parse(std::env::args_os().skip(1)) {
        rustket::cli::Action::Print(text) => {
            // A closed pipe (`| head`) is not an error worth a panic.
            use std::io::Write as _;
            let _ = std::io::stdout().write_all(text.as_bytes());
            return Ok(());
        }
        rustket::cli::Action::Fail(text) => {
            eprint!("{text}");
            std::process::exit(2);
        }
        rustket::cli::Action::DumpPolicy(out) => {
            let policy = rustket::policy::load();
            if let Err(e) = rustket::policy::dump(&policy, out.as_deref()) {
                eprintln!("{}", e.localized());
                std::process::exit(2);
            }
            return Ok(());
        }
        rustket::cli::Action::Run => {}
    }
    // Before logging and the renderer restart: no side effects, cheap (the
    // restarted process resolves it again).
    let (policy_sources, policy) = rustket::policy::load_with_sources();
    // A relaunch after an update waits until the old process has exited.
    #[cfg(feature = "self-update")]
    let waited = rustket::updater::relaunch::wait_for_parent_if_requested();
    // Before anything else: the restarted process redoes all of it.
    let reexec_error = software_reexec(&policy);
    let user_filter = std::env::var("RUST_LOG").ok();
    // Read like the renderer preference: no side effects, any error is off.
    let sink = rustket::logging::start_sink(
        rustket_core::config::AppConfig::default_dir()
            .ok()
            .as_deref(),
        rustket::logging::default_dir,
    );
    rustket::logging::init(user_filter.as_deref(), sink);
    rustket::policy::log_summary(&policy);
    #[cfg(feature = "self-update")]
    waited.log();
    if let Some(e) = reexec_error {
        tracing::warn!(error = %e, "cannot restart with the software renderer; using the default");
    }
    tracing::info!(renderer = ?renderer::active(), "renderer requested");
    #[cfg(target_os = "macos")]
    rustket::desktop::sandbox::check_flavor();

    let urls: Vec<String> = std::env::args()
        .skip(1)
        .filter(|a| !a.starts_with('-'))
        .collect();
    let endpoint = single_instance::endpoint();
    // Kept until `main` returns; on unix dropping it removes the socket, then
    // releases the instance lock.
    let (listener, _guard) = match &endpoint {
        None => {
            tracing::warn!("no usable single-instance endpoint");
            (None, None)
        }
        Some(endpoint) => match single_instance::acquire(endpoint, &urls) {
            Ok(Instance::Forwarded) => {
                tracing::info!(
                    "forwarded {} argument(s) to the running instance",
                    urls.len()
                );
                return Ok(());
            }
            Ok(Instance::Primary(l, g)) => (Some(ListenerSlot::new(l)), Some(g)),
            Ok(Instance::LockOnly(g)) => {
                tracing::info!("single instance without forwarding (socket path too long)");
                (None, Some(g))
            }
            Ok(Instance::AlreadyRunning) => {
                tracing::info!("another instance is running and cannot be reached; exiting");
                return Ok(());
            }
            Err(e) => {
                tracing::warn!(?endpoint, error = %e, "single-instance endpoint unavailable");
                (None, None)
            }
        },
    };

    #[cfg(target_os = "macos")]
    {
        rustket::macos_events::install();
        rustket::desktop::notifications::install();
    }
    let config_dir = rustket_core::config::AppConfig::default_dir().ok();
    // Before `rustket_ui::settings()`: the theme's fonts are fixed per session.
    rustket::themes::init_fonts(config_dir.as_deref());
    // After the theme's font check: Medium/SemiBold CJK, Thai, … text then
    // falls back to the script's family instead of any face with that weight.
    rustket_ui::fonts::alias_missing_weights();
    let system_title_bar =
        rustket::workspace::titlebar::saved_system_title_bar(config_dir.as_deref());
    // macOS keeps the native frame and traffic lights in either layout.
    let decorations = system_title_bar || rustket::workspace::titlebar::NATIVE_FRAME;
    let flags = Flags {
        urls,
        listener,
        open_urls: true,
        desktop: true,
        window_decorated: Some(system_title_bar),
        offline_cache: rustket::cache::default_base(),
        policy,
        policy_sources,
        policy_refresh: true,
        ..Flags::default()
    };
    iced::application(move || App::new(flags.clone()), App::update, App::view)
        .settings(rustket_ui::settings())
        .title(App::title)
        .theme(App::theme)
        .subscription(App::subscription)
        .scale_factor(App::scale_factor)
        .window(iced::window::Settings {
            icon: rustket::desktop::icon::window_icon(),
            decorations,
            min_size: Some(rustket::workspace::titlebar::MIN_WINDOW_SIZE),
            platform_specific: platform_specific(),
            ..Default::default()
        })
        .window_size((1280.0, 820.0))
        // Closing goes through `Message::CloseRequested` (flushes the config).
        .exit_on_close_request(false)
        .run()
}
