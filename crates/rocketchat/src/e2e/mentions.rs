//! Plaintext `e2eMentions` sent next to encrypted messages so the server can
//! notify mentioned users (§7.6, `MentionsParser.ts`).

use std::sync::LazyLock;

use regex::{Regex, RegexBuilder};
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

/// Longest `@name`/`#name` entry sent in plaintext, in bytes (sigil excluded).
const MAX_MENTION_LEN: usize = 128;
/// Most entries per list sent in plaintext with one message.
const MAX_MENTIONS: usize = 50;
/// Longest accepted `UTF8_User_Names_Validation`.
const MAX_PATTERN_LEN: usize = 256;
/// Compiled-size limit of the server pattern.
const PATTERN_SIZE_LIMIT: usize = 4 << 20;

/// Markdown links, removed before the scan.
static LINKS: LazyLock<Option<Regex>> = LazyLock::new(|| Regex::new(r"\[[^\]]*\]\([^)]+\)").ok());

/// The server pattern as a start-anchored filter for one name. The pattern is
/// server-controlled, so it never takes part in finding mentions in the text:
/// it only decides how much of an already isolated token is a name. It must
/// compile on its own, so it cannot close the wrapping group.
fn name_regex(pattern: &str) -> Option<Regex> {
    if pattern.is_empty() || pattern.len() > MAX_PATTERN_LEN {
        return None;
    }
    let p = js_class_to_rust(pattern);
    let build = |src: &str| {
        RegexBuilder::new(src)
            .size_limit(PATTERN_SIZE_LIMIT)
            .build()
            .ok()
    };
    build(&p)?;
    build(&format!("^(?:{p})"))
}

/// Length of the non-empty name at the start of `s`.
fn name_len(name: &Regex, s: &str) -> Option<usize> {
    let m = name.find(s)?;
    (m.start() == 0 && m.end() > 0).then_some(m.end())
}

