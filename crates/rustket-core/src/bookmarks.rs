//! Channel bookmarks: the links of a room's topic, announcement and
//! description, shown as chips under the room header. They live in the
//! standard room fields, so every member sees the same set and other
//! clients show them as text; adding one appends `[title](url)` to the
//! description.

/// A link of the room.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Bookmark {
    /// The markdown link's text (`[title](url)`, `<url|title>`).
    pub title: Option<String>,
    pub url: String,
}

/// Characters cut from the end of a bare URL (sentence punctuation; a
/// `)` only when it closes nothing in the URL).
const TRAILING: &[char] = &[
    '.', ',', ';', ':', '!', '?', ']', '}', '\'', '"', '*', '_', '~',
];

/// Bytes of a field scanned for links: a bookmark bar has no use for more,
/// and the scan runs whenever the room changes.
const MAX_SCAN: usize = 16 * 1024;
/// Longest URL taken from a markdown link.
const MAX_URL: usize = 2048;

/// The URL as the client opens and shows it: an `http(s)` URL with a host,
/// normalized (`Url::as_str`: scheme and host lowercased, a non-ASCII host
/// in punycode, spaces and controls percent-encoded), so what a tooltip
/// shows is where a click goes.
pub fn web_url(s: &str) -> Option<String> {
    let u = url::Url::parse(s).ok()?;
    (matches!(u.scheme(), "http" | "https") && u.host_str().is_some())
        .then(|| u.as_str().to_owned())
}

/// An `http(s)` URL with a host.
pub fn is_web_url(s: &str) -> bool {
    web_url(s).is_some()
}

/// `s` starts with `prefix`, ASCII case-insensitively.
fn starts_with_ci(s: &str, prefix: &str) -> bool {
    s.as_bytes()
        .get(..prefix.len())
        .is_some_and(|b| b.eq_ignore_ascii_case(prefix.as_bytes()))
}

/// The links of `text` in order: markdown links, `<url|title>` links and
/// bare `http(s)` URLs. One forward pass: every candidate's own scan stops
/// at the next place another candidate could start (or a bounded length),
/// so the work stays linear in the text.
fn links_of(text: &str, out: &mut Vec<Bookmark>) {
    let mut cut = text.len().min(MAX_SCAN);
    while !text.is_char_boundary(cut) {
        cut -= 1;
    }
    let text = &text[..cut];
    let mut at = 0;
    while at < text.len() {
        let tail = &text[at..];
        let used = if tail.starts_with('[') {
            markdown_link(tail).map(|(b, used)| {
                out.push(b);
                used
            })
        } else if tail.starts_with('<') && starts_with_ci(&tail[1..], "http") {
            angle_link(tail).map(|(b, used)| {
                out.push(b);
                used
            })
        } else if starts_with_ci(tail, "http://") || starts_with_ci(tail, "https://") {
            let end = tail
                .find(|c: char| c.is_whitespace() || matches!(c, '<' | '>' | '|' | '[' | ']'))
                .unwrap_or(tail.len());
            if let Some(url) = web_url(trim_bare(&tail[..end])) {
                out.push(Bookmark { title: None, url });
            }
            Some(end.max(1))
        } else {
            None
        };
        at += used.unwrap_or_else(|| tail.chars().next().map_or(1, char::len_utf8));
    }
}

/// A bare URL without trailing punctuation; a trailing `)` goes only when
/// unbalanced (`…/Rust_(language)` keeps it, `(see https://a.b/x)` not).
fn trim_bare(url: &str) -> &str {
    let open = url.matches('(').count();
    let mut close = url.matches(')').count();
    let mut end = url.len();
    while let Some(c) = url[..end].chars().next_back() {
        if TRAILING.contains(&c) {
            end -= c.len_utf8();
        } else if c == ')' && close > open {
            close -= 1;
            end -= 1;
        } else {
            break;
        }
    }
    &url[..end]
}

/// `<url|title>` or `<url>` at the start of `s`: the bookmark and the bytes
/// used. The link ends before the next `<` or line break.
fn angle_link(s: &str) -> Option<(Bookmark, usize)> {
    let end = s[1..].find(['>', '<', '\n']).map(|e| e + 1)?;
    if !s[end..].starts_with('>') {
        return None;
    }
    let inner = &s[1..end];
    let (url, title) = match inner.split_once('|') {
        Some((u, t)) => (u.trim(), Some(t.trim())),
        None => (inner.trim(), None),
    };
    let url = web_url(url)?;
    Some((
        Bookmark {
            title: title.filter(|t| !t.is_empty()).map(str::to_owned),
            url,
        },
        end + 1,
    ))
}

