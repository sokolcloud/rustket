//! Invite URLs (docs/admin.md §6): stored invites have `url: ""`, so the URL is
//! rebuilt like the server's `getInviteUrl` (`findOrCreateInvite.ts` + `getURL`).

/// URL settings needed to build invite links. `invite_url_type` is the private
/// setting `Accounts_Registration_InviteUrlType` (`direct` or `proxy`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct InviteUrlSettings {
    pub site_url: String,
    pub invite_url_type: Option<String>,
    /// `DeepLink_Url` (e.g. `https://go.rocket.chat`).
    pub deep_link_url: String,
    /// `CDN_PREFIX` (applies to direct links only).
    pub cdn_prefix: String,
}

impl InviteUrlSettings {
    /// From the private settings (`Site_Url`, `Accounts_Registration_InviteUrlType`,
    /// `DeepLink_Url`, `CDN_PREFIX`).
    pub fn from_store(store: &super::settings_editor::SettingsStore) -> Self {
        let s = |id: &str| store.value(id).and_then(serde_json::Value::as_str);
        Self {
            site_url: s("Site_Url").unwrap_or_default().to_owned(),
            invite_url_type: s("Accounts_Registration_InviteUrlType").map(str::to_owned),
            deep_link_url: s("DeepLink_Url").unwrap_or_default().to_owned(),
            cdn_prefix: s("CDN_PREFIX").unwrap_or_default().to_owned(),
        }
    }
}

/// The invite's own `url` when the server filled it, else the rebuilt link.
pub fn invite_link(invite: &rocketchat::models::admin::Invite, s: &InviteUrlSettings) -> String {
    if invite.url.is_empty() {
        invite_url(&invite.id, s)
    } else {
        invite.url.clone()
    }
}

/// The link for invite `id`: `{Site_Url}/invite/{id}` when the type is `direct`,
/// else the deep-link proxy form
/// `{DeepLink_Url}/invite?host=…&path=invite%2F{id}[&secure=no]`.
pub fn invite_url(id: &str, s: &InviteUrlSettings) -> String {
    let site = s.site_url.trim().trim_end_matches('/');
    let prefix = path_prefix(site);
    let path = format!("{prefix}/invite/{id}");
    if s.invite_url_type.as_deref() == Some("direct") {
        let cdn = s.cdn_prefix.trim().trim_end_matches('/');
        if !cdn.is_empty() {
            return format!("{cdn}{path}");
        }
        let base = site.strip_suffix(prefix).unwrap_or(site);
        return format!("{base}{path}");
    }
    let host = strip_scheme(site);
    let mut params = vec![
        ("host", host.to_owned()),
        ("path", path.trim_start_matches('/').to_owned()),
    ];
    if site.contains("http://") {
        params.push(("secure", "no".to_owned()));
    }
    let query: Vec<String> = params
        .into_iter()
        .map(|(k, v)| format!("{k}={}", encode_uri_component(&v)))
        .collect();
    format!(
        "{}/invite?{}",
        s.deep_link_url.trim_end_matches('/'),
        query.join("&")
    )
}

fn strip_scheme(url: &str) -> &str {
    let lower = url.to_ascii_lowercase();
    if lower.starts_with("https://") {
        &url[8..]
    } else if lower.starts_with("http://") {
        &url[7..]
    } else {
        url
    }
}

/// Path of the site URL (`/chat` for `https://x.org/chat`), like
/// `ROOT_URL_PATH_PREFIX`.
fn path_prefix(site: &str) -> &str {
    let rest = strip_scheme(site);
    match rest.find('/') {
        Some(i) => rest[i..].trim_end_matches('/'),
        None => "",
    }
}

/// JavaScript `encodeURIComponent`.
fn encode_uri_component(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || b"-_.!~*'()".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings(site: &str, kind: &str) -> InviteUrlSettings {
        InviteUrlSettings {
            site_url: site.into(),
            invite_url_type: Some(kind.into()),
            deep_link_url: "https://go.rocket.chat".into(),
            cdn_prefix: String::new(),
        }
    }

    #[test]
    fn proxy_matches_live_server() {
        // Verified on the dev server (docs/admin.md §6).
        assert_eq!(
            invite_url("5BAhAq", &settings("http://localhost:3000", "proxy")),
            "https://go.rocket.chat/invite?host=localhost%3A3000&path=invite%2F5BAhAq&secure=no"
        );
        assert_eq!(
            invite_url("abc", &settings("https://chat.example.org/", "proxy")),
            "https://go.rocket.chat/invite?host=chat.example.org&path=invite%2Fabc"
        );
    }

    #[test]
    fn from_settings_fixture() {
        let store = super::super::settings_editor::SettingsStore::new(
            rocketchat::models::admin::parse_private_settings(crate::test_util::fixture(
                "admin/private_settings.json",
            )),
        );
        let s = InviteUrlSettings::from_store(&store);
        assert_eq!(s.site_url, "http://localhost:3000");
        assert_eq!(s.invite_url_type.as_deref(), Some("proxy"));
        let mut invite = rocketchat::models::admin::Invite {
            id: "5BAhAq".into(),
            ..Default::default()
        };
        assert_eq!(
            invite_link(&invite, &s),
            "https://go.rocket.chat/invite?host=localhost%3A3000&path=invite%2F5BAhAq&secure=no"
        );
        invite.url = "https://x/invite/5BAhAq".into();
        assert_eq!(invite_link(&invite, &s), invite.url);
    }

    #[test]
    fn direct() {
        assert_eq!(
            invite_url("abc", &settings("https://chat.example.org/", "direct")),
            "https://chat.example.org/invite/abc"
        );
        assert_eq!(
            invite_url("abc", &settings("https://example.org/chat", "direct")),
            "https://example.org/chat/invite/abc"
        );
        let mut s = settings("https://example.org", "direct");
        s.cdn_prefix = "https://cdn.example.org/".into();
        assert_eq!(invite_url("abc", &s), "https://cdn.example.org/invite/abc");
    }
}
