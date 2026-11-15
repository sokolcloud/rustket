//! Keyboard chords: one table for the app-wide shortcuts, the composer's
//! formatting chords and the cheat sheet (Ctrl/Cmd+/).
//!
//! App-wide chords reach the app through `iced::event::listen_with`, at any
//! capture status: a focused composer swallows them (`Binding::Sequence`
//! with nothing in it) so they neither insert text nor trigger an editor
//! binding, and the app listener still acts on them. Tab, F6 and Escape stay
//! uncaptured-only (`app/subscription.rs`), Alt(+Shift)+Up/Down live in
//! `desktop::shortcuts`; the table lists them for the cheat sheet.
//!
//! Chords use `Modifiers::command()`: Ctrl on Linux and Windows, Cmd on
//! macOS. Punctuation and digits also match by physical key, so they work on
//! layouts where the key types another character (`/` is `.` on a Russian
//! layout, `9` is `ç` on AZERTY).

use iced::keyboard::key::{Code, Named, Physical};
use iced::keyboard::{self, Key, Modifiers};

use crate::room::composer::text::Format;

/// An app-wide action bound to a chord.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Shortcut {
    /// Ctrl/Cmd+K.
    QuickSwitcher,
    /// Ctrl/Cmd+/: the cheat sheet.
    CheatSheet,
    /// Ctrl/Cmd+,.
    Settings,
    /// Shift+Esc: mark every room of the active server read (asks first).
    MarkAllRead,
    /// Ctrl/Cmd+Shift+\: react to the newest message of the focused
    /// composer's timeline (the room's when no composer has the focus).
    ReactLast,
    /// Ctrl/Cmd+Shift+A, reserved for "All unreads".
    AllUnreads,
    /// Ctrl/Cmd+Shift+M, reserved for "Activity".
    Activity,
    /// Ctrl/Cmd+Shift+S, reserved for "Later".
    Later,
}

impl Shortcut {
    /// Whether the action exists yet. Reserved chords of features still to
    /// come are not matched (the key press passes through untouched) and
    /// are left out of the cheat sheet.
    pub const fn is_available(self) -> bool {
        !matches!(self, Self::AllUnreads | Self::Activity | Self::Later)
    }
}

/// `key` is the character `c`, or (punctuation and digits) the physical key
/// is where `c` sits on a US layout and types no letter there (Dvorak's `z`
/// on that `/` key stays Ctrl+Z).
fn is_char(key: &Key, physical: Physical, c: char) -> bool {
    let typed = key.to_latin(physical).map(|k| k.to_ascii_lowercase());
    if typed == Some(c) {
        return true;
    }
    let code = match c {
        '/' => Code::Slash,
        '\\' => Code::Backslash,
        ',' => Code::Comma,
        '9' => Code::Digit9,
        _ => return false,
    };
    physical == Physical::Code(code) && !typed.is_some_and(|k| k.is_ascii_alphabetic())
}

/// The app-wide shortcut of a key press, if any (only available ones).
pub fn global(key: &Key, physical: Physical, m: Modifiers) -> Option<Shortcut> {
    let s = if *key == Key::Named(Named::Escape) {
        if !m.shift() || m.command() || m.alt() || m.control() {
            return None;
        }
        Shortcut::MarkAllRead
    } else {
        if !m.command() || m.alt() {
            return None;
        }
        let is = |c| is_char(key, physical, c);
        match m.shift() {
            false if is('k') => Shortcut::QuickSwitcher,
            false if is(',') => Shortcut::Settings,
            // Some layouts need Shift for `/`.
            _ if is('/') => Shortcut::CheatSheet,
            true if is('\\') => Shortcut::ReactLast,
            true if is('a') => Shortcut::AllUnreads,
            true if is('m') => Shortcut::Activity,
            true if is('s') => Shortcut::Later,
            _ => return None,
        }
    };
    s.is_available().then_some(s)
}

/// The composer's formatting chord of a key press, if any. `text`: the
/// text the press produced; printable text together with Ctrl+Alt is AltGr
/// on Windows (e.g. Polish AltGr+Shift+C types `Ć`), which is not a chord.
/// On macOS Cmd+Option is never AltGr, and the text there carries the
/// Option character (Cmd+Option+Shift+C gives `Ç`), so it is not checked.
pub fn format(key: &Key, physical: Physical, m: Modifiers, text: Option<&str>) -> Option<Format> {
    format_on(key, physical, m, text, cfg!(target_os = "macos"))
}