/// `[title](url)` at the start of `s`: the bookmark and the bytes used.
/// The title ends at the first `]` (no `[` or line break before it); the
/// URL at the `)` that balances its parentheses, before any whitespace.
fn markdown_link(s: &str) -> Option<(Bookmark, usize)> {
    let close = s[1..].find([']', '[', '\n']).map(|e| e + 1)?;
    if !s[close..].starts_with("](") {
        return None;
    }
    let title = &s[1..close];
    let after = &s[close + 2..];
    let mut lim = after.len().min(MAX_URL);
    while !after.is_char_boundary(lim) {
        lim -= 1;
    }
    let after = &after[..lim];
    let mut depth = 0usize;
    let mut end = None;
    for (i, c) in after.char_indices() {
        match c {
            '(' => depth += 1,
            ')' if depth == 0 => {
                end = Some(i);
                break;
            }
            ')' => depth -= 1,
            c if c.is_whitespace() => return None,
            _ => {}
        }
    }
    let end = end?;
    let url = web_url(&after[..end])?;
    let title = title.trim();
    Some((
        Bookmark {
            title: (!title.is_empty()).then(|| title.to_owned()),
            url,
        },
        close + 2 + end + 1,
    ))
}

/// The same link written twice (`https://a.b` and `https://a.b/`).
fn same(a: &str, b: &str) -> bool {
    a.trim_end_matches('/') == b.trim_end_matches('/')
}

/// The room's links: topic, then announcement, then description; the
/// first occurrence of a URL keeps its place and takes a title from a
/// later one if it has none.
pub fn extract(
    topic: Option<&str>,
    announcement: Option<&str>,
    description: Option<&str>,
) -> Vec<Bookmark> {
    let mut all = Vec::new();
    for text in [topic, announcement, description].into_iter().flatten() {
        links_of(text, &mut all);
    }
    let mut out: Vec<Bookmark> = Vec::new();
    for b in all {
        match out.iter_mut().find(|x| same(&x.url, &b.url)) {
            Some(x) => {
                if x.title.is_none() {
                    x.title = b.title;
                }
            }
            None => out.push(b),
        }
    }
    out
}

/// What a chip shows: the title, else host and path, shortened to about
/// `max` characters.
pub fn label(b: &Bookmark, max: usize) -> String {
    let full = match &b.title {
        Some(t) => t.clone(),
        None => match url::Url::parse(&b.url) {
            Ok(u) => {
                let host = u.host_str().unwrap_or_default();
                let host = host.strip_prefix("www.").unwrap_or(host);
                let path = u.path().trim_end_matches('/');
                format!("{host}{path}")
            }
            Err(_) => b.url.clone(),
        },
    };
    shorten(&full, max)
}

fn shorten(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_owned();
    }
    let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
}

