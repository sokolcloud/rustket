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

/// Byte ranges of URLs in `text` (`http(s)://…`, `www.…`, `mailto:…`), with
/// trailing punctuation and unbalanced closing brackets left out.
pub fn find_links(text: &str) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    let lower = text.to_ascii_lowercase();
    let bytes = lower.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let at_boundary = i == 0 || !is_word_byte(bytes[i - 1]);
        let prefix = ["https://", "http://", "mailto:", "www."]
            .into_iter()
            .find(|p| lower[i..].starts_with(p));
        match prefix {
            Some(p) if at_boundary => {
                let start = i;
                let mut end = i + p.len();
                while end < bytes.len() {
                    let c = text[end..].chars().next().unwrap_or(' ');
                    if c.is_whitespace() || c == '<' || c == '>' || c == '"' {
                        break;
                    }
                    end += c.len_utf8();
                }
                let end = trim_trailing(&text[start..end]) + start;
                if end > start + p.len() {
                    out.push((start, end));
                    i = end;
                    continue;
                }
                i += p.len();
            }
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
fn trim_trailing(url: &str) -> usize {
    let mut end = url.len();
    loop {
        let Some(c) = url[..end].chars().next_back() else {
            return end;
        };
        let strip = match c {
            '.' | ',' | ':' | ';' | '!' | '?' | '\'' | '*' | '_' => true,
            ')' => url[..end].matches('(').count() < url[..end].matches(')').count(),
            ']' => url[..end].matches('[').count() < url[..end].matches(']').count(),
            _ => false,
        };
        if !strip {
            return end;
        }
        end -= c.len_utf8();
    }
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
