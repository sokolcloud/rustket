//! Client-side parser for UIKit `mrkdwn` text objects (docs/uikit.md §2.1):
//! the server never sends an `md` tree for block text, so the text is turned
//! into the `@rocket.chat/message-parser` AST here and rendered by the shared
//! message renderer. A practical subset of the grammar: one paragraph per
//! line, fenced code, `>` quotes, `-`/`*`/`1.` lists, `*bold*`/`**bold**`,
//! `_italic_`/`__italic__`, `~strike~`/`~~strike~~`, `` `code` ``,
//! `[label](url)`, `<url|label>`, bare URLs, `:emoji:`, `@user` and
//! `#channel`.

use rocketchat::models::md::{
    Block, CodeLine, Emoji, Inline, LinkValue, ListItem, OneOrMany, Plain,
};

use crate::room::render::find_links;

/// Deepest nesting of inline formatting.
const MAX_DEPTH: usize = 6;
/// Longest text parsed; the rest is dropped (views are trusted, messages are
/// size-guarded, this only bounds the work per text object).
const MAX_LEN: usize = 20_000;

fn plain(s: &str) -> Plain {
    Plain {
        value: s.to_owned(),
    }
}

/// Parses `text` into `md` blocks.
pub fn parse(text: &str) -> Vec<Block> {
    let text = match text.char_indices().nth(MAX_LEN) {
        Some((i, _)) => &text[..i],
        None => text,
    };
    let mut out = Vec::new();
    let mut lines = text.lines().peekable();
    while let Some(line) = lines.next() {
        let trimmed = line.trim_start();
        if let Some(rest) = trimmed.strip_prefix("```") {
            let language = Some(rest.trim().to_owned()).filter(|l| !l.is_empty());
            let mut code = Vec::new();
            for l in lines.by_ref() {
                if l.trim_start().starts_with("```") {
                    break;
                }
                code.push(CodeLine { value: plain(l) });
            }
            out.push(Block::Code {
                language,
                value: code,
            });
        } else if let Some(q) = quote_line(line) {
            let mut value = vec![paragraph(q)];
            while let Some(q) = lines.peek().and_then(|l| quote_line(l)) {
                value.push(paragraph(q));
                lines.next();
            }
            out.push(Block::Quote { value });
        } else if let Some(item) = bullet(line) {
            let mut value = vec![list_item(item, None)];
            while let Some(item) = lines.peek().and_then(|l| bullet(l)) {
                value.push(list_item(item, None));
                lines.next();
            }
            out.push(Block::UnorderedList { value });
        } else if let Some((n, item)) = numbered(line) {
            let mut value = vec![list_item(item, Some(n))];
            while let Some((n, item)) = lines.peek().and_then(|l| numbered(l)) {
                value.push(list_item(item, Some(n)));
                lines.next();
            }
            out.push(Block::OrderedList { value });
        } else if line.trim().is_empty() {
            out.push(Block::LineBreak);
        } else {
            out.push(paragraph(line));
        }
    }
    // A trailing break renders as an empty line.
    while matches!(out.last(), Some(Block::LineBreak)) {
        out.pop();
    }
    out
}

fn quote_line(line: &str) -> Option<&str> {
    let rest = line.strip_prefix('>')?;
    Some(rest.strip_prefix(' ').unwrap_or(rest))
}

fn bullet(line: &str) -> Option<&str> {
    line.strip_prefix("- ").or_else(|| line.strip_prefix("* "))
}

fn numbered(line: &str) -> Option<(u64, &str)> {
    let dot = line.find(". ")?;
    let n = line[..dot].parse().ok().filter(|_| dot <= 9)?;
    Some((n, &line[dot + 2..]))
}

fn list_item(text: &str, number: Option<u64>) -> ListItem {
    ListItem {
        value: inlines(text, 0),
        number,
    }
}

fn paragraph(text: &str) -> Block {
    Block::Paragraph {
        value: inlines(text, 0),
    }
}

/// Byte index of the closing `delim` for an opening one ending at `from`:
/// non-empty content on one line that does not start or end with a space;
/// `_` must not be followed by a word character.
fn closing(text: &str, from: usize, delim: &str) -> Option<usize> {
    let rest = &text[from..];
    if rest.starts_with(char::is_whitespace) || rest.starts_with(delim) {
        return None;
    }
    let mut search = 0;
    while let Some(off) = rest[search..].find(delim) {
        let at = search + off;
        let end = from + at;
        let before_ok = at > 0 && !rest[..at].ends_with(char::is_whitespace);
        let after = &text[end + delim.len()..];
        let after_ok = !delim.starts_with('_') || !after.starts_with(char::is_alphanumeric);
        let doubled = delim.len() == 1 && after.starts_with(delim);
        if before_ok && after_ok && !doubled {
            return Some(end);
        }
        search = at + delim.len();
    }
    None
}