fn format_on(
    key: &Key,
    physical: Physical,
    m: Modifiers,
    text: Option<&str>,
    mac: bool,
) -> Option<Format> {
    if !m.command() {
        return None;
    }
    let is = |c| is_char(key, physical, c);
    if m.alt() {
        let altgr = !mac && text.is_some_and(|t| t.chars().any(|c| !c.is_control()));
        return (m.shift() && is('c') && !altgr).then_some(Format::CodeBlock);
    }
    Some(match m.shift() {
        false if is('b') => Format::Bold,
        false if is('i') => Format::Italic,
        true if is('x') => Format::Strike,
        true if is('c') => Format::Code,
        true if is('u') => Format::Link,
        true if is('9') => Format::Quote,
        _ => return None,
    })
}

/// The app listener (`listen_with`): app-wide chords at any capture status,
/// except two that only count when no widget took the key:
/// - "react to the last message": a focused composer handles it itself for
///   its own timeline;
/// - Shift+Esc: a dialog, menu or field that takes Escape (whatever the
///   modifiers) closes first, and the listener only hears of the key after
///   that, so it could not tell the dialog was there. A focused composer
///   asks for the dialog itself (`composer::Message::AskReadAll`).
///
/// Key repeats are ignored.
pub fn from_event(ev: &iced::Event, status: iced::event::Status) -> Option<Shortcut> {
    let iced::Event::Keyboard(keyboard::Event::KeyPressed {
        key,
        physical_key,
        modifiers,
        repeat: false,
        ..
    }) = ev
    else {
        return None;
    };
    match global(key, *physical_key, *modifiers)? {
        Shortcut::ReactLast | Shortcut::MarkAllRead if status == iced::event::Status::Captured => {
            None
        }
        s => Some(s),
    }
}

// --- catalog (cheat sheet, tooltips) ---------------------------------------------

/// One key of a chord as printed on the key cap.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Token {
    /// Ctrl, or Cmd on macOS.
    Cmd,
    /// Alt, or Option on macOS.
    Alt,
    Shift,
    Char(char),
    Esc,
    Enter,
    Up,
    Down,
    Tab,
    F6,
}

/// What a catalog row is bound to (for the consistency test).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Bound {
    Global(Shortcut),
    Format(Format),
    /// Handled elsewhere (focus traversal, room steps, the composer).
    Other,
}

/// A cheat sheet row: label (i18n key) and its chords (alternatives).
#[derive(Debug, Clone, Copy)]
pub struct Entry {
    pub label: &'static str,
    pub chords: &'static [&'static [Token]],
    pub bound: Bound,
}

impl Entry {
    /// Shown in the cheat sheet (the feature exists).
    pub fn is_available(&self) -> bool {
        match self.bound {
            Bound::Global(s) => s.is_available(),
            _ => true,
        }
    }
}

/// A cheat sheet group: heading (i18n key) and rows.
#[derive(Debug, Clone, Copy)]
pub struct Group {
    pub label: &'static str,
    pub entries: &'static [Entry],
}

use Token::{Alt, Cmd, Shift};

/// Label key of the Esc row (it depends on `UiPrefs::escape_marks_read`).
pub const ESCAPE_LABEL: &str = "shortcuts.escape";
/// The Esc row's label while Esc does not mark rooms read.
pub const ESCAPE_CLOSE_LABEL: &str = "shortcuts.escape_close";

