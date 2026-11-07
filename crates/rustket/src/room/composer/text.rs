//! Pure text operations of the composer: formatting around a selection,
//! autocomplete triggers and completion, slash command parsing, quotes and
//! permalinks. Offsets are byte offsets into a `\n`-separated string.

use std::ops::Range;

use rocketchat::models::RoomType;

/// Formatting toolbar actions (Rocket.Chat markdown, as the web client's
/// formatting buttons insert it).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    Bold,
    Italic,
    Strike,
    Code,
    CodeBlock,
    Link,
    Quote,
}

impl Format {
    pub const ALL: [Self; 7] = [
        Self::Bold,
        Self::Italic,
        Self::Strike,
        Self::Code,
        Self::CodeBlock,
        Self::Link,
        Self::Quote,
    ];

    /// Inline markers `(before, after)`; `None` for line-based formats.
    fn markers(self) -> Option<(&'static str, &'static str)> {
        Some(match self {
            Self::Bold => ("*", "*"),
            Self::Italic => ("_", "_"),
            Self::Strike => ("~", "~"),
            Self::Code => ("`", "`"),
            Self::CodeBlock | Self::Link | Self::Quote => return None,
        })
    }
}

/// Result of an edit: the new text and the new selection (`start == end` for a
/// plain cursor).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Edited {
    pub text: String,
    pub selection: Range<usize>,
}

/// Applies `fmt` around `sel` (a cursor when empty). Inline formats toggle:
/// a selection already wrapped in the markers is unwrapped.
pub fn apply_format(text: &str, sel: Range<usize>, fmt: Format) -> Edited {
    let sel = clamp(text, sel);
    let selected = &text[sel.clone()];
    if let Some((before, after)) = fmt.markers() {
        // Toggle off: markers right outside or inside the selection.
        let outside = sel.start >= before.len()
            && text[..sel.start].ends_with(before)
            && text[sel.end..].starts_with(after);
        if outside && !selected.is_empty() {
            let start = sel.start - before.len();
            let end = sel.end + after.len();
            let new = format!("{}{}{}", &text[..start], selected, &text[end..]);
            return Edited {
                text: new,
                selection: start..start + selected.len(),
            };
        }
        let inside = selected.len() >= before.len() + after.len()
            && selected.starts_with(before)
            && selected.ends_with(after);
        if inside {
            let inner = &selected[before.len()..selected.len() - after.len()];
            let new = format!("{}{}{}", &text[..sel.start], inner, &text[sel.end..]);
            return Edited {
                text: new,
                selection: sel.start..sel.start + inner.len(),
            };
        }
        let new = format!(
            "{}{before}{selected}{after}{}",
            &text[..sel.start],
            &text[sel.end..]
        );
        let start = sel.start + before.len();
        return Edited {
            text: new,
            selection: start..start + selected.len(),
        };
    }
    match fmt {
        Format::Link => {
            // `[text](https://)`: the URL placeholder is selected for typing.
            let label = selected;
            let url = "https://";
            let new = format!("{}[{label}]({url}){}", &text[..sel.start], &text[sel.end..]);
            let url_start = sel.start + 1 + label.len() + 2;
            Edited {
                text: new,
                selection: url_start..url_start + url.len(),
            }
        }
        Format::CodeBlock => {
            let lead = if sel.start == 0 || text[..sel.start].ends_with('\n') {
                ""
            } else {
                "\n"
            };
            let trail = if text[sel.end..].is_empty() || text[sel.end..].starts_with('\n') {
                ""
            } else {
                "\n"
            };
            let new = format!(
                "{}{lead}```\n{selected}\n```{trail}{}",
                &text[..sel.start],
                &text[sel.end..]
            );
            let start = sel.start + lead.len() + 4;
            Edited {
                text: new,
                selection: start..start + selected.len(),
            }
        }
        Format::Quote => {
            // Prefix every touched line with "> ".
            let line_start = text[..sel.start].rfind('\n').map_or(0, |i| i + 1);
            let line_end = text[sel.end..]
                .find('\n')
                .map_or(text.len(), |i| sel.end + i);
            let block = &text[line_start..line_end];
            let all_quoted = block.lines().all(|l| l.starts_with("> ")) && !block.is_empty();
            let lines: Vec<String> = if block.is_empty() {
                vec!["> ".to_owned()]
            } else if all_quoted {
                block
                    .split('\n')
                    .map(|l| l.strip_prefix("> ").unwrap_or(l).to_owned())
                    .collect()
            } else {
                block.split('\n').map(|l| format!("> {l}")).collect()
            };
            let replaced = lines.join("\n");
            let new = format!("{}{replaced}{}", &text[..line_start], &text[line_end..]);
            let end = line_start + replaced.len();
            Edited {
                text: new,
                selection: end..end,
            }
        }
        _ => Edited {
            text: text.to_owned(),
            selection: sel,
        },
    }
}

