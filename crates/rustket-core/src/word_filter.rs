//! Hidden words: messages containing one of the user's phrases are collapsed
//! in the timeline (local, all accounts). Matching ignores case; with
//! `whole_word` a phrase must not touch a letter or digit on either side
//! (letters of scripts written without spaces, such as Thai or Chinese,
//! never count as touching).

use serde::{Deserialize, Serialize};

/// Most phrases kept (the editor refuses more).
pub const MAX_PHRASES: usize = 200;
/// Longest phrase in characters.
pub const MAX_PHRASE_CHARS: usize = 100;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct HiddenWords {
    /// As the user typed them (trimmed, no duplicates ignoring case).
    pub phrases: Vec<String>,
    /// Match whole words only ("ass" does not hide "class").
    pub whole_word: bool,
}

impl Default for HiddenWords {
    fn default() -> Self {
        Self {
            phrases: Vec::new(),
            whole_word: true,
        }
    }
}

/// Why a phrase was not added.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AddError {
    Empty,
    TooLong,
    Duplicate,
    TooMany,
}

/// Case folding used on both sides of a match.
fn fold(s: &str) -> String {
    s.to_lowercase()
}

impl HiddenWords {
    pub fn is_empty(&self) -> bool {
        self.phrases.iter().all(|p| p.trim().is_empty())
    }

    /// Adds `phrase` (trimmed, inner whitespace collapsed).
    pub fn add(&mut self, phrase: &str) -> Result<(), AddError> {
        let p = phrase.split_whitespace().collect::<Vec<_>>().join(" ");
        if p.is_empty() {
            return Err(AddError::Empty);
        }
        if p.chars().count() > MAX_PHRASE_CHARS {
            return Err(AddError::TooLong);
        }
        let folded = fold(&p);
        if self.phrases.iter().any(|x| fold(x) == folded) {
            return Err(AddError::Duplicate);
        }
        if self.phrases.len() >= MAX_PHRASES {
            return Err(AddError::TooMany);
        }
        self.phrases.push(p);
        Ok(())
    }

    /// Removes the phrase at `index` (out of range: nothing).
    pub fn remove(&mut self, index: usize) {
        if index < self.phrases.len() {
            self.phrases.remove(index);
        }
    }

    /// Whether `text` contains one of the phrases.
    pub fn matches(&self, text: &str) -> bool {
        if self.is_empty() || text.is_empty() {
            return false;
        }
        let hay = fold(text);
        self.phrases
            .iter()
            .map(|p| fold(p.trim()))
            .filter(|p| !p.is_empty())
            .any(|p| contains(&hay, &p, self.whole_word))
    }

    /// A stable hash of the filter (row cache keys).
    pub fn key(&self) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        self.phrases.hash(&mut h);
        self.whole_word.hash(&mut h);
        h.finish()
    }
}

/// Scripts written without spaces between words (Thai, Lao, Myanmar, Khmer,
/// Chinese, Japanese): no word boundary can be seen next to their letters.
fn is_unspaced(c: char) -> bool {
    matches!(
        c,
        '\u{0E00}'..='\u{0EFF}'
            | '\u{1000}'..='\u{109F}'
            | '\u{1780}'..='\u{17FF}'
            | '\u{3040}'..='\u{30FF}'
            | '\u{3400}'..='\u{4DBF}'
            | '\u{4E00}'..='\u{9FFF}'
            | '\u{F900}'..='\u{FAFF}'
            | '\u{FF66}'..='\u{FF9F}'
            | '\u{20000}'..='\u{3134F}'
    )
}

/// A letter, digit or `_` of a script that separates words with spaces.
fn is_word_char(c: char) -> bool {
    (c.is_alphanumeric() || c == '_') && !is_unspaced(c)
}

