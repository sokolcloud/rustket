//! Serialised config writes: one background thread writes the latest
//! snapshot of `AppConfig` + `UiState`, so concurrent saves can never land an
//! older snapshot after a newer one.

use std::path::PathBuf;
use std::sync::mpsc;
use std::time::Duration;

use rustket_core::config::AppConfig;

use crate::ui_state::UiState;

enum Cmd {
    Save(Box<(AppConfig, UiState)>),
    Flush(mpsc::Sender<()>),
}

pub struct ConfigWriter {
    tx: mpsc::Sender<Cmd>,
}

impl ConfigWriter {
    pub fn spawn(dir: PathBuf) -> Option<Self> {
        let (tx, rx) = mpsc::channel::<Cmd>();
        std::thread::Builder::new()
            .name("rustket-config".into())
            .spawn(move || {
                while let Ok(cmd) = rx.recv() {
                    let mut latest = None;
                    let mut flushes = Vec::new();
                    let mut next = Some(cmd);
                    // Coalesce everything queued so far into the newest snapshot.
                    while let Some(c) = next {
                        match c {
                            Cmd::Save(s) => latest = Some(s),
                            Cmd::Flush(ack) => flushes.push(ack),
                        }
                        next = rx.try_recv().ok();
                    }
                    if let Some(s) = latest {
                        let (cfg, ui) = *s;
                        if let Err(e) = cfg.save(&dir) {
                            tracing::error!(error = %e, "saving config failed");
                        }
                        if let Err(e) = ui.save(&dir) {
                            tracing::warn!(error = %e, "saving ui state failed");
                        }
                    }
                    for ack in flushes {
                        let _ = ack.send(());
                    }
                }
            })
            .map_err(|e| tracing::error!(error = %e, "cannot start the config writer"))
            .ok()?;
        Some(Self { tx })
    }

    pub fn save(&self, cfg: AppConfig, ui: UiState) {
        let _ = self.tx.send(Cmd::Save(Box::new((cfg, ui))));
    }

    /// Waits until every queued save is on disk (tests, shutdown).
    pub fn flush(&self) {
        let (ack, done) = mpsc::channel();
        if self.tx.send(Cmd::Flush(ack)).is_ok() {
            let _ = done.recv_timeout(Duration::from_secs(5));
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use rustket_core::config::ServerEntry;

    /// Review #17: the newest snapshot always wins.
    #[test]
    fn latest_snapshot_wins() {
        let dir = tempfile::tempdir().unwrap();
        let w = ConfigWriter::spawn(dir.path().to_owned()).unwrap();
        let mut cfg = AppConfig::default();
        for i in 0..50 {
            cfg.servers.clear();
            cfg.add_server(ServerEntry::new(format!("https://s{i}.example"), "x"));
            w.save(cfg.clone(), UiState::default());
        }
        w.flush();
        let disk = AppConfig::load(dir.path()).unwrap();
        assert_eq!(disk.servers[0].url, "https://s49.example");
    }
}
