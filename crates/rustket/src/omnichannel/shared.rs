//! The omnichannel state of each server as a read-only snapshot for views and
//! room components, which only get the workspace in their contexts. The app
//! publishes a new snapshot whenever the server's [`OmnichannelState`]
//! changes (`glue.rs`); readers look it up by server id.

use std::collections::HashMap;
use std::sync::{Arc, LazyLock, RwLock};

use rustket_core::ServerId;
use rustket_core::omnichannel::OmnichannelState;

type Snapshots = HashMap<ServerId, Arc<OmnichannelState>>;

static SNAPSHOTS: LazyLock<RwLock<Snapshots>> = LazyLock::new(RwLock::default);

/// The last published state of `sid` (`None` before the first publish or
/// after [`forget`]).
pub fn get(sid: &ServerId) -> Option<Arc<OmnichannelState>> {
    let map = SNAPSHOTS.read().unwrap_or_else(|e| e.into_inner());
    map.get(sid).cloned()
}

/// Stores the current state of `sid`.
pub fn publish(sid: &ServerId, state: &OmnichannelState) {
    let mut map = SNAPSHOTS.write().unwrap_or_else(|e| e.into_inner());
    if map.get(sid).is_some_and(|s| **s == *state) {
        return;
    }
    map.insert(sid.clone(), Arc::new(state.clone()));
}

/// Drops the snapshot of `sid` (logout, server removed).
pub fn forget(sid: &ServerId) {
    let mut map = SNAPSHOTS.write().unwrap_or_else(|e| e.into_inner());
    map.remove(sid);
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn publish_get_forget() {
        let sid = ServerId("omni-shared-test".into());
        assert!(get(&sid).is_none());
        let mut s = OmnichannelState::new();
        s.accessible = true;
        publish(&sid, &s);
        assert!(get(&sid).is_some_and(|g| g.accessible));
        let first = get(&sid).unwrap();
        publish(&sid, &s);
        assert!(Arc::ptr_eq(&first, &get(&sid).unwrap()), "unchanged");
        forget(&sid);
        assert!(get(&sid).is_none());
    }
}
