//! Administration models (docs/admin.md). Admin payloads may contain secrets in
//! clear text: `Debug` impls redact them, and nothing here should be logged or
//! persisted.

mod integrations;
mod misc;
mod moderation;
mod rooms;
mod settings;
mod stats;
mod users;

pub use integrations::*;
pub use misc::*;
pub use moderation::*;
pub use rooms::*;
pub use settings::*;
pub use stats::*;
pub use users::*;
