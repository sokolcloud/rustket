//! Emoji tables: shortcode ↔ Unicode, custom emoji registry, search and picker data.
//!
//! The Unicode table is Rocket.Chat 8.9's own native emoji list, generated from
//! emojibase-data 16.0.3 by `data/gen_emoji.py` exactly like the server's
//! `generateEmojiData.ts` + `legacyEmojioneMap.ts`: the first JoyPixels shortcode
//! is an emoji's primary name, further JoyPixels names are aliases, emojibase
//! names are used only when JoyPixels has none, and skin tones are
//! `<primary>_tone<n>`. The server accepts a reaction only when its key is in
//! that list (or names a custom emoji), and stores aliases as separate keys, so
//! outgoing keys ([`to_shortcode`], picker, search) are always primary names —
//! the same keys the web client sends. Reaction keys are `:shortcode:`.

use std::collections::{BTreeMap, HashMap};
use std::sync::LazyLock;

use rocketchat::ServerUrl;
use rocketchat::models::CustomEmoji;

/// The generated table (see the module docs and `data/gen_emoji.py`).
const TABLE: &str = include_str!("../data/emoji.tsv");

/// Emoji picker category, in Rocket.Chat's picker order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum EmojiCategory {
    /// Smileys & emotion and people & body.
    People,
    Nature,
    Food,
    Activity,
    Travel,
    Objects,
    Symbols,
    Flags,
}

impl EmojiCategory {
    pub const ALL: [Self; 8] = [
        Self::People,
        Self::Nature,
        Self::Food,
        Self::Activity,
        Self::Travel,
        Self::Objects,
        Self::Symbols,
        Self::Flags,
    ];

    /// emojibase group number → category (`generateEmojiData.ts` `groupToCategory`).
    fn from_group(g: u8) -> Option<Self> {
        Some(match g {
            0 | 1 => Self::People,
            3 => Self::Nature,
            4 => Self::Food,
            5 => Self::Travel,
            6 => Self::Activity,
            7 => Self::Objects,
            8 => Self::Symbols,
            9 => Self::Flags,
            _ => return None,
        })
    }

    pub fn i18n_key(&self) -> &'static str {
        match self {
            Self::People => "emoji.people",
            Self::Nature => "emoji.nature",
            Self::Food => "emoji.food",
            Self::Activity => "emoji.activities",
            Self::Travel => "emoji.travel",
            Self::Objects => "emoji.objects",
            Self::Symbols => "emoji.symbols",
            Self::Flags => "emoji.flags",
        }
    }
}

/// One emoji of the table (default skin tone).
#[derive(Debug)]
struct Entry {
    category: Option<EmojiCategory>,
    unicode: &'static str,
    /// Primary shortcode first.
    codes: Vec<&'static str>,
    /// Shown in the picker (not a regional indicator).
    picker: bool,
    /// (tone spec, unicode), e.g. ("3", "👍🏽") or ("5-4", "🫱🏿‍🫲🏾").
    tones: Vec<(&'static str, &'static str)>,
}

impl Entry {
    fn primary(&self) -> &'static str {
        self.codes[0]
    }
}

#[derive(Debug, Default)]
struct Data {
    entries: Vec<Entry>,
    /// Every key the server accepts → Unicode.
    by_key: HashMap<String, &'static str>,
    /// Unicode without U+FE0F → the key Rocket.Chat's picker sends.
    reverse: HashMap<String, String>,
}

fn bare(unicode: &str) -> String {
    unicode.chars().filter(|c| *c != '\u{FE0F}').collect()
}

