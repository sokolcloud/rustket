//! The macOS App Sandbox: whether this process runs in one (for diagnostics
//! and the single-instance paths). The Mac App Store flavor (`mas` feature)
//! is expected to be sandboxed, every other build not.

/// The environment variable macOS sets in sandboxed processes (the
/// container's bundle id).
pub const CONTAINER_VAR: &str = "APP_SANDBOX_CONTAINER_ID";

/// Whether the process runs in an App Sandbox container.
pub fn sandboxed() -> bool {
    sandboxed_by(std::env::var_os(CONTAINER_VAR).as_deref())
}

/// [`sandboxed`] for a value of [`CONTAINER_VAR`].
pub fn sandboxed_by(container: Option<&std::ffi::OsStr>) -> bool {
    container.is_some_and(|v| !v.is_empty())
}

/// Logs a warning when the build flavor and the sandbox disagree (a store
/// build without its entitlements, or a dmg build run sandboxed).
pub fn check_flavor() {
    let sandboxed = sandboxed();
    let mas = cfg!(feature = "mas");
    if mas != sandboxed {
        tracing::warn!(mas, sandboxed, "build flavor and App Sandbox disagree");
    } else if sandboxed {
        tracing::info!("running in the App Sandbox");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsStr;

    #[test]
    fn container_variable() {
        assert!(!sandboxed_by(None));
        assert!(!sandboxed_by(Some(OsStr::new(""))));
        assert!(sandboxed_by(Some(OsStr::new(
            "io.github.sokolcloud.rustket"
        ))));
    }
}
