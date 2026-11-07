use rustket::app::{App, Flags};
use rustket::single_instance::{self, Instance, ListenerSlot};

/// The log filter: the user's `RUST_LOG` (or the recommended default), always
/// followed by `rocketchat::LOG_FILTER_PINS` so transport crates that log
/// credentials at TRACE (tungstenite logs the resume token) stay at `info`.
fn log_filter(user: Option<&str>) -> String {
    match user.map(str::trim).filter(|f| !f.is_empty()) {
        Some(f) => format!("{f},{}", rocketchat::LOG_FILTER_PINS),
        None => format!("{},wgpu=warn,naga=warn", rocketchat::RECOMMENDED_LOG_FILTER),
    }
}

fn main() -> iced::Result {
    let user_filter = std::env::var("RUST_LOG").ok();
    let filter = log_filter(user_filter.as_deref());
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_new(&filter).unwrap_or_else(|_| {
                tracing_subscriber::EnvFilter::new(rocketchat::RECOMMENDED_LOG_FILTER)
            }),
        )
        .init();

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
    let flags = Flags {
        urls,
        listener,
        open_urls: true,
        ..Flags::default()
    };
    let result = iced::application(move || App::new(flags.clone()), App::update, App::view)
        .settings(rustket_ui::settings())
        .title(App::title)
        .theme(App::theme)
        .subscription(App::subscription)
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
    }
}
