//! The key distribution loop (§7.4): every ~10 s, provide the room key to
//! members waiting for it in up to 10 random rooms where this client holds it.

use std::sync::Weak;
use std::time::Duration;

use super::{E2eApi, E2eManager, Inner, random_u64};

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
