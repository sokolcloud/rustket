//! macOS sounds, played in-process by NSSound: a system sound by name or
//! one of Rocket.Chat's sounds (a cached WAV file); notifications play a
//! copy of the latter in `~/Library/Sounds` by name ([`install_in`]).

use std::path::PathBuf;

/// System sound of a freedesktop sound id (`None`: the message sound).
pub fn system_sound(id: Option<&str>) -> &'static str {
    match id {
        Some("phone-incoming-call") => "Submarine",
        Some("phone-outgoing-calling") => "Purr",
        _ => "Glass",
    }
}

/// Nothing to look up: NSSound is always there.
#[cfg(target_os = "macos")]
#[derive(Debug, Clone)]
pub struct Resolved;

#[cfg(target_os = "macos")]
pub fn lookup() -> Resolved {
    Resolved
}

/// What one play request plays.
#[cfg(target_os = "macos")]
#[derive(Debug)]
pub enum Job {
    /// A system sound by name (`Glass`, …).
    Named(&'static str),
    /// A sound file.
    File(PathBuf),
}

/// The system sound of freedesktop sound id `id`.
#[cfg(target_os = "macos")]
pub fn play_command(_: &Resolved, id: Option<&'static str>) -> Option<Job> {
    Some(Job::Named(system_sound(id)))
}

/// The sound file `file`.
#[cfg(target_os = "macos")]
pub fn file_command(_: &Resolved, file: &std::path::Path) -> Option<Job> {
    Some(Job::File(file.to_owned()))
}

/// Plays `job` and keeps the sound alive until it ends or
/// `super::PLAY_TIMEOUT` passes. Blocking.
#[cfg(target_os = "macos")]
pub fn run(job: Job) {
    use objc2::AllocAnyThread;
    use objc2_app_kit::NSSound;
    use objc2_foundation::{NSCopying, NSString};
    use std::time::{Duration, Instant};
    let sound = match &job {
        // `soundNamed` returns a shared instance: a copy plays on its own.
        Job::Named(name) => NSSound::soundNamed(&NSString::from_str(name))
            .or_else(|| NSSound::soundNamed(&NSString::from_str(system_sound(None))))
            .map(|s| s.copy()),
        Job::File(path) => NSSound::initWithContentsOfFile_byReference(
            NSSound::alloc(),
            &NSString::from_str(&path.to_string_lossy()),
            true,
        ),
    };
    let Some(sound) = sound else {
        tracing::debug!(?job, "sound not found");
        return;
    };
    if !sound.play() {
        tracing::debug!("NSSound did not start");
        return;
    }
    let started = Instant::now();
    while sound.isPlaying() {
        if started.elapsed() >= super::PLAY_TIMEOUT {
            sound.stop();
            return;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// Prefix of Rustket's sound files in `~/Library/Sounds`.
pub const INSTALLED_PREFIX: &str = "rustket-";

/// Copies used more recently than this survive the sweep of every install:
/// a notification posted just before may still play them by name.
pub const KEEP_RECENT: std::time::Duration = std::time::Duration::from_secs(120);

/// Copies the cached sound `file` into the sounds directory `dir` as
/// `rustket-<file name>`, so a notification can play it by name (an existing
/// copy is reused). Every call removes Rustket's other copies there (only
/// `rustket-*` files) that were not used for [`KEEP_RECENT`]. Calls are
/// serialized. Blocking.
pub fn install_in(dir: &std::path::Path, file: &std::path::Path) -> std::io::Result<PathBuf> {
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::SystemTime;
    static LOCK: Mutex<()> = Mutex::new(());
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let _guard = LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let name = file
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| std::io::Error::other("sound file without a name"))?;
    let installed = format!("{INSTALLED_PREFIX}{name}");
    let target = dir.join(&installed);
    // The cache files are versioned by name: an existing copy is current.
    // Its modification time marks it as in use.
    if std::fs::metadata(&target).is_ok_and(|m| m.is_file() && m.len() > 0) {
        if let Ok(f) = std::fs::File::options().append(true).open(&target) {
            let _ = f.set_modified(SystemTime::now());
        }
    } else {
        std::fs::create_dir_all(dir)?;
        let seq = SEQ.fetch_add(1, Ordering::Relaxed);
        let tmp = dir.join(format!(".{installed}.{}.{seq}.tmp", std::process::id()));
        // On macOS `copy` clones the file with its modification time: mark
        // the copy as used now, or the next sweep would take it as stale.
        let written = std::fs::copy(file, &tmp)
            .and_then(|_| std::fs::File::options().append(true).open(&tmp))
            .and_then(|f| f.set_modified(SystemTime::now()))
            .and_then(|()| std::fs::rename(&tmp, &target));
        if let Err(e) = written {
            let _ = std::fs::remove_file(&tmp);
            return Err(e);
        }
    }
    // Sweep on every call, so an old copy goes even while the current one
    // is reused. A failed sweep does not fail the install.
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Ok(target);
    };
    let now = SystemTime::now();
    for e in entries.flatten() {
        let n = e.file_name();
        let Some(n) = n.to_str() else { continue };
        if !n.starts_with(INSTALLED_PREFIX) || n == installed {
            continue;
        }
        let stale = e.metadata().is_ok_and(|m| {
            m.is_file()
                && m.modified()
                    .is_ok_and(|t| now.duration_since(t).is_ok_and(|age| age >= KEEP_RECENT))
        });
        if stale {
            let _ = std::fs::remove_file(e.path());
        }
    }
    Ok(target)
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn install_copies_and_replaces_only_our_files() {
        let cache = tempfile::tempdir().unwrap();
        let sounds = tempfile::tempdir().unwrap();
        let chime = cache.path().join("v1-chime-80.wav");
        let ding = cache.path().join("v1-ding-80.wav");
        std::fs::write(&chime, b"RIFFchime").unwrap();
        std::fs::write(&ding, b"RIFFding").unwrap();
        std::fs::write(sounds.path().join("Other.aiff"), b"x").unwrap();
        let a = install_in(sounds.path(), &chime).unwrap();
        assert_eq!(a, sounds.path().join("rustket-v1-chime-80.wav"));
        assert_eq!(std::fs::read(&a).unwrap(), b"RIFFchime");
        // Again: kept as it is.
        assert_eq!(install_in(sounds.path(), &chime).unwrap(), a);
        // A copy used just now stays next to the new one.
        let b = install_in(sounds.path(), &ding).unwrap();
        assert!(b.is_file() && a.is_file());
        // One not used for a while goes when another copy is written.
        let old = std::time::SystemTime::now() - KEEP_RECENT * 2;
        let age = |p: &std::path::Path| {
            std::fs::File::options()
                .append(true)
                .open(p)
                .unwrap()
                .set_modified(old)
                .unwrap();
        };
        age(&a);
        age(&b);
        std::fs::remove_file(&b).unwrap();
        let b = install_in(sounds.path(), &ding).unwrap();
        assert!(b.is_file() && !a.exists());
        // Reusing a copy marks it as used and sweeps old copies too; a
        // copy used just now stays.
        let c = install_in(sounds.path(), &chime).unwrap();
        age(&b);
        age(&c);
        std::fs::write(sounds.path().join("Other.aiff"), b"x").unwrap();
        age(&sounds.path().join("Other.aiff"));
        let fresh = sounds.path().join("rustket-v1-door-80.wav");
        std::fs::write(&fresh, b"RIFFdoor").unwrap();
        install_in(sounds.path(), &ding).unwrap();
        let used = std::fs::metadata(&b).unwrap().modified().unwrap();
        assert!(used > old + KEEP_RECENT);
        assert!(!c.exists() && fresh.is_file());
        std::fs::remove_file(&fresh).unwrap();
        assert!(sounds.path().join("Other.aiff").is_file());
        let mut names: Vec<_> = std::fs::read_dir(sounds.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
        names.sort();
        assert_eq!(names, ["Other.aiff", "rustket-v1-ding-80.wav"]);
    }

    /// A fresh copy of an old cache file is not swept by the next install.
    #[test]
    fn copy_of_an_old_cache_file_counts_as_used() {
        let cache = tempfile::tempdir().unwrap();
        let sounds = tempfile::tempdir().unwrap();
        let chime = cache.path().join("v1-chime-80.wav");
        let ding = cache.path().join("v1-ding-80.wav");
        std::fs::write(&chime, b"RIFFchime").unwrap();
        std::fs::write(&ding, b"RIFFding").unwrap();
        std::fs::File::options()
            .append(true)
            .open(&chime)
            .unwrap()
            .set_modified(std::time::SystemTime::now() - KEEP_RECENT * 10)
            .unwrap();
        let a = install_in(sounds.path(), &chime).unwrap();
        let b = install_in(sounds.path(), &ding).unwrap();
        assert!(a.is_file() && b.is_file());
    }

    #[test]
    fn sound_mapping() {
        assert_eq!(system_sound(None), "Glass");
        assert_eq!(system_sound(Some("phone-incoming-call")), "Submarine");
        assert_eq!(system_sound(Some("phone-outgoing-calling")), "Purr");
    }
}
