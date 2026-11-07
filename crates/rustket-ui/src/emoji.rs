//! Emoji detection and colour-emoji text helpers.
//!
//! On Linux the generic font fallback reaches monochrome emoji glyphs (DejaVu Sans,
//! Noto Sans Symbols) before Noto Color Emoji, so emoji runs must be given the
//! colour emoji font explicitly: use [`spans`] / [`text_with_emoji`] for text that
//! may contain emoji.

use iced::Font;
use iced::widget::text::{Rich, Span};
use iced::widget::{rich_text, span};
use unicode_properties::{EmojiStatus, UnicodeEmoji};
use unicode_segmentation::UnicodeSegmentation;

/// System colour emoji font.
pub const EMOJI_FONT: Font = Font::with_name("Noto Color Emoji");

/// True when the grapheme cluster `g` should be shown as a (colour) emoji:
/// default-emoji-presentation characters, anything with VS16 (U+FE0F), keycaps,
/// flags, modifier and ZWJ sequences. Text-presentation characters such as
/// © ™ ✓ ★ ♥ → stay text unless followed by VS16; VS15 (U+FE0E) forces text.
pub fn is_emoji_grapheme(g: &str) -> bool {
    let mut chars = g.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    if g.contains('\u{FE0E}') {
        return false;
    }
    // Keycap bases (digits, #, *) are emoji only as a full keycap sequence.
    if matches!(first, '0'..='9' | '#' | '*') {
        return g.contains('\u{20E3}');
    }
    let status = first.emoji_status();
    // Lone joiners/selectors/keycap marks are not emoji.
    if !first.is_emoji_char() {
        return false;
    }
    if g.contains('\u{FE0F}') {
        return true;
    }
    if matches!(
        status,
        EmojiStatus::EmojiPresentation
            | EmojiStatus::EmojiPresentationAndModifierBase
            | EmojiStatus::EmojiPresentationAndEmojiComponent
            | EmojiStatus::EmojiPresentationAndModifierAndEmojiComponent
    ) {
        return true;
    }
    // Text-default base made emoji by a modifier or a ZWJ sequence.
    first.is_emoji_char()
        && g.chars().skip(1).any(|c| {
            c == '\u{200D}'
                || matches!(
                    c.emoji_status(),
                    EmojiStatus::EmojiPresentationAndModifierAndEmojiComponent
                )
        })
}

/// Splits `text` into alternating runs on grapheme boundaries; `true` marks emoji
/// runs. Whitespace between two emoji stays in the emoji run so "👍 🎉" is a single
/// run. Linear in the input length.
pub fn segments(text: &str) -> Vec<(bool, &str)> {
    let mut out: Vec<(bool, &str)> = Vec::new();
    // Start of the current run and its kind.
    let mut start = 0;
    let mut current: Option<bool> = None;
    // Start of a pending whitespace stretch that follows an emoji run.
    let mut pending_ws: Option<usize> = None;
    for (i, g) in text.grapheme_indices(true) {
        if g.chars().all(char::is_whitespace) {
            if current == Some(true) && pending_ws.is_none() {
                pending_ws = Some(i);
            } else if current.is_none() {
                current = Some(false);
            }
            continue;
        }
        let is_emoji = is_emoji_grapheme(g);
        match (current, pending_ws.take()) {
            (Some(true), Some(_)) if is_emoji => {}
            (Some(true), Some(ws)) => {
                // Whitespace after an emoji run joins the following text.
                out.push((true, &text[start..ws]));
                start = ws;
                current = Some(false);
            }
            (Some(kind), _) if kind == is_emoji => {}
            (Some(kind), _) => {
                out.push((kind, &text[start..i]));
                start = i;
                current = Some(is_emoji);
            }
            (None, _) => current = Some(is_emoji),
        }
    }
    if let Some(kind) = current {
        match pending_ws {
            Some(ws) if kind => {
                out.push((true, &text[start..ws]));
                out.push((false, &text[ws..]));
            }
            _ => out.push((kind, &text[start..])),
        }
    }
    out
}