/// `description` with the link appended on its own line: `[title](url)`,
/// or the bare URL without a title. `url` is normalized first; a link
/// that would not read back as the same URL (a bare URL ending in `)` or
/// with `[`, a `|` …) is written `<url>` / `[title](url)` with `|` and
/// unbalanced parentheses percent-encoded. Brackets in the title are
/// dropped so the link stays one.
pub fn append(description: &str, title: &str, url: &str) -> String {
    let title: String = title
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .filter(|c| !matches!(c, '[' | ']'))
        .collect();
    let url = web_url(url.trim()).unwrap_or_else(|| url.trim().to_owned());
    let reads_back = |link: &str| {
        let mut found = Vec::new();
        links_of(link, &mut found);
        matches!(found.as_slice(), [b] if b.url == url)
    };
    let link = if title.is_empty() {
        url.clone()
    } else {
        format!("[{title}]({url})")
    };
    let link = if reads_back(&link) {
        link
    } else {
        // Characters that end a link percent-encoded (same target).
        let balanced = url.matches('(').count() == url.matches(')').count();
        let mut target = url.replace('|', "%7C");
        if !balanced {
            target = target.replace('(', "%28").replace(')', "%29");
        }
        if title.is_empty() {
            format!("<{target}>")
        } else {
            format!("[{title}]({target})")
        }
    };
    let base = description.trim_end();
    if base.is_empty() {
        link
    } else {
        format!("{base}\n{link}")
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    fn urls(v: &[Bookmark]) -> Vec<&str> {
        v.iter().map(|b| b.url.as_str()).collect()
    }

    #[test]
    fn extracts_in_field_order_and_dedupes() {
        let topic =
            "Docs: https://docs.example.com/guide. Board [Jira](https://jira.example.com/b/1)";
        let ann = "Deploy freeze, see <https://wiki.example.com/freeze|the plan> (and https://jira.example.com/b/1/)";
        let desc = "[Docs](https://docs.example.com/guide)\nftp://nope.example.com [x](javascript:alert(1)) https://";
        let b = extract(Some(topic), Some(ann), Some(desc));
        assert_eq!(
            urls(&b),
            [
                "https://docs.example.com/guide",
                "https://jira.example.com/b/1",
                "https://wiki.example.com/freeze"
            ]
        );
        assert_eq!(
            b[0].title.as_deref(),
            Some("Docs"),
            "title from a later copy"
        );
        assert_eq!(b[1].title.as_deref(), Some("Jira"));
        assert_eq!(b[2].title.as_deref(), Some("the plan"));
        assert!(extract(None, Some("no links here [a](b)"), None).is_empty());
    }

    #[test]
    fn bare_urls_drop_trailing_punctuation() {
        let b = extract(
            Some("(see https://a.example.com/x?y=1), then https://b.example.com!"),
            None,
            None,
        );
        assert_eq!(
            urls(&b),
            ["https://a.example.com/x?y=1", "https://b.example.com/"]
        );
    }

    #[test]
    fn parentheses_case_and_odd_urls_round_trip() {
        let kibana = "https://kibana.example.com/app/discover#/?_g=(time:(from:now-15m))";
        let wiki = "https://en.wikipedia.org/wiki/Rust_(programming_language)";
        for (title, url) in [
            ("Logs", kibana),
            ("", wiki),
            ("Wiki", wiki),
            ("", "HTTP://intranet.example.com/wiki"),
            ("", "https://example.com/a b"),
            ("", "http://[::1]:8080/"),
            ("", "https://example.com/a|b"),
            ("Half", "https://example.com/x)"),
            ("", "https://example.com/(x"),
        ] {
            let decode = |u: &str| {
                u.replace("%7C", "|")
                    .replace("%28", "(")
                    .replace("%29", ")")
            };
            let want = web_url(url).unwrap();
            let d = append("About", title, url);
            let b = extract(None, None, Some(&d));
            assert_eq!(b.len(), 1, "{title:?} {url:?} -> {d:?}");
            assert_eq!(decode(&b[0].url), want, "{title:?} {url:?} -> {d:?}");
            if !title.is_empty() {
                assert_eq!(b[0].title.as_deref(), Some(title));
            }
        }
        // Written by others: balanced parentheses stay, scheme case ignored.
        let b = extract(
            Some(&format!(
                "see {wiki}, and [L]({kibana}) HTTPS://A.example.com"
            )),
            None,
            None,
        );
        assert_eq!(urls(&b), [wiki, kibana, "https://a.example.com/"]);
    }

    #[test]
    fn spoofed_urls_are_shown_normalized() {
        let b = extract(
            Some("[Docs](https://\u{202E}moc.elgoog@evil.example)"),
            None,
            None,
        );
        assert_eq!(b.len(), 1);
        assert!(!b[0].url.contains('\u{202E}'));
        assert_eq!(
            url::Url::parse(&b[0].url).unwrap().host_str(),
            Some("evil.example")
        );
    }

    #[test]
    fn hostile_text_is_scanned_in_linear_time() {
        let started = std::time::Instant::now();
        for text in [
            "[".repeat(200_000),
            "<http".repeat(40_000),
            "[a](http".repeat(25_000),
            format!("https://a.example.com/{}", ")".repeat(200_000)),
        ] {
            extract(Some(&text), Some(&text), Some(&text));
        }
        // Quadratic scanning took seconds for one of these.
        assert!(started.elapsed() < std::time::Duration::from_secs(2));
        // Links past the scanned part are not looked for.
        let late = format!("{} https://late.example.com", "x".repeat(MAX_SCAN));
        assert!(extract(None, None, Some(&late)).is_empty());
    }

    #[test]
    fn long_non_ascii_text_after_a_link_does_not_split_a_char() {
        // Byte MAX_URL after "](" falls inside a two-byte Cyrillic letter.
        for pad in ["п", "xп", "ภ", "xภ", "xxภ"] {
            let text = format!(
                "[Регламент](https://wiki.example.com/reglament)\n{}",
                pad.repeat(MAX_URL)
            );
            let got = extract(None, None, Some(&text));
            assert_eq!(got.len(), 1);
            assert_eq!(got[0].url, "https://wiki.example.com/reglament");
            // An unclosed link followed by the same text is no link.
            let open = format!("[a]({}", pad.repeat(MAX_URL));
            assert!(extract(None, None, Some(&open)).is_empty());
        }
    }

    #[test]
    fn labels_and_append() {
        let b = Bookmark {
            title: None,
            url: "https://www.example.com/very/long/path/to/some/page/".into(),
        };
        assert_eq!(label(&b, 100), "example.com/very/long/path/to/some/page");
        assert_eq!(label(&b, 12), "example.com…");
        let t = Bookmark {
            title: Some("Runbook".into()),
            url: "https://x.example.com".into(),
        };
        assert_eq!(label(&t, 20), "Runbook");
        assert_eq!(
            append("", "  Run [book] ", "https://x.example.com"),
            "[Run book](https://x.example.com/)"
        );
        assert_eq!(
            append("About us\n", "", " https://x.example.com "),
            "About us\nhttps://x.example.com/"
        );
        let d = append("About", "Board", "https://jira.example.com");
        assert_eq!(
            extract(None, None, Some(&d))[0].title.as_deref(),
            Some("Board")
        );
    }
}
