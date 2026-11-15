//! Decoded message blocks per message content: `Message::uikit_blocks` runs
//! the size guard and a full decode, so it must not run on every frame.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use rocketchat::models::Message;
use rocketchat::uikit::LayoutBlock;
use serde_json::Value;

/// Entries kept before the cache starts over.
const CAPACITY: usize = 512;

/// The raw `blocks` an entry was decoded from, and the result.
struct Entry {
    raw: Value,
    decoded: Option<Rc<Vec<LayoutBlock>>>,
}

thread_local! {
    /// By room and message id. The cache is shared by all servers, and ids
    /// and timestamps are chosen by whoever delivers a message, so a hit
    /// counts only when the stored raw blocks equal the message's.
    static CACHE: RefCell<HashMap<(String, String), Entry>> = RefCell::new(HashMap::new());
}

/// The decoded blocks of `m`; `None` when it has none (or only blocks the
/// size guard dropped), so its body is shown (§3.1, §11.1).
pub fn blocks(m: &Message) -> Option<Rc<Vec<LayoutBlock>>> {
    let raw = m.blocks.as_ref()?;
    if raw.as_array().is_none_or(Vec::is_empty) {
        return None;
    }
    let key = (m.rid.clone(), m.id.clone());
    CACHE.with(|c| {
        if let Some(e) = c.borrow().get(&key)
            && e.raw == *raw
        {
            return e.decoded.clone();
        }
        let decoded = m.has_blocks().then(|| Rc::new(m.uikit_blocks()));
        let mut c = c.borrow_mut();
        if c.len() >= CAPACITY && !c.contains_key(&key) {
            c.clear();
        }
        c.insert(
            key,
            Entry {
                raw: raw.clone(),
                decoded: decoded.clone(),
            },
        );
        decoded
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use rocketchat::RcDate;
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

    fn button(app: &str, action: &str) -> Value {
        json!([{"type": "actions", "blockId": "b", "appId": app, "elements": [
            {"type": "button", "appId": app, "actionId": action,
             "text": {"type": "plain_text", "text": action}, "value": "v"}
        ]}])
    }

    fn message(rid: &str, raw: Value) -> Message {
        let ts: Option<RcDate> =
            serde_json::from_value(json!({"$date": 1_794_088_390_781_i64})).ok();
        assert!(ts.is_some());
        Message {
            id: "same-id".into(),
            rid: rid.into(),
            ts,
            updated_at: ts,
            blocks: Some(raw),
            ..Default::default()
        }
    }

    /// A message of another server with the same id, room id, timestamps and
    /// block count must not supply the blocks of this one.
    #[test]
    fn same_id_and_timestamps_with_other_blocks_are_decoded_apart() {
        let forged = message("room", button("victim-app", "delete-everything"));
        let real = message("room", button("victim-app", "show-report"));
        let forged_blocks = blocks(&forged);
        assert_eq!(forged_blocks.as_deref(), Some(&forged.uikit_blocks()));
        let real_blocks = blocks(&real);
        assert_eq!(real_blocks.as_deref(), Some(&real.uikit_blocks()));
        assert_ne!(real_blocks, forged_blocks);
        // And back: the entry follows the content, not the id.
        assert_eq!(blocks(&forged).as_deref(), Some(&forged.uikit_blocks()));
        // Same content is served from the cache.
        let again = blocks(&forged);
        assert!(
            again
                .zip(blocks(&forged))
                .is_some_and(|(a, b)| Rc::ptr_eq(&a, &b))
        );
    }

    #[test]
    fn rooms_do_not_share_entries() {
        let a = message("room-a", button("app", "one"));
        let b = message("room-b", button("app", "two"));
        let (da, db) = (blocks(&a), blocks(&b));
        assert_eq!(da.as_deref(), Some(&a.uikit_blocks()));
        assert_eq!(db.as_deref(), Some(&b.uikit_blocks()));
        // Both stay cached.
        assert!(da.zip(blocks(&a)).is_some_and(|(x, y)| Rc::ptr_eq(&x, &y)));
        assert!(db.zip(blocks(&b)).is_some_and(|(x, y)| Rc::ptr_eq(&x, &y)));
    }
}
