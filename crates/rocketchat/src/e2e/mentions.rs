//! Plaintext `e2eMentions` sent next to encrypted messages so the server can
//! notify mentioned users (§7.6, `MentionsParser.ts`).

use regex::Regex;
use serde::{Deserialize, Serialize};

/// Default of `UTF8_User_Names_Validation`.
pub const DEFAULT_USERNAME_PATTERN: &str = "[0-9a-zA-Z-_.]+";

/// `IMessage.e2eMentions`.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct E2eMentions {
    /// `@user`, `@all`, `@here`, `@user@server`.
    pub e2e_user_mentions: Vec<String>,
    /// `#channel`.
    pub e2e_channel_mentions: Vec<String>,
}

impl E2eMentions {
    pub fn is_empty(&self) -> bool {
        self.e2e_user_mentions.is_empty() && self.e2e_channel_mentions.is_empty()
    }
}

/// JS character classes allow a literal `-` right after a range (`Z-_`);
/// the Rust regex parser does not, so escape such dashes.
fn js_class_to_rust(pattern: &str) -> String {
    let mut out = String::with_capacity(pattern.len() + 4);
    let chars: Vec<char> = pattern.chars().collect();
    let mut in_class = false;
    let mut after_range = false;
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c == '\\' {
            out.push(c);
            if let Some(n) = chars.get(i + 1) {
                out.push(*n);
            }
            i += 2;
            after_range = false;
            continue;
        }
        if in_class {
            if c == ']' {
                in_class = false;
            } else if c == '-' && after_range {
                out.push_str("\\-");
                after_range = false;
                i += 1;
                continue;
            } else if c == '-'
                && i > 0
                && chars[i - 1] != '['
                && chars.get(i + 1).is_some_and(|n| *n != ']')
            {
                // A range: the char after it ends the range.
                out.push(c);
                if let Some(n) = chars.get(i + 1) {
                    out.push(*n);
                }
                i += 2;
                after_range = true;
                continue;
            }
            after_range = false;
        } else if c == '[' {
            in_class = true;
            after_range = false;
        }
        out.push(c);
        i += 1;
    }
    out
}

fn regexes(pattern: &str) -> Option<(Regex, Regex)> {
    let p = js_class_to_rust(pattern);
    let user = Regex::new(&format!(
        r"(?m)(^|\s|>)@({p}(@({p}))?(:([0-9a-zA-Z\-_.]+))?)"
    ))
    .ok()?;
    let channel = Regex::new(&format!(r"(?m)(^|\s|>)#({p}(@({p}))?)")).ok()?;
    Some((user, channel))
}

/// Extracts mentions like the web client: markdown links are removed first,
/// matches are trimmed; a leading `>` (quote prefix) is dropped as well.
/// `username_pattern` is `UTF8_User_Names_Validation`; an invalid pattern falls
/// back to the default.
pub fn e2e_mentions(text: &str, username_pattern: &str) -> E2eMentions {
    let Some((user, channel)) =
        regexes(username_pattern).or_else(|| regexes(DEFAULT_USERNAME_PATTERN))
    else {
        return E2eMentions::default();
    };
    let links = Regex::new(r"\[[^\]]*\]\([^)]+\)").ok();
    let stripped = match &links {
        Some(re) => re.replace_all(text, "").into_owned(),
        None => text.to_owned(),
    };
    let collect = |re: &Regex| {
        re.find_iter(&stripped)
            .map(|m| {
                let t = m.as_str().trim();
                t.strip_prefix('>').unwrap_or(t).to_owned()
            })
            .collect()
    };
    E2eMentions {
        e2e_user_mentions: collect(&user),
        e2e_channel_mentions: collect(&channel),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn users_and_channels() {
        let m = e2e_mentions(
            "hi @alice and @all, see #general\n@bob@remote.org:x #team@srv [@carol](http://x/@carol)",
            DEFAULT_USERNAME_PATTERN,
        );
        assert_eq!(m.e2e_user_mentions, ["@alice", "@all", "@bob@remote.org:x"]);
        assert_eq!(m.e2e_channel_mentions, ["#general", "#team@srv"]);
        assert!(e2e_mentions("mail me at a@b.c", DEFAULT_USERNAME_PATTERN).is_empty());
        let q = e2e_mentions(">@alice", DEFAULT_USERNAME_PATTERN);
        assert_eq!(q.e2e_user_mentions, ["@alice"]);
    }

    #[test]
    fn patterns() {
        assert_eq!(js_class_to_rust("[0-9a-zA-Z-_.]+"), r"[0-9a-zA-Z\-_.]+");
        assert_eq!(js_class_to_rust("[-a-z]"), "[-a-z]");
        assert_eq!(js_class_to_rust("[a-z-]"), r"[a-z\-]");
        assert_eq!(js_class_to_rust(r"[\w.\-]+"), r"[\w.\-]+");
        // Unicode-friendly pattern.
        let m = e2e_mentions("привет @иван", r"[\p{L}0-9._-]+");
        assert_eq!(m.e2e_user_mentions, ["@иван"]);
        // Invalid pattern falls back to the default.
        let m = e2e_mentions("@alice", "[");
        assert_eq!(m.e2e_user_mentions, ["@alice"]);
    }
}
