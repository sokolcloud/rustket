//! "Catch up": the rooms of "All unreads" one at a time. The order is fixed
//! when it starts; rooms read meanwhile (here or elsewhere) drop out, rooms
//! that became unread are appended.

use std::collections::BTreeSet;

use super::UnreadRoom;

/// How the current room is left.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Advance {
    /// Marked read.
    Read,
    /// Skipped (offered again by [`CatchUp::review_skipped`]).
    Skip,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CatchUp {
    order: Vec<String>,
    pos: usize,
    skipped: BTreeSet<String>,
}

impl CatchUp {
    pub fn start(rooms: &[UnreadRoom]) -> Self {
        Self {
            order: rooms.iter().map(|r| r.rid.clone()).collect(),
            pos: 0,
            skipped: BTreeSet::new(),
        }
    }

    /// The room shown; `None` at the end.
    pub fn current(&self) -> Option<&str> {
        self.order.get(self.pos).map(String::as_str)
    }

    pub fn is_done(&self) -> bool {
        self.pos >= self.order.len()
    }

    pub fn advance(&mut self, how: Advance) {
        let Some(rid) = self.order.get(self.pos) else {
            return;
        };
        match how {
            Advance::Read => {
                self.skipped.remove(rid);
            }
            Advance::Skip => {
                self.skipped.insert(rid.clone());
            }
        }
        self.pos += 1;
    }

    pub fn back(&mut self) {
        self.pos = self.pos.saturating_sub(1).min(self.order.len());
    }

    /// Follows the current unread rooms: rooms no longer listed drop out
    /// (the position stays on the same room, or the next one when the
    /// current one went), new ones are appended.
    pub fn refresh(&mut self, rooms: &[UnreadRoom]) {
        let listed: BTreeSet<&str> = rooms.iter().map(|r| r.rid.as_str()).collect();
        let before = self.order[..self.pos.min(self.order.len())]
            .iter()
            .filter(|r| !listed.contains(r.as_str()))
            .count();
        self.order.retain(|r| listed.contains(r.as_str()));
        self.pos = self.pos.saturating_sub(before);
        self.skipped.retain(|r| listed.contains(r.as_str()));
        let known: BTreeSet<String> = self.order.iter().cloned().collect();
        for r in rooms {
            if !known.contains(&r.rid) {
                self.order.push(r.rid.clone());
            }
        }
    }

    /// `(position, total)`, 1-based ("3 of 12"); the position is
    /// `total + 1` at the end.
    pub fn progress(&self) -> (usize, usize) {
        (self.pos + 1, self.order.len())
    }

    pub fn skipped_count(&self) -> usize {
        self.skipped.len()
    }

    /// Starts over with the skipped rooms.
    pub fn review_skipped(&mut self) {
        let skipped = std::mem::take(&mut self.skipped);
        self.order.retain(|r| skipped.contains(r));
        self.pos = 0;
    }

    /// The current room and the next `n - 1` (prefetched).
    pub fn upcoming(&self, n: usize) -> impl Iterator<Item = &str> + '_ {
        self.order.iter().skip(self.pos).take(n).map(String::as_str)
    }
}
