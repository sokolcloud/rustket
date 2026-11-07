//! Offset pagination state shared by the panel lists. Every (re)load gets a
//! sequence number so results of an earlier query (filter changed, reload) are
//! dropped.

use rocketchat::models::Paginated;

use crate::error::{Failure, Res};

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum LoadState {
    #[default]
    Idle,
    /// First page in flight.
    Loading,
    /// A further page in flight.
    LoadingMore,
    Failed(String),
}

#[derive(Debug, Clone)]
pub struct Paged<T> {
    pub items: Vec<T>,
    pub total: u64,
    next_offset: u64,
    pub state: LoadState,
    seq: u64,
    /// At least one page arrived for the current query.
    pub loaded: bool,
}

impl<T> Default for Paged<T> {
    fn default() -> Self {
        Self {
            items: Vec::new(),
            total: 0,
            next_offset: 0,
            state: LoadState::Idle,
            seq: 0,
            loaded: false,
        }
    }
}

/// Identifies one page request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PageReq {
    pub seq: u64,
    pub offset: u64,
}

impl<T> Paged<T> {
    /// Starts over (new query): clears the list and returns the first request.
    pub fn reset(&mut self) -> PageReq {
        self.seq += 1;
        self.items.clear();
        self.total = 0;
        self.next_offset = 0;
        self.loaded = false;
        self.state = LoadState::Loading;
        PageReq {
            seq: self.seq,
            offset: 0,
        }
    }

    /// Drops everything without loading (e.g. an empty search query).
    pub fn clear(&mut self) {
        self.seq += 1;
        self.items.clear();
        self.total = 0;
        self.next_offset = 0;
        self.loaded = false;
        self.state = LoadState::Idle;
    }

    pub fn has_more(&self) -> bool {
        self.loaded && self.next_offset < self.total
    }

    pub fn is_busy(&self) -> bool {
        matches!(self.state, LoadState::Loading | LoadState::LoadingMore)
    }

    /// The next page, unless one is in flight or the list is complete.
    pub fn begin_more(&mut self) -> Option<PageReq> {
        if self.is_busy() || !self.has_more() {
            return None;
        }
        self.state = LoadState::LoadingMore;
        Some(PageReq {
            seq: self.seq,
            offset: self.next_offset,
        })
    }

    /// Applies a page; stale pages are ignored. Returns the failure, if any.
    pub fn finish(&mut self, req: PageReq, result: Res<Paginated<T>>) -> Option<Failure> {
        if req.seq != self.seq || req.offset != self.next_offset {
            return None;
        }
        match result {
            Ok(page) => {
                self.next_offset = req.offset + page.raw_len as u64;
                // Some endpoints omit `total`; an empty page ends the list.
                self.total = if page.raw_len == 0 {
                    self.next_offset
                } else {
                    page.total.max(self.next_offset)
                };
                self.items.extend(page.items);
                self.loaded = true;
                self.state = LoadState::Idle;
                None
            }
            Err(e) => {
                self.state = LoadState::Failed(super::common::failure_text(&e));
                Some(e)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    fn page(n: usize, offset: u64, total: u64) -> Res<Paginated<u32>> {
        Ok(Paginated {
            items: (0..n as u32).collect(),
            count: n as u64,
            offset,
            total,
            raw_len: n,
            dropped: 0,
        })
    }

    #[test]
    fn pages_and_stale_results() {
        let mut p = Paged::<u32>::default();
        let r1 = p.reset();
        assert_eq!(p.state, LoadState::Loading);
        assert!(p.begin_more().is_none(), "busy");
        p.finish(r1, page(25, 0, 30));
        assert_eq!(p.items.len(), 25);
        assert!(p.has_more());
        let r2 = p.begin_more().unwrap();
        assert_eq!(r2.offset, 25);
        // A new query while page 2 is in flight: page 2 is dropped.
        let r3 = p.reset();
        p.finish(r2, page(5, 25, 30));
        assert!(p.items.is_empty());
        p.finish(r3, page(3, 0, 3));
        assert_eq!(p.items.len(), 3);
        assert!(!p.has_more());
        assert!(p.begin_more().is_none());
    }

    #[test]
    fn empty_page_ends_list() {
        let mut p = Paged::<u32>::default();
        let r = p.reset();
        p.finish(r, page(0, 0, 10));
        assert!(!p.has_more());
        assert!(p.loaded);
    }

    #[test]
    fn failure_sets_state() {
        let mut p = Paged::<u32>::default();
        let r = p.reset();
        let e = Failure::from(rocketchat::Error::Protocol("boom".into()));
        assert!(p.finish(r, Err(e)).is_some());
        assert!(matches!(p.state, LoadState::Failed(_)));
    }
}
