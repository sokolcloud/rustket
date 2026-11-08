//! Renderer choice. iced selects it only from the `ICED_BACKEND` environment
//! variable, so turning hardware acceleration off re-executes the binary with
//! `ICED_BACKEND=tiny-skia` (see `main.rs`).

use std::path::Path;
use std::sync::LazyLock;

use rustket_core::config::{AppConfig, CONFIG_FILE};

/// Read by iced when it creates the compositor.
pub const BACKEND_VAR: &str = "ICED_BACKEND";
/// The CPU renderer's name in `ICED_BACKEND`.
pub const SOFTWARE_BACKEND: &str = "tiny-skia";
/// Set in the environment of the re-executed child (loop guard).
pub const REEXEC_MARKER: &str = "RUSTKET_SOFTWARE_REEXEC";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Renderer {
    Gpu,
    Cpu,
}

impl Renderer {
    /// The renderer iced will try first for an `ICED_BACKEND` value.
    pub fn from_backend(value: Option<&str>) -> Self {
        let first = value
            .and_then(|v| v.split(',').map(str::trim).find(|c| !c.is_empty()))
            .unwrap_or_default();
        if first.eq_ignore_ascii_case(SOFTWARE_BACKEND) {
            Self::Cpu
        } else {
            Self::Gpu
        }
    }
}

/// The renderer requested for this process (read once at startup).
pub fn active() -> Renderer {
    static ACTIVE: LazyLock<Renderer> =
        LazyLock::new(|| Renderer::from_backend(std::env::var(BACKEND_VAR).ok().as_deref()));
    *ACTIVE
}

/// Whether to restart with the CPU renderer: only when the user disabled
/// hardware acceleration and neither `ICED_BACKEND` nor the marker is set.
pub fn should_reexec(backend_set: bool, marker_set: bool, hardware_acceleration: bool) -> bool {
    !hardware_acceleration && !backend_set && !marker_set
}

/// The saved `hardware_acceleration` preference, read without side effects
/// (no repair or move-aside of a broken file); any error yields the default.
pub fn saved_hardware_acceleration(dir: Option<&Path>) -> bool {
    dir.and_then(|d| std::fs::read_to_string(d.join(CONFIG_FILE)).ok())
        .and_then(|text| AppConfig::parse(&text).ok())
        .unwrap_or_default()
        .ui
        .hardware_acceleration
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn reexec_only_when_disabled_and_unset() {
        assert!(should_reexec(false, false, false));
        assert!(!should_reexec(false, false, true));
        assert!(
            !should_reexec(true, false, false),
            "user's ICED_BACKEND wins"
        );
        assert!(!should_reexec(false, true, false), "loop guard");
        assert!(!should_reexec(true, true, true));
    }

    #[test]
    fn backend_value_maps_to_renderer() {
        assert_eq!(Renderer::from_backend(None), Renderer::Gpu);
        assert_eq!(Renderer::from_backend(Some("")), Renderer::Gpu);
        assert_eq!(Renderer::from_backend(Some("wgpu")), Renderer::Gpu);
        assert_eq!(Renderer::from_backend(Some("tiny-skia")), Renderer::Cpu);
        assert_eq!(
            Renderer::from_backend(Some(",tiny-skia,wgpu")),
            Renderer::Cpu
        );
        assert_eq!(
            Renderer::from_backend(Some("wgpu,tiny-skia")),
            Renderer::Gpu
        );
    }

    #[test]
    fn saved_preference_is_read_without_side_effects() {
        let dir = tempfile::tempdir().unwrap();
        assert!(saved_hardware_acceleration(None));
        assert!(saved_hardware_acceleration(Some(dir.path())), "no file");
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "[ui]\nhardware_acceleration = false\n").unwrap();
        assert!(!saved_hardware_acceleration(Some(dir.path())));
        std::fs::write(&path, "not = = toml").unwrap();
        assert!(saved_hardware_acceleration(Some(dir.path())));
        assert!(path.exists(), "a broken file is left in place");
    }
}
