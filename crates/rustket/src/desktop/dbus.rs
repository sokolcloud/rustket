//! The shared session-bus connection and small method-call helpers (every
//! call is bounded by a timeout).

use std::time::Duration;

pub const CALL_TIMEOUT: Duration = Duration::from_secs(2);

static BUS: tokio::sync::OnceCell<Option<zbus::Connection>> = tokio::sync::OnceCell::const_new();

/// The session bus (connected once; `None` when there is none).
pub async fn session_bus() -> Option<zbus::Connection> {
    BUS.get_or_init(|| async {
        match tokio::time::timeout(CALL_TIMEOUT, zbus::Connection::session()).await {
            Ok(Ok(c)) => Some(c),
            Ok(Err(e)) => {
                tracing::debug!(error = %e, "no session bus");
                None
            }
            Err(_) => {
                tracing::debug!("session bus connection timed out");
                None
            }
        }
    })
    .await
    .clone()
}

/// Calls `dest path iface.method(body)` and returns the reply message.
pub async fn call<B>(
    dest: &str,
    path: &str,
    iface: &str,
    method: &str,
    body: &B,
) -> Option<zbus::Message>
where
    B: serde::Serialize + zbus::zvariant::DynamicType,
{
    let bus = session_bus().await?;
    match tokio::time::timeout(
        CALL_TIMEOUT,
        bus.call_method(Some(dest), path, Some(iface), method, body),
    )
    .await
    {
        Ok(Ok(m)) => Some(m),
        Ok(Err(e)) => {
            tracing::debug!(%dest, %method, error = %e, "D-Bus call failed");
            None
        }
        Err(_) => {
            tracing::debug!(%dest, %method, "D-Bus call timed out");
            None
        }
    }
}