/// Emoji grapheme at the start of `s` after skipping non-emoji graphemes.
pub fn first_emoji(s: &str) -> Option<&str> {
    s.graphemes(true).find(|g| is_emoji_grapheme(g))
}

/// True when `text` consists only of emoji (and whitespace), e.g. for "big emoji"
/// rendering of a message.
pub fn is_emoji_only(text: &str) -> bool {
    let trimmed = text.trim();
    !trimmed.is_empty() && segments(trimmed).iter().all(|(emoji, _)| *emoji)
}

/// Text spans with emoji runs set in [`EMOJI_FONT`].
pub fn spans<'a, Link>(text: &'a str) -> Vec<Span<'a, Link, Font>> {
    segments(text)
        .into_iter()
        .map(|(emoji, run)| {
            let s = span(run);
            if emoji { s.font(EMOJI_FONT) } else { s }
        })
        .collect()
}

/// Like [`spans`], but owning the text.
pub fn owned_spans<Link>(text: &str) -> Vec<Span<'static, Link, Font>> {
    segments(text)
        .into_iter()
        .map(|(emoji, run)| {
            let s = span(run.to_owned());
            if emoji { s.font(EMOJI_FONT) } else { s }
        })
        .collect()
}

/// Rich text for plain strings that may contain emoji.
pub fn text_with_emoji<'a, Message: 'a>(text: &'a str) -> Rich<'a, (), Message> {
    rich_text(spans(text))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_runs() {
        assert_eq!(
            segments("hi 👍🎉 there"),
            vec![(false, "hi "), (true, "👍🎉"), (false, " there")]
        );
        assert_eq!(segments("👩‍💻 ok"), vec![(true, "👩‍💻"), (false, " ok")]);
        assert_eq!(segments("👍 🎉 x"), vec![(true, "👍 🎉"), (false, " x")]);
        assert_eq!(segments("Привет"), vec![(false, "Привет")]);
        assert_eq!(segments(""), vec![]);
        assert_eq!(segments("❤️"), vec![(true, "❤️")]);
        assert_eq!(
            segments("😀\u{200D}x"),
            vec![(true, "😀\u{200D}"), (false, "x")]
        );
        assert_eq!(segments("a  "), vec![(false, "a  ")]);
        assert_eq!(segments("👍 "), vec![(true, "👍"), (false, " ")]);
        assert_eq!(segments(" 👍"), vec![(false, " "), (true, "👍")]);
    }

    #[test]
    fn emoji_only() {
        assert!(is_emoji_only("👍"));
        assert!(is_emoji_only(" 👍 🎉🔥 "));
        assert!(is_emoji_only("👍🏽"));
        assert!(!is_emoji_only("👍 ok"));
        assert!(!is_emoji_only(""));
        assert!(!is_emoji_only("   "));
        for text_like in ["©", "®", "™", "⌘", "✓", "★", "♥", "→", "*", "123", "#"] {
            assert!(!is_emoji_only(text_like), "{text_like}");
        }
        for emoji in ["#️⃣", "1️⃣", "↔️", "▶️", "🇷🇺", "👨‍👩‍👧", "✅", "⭐", "©️", "❤️"]
        {
            assert!(is_emoji_only(emoji), "{emoji}");
        }
        assert!(!is_emoji_only("✅\u{FE0E}"));
        for not_emoji in ["\u{FE0F}", "\u{20E3}", "1\u{FE0F}", "#\u{FE0F}", "\u{200D}"] {
            assert!(!is_emoji_only(not_emoji), "{not_emoji:?}");
        }
    }

    #[test]
    fn segments_is_linear() {
        let big = format!("👍{}👍", " ".repeat(200_000));
        let t = std::time::Instant::now();
        assert_eq!(segments(&big).len(), 1);
        assert!(t.elapsed() < std::time::Duration::from_millis(500));
    }
}
