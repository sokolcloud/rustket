//! Message search through the server's search provider
//! (`rocketchatSearch.getProvider` / `rocketchatSearch.search`).

use serde::{Deserialize, Serialize};

use super::Message;

/// Key of the built-in MongoDB provider (`DefaultProvider`).
pub const DEFAULT_SEARCH_PROVIDER: &str = "defaultProvider";

/// The active search provider (`rocketchatSearch.getProvider`; any logged-in
/// user may call it).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SearchProvider {
    pub key: String,
    pub settings: SearchProviderSettings,
}

/// Settings the provider exposes. The default provider has
/// `Search.defaultProvider.GlobalSearchEnabled` (off by default) and
/// `PageSize`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SearchProviderSettings {
    #[serde(rename = "GlobalSearchEnabled")]
    pub global_search_enabled: bool,
    #[serde(rename = "PageSize", skip_serializing_if = "Option::is_none")]
    pub page_size: Option<u32>,
}

impl SearchProvider {
    /// The default provider with its server-wide search switched on.
    pub fn global_search(&self) -> bool {
        self.key == DEFAULT_SEARCH_PROVIDER && self.settings.global_search_enabled
    }

    pub fn is_default(&self) -> bool {
        self.key == DEFAULT_SEARCH_PROVIDER
    }
}

/// `rocketchatSearch.search` result: `{message: {docs: [...]}}`; the docs are
/// raw messages (DDP dates) with the room's `r: {name, t}` added.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct SearchResult {
    pub message: Option<SearchDocs>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct SearchDocs {
    pub docs: Vec<Message>,
}
