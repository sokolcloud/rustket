//! Keyboard shortcuts for room navigation: Alt+Up/Down = previous/next room
//! in sidebar order, Alt+Shift+Up/Down = previous/next unread room.

use iced::keyboard::{self, Key, Modifiers, key::Named};

/// A room navigation step.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RoomStep {
    /// `-1` previous, `1` next.
    pub delta: i8,
    pub unread_only: bool,
}

/// The step bound to a key press, if any.
pub fn room_step(key: &Key, modifiers: Modifiers) -> Option<RoomStep> {
    if !modifiers.alt() || modifiers.control() || modifiers.logo() {
        return None;
    }
    let delta = match key {
        Key::Named(Named::ArrowUp) => -1,
        Key::Named(Named::ArrowDown) => 1,
        _ => return None,
    };
    Some(RoomStep {
        delta,
        unread_only: modifiers.shift(),
    })
}

/// Maps a keyboard event to a room step.
pub fn from_event(ev: &keyboard::Event) -> Option<RoomStep> {
    match ev {
        keyboard::Event::KeyPressed { key, modifiers, .. } => room_step(key, *modifiers),
        _ => None,
    }
}

/// The room to open: `rows` are `(rid, unread)` in sidebar order, `current`
/// the open room. Wraps around; with `unread_only`, skips read rooms (and
/// stays put when none is unread).
pub fn target<'a>(
    rows: &[(&'a str, bool)],
    current: Option<&str>,
    step: RoomStep,
) -> Option<&'a str> {
    if rows.is_empty() {
        return None;
    }
    let n = rows.len() as isize;
    let start = current.and_then(|c| rows.iter().position(|(r, _)| *r == c));
    let (mut i, mut left) = match start {
        Some(i) => (i as isize, n - 1),
        // Nothing open: Down starts at the first row, Up at the last.
        None => (if step.delta > 0 { -1 } else { n }, n),
    };
    while left > 0 {
        i = (i + isize::from(step.delta)).rem_euclid(n);
        left -= 1;
        let (rid, unread) = rows[i as usize];
        if !step.unread_only || unread {
            return Some(rid);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    const NEXT: RoomStep = RoomStep {
        delta: 1,
        unread_only: false,
    };
    const PREV: RoomStep = RoomStep {
        delta: -1,
        unread_only: false,
    };
    const NEXT_UNREAD: RoomStep = RoomStep {
        delta: 1,
        unread_only: true,
    };
    const PREV_UNREAD: RoomStep = RoomStep {
        delta: -1,
        unread_only: true,
    };

    #[test]
    fn keys() {
        let up = Key::Named(Named::ArrowUp);
        let down = Key::Named(Named::ArrowDown);
        assert_eq!(room_step(&down, Modifiers::ALT), Some(NEXT));
        assert_eq!(room_step(&up, Modifiers::ALT), Some(PREV));
        assert_eq!(
            room_step(&up, Modifiers::ALT | Modifiers::SHIFT),
            Some(PREV_UNREAD)
        );
        assert_eq!(room_step(&up, Modifiers::empty()), None);
        assert_eq!(room_step(&up, Modifiers::ALT | Modifiers::CTRL), None);
        assert_eq!(room_step(&Key::Character("k".into()), Modifiers::ALT), None);
    }

    #[test]
    fn steps_wrap_and_skip_read_rooms() {
        let rows = [("a", false), ("b", true), ("c", false), ("d", true)];
        assert_eq!(target(&rows, Some("a"), NEXT), Some("b"));
        assert_eq!(target(&rows, Some("a"), PREV), Some("d"));
        assert_eq!(target(&rows, Some("d"), NEXT), Some("a"));
        assert_eq!(target(&rows, None, NEXT), Some("a"));
        assert_eq!(target(&rows, None, PREV), Some("d"));
        assert_eq!(target(&rows, Some("b"), NEXT_UNREAD), Some("d"));
        assert_eq!(target(&rows, Some("b"), PREV_UNREAD), Some("d"));
        assert_eq!(target(&rows, Some("d"), NEXT_UNREAD), Some("b"));
        assert_eq!(target(&rows, None, NEXT_UNREAD), Some("b"));
        // The open room is not in the list (filtered out): start from the edge.
        assert_eq!(target(&rows, Some("zz"), PREV_UNREAD), Some("d"));
        let read = [("a", false), ("b", false)];
        assert_eq!(target(&read, Some("a"), NEXT_UNREAD), None);
        // Only the current room is unread: stay.
        let one = [("a", true), ("b", false)];
        assert_eq!(target(&one, Some("a"), NEXT_UNREAD), None);
        assert_eq!(target(&[], None, NEXT), None);
    }
}
