//! Rocket.Chat protocol client: models, REST, auth flows, TLS.
//!
//! The realtime (DDP) session lives in [`session`], built on `meteor-ddp`.

pub mod auth;
pub mod date;
pub mod e2e;
pub mod error;
pub mod ids;
pub mod models;
pub mod rest;
pub mod server_url;
pub mod session;
pub mod tls;
pub mod uikit;

pub use date::RcDate;
pub use error::{Error, MethodError, Result, TwoFactorChallenge, TwoFactorMethod};
pub use rest::{Credentials, RestClient};
pub use server_url::ServerUrl;
pub use session::{
    LOG_FILTER_PINS, RECOMMENDED_LOG_FILTER, Session, SessionEvent, SessionEvents, SessionOptions,
};
pub use tls::{CertificateInfo, TlsConfig, TlsContext};

/// `User-Agent` sent with every HTTP request and the DDP upgrade. The platform
/// comment lets the server's `ua-parser-js` record the OS of the session; it
/// keeps no name for unknown apps, and a `Mozilla/5.0` prefix is avoided on
/// purpose (bot checks of CDNs flag fake browsers).
pub const USER_AGENT: &str = concat!(
    "Rustket/",
    env!("CARGO_PKG_VERSION"),
    " (",
    ua_platform!(),
    ")"
);

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
macro_rules! ua_platform {
    () => {
        "X11; Linux x86_64"
    };
}
#[cfg(all(target_os = "linux", not(target_arch = "x86_64")))]
macro_rules! ua_platform {
    () => {
        "X11; Linux"
    };
}
#[cfg(all(target_os = "windows", target_arch = "aarch64"))]
macro_rules! ua_platform {
    () => {
        "Windows NT 10.0; ARM64"
    };
}
#[cfg(all(target_os = "windows", not(target_arch = "aarch64")))]
macro_rules! ua_platform {
    () => {
        "Windows NT 10.0; Win64; x64"
    };
}
#[cfg(target_os = "macos")]
macro_rules! ua_platform {
    () => {
        "Macintosh; Mac OS X"
    };
}
#[cfg(not(any(target_os = "linux", target_os = "windows", target_os = "macos")))]
macro_rules! ua_platform {
    () => {
        "Unknown"
    };
}
use ua_platform;

#[cfg(test)]
mod ua_tests {
    #[test]
    fn user_agent_names_the_platform() {
        assert!(super::USER_AGENT.starts_with(concat!(
            "Rustket/",
            env!("CARGO_PKG_VERSION"),
            " ("
        )));
        assert!(super::USER_AGENT.ends_with(')'));
    }
}
