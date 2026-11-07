//! Client-side checks of room and user names, following the server's rules
//! (`getValidRoomName`, `validateNameChars`): the name must match
//! `^{UTF8_Channel_Names_Validation}$` (`UTF8_User_Names_Validation` for
//! usernames), unless `UI_Allow_room_names_with_special_chars` is on, in which
//! case anything without `<>\/` goes (the server slugifies it).
//!
//! The setting is a JavaScript regex; only the usual `[class]+` form is
//! evaluated here. Other patterns are left to the server.

use rustket_core::Workspace;

pub const DEFAULT_PATTERN: &str = "[0-9a-zA-Z-_.]+";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NameError {
    Empty,
    Invalid,
}

/// Validates a new room name against the workspace settings.
pub fn check_room_name(ws: &Workspace, name: &str) -> Result<(), NameError> {
    let name = name.trim();
    if name.is_empty() {
        return Err(NameError::Empty);
    }
    if ws.settings().allow_special_chars_in_room_names() {
        return if name.contains(['<', '>', '\\', '/']) {
            Err(NameError::Invalid)
        } else {
            Ok(())
        };
    }
    let pattern = ws
        .settings()
        .str("UTF8_Channel_Names_Validation")
        .unwrap_or(DEFAULT_PATTERN);
    match matches_pattern(pattern, name) {
        Some(false) => Err(NameError::Invalid),
        _ => Ok(()),
    }
}

/// Validates a username against `UTF8_User_Names_Validation`.
pub fn check_username(ws: &Workspace, name: &str) -> Result<(), NameError> {
    if name.is_empty() {
        return Err(NameError::Empty);
    }
    let pattern = ws
        .settings()
        .str("UTF8_User_Names_Validation")
        .unwrap_or(DEFAULT_PATTERN);
    match matches_pattern(pattern, name) {
        Some(false) => Err(NameError::Invalid),
        _ => Ok(()),
    }
}

/// Whether `s` fully matches a `[class]+` (or `[class]*`) pattern; `None` when
/// the pattern has another form.
pub fn matches_pattern(pattern: &str, s: &str) -> Option<bool> {
    let p = pattern.trim();
    let p = p.strip_prefix('^').unwrap_or(p);
    let p = p.strip_suffix('$').unwrap_or(p);
    let (body, min) = match p.strip_suffix('+') {
        Some(b) => (b, 1),
        None => (p.strip_suffix('*')?, 0),
    };
    let class = body.strip_prefix('[')?.strip_suffix(']')?;
    let set = CharClass::parse(class)?;
    Some(s.chars().count() >= min && s.chars().all(|c| set.contains(c)))
}

#[derive(Debug, Default)]
struct CharClass {
    negated: bool,
    ranges: Vec<(char, char)>,
    word: bool,
    digit: bool,
    space: bool,
}

impl CharClass {
    fn parse(class: &str) -> Option<Self> {
        let mut out = Self::default();
        let mut chars: Vec<char> = class.chars().collect();
        if chars.first() == Some(&'^') {
            out.negated = true;
            chars.remove(0);
        }
        let mut items: Vec<char> = Vec::new();
        let mut i = 0;
        while i < chars.len() {
            let c = chars[i];
            if c == '\\' {
                let e = *chars.get(i + 1)?;
                i += 2;
                match e {
                    'w' => out.word = true,
                    'd' => out.digit = true,
                    's' => out.space = true,
                    'p' | 'P' | 'u' | 'x' => return None,
                    other => items.push(other),
                }
                continue;
            }
            // `a-z` range (a `-` at either end is literal).
            if i + 2 < chars.len() && chars[i + 1] == '-' && chars[i + 2] != '\\' {
                let (a, b) = (c, chars[i + 2]);
                if a > b {
                    return None;
                }
                out.ranges.push((a, b));
                i += 3;
                continue;
            }
            items.push(c);
            i += 1;
        }
        out.ranges.extend(items.into_iter().map(|c| (c, c)));
        Some(out)
    }

    fn contains(&self, c: char) -> bool {
        let hit = self.ranges.iter().any(|&(a, b)| a <= c && c <= b)
            || (self.word && (c.is_ascii_alphanumeric() || c == '_'))
            || (self.digit && c.is_ascii_digit())
            || (self.space && c.is_whitespace());
        hit != self.negated
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rocketchat::models::Setting;
    use serde_json::json;

    #[test]
    fn default_pattern() {
        assert_eq!(
            matches_pattern(DEFAULT_PATTERN, "dev-public_1.x"),
            Some(true)
        );
        assert_eq!(matches_pattern(DEFAULT_PATTERN, "bad name!"), Some(false));
        assert_eq!(matches_pattern(DEFAULT_PATTERN, ""), Some(false));
        assert_eq!(matches_pattern(DEFAULT_PATTERN, "кириллица"), Some(false));
        assert_eq!(matches_pattern("^[a-z]*$", ""), Some(true));
        assert_eq!(matches_pattern("[\\w-]+", "a_b-1"), Some(true));
        assert_eq!(matches_pattern("[^ ]+", "a b"), Some(false));
        assert_eq!(matches_pattern("[\\p{L}]+", "x"), None);
        assert_eq!(matches_pattern("(a|b)+", "a"), None);
    }

    fn ws_with(settings: &[(&str, serde_json::Value)]) -> Workspace {
        let mut ws = Workspace::new();
        for (id, v) in settings {
            ws.apply_setting(
                rustket_core::workspace::Change::Inserted,
                Setting {
                    id: (*id).to_owned(),
                    value: v.clone(),
                    ..Default::default()
                },
            );
        }
        ws
    }

    #[test]
    fn room_names_follow_settings() {
        let ws = ws_with(&[]);
        assert_eq!(check_room_name(&ws, "  "), Err(NameError::Empty));
        assert_eq!(check_room_name(&ws, "general"), Ok(()));
        assert_eq!(check_room_name(&ws, "Team room"), Err(NameError::Invalid));
        let ws = ws_with(&[("UI_Allow_room_names_with_special_chars", json!(true))]);
        assert_eq!(check_room_name(&ws, "Team room"), Ok(()));
        assert_eq!(check_room_name(&ws, "a/b"), Err(NameError::Invalid));
        let ws = ws_with(&[("UTF8_Channel_Names_Validation", json!("[a-z]+"))]);
        assert_eq!(check_room_name(&ws, "Abc"), Err(NameError::Invalid));
        assert_eq!(check_username(&ws, "Abc"), Ok(()));
        assert_eq!(check_username(&ws, "a b"), Err(NameError::Invalid));
    }
}