/// Length of `P(@P)?` (channels) or `P(@P)?(:[0-9a-zA-Z-_.]+)?` (users) at the
/// start of `token`.
fn mention_len(name: &Regex, token: &str, user: bool) -> Option<usize> {
    let mut end = name_len(name, token)?;
    let server = token[end..]
        .strip_prefix('@')
        .and_then(|rest| name_len(name, rest));
    if let Some(n) = server {
        end += 1 + n;
    }
    if user {
        let suffix = token[end..].strip_prefix(':').map(|rest| {
            rest.bytes()
                .take_while(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
                .count()
        });
        if let Some(n) = suffix.filter(|n| *n > 0) {
            end += 1 + n;
        }
    }
    Some(end)
}

/// Fixed candidate scan: `sigil` at the start of the text or after whitespace
/// or `>`, followed by the token up to the next whitespace. Every entry is
/// the sigil plus a prefix of such a token, so no whitespace and nothing
/// outside a mention-shaped token can be emitted, whatever `name` matches.
fn collect(text: &str, sigil: char, name: &Regex, user: bool) -> Vec<String> {
    let mut out = Vec::new();
    // End of the previous entry: matches do not overlap.
    let mut consumed = 0;
    let mut prev: Option<(usize, char)> = None;
    for (i, c) in text.char_indices() {
        let boundary = match prev {
            None => true,
            Some((pi, pc)) => pi >= consumed && (pc.is_whitespace() || pc == '>'),
        };
        prev = Some((i, c));
        if c != sigil || !boundary {
            continue;
        }
        let rest = &text[i + sigil.len_utf8()..];
        let token = &rest[..rest.find(char::is_whitespace).unwrap_or(rest.len())];
        // A window just past the cap is enough to tell an overlong entry.
        let window = match token.char_indices().find(|(at, _)| *at > MAX_MENTION_LEN) {
            Some((at, _)) => &token[..at],
            None => token,
        };
        let Some(len) = mention_len(name, window, user).filter(|n| *n <= MAX_MENTION_LEN) else {
            continue;
        };
        out.push(format!("{sigil}{}", &window[..len]));
        consumed = i + sigil.len_utf8() + len;
        if out.len() >= MAX_MENTIONS {
            break;
        }
    }
    out
}

/// Extracts mentions like the web client: markdown links are removed first;
/// a leading `>` (quote prefix) is not part of the entry.
/// `username_pattern` is `UTF8_User_Names_Validation`; a pattern that is
/// invalid on its own, too long or too large falls back to the default.
pub fn e2e_mentions(text: &str, username_pattern: &str) -> E2eMentions {
    let Some(name) = name_regex(username_pattern).or_else(|| name_regex(DEFAULT_USERNAME_PATTERN))
    else {
        return E2eMentions::default();
    };
    let stripped = match LINKS.as_ref() {
        Some(re) => re.replace_all(text, ""),
        None => std::borrow::Cow::Borrowed(text),
    };
    E2eMentions {
        e2e_user_mentions: collect(&stripped, '@', &name, true),
        e2e_channel_mentions: collect(&stripped, '#', &name, false),
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

    #[test]
    fn adjacent_and_partial_tokens() {
        let text = "@alice>@bob @carol! #a#b\n#c";
        let m = e2e_mentions(text, DEFAULT_USERNAME_PATTERN);
        assert_eq!(m.e2e_user_mentions, ["@alice", "@bob", "@carol"]);
        assert_eq!(m.e2e_channel_mentions, ["#a", "#c"]);
        assert!(e2e_mentions("@ # @! x@y", DEFAULT_USERNAME_PATTERN).is_empty());
    }

    /// Every entry is a sigil plus a short whitespace-free token.
    fn assert_bounded(m: &E2eMentions) {
        assert!(m.e2e_user_mentions.len() <= MAX_MENTIONS);
        assert!(m.e2e_channel_mentions.len() <= MAX_MENTIONS);
        for (list, sigil) in [(&m.e2e_user_mentions, '@'), (&m.e2e_channel_mentions, '#')] {
            for e in list {
                assert!(e.starts_with(sigil), "{e:?}");
                assert!(!e.contains(char::is_whitespace), "{e:?}");
                assert!(e.len() <= MAX_MENTION_LEN + 1, "{e:?}");
            }
        }
    }

    #[test]
    fn hostile_patterns_cannot_widen_the_match() {
        let text = "top secret plan\nmeet @alice at noon in #ops, bring the keys\nlast line";
        // Group break-outs: not valid on their own, so the default is used.
        for p in [
            r"x)|([\s\S]+)|(x",
            r"x)|([\s\S]+",
            r"x)|(?s:.+",
            r"[0-9a-z]+)|(.*",
        ] {
            assert!(name_regex(p).is_none(), "{p}");
            let m = e2e_mentions(text, p);
            assert_eq!(m.e2e_user_mentions, ["@alice"], "{p}");
            assert_eq!(m.e2e_channel_mentions, ["#ops"], "{p}");
        }
        // Valid but greedy patterns stay inside the mention token.
        for p in [
            r"[\s\S]+",
            r"(?s).*",
            r"(?s:.+)",
            r"x|[\s\S]+",
            r"(?x)[\s\S]+ #",
            "(?x)[\\s\\S]+ #\n",
            r".*$",
            r"(?m)^.*$",
        ] {
            let m = e2e_mentions(text, p);
            assert_bounded(&m);
            for e in &m.e2e_user_mentions {
                assert!("@alice".starts_with(e.as_str()), "{p}: {e:?}");
            }
            for e in &m.e2e_channel_mentions {
                assert!("#ops,".starts_with(e.as_str()), "{p}: {e:?}");
            }
        }
        // No sigil, nothing leaves in plaintext.
        assert!(e2e_mentions("top secret plan", r"[\s\S]+").is_empty());
        assert!(e2e_mentions("top secret plan", r"x)|([\s\S]+)|(x").is_empty());
    }

    #[test]
    fn caps() {
        let many = (0..MAX_MENTIONS * 4)
            .map(|i| format!("@u{i} #c{i}"))
            .collect::<Vec<_>>()
            .join(" ");
        let m = e2e_mentions(&many, DEFAULT_USERNAME_PATTERN);
        assert_eq!(m.e2e_user_mentions.len(), MAX_MENTIONS);
        assert_eq!(m.e2e_channel_mentions.len(), MAX_MENTIONS);
        assert_bounded(&m);

        // An overlong token is dropped, not truncated.
        let long = "a".repeat(MAX_MENTION_LEN + 1);
        let m = e2e_mentions(&format!("@{long} @ok #{long}"), DEFAULT_USERNAME_PATTERN);
        assert_eq!(m.e2e_user_mentions, ["@ok"]);
        assert!(m.e2e_channel_mentions.is_empty());
        let fits = "a".repeat(MAX_MENTION_LEN);
        let m = e2e_mentions(&format!("@{fits}"), DEFAULT_USERNAME_PATTERN);
        assert_eq!(m.e2e_user_mentions, [format!("@{fits}")]);
        let wide = "я".repeat(MAX_MENTION_LEN);
        assert!(e2e_mentions(&format!("@{wide}"), r"[\p{L}]+").is_empty());

        // Oversized patterns fall back to the default.
        let big = format!("[a-z]{}", "|[a-z]".repeat(MAX_PATTERN_LEN));
        assert!(name_regex(&big).is_none());
        assert!(name_regex(r"\pL{1000}{1000}").is_none());
        assert!(name_regex("").is_none());
        assert_eq!(e2e_mentions("@alice", &big).e2e_user_mentions, ["@alice"]);
    }
}