/// `needle` in `hay` (both folded); with `whole` the match must not be
/// preceded or followed by a word character.
fn contains(hay: &str, needle: &str, whole: bool) -> bool {
    if !whole {
        return hay.contains(needle);
    }
    let mut from = 0;
    while let Some(pos) = hay[from..].find(needle) {
        let start = from + pos;
        let end = start + needle.len();
        let before = hay[..start].chars().next_back();
        let after = hay[end..].chars().next();
        // A phrase that itself starts or ends with punctuation (or a letter
        // of a script without spaces) needs no boundary on that side.
        let first = needle.chars().next().is_some_and(is_word_char);
        let last = needle.chars().next_back().is_some_and(is_word_char);
        let left_ok = !first || !before.is_some_and(is_word_char);
        let right_ok = !last || !after.is_some_and(is_word_char);
        if left_ok && right_ok {
            return true;
        }
        // Next char boundary after `start`.
        from = start + hay[start..].chars().next().map_or(1, char::len_utf8);
        if from >= hay.len() {
            break;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    fn filter(words: &[&str], whole: bool) -> HiddenWords {
        HiddenWords {
            phrases: words.iter().map(|s| (*s).to_owned()).collect(),
            whole_word: whole,
        }
    }

    #[test]
    fn case_insensitive() {
        let f = filter(&["Spoiler"], true);
        assert!(f.matches("big SPOILER ahead"));
        assert!(f.matches("spoiler"));
        assert!(!f.matches("nothing here"));
        // Non-ASCII folding.
        let f = filter(&["ПРИВЕТ"], true);
        assert!(f.matches("ну привет, мир"));
    }

    #[test]
    fn whole_word_boundaries() {
        let f = filter(&["ass"], true);
        assert!(!f.matches("first class"));
        assert!(!f.matches("assessment"));
        assert!(f.matches("what an ass!"));
        assert!(f.matches("ass"));
        assert!(f.matches("(ass)"));
        // The second occurrence counts when the first is inside a word.
        assert!(f.matches("class ass"));
        let f = filter(&["ass"], false);
        assert!(f.matches("first class"));
    }

    #[test]
    fn phrases_and_punctuation() {
        let f = filter(&["bad word"], true);
        assert!(f.matches("this is a Bad Word."));
        assert!(!f.matches("bad wording"));
        let f = filter(&["c++"], true);
        assert!(f.matches("I like c++ a lot"));
        assert!(f.matches("c++!"));
        let f = filter(&["日本"], true);
        assert!(f.matches("日本"));
    }

    #[test]
    fn whole_word_in_scripts_without_spaces() {
        // Chinese, Japanese and Thai run words together: whole-word mode
        // still finds them inside a sentence.
        assert!(filter(&["日本"], true).matches("我爱日本。"));
        assert!(filter(&["ネタバレ"], true).matches("これはネタバレです"));
        assert!(filter(&["สปอยล์"], true).matches("อันนี้สปอยล์นะ"));
        // A Latin word between letters of such a script is still a word.
        let f = filter(&["spoiler"], true);
        assert!(f.matches("这是spoiler啊"));
        assert!(!f.matches("这是spoilers啊"));
        assert!(!filter(&["ass"], true).matches("这是class啊"));
    }

    #[test]
    fn empty_filter_matches_nothing() {
        assert!(!HiddenWords::default().matches("anything"));
        assert!(!filter(&["  "], false).matches("anything"));
        assert!(!filter(&["x"], false).matches(""));
    }

    #[test]
    fn add_and_remove() {
        let mut f = HiddenWords::default();
        assert_eq!(f.add("  hello   there "), Ok(()));
        assert_eq!(f.phrases, vec!["hello there".to_owned()]);
        assert_eq!(f.add("HELLO there"), Err(AddError::Duplicate));
        assert_eq!(f.add("   "), Err(AddError::Empty));
        assert_eq!(
            f.add(&"x".repeat(MAX_PHRASE_CHARS + 1)),
            Err(AddError::TooLong)
        );
        f.remove(5);
        f.remove(0);
        assert!(f.phrases.is_empty());
        for i in 0..MAX_PHRASES {
            f.add(&format!("w{i}")).unwrap();
        }
        assert_eq!(f.add("one more"), Err(AddError::TooMany));
    }

    #[test]
    fn key_follows_content() {
        let a = filter(&["a"], true);
        let mut b = a.clone();
        assert_eq!(a.key(), b.key());
        b.whole_word = false;
        assert_ne!(a.key(), b.key());
    }

    #[test]
    fn toml_roundtrip_and_defaults() {
        let f: HiddenWords = toml::from_str("phrases = [\"x\"]").unwrap();
        assert!(f.whole_word);
        assert_eq!(f.phrases, vec!["x".to_owned()]);
        let s = toml::to_string(&f).unwrap();
        assert_eq!(toml::from_str::<HiddenWords>(&s).unwrap(), f);
    }
}
