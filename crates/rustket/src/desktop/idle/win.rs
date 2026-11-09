//! Windows idle time: `GetLastInputInfo` (through `user-idle`).

use std::time::Duration;

pub async fn query_idle() -> Option<Duration> {
    // `user-idle` subtracts tick counts without wrapping (a panic in debug
    // builds right after the 49.7-day tick wrap).
    match std::panic::catch_unwind(user_idle::UserIdle::get_time) {
        Ok(Ok(t)) => Some(t.duration()),
        Ok(Err(e)) => {
            tracing::debug!(error = %e, "no idle time");
            None
        }
        Err(_) => None,
    }
}