fn parse_table(text: &'static str) -> Data {
    let mut d = Data::default();
    let mut index: HashMap<&str, usize> = HashMap::new();
    let mut legacy = Vec::new();
    for line in text.lines() {
        let f: Vec<&'static str> = line.split('\t').collect();
        match f.as_slice() {
            ["E", group, unicode, codes, picker] => {
                let codes: Vec<&'static str> = codes.split(' ').filter(|c| !c.is_empty()).collect();
                if codes.is_empty() {
                    continue;
                }
                for c in &codes {
                    d.by_key.insert((*c).to_owned(), unicode);
                }
                index.insert(codes[0], d.entries.len());
                d.entries.push(Entry {
                    category: group.parse().ok().and_then(EmojiCategory::from_group),
                    unicode,
                    codes,
                    picker: *picker == "1",
                    tones: Vec::new(),
                });
            }
            ["T", primary, spec, unicode, extra] => {
                for c in extra.split(' ').filter(|c| !c.is_empty()) {
                    d.by_key.insert(c.to_owned(), unicode);
                }
                d.by_key.insert(format!("{primary}_tone{spec}"), unicode);
                if let Some(&i) = index.get(primary) {
                    d.entries[i].tones.push((spec, unicode));
                }
            }
            ["L", code, unicode] => legacy.push((*code, *unicode)),
            _ => {}
        }
    }
    for (code, unicode) in legacy {
        d.by_key.entry(code.to_owned()).or_insert(unicode);
    }
    for e in &d.entries {
        d.reverse
            .entry(bare(e.unicode))
            .or_insert_with(|| e.primary().to_owned());
        for (spec, unicode) in &e.tones {
            d.reverse
                .entry(bare(unicode))
                .or_insert_with(|| format!("{}_tone{spec}", e.primary()));
        }
    }
    d
}

static DATA: LazyLock<Data> = LazyLock::new(|| parse_table(TABLE));

fn strip_colons(code: &str) -> &str {
    let c = code.trim();
    let c = c.strip_prefix(':').unwrap_or(c);
    c.strip_suffix(':').unwrap_or(c)
}

/// The Unicode emoji for any key Rocket.Chat 8.9 knows (`thumbsup`, `:thumbsup:`,
/// `:+1:`, `:slight_smile:`, `:flag_ru:`, `:thumbsup_tone3:`, legacy emojione names).
pub fn to_unicode(code: &str) -> Option<&'static str> {
    let name = strip_colons(code);
    if name.is_empty() {
        return None;
    }
    DATA.by_key.get(name).copied()
}

/// The `:shortcode:` Rocket.Chat's picker sends for a Unicode emoji, e.g. `👍` →
/// `:thumbsup:`, `🇷🇺` → `:flag_ru:`, `👍🏽` → `:thumbsup_tone3:`. Variation
/// selectors (U+FE0F) are ignored.
pub fn to_shortcode(unicode: &str) -> Option<String> {
    DATA.reverse.get(&bare(unicode)).map(|k| format!(":{k}:"))
}

/// Whether the server accepts `code` as a Unicode emoji key.
pub fn is_known_shortcode(code: &str) -> bool {
    to_unicode(code).is_some()
}

/// The key of `key`'s emoji with skin tone `tone` (1–5), e.g. (`:thumbsup:`, 3)
/// → `:thumbsup_tone3:`. `None` when the emoji has no skin tones.
pub fn with_skin_tone(key: &str, tone: u8) -> Option<String> {
    let unicode = to_unicode(key)?;
    let primary = DATA.reverse.get(&bare(unicode))?;
    let e = DATA.entries.iter().find(|e| e.primary() == primary)?;
    let spec = tone.to_string();
    e.tones
        .iter()
        .any(|(s, _)| *s == spec)
        .then(|| format!(":{primary}_tone{tone}:"))
}

/// Every key the server accepts as a Unicode emoji (without colons).
pub fn all_shortcodes() -> impl Iterator<Item = &'static str> {
    DATA.by_key.keys().map(String::as_str)
}

// --- custom emoji ---------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResolvedEmoji {
    Unicode(&'static str),
    Custom {
        name: String,
        extension: String,
        etag: Option<String>,
    },
}

impl ResolvedEmoji {
    /// Image URL of a custom emoji (`/emoji-custom/{name}.{ext}?etag=`).
    pub fn custom_url(&self, server: &ServerUrl) -> Option<String> {
        match self {
            Self::Custom {
                name,
                extension,
                etag,
            } => {
                let mut url = server.emoji_custom(name, extension);
                if let Some(e) = etag.as_deref().filter(|e| !e.is_empty()) {
                    url.push_str("?etag=");
                    url.push_str(e);
                }
                Some(url)
            }
            Self::Unicode(_) => None,
        }
    }
}

/// Custom emoji of a server, by name and alias. Custom names take precedence
/// over Unicode shortcodes.
#[derive(Debug, Clone, Default)]
pub struct EmojiRegistry {
    by_id: BTreeMap<String, CustomEmoji>,
    names: HashMap<String, String>,
}

