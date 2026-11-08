use rustket::app::{App, Flags};
use rustket::renderer;
use rustket::single_instance::{self, Instance, ListenerSlot};

/// The log filter: the user's `RUST_LOG` (or the recommended default), always
/// followed by `rocketchat::LOG_FILTER_PINS` so transport crates that log
/// credentials at TRACE (tungstenite logs the resume token) stay at `info`.
/// Toolkit crates that are noisy at `info`: `iced_winit` dumps the window
/// attributes including every byte of the icon, `winit` warns about XSETTINGS
/// on every start under KDE.
const QUIET_DEPS: &str = "wgpu=warn,naga=warn,iced_winit=warn,winit=error";

fn log_filter(user: Option<&str>) -> String {
    match user.map(str::trim).filter(|f| !f.is_empty()) {
        Some(f) => format!("{f},{}", rocketchat::LOG_FILTER_PINS),
        None => format!("{},{QUIET_DEPS}", rocketchat::RECOMMENDED_LOG_FILTER),
    }
}

/// Restarts the process with the CPU renderer when hardware acceleration is
/// off. Returns only when no restart is needed or it failed (the error).
fn software_reexec() -> Option<std::io::Error> {
    let backend_set = std::env::var_os(renderer::BACKEND_VAR).is_some();
    let marker_set = std::env::var_os(renderer::REEXEC_MARKER).is_some();
    if backend_set || marker_set {
        return None;
    }
    let dir = rustket_core::config::AppConfig::default_dir().ok();
    let hw = renderer::saved_hardware_acceleration(dir.as_deref());
    if !renderer::should_reexec(backend_set, marker_set, hw) {
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

fn main() -> iced::Result {
    // Before anything else: the restarted process redoes all of it.
    let reexec_error = software_reexec();
    let user_filter = std::env::var("RUST_LOG").ok();
    let filter = log_filter(user_filter.as_deref());
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_new(&filter).unwrap_or_else(|_| {
                tracing_subscriber::EnvFilter::new(rocketchat::RECOMMENDED_LOG_FILTER)
            }),
        )
        .init();
    if let Some(e) = reexec_error {
        tracing::warn!(error = %e, "cannot restart with the software renderer; using the default");
    }
    tracing::info!(renderer = ?renderer::active(), "renderer requested");

    let urls: Vec<String> = std::env::args()
        .skip(1)
        .filter(|a| !a.starts_with('-'))
        .collect();
    let path = single_instance::socket_path();
    let mut lock = None;
    let listener = match &path {
        None => {
            tracing::warn!("no private directory for the single-instance socket");
            None
        }
        Some(path) => match single_instance::acquire(path, &urls) {
            Ok(Instance::Forwarded) => {
                tracing::info!(
                    "forwarded {} argument(s) to the running instance",
                    urls.len()
                );
                return Ok(());
            }
            Ok(Instance::Primary(l, file)) => {
                lock = Some(file);
                Some(ListenerSlot::new(l))
            }
            Err(e) => {
                tracing::warn!(path = %path.display(), error = %e, "single-instance socket unavailable");
                None
            }
        },
    };

    let primary = listener.is_some();
    let config_dir = rustket_core::config::AppConfig::default_dir().ok();
    let decorations = rustket::workspace::titlebar::saved_system_title_bar(config_dir.as_deref());
    let flags = Flags {
        urls,
        listener,
        open_urls: true,
        desktop: true,
        window_decorated: Some(decorations),
        offline_cache: rustket::cache::default_base(),
        ..Flags::default()
    };
    let result = iced::application(move || App::new(flags.clone()), App::update, App::view)
        .settings(rustket_ui::settings())
        .title(App::title)
        .theme(App::theme)
        .subscription(App::subscription)
        .scale_factor(App::scale_factor)
        .window(iced::window::Settings {
            icon: rustket::desktop::icon::window_icon(),
            decorations,
            min_size: Some(rustket::workspace::titlebar::MIN_WINDOW_SIZE),
            platform_specific: iced::window::settings::PlatformSpecific {
                application_id: "rustket".into(),
                ..Default::default()
            },
            ..Default::default()
        })
        .window_size((1280.0, 820.0))
        // Closing goes through `Message::CloseRequested` (flushes the config).
        .exit_on_close_request(false)
        .run();
    // Only the lock holder removes the socket, before releasing the lock.
    if primary && let Some(path) = path {
        let _ = std::fs::remove_file(&path);
    }
    drop(lock);
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Session re-review #2: the pins always follow the user's filter.
    #[test]
    fn log_filter_appends_pins() {
        let f = log_filter(Some("trace"));
        assert!(f.starts_with("trace,"));
        assert!(f.ends_with(rocketchat::LOG_FILTER_PINS));
        assert!(f.contains("tungstenite=info"));
        assert!(log_filter(None).starts_with(rocketchat::RECOMMENDED_LOG_FILTER));
        assert!(log_filter(Some("  ")).starts_with(rocketchat::RECOMMENDED_LOG_FILTER));
        assert!(log_filter(None).contains("iced_winit=warn"));
    }
}