/// Every chord. Reserved rows (`Shortcut::is_available` false) are hidden;
/// their label keys come with their features.
pub const GROUPS: &[Group] = &[
    Group {
        label: "shortcuts.group_navigation",
        entries: &[
            Entry {
                label: "shortcuts.quick_switcher",
                chords: &[&[Cmd, Token::Char('k')]],
                bound: Bound::Global(Shortcut::QuickSwitcher),
            },
            Entry {
                label: "shortcuts.room_step",
                chords: &[&[Alt, Token::Up], &[Alt, Token::Down]],
                bound: Bound::Other,
            },
            Entry {
                label: "shortcuts.unread_step",
                chords: &[&[Alt, Shift, Token::Up], &[Alt, Shift, Token::Down]],
                bound: Bound::Other,
            },
            Entry {
                label: "shortcuts.all_unreads",
                chords: &[&[Cmd, Shift, Token::Char('a')]],
                bound: Bound::Global(Shortcut::AllUnreads),
            },
            Entry {
                label: "shortcuts.activity",
                chords: &[&[Cmd, Shift, Token::Char('m')]],
                bound: Bound::Global(Shortcut::Activity),
            },
            Entry {
                label: "shortcuts.later",
                chords: &[&[Cmd, Shift, Token::Char('s')]],
                bound: Bound::Global(Shortcut::Later),
            },
            Entry {
                label: "shortcuts.regions",
                chords: &[&[Token::F6], &[Shift, Token::F6]],
                bound: Bound::Other,
            },
            Entry {
                label: "shortcuts.focus_next",
                chords: &[&[Token::Tab], &[Shift, Token::Tab]],
                bound: Bound::Other,
            },
            Entry {
                label: "shortcuts.settings",
                chords: &[&[Cmd, Token::Char(',')]],
                bound: Bound::Global(Shortcut::Settings),
            },
            Entry {
                label: "shortcuts.cheat_sheet",
                chords: &[&[Cmd, Token::Char('/')]],
                bound: Bound::Global(Shortcut::CheatSheet),
            },
        ],
    },
    Group {
        label: "shortcuts.group_messages",
        entries: &[
            Entry {
                label: ESCAPE_LABEL,
                chords: &[&[Token::Esc]],
                bound: Bound::Other,
            },
            Entry {
                label: "shortcuts.mark_all_read",
                chords: &[&[Shift, Token::Esc]],
                bound: Bound::Global(Shortcut::MarkAllRead),
            },
            Entry {
                label: "shortcuts.react_last",
                chords: &[&[Cmd, Shift, Token::Char('\\')]],
                bound: Bound::Global(Shortcut::ReactLast),
            },
        ],
    },
    Group {
        label: "shortcuts.group_composer",
        entries: &[
            Entry {
                label: "room_composer.send",
                chords: &[&[Token::Enter]],
                bound: Bound::Other,
            },
            Entry {
                label: "shortcuts.new_line",
                chords: &[&[Shift, Token::Enter]],
                bound: Bound::Other,
            },
            Entry {
                label: "shortcuts.edit_last",
                chords: &[&[Token::Up]],
                bound: Bound::Other,
            },
            Entry {
                label: "room_composer.format.bold",
                chords: &[&[Cmd, Token::Char('b')]],
                bound: Bound::Format(Format::Bold),
            },
            Entry {
                label: "room_composer.format.italic",
                chords: &[&[Cmd, Token::Char('i')]],
                bound: Bound::Format(Format::Italic),
            },
            Entry {
                label: "room_composer.format.strike",
                chords: &[&[Cmd, Shift, Token::Char('x')]],
                bound: Bound::Format(Format::Strike),
            },
            Entry {
                label: "room_composer.format.code",
                chords: &[&[Cmd, Shift, Token::Char('c')]],
                bound: Bound::Format(Format::Code),
            },
            Entry {
                label: "room_composer.format.code_block",
                chords: &[&[Cmd, Alt, Shift, Token::Char('c')]],
                bound: Bound::Format(Format::CodeBlock),
            },
            Entry {
                label: "room_composer.format.link",
                chords: &[&[Cmd, Shift, Token::Char('u')]],
                bound: Bound::Format(Format::Link),
            },
            Entry {
                label: "room_composer.format.quote",
                chords: &[&[Cmd, Shift, Token::Char('9')]],
                bound: Bound::Format(Format::Quote),
            },
        ],
    },
];

/// A chord as printed: "Ctrl+Shift+\" (Linux, Windows) or "⌘⇧\" (macOS).
/// Key names are not translated: they are what the keys show.
pub fn display(chord: &[Token]) -> String {
    display_for(chord, cfg!(target_os = "macos"))
}

fn display_for(chord: &[Token], mac: bool) -> String {
    let name = |t: &Token| -> String {
        match (t, mac) {
            (Token::Cmd, true) => "⌘".into(),
            (Token::Cmd, false) => "Ctrl".into(),
            (Token::Alt, true) => "⌥".into(),
            (Token::Alt, false) => "Alt".into(),
            (Token::Shift, true) => "⇧".into(),
            (Token::Shift, false) => "Shift".into(),
            (Token::Char(c), _) => c.to_ascii_uppercase().to_string(),
            (Token::Esc, _) => "Esc".into(),
            (Token::Enter, _) => "Enter".into(),
            (Token::Up, _) => "↑".into(),
            (Token::Down, _) => "↓".into(),
            (Token::Tab, _) => "Tab".into(),
            (Token::F6, _) => "F6".into(),
        }
    };
    let parts: Vec<String> = chord.iter().map(name).collect();
    parts.join(if mac { "" } else { "+" })
}

