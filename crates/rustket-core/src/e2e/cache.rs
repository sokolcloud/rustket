//! Decrypted message contents keyed by `(room id, message id, _updatedAt)`: an edit
//! changes `_updatedAt` and so misses the cache. Only successful decryptions are
//! cached; messages that could not be decrypted yet are remembered per room so a
//! key arrival can name them.

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;

use super::DecryptedContent;

/// Default number of cached entries.
pub const DEFAULT_CAPACITY: usize = 5000;
/// Pending ids remembered per room.
const MAX_PENDING_PER_ROOM: usize = 1000;

type Key = (String, String, Option<i64>);

#[derive(Debug)]
pub struct DecryptCache {
    capacity: usize,
    entries: HashMap<Key, Arc<DecryptedContent>>,
    order: VecDeque<Key>,
    pending: HashMap<String, HashSet<String>>,
}

impl Default for DecryptCache {
    fn default() -> Self {
        Self::new(DEFAULT_CAPACITY)
    }
}

impl DecryptCache {
    pub fn new(capacity: usize) -> Self {
        Self {
            capacity: capacity.max(1),
            entries: HashMap::new(),
            order: VecDeque::new(),
            pending: HashMap::new(),
        }
    }

    pub fn get(
        &self,
        rid: &str,
        id: &str,
        updated_at: Option<i64>,
    ) -> Option<Arc<DecryptedContent>> {
        self.entries
            .get(&(rid.to_owned(), id.to_owned(), updated_at))
            .cloned()
    }

    pub fn insert(
        &mut self,
        rid: &str,
        id: &str,
        updated_at: Option<i64>,
        value: Arc<DecryptedContent>,
    ) {
        if let Some(p) = self.pending.get_mut(rid) {
            p.remove(id);
        }
        let key = (rid.to_owned(), id.to_owned(), updated_at);
        if self.entries.insert(key.clone(), value).is_none() {
            self.order.push_back(key);
        }
        while self.entries.len() > self.capacity {
            match self.order.pop_front() {
                Some(old) => {
                    self.entries.remove(&old);
                }
                None => break,
            }
        }
    }

    pub fn mark_pending(&mut self, rid: &str, id: &str) {
        let set = self.pending.entry(rid.to_owned()).or_default();
        if set.len() < MAX_PENDING_PER_ROOM {
            set.insert(id.to_owned());
        }
    }

    /// Takes the ids that were pending in `rid`.
    pub fn take_pending(&mut self, rid: &str) -> Vec<String> {
        let mut v: Vec<String> = self
            .pending
            .remove(rid)
            .unwrap_or_default()
            .into_iter()
            .collect();
        v.sort();
        v
    }

    pub fn clear(&mut self) {
        self.entries.clear();
        self.order.clear();
        self.pending.clear();
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn content(text: &str) -> Arc<DecryptedContent> {
        Arc::new(DecryptedContent {
            message: rocketchat::e2e::DecryptedMessage {
                msg: Some(text.into()),
                ..Default::default()
            },
            kid: None,
            plaintext_edit: false,
        })
    }

    #[test]
    fn keyed_by_id_and_updated_at_with_eviction() {
        let mut c = DecryptCache::new(2);
        c.mark_pending("r", "a");
        c.insert("r", "a", Some(1), content("a1"));
        assert!(c.take_pending("r").is_empty());
        assert_eq!(c.get("r", "a", Some(1)).unwrap().message.text(), "a1");
        assert!(c.get("r", "a", Some(2)).is_none());
        c.insert("r", "b", None, content("b"));
        c.insert("r", "c", None, content("c"));
        assert_eq!(c.len(), 2);
        assert!(c.get("r", "a", Some(1)).is_none());
        c.mark_pending("r", "x");
        c.mark_pending("r", "y");
        assert_eq!(c.take_pending("r"), ["x", "y"]);
        c.clear();
        assert!(c.is_empty());
    }
}
