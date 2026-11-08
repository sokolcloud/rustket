//! A small subset of `@rocket.chat/message-parser` for decrypted messages.
//!
//! The server never builds an `md` tree for `t: 'e2e'` messages (and one sent
//! by a client must not be trusted, §4), so the decrypted `msg` is parsed here
//! into the same AST the renderer already draws: one paragraph per line, code
//! fences, `>` quotes, empty lines as line breaks; inline `*bold*`,
//! `_italic_`, `~strike~`, `` `code` ``, `[label](url)`, bare `http(s)` links,
//! `@user`, `#channel` and `:emoji:`.

use rocketchat::models::md::{Block, CodeLine, Emoji, Inline, LinkValue, OneOrMany, Plain, Root};

/// Longest input parsed (longer texts become one plain paragraph per line).
const MAX_PARSE_BYTES: usize = 64 * 1024;

/// Parses `text` into an `md` tree (empty for an empty text).
pub fn parse(text: &str) -> Root {
    let mut out = Vec::new();
    let lines: Vec<&str> = text.split('\n').collect();
    let mut i = 0;
    let mut quote: Vec<Block> = Vec::new();
    let flush_quote = |out: &mut Vec<Block>, quote: &mut Vec<Block>| {
        if !quote.is_empty() {
            out.push(Block::Quote {
                value: std::mem::take(quote),
            });
        }
    };
    while i < lines.len() {
        let line = lines[i].trim_end_matches('\r');
        if let Some(lang) = line.trim_start().strip_prefix("```") {
            // A fence: everything up to the closing fence (or the end).
            flush_quote(&mut out, &mut quote);
            let mut code = Vec::new();
            let mut j = i + 1;
            let mut closed = false;
            while j < lines.len() {
                let l = lines[j].trim_end_matches('\r');
                if l.trim_start().starts_with("```") {
                    closed = true;
                    break;
                }
                code.push(CodeLine { value: plain(l) });
                j += 1;
            }
            if closed || !code.is_empty() {
                let lang = lang.trim();
                out.push(Block::Code {
                    language: (!lang.is_empty()).then(|| lang.to_owned()),
                    value: code,
                });
                i = j + 1;
                continue;
            }
        }
        if let Some(rest) = line.strip_prefix('>') {
            let rest = rest.strip_prefix(' ').unwrap_or(rest);
            quote.push(Block::Paragraph {
                value: inlines(rest),
            });
            i += 1;
            continue;
        }
        flush_quote(&mut out, &mut quote);
        if line.trim().is_empty() {
            out.push(Block::LineBreak);
        } else {
            out.push(Block::Paragraph {
                value: inlines(line),
            });
        }
        i += 1;
    }
    flush_quote(&mut out, &mut quote);
    // Trailing empty lines carry nothing.
    while matches!(out.last(), Some(Block::LineBreak)) {
        out.pop();
    }
    out
}

fn plain(s: &str) -> Plain {
    Plain {
        value: s.to_owned(),
    }
}

fn text_node(s: &str) -> Inline {
    Inline::PlainText {
        value: s.to_owned(),
    }
}

/// Inline nodes of one line.
fn inlines(line: &str) -> Vec<Inline> {
    if line.len() > MAX_PARSE_BYTES {
        return vec![text_node(line)];
    }
    let mut out = Vec::new();
    parse_inline(line, &mut out, 0);
    merge_text(out)
}

/// Joins adjacent plain-text nodes.
fn merge_text(nodes: Vec<Inline>) -> Vec<Inline> {
    let mut out: Vec<Inline> = Vec::with_capacity(nodes.len());
    for n in nodes {
        if let (Inline::PlainText { value: new }, Some(Inline::PlainText { value })) =
            (&n, out.last_mut())
        {
            value.push_str(new);
            continue;
        }
        out.push(n);
    }
    out
}

