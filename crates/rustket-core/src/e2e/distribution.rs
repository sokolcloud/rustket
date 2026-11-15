//! The key distribution loop (§7.4): every ~10 s, provide the room key to
//! members waiting for it in up to 10 random rooms where this client holds it.

use std::collections::HashMap;
use std::sync::{Arc, Weak};
use std::time::Duration;

use rocketchat::RcDate;
use rocketchat::e2e::RoomKey;
use rocketchat::rest::SuggestedKey;

use super::distribution;
use super::{
    E2eApi, E2eManager, Inner, Result, RoomE2eState, lock, random_u64, read, wrap_for_users,
};

/// Rooms per round (web `ROOM_KEY_EXCHANGE_SIZE`).
pub const ROOM_KEY_EXCHANGE_SIZE: usize = 10;

/// Keeps a uniformly random subset of at most `n` items (partial Fisher–Yates).
pub(crate) fn sample<T>(items: &mut Vec<T>, n: usize) {
    let len = items.len();
    for i in 0..n.min(len) {
        let span = (len - i) as u64;
        let j = i + usize::try_from(random_u64() % span).unwrap_or(0);
        items.swap(i, j);
    }
    items.truncate(n);
}

/// `period` ± 20 %.
fn jittered(period: Duration) -> Duration {
    let ms = u64::try_from(period.as_millis()).unwrap_or(u64::MAX).max(1);
    let spread = ms / 5;
    let offset = if spread == 0 {
        0
    } else {
        random_u64() % (2 * spread + 1)
    };
    Duration::from_millis(ms - spread + offset)
}

/// Runs until the manager is dropped or the task is aborted.
pub(crate) async fn run_loop<A: E2eApi>(inner: Weak<Inner<A>>, period: Duration) {
    loop {
        {
            let Some(inner) = inner.upgrade() else {
                return;
            };
            let manager = E2eManager { inner };
            if let Err(e) = manager.run_distribution_once().await {
                tracing::debug!(error = %e, "key distribution round failed");
            }
        }
        tokio::time::sleep(jittered(period)).await;
    }
}

impl<A: E2eApi> E2eManager<A> {
    // ----- distribution -----------------------------------------------------------

    pub(super) fn start_distribution(&self) {
        let mut slot = lock(&self.inner.distribution);
        if slot.as_ref().is_some_and(|h| !h.is_finished()) {
            return;
        }
        let weak = Arc::downgrade(&self.inner);
        let period = self.inner.config.distribution_interval;
        *slot = Some(tokio::spawn(distribution::run_loop(weak, period)));
    }

    /// One distribution round over up to 10 random candidate rooms (§7.4).
    /// Returns the number of keys provided.
    pub async fn run_distribution_once(&self) -> Result<usize> {
        if !self.writable() || self.private_key().is_none() {
            return Ok(0);
        }
        let me = &self.inner.user_id;
        let mut candidates: Vec<String> = read(&self.inner.summaries)
            .iter()
            .filter(|(_, s)| {
                s.has_key
                    && s.state == RoomE2eState::Ready
                    && s.waiting.as_ref().is_some_and(|w| !w.contains(me))
            })
            .map(|(rid, _)| rid.clone())
            .collect();
        distribution::sample(&mut candidates, distribution::ROOM_KEY_EXCHANGE_SIZE);
        self.distribute_rooms(candidates).await
    }

    /// Provides keys to the waiting members of `rids` (rooms without a loaded key
    /// are skipped).
    pub async fn distribute_rooms(&self, rids: Vec<String>) -> Result<usize> {
        if rids.is_empty() || !self.writable() || self.private_key().is_none() {
            return Ok(0);
        }
        let t = self.token();
        let waiting = self.inner.api.users_waiting_for_group_key(&rids).await?;
        self.check(t)?;
        let mut map: HashMap<String, Vec<SuggestedKey>> = HashMap::new();
        for (rid, users) in waiting {
            let users: Vec<_> = users
                .into_iter()
                .filter(|u| u.user_id != self.inner.user_id)
                .collect();
            if users.is_empty() {
                continue;
            }
            let (current, old) = {
                let cell = self.room_cell(&rid);
                let rc = cell.lock().await;
                let Some(cur) = rc
                    .current
                    .clone()
                    .filter(|_| rc.state == RoomE2eState::Ready && self.attached(&rid, &rc))
                else {
                    continue;
                };
                let old: Vec<(RoomKey, String)> = rc
                    .old_keys()
                    .into_iter()
                    .filter_map(|o| {
                        let ts = o.ts.unwrap_or_else(RcDate::now).to_iso();
                        o.key.clone().map(|k| (k, ts))
                    })
                    .collect();
                (cur, old)
            };
            let entries = wrap_for_users(current, old, users).await;
            if !entries.is_empty() {
                map.insert(rid, entries);
            }
        }
        let count = map.values().map(Vec::len).sum();
        if count > 0 {
            self.check(t)?;
            self.inner
                .api
                .provide_users_suggested_group_keys(&map)
                .await?;
            tracing::debug!(rooms = map.len(), count, "provided room keys");
        }
        Ok(count)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sampling_and_jitter() {
        let mut v: Vec<u32> = (0..30).collect();
        sample(&mut v, 10);
        assert_eq!(v.len(), 10);
        let mut sorted = v.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), 10);
        let mut few = vec![1, 2];
        sample(&mut few, 10);
        assert_eq!(few.len(), 2);
        for _ in 0..100 {
            let d = jittered(Duration::from_secs(10));
            assert!(d >= Duration::from_secs(8) && d <= Duration::from_secs(12));
        }
    }
}
