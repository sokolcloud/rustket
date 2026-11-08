//! Public settings map with typed getters for the settings the UI needs.
//! Defaults follow Rocket.Chat's own defaults when a setting is missing.

use std::collections::HashMap;

use rocketchat::models::Setting;
use serde_json::Value;

#[derive(Debug, Clone, Default)]
pub struct Settings {
    map: HashMap<String, Value>,
}

impl Settings {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn from_list(list: impl IntoIterator<Item = Setting>) -> Self {
        let mut s = Self::new();
        for setting in list {
            s.set(setting);
        }
        s
    }

    pub fn set(&mut self, s: Setting) {
        self.map.insert(s.id, s.value);
    }

    pub fn set_value(&mut self, id: impl Into<String>, value: Value) {
        self.map.insert(id.into(), value);
    }

    pub fn remove(&mut self, id: &str) -> Option<Value> {
        self.map.remove(id)
    }

    pub fn len(&self) -> usize {
        self.map.len()
    }

    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }

    pub fn get(&self, id: &str) -> Option<&Value> {
        self.map.get(id)
    }

    /// Every setting as `(id, value)`, in no particular order.
    pub fn iter(&self) -> impl Iterator<Item = (&str, &Value)> + '_ {
        self.map.iter().map(|(k, v)| (k.as_str(), v))
    }

    pub fn bool(&self, id: &str) -> Option<bool> {
        match self.map.get(id)? {
            Value::Bool(b) => Some(*b),
            Value::String(s) if s == "true" => Some(true),
            Value::String(s) if s == "false" => Some(false),
            _ => None,
        }
    }

    pub fn bool_or(&self, id: &str, default: bool) -> bool {
        self.bool(id).unwrap_or(default)
    }

    pub fn i64(&self, id: &str) -> Option<i64> {
        match self.map.get(id)? {
            Value::Number(n) => n.as_i64().or_else(|| n.as_f64().map(|f| f as i64)),
            Value::String(s) => s.trim().parse().ok(),
            _ => None,
        }
    }

    pub fn i64_or(&self, id: &str, default: i64) -> i64 {
        self.i64(id).unwrap_or(default)
    }

    pub fn str(&self, id: &str) -> Option<&str> {
        self.map.get(id)?.as_str()
    }

    fn list(&self, id: &str) -> Vec<String> {
        self.str(id)
            .map(|s| {
                s.split(',')
                    .map(str::trim)
                    .filter(|p| !p.is_empty())
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_default()
    }

    pub fn site_name(&self) -> &str {
        self.str("Site_Name").unwrap_or("Rocket.Chat")
    }
    pub fn use_real_name(&self) -> bool {
        self.bool_or("UI_Use_Real_Name", false)
    }
    /// Channels/groups show `fname` (names with special characters) when set.
    pub fn allow_special_chars_in_room_names(&self) -> bool {
        self.bool_or("UI_Allow_room_names_with_special_chars", false)
    }
    pub fn allow_editing(&self) -> bool {
        self.bool_or("Message_AllowEditing", true)
    }
    /// 0 = no limit.
    pub fn edit_block_minutes(&self) -> i64 {
        self.i64_or("Message_AllowEditing_BlockEditInMinutes", 0)
    }
    pub fn allow_deleting(&self) -> bool {
        self.bool_or("Message_AllowDeleting", true)
    }
    /// 0 = no limit.
    pub fn delete_block_minutes(&self) -> i64 {
        self.i64_or("Message_AllowDeleting_BlockDeleteInMinutes", 0)
    }
    pub fn allow_pinning(&self) -> bool {
        self.bool_or("Message_AllowPinning", true)
    }
    pub fn allow_starring(&self) -> bool {
        self.bool_or("Message_AllowStarring", true)
    }
    /// Seconds within which consecutive messages of one author are grouped.
    pub fn grouping_period_secs(&self) -> i64 {
        self.i64_or("Message_GroupingPeriod", 300)
    }
    pub fn show_edited_status(&self) -> bool {
        self.bool_or("Message_ShowEditedStatus", true)
    }
    pub fn max_message_size(&self) -> usize {
        usize::try_from(self.i64_or("Message_MaxAllowedSize", 5000)).unwrap_or(5000)
    }
    pub fn read_receipts_enabled(&self) -> bool {
        self.bool_or("Message_Read_Receipt_Enabled", false)
    }
    pub fn threads_enabled(&self) -> bool {
        self.bool_or("Threads_enabled", true)
    }
    pub fn discussions_enabled(&self) -> bool {
        self.bool_or("Discussion_enabled", true)
    }
    pub fn file_upload_enabled(&self) -> bool {
        self.bool_or("FileUpload_Enabled", true)
    }
    /// Bytes; `None` or negative = unlimited. Rocket.Chat's default (100 MiB)
    /// applies when the setting is missing.
    pub fn file_upload_max_size(&self) -> Option<u64> {
        u64::try_from(self.i64_or("FileUpload_MaxFileSize", 104_857_600)).ok()
    }
    /// Allowed MIME patterns; empty = everything (RC `fileUploadMediaWhiteList`:
    /// an empty value or `*` means no whitelist).
    pub fn media_type_whitelist(&self) -> Vec<String> {
        match self.str("FileUpload_MediaTypeWhiteList") {
            Some(s) if s.trim() == "*" => Vec::new(),
            _ => self.list("FileUpload_MediaTypeWhiteList"),
        }
    }
    /// Refused MIME patterns (default `image/svg+xml`, as in Rocket.Chat).
    pub fn media_type_blacklist(&self) -> Vec<String> {
        if self.get("FileUpload_MediaTypeBlackList").is_none() {
            return vec!["image/svg+xml".to_owned()];
        }
        self.list("FileUpload_MediaTypeBlackList")
    }
    /// Whether a MIME type may be uploaded (RC `fileUploadIsValidContentType`).
    pub fn is_upload_type_allowed(&self, mime: &str) -> bool {
        let mime = mime.trim().to_ascii_lowercase();
        let matches = |pattern: &String| {
            let p = pattern.to_ascii_lowercase();
            if p == mime {
                return true;
            }
            match p.strip_suffix("/*") {
                Some(prefix) => mime.split('/').next() == Some(prefix),
                None => false,
            }
        };
        if self.media_type_blacklist().iter().any(matches) {
            return false;
        }
        let white = self.media_type_whitelist();
        white.is_empty() || white.iter().any(matches)
    }
    pub fn e2e_enabled(&self) -> bool {
        self.bool_or("E2E_Enable", false)
    }
    pub fn allow_status_message_change(&self) -> bool {
        self.bool_or("Accounts_AllowUserStatusMessageChange", true)
    }
    pub fn allow_real_name_change(&self) -> bool {
        self.bool_or("Accounts_AllowRealNameChange", true)
    }
    pub fn allow_username_change(&self) -> bool {
        self.bool_or("Accounts_AllowUsernameChange", true)
    }
    pub fn allow_email_change(&self) -> bool {
        self.bool_or("Accounts_AllowEmailChange", true)
    }
    pub fn allow_password_change(&self) -> bool {
        self.bool_or("Accounts_AllowPasswordChange", true)
    }
    pub fn allow_avatar_change(&self) -> bool {
        self.bool_or("Accounts_AllowUserAvatarChange", true)
    }
    pub fn api_embed(&self) -> bool {
        self.bool_or("API_Embed", true)
    }
    pub fn language(&self) -> Option<&str> {
        self.str("Language").filter(|s| !s.is_empty())
    }
    pub fn sidenav_footer(&self) -> Option<&str> {
        self.str("Layout_Sidenav_Footer").filter(|s| !s.is_empty())
    }
    pub fn hide_system_messages(&self) -> Vec<String> {
        match self.map.get("Hide_System_Messages") {
            Some(Value::Array(a)) => a
                .iter()
                .filter_map(|v| v.as_str().map(str::to_owned))
                .collect(),
            _ => Vec::new(),
        }
    }
    /// `Accounts_AllowInvisibleStatusOption`: whether the Offline/Invisible
    /// status (and custom statuses of type offline) may be chosen.
    pub fn invisible_status_allowed(&self) -> bool {
        self.bool_or("Accounts_AllowInvisibleStatusOption", true)
    }
    /// Whether custom statuses (and the "Custom…" entry) are offered: Rocket.Chat
    /// gates them on `Accounts_AllowUserStatusMessageChange`.
    #[deprecated(note = "use allow_status_message_change / invisible_status_allowed")]
    pub fn custom_statuses_enabled(&self) -> bool {
        self.allow_status_message_change()
    }
    /// `Message_KeepHistory` && !`Message_ShowDeletedStatus`: deletions only hide
    /// messages, and `chat.syncMessages` reports them neither as updated nor as
    /// deleted, so open timelines must be reloaded after a disconnect.
    pub fn deletions_invisible_to_sync(&self) -> bool {
        self.bool_or("Message_KeepHistory", false)
            && !self.bool_or("Message_ShowDeletedStatus", false)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use serde_json::json;

    #[test]
    fn from_fixture() {
        let v: Value = crate::test_util::fixture("settings_public_page.json");
        let list: Vec<Setting> = serde_json::from_value(v["settings"].clone()).unwrap();
        let s = Settings::from_list(list);
        assert!(!s.is_empty());
    }

    #[test]
    fn typed_getters_and_defaults() {
        let mut s = Settings::new();
        assert_eq!(s.site_name(), "Rocket.Chat");
        assert!(s.allow_editing());
        assert_eq!(s.grouping_period_secs(), 300);
        s.set_value("Site_Name", json!("Corp"));
        s.set_value("Message_AllowEditing", json!(false));
        s.set_value("Message_AllowEditing_BlockEditInMinutes", json!(5));
        s.set_value("FileUpload_MaxFileSize", json!(-1));
        s.set_value("Message_GroupingPeriod", json!("60"));
        assert_eq!(s.site_name(), "Corp");
        assert!(!s.allow_editing());
        assert_eq!(s.edit_block_minutes(), 5);
        assert_eq!(s.file_upload_max_size(), None);
        assert_eq!(s.grouping_period_secs(), 60);
    }

    #[test]
    fn upload_types() {
        let mut s = Settings::new();
        assert!(s.is_upload_type_allowed("image/png"));
        s.set_value(
            "FileUpload_MediaTypeWhiteList",
            json!("image/*, application/pdf"),
        );
        s.set_value("FileUpload_MediaTypeBlackList", json!("image/svg+xml"));
        assert!(s.is_upload_type_allowed("image/png"));
        assert!(s.is_upload_type_allowed("application/pdf"));
        assert!(!s.is_upload_type_allowed("image/svg+xml"));
        assert!(!s.is_upload_type_allowed("text/plain"));
    }

    #[test]
    fn upload_defaults_and_star_whitelist() {
        let mut s = Settings::new();
        // RC defaults when missing: svg blacklisted, 100 MiB limit.
        assert!(!s.is_upload_type_allowed("image/svg+xml"));
        assert_eq!(s.file_upload_max_size(), Some(104_857_600));
        s.set_value("FileUpload_MediaTypeWhiteList", json!("*"));
        s.set_value("FileUpload_MediaTypeBlackList", json!("image/svg+xml"));
        assert!(s.is_upload_type_allowed("image/png"));
        assert!(s.is_upload_type_allowed("application/zip"));
        assert!(!s.is_upload_type_allowed("image/svg+xml"));
        s.set_value("FileUpload_MediaTypeBlackList", json!(""));
        assert!(s.is_upload_type_allowed("image/svg+xml"));
    }

    #[test]
    fn status_settings() {
        let mut s = Settings::new();
        assert!(s.invisible_status_allowed());
        s.set_value("Accounts_AllowInvisibleStatusOption", json!(false));
        assert!(!s.invisible_status_allowed());
        assert!(s.allow_status_message_change());
        assert!(!s.deletions_invisible_to_sync());
        s.set_value("Message_KeepHistory", json!(true));
        assert!(s.deletions_invisible_to_sync());
        s.set_value("Message_ShowDeletedStatus", json!(true));
        assert!(!s.deletions_invisible_to_sync());
    }
}