fn is_word(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

fn mention_char(c: char) -> bool {
    c.is_alphanumeric() || matches!(c, '-' | '_' | '.')
}

/// Byte index of a closing `delim` for an opening one ending at `from`:
/// non-empty, no space right inside the delimiters, no word character right
/// after the closing one.
fn closing(s: &str, from: usize, delim: char) -> Option<usize> {
    let rest = &s[from..];
    if rest.starts_with(char::is_whitespace) || rest.starts_with(delim) {
        return None;
    }
    let mut search = 0;
    while let Some(off) = rest[search..].find(delim) {
        let at = search + off;
        let end = from + at;
        let before_ok = at > 0 && !rest[..at].ends_with(char::is_whitespace);
        let after_ok = !s[end + delim.len_utf8()..].starts_with(is_word);
        if before_ok && after_ok {
            return Some(end);
        }
        search = at + delim.len_utf8();
    }
    None
}

/// Length of a URL starting at `s` (`http://` / `https://`), trailing
/// punctuation excluded.
fn url_len(s: &str) -> Option<usize> {
    let lower = s.get(..8).unwrap_or(s).to_ascii_lowercase();
    if !lower.starts_with("http://") && !lower.starts_with("https://") {
        return None;
    }
    let mut end = s
        .find(|c: char| c.is_whitespace() || matches!(c, '<' | '>' | '"'))
        .unwrap_or(s.len());
    while end > 0 && s[..end].ends_with(['.', ',', ';', ':', '!', '?', ')', ']', '\'']) {
        // Keep a closing parenthesis that belongs to the URL.
        if s[..end].ends_with(')') && s[..end].matches('(').count() >= s[..end].matches(')').count()
        {
            break;
        }
        end -= 1;
    }
    let scheme = if lower.starts_with("https://") { 8 } else { 7 };
    (end > scheme).then_some(end)
}

const MAX_DEPTH: usize = 6;

fn parse_inline(s: &str, out: &mut Vec<Inline>, depth: usize) {
    let mut text_start = 0;
    let mut i = 0;
    let push_text = |out: &mut Vec<Inline>, from: usize, to: usize| {
        if to > from {
            out.push(text_node(&s[from..to]));
        }
    };
    while i < s.len() {
        let rest = &s[i..];
        let Some(c) = rest.chars().next() else {
            break;
        };
        let prev = s[..i].chars().next_back();
        let at_boundary = prev.is_none_or(|p| !is_word(p));
        let mut node: Option<(Inline, usize)> = None;
        match c {
            '`' => {
                if let Some(end) = rest[1..].find('`').filter(|e| *e > 0) {
                    node = Some((
                        Inline::InlineCode {
                            value: plain(&rest[1..1 + end]),
                        },
                        end + 2,
                    ));
                }
            }
            '*' | '_' | '~' if at_boundary && depth < MAX_DEPTH => {
                if let Some(end) = closing(s, i + 1, c) {
                    let mut inner = Vec::new();
                    parse_inline(&s[i + 1..end], &mut inner, depth + 1);
                    let value = merge_text(inner);
                    let n = match c {
                        '*' => Inline::Bold { value },
                        '_' => Inline::Italic { value },
                        _ => Inline::Strike { value },
                    };
                    node = Some((n, end + 1 - i));
                }
            }
            '[' => {
                if let Some(close) = rest.find("](") {
                    let label = &rest[1..close];
                    let after = &rest[close + 2..];
                    if !label.contains(']')
                        && let Some(paren) = after.find(')')
                    {
                        let url = &after[..paren];
                        if url_len(url) == Some(url.len()) || url.starts_with('/') {
                            let mut inner = Vec::new();
                            if depth < MAX_DEPTH {
                                parse_inline(label, &mut inner, depth + 1);
                            } else {
                                inner.push(text_node(label));
                            }
                            node = Some((
                                Inline::Link {
                                    value: LinkValue {
                                        src: plain(url),
                                        label: OneOrMany::Many(merge_text(inner)),
                                    },
                                },
                                close + 2 + paren + 1,
                            ));
                        }
                    }
                }
            }
            'h' | 'H' if at_boundary => {
                if let Some(len) = url_len(rest) {
                    let url = &rest[..len];
                    node = Some((
                        Inline::Link {
                            value: LinkValue {
                                src: plain(url),
                                label: OneOrMany::Many(vec![text_node(url)]),
                            },
                        },
                        len,
                    ));
                }
            }
            '@' | '#' if prev.is_none_or(|p| p.is_whitespace() || p == '(') => {
                let len = rest[1..]
                    .find(|c: char| !mention_char(c))
                    .unwrap_or(rest.len() - 1);
                // A trailing dot ends the sentence, not the name.
                let name = rest[1..1 + len].trim_end_matches('.');
                if !name.is_empty() {
                    let value = plain(name);
                    let n = if c == '@' {
                        Inline::MentionUser { value }
                    } else {
                        Inline::MentionChannel { value }
                    };
                    node = Some((n, 1 + name.len()));
                }
            }
            ':' => {
                if let Some(end) = rest[1..].find(':') {
                    let code = &rest[1..1 + end];
                    if !code.is_empty()
                        && code.len() <= 64
                        && code
                            .chars()
                            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '+' | '-'))
                    {
                        node = Some((
                            Inline::Emoji(Emoji {
                                value: Some(plain(code)),
                                short_code: Some(code.to_owned()),
                                unicode: None,
                            }),
                            end + 2,
                        ));
                    }
                }
            }
            _ => {}
        }
        match node {
            Some((n, len)) => {
                push_text(out, text_start, i);
                out.push(n);
                i += len;
                text_start = i;
            }
            None => i += c.len_utf8(),
        }
    }
    push_text(out, text_start, s.len());
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn text_of(n: &Inline) -> String {
        let mut s = String::new();
        n.plain_text(&mut s);
        s
    }

    #[test]
    fn lines_quotes_and_code() {
        let root = parse("hello\n\n> quoted\n> more\n```rust\nlet x = 1;\n```\nend\n\n");
        assert!(matches!(&root[0], Block::Paragraph { .. }));
        assert!(matches!(&root[1], Block::LineBreak));
        match &root[2] {
            Block::Quote { value } => assert_eq!(value.len(), 2),
            other => panic!("{other:?}"),
        }
        match &root[3] {
            Block::Code { language, value } => {
                assert_eq!(language.as_deref(), Some("rust"));
                assert_eq!(value[0].value.value, "let x = 1;");
            }
            other => panic!("{other:?}"),
        }
        assert!(matches!(&root[4], Block::Paragraph { .. }));
        assert_eq!(root.len(), 5);
        assert!(parse("").is_empty());
    }

    #[test]
    fn inline_nodes() {
        let v = inlines("Hi *bold* _it_ ~s~ `c` @alice #general :smile: https://x.org/a.");
        let kinds: Vec<&str> = v
            .iter()
            .map(|n| match n {
                Inline::PlainText { .. } => "t",
                Inline::Bold { .. } => "b",
                Inline::Italic { .. } => "i",
                Inline::Strike { .. } => "s",
                Inline::InlineCode { .. } => "c",
                Inline::MentionUser { .. } => "u",
                Inline::MentionChannel { .. } => "h",
                Inline::Emoji(_) => "e",
                Inline::Link { .. } => "l",
                _ => "?",
            })
            .collect();
        assert_eq!(
            kinds,
            [
                "t", "b", "t", "i", "t", "s", "t", "c", "t", "u", "t", "h", "t", "e", "t", "l", "t"
            ]
        );
        match &v[15] {
            Inline::Link { value } => assert_eq!(value.src.value, "https://x.org/a"),
            other => panic!("{other:?}"),
        }
        assert_eq!(text_of(&v[1]), "bold");
    }

    #[test]
    fn quote_links_and_plain_cases() {
        let v = inlines("[ ](http://localhost:3000/group/x?msg=abc) reply");
        match &v[0] {
            Inline::Link { value } => {
                assert_eq!(value.src.value, "http://localhost:3000/group/x?msg=abc");
                assert_eq!(value.label.as_slice().len(), 1);
                assert_eq!(text_of(&value.label.as_slice()[0]), " ");
            }
            other => panic!("{other:?}"),
        }
        // Not formatting: snake_case, a lone star, e-mail addresses.
        let v = inlines("snake_case_name 2 * 3 me@example.org");
        assert_eq!(v.len(), 1, "{v:?}");
        // A label that is not a link stays text.
        let v = inlines("[x](javascript:alert(1))");
        assert!(v.iter().all(|n| matches!(n, Inline::PlainText { .. })));
    }
}
