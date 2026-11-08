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

/// `User-Agent` sent with every HTTP request.
pub const USER_AGENT: &str = concat!("Rustket/", env!("CARGO_PKG_VERSION"));
