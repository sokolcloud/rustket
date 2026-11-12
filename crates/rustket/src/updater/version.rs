//! Release tags and version comparison.

use semver::Version;

/// `v1.2.3`, `V1.2.3` or `1.2.3` (surrounding whitespace ignored), parsed as
/// semver; anything else is `None`.
pub fn parse_tag(tag: &str) -> Option<Version> {
    let tag = tag.trim();
    let bare = tag
        .strip_prefix('v')
        .or_else(|| tag.strip_prefix('V'))
        .unwrap_or(tag);
    Version::parse(bare).ok()
}

/// `remote` is newer than `current` by semver precedence.
pub fn is_newer(remote: &Version, current: &Version) -> bool {
    remote.cmp_precedence(current).is_gt()
}

/// Whether to offer `remote`: newer, and a pre-release only when allowed.
pub fn offer(remote: &Version, current: &Version, prereleases: bool) -> bool {
    is_newer(remote, current) && (prereleases || remote.pre.is_empty())
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn v(s: &str) -> Version {
        Version::parse(s).unwrap()
    }

    #[test]
    fn tags() {
        assert_eq!(parse_tag("v0.2.3"), Some(v("0.2.3")));
        assert_eq!(parse_tag(" V1.0.0-rc1\n"), Some(v("1.0.0-rc1")));
        assert_eq!(parse_tag("0.2.2"), Some(v("0.2.2")));
        assert_eq!(parse_tag("vv1.0.0"), None);
        assert_eq!(parse_tag("nightly"), None);
        assert_eq!(parse_tag("v1.2"), None);
        assert_eq!(parse_tag(""), None);
    }

    #[test]
    fn ordering() {
        assert!(is_newer(&v("0.2.3"), &v("0.2.2")));
        assert!(!is_newer(&v("0.2.2"), &v("0.2.2")));
        assert!(!is_newer(&v("0.2.1"), &v("0.2.2")));
        assert!(is_newer(&v("0.3.0"), &v("0.3.0-rc1")));
        assert!(!is_newer(&v("0.3.0-rc1"), &v("0.3.0")));
        assert!(is_newer(&v("0.3.0-rc1"), &v("0.2.2")));
        // Build metadata does not count.
        assert!(!is_newer(&v("0.2.2+b1"), &v("0.2.2")));
    }

    #[test]
    fn offers() {
        let cur = v("0.2.2");
        assert!(offer(&v("0.2.3"), &cur, false));
        assert!(!offer(&v("0.3.0-rc1"), &cur, false));
        assert!(offer(&v("0.3.0-rc1"), &cur, true));
        assert!(!offer(&cur, &cur, true));
        assert!(!offer(&v("0.2.1"), &cur, true));
    }
}
