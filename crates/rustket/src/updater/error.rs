//! Updater errors. Display texts are for logs; the UI maps variants to
//! translated strings.

use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum UpdateError {
    #[error("network error: {0}")]
    Network(String),
    /// `reset`: unix seconds when the next request may succeed.
    #[error("GitHub API rate limit exceeded")]
    RateLimited { reset: Option<i64> },
    #[error("unexpected HTTP status {0}")]
    Http(u16),
    #[error("invalid release data: {0}")]
    BadRelease(String),
    #[error("no package for this system in the release")]
    NoAsset,
    #[error("the release has no checksum for {0}")]
    NoChecksum(String),
    #[error("checksum mismatch")]
    ChecksumMismatch,
    #[error("download has the wrong size")]
    SizeMismatch,
    #[error("{op}: {kind:?}")]
    Io {
        op: &'static str,
        kind: std::io::ErrorKind,
    },
    /// pkexec exit 126. (A declined UAC prompt is not seen: the app has
    /// already quit for the installer.)
    #[error("authentication cancelled")]
    AuthCancelled,
    /// pkexec exit 127, or no pkexec at all.
    #[error("no authentication agent")]
    NoAuthAgent,
    /// The tool's stderr goes to `tracing::debug` only.
    #[error("{tool} failed (exit {code:?})")]
    Tool { tool: String, code: Option<i32> },
    #[error("{} is not writable", .0.display())]
    NotWritable(PathBuf),
    #[error("the app runs from a read-only or translocated location")]
    Translocated,
    #[error("this installation cannot be updated automatically")]
    Unsupported,
    #[error("cancelled")]
    Cancelled,
}

impl UpdateError {
    pub fn io(op: &'static str, e: &std::io::Error) -> Self {
        Self::Io { op, kind: e.kind() }
    }

    /// The file does not match the release's size or checksum.
    pub fn is_mismatch(&self) -> bool {
        matches!(self, Self::ChecksumMismatch | Self::SizeMismatch)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn display() {
        let e = UpdateError::io(
            "write",
            &std::io::Error::from(std::io::ErrorKind::StorageFull),
        );
        assert_eq!(e.to_string(), "write: StorageFull");
        assert_eq!(
            UpdateError::NotWritable(PathBuf::from("/opt/x")).to_string(),
            "/opt/x is not writable"
        );
        assert_eq!(
            UpdateError::Tool {
                tool: "dnf".into(),
                code: Some(1)
            }
            .to_string(),
            "dnf failed (exit Some(1))"
        );
    }
}
