//! Administration endpoints (docs/admin.md). Admin request and response bodies may
//! contain secrets: never log them.
//!
//! 2FA-protected calls take `two_factor: Option<&TwoFactorCode>`: call without a
//! code first, and on [`crate::Error::TwoFactorRequired`] retry with the code for the
//! challenge's method (`TwoFactorCode::password` sends the SHA-256 hex of the
//! password with `x-2fa-method: password`). Apps (`/api/apps/*`) are not here.

mod ee;
mod email_inbox;
mod emoji;
mod integrations;
mod invites;
mod ldap;
mod mailer;
mod moderation;
mod oauth_apps;
mod permissions;
mod rooms;
mod settings;
mod sounds;
mod stats;
mod user_status;
mod users;

pub use moderation::ModerationFilter;
pub use settings::server_translations_from_bytes;
pub use sounds::MAX_SOUND_BYTES;

use std::fmt;

use super::Query;
use crate::rest::{ProgressFn, UploadSource};

/// Sort of list endpoints, serialised as the JSON string `{"field":1,…}` in key order.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Sort(Vec<(String, i8)>);

impl Sort {
    pub fn asc(field: &str) -> Self {
        Self(vec![(field.to_owned(), 1)])
    }

    pub fn desc(field: &str) -> Self {
        Self(vec![(field.to_owned(), -1)])
    }

    /// Adds a secondary key.
    pub fn then(mut self, field: &str, ascending: bool) -> Self {
        self.0
            .push((field.to_owned(), if ascending { 1 } else { -1 }));
        self
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn to_json(&self) -> String {
        // Built by hand: `serde_json::Map` would reorder the keys.
        let entries: Vec<String> = self
            .0
            .iter()
            .map(|(k, v)| format!("{}:{v}", serde_json::Value::String(k.clone())))
            .collect();
        format!("{{{}}}", entries.join(","))
    }
}

/// Paging of admin lists. `count` is capped by the server (`API_Upper_Count_Limit`,
/// 100 on the dev server); `None` uses the server default (50).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Page {
    pub count: Option<u64>,
    pub offset: u64,
    pub sort: Option<Sort>,
}

impl Page {
    pub fn new(offset: u64, count: u64) -> Self {
        Self {
            count: Some(count),
            offset,
            sort: None,
        }
    }

    pub fn sorted(mut self, sort: Sort) -> Self {
        self.sort = Some(sort);
        self
    }

    pub(crate) fn apply(&self, q: Query) -> Query {
        q.opt("count", self.count).push("offset", self.offset).opt(
            "sort",
            self.sort
                .as_ref()
                .filter(|s| !s.is_empty())
                .map(Sort::to_json),
        )
    }
}

/// [`crate::rest::paginated`] that falls back to the request's paging when the
/// response lacks `offset`/`count` (e.g. `roles.getUsersInRole` answers only
/// `{users, total}`).
pub(crate) fn paged<T: serde::de::DeserializeOwned>(
    v: serde_json::Value,
    key: &str,
    page: &Page,
) -> crate::Result<crate::models::Paginated<T>> {
    let has = |k: &str| v.get(k).and_then(serde_json::Value::as_u64).is_some();
    let (has_offset, has_count) = (has("offset"), has("count"));
    let mut p = crate::rest::paginated(v, key)?;
    if !has_offset {
        p.offset = page.offset;
    }
    if !has_count {
        p.count = p.raw_len as u64;
    }
    Ok(p)
}

/// A file for an admin multipart upload (emoji, sound, asset, avatar). Emoji take
/// their extension from `mime`, sounds from the extension of `filename`
/// (docs/admin.md §2.4).
pub struct AdminUpload {
    pub filename: String,
    pub mime: String,
    pub source: UploadSource,
    pub progress: Option<ProgressFn>,
}

impl AdminUpload {
    pub fn new(
        filename: impl Into<String>,
        mime: impl Into<String>,
        data: impl Into<bytes::Bytes>,
    ) -> Self {
        Self {
            filename: filename.into(),
            mime: mime.into(),
            source: UploadSource::Bytes(data.into()),
            progress: None,
        }
    }
}

impl fmt::Debug for AdminUpload {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AdminUpload")
            .field("filename", &self.filename)
            .field("mime", &self.mime)
            .field("source", &self.source)
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn sort_and_page() {
        let s = Sort::asc("name").then("default", false);
        assert_eq!(s.to_json(), r#"{"name":1,"default":-1}"#);
        let q = Page::new(10, 25)
            .sorted(Sort::desc("ts"))
            .apply(Query::new());
        assert_eq!(
            q.apply("x"),
            "x?count=25&offset=10&sort=%7B%22ts%22%3A-1%7D"
        );
        assert_eq!(Page::default().apply(Query::new()).apply("x"), "x?offset=0");
    }
}