/// Inserts `insert` at `sel` (replacing the selection).
pub fn insert(text: &str, sel: Range<usize>, insert: &str) -> Edited {
    let sel = clamp(text, sel);
    let new = format!("{}{insert}{}", &text[..sel.start], &text[sel.end..]);
    let end = sel.start + insert.len();
    Edited {
        text: new,
        selection: end..end,
    }
}

/// Inserts a mention/word at the cursor with a space before it when the
/// previous character is not whitespace, and a trailing space.
pub fn insert_word(text: &str, sel: Range<usize>, word: &str) -> Edited {
    let sel = clamp(text, sel);
    let before = &text[..sel.start];
    let lead = if before.is_empty() || before.ends_with(char::is_whitespace) {
        ""
    } else {
        " "
    };
    insert(text, sel, &format!("{lead}{word} "))
}

fn clamp(text: &str, sel: Range<usize>) -> Range<usize> {
    let fix = |mut i: usize| {
        i = i.min(text.len());
        while !text.is_char_boundary(i) {
            i -= 1;
        }
        i
    };
    let (a, b) = (fix(sel.start), fix(sel.end));
    a.min(b)..a.max(b)
}

/// What an autocomplete popup completes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TriggerKind {
    User,
    Room,
    Emoji,
    Command,
}

/// The token being completed: `text[start..end]` is the whole token (with
/// its trigger character), `query` the part after the trigger character.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Trigger {
    pub kind: TriggerKind,
    pub start: usize,
    pub end: usize,
    pub query: String,
}

/// Minimum query length for `:` emoji completion (as the web client).
pub const EMOJI_MIN_QUERY: usize = 2;

/// The autocomplete trigger for a cursor at `cursor`: the word before the
/// cursor starting with `@`, `#`, `:` (≥ 2 characters, not yet closed) or a
/// `/` at the very start of the message.
pub fn trigger_at(text: &str, cursor: usize) -> Option<Trigger> {
    let cursor = clamp(text, cursor..cursor).start;
    let before = &text[..cursor];
    let start = before
        .char_indices()
        .rev()
        .find(|(_, c)| c.is_whitespace())
        .map_or(0, |(i, c)| i + c.len_utf8());
    let token = &before[start..];
    let mut chars = token.chars();
    let first = chars.next()?;
    let query: String = chars.collect();
    let kind = match first {
        '@' => TriggerKind::User,
        '#' => TriggerKind::Room,
        ':' if query.chars().count() >= EMOJI_MIN_QUERY
            && !query.contains(':')
            && query
                .chars()
                .all(|c| c.is_alphanumeric() || matches!(c, '_' | '-' | '+')) =>
        {
            TriggerKind::Emoji
        }
        '/' if start == 0 && !query.contains('/') => TriggerKind::Command,
        _ => return None,
    };
    if kind == TriggerKind::User && query.contains('@') {
        return None;
    }
    // The token extends to the end of the word after the cursor.
    let rest = &text[cursor..];
    let end = cursor
        + rest
            .char_indices()
            .find(|(_, c)| c.is_whitespace())
            .map_or(rest.len(), |(i, _)| i);
    Some(Trigger {
        kind,
        start,
        end,
        query,
    })
}

