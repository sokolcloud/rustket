//! Client-side parser for UIKit `mrkdwn` text objects (docs/uikit.md §2.1):
//! the server never sends an `md` tree for block text, so the text is turned
//! into the `@rocket.chat/message-parser` AST here and rendered by the shared
//! message renderer. A practical subset of the grammar: one paragraph per
//! line, fenced code, `>` quotes, `-`/`*`/`1.` lists, `*bold*`/`**bold**`,
//! `_italic_`/`__italic__`, `~strike~`/`~~strike~~`, `` `code` ``,
//! `[label](url)`, `<url|label>`, bare URLs, `:emoji:`, `@user` and
//! `#channel`.

use std::cell::RefCell;
use std::collections::HashMap;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::rc::Rc;

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

/// Parsed texts kept before the cache starts over.
const CACHE_CAPACITY: usize = 1024;

/// Text hash → (text, parsed blocks).
type Cache = HashMap<u64, (String, Rc<Vec<Block>>)>;

thread_local! {
    static CACHE: RefCell<Cache> = RefCell::new(HashMap::new());
}

/// [`parse`] through a per-thread cache: views re-render often and block
/// texts rarely change.
pub fn parse_cached(text: &str) -> Rc<Vec<Block>> {
    let mut h = DefaultHasher::new();
    text.hash(&mut h);
    let key = h.finish();
    CACHE.with(|c| {
        if let Some((t, b)) = c.borrow().get(&key)
            && t == text
        {
            return b.clone();
        }
        let parsed = Rc::new(parse(text));
        let mut c = c.borrow_mut();
        if c.len() >= CACHE_CAPACITY {
            c.clear();
        }
        c.insert(key, (text.to_owned(), parsed.clone()));
        parsed
    })
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

/// Memo of one `inlines` pass that keeps the scan linear: a failed search
/// for a closing delimiter (or for the next `pattern`) fails for every later
/// start as well, and a found position stays valid while it lies ahead.
#[derive(Default)]
struct Memo {
    /// Per delimiter: the smallest start whose closing search failed.
    failed: Vec<(&'static str, usize)>,
    /// Per pattern: (search start, first match at or after it).
    next: Vec<(&'static str, usize, Option<usize>)>,
    /// Position of a `](` whose link source was rejected.
    bad_src: Option<usize>,
}

impl Memo {
    /// Byte index of the first `pat` in `text` at or after `from`.
    fn find(&mut self, text: &str, pat: &'static str, from: usize) -> Option<usize> {
        if let Some((_, start, hit)) = self.next.iter().find(|(p, ..)| *p == pat)
            && from >= *start
            && hit.is_none_or(|h| h >= from)
        {
            return *hit;
        }
        let hit = text.get(from..)?.find(pat).map(|o| from + o);
        self.next.retain(|(p, ..)| *p != pat);
        self.next.push((pat, from, hit));
        hit
    }

    /// Byte index of the closing `delim` for an opening one ending at `from`:
    /// non-empty content on one line that does not start or end with a space;
    /// `_` must not be followed by a word character. Whether a candidate
    /// closes depends only on its position, so a failed search from `from`
    /// fails from any later start too.
    fn closing(&mut self, text: &str, from: usize, delim: &'static str) -> Option<usize> {
        let rest = &text[from..];
        if rest.starts_with(char::is_whitespace) || rest.starts_with(delim) {
            return None;
        }
        if self.failed.iter().any(|(d, at)| *d == delim && from >= *at) {
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
        self.failed.retain(|(d, _)| *d != delim);
        self.failed.push((delim, from));
        None
    }
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
    let mut memo = Memo::default();
    let mut plain_from = 0;
    let mut i = 0;
    while i < text.len() {
        let rest = &text[i..];
        let prev = text[..i].chars().next_back();
        let prev_word = prev.is_some_and(char::is_alphanumeric);
        let mut matched: Option<(usize, Inline)> = None;
        if let Some(after) = rest.strip_prefix('`') {
            if let Some(len) = memo
                .find(text, "`", i + 1)
                .map(|at| at - i - 1)
                .filter(|l| *l > 0)
            {
                matched = Some((
                    len + 2,
                    Inline::InlineCode {
                        value: plain(&after[..len]),
                    },
                ));
            }
        } else if rest.starts_with('[') {
            if let Some(mid) = memo.find(text, "](", i).map(|at| at - i)
                && memo.bad_src != Some(i + mid)
                && let Some(close) = memo.find(text, ")", i + mid + 2).map(|at| at - i - mid - 2)
            {
                let label = &rest[1..mid];
                let src = &rest[mid + 2..mid + 2 + close];
                // Every later `[` before this `](` gets the same source.
                if src.is_empty() || src.contains(char::is_whitespace) {
                    memo.bad_src = Some(i + mid);
                } else {
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
            // Only the last `<` before a `>` can open a link, so each
            // candidate's content is looked at once.
            if let Some(close) = memo.find(text, ">", i).map(|at| at - i)
                && memo
                    .find(text, "<", i + 1)
                    .is_none_or(|next| next > i + close)
            {
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
            if let Some(end) = memo.find(text, ":", i + 1).map(|at| at - i - 1)
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
                if let Some(end) = memo.closing(text, i + delim.len(), delim) {
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
    fn hostile_input_parses_quickly() {
        // Each of these was quadratic: every opener searched the whole rest.
        for s in [
            "*a ".repeat(6666),
            " _a".repeat(6666),
            "[".repeat(20_000),
            "<".repeat(20_000),
            "`a ".repeat(6666),
            ":a ".repeat(6666),
            "~~a ".repeat(5000),
            "[a](".repeat(5000),
            "<".repeat(19_990) + "a b>",
            "<".repeat(19_990) + "a|b>",
            "[".repeat(10_000) + "](" + &"a".repeat(9_990) + " )",
            "[".repeat(10_000) + "](" + &"a ".repeat(4_990) + ")",
        ] {
            let started = std::time::Instant::now();
            let b = parse(&s);
            assert_eq!(b.len(), 1);
            assert!(
                started.elapsed() < std::time::Duration::from_millis(500),
                "{:?} took {:?}",
                &s[..8],
                started.elapsed()
            );
        }
    }

    #[test]
    fn memo_keeps_results() {
        let p = parse("`a` and `b`, [x](https://x.example) [y](https://y.example) :a: :b:");
        let p = para(&p[0]);
        let codes = p
            .iter()
            .filter(|i| matches!(i, Inline::InlineCode { .. }))
            .count();
        let links = p
            .iter()
            .filter(|i| matches!(i, Inline::Link { .. }))
            .count();
        let emoji = p.iter().filter(|i| matches!(i, Inline::Emoji(_))).count();
        assert_eq!((codes, links, emoji), (2, 2, 2));
        let p = parse("*a *b* c*");
        assert!(matches!(&para(&p[0])[0], Inline::Bold { .. }));
    }

    #[test]
    fn deep_nesting_is_bounded() {
        let s = "*_~".repeat(50) + "x" + &"~_*".repeat(50);
        let b = parse(&s);
        assert_eq!(b.len(), 1);
    }
}
