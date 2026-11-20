//! The user's highlight words (`highlights` preference, the subscription's
//! `userHighlights`), matched like Rocket.Chat's `messageContainsHighlight`
//! (`apps/meteor/server/lib/notifications/message/messageContainsHighlight.ts`):
//! case-insensitive (simple case folding, the `iu` flags) whole words, where
//! a word character is one of `[\p{L}\p{N}_]` (a combining mark is not),
//! and a word wrapped in colons on both sides (an emoji code such as
//! `:rocket:`) does not count.

use std::ops::Range;
use std::sync::{Arc, LazyLock, Mutex};

use regex::Regex;

/// Highlight words compiled once ([`Highlights::cached`]): blank words are
/// ignored, a word that does not compile is skipped.
#[derive(Debug, Clone, Default)]
pub struct Highlights {
    /// The list it was built from, as given (the cache key).
    key: Vec<String>,
    res: Vec<Regex>,
}

/// Word lists compiled lately, newest first (one per account, typically).
static CACHE: LazyLock<Mutex<Vec<Arc<Highlights>>>> = LazyLock::new(Default::default);
const CACHE_LEN: usize = 8;

impl Highlights {
    pub fn new(words: &[String]) -> Self {
        Self {
            key: words.to_vec(),
            res: words
                .iter()
                .map(|w| w.trim())
                .filter(|w| !w.is_empty())
                .filter_map(|w| Regex::new(&format!("(?i){}", regex::escape(w))).ok())
                .collect(),
        }
    }

    /// [`Highlights::new`] of `words`, compiled only when this exact list
    /// is not among the last few compiled.
    pub fn cached(words: &[String]) -> Arc<Self> {
        Self::cached_in(&CACHE, words)
    }

    /// [`Highlights::cached`] with the cache `cache`.
    fn cached_in(cache: &Mutex<Vec<Arc<Self>>>, words: &[String]) -> Arc<Self> {
        let mut cache = cache.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(i) = cache.iter().position(|h| h.key == words) {
            let hit = cache.remove(i);
            cache.insert(0, Arc::clone(&hit));
            return hit;
        }
        let built = Arc::new(Self::new(words));
        cache.insert(0, Arc::clone(&built));
        cache.truncate(CACHE_LEN);
        built
    }

    /// No word to match.
    pub fn is_empty(&self) -> bool {
        self.res.is_empty()
    }

    /// The words that occur in `text` on their own (each matched alone).
    pub fn matching(&self, text: &str) -> Self {
        Self {
            key: Vec::new(),
            res: self
                .res
                .iter()
                .filter(|re| !text.is_empty() && !matches(re, text).is_empty())
                .cloned()
                .collect(),
        }
    }

    /// Byte ranges of `text` matching one of the words, in order and not
    /// overlapping (the longest word wins at a position).
    pub fn ranges(&self, text: &str) -> Vec<Range<usize>> {
        if text.is_empty() {
            return Vec::new();
        }
        let mut found: Vec<Range<usize>> =
            self.res.iter().flat_map(|re| matches(re, text)).collect();
        found.sort_by(|a, b| a.start.cmp(&b.start).then(b.end.cmp(&a.end)));
        let mut out: Vec<Range<usize>> = Vec::new();
        for r in found {
            if out.last().is_none_or(|l| r.start >= l.end) {
                out.push(r);
            }
        }
        out
    }

    /// Whether `text` contains one of the words.
    pub fn contains(&self, text: &str) -> bool {
        !text.is_empty() && self.res.iter().any(|re| !matches(re, text).is_empty())
    }
}

/// Byte ranges of `text` matching one of `words`, in order and not
/// overlapping (the longest word wins at a position). Blank words are
/// ignored.
pub fn ranges(text: &str, words: &[String]) -> Vec<Range<usize>> {
    if text.is_empty() || words.is_empty() {
        return Vec::new();
    }
    Highlights::cached(words).ranges(text)
}

/// Whether `text` contains one of `words`.
pub fn contains(text: &str, words: &[String]) -> bool {
    !text.is_empty() && !words.is_empty() && Highlights::cached(words).contains(text)
}

/// Every match of `re` in `text` that stands as a whole word, overlapping
/// candidates included (a rejected match does not hide a later one).
fn matches(re: &Regex, text: &str) -> Vec<Range<usize>> {
    let mut out = Vec::new();
    let mut pos = 0;
    while let Some(m) = re.find_at(text, pos) {
        if m.is_empty() {
            break;
        }
        let before = text[..m.start()].chars().next_back();
        let after = text[m.end()..].chars().next();
        let bounded = !before.is_some_and(is_word) && !after.is_some_and(is_word);
        let emoji = before == Some(':') && after == Some(':');
        if bounded && !emoji {
            out.push(m.range());
            pos = m.end();
        } else {
            pos = m.start() + text[m.start()..].chars().next().map_or(1, char::len_utf8);
        }
    }
    out
}

