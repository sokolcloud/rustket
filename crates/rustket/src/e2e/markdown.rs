//! A small subset of `@rocket.chat/message-parser` for decrypted messages
//! and other texts the server sends without an `md` tree (pinned-message
//! attachments, room topic/announcement/description).
//!
//! The server never builds an `md` tree for `t: 'e2e'` messages (and one sent
//! by a client must not be trusted, §4), so the decrypted `msg` is parsed here
//! into the same AST the renderer already draws: one paragraph per line, code
//! fences, `>` quotes, `#`..`####` headings, `\[..\]` math lines, empty
//! lines as line breaks; inline `*bold*`/`**bold**`, `_italic_`/`__italic__`,
//! `~strike~`/`~~strike~~`, `||spoiler||`, `` `code` ``, `\(math\)`,
//! `<t:unix:f>` timestamps, `[label](url)`, bare `http(s)` links, `@user`,
//! `#channel` and `:emoji:`.

use rocketchat::models::md::{
    Block, CodeLine, Emoji, Inline, LinkValue, OneOrMany, Plain, Root, TimestampValue,
};

/// Bytes of inline formatting parsed per message; the lines after that are
/// plain text.
const MAX_PARSE_BYTES: usize = 64 * 1024;

/// Parses `text` into an `md` tree (empty for an empty text).
pub fn parse(text: &str) -> Root {
    let mut out = Vec::new();
    let mut budget = MAX_PARSE_BYTES;
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
        if let Some((level, rest)) = heading(line) {
            flush_quote(&mut out, &mut quote);
            out.push(Block::Heading {
                level,
                value: inlines(rest, &mut budget),
            });
            i += 1;
            continue;
        }
        if let Some(math) = line
            .trim()
            .strip_prefix("\\[")
            .and_then(|l| l.strip_suffix("\\]"))
            .filter(|m| !m.trim().is_empty())
        {
            flush_quote(&mut out, &mut quote);
            out.push(Block::Katex {
                value: math.trim().to_owned(),
            });
            i += 1;
            continue;
        }
        if let Some(rest) = line.strip_prefix('>') {
            let rest = rest.strip_prefix(' ').unwrap_or(rest);
            quote.push(Block::Paragraph {
                value: inlines(rest, &mut budget),
            });
            i += 1;
            continue;
        }
        flush_quote(&mut out, &mut quote);
        if line.trim().is_empty() {
            out.push(Block::LineBreak);
        } else if line.trim().len() >= 3 && line.trim().bytes().all(|b| b == b'-') {
            out.push(Block::HorizontalRule { fallback: None });
        } else {
            out.push(Block::Paragraph {
                value: inlines(line, &mut budget),
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

/// `# text` .. `#### text`: the level and the text.
fn heading(line: &str) -> Option<(u8, &str)> {
    let hashes = line.bytes().take_while(|b| *b == b'#').count();
    if !(1..=4).contains(&hashes) {
        return None;
    }
    let rest = &line[hashes..];
    let text = rest.trim_start_matches([' ', '\t']);
    (text.len() < rest.len() && !text.trim().is_empty()).then_some((hashes as u8, text))
}

/// `<t:1767225600>` / `<t:1767225600:f>` at the start of `s`: the node and
/// its length.
fn timestamp(s: &str) -> Option<(Inline, usize)> {
    let close = s.as_bytes().iter().take(24).position(|b| *b == b'>')?;
    let inner = s.get(3..close)?;
    let (secs, format) = match inner.split_once(':') {
        Some((secs, f)) => (secs, Some(f)),
        None => (inner, None),
    };
    let valid = !secs.is_empty()
        && secs.len() <= 10
        && secs.bytes().all(|b| b.is_ascii_digit())
        && format.is_none_or(|f| f.len() == 1 && "tTdDfFR".contains(f));
    valid.then(|| {
        (
            Inline::Timestamp {
                value: TimestampValue {
                    timestamp: secs.to_owned(),
                    format: format.map(str::to_owned),
                },
                fallback: None,
            },
            close + 1,
        )
    })
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
fn inlines(line: &str, budget: &mut usize) -> Vec<Inline> {
    if line.len() > *budget {
        *budget = 0;
        return vec![text_node(line)];
    }
    *budget -= line.len();
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

/// Byte positions in `s` where `delim` can close a span: no space right
/// before it, no word character right after it.
fn closers(s: &str, delim: char) -> Vec<usize> {
    let mut out = Vec::new();
    let mut prev: Option<char> = None;
    let mut it = s.char_indices().peekable();
    while let Some((i, c)) = it.next() {
        if c == delim
            && prev.is_some_and(|p| !p.is_whitespace())
            && !it.peek().is_some_and(|(_, n)| is_word(*n))
        {
            out.push(i);
        }
        prev = Some(c);
    }
    out
}

/// Byte index of a closing `delim` for an opening one ending at `from`
/// (non-empty, no space right inside the delimiters), from the precomputed
/// [`closers`].
fn closing(s: &str, from: usize, delim: char, closers: &[usize]) -> Option<usize> {
    let rest = &s[from..];
    if rest.starts_with(char::is_whitespace) || rest.starts_with(delim) {
        return None;
    }
    let k = closers.partition_point(|p| *p <= from);
    closers.get(k).copied()
}

/// The next occurrence of a pattern at or after a position, for positions
/// that only grow (amortised linear scanning).
struct Next {
    pat: &'static str,
    /// The last answer: the start of the search and what was found.
    last: Option<(usize, Option<usize>)>,
}

impl Next {
    fn new(pat: &'static str) -> Self {
        Self { pat, last: None }
    }

    fn at_or_after(&mut self, s: &str, from: usize) -> Option<usize> {
        if let Some((start, found)) = self.last
            && start <= from
            && found.is_none_or(|f| f >= from)
        {
            return found;
        }
        let found = s.get(from..)?.find(self.pat).map(|o| from + o);
        self.last = Some((from, found));
        found
    }
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
    let opens = s[..end].matches('(').count();
    let mut closes = s[..end].matches(')').count();
    while end > 0 && s[..end].ends_with(['.', ',', ';', ':', '!', '?', ')', ']', '\'']) {
        // Keep a closing parenthesis that belongs to the URL.
        if s[..end].ends_with(')') {
            if opens >= closes {
                break;
            }
            closes -= 1;
        }
        end -= 1;
    }
    let scheme = if lower.starts_with("https://") { 8 } else { 7 };
    (end > scheme).then_some(end)
}

/// Longest emoji short code.
const MAX_EMOJI: usize = 64;

const MAX_DEPTH: usize = 6;

fn parse_inline(s: &str, out: &mut Vec<Inline>, depth: usize) {
    let mut text_start = 0;
    let mut i = 0;
    let (stars, unders, tildes) = if depth < MAX_DEPTH {
        (closers(s, '*'), closers(s, '_'), closers(s, '~'))
    } else {
        Default::default()
    };
    let mut doubles = [Next::new("**"), Next::new("__"), Next::new("~~")];
    let mut pipes = Next::new("||");
    let mut math_end = Next::new("\\)");
    let mut link_sep = Next::new("](");
    let mut rbracket = Next::new("]");
    let mut rparen = Next::new(")");
    // The link target last checked (by the position of its `](`).
    let mut checked_link: Option<(usize, bool)> = None;
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
            '*' | '_' | '~' if at_boundary && depth < MAX_DEPTH && rest[1..].starts_with(c) => {
                let finder = match c {
                    '*' => &mut doubles[0],
                    '_' => &mut doubles[1],
                    _ => &mut doubles[2],
                };
                if let Some(end) = finder.at_or_after(s, i + 2)
                    && end > i + 2
                    && !s[i + 2..].starts_with(char::is_whitespace)
                    && !s[..end].ends_with(char::is_whitespace)
                {
                    let mut inner = Vec::new();
                    parse_inline(&s[i + 2..end], &mut inner, depth + 1);
                    let value = merge_text(inner);
                    let n = match c {
                        '*' => Inline::Bold { value },
                        '_' => Inline::Italic { value },
                        _ => Inline::Strike { value },
                    };
                    node = Some((n, end + 2 - i));
                }
            }
            '|' if depth < MAX_DEPTH && rest.starts_with("||") => {
                if let Some(end) = pipes.at_or_after(s, i + 2).filter(|e| *e > i + 2) {
                    let mut inner = Vec::new();
                    parse_inline(&s[i + 2..end], &mut inner, depth + 1);
                    node = Some((
                        Inline::Spoiler {
                            value: merge_text(inner),
                        },
                        end + 2 - i,
                    ));
                }
            }
            '<' if rest.starts_with("<t:") => node = timestamp(rest),
            '\\' if rest.starts_with("\\(") => {
                if let Some(end) = math_end.at_or_after(s, i + 2).filter(|e| *e > i + 2) {
                    node = Some((
                        Inline::InlineKatex {
                            value: s[i + 2..end].to_owned(),
                        },
                        end + 2 - i,
                    ));
                }
            }
            '*' | '_' | '~' if at_boundary && depth < MAX_DEPTH => {
                let list = match c {
                    '*' => &stars,
                    '_' => &unders,
                    _ => &tildes,
                };
                if let Some(end) = closing(s, i + 1, c, list) {
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
                if let Some(sep) = link_sep.at_or_after(s, i + 1)
                    && rbracket.at_or_after(s, i + 1) == Some(sep)
                    && let Some(paren) = rparen.at_or_after(s, sep + 2)
                {
                    let label = &s[i + 1..sep];
                    let url = &s[sep + 2..paren];
                    let valid = match checked_link {
                        Some((at, v)) if at == sep => v,
                        _ => {
                            let v = url_len(url) == Some(url.len()) || url.starts_with('/');
                            checked_link = Some((sep, v));
                            v
                        }
                    };
                    if valid {
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
                            paren + 1 - i,
                        ));
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
                let window = &rest.as_bytes()[1..rest.len().min(MAX_EMOJI + 2)];
                if let Some(end) = window.iter().position(|b| *b == b':') {
                    let code = &rest[1..1 + end];
                    if !code.is_empty()
                        && code.len() <= MAX_EMOJI
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

    fn inlines_all(line: &str) -> Vec<Inline> {
        let mut budget = usize::MAX;
        inlines(line, &mut budget)
    }

    #[test]
    fn hostile_lines_parse_fast() {
        let lines = [
            "*. ".repeat(20_000),
            "*a _b ~c ".repeat(6_000),
            format!("http://a/{}", ")".repeat(50_000)),
            "[".repeat(50_000) + "](http://a)",
            "[a](".repeat(15_000),
            ":a".repeat(30_000),
            "||a ".repeat(20_000),
            "**a ".repeat(20_000),
            "\\(a ".repeat(20_000),
            "<t:1 ".repeat(20_000),
        ];
        let start = std::time::Instant::now();
        for l in &lines {
            parse(l);
        }
        // Quadratic scanning took seconds here; linear takes milliseconds.
        assert!(
            start.elapsed() < std::time::Duration::from_secs(1),
            "{:?}",
            start.elapsed()
        );
        assert!(matches!(
            &inlines_all("a [x](http://a.b)")[..],
            [Inline::PlainText { .. }, Inline::Link { .. }]
        ));
        assert!(matches!(
            &inlines_all("*a* b *c*")[..],
            [Inline::Bold { .. }, _, Inline::Bold { .. }]
        ));
    }

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
        let v = inlines_all("Hi *bold* _it_ ~s~ `c` @alice #general :smile: https://x.org/a.");
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

    /// Pinned-message texts carry no `md`: spoilers must stay hidden and
    /// headings, timestamps and math must not show as raw syntax.
    #[test]
    fn spoilers_headings_timestamps_and_math() {
        let root = parse("# Release 2.4\n## Highlights\n#general\n\\[e^{i\\pi}+1=0\\]");
        assert!(matches!(&root[0], Block::Heading { level: 1, .. }));
        assert!(matches!(&root[1], Block::Heading { level: 2, .. }));
        assert!(matches!(&parse("a\n---")[1], Block::HorizontalRule { .. }));
        assert!(matches!(
            &parse("a\n-----")[1],
            Block::HorizontalRule { .. }
        ));
        assert!(matches!(&parse("a\n--")[1], Block::Paragraph { .. }));
        assert!(
            matches!(&root[2], Block::Paragraph { .. }),
            "a channel, not a heading"
        );
        assert!(matches!(&root[3], Block::Katex { value } if value == "e^{i\\pi}+1=0"));
        let v = inlines_all("a ||the cake is a lie|| b");
        match &v[1] {
            Inline::Spoiler { value } => assert_eq!(text_of(&value[0]), "the cake is a lie"),
            other => panic!("{other:?}"),
        }
        let v = inlines_all("at <t:1767225600:f> and <t:1767225600>");
        assert!(matches!(&v[1], Inline::Timestamp { value, .. }
            if value.timestamp == "1767225600" && value.format.as_deref() == Some("f")));
        assert!(matches!(&v[3], Inline::Timestamp { value, .. } if value.format.is_none()));
        let v = inlines_all("\\(e^{i\\pi}+1=0\\)");
        assert!(matches!(&v[0], Inline::InlineKatex { value } if value == "e^{i\\pi}+1=0"));
        let v = inlines_all("**bold** __it__ ~~gone~~");
        assert!(matches!(&v[0], Inline::Bold { value } if text_of(&value[0]) == "bold"));
        assert!(matches!(&v[2], Inline::Italic { .. }));
        assert!(matches!(&v[4], Inline::Strike { .. }));
        // Not markup.
        for s in ["a || b", "<t:abc>", "<t:12a>", "||||", "\\()", "** x**"] {
            let v = inlines_all(s);
            assert!(
                v.iter().all(|n| matches!(n, Inline::PlainText { .. })),
                "{s}: {v:?}"
            );
        }
    }

    #[test]
    fn quote_links_and_plain_cases() {
        let v = inlines_all("[ ](http://localhost:3000/group/x?msg=abc) reply");
        match &v[0] {
            Inline::Link { value } => {
                assert_eq!(value.src.value, "http://localhost:3000/group/x?msg=abc");
                assert_eq!(value.label.as_slice().len(), 1);
                assert_eq!(text_of(&value.label.as_slice()[0]), " ");
            }
            other => panic!("{other:?}"),
        }
        // Not formatting: snake_case, a lone star, e-mail addresses.
        let v = inlines_all("snake_case_name 2 * 3 me@example.org");
        assert_eq!(v.len(), 1, "{v:?}");
        // A label that is not a link stays text.
        let v = inlines_all("[x](javascript:alert(1))");
        assert!(v.iter().all(|n| matches!(n, Inline::PlainText { .. })));
    }
}