fn is_name_char(c: char) -> bool {
    c.is_alphanumeric() || matches!(c, '_' | '-' | '.')
}

/// Plain text with bare URLs turned into links.
fn push_plain(out: &mut Vec<Inline>, text: &str) {
    let mut at = 0;
    for (s, e) in find_links(text) {
        if s > at {
            out.push(Inline::PlainText {
                value: text[at..s].to_owned(),
            });
        }
        let url = &text[s..e];
        out.push(link(
            url,
            vec![Inline::PlainText {
                value: url.to_owned(),
            }],
        ));
        at = e;
    }
    if at < text.len() {
        out.push(Inline::PlainText {
            value: text[at..].to_owned(),
        });
    }
}

fn link(src: &str, label: Vec<Inline>) -> Inline {
    let src = if src.starts_with("www.") {
        format!("https://{src}")
    } else {
        src.to_owned()
    };
    Inline::Link {
        value: LinkValue {
            src: plain(&src),
            label: OneOrMany::Many(label),
        },
    }
}

/// Inline nodes of one line.
fn inlines(text: &str, depth: usize) -> Vec<Inline> {
    let mut out = Vec::new();
    if depth > MAX_DEPTH {
        push_plain(&mut out, text);
        return out;
    }
    let mut plain_from = 0;
    let mut i = 0;
    while i < text.len() {
        let rest = &text[i..];
        let prev = text[..i].chars().next_back();
        let prev_word = prev.is_some_and(char::is_alphanumeric);
        let mut matched: Option<(usize, Inline)> = None;
        if let Some(after) = rest.strip_prefix('`') {
            if let Some(len) = after.find('`').filter(|l| *l > 0) {
                matched = Some((
                    len + 2,
                    Inline::InlineCode {
                        value: plain(&after[..len]),
                    },
                ));
            }
        } else if rest.starts_with('[') {
            if let Some(mid) = rest.find("](")
                && let Some(close) = rest[mid + 2..].find(')')
            {
                let label = &rest[1..mid];
                let src = &rest[mid + 2..mid + 2 + close];
                if !src.is_empty() && !src.contains(char::is_whitespace) {
                    let label = if label.trim().is_empty() {
                        vec![Inline::PlainText {
                            value: src.to_owned(),
                        }]
                    } else {
                        inlines(label, depth + 1)
                    };
                    matched = Some((mid + 3 + close, link(src, label)));
                }
            }
        } else if rest.starts_with('<') {
            if let Some(close) = rest.find('>') {
                let inner = &rest[1..close];
                let (src, label) = inner.split_once('|').unwrap_or((inner, inner));
                if !src.is_empty()
                    && !src.contains(char::is_whitespace)
                    && (src.contains("://") || src.starts_with("mailto:"))
                {
                    let label = vec![Inline::PlainText {
                        value: label.to_owned(),
                    }];
                    matched = Some((close + 1, link(src, label)));
                }
            }
        } else if rest.starts_with(':') && !prev_word {
            let body = &rest[1..];
            if let Some(end) = body.find(':')
                && end > 0
                && end <= 64
                && body[..end]
                    .chars()
                    .all(|c| c.is_alphanumeric() || matches!(c, '_' | '-' | '+'))
            {
                let code = &body[..end];
                matched = Some((
                    end + 2,
                    Inline::Emoji(Emoji {
                        value: Some(plain(code)),
                        short_code: Some(code.to_owned()),
                        unicode: None,
                    }),
                ));
            }
        } else if (rest.starts_with('@') || rest.starts_with('#'))
            && !prev.is_some_and(|c| c.is_alphanumeric() || c == '@' || c == '#')
        {
            let name: String = rest[1..].chars().take_while(|c| is_name_char(*c)).collect();
            let name = name.trim_end_matches('.');
            if !name.is_empty() {
                let node = if rest.starts_with('@') {
                    Inline::MentionUser { value: plain(name) }
                } else {
                    Inline::MentionChannel { value: plain(name) }
                };
                matched = Some((name.len() + 1, node));
            }
        } else if !prev_word {
            for delim in ["**", "__", "~~", "*", "_", "~"] {
                if !rest.starts_with(delim) {
                    continue;
                }
                if let Some(end) = closing(text, i + delim.len(), delim) {
                    let inner = inlines(&text[i + delim.len()..end], depth + 1);
                    let node = match delim.as_bytes()[0] {
                        b'*' => Inline::Bold { value: inner },
                        b'_' => Inline::Italic { value: inner },
                        _ => Inline::Strike { value: inner },
                    };
                    matched = Some((end + delim.len() - i, node));
                }
                break;
            }
        }
        match matched {
            Some((len, node)) => {
                if plain_from < i {
                    push_plain(&mut out, &text[plain_from..i]);
                }
                out.push(node);
                i += len;
                plain_from = i;
            }
            None => i += rest.chars().next().map_or(1, char::len_utf8),
        }
    }
    if plain_from < text.len() {
        push_plain(&mut out, &text[plain_from..]);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn texts(v: &[Inline]) -> Vec<String> {
        v.iter()
            .map(|i| {
                let mut s = String::new();
                i.plain_text(&mut s);
                s
            })
            .collect()
    }

    fn para(b: &Block) -> &[Inline] {
        match b {
            Block::Paragraph { value } => value,
            other => panic!("paragraph expected, got {other:?}"),
        }
    }

    #[test]
    fn inline_formatting() {
        let blocks = parse("*Bold* section with `code` and :smile: _it_ ~gone~");
        let p = para(&blocks[0]);
        assert!(matches!(&p[0], Inline::Bold { value } if texts(value) == ["Bold"]));
        assert!(matches!(&p[2], Inline::InlineCode { value } if value.value == "code"));
        assert!(
            p.iter()
                .any(|i| matches!(i, Inline::Emoji(e) if e.short_code.as_deref() == Some("smile")))
        );
        assert!(p.iter().any(|i| matches!(i, Inline::Italic { .. })));
        assert!(p.iter().any(|i| matches!(i, Inline::Strike { .. })));
    }

    #[test]
    fn no_formatting_inside_words_or_with_spaces() {
        let p = parse("snake_case_name and * not bold * 2*3*4");
        assert!(
            para(&p[0])
                .iter()
                .all(|i| matches!(i, Inline::PlainText { .. }))
        );
    }

    #[test]
    fn links_mentions_and_urls() {
        let p = parse(
            "see <https://a.example|A>, [B](https://b.example) or https://c.example @bob #dev",
        );
        let p = para(&p[0]);
        let links: Vec<String> = p
            .iter()
            .filter_map(|i| match i {
                Inline::Link { value } => Some(value.src.value.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(
            links,
            [
                "https://a.example",
                "https://b.example",
                "https://c.example"
            ]
        );
        assert!(
            p.iter()
                .any(|i| matches!(i, Inline::MentionUser { value } if value.value == "bob"))
        );
        assert!(
            p.iter()
                .any(|i| matches!(i, Inline::MentionChannel { value } if value.value == "dev"))
        );
        // An e-mail address is not a mention.
        let p = parse("mail a@b.example");
        assert!(
            !para(&p[0])
                .iter()
                .any(|i| matches!(i, Inline::MentionUser { .. }))
        );
    }

    #[test]
    fn blocks() {
        let b =
            parse("line one\n\n> quoted\n> more\n- a\n- b\n1. x\n```rust\nlet a = 1;\n```\nend\n");
        assert!(matches!(b[0], Block::Paragraph { .. }));
        assert!(matches!(b[1], Block::LineBreak));
        assert!(matches!(&b[2], Block::Quote { value } if value.len() == 2));
        assert!(matches!(&b[3], Block::UnorderedList { value } if value.len() == 2));
        assert!(matches!(&b[4], Block::OrderedList { value } if value[0].number == Some(1)));
        assert!(
            matches!(&b[5], Block::Code { language, value } if language.as_deref() == Some("rust") && value.len() == 1)
        );
        assert!(matches!(b[6], Block::Paragraph { .. }));
        assert_eq!(b.len(), 7);
    }

    #[test]
    fn deep_nesting_is_bounded() {
        let s = "*_~".repeat(50) + "x" + &"~_*".repeat(50);
        let b = parse(&s);
        assert_eq!(b.len(), 1);
    }
}