/// Rocket.Chat's word character class `[\p{L}\p{N}_]`.
static WORD: LazyLock<Option<Regex>> = LazyLock::new(|| Regex::new(r"^[\p{L}\p{N}_]$").ok());

fn is_word(c: char) -> bool {
    let mut buf = [0u8; 4];
    match WORD.as_ref() {
        Some(re) => re.is_match(c.encode_utf8(&mut buf)),
        None => c.is_alphanumeric() || c == '_',
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn w(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn whole_words_case_insensitive() {
        let words = w(&["deploy"]);
        assert!(contains("Deploy now", &words));
        assert!(contains("we DEPLOY.", &words));
        assert!(contains("(deploy)", &words));
        assert!(contains("deploy", &words));
        assert!(!contains("deployment", &words));
        assert!(!contains("redeploy", &words));
        assert!(!contains("deploy_x", &words));
        assert!(!contains("deploy2", &words));
        assert!(!contains("", &words));
        assert!(!contains("deploy", &w(&["", "  "])));
    }

    #[test]
    fn unicode_words() {
        let words = w(&["релиз"]);
        assert!(contains("Когда РЕЛИЗ?", &words));
        assert!(!contains("релизы", &words));
        assert!(!contains("пререлиз", &words));
        assert_eq!(ranges("Это Релиз!", &words), vec![7..17]);
    }

    #[test]
    fn emoji_codes_do_not_count() {
        let words = w(&["rocket"]);
        assert!(!contains(":rocket:", &words));
        assert!(contains(":rocket", &words));
        assert!(contains("rocket:", &words));
        assert!(contains("a :rocket: and a rocket", &words));
    }

    /// RC's `[\p{L}\p{N}_]`: a combining vowel sign (Mc/Mn) is a
    /// boundary; case folding is Unicode's simple one.
    #[test]
    fn rocket_chat_word_class_and_folding() {
        // Devanagari: राम + ा (U+093E, a spacing mark).
        assert!(contains("रामायण", &w(&["राम"])));
        // Thai: a vowel sign below (U+0E38) after the word.
        assert!(contains("กุ", &w(&["ก"])));
        // Arabic fatha (U+064E).
        assert!(contains("بَ", &w(&["ب"])));
        // Greek final sigma and the long s fold like the server's.
        assert!(contains("ΛΌΓΟΣ", &w(&["λόγος"])));
        assert!(contains("λόγοσ", &w(&["λόγος"])));
        assert!(contains("ſhip it", &w(&["ship"])));
        // A rejected candidate does not hide an overlapping one.
        assert_eq!(ranges("aaa aa", &w(&["aa"])), vec![4..6]);
    }

    #[test]
    fn ranges_are_ordered_and_longest_first() {
        let words = w(&["go", "go live", " live "]);
        assert_eq!(
            ranges("go live, Go and LIVE", &words),
            vec![0..7, 9..11, 16..20]
        );
        assert_eq!(
            Highlights::new(&words).ranges("go live, Go and LIVE"),
            vec![0..7, 9..11, 16..20]
        );
        // Words may contain spaces and punctuation.
        assert_eq!(ranges("ship v1.2 today", &w(&["v1.2"])), vec![5..9]);
    }

    #[test]
    fn compiled_once_and_reused() {
        let cache = Mutex::default();
        let words = w(&["cache-probe-α", "  ", "cache-probe-β"]);
        let a = Highlights::cached_in(&cache, &words);
        let b = Highlights::cached_in(&cache, &words);
        assert!(Arc::ptr_eq(&a, &b));
        assert_eq!(a.res.len(), 2);
        assert!(!Arc::ptr_eq(
            &a,
            &Highlights::cached_in(&cache, &w(&["cache-probe-α"]))
        ));
        // The least recently used list is evicted beyond CACHE_LEN.
        for i in 0..CACHE_LEN - 1 {
            Highlights::cached_in(&cache, &w(&[&format!("filler-{i}")]));
        }
        assert_eq!(cache.lock().map(|c| c.len()).ok(), Some(CACHE_LEN));
        assert!(!Arc::ptr_eq(&a, &Highlights::cached_in(&cache, &words)));
        assert!(Highlights::new(&w(&["", " "])).is_empty());
    }

    #[test]
    fn matching_keeps_the_words_found_alone() {
        let h = Highlights::new(&w(&["deploy", "ship", "go live"]));
        let only = h.matching("Deploy now, then go live");
        assert_eq!(only.res.len(), 2);
        assert_eq!(only.ranges("ship, deploy, go live"), vec![6..12, 14..21]);
        assert!(h.matching("").is_empty());
        assert!(!h.contains(""));
        assert!(h.contains("ship it"));
    }
}