/// The chord of a formatting action, as printed (composer tooltips).
pub fn format_chord(f: Format) -> Option<String> {
    GROUPS
        .iter()
        .flat_map(|g| g.entries)
        .find(|e| e.bound == Bound::Format(f))
        .and_then(|e| e.chords.first())
        .map(|c| display(c))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use iced::keyboard::key::NativeCode;

    const NONE: Physical = Physical::Unidentified(NativeCode::Unidentified);

    fn ch(s: &str) -> Key {
        Key::Character(s.into())
    }

    fn code_of(c: char) -> Physical {
        let code = match c {
            'a' => Code::KeyA,
            'b' => Code::KeyB,
            'c' => Code::KeyC,
            'i' => Code::KeyI,
            'k' => Code::KeyK,
            'm' => Code::KeyM,
            's' => Code::KeyS,
            'u' => Code::KeyU,
            'x' => Code::KeyX,
            '9' => Code::Digit9,
            '/' => Code::Slash,
            '\\' => Code::Backslash,
            ',' => Code::Comma,
            _ => return NONE,
        };
        Physical::Code(code)
    }

    /// The key press a catalog chord stands for.
    fn press(chord: &[Token]) -> Option<(Key, Physical, Modifiers)> {
        let mut m = Modifiers::empty();
        let mut key = None;
        for t in chord {
            match t {
                Token::Cmd => m |= Modifiers::COMMAND,
                Token::Alt => m |= Modifiers::ALT,
                Token::Shift => m |= Modifiers::SHIFT,
                Token::Char(c) => key = Some((ch(&c.to_string()), code_of(*c))),
                Token::Esc => key = Some((Key::Named(Named::Escape), NONE)),
                // Not app-wide chords.
                _ => return None,
            }
        }
        key.map(|(k, p)| (k, p, m))
    }

    #[test]
    fn app_wide_chords() {
        let cmd = Modifiers::COMMAND;
        let cs = Modifiers::COMMAND | Modifiers::SHIFT;
        assert_eq!(global(&ch("k"), NONE, cmd), Some(Shortcut::QuickSwitcher));
        assert_eq!(global(&ch("k"), NONE, cs), None);
        assert_eq!(global(&ch("/"), NONE, cmd), Some(Shortcut::CheatSheet));
        // Russian layout: the `/` key types `.`.
        let slash = Physical::Code(Code::Slash);
        assert_eq!(global(&ch("."), slash, cmd), Some(Shortcut::CheatSheet));
        assert_eq!(global(&ch("."), NONE, cmd), None);
        // Dvorak types `z` on that key: Ctrl+Z is no shortcut.
        assert_eq!(global(&ch("z"), slash, cmd), None);
        assert_eq!(global(&ch(","), NONE, cmd), Some(Shortcut::Settings));
        let backslash = Physical::Code(Code::Backslash);
        assert_eq!(global(&ch("\\"), NONE, cs), Some(Shortcut::ReactLast));
        assert_eq!(global(&ch("ё"), backslash, cs), Some(Shortcut::ReactLast));
        assert_eq!(global(&ch("\\"), NONE, cmd), None, "needs Shift");
        let esc = Key::Named(Named::Escape);
        assert_eq!(
            global(&esc, NONE, Modifiers::SHIFT),
            Some(Shortcut::MarkAllRead)
        );
        assert_eq!(global(&esc, NONE, Modifiers::empty()), None);
        assert_eq!(global(&esc, NONE, cs), None);
        // Reserved chords pass through until their features land.
        for c in ["a", "m", "s"] {
            assert_eq!(global(&ch(c), NONE, cs), None, "{c}");
        }
        // Alt (AltGr) is never part of an app-wide chord.
        assert_eq!(global(&ch("k"), NONE, cmd | Modifiers::ALT), None);
    }

    #[test]
    fn formatting_chords() {
        let cmd = Modifiers::COMMAND;
        let cs = cmd | Modifiers::SHIFT;
        let cas = cs | Modifiers::ALT;
        assert_eq!(format(&ch("b"), NONE, cmd, None), Some(Format::Bold));
        assert_eq!(format(&ch("i"), NONE, cmd, None), Some(Format::Italic));
        assert_eq!(format(&ch("x"), NONE, cs, None), Some(Format::Strike));
        assert_eq!(format(&ch("c"), NONE, cs, None), Some(Format::Code));
        assert_eq!(format(&ch("u"), NONE, cs, None), Some(Format::Link));
        assert_eq!(format(&ch("9"), NONE, cs, None), Some(Format::Quote));
        // AZERTY: the 9 key types `ç`.
        let nine = Physical::Code(Code::Digit9);
        assert_eq!(format(&ch("ç"), nine, cs, None), Some(Format::Quote));
        // Russian layout: Cyrillic `с` on the C key.
        let c = Physical::Code(Code::KeyC);
        assert_eq!(format(&ch("с"), c, cs, None), Some(Format::Code));
        assert_eq!(format(&ch("c"), NONE, cas, None), Some(Format::CodeBlock));
        assert_eq!(
            format_on(&ch("c"), NONE, cas, Some("Ć"), false),
            None,
            "AltGr"
        );
        // macOS: Cmd+Option+Shift+C reports the Option character.
        assert_eq!(
            format_on(&ch("c"), NONE, cas, Some("Ç"), true),
            Some(Format::CodeBlock)
        );
        assert_eq!(format(&ch("c"), NONE, cmd, None), None, "plain copy");
        assert_eq!(format(&ch("b"), NONE, Modifiers::SHIFT, None), None);
    }

    #[test]
    fn listener_skips_keys_a_widget_took() {
        use iced::event::Status;
        let ev = |key: Key, physical, modifiers, repeat| {
            iced::Event::Keyboard(keyboard::Event::KeyPressed {
                modified_key: key.clone(),
                key,
                physical_key: physical,
                location: keyboard::Location::Standard,
                modifiers,
                text: None,
                repeat,
            })
        };
        let cs = Modifiers::COMMAND | Modifiers::SHIFT;
        let react = ev(ch("\\"), NONE, cs, false);
        assert_eq!(
            from_event(&react, Status::Ignored),
            Some(Shortcut::ReactLast)
        );
        assert_eq!(from_event(&react, Status::Captured), None);
        let sheet = ev(ch("/"), NONE, Modifiers::COMMAND, false);
        assert_eq!(
            from_event(&sheet, Status::Captured),
            Some(Shortcut::CheatSheet)
        );
        // A dialog that took Escape closes; no "Mark all as read?" after it.
        let shift_esc = ev(Key::Named(Named::Escape), NONE, Modifiers::SHIFT, false);
        assert_eq!(from_event(&shift_esc, Status::Captured), None);
        assert_eq!(
            from_event(&shift_esc, Status::Ignored),
            Some(Shortcut::MarkAllRead)
        );
        let repeated = ev(ch("/"), NONE, Modifiers::COMMAND, true);
        assert_eq!(from_event(&repeated, Status::Ignored), None);
    }

    #[test]
    fn catalog_matches_the_bindings() {
        let mut seen: Vec<String> = Vec::new();
        let mut reserved = Vec::new();
        for e in GROUPS.iter().flat_map(|g| g.entries) {
            for chord in e.chords {
                let shown = display_for(chord, false);
                assert!(!seen.contains(&shown), "{shown} bound twice");
                seen.push(shown.clone());
                match e.bound {
                    Bound::Global(s) => {
                        let (k, p, m) = press(chord).unwrap();
                        let want = s.is_available().then_some(s);
                        assert_eq!(global(&k, p, m), want, "{shown}");
                        if !s.is_available() {
                            reserved.push(shown);
                        }
                    }
                    Bound::Format(f) => {
                        let (k, p, m) = press(chord).unwrap();
                        assert_eq!(format(&k, p, m, None), Some(f), "{shown}");
                        assert_eq!(global(&k, p, m), None, "{shown}");
                    }
                    Bound::Other => {}
                }
            }
        }
        assert_eq!(reserved, ["Ctrl+Shift+A", "Ctrl+Shift+M", "Ctrl+Shift+S"]);
        for f in Format::ALL {
            assert!(format_chord(f).is_some(), "{f:?}");
        }
    }

    #[test]
    fn labels_are_translated() {
        let mut keys: Vec<&str> = vec![ESCAPE_CLOSE_LABEL];
        for g in GROUPS {
            keys.push(g.label);
            keys.extend(
                g.entries
                    .iter()
                    .filter(|e| e.is_available())
                    .map(|e| e.label),
            );
        }
        for loc in crate::i18n::tests::BUNDLED_COMPLETE {
            for k in &keys {
                assert_ne!(rust_i18n::t!(*k, locale = *loc), *k, "{k} ({loc})");
            }
        }
    }

    #[test]
    fn chords_print_per_platform() {
        let chord = [Cmd, Shift, Token::Char('\\')];
        assert_eq!(display_for(&chord, false), "Ctrl+Shift+\\");
        assert_eq!(display_for(&chord, true), "⌘⇧\\");
        assert_eq!(display_for(&[Alt, Token::Up], false), "Alt+↑");
        assert_eq!(display_for(&[Shift, Token::Esc], true), "⇧Esc");
    }
}