/// Replaces the trigger token with `replacement` plus a space (unless one
/// follows already); the cursor ends after the space.
pub fn complete(text: &str, trigger: &Trigger, replacement: &str) -> Edited {
    let range = clamp(text, trigger.start..trigger.end);
    let after = &text[range.end..];
    let space = if after.starts_with(' ') { "" } else { " " };
    let new = format!("{}{replacement}{space}{after}", &text[..range.start]);
    let cursor = range.start + replacement.len() + 1;
    Edited {
        text: new,
        selection: cursor..cursor,
    }
}

/// `/name params` → `(name, params)` (params trimmed), as the web client's
/// `^/([^\s]+)(.*)` parse.
pub fn parse_slash(text: &str) -> Option<(String, String)> {
    let rest = text.strip_prefix('/')?;
    let end = rest.find(char::is_whitespace).unwrap_or(rest.len());
    let name = &rest[..end];
    if name.is_empty() || name.contains('/') {
        return None;
    }
    Some((name.to_owned(), rest[end..].trim().to_owned()))
}

/// The message text with quote links before it (`prependReplies`): one
/// `[ ](permalink)` line per quoted message, then the text.
pub fn with_quotes(text: &str, permalinks: &[String]) -> String {
    let mut parts: Vec<String> = permalinks.iter().map(|l| format!("[ ]({l})")).collect();
    parts.push(text.to_owned());
    parts.join("\n")
}

/// Permalink of a message as the web client builds it (`getPermaLink`):
/// `{site}/channel/{name}?msg={mid}`, `/group/{name}`, `/direct/{rid}`,
/// `/live/{rid}`.
pub fn permalink(site: &str, t: RoomType, name: &str, rid: &str, mid: &str) -> String {
    let site = site.trim_end_matches('/');
    let path = match t {
        RoomType::Channel => format!("channel/{}", encode(name)),
        RoomType::Private => format!("group/{}", encode(name)),
        RoomType::Livechat => format!("live/{}", encode(rid)),
        _ => format!("direct/{}", encode(rid)),
    };
    format!("{site}/{path}?msg={}", encode(mid))
}

