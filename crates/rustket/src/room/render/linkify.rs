//! URL detection for plain-text fallbacks and link safety checks.

/// The URL to open for a link target, if it is safe: absolute `http`,
/// `https` or `mailto`. Protocol-relative (`//host`) and bare `www.` links
/// become `https`.
pub fn safe_url(src: &str) -> Option<String> {
    let src = src.trim();
    if src.is_empty() || src.chars().any(char::is_control) {
        return None;
    }
    let lower = src.to_ascii_lowercase();
    if lower.starts_with("http://") || lower.starts_with("https://") {
        return (src.len() > lower.find("://")? + 3).then(|| src.to_owned());
    }
    if lower.starts_with("mailto:") {
        return (src.len() > 7).then(|| src.to_owned());
    }
    if let Some(rest) = src.strip_prefix("//") {
        return (!rest.is_empty()).then(|| format!("https://{rest}"));
    }
    if lower.starts_with("www.") && src.len() > 4 {
        return Some(format!("https://{src}"));
    }
    None
}

/// Longest URL recognised, in bytes; a longer run is cut here. Bounds the
/// work per link whatever the input.
const MAX_URL_LEN: usize = 8 * 1024;

/// Most links recognised in one text; the rest stays plain text. Bounds the
/// link widgets/spans built downstream.
pub const MAX_LINKS: usize = 500;

/// Byte ranges of URLs in `text` (`http(s)://…`, `www.…`, `mailto:…`), with
/// trailing punctuation and unbalanced closing brackets left out. Linear in
/// the length of `text`: it runs on the UI thread over untrusted texts. At
/// most [`MAX_LINKS`] ranges.
pub fn find_links(text: &str) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    let lower = text.to_ascii_lowercase();
    let bytes = lower.as_bytes();
    let mut i = 0;
    while i < bytes.len() && out.len() < MAX_LINKS {
        let at_boundary = i == 0 || !is_word_byte(bytes[i - 1]);
        let prefix = matches!(bytes[i], b'h' | b'm' | b'w')
            .then(|| {
                ["https://", "http://", "mailto:", "www."]
                    .into_iter()
                    .find(|p| lower[i..].starts_with(p))
            })
            .flatten();
        match prefix {
            Some(p) if at_boundary => {
                let start = i;
                let limit = (start + MAX_URL_LEN).min(bytes.len());
                let mut end = i + p.len();
                while end < limit {
                    let c = text[end..].chars().next().unwrap_or(' ');
                    if c.is_whitespace() || c == '<' || c == '>' || c == '"' {
                        break;
                    }
                    if end + c.len_utf8() > limit {
                        break;
                    }
                    end += c.len_utf8();
                }
                let scanned = end;
                let end = trim_trailing(&text[start..end]) + start;
                if end > start + p.len() {
                    out.push((start, end));
                    i = end;
                    continue;
                }
                // Everything after the prefix was trimmed punctuation, which
                // cannot start another link: skip it in one step.
                i = scanned.max(i + p.len());
            }
            _ if bytes[i].is_ascii() => i += 1,
            _ => {
                i += text[i..].chars().next().map_or(1, char::len_utf8);
            }
        }
    }
    out
}

fn is_word_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b == b'/' || b == b'@' || b == b'.'
}

/// Length of `url` without trailing punctuation and unbalanced `)`/`]`.
/// Brackets are counted once and the counts kept up to date while trimming.
fn trim_trailing(url: &str) -> usize {
    let bytes = url.as_bytes();
    let count = |b: u8| bytes.iter().filter(|&&c| c == b).count();
    let (open_paren, mut close_paren) = (count(b'('), count(b')'));
    let (open_bracket, mut close_bracket) = (count(b'['), count(b']'));
    let mut end = url.len();
    while let Some(&c) = bytes[..end].last() {
        let strip = match c {
            b'.' | b',' | b':' | b';' | b'!' | b'?' | b'\'' | b'*' | b'_' => true,
            b')' if open_paren < close_paren => {
                close_paren -= 1;
                true
            }
            b']' if open_bracket < close_bracket => {
                close_bracket -= 1;
                true
            }
            _ => false,
        };
        if !strip {
            break;
        }
        end -= 1;
    }
    // Only ASCII bytes are stripped, so `end` stays on a char boundary.
    end
}

#[cfg(test)]
mod tests {
    use super::*;

    fn links(t: &str) -> Vec<&str> {
        find_links(t).into_iter().map(|(a, b)| &t[a..b]).collect()
    }

    #[test]
    fn finds_links() {
        assert_eq!(
            links("see https://example.com/a?b=1, and www.rust-lang.org."),
            ["https://example.com/a?b=1", "www.rust-lang.org"]
        );
        assert_eq!(
            links("(https://en.wikipedia.org/wiki/Rust_(language))"),
            ["https://en.wikipedia.org/wiki/Rust_(language)"]
        );
        assert_eq!(links("mail mailto:a@b.c now"), ["mailto:a@b.c"]);
        assert_eq!(
            links("Привет https://пример.рф/путь!"),
            ["https://пример.рф/путь"]
        );
        assert!(links("no links here, http:// alone").is_empty());
        assert!(links("abcwww.example.com").is_empty());
        assert_eq!(links("[www.a.org]"), ["www.a.org"]);
        assert_eq!(links("see http://a.b/c)."), ["http://a.b/c"]);
        assert!(links("www.)))) mailto:...").is_empty());
    }

    #[test]
    fn hostile_input_is_linear() {
        let inputs = [
            format!("https://a{}", ")".repeat(60_000)),
            format!("https://a{}", "]".repeat(60_000)),
            format!("www.a{}", ".".repeat(60_000)),
            "www.)".repeat(12_000),
            format!("http://{}", "(".repeat(30_000) + &")".repeat(30_000)),
            format!("https://{}", "a".repeat(60_000)),
            "https://a ".repeat(6_000),
        ];
        assert_eq!(find_links(&inputs[6]).len(), MAX_LINKS);
        let started = std::time::Instant::now();
        for _ in 0..20 {
            for text in &inputs {
                let found = find_links(text);
                assert!(found.iter().all(|(a, b)| b - a <= MAX_URL_LEN));
            }
        }
        // The old per-character recount took minutes here.
        assert!(
            started.elapsed() < std::time::Duration::from_secs(2),
            "{:?}",
            started.elapsed()
        );
        assert_eq!(links(&inputs[0]), ["https://a"]);
    }

    #[test]
    fn safe_urls() {
        assert_eq!(safe_url("https://a.b").as_deref(), Some("https://a.b"));
        assert_eq!(safe_url("HTTP://a.b").as_deref(), Some("HTTP://a.b"));
        assert_eq!(safe_url("mailto:x@y.z").as_deref(), Some("mailto:x@y.z"));
        assert_eq!(safe_url("//cdn.x/y").as_deref(), Some("https://cdn.x/y"));
        assert_eq!(safe_url("www.x.org").as_deref(), Some("https://www.x.org"));
        for bad in [
            "javascript:alert(1)",
            "file:///etc/passwd",
            "data:text/html,x",
            "https://",
            "",
            "ftp://x",
            "https://a\nb",
        ] {
            assert_eq!(safe_url(bad), None, "{bad}");
        }
    }
}
