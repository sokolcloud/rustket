//! Decoded message blocks per message version: `Message::uikit_blocks` runs
//! the size guard and a full decode, so it must not run on every frame.

use std::cell::RefCell;
use std::collections::HashMap;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::rc::Rc;

use rocketchat::models::Message;
use rocketchat::uikit::LayoutBlock;

/// Entries kept before the cache starts over.
const CAPACITY: usize = 512;

type Entry = Option<Rc<Vec<LayoutBlock>>>;

thread_local! {
    static CACHE: RefCell<HashMap<(String, u64), Entry>> = RefCell::new(HashMap::new());
}

/// Identity of the blocks of one message version (`_updatedAt` is missing on
/// app ephemerals, which reuse their `_id`; `ts` changes on replacement).
fn version(m: &Message) -> u64 {
    let mut h = DefaultHasher::new();
    m.updated_at.map(|d| d.timestamp_millis()).hash(&mut h);
    m.ts.map(|d| d.timestamp_millis()).hash(&mut h);
    m.edited_at.map(|d| d.timestamp_millis()).hash(&mut h);
    match &m.blocks {
        Some(serde_json::Value::Array(a)) => a.len().hash(&mut h),
        Some(_) => 1usize.hash(&mut h),
        None => 0usize.hash(&mut h),
    }
    h.finish()
}

/// The decoded blocks of `m`; `None` when it has none (or only blocks the
/// size guard dropped), so its body is shown (§3.1, §11.1).
pub fn blocks(m: &Message) -> Option<Rc<Vec<LayoutBlock>>> {
    let raw = m.blocks.as_ref()?;
    if raw.as_array().is_none_or(Vec::is_empty) {
        return None;
    }
    let key = (m.id.clone(), version(m));
    CACHE.with(|c| {
        if let Some(e) = c.borrow().get(&key) {
            return e.clone();
        }
        let decoded = m.has_blocks().then(|| Rc::new(m.uikit_blocks()));
        let mut c = c.borrow_mut();
        if c.len() >= CAPACITY {
            c.clear();
        }
        c.insert(key, decoded.clone());
        decoded
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn empty_blocks_show_the_body() {
        let mut m = Message {
            id: "c1".into(),
            ..Default::default()
        };
        assert!(blocks(&m).is_none());
        m.blocks = Some(json!([]));
        assert!(blocks(&m).is_none());
        m.blocks = Some(json!([{"type": "divider"}]));
        assert_eq!(blocks(&m).map(|b| b.len()), Some(1));
        // A new version with the same id is decoded again.
        m.blocks = Some(json!([{"type": "divider"}, {"type": "divider"}]));
        assert_eq!(blocks(&m).map(|b| b.len()), Some(2));
    }
}