fn encode(s: &str) -> String {
    url::form_urlencoded::byte_serialize(s.as_bytes())
        .collect::<String>()
        .replace('+', "%20")
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[test]
    fn inline_format_wraps_and_toggles() {
        let e = apply_format("hello world", 6..11, Format::Bold);
        assert_eq!(e.text, "hello *world*");
        assert_eq!(&e.text[e.selection.clone()], "world");
        // Applying again unwraps.
        let e2 = apply_format(&e.text, e.selection.clone(), Format::Bold);
        assert_eq!(e2.text, "hello world");
        assert_eq!(e2.selection, 6..11);
        // Selection including the markers unwraps too.
        let e3 = apply_format("a ~b~", 2..5, Format::Strike);
        assert_eq!(e3.text, "a b");
    }

    #[test]
    fn inline_format_without_selection_places_cursor_inside() {
        let e = apply_format("x ", 2..2, Format::Code);
        assert_eq!(e.text, "x ``");
        assert_eq!(e.selection, 3..3);
        let e = apply_format("", 0..0, Format::Italic);
        assert_eq!(e.text, "__");
        assert_eq!(e.selection, 1..1);
    }

    #[test]
    fn link_selects_the_url() {
        let e = apply_format("see docs", 4..8, Format::Link);
        assert_eq!(e.text, "see [docs](https://)");
        assert_eq!(&e.text[e.selection.clone()], "https://");
    }

    #[test]
    fn code_block_on_own_lines() {
        let e = apply_format("run ls now", 4..6, Format::CodeBlock);
        assert_eq!(e.text, "run \n```\nls\n```\n now");
        assert_eq!(&e.text[e.selection.clone()], "ls");
        let e = apply_format("", 0..0, Format::CodeBlock);
        assert_eq!(e.text, "```\n\n```");
        assert_eq!(e.selection, 4..4);
    }

    #[test]
    fn quote_prefixes_lines_and_toggles() {
        let e = apply_format("one\ntwo\nthree", 1..5, Format::Quote);
        assert_eq!(e.text, "> one\n> two\nthree");
        let e = apply_format(&e.text, 2..8, Format::Quote);
        assert_eq!(e.text, "one\ntwo\nthree");
        let e = apply_format("", 0..0, Format::Quote);
        assert_eq!(e.text, "> ");
        assert_eq!(e.selection, 2..2);
    }

    #[test]
    fn insert_word_spacing() {
        assert_eq!(insert_word("hi", 2..2, "@bob").text, "hi @bob ");
        assert_eq!(insert_word("hi ", 3..3, "@bob").text, "hi @bob ");
        assert_eq!(insert_word("", 0..0, "@bob").text, "@bob ");
        let e = insert_word("ab", 1..1, ":x:");
        assert_eq!(e.text, "a :x: b");
        assert_eq!(e.selection, 6..6);
    }

    #[test]
    fn triggers() {
        let t = trigger_at("hello @al", 9).unwrap();
        assert_eq!(
            (t.kind, t.start, t.end, t.query.as_str()),
            (TriggerKind::User, 6, 9, "al")
        );
        let t = trigger_at("@", 1).unwrap();
        assert_eq!((t.kind, t.query.as_str()), (TriggerKind::User, ""));
        assert_eq!(trigger_at("#gen", 4).unwrap().kind, TriggerKind::Room);
        assert_eq!(trigger_at("x :sm", 5).unwrap().kind, TriggerKind::Emoji);
        assert!(trigger_at("x :s", 4).is_none(), "emoji needs 2 chars");
        assert!(trigger_at(":smile: ", 8).is_none());
        assert!(trigger_at(":smile:", 7).is_none(), "closed emoji");
        assert!(trigger_at("12:30", 5).is_none());
        assert_eq!(trigger_at("/gi", 3).unwrap().kind, TriggerKind::Command);
        assert!(
            trigger_at("a /gi", 5).is_none(),
            "commands only at the start"
        );
        assert!(trigger_at("mail me@host", 12).is_none());
        assert!(trigger_at("hello ", 6).is_none());
        // Cursor in the middle of a word: the token extends to its end.
        let t = trigger_at("@alice rest", 3).unwrap();
        assert_eq!((t.end, t.query.as_str()), (6, "al"));
        // Multi-byte text before the trigger.
        let t = trigger_at("привет @бо", "привет @бо".len()).unwrap();
        assert_eq!(t.query, "бо");
    }

    #[test]
    fn completion_replaces_token() {
        let t = trigger_at("hi @al", 6).unwrap();
        let e = complete("hi @al", &t, "@alice");
        assert_eq!(e.text, "hi @alice ");
        assert_eq!(e.selection, 10..10);
        let t = trigger_at("hi @al there", 5).unwrap();
        let e = complete("hi @al there", &t, "@alice");
        assert_eq!(e.text, "hi @alice there");
        assert_eq!(e.selection, 10..10);
        let t = trigger_at(":thu", 4).unwrap();
        assert_eq!(complete(":thu", &t, ":thumbsup:").text, ":thumbsup: ");
    }

    #[test]
    fn slash_parsing() {
        assert_eq!(
            parse_slash("/giphy  cats "),
            Some(("giphy".into(), "cats".into()))
        );
        assert_eq!(parse_slash("/leave"), Some(("leave".into(), String::new())));
        assert_eq!(parse_slash("/ x"), None);
        assert_eq!(parse_slash("hello /x"), None);
        assert_eq!(parse_slash("/usr/bin/ls"), None);
        assert_eq!(
            parse_slash("/msg @bob hi\nthere"),
            Some(("msg".into(), "@bob hi\nthere".into()))
        );
    }

    #[test]
    fn quotes_and_permalinks() {
        let l = permalink(
            "https://chat.example/",
            RoomType::Channel,
            "dev team",
            "R1",
            "M1",
        );
        assert_eq!(l, "https://chat.example/channel/dev%20team?msg=M1");
        assert_eq!(
            permalink("http://h", RoomType::Direct, "bob", "R2", "M2"),
            "http://h/direct/R2?msg=M2"
        );
        assert_eq!(
            permalink("http://h", RoomType::Private, "ops", "R3", "M3"),
            "http://h/group/ops?msg=M3"
        );
        assert_eq!(
            with_quotes("hello", &["L1".into(), "L2".into()]),
            "[ ](L1)\n[ ](L2)\nhello"
        );
        assert_eq!(with_quotes("hello", &[]), "hello");
    }
}
