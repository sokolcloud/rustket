//! Small UI state persisted next to the config (`ui-state.toml`): collapsed
//! sidebar sections and sidebar width. `AppConfig` has no field for these yet.

use std::collections::BTreeSet;
use std::path::Path;

use serde::{Deserialize, Serialize};

pub const FILE: &str = "ui-state.toml";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct UiState {
    /// Collapse keys of collapsed sidebar sections (`SectionId::collapse_key`,
    /// omnichannel section keys), shared by all servers.
    pub collapsed_sections: BTreeSet<String>,
    pub sidebar_width: f32,
}

impl Default for UiState {
    fn default() -> Self {
        Self {
            collapsed_sections: BTreeSet::new(),
            sidebar_width: rustket_ui::theme::SIDEBAR_WIDTH,
        }
    }
}

impl UiState {
    pub fn load(dir: &Path) -> Self {
        std::fs::read_to_string(dir.join(FILE))
            .ok()
            .and_then(|t| toml::from_str(&t).ok())
            .unwrap_or_default()
    }

    pub fn save(&self, dir: &Path) -> std::io::Result<()> {
        let text = toml::to_string_pretty(self).map_err(std::io::Error::other)?;
        std::fs::create_dir_all(dir)?;
        use std::sync::atomic::{AtomicU64, Ordering};
        static N: AtomicU64 = AtomicU64::new(0);
        let tmp = dir.join(format!(
            "{FILE}.tmp{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::write(&tmp, text)?;
        std::fs::rename(tmp, dir.join(FILE))
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(UiState::load(dir.path()), UiState::default());
        let mut s = UiState::default();
        s.collapsed_sections.insert("sidebar.channels".into());
        s.sidebar_width = 300.0;
        s.save(dir.path()).unwrap();
        assert_eq!(UiState::load(dir.path()), s);
    }
}
