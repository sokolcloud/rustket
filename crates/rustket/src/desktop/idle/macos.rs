//! macOS idle time: IOHIDSystem's `HIDIdleTime` (nanoseconds), read from
//! `ioreg -c IOHIDSystem -d 4`.

use std::time::Duration;

/// `HIDIdleTime` in `ioreg` output: `"HIDIdleTime" = 123456789`.
pub fn parse_hid_idle_time(out: &str) -> Option<Duration> {
    out.lines().find_map(|l| {
        let rest = l.split_once("\"HIDIdleTime\"")?.1.trim_start();
        let digits: String = rest
            .strip_prefix('=')?
            .trim_start()
            .chars()
            .take_while(char::is_ascii_digit)
            .collect();
        digits.parse::<u64>().ok().map(Duration::from_nanos)
    })
}

#[cfg(target_os = "macos")]
fn ioreg_blocking() -> Option<Duration> {
    let out = std::process::Command::new("/usr/sbin/ioreg")
        .args(["-c", "IOHIDSystem", "-d", "4"])
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    parse_hid_idle_time(&String::from_utf8_lossy(&out.stdout))
}

#[cfg(target_os = "macos")]
pub async fn query_idle() -> Option<Duration> {
    tokio::time::timeout(
        super::QUERY_TIMEOUT,
        tokio::task::spawn_blocking(ioreg_blocking),
    )
    .await
    .ok()?
    .ok()?
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_ioreg_output() {
        let out = r#"+-o Root  <class IORegistryEntry, id 0x100000100>
    +-o IOHIDSystem  <class IOHIDSystem, id 0x100000466>
        {
          "HIDIdleTimeDelta" = 0
          "HIDIdleTime" = 4518249416
          "HIDParameters" = {}
        }
"#;
        assert_eq!(
            parse_hid_idle_time(out),
            Some(Duration::from_nanos(4_518_249_416))
        );
        assert_eq!(parse_hid_idle_time("\"HIDIdleTime\" = x"), None);
        assert_eq!(parse_hid_idle_time(""), None);
    }
}