impl EmojiRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn set_all(&mut self, list: impl IntoIterator<Item = CustomEmoji>) {
        self.by_id.clear();
        for e in list {
            self.by_id.insert(e.id.clone(), e);
        }
        self.reindex();
    }

    pub fn upsert(&mut self, e: CustomEmoji) {
        self.by_id.insert(e.id.clone(), e);
        self.reindex();
    }

    /// Removes by `_id`, or by name when the id is unknown.
    pub fn remove(&mut self, id_or_name: &str) -> bool {
        let id = if self.by_id.contains_key(id_or_name) {
            Some(id_or_name.to_owned())
        } else {
            self.names.get(id_or_name).cloned()
        };
        let removed = id.and_then(|id| self.by_id.remove(&id)).is_some();
        if removed {
            self.reindex();
        }
        removed
    }

    fn reindex(&mut self) {
        self.names.clear();
        for e in self.by_id.values() {
            for a in &e.aliases {
                self.names.insert(a.clone(), e.id.clone());
            }
        }
        // Names win over aliases.
        for e in self.by_id.values() {
            self.names.insert(e.name.clone(), e.id.clone());
        }
    }

    pub fn len(&self) -> usize {
        self.by_id.len()
    }

    pub fn is_empty(&self) -> bool {
        self.by_id.is_empty()
    }

    pub fn custom(&self) -> impl Iterator<Item = &CustomEmoji> + '_ {
        self.by_id.values()
    }

    pub fn get_custom(&self, name_or_alias: &str) -> Option<&CustomEmoji> {
        self.by_id.get(self.names.get(strip_colons(name_or_alias))?)
    }

    fn custom_resolved(c: &CustomEmoji) -> ResolvedEmoji {
        ResolvedEmoji::Custom {
            name: c.name.clone(),
            extension: c.extension.clone(),
            etag: c.etag.clone(),
        }
    }

    /// Resolves `:code:` to a custom emoji (preferred) or a Unicode emoji.
    pub fn resolve(&self, code: &str) -> Option<ResolvedEmoji> {
        if let Some(c) = self.get_custom(code) {
            return Some(Self::custom_resolved(c));
        }
        to_unicode(code).map(ResolvedEmoji::Unicode)
    }

    /// Whether the server accepts `code` as a reaction (Unicode key or custom
    /// emoji name/alias).
    pub fn is_valid_reaction(&self, code: &str) -> bool {
        self.get_custom(code).is_some() || is_known_shortcode(code)
    }

    /// Autocomplete / picker search: exact, then prefix, then substring matches
    /// over custom names/aliases and Unicode shortcodes (aliases included); custom
    /// emoji first within a rank. `query` may carry colons. Keys are primary names.
    pub fn search(&self, query: &str, limit: usize) -> Vec<EmojiMatch> {
        let q = strip_colons(query).to_lowercase();
        if q.is_empty() || limit == 0 {
            return Vec::new();
        }
        let rank = |name: &str| -> Option<u8> {
            if name == q {
                Some(0)
            } else if name.starts_with(&q) {
                Some(1)
            } else if name.contains(&q) {
                Some(2)
            } else {
                None
            }
        };
        // (rank, custom-first, matched len, key)
        let mut found: Vec<(u8, u8, usize, EmojiMatch)> = Vec::new();
        let mut best: HashMap<String, usize> = HashMap::new();
        let mut push = |r: u8, custom: u8, m: EmojiMatch, found: &mut Vec<_>| {
            if let Some(&i) = best.get(&m.key) {
                let (or, _, ol, _) = &found[i];
                if (r, m.matched.len()) < (*or, *ol) {
                    found[i] = (r, custom, m.matched.len(), m);
                }
                return;
            }
            best.insert(m.key.clone(), found.len());
            found.push((r, custom, m.matched.len(), m));
        };
        for c in self.by_id.values() {
            for name in std::iter::once(&c.name).chain(c.aliases.iter()) {
                if let Some(r) = rank(&name.to_lowercase()) {
                    push(
                        r,
                        0,
                        EmojiMatch {
                            key: format!(":{}:", c.name),
                            matched: name.clone(),
                            emoji: Self::custom_resolved(c),
                        },
                        &mut found,
                    );
                }
            }
        }
        for e in DATA.entries.iter().filter(|e| e.picker) {
            if self.names.contains_key(e.primary()) {
                continue;
            }
            for name in &e.codes {
                if let Some(r) = rank(&name.to_lowercase()) {
                    push(
                        r,
                        1,
                        EmojiMatch {
                            key: format!(":{}:", e.primary()),
                            matched: (*name).to_owned(),
                            emoji: ResolvedEmoji::Unicode(e.unicode),
                        },
                        &mut found,
                    );
                }
            }
        }
        found.sort_by(|a, b| (a.0, a.1, a.2, &a.3.key).cmp(&(b.0, b.1, b.2, &b.3.key)));
        found.into_iter().take(limit).map(|f| f.3).collect()
    }

    /// Emoji picker sections: recent (from `recent` keys), custom, then the
    /// Unicode categories in Rocket.Chat's order with its primary keys.
    pub fn picker_categories(&self, recent: &[String]) -> Vec<PickerCategory> {
        let mut out = Vec::new();
        let recent_items: Vec<PickerEmoji> = recent
            .iter()
            .filter_map(|k| {
                self.resolve(k).map(|emoji| PickerEmoji {
                    key: format!(":{}:", strip_colons(k)),
                    emoji,
                })
            })
            .collect();
        if !recent_items.is_empty() {
            out.push(PickerCategory {
                kind: CategoryKind::Recent,
                emojis: recent_items,
            });
        }
        if !self.by_id.is_empty() {
            let mut custom: Vec<PickerEmoji> = self
                .by_id
                .values()
                .map(|c| PickerEmoji {
                    key: format!(":{}:", c.name),
                    emoji: Self::custom_resolved(c),
                })
                .collect();
            custom.sort_by(|a, b| a.key.cmp(&b.key));
            out.push(PickerCategory {
                kind: CategoryKind::Custom,
                emojis: custom,
            });
        }
        for cat in EmojiCategory::ALL {
            let emojis: Vec<PickerEmoji> = DATA
                .entries
                .iter()
                .filter(|e| e.picker && e.category == Some(cat))
                .map(|e| PickerEmoji {
                    key: format!(":{}:", e.primary()),
                    emoji: ResolvedEmoji::Unicode(e.unicode),
                })
                .collect();
            out.push(PickerCategory {
                kind: CategoryKind::Unicode(cat),
                emojis,
            });
        }
        out
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmojiMatch {
    /// Key to insert / react with (`:name:`).
    pub key: String,
    /// Name or alias that matched the query.
    pub matched: String,
    pub emoji: ResolvedEmoji,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CategoryKind {
    Recent,
    Custom,
    Unicode(EmojiCategory),
}

impl CategoryKind {
    pub fn i18n_key(&self) -> &'static str {
        match self {
            Self::Recent => "emoji.recent",
            Self::Custom => "emoji.custom",
            Self::Unicode(c) => c.i18n_key(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PickerEmoji {
    pub key: String,
    pub emoji: ResolvedEmoji,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PickerCategory {
    pub kind: CategoryKind,
    pub emojis: Vec<PickerEmoji>,
}

/// Moves `key` to the front of `list`, deduplicated and truncated to `max`.
pub fn push_recent(list: &mut Vec<String>, key: &str, max: usize) {
    let key = format!(":{}:", strip_colons(key));
    list.retain(|k| k != &key);
    list.insert(0, key);
    list.truncate(max);
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn table_matches_rocket_chat_list_size() {
        // generateEmojiData + legacyEmojioneMap on emojibase-data 16.0.3.
        assert_eq!(DATA.by_key.len(), 6564);
        let picker = DATA.entries.iter().filter(|e| e.picker).count();
        assert_eq!(picker, 1906);
    }

    #[test]
    fn shortcode_to_unicode() {
        // Compared without U+FE0F: the table keeps emojibase's qualified forms.
        let u = |c: &str| to_unicode(c).map(bare);
        assert_eq!(u(":thumbsup:"), Some(bare("👍")));
        assert_eq!(u("+1"), Some(bare("👍")));
        assert_eq!(u(":thumbsdown:"), Some(bare("👎")));
        assert_eq!(u(":slight_smile:"), Some(bare("🙂")));
        assert_eq!(u(":slightly_smiling_face:"), Some(bare("🙂")));
        assert_eq!(u(":flag_ru:"), Some(bare("🇷🇺")));
        assert_eq!(u(":ru:"), Some(bare("🇷🇺")));
        assert_eq!(u(":thumbsup_tone3:"), Some(bare("👍🏽")));
        assert_eq!(u(":hugging:"), Some(bare("🤗")));
        assert_eq!(u(":heart:"), Some(bare("❤️")));
        // Web picker keys the gemoji tables did not know.
        assert_eq!(u(":person_shrugging:"), Some(bare("🤷")));
        assert_eq!(u(":city_sunset:"), Some(bare("🌇")));
        assert_eq!(u(":city_dusk:"), Some(bare("🌆")));
        // Legacy emojione names.
        assert_eq!(u(":digit_one:"), Some(bare("1️⃣")));
        assert_eq!(u(":tone3:"), Some(bare("🏽")));
        // Gemoji-only names the server rejects.
        assert_eq!(u(":hugs:"), None);
        assert_eq!(u(":hand:"), None);
        assert_eq!(u(":nope_not_an_emoji:"), None);
        assert_eq!(u("::"), None);
    }

    #[test]
    fn unicode_to_shortcode_uses_primary_names() {
        let cases = [
            ("👍", ":thumbsup:"),
            ("👎", ":thumbsdown:"),
            ("🙂", ":slight_smile:"),
            ("🤗", ":hugging:"),
            ("🇷🇺", ":flag_ru:"),
            ("👍🏽", ":thumbsup_tone3:"),
            ("😂", ":joy:"),
            ("✋", ":raised_hand:"),
            ("✊", ":fist:"),
            ("👊", ":punch:"),
            ("📱", ":mobile_phone:"),
            ("☎️", ":telephone:"),
            ("☎", ":telephone:"),
            ("🚗", ":red_car:"),
            ("⛵", ":sailboat:"),
            ("🔪", ":knife:"),
            ("🔵", ":blue_circle:"),
            ("🍳", ":cooking:"),
            ("🦖", ":t_rex:"),
            ("🇺🇳", ":united_nations:"),
            ("✋🏽", ":raised_hand_tone3:"),
            ("💩", ":poop:"),
            ("🤷", ":person_shrugging:"),
            ("📝", ":pencil:"),
            ("🕉️", ":om_symbol:"),
            ("🌆", ":city_dusk:"),
        ];
        for (u, k) in cases {
            assert_eq!(to_shortcode(u).as_deref(), Some(k), "{u}");
        }
        assert_eq!(to_shortcode("x"), None);
    }

    #[test]
    fn every_generated_key_round_trips() {
        for e in &DATA.entries {
            let key = to_shortcode(e.unicode).unwrap();
            assert_eq!(to_unicode(&key), Some(e.unicode), "{key}");
            for (spec, u) in &e.tones {
                let key = to_shortcode(u).unwrap();
                assert_eq!(key, format!(":{}_tone{spec}:", e.primary()));
                assert_eq!(to_unicode(&key), Some(*u), "{key}");
            }
        }
    }

    #[test]
    fn every_picker_key_is_in_the_server_list() {
        let r = EmojiRegistry::new();
        let mut n = 0;
        for cat in r.picker_categories(&[]) {
            for e in cat.emojis {
                assert!(is_known_shortcode(&e.key), "{}", e.key);
                assert_eq!(r.resolve(&e.key), Some(e.emoji.clone()), "{}", e.key);
                for tone in 1..=5 {
                    if let Some(t) = with_skin_tone(&e.key, tone) {
                        assert!(is_known_shortcode(&t), "{t}");
                    }
                }
                n += 1;
            }
        }
        assert_eq!(n, 1906);
        assert_eq!(
            with_skin_tone(":raised_hand:", 2).as_deref(),
            Some(":raised_hand_tone2:")
        );
        assert_eq!(with_skin_tone(":joy:", 2), None);
    }

    fn custom(id: &str, name: &str, aliases: &[&str]) -> CustomEmoji {
        CustomEmoji {
            id: id.into(),
            name: name.into(),
            aliases: aliases.iter().map(|a| (*a).to_owned()).collect(),
            extension: "png".into(),
            etag: Some("e1".into()),
            updated_at: None,
        }
    }

    #[test]
    fn custom_registry() {
        let mut r = EmojiRegistry::new();
        r.set_all([
            custom("1", "party_parrot", &["pp"]),
            custom("2", "smile", &[]),
        ]);
        // Custom takes precedence over Unicode.
        assert!(matches!(
            r.resolve(":smile:"),
            Some(ResolvedEmoji::Custom { .. })
        ));
        assert!(
            matches!(r.resolve(":pp:"), Some(ResolvedEmoji::Custom { name, .. }) if name == "party_parrot")
        );
        assert!(r.is_valid_reaction(":pp:"));
        assert!(r.is_valid_reaction(":joy:"));
        assert!(!r.is_valid_reaction(":hand:"));
        assert_eq!(r.resolve(":joy:"), Some(ResolvedEmoji::Unicode("😂")));
        let server = ServerUrl::parse("https://chat.example.com").unwrap();
        assert_eq!(
            r.resolve(":pp:").unwrap().custom_url(&server).as_deref(),
            Some("https://chat.example.com/emoji-custom/party_parrot.png?etag=e1")
        );
        assert!(r.remove("party_parrot"));
        assert!(r.resolve(":pp:").is_none());
        assert!(r.remove("2"));
        assert!(r.is_empty());
    }

    #[test]
    fn custom_from_fixture() {
        let v = crate::test_util::fixture("emoji_custom_list.json");
        let list: rocketchat::models::UpdatedSince<CustomEmoji> =
            serde_json::from_value(v["emojis"].clone()).unwrap();
        let mut r = EmojiRegistry::new();
        r.set_all(list.update.clone());
        assert_eq!(r.len(), list.update.len());
    }

    #[test]
    fn search_ranking() {
        let mut r = EmojiRegistry::new();
        r.set_all([custom("1", "thumbs_party", &[])]);
        let res = r.search(":thumbs", 10);
        assert!(!res.is_empty());
        assert_eq!(
            res[0].key, ":thumbs_party:",
            "custom first within prefix rank"
        );
        assert!(res.iter().any(|m| m.key == ":thumbsup:"));
        assert!(res.iter().any(|m| m.key == ":thumbsdown:"));
        // Exact beats prefix.
        let res = r.search("joy", 5);
        assert_eq!(res[0].key, ":joy:");
        // An alias matches but the key is the primary name.
        let res = r.search("hankey", 5);
        assert_eq!(res[0].key, ":poop:");
        let res = r.search("hand", 50);
        assert!(
            res.iter()
                .all(|m| is_known_shortcode(&m.key) || m.key == ":thumbs_party:")
        );
        // Substring matches come after prefix ones.
        let res = r.search("smile", 50);
        let first_contains = res.iter().position(|m| !m.matched.starts_with("smile"));
        let last_prefix = res.iter().rposition(|m| m.matched.starts_with("smile"));
        if let (Some(c), Some(p)) = (first_contains, last_prefix) {
            assert!(p < c);
        }
        // No duplicates.
        let keys: HashSet<_> = res.iter().map(|m| &m.key).collect();
        assert_eq!(keys.len(), res.len());
        assert!(r.search("", 5).is_empty());
        assert_eq!(r.search("a", 3).len(), 3);
    }

    #[test]
    fn picker() {
        let mut r = EmojiRegistry::new();
        r.set_all([custom("1", "parrot", &[])]);
        let cats = r.picker_categories(&[
            ":thumbsup:".into(),
            ":parrot:".into(),
            ":zzzz_unknown:".into(),
        ]);
        assert_eq!(cats[0].kind, CategoryKind::Recent);
        assert_eq!(cats[0].emojis.len(), 2);
        assert_eq!(cats[1].kind, CategoryKind::Custom);
        assert_eq!(cats.len(), 2 + EmojiCategory::ALL.len());
        let people = &cats[2];
        assert_eq!(people.kind, CategoryKind::Unicode(EmojiCategory::People));
        assert_eq!(people.emojis[0].emoji, ResolvedEmoji::Unicode("😀"));
        assert_eq!(people.emojis[0].key, ":grinning:");
        let flags = cats.last().unwrap();
        assert!(flags.emojis.iter().any(|e| e.key == ":flag_ru:"));
        assert!(
            flags
                .emojis
                .iter()
                .all(|e| !e.key.starts_with(":regional_indicator")),
            "regional indicators are not in the picker"
        );
    }

    #[test]
    fn recent() {
        let mut v = vec![];
        push_recent(&mut v, "joy", 3);
        push_recent(&mut v, ":a:", 3);
        push_recent(&mut v, ":joy:", 3);
        assert_eq!(v, [":joy:", ":a:"]);
    }
}
