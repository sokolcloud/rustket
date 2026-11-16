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

/// Characters cut from the end of a bare URL (sentence punctuation).
const TRAILING: &[char] = &[
    '.', ',', ';', ':', '!', '?', ')', ']', '}', '\'', '"', '*', '_', '~',
];

/// An `http(s)` URL with a host.
pub fn is_web_url(s: &str) -> bool {
    url::Url::parse(s)
        .ok()
        .is_some_and(|u| matches!(u.scheme(), "http" | "https") && u.host_str().is_some())
}

/// The links of `text` in order: markdown links, `<url|title>` links and
/// bare `http(s)` URLs.
fn links_of(text: &str, out: &mut Vec<Bookmark>) {
    let mut rest = text;
    while !rest.is_empty() {
        // The earliest of the three forms.
        let md = rest.find('[');
        let angle = rest.find("<http");
        let bare = rest
            .find("http://")
            .into_iter()
            .chain(rest.find("https://"))
            .min();
        let Some(at) = [md, angle, bare].into_iter().flatten().min() else {
            break;
        };
        let tail = &rest[at..];
        if Some(at) == md
            && let Some((b, used)) = markdown_link(tail)
        {
            out.push(b);
            rest = &tail[used..];
            continue;
        }
        if Some(at) == angle
            && let Some(end) = tail.find('>')
        {
            let inner = &tail[1..end];
            let (url, title) = match inner.split_once('|') {
                Some((u, t)) => (u.trim(), Some(t.trim())),
                None => (inner.trim(), None),
            };
            if is_web_url(url) {
                out.push(Bookmark {
                    title: title.filter(|t| !t.is_empty()).map(str::to_owned),
                    url: url.to_owned(),
                });
                rest = &tail[end + 1..];
                continue;
            }
        }
        if Some(at) == bare {
            let end = tail
                .find(|c: char| c.is_whitespace() || matches!(c, '<' | '>' | '|' | '[' | ']'))
                .unwrap_or(tail.len());
            let url = tail[..end].trim_end_matches(TRAILING);
            if is_web_url(url) {
                out.push(Bookmark {
                    title: None,
                    url: url.to_owned(),
                });
            }
            rest = &tail[end.max(1)..];
            continue;
        }
        // Not a link after all: skip the character that looked like one.
        let skip = tail.chars().next().map_or(1, char::len_utf8);
        rest = &tail[skip..];
    }
}

/// `[title](url)` at the start of `s`: the bookmark and the bytes used.
fn markdown_link(s: &str) -> Option<(Bookmark, usize)> {
    let close = s.find("](")?;
    let title = &s[1..close];
    if title.contains(['[', '\n']) {
        return None;
    }
    let after = &s[close + 2..];
    let end = after.find(')')?;
    let url = after[..end].trim();
    if !is_web_url(url) || url.contains(char::is_whitespace) {
        return None;
    }
    let title = title.trim();
    Some((
        Bookmark {
            title: (!title.is_empty()).then(|| title.to_owned()),
            url: url.to_owned(),
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

/// `description` with `[title](url)` appended on its own line (a title
/// without one is the URL itself; brackets in the title are dropped so the
/// link stays one).
pub fn append(description: &str, title: &str, url: &str) -> String {
    let title: String = title
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .filter(|c| !matches!(c, '[' | ']'))
        .collect();
    let link = if title.is_empty() {
        url.trim().to_owned()
    } else {
        format!("[{title}]({})", url.trim())
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
            ["https://a.example.com/x?y=1", "https://b.example.com"]
        );
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
            "[Run book](https://x.example.com)"
        );
        assert_eq!(
            append("About us\n", "", " https://x.example.com "),
            "About us\nhttps://x.example.com"
        );
        let d = append("About", "Board", "https://jira.example.com");
        assert_eq!(
            extract(None, None, Some(&d))[0].title.as_deref(),
            Some("Board")
        );
    }
}
