//! Logging set-up: standard output always, and with the `log_file`
//! preference also a size-rotated file in the per-user local log folder
//! (release builds on Windows have no console, and macOS discards the
//! standard output of a GUI launch). Both sinks share one filter; the
//! file also drops everything below `info`.

use std::borrow::Cow;
use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, Mutex, MutexGuard, PoisonError};

use rustket_core::config::{AppConfig, CONFIG_FILE};
use tracing_subscriber::Layer as _;
use tracing_subscriber::field::RecordFields;
use tracing_subscriber::filter::LevelFilter;
use tracing_subscriber::fmt::MakeWriter;
use tracing_subscriber::fmt::format::{DefaultFields, FormatFields};
use tracing_subscriber::layer::SubscriberExt as _;
use tracing_subscriber::util::SubscriberInitExt as _;

/// Toolkit crates that are noisy at `info`: `iced_winit` dumps the window
/// attributes including every byte of the icon, `winit` warns about XSETTINGS
/// on every start under KDE.
pub const QUIET_DEPS: &str = "wgpu=warn,naga=warn,iced_winit=warn,winit=error";

/// The active log file in [`default_dir`].
pub const FILE_NAME: &str = "rustket.log";
/// The previous log file, replaced at each rotation.
pub const OLD_FILE_NAME: &str = "rustket.1.log";
/// The active file is rotated before it would exceed this size.
pub const MAX_BYTES: u64 = 2 * 1024 * 1024;
/// After a failed rotation (another process holds the file on Windows), the
/// next attempt waits for this many more bytes.
const RETRY_BYTES: u64 = 256 * 1024;
/// Writes dropped between two attempts to reopen a file that failed.
const REOPEN_EVERY: u32 = 64;

/// The log filter: the user's `RUST_LOG` (or the recommended default), after
/// [`QUIET_DEPS`] (more specific directives win, so `RUST_LOG=debug` keeps the
/// toolkit quiet unless it names those crates) and always followed by
/// `rocketchat::LOG_FILTER_PINS` so transport crates that log credentials at
/// TRACE (tungstenite logs the resume token) stay at `info`.
pub fn filter(user: Option<&str>) -> String {
    match user.map(str::trim).filter(|f| !f.is_empty()) {
        Some(f) => format!("{QUIET_DEPS},{f},{}", rocketchat::LOG_FILTER_PINS),
        None => format!("{},{QUIET_DEPS}", rocketchat::RECOMMENDED_LOG_FILTER),
    }
}

/// The saved `log_file` preference, read without side effects like
/// `renderer::saved_hardware_acceleration`; any error means off.
pub fn saved_log_file(dir: Option<&Path>) -> bool {
    dir.and_then(|d| fs::read_to_string(d.join(CONFIG_FILE)).ok())
        .and_then(|text| AppConfig::parse(&text).ok())
        .is_some_and(|c| c.ui.log_file)
}

/// Which convention [`pick_dir`] follows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Platform {
    Windows,
    MacOs,
    Other,
}

impl Platform {
    fn current() -> Self {
        if cfg!(windows) {
            Self::Windows
        } else if cfg!(target_os = "macos") {
            Self::MacOs
        } else {
            Self::Other
        }
    }
}

/// The log folder from the per-user locations: Local AppData on Windows,
/// `~/Library/Logs` on macOS (inside the container in the sandbox, where
/// the home directory is the container's), the XDG state directory
/// elsewhere; the cache location when those are unknown.
fn pick_dir(
    platform: Platform,
    home: Option<&Path>,
    local_data: Option<&Path>,
    state: Option<&Path>,
    cache: Option<&Path>,
) -> Option<PathBuf> {
    let preferred = match platform {
        Platform::Windows => local_data.map(|d| d.join("rustket").join("logs")),
        Platform::MacOs => home.map(|h| h.join("Library").join("Logs").join("rustket")),
        Platform::Other => state.map(|s| s.join("rustket").join("logs")),
    };
    preferred.or_else(|| cache.map(|c| c.join("logs")))
}

/// Where the log file is kept (`None` when no per-user location is known).
pub fn default_dir() -> Option<PathBuf> {
    static DIR: LazyLock<Option<PathBuf>> = LazyLock::new(|| {
        let base = directories::BaseDirs::new();
        let project = directories::ProjectDirs::from("", "", "rustket");
        pick_dir(
            Platform::current(),
            base.as_ref().map(directories::BaseDirs::home_dir),
            base.as_ref().map(directories::BaseDirs::data_local_dir),
            base.as_ref().and_then(directories::BaseDirs::state_dir),
            project.as_ref().map(directories::ProjectDirs::cache_dir),
        )
    });
    DIR.clone()
}

/// Creates the log folder (and missing parents); on Unix the folder is
/// made accessible to the user only.
pub fn create_dir(dir: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::{DirBuilderExt as _, PermissionsExt as _};
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(dir)?;
        // A folder the user replaced by a link keeps the target's mode.
        if fs::symlink_metadata(dir)?.file_type().is_symlink() {
            return Ok(());
        }
        let meta = fs::metadata(dir)?;
        if meta.permissions().mode() & 0o077 != 0 {
            fs::set_permissions(dir, fs::Permissions::from_mode(0o700))?;
        }
        Ok(())
    }
    #[cfg(not(unix))]
    fs::create_dir_all(dir)
}

/// Opens `path` for appending (created 0600 on Unix). Other processes may
/// open, append to and rename it meanwhile: Unix appends atomically per
/// write, and on Windows the standard library shares read, write and
/// delete access. Only a regular file is accepted: on Unix a link at the
/// path is not followed and a FIFO fails instead of blocking the start.
fn open_append(path: &Path) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.create(true).append(true);
    #[cfg(unix)]
    {
        use rustix::fs::OFlags;
        use std::os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _};
        options
            .mode(0o600)
            .custom_flags((OFlags::NOFOLLOW | OFlags::NONBLOCK).bits().cast_signed());
        let file = options.open(path)?;
        let meta = file.metadata()?;
        if !meta.is_file() {
            return Err(not_a_regular_file());
        }
        if meta.permissions().mode() & 0o077 != 0 {
            file.set_permissions(fs::Permissions::from_mode(0o600))?;
        }
        Ok(file)
    }
    #[cfg(not(unix))]
    {
        let file = options.open(path)?;
        if !file.metadata()?.is_file() {
            return Err(not_a_regular_file());
        }
        Ok(file)
    }
}

fn not_a_regular_file() -> io::Error {
    io::Error::other("the log path is not a regular file")
}

/// Whether the open file (`handle` is its metadata) is still the one at
/// `path`: another process may have renamed or removed it.
#[cfg(unix)]
fn is_file_at(handle: &fs::Metadata, path: &Path) -> bool {
    use std::os::unix::fs::MetadataExt as _;
    fs::symlink_metadata(path).is_ok_and(|p| p.dev() == handle.dev() && p.ino() == handle.ino())
}

/// Whether the open file (`handle` is its metadata) is still the one at
/// `path`. The standard library gives no stable file identity on Windows
/// without unsafe code; a missing file at the path, or one shorter than the
/// open file, is the best available sign of a rename or removal.
#[cfg(not(unix))]
fn is_file_at(handle: &fs::Metadata, path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok_and(|p| p.len() >= handle.len())
}

/// `line` with terminal control bytes written out as text (`\x1b`,
/// `\u{9b}`): the file layer emits no colours itself, so such bytes can
/// only come from logged data (a `%display` field value, a span name, a
/// `log` target) and must not reach a terminal that shows the file. Tab and
/// newline are kept.
fn escape_controls(line: &[u8]) -> Cow<'_, [u8]> {
    let is_control = |b: u8| (b < 0x20 && b != b'\t' && b != b'\n') || b == 0x7f;
    let is_c1 =
        |i: usize| line[i] == 0xc2 && line.get(i + 1).is_some_and(|n| (0x80..=0x9f).contains(n));
    if !(0..line.len()).any(|i| is_control(line[i]) || is_c1(i)) {
        return Cow::Borrowed(line);
    }
    let mut out = Vec::with_capacity(line.len() + 16);
    let mut i = 0;
    while i < line.len() {
        let b = line[i];
        if is_control(b) {
            out.extend_from_slice(format!("\\x{b:02x}").as_bytes());
        } else if is_c1(i) {
            out.extend_from_slice(format!("\\u{{{:x}}}", line[i + 1]).as_bytes());
            i += 1;
        } else {
            out.push(b);
        }
        i += 1;
    }
    Cow::Owned(out)
}

#[derive(Debug)]
struct Inner {
    dir: PathBuf,
    limit: u64,
    file: Option<File>,
    /// Bytes in the open file, read again before each line (another
    /// process may append to it too).
    size: u64,
    /// The size from which the next rotation is attempted.
    rotate_from: u64,
    /// Writes left to drop before the next reopen attempt.
    reopen_in: u32,
}

impl Inner {
    fn active(&self) -> PathBuf {
        self.dir.join(FILE_NAME)
    }

    fn open_active(&mut self) -> io::Result<()> {
        let file = open_append(&self.active())?;
        self.size = file.metadata().map(|m| m.len()).unwrap_or(0);
        self.rotate_from = self.limit;
        self.file = Some(file);
        Ok(())
    }

    /// Makes room for `incoming` bytes. Another process may have rotated
    /// already: when the file at the path has room, it is reopened instead.
    /// A failed rename keeps the current file (it grows past the limit) and
    /// is retried later; nothing is truncated.
    fn rotate(&mut self, incoming: u64) {
        let active = self.active();
        if let Ok(meta) = fs::metadata(&active)
            && meta.len().saturating_add(incoming) <= self.limit
        {
            if self.open_active().is_err() {
                self.file = None;
            }
            return;
        }
        // Released first: Windows renames an open file only with share access.
        self.file = None;
        let renamed = fs::rename(&active, self.dir.join(OLD_FILE_NAME));
        match self.open_active() {
            Ok(()) if renamed.is_err() => {
                self.rotate_from = self.size.saturating_add(RETRY_BYTES);
            }
            Ok(()) => {}
            Err(_) => self.file = None,
        }
    }

    fn write_line(&mut self, buf: &[u8]) {
        let buf = escape_controls(buf);
        if self.file.is_none() {
            if self.reopen_in > 0 {
                self.reopen_in -= 1;
                return;
            }
            if self.open_active().is_err() {
                self.reopen_in = REOPEN_EVERY;
                return;
            }
        }
        if let Some(meta) = self.file.as_ref().and_then(|f| f.metadata().ok()) {
            self.size = meta.len();
            // A handle to a file renamed or removed by another process would
            // keep writing where nobody looks.
            if !is_file_at(&meta, &self.active()) && self.open_active().is_err() {
                self.file = None;
                self.reopen_in = REOPEN_EVERY;
                return;
            }
        }
        let incoming = u64::try_from(buf.len()).unwrap_or(u64::MAX);
        let next = self.size.saturating_add(incoming);
        if self.size > 0 && next > self.limit && next > self.rotate_from {
            self.rotate(incoming);
        }
        let Some(file) = self.file.as_mut() else {
            self.reopen_in = REOPEN_EVERY;
            return;
        };
        match file.write_all(&buf) {
            Ok(()) => self.size = self.size.saturating_add(incoming),
            Err(_) => {
                self.file = None;
                self.reopen_in = REOPEN_EVERY;
            }
        }
    }
}

/// An append-only log file that rotates to [`OLD_FILE_NAME`] before it would
/// exceed its limit. Cloning shares the file; writers from several threads
/// take turns per event, so lines never mix. Errors after opening are
/// swallowed: logging never fails the app.
#[derive(Debug, Clone)]
pub struct RotatingFile {
    inner: Arc<Mutex<Inner>>,
}

impl RotatingFile {
    /// Opens `dir/rustket.log` for appending with the [`MAX_BYTES`] limit.
    /// The folder must exist ([`create_dir`]).
    pub fn open(dir: &Path) -> io::Result<Self> {
        Self::with_limit(dir, MAX_BYTES)
    }

    /// [`Self::open`] with another size limit.
    pub fn with_limit(dir: &Path, limit: u64) -> io::Result<Self> {
        let mut inner = Inner {
            dir: dir.to_owned(),
            limit,
            file: None,
            size: 0,
            rotate_from: limit,
            reopen_in: 0,
        };
        inner.open_active()?;
        Ok(Self {
            inner: Arc::new(Mutex::new(inner)),
        })
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Appends one complete line.
    pub fn write_line(&self, line: &str) {
        self.lock().write_line(line.as_bytes());
    }
}

/// The writer of one event: holds the file until the event is written, and
/// writes the event's bytes in one piece when dropped (so a rotation never
/// splits a line).
pub struct EventWriter<'a> {
    inner: MutexGuard<'a, Inner>,
    buf: Vec<u8>,
}

impl Write for EventWriter<'_> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.buf.extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl Drop for EventWriter<'_> {
    fn drop(&mut self) {
        if !self.buf.is_empty() {
            self.inner.write_line(&self.buf);
        }
    }
}

impl<'a> MakeWriter<'a> for RotatingFile {
    type Writer = EventWriter<'a>;

    fn make_writer(&'a self) -> Self::Writer {
        EventWriter {
            inner: self.lock(),
            buf: Vec::new(),
        }
    }
}

/// The file side of the logger at start-up.
#[derive(Debug)]
pub enum FileSink {
    /// The preference is off: nothing was touched.
    Off,
    On(RotatingFile),
    /// The preference is on but there is no file; logged as one warning.
    Failed {
        dir: Option<PathBuf>,
        error: String,
    },
}

/// Opens the log file when `enabled`; `dir` is asked only then. Creates the
/// folder first.
pub fn file_sink(enabled: bool, dir: impl FnOnce() -> Option<PathBuf>) -> FileSink {
    if !enabled {
        return FileSink::Off;
    }
    let Some(dir) = dir() else {
        return FileSink::Failed {
            dir: None,
            error: "no per-user folder for the log".to_owned(),
        };
    };
    match create_dir(&dir).and_then(|()| RotatingFile::open(&dir)) {
        Ok(file) => FileSink::On(file),
        Err(e) => FileSink::Failed {
            dir: Some(dir),
            error: e.to_string(),
        },
    }
}

/// The file side at start-up from the saved preference in `config_dir`
/// ([`saved_log_file`]); `log_dir` is asked only when it is on.
pub fn start_sink(
    config_dir: Option<&Path>,
    log_dir: impl FnOnce() -> Option<PathBuf>,
) -> FileSink {
    file_sink(saved_log_file(config_dir), log_dir)
}

/// The build flavor named in the first line of the file.
fn flavor() -> &'static str {
    if cfg!(feature = "mas") {
        "mas"
    } else if cfg!(feature = "self-update") {
        "self-update"
    } else {
        "standard"
    }
}

/// The message of the file's first line after start: version, OS and
/// flavor only (no host or user name, no path).
pub fn header() -> String {
    format!(
        "Rustket {} started os={} arch={} flavor={}",
        env!("CARGO_PKG_VERSION"),
        std::env::consts::OS,
        std::env::consts::ARCH,
        flavor()
    )
}

/// [`header`] as a line in the file's format (the formatter's own UTC time).
fn header_line() -> String {
    use tracing_subscriber::fmt::time::FormatTime as _;
    let mut ts = String::new();
    let _ = tracing_subscriber::fmt::time::SystemTime
        .format_time(&mut tracing_subscriber::fmt::format::Writer::new(&mut ts));
    format!("{ts}  INFO {}: {}\n", module_path!(), header())
}

/// The file layer's field formatter: [`DefaultFields`] under a type of its
/// own. A span's formatted fields are stored once per formatter type
/// (`FormattedFields<N>`); with the standard output layer's type the file
/// would print that layer's coloured copy instead of formatting them itself.
#[derive(Debug, Default)]
struct FileFields(DefaultFields);

impl<'writer> FormatFields<'writer> for FileFields {
    fn format_fields<R: RecordFields>(
        &self,
        writer: tracing_subscriber::fmt::format::Writer<'writer>,
        fields: R,
    ) -> fmt::Result {
        self.0.format_fields(writer, fields)
    }
}

/// The subscriber: one filter (`filter`, else the recommended default when
/// it does not parse) in front of the standard output layer and, when
/// given, the file layer (no ANSI colours). The file never takes DEBUG or
/// TRACE, whatever `RUST_LOG` says: the rule against secrets and message
/// bodies holds from `info` up, and the file outlives the session.
pub fn subscriber<W>(
    filter: &str,
    stdout: W,
    file: Option<RotatingFile>,
) -> impl tracing::Subscriber + Send + Sync
where
    W: for<'a> MakeWriter<'a> + Send + Sync + 'static,
{
    build_subscriber(filter, stdout, None, file)
}

/// [`subscriber`]; `stdout_ansi` overrides the standard output layer's own
/// choice of colours (on unless `NO_COLOR` is set).
fn build_subscriber<W>(
    filter: &str,
    stdout: W,
    stdout_ansi: Option<bool>,
    file: Option<RotatingFile>,
) -> impl tracing::Subscriber + Send + Sync
where
    W: for<'a> MakeWriter<'a> + Send + Sync + 'static,
{
    let env_filter = tracing_subscriber::EnvFilter::try_new(filter)
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new(rocketchat::RECOMMENDED_LOG_FILTER));
    let mut stdout_layer = tracing_subscriber::fmt::layer().with_writer(stdout);
    if let Some(ansi) = stdout_ansi {
        stdout_layer = stdout_layer.with_ansi(ansi);
    }
    tracing_subscriber::registry()
        .with(env_filter)
        .with(stdout_layer)
        .with(file.map(|f| {
            tracing_subscriber::fmt::layer()
                .fmt_fields(FileFields::default())
                .with_ansi(false)
                .with_writer(f)
                .with_filter(LevelFilter::INFO)
        }))
}

/// Colours on standard output: only on a terminal (the README tells users to
/// redirect it into a file) and not with a non-empty `NO_COLOR`.
fn stdout_ansi(is_terminal: bool, no_color: Option<&std::ffi::OsStr>) -> bool {
    is_terminal && no_color.is_none_or(|v| v.is_empty())
}

/// Installs the global logger: standard output with [`filter`] of
/// `user_filter`, and the file of `sink` with the same filter (capped at
/// `info`). The file's first line is [`header`]; a failed file is one
/// warning on standard output.
pub fn init(user_filter: Option<&str>, sink: FileSink) {
    let filter = filter(user_filter);
    let (file, failure) = match sink {
        FileSink::Off => (None, None),
        FileSink::On(file) => (Some(file), None),
        FileSink::Failed { dir, error } => (None, Some((dir, error))),
    };
    if let Some(file) = &file {
        file.write_line(&header_line());
    }
    let ansi = stdout_ansi(
        io::IsTerminal::is_terminal(&io::stdout()),
        std::env::var_os("NO_COLOR").as_deref(),
    );
    // Already set only in tests that install their own.
    let _ = build_subscriber(&filter, io::stdout, Some(ansi), file).try_init();
    if let Some((dir, error)) = failure {
        tracing::warn!(?dir, %error, "cannot open the log file; logging to standard output only");
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn read(path: &Path) -> String {
        fs::read_to_string(path).unwrap_or_default()
    }

    #[test]
    fn appends_to_an_existing_file() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join(FILE_NAME), "old\n").unwrap();
        let f = RotatingFile::open(dir.path()).unwrap();
        f.write_line("one\n");
        f.write_line("two\n");
        assert_eq!(read(&dir.path().join(FILE_NAME)), "old\none\ntwo\n");
        let f = RotatingFile::open(dir.path()).unwrap();
        f.write_line("three\n");
        assert_eq!(read(&dir.path().join(FILE_NAME)), "old\none\ntwo\nthree\n");
        assert!(!dir.path().join(OLD_FILE_NAME).exists());
    }

    #[test]
    fn rotates_at_the_limit_and_keeps_one_old_file() {
        let dir = tempfile::tempdir().unwrap();
        let f = RotatingFile::with_limit(dir.path(), 10).unwrap();
        f.write_line("aaaa\n");
        f.write_line("bbbb\n");
        // 10 bytes: exactly the limit, no rotation yet.
        assert_eq!(read(&dir.path().join(FILE_NAME)), "aaaa\nbbbb\n");
        f.write_line("cccc\n");
        assert_eq!(read(&dir.path().join(OLD_FILE_NAME)), "aaaa\nbbbb\n");
        assert_eq!(read(&dir.path().join(FILE_NAME)), "cccc\n");
        f.write_line("dddd\n");
        f.write_line("eeee\n");
        assert_eq!(read(&dir.path().join(OLD_FILE_NAME)), "cccc\ndddd\n");
        assert_eq!(read(&dir.path().join(FILE_NAME)), "eeee\n");
        let mut names: Vec<String> = fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        assert_eq!(names, [OLD_FILE_NAME, FILE_NAME]);
        // A line longer than the limit still goes into an empty file.
        let long = "x".repeat(25) + "\n";
        f.write_line(&long);
        assert_eq!(read(&dir.path().join(FILE_NAME)), long);
    }

    /// Two writers on one folder (two processes): each decides on the
    /// file's real length, and one that finds the file already rotated
    /// reopens it instead of rotating again.
    #[test]
    fn a_rotation_by_another_writer_is_not_repeated() {
        let dir = tempfile::tempdir().unwrap();
        let a = RotatingFile::with_limit(dir.path(), 10).unwrap();
        let b = RotatingFile::with_limit(dir.path(), 10).unwrap();
        a.write_line("aaaa\n");
        a.write_line("bbbb\n");
        // `b` wrote nothing itself but sees the full file and rotates it.
        b.write_line("cccc\n");
        assert_eq!(read(&dir.path().join(OLD_FILE_NAME)), "aaaa\nbbbb\n");
        assert_eq!(read(&dir.path().join(FILE_NAME)), "cccc\n");
        // `a` still holds the renamed file: it finds room at the path and
        // reopens instead of rotating again (which would drop `a`'s lines).
        a.write_line("dddd\n");
        assert_eq!(read(&dir.path().join(OLD_FILE_NAME)), "aaaa\nbbbb\n");
        assert_eq!(read(&dir.path().join(FILE_NAME)), "cccc\ndddd\n");
    }

    /// Two live writers on one folder never let the files grow past the
    /// limit (each counts the other's lines too).
    #[test]
    fn two_writers_keep_the_limit() {
        let dir = tempfile::tempdir().unwrap();
        let a = RotatingFile::with_limit(dir.path(), 100).unwrap();
        let b = RotatingFile::with_limit(dir.path(), 100).unwrap();
        let size = |name| fs::metadata(dir.path().join(name)).map_or(0, |m| m.len());
        for i in 0..200 {
            let w = if i % 2 == 0 { &a } else { &b };
            w.write_line(&format!("line {i:04}\n"));
            assert!(size(FILE_NAME) <= 100, "{i}: {}", size(FILE_NAME));
            assert!(size(OLD_FILE_NAME) <= 100, "{i}: {}", size(OLD_FILE_NAME));
        }
        assert!(read(&dir.path().join(FILE_NAME)).ends_with("line 0199\n"));
    }

    #[test]
    fn a_missing_directory_is_reported() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("nope");
        assert!(RotatingFile::open(&missing).is_err());
        assert!(!missing.exists());
    }

    #[test]
    fn an_unwritable_path_never_panics() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("plain");
        fs::write(&file, "x").unwrap();
        // A folder below a regular file can be neither created nor opened.
        let below = file.join("logs");
        assert!(RotatingFile::open(&below).is_err());
        match file_sink(true, || Some(below.clone())) {
            FileSink::Failed { dir, error } => {
                assert_eq!(dir.as_deref(), Some(below.as_path()));
                assert!(!error.is_empty());
            }
            other => panic!("{other:?}"),
        }
        assert!(matches!(
            file_sink(true, || None),
            FileSink::Failed { dir: None, .. }
        ));
        // The folder vanishes while the file is in use: writes and
        // rotations fail quietly.
        let gone = dir.path().join("gone");
        fs::create_dir(&gone).unwrap();
        let f = RotatingFile::with_limit(&gone, 8).unwrap();
        // Windows refuses to remove a folder with an open file.
        let removed = fs::remove_dir_all(&gone).is_ok();
        for _ in 0..200 {
            f.write_line("a line that rotates\n");
        }
        if removed {
            assert!(!gone.exists(), "nothing recreates the folder");
        }
    }

    #[test]
    fn threads_never_mix_bytes_inside_a_line() {
        let dir = tempfile::tempdir().unwrap();
        let f = RotatingFile::open(dir.path()).unwrap();
        let sub = subscriber("info", io::sink, Some(f));
        let dispatch = tracing::Dispatch::new(sub);
        std::thread::scope(|s| {
            for t in 0..8 {
                let dispatch = dispatch.clone();
                s.spawn(move || {
                    tracing::dispatcher::with_default(&dispatch, || {
                        for i in 0..200 {
                            tracing::info!(target: "fake", "thread {t} line {i} {}", "y".repeat(100));
                        }
                    });
                });
            }
        });
        let text = format!(
            "{}{}",
            read(&dir.path().join(OLD_FILE_NAME)),
            read(&dir.path().join(FILE_NAME))
        );
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 8 * 200);
        for line in lines {
            assert!(line.ends_with(&"y".repeat(100)), "{line}");
            assert_eq!(line.matches("thread ").count(), 1, "{line}");
        }
    }

    /// Writers that hand an event over in several pieces (with other
    /// threads running in between) still get whole lines, also while the
    /// files rotate.
    #[test]
    fn a_split_event_stays_one_line() {
        for limit in [MAX_BYTES, 4096] {
            let dir = tempfile::tempdir().unwrap();
            let f = RotatingFile::with_limit(dir.path(), limit).unwrap();
            std::thread::scope(|s| {
                for t in 0..8 {
                    let f = &f;
                    s.spawn(move || {
                        for i in 0..100 {
                            let mut w = f.make_writer();
                            w.write_all(format!("<{t}:{i}|").as_bytes()).unwrap();
                            std::thread::yield_now();
                            w.write_all("z".repeat(50).as_bytes()).unwrap();
                            std::thread::yield_now();
                            w.write_all(format!("|{t}:{i}>\n").as_bytes()).unwrap();
                        }
                    });
                }
            });
            let old = read(&dir.path().join(OLD_FILE_NAME));
            let active = read(&dir.path().join(FILE_NAME));
            let mut count = 0;
            for line in old.lines().chain(active.lines()) {
                count += 1;
                let (head, rest) = line.split_once('|').unwrap();
                let (body, tail) = rest.split_once('|').unwrap();
                assert_eq!(
                    head.trim_start_matches('<'),
                    tail.trim_end_matches('>'),
                    "{line}"
                );
                assert_eq!(body, "z".repeat(50), "{line}");
            }
            if limit == MAX_BYTES {
                assert_eq!(count, 8 * 100);
            } else {
                assert!(!old.is_empty(), "the small limit rotates");
            }
        }
    }

    #[test]
    fn early_read_of_the_preference() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(CONFIG_FILE);
        let cases: [(Option<&str>, bool); 5] = [
            (None, false),
            (Some("not = = toml"), false),
            (Some("[ui]\nlog_file = true\n"), true),
            (Some("[ui]\nlog_file = false\n"), false),
            (Some("[ui]\nlog_file = \"yes\"\n"), false),
        ];
        for (content, expected) in cases {
            match content {
                Some(c) => fs::write(&path, c).unwrap(),
                None => {
                    let _ = fs::remove_file(&path);
                }
            }
            assert_eq!(saved_log_file(Some(dir.path())), expected, "{content:?}");
        }
        assert!(!saved_log_file(None));
        fs::write(&path, "not = = toml").unwrap();
        let _ = saved_log_file(Some(dir.path()));
        assert_eq!(
            read(&path),
            "not = = toml",
            "a broken file is left as it is"
        );
    }

    /// Main's start-up path: with the preference off (or unreadable) the
    /// log folder is never asked for, so nothing can be created; on, the
    /// folder and the file appear.
    #[test]
    fn off_creates_nothing() {
        let config = tempfile::tempdir().unwrap();
        let logs = config.path().join("state").join("logs");
        let never = || -> Option<PathBuf> { panic!("the folder is not asked for") };
        assert!(matches!(start_sink(None, never), FileSink::Off));
        assert!(matches!(
            start_sink(Some(config.path()), never),
            FileSink::Off
        ));
        fs::write(config.path().join(CONFIG_FILE), "[ui]\nlog_file = false\n").unwrap();
        assert!(matches!(
            start_sink(Some(config.path()), never),
            FileSink::Off
        ));
        fs::write(config.path().join(CONFIG_FILE), "[ui]\nlog_file = true\n").unwrap();
        match start_sink(Some(config.path()), || Some(logs.clone())) {
            FileSink::On(_) => {}
            other => panic!("{other:?}"),
        }
        assert!(logs.join(FILE_NAME).exists());
    }

    /// A link or a FIFO planted at the file's path fails the sink instead of
    /// writing through the link (or changing its target's mode) or blocking
    /// the start.
    #[cfg(unix)]
    #[test]
    fn only_a_regular_file_is_opened() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = tempfile::tempdir().unwrap();
        let logs = dir.path().join("logs");
        create_dir(&logs).unwrap();
        let target = dir.path().join("target.txt");
        fs::write(&target, "keep\n").unwrap();
        fs::set_permissions(&target, fs::Permissions::from_mode(0o644)).unwrap();
        std::os::unix::fs::symlink(&target, logs.join(FILE_NAME)).unwrap();
        assert!(matches!(
            file_sink(true, || Some(logs.clone())),
            FileSink::Failed { .. }
        ));
        assert_eq!(read(&target), "keep\n");
        assert_eq!(
            fs::metadata(&target).unwrap().permissions().mode() & 0o777,
            0o644
        );
        #[cfg(target_os = "linux")]
        {
            use rustix::fs::{FileType, Mode};
            fs::remove_file(logs.join(FILE_NAME)).unwrap();
            rustix::fs::mknodat(
                rustix::fs::CWD,
                logs.join(FILE_NAME),
                FileType::Fifo,
                Mode::from_raw_mode(0o600),
                0,
            )
            .unwrap();
            assert!(matches!(
                file_sink(true, || Some(logs.clone())),
                FileSink::Failed { .. }
            ));
        }
    }

    /// A log folder the user replaced by a link keeps its target's mode.
    #[cfg(unix)]
    #[test]
    fn a_linked_folder_keeps_its_mode() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = tempfile::tempdir().unwrap();
        let shared = dir.path().join("shared");
        fs::create_dir(&shared).unwrap();
        fs::set_permissions(&shared, fs::Permissions::from_mode(0o750)).unwrap();
        let logs = dir.path().join("logs");
        std::os::unix::fs::symlink(&shared, &logs).unwrap();
        let FileSink::On(_) = file_sink(true, || Some(logs.clone())) else {
            panic!("no file");
        };
        let mode = |p: &Path| fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&shared), 0o750);
        assert_eq!(mode(&shared.join(FILE_NAME)), 0o600);
    }

    #[cfg(unix)]
    #[test]
    fn folder_and_file_are_private() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = tempfile::tempdir().unwrap();
        let logs = dir.path().join("a").join("logs");
        let FileSink::On(_) = file_sink(true, || Some(logs.clone())) else {
            panic!("no file");
        };
        let mode = |p: &Path| fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&logs), 0o700);
        assert_eq!(mode(&logs.join(FILE_NAME)), 0o600);
        fs::set_permissions(&logs, fs::Permissions::from_mode(0o755)).unwrap();
        fs::set_permissions(logs.join(FILE_NAME), fs::Permissions::from_mode(0o644)).unwrap();
        let FileSink::On(_) = file_sink(true, || Some(logs.clone())) else {
            panic!("no file");
        };
        assert_eq!(mode(&logs), 0o700);
        assert_eq!(mode(&logs.join(FILE_NAME)), 0o600);
    }

    #[test]
    fn log_folder_per_platform() {
        let home = Path::new("/h");
        let local = Path::new("/h/local");
        let state = Path::new("/h/.local/state");
        let cache = Path::new("/h/cache/rustket");
        let all = |p| pick_dir(p, Some(home), Some(local), Some(state), Some(cache));
        assert_eq!(
            all(Platform::Windows),
            Some(local.join("rustket").join("logs"))
        );
        assert_eq!(
            all(Platform::MacOs),
            Some(home.join("Library").join("Logs").join("rustket"))
        );
        assert_eq!(
            all(Platform::Other),
            Some(state.join("rustket").join("logs"))
        );
        assert_eq!(
            pick_dir(Platform::Other, Some(home), Some(local), None, Some(cache)),
            Some(cache.join("logs")),
            "no state directory: the cache location"
        );
        assert_eq!(pick_dir(Platform::Other, None, None, None, None), None);
    }

    #[test]
    fn header_names_version_os_and_flavor_only() {
        let h = header();
        let os = format!("os={}", std::env::consts::OS);
        let arch = format!("arch={}", std::env::consts::ARCH);
        let flavor = format!("flavor={}", flavor());
        assert_eq!(
            h.split(' ').collect::<Vec<_>>(),
            [
                "Rustket",
                env!("CARGO_PKG_VERSION"),
                "started",
                os.as_str(),
                arch.as_str(),
                flavor.as_str()
            ]
        );
        let line = header_line();
        assert!(
            line.ends_with(&format!("  INFO rustket::logging: {h}\n")),
            "{line}"
        );
        assert!(
            line.split(' ').next().unwrap().ends_with('Z'),
            "UTC: {line}"
        );
    }

    /// Colours only on a terminal and never with a non-empty `NO_COLOR`.
    #[test]
    fn colours_only_on_a_terminal() {
        use std::ffi::OsStr;
        assert!(stdout_ansi(true, None));
        assert!(stdout_ansi(true, Some(OsStr::new(""))));
        assert!(!stdout_ansi(true, Some(OsStr::new("1"))));
        assert!(!stdout_ansi(false, None));
        assert!(!stdout_ansi(false, Some(OsStr::new("1"))));
    }

    /// The pins always follow the user's filter.
    #[test]
    fn filter_appends_pins() {
        let f = filter(Some("trace"));
        assert!(f.starts_with(&format!("{QUIET_DEPS},trace,")));
        assert!(f.ends_with(rocketchat::LOG_FILTER_PINS));
        assert!(f.contains("tungstenite=info"));
        assert!(filter(None).starts_with(rocketchat::RECOMMENDED_LOG_FILTER));
        assert!(filter(Some("  ")).starts_with(rocketchat::RECOMMENDED_LOG_FILTER));
        assert!(filter(None).contains("tungstenite=info"));
        assert!(filter(None).contains("iced_winit=warn"));
    }

    /// A [`MakeWriter`] into a shared buffer (the standard output side).
    #[derive(Clone, Default)]
    struct Buffer(Arc<Mutex<Vec<u8>>>);

    impl Write for Buffer {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    impl<'a> MakeWriter<'a> for Buffer {
        type Writer = Buffer;
        fn make_writer(&'a self) -> Buffer {
            self.clone()
        }
    }

    /// Drops ANSI escape sequences (`ESC [ … letter`).
    fn strip_ansi(s: &str) -> String {
        let mut out = String::with_capacity(s.len());
        let mut chars = s.chars();
        while let Some(c) = chars.next() {
            if c == '\u{1b}' {
                for c in chars.by_ref() {
                    if c.is_ascii_alphabetic() {
                        break;
                    }
                }
            } else {
                out.push(c);
            }
        }
        out
    }

    /// The lines without their timestamps.
    fn bodies(text: &str) -> Vec<String> {
        text.lines()
            .map(|l| l.split_once(' ').map_or(l, |(_, rest)| rest).to_owned())
            .collect()
    }

    fn log_every_level() {
        tracing::trace!(target: "fake_module", "trace event");
        tracing::debug!(target: "fake_module", "debug event");
        tracing::info!(target: "fake_module", "info event");
        tracing::warn!(target: "fake_module", "warn event");
        tracing::error!(target: "fake_module", "error event");
        tracing::trace!(target: "tungstenite::protocol", "trace frame resume-token");
        tracing::debug!(target: "tungstenite::protocol", "debug frame");
        tracing::info!(target: "tungstenite::protocol", "info frame");
        tracing::trace!(target: "rustls::client", "trace handshake");
    }

    /// The level of a line in the fmt format (`<time> <LEVEL> …`).
    fn level(line: &str) -> &str {
        line.split_whitespace().nth(1).unwrap_or_default()
    }

    /// Privacy guard: through the configured subscriber at the default
    /// filter, the file has no ANSI escapes and no TRACE lines of the pinned
    /// transport crates; standard output and the file get the same events
    /// from `info` up (one filter), and the file never takes DEBUG or TRACE,
    /// whatever `RUST_LOG` says.
    #[test]
    fn file_and_stdout_share_the_filter_and_pins() {
        for user in [
            None,
            Some("trace"),
            Some("debug"),
            Some("info,tungstenite::protocol=trace"),
        ] {
            let dir = tempfile::tempdir().unwrap();
            let file = RotatingFile::open(dir.path()).unwrap();
            let stdout = Buffer::default();
            let sub = build_subscriber(&filter(user), stdout.clone(), Some(true), Some(file));
            tracing::subscriber::with_default(sub, log_every_level);
            let text = read(&dir.path().join(FILE_NAME));
            assert!(!text.contains('\u{1b}'), "{text}");
            assert!(!text.contains("\\x1b"), "not escaped either: {text}");
            assert!(stdout.0.lock().unwrap().contains(&0x1b), "colours on");
            assert!(!text.contains("resume-token"), "{user:?}: {text}");
            assert!(!text.contains("trace handshake"), "{user:?}: {text}");
            assert!(!text.contains("debug frame"), "{user:?}: {text}");
            assert!(text.contains("info frame"));
            assert!(text.contains("error event"));
            assert!(text.contains("info event"));
            assert!(!text.contains("trace event") && !text.contains("debug event"));
            let out = strip_ansi(&String::from_utf8(stdout.0.lock().unwrap().clone()).unwrap());
            assert_eq!(out.contains("trace event"), user == Some("trace"));
            // A more specific `RUST_LOG` target beats a pin on standard
            // output; the file still drops it.
            assert_eq!(
                out.contains("resume-token"),
                user == Some("info,tungstenite::protocol=trace")
            );
            let loud: String = out
                .lines()
                .filter(|l| !matches!(level(l), "DEBUG" | "TRACE"))
                .map(|l| format!("{l}\n"))
                .collect();
            assert_eq!(bodies(&loud), bodies(&text), "{user:?}");
            for line in text.lines() {
                assert!(line.split(' ').next().unwrap().ends_with('Z'), "{line}");
                assert!(matches!(level(line), "INFO" | "WARN" | "ERROR"), "{line}");
            }
        }
    }

    /// Review J2: span fields. Standard output (colours on) stores its
    /// formatted copy of a span's fields first; the file formats them itself
    /// without colours. A DEBUG span above the file's cap never shows up in
    /// the file, not even as the context of an INFO event.
    #[test]
    fn span_fields_reach_the_file_without_colours() {
        for user in [None, Some("debug")] {
            let dir = tempfile::tempdir().unwrap();
            let file = RotatingFile::open(dir.path()).unwrap();
            let stdout = Buffer::default();
            let sub = build_subscriber(&filter(user), stdout.clone(), Some(true), Some(file));
            tracing::subscriber::with_default(sub, || {
                let outer = tracing::info_span!(target: "fake_module", "sync", server = "srv-one", rooms = 42_u32);
                let _outer = outer.enter();
                let hidden =
                    tracing::debug_span!(target: "fake_module", "frame", secret = "debug-field");
                let _hidden = hidden.enter();
                outer.record("rooms", 43_u32);
                tracing::info!(target: "fake_module", "inside the span");
            });
            let text = read(&dir.path().join(FILE_NAME));
            assert!(!text.as_bytes().contains(&0x1b), "{text:?}");
            assert!(!text.contains("\\x1b"), "not escaped either: {text:?}");
            assert!(
                text.contains("sync{server=\"srv-one\" rooms=42 rooms=43}"),
                "{text}"
            );
            assert!(text.contains("inside the span"), "{text}");
            assert!(
                !text.contains("debug-field") && !text.contains("frame"),
                "{text}"
            );
            let out = stdout.0.lock().unwrap().clone();
            assert!(out.contains(&0x1b), "standard output has colours");
            let out = strip_ansi(&String::from_utf8(out).unwrap());
            assert!(out.contains("srv-one"), "{out}");
            assert_eq!(out.contains("debug-field"), user == Some("debug"), "{out}");
        }
    }

    /// Escape bytes in logged data (a `%display` value, which the field
    /// formatter does not escape) and other control bytes are written as text.
    #[test]
    fn control_bytes_in_data_are_written_as_text() {
        let dir = tempfile::tempdir().unwrap();
        let file = RotatingFile::open(dir.path()).unwrap();
        let sub = build_subscriber("info", io::sink, Some(false), Some(file.clone()));
        tracing::subscriber::with_default(sub, || {
            let span = tracing::info_span!(target: "fake_module", "room", name = %"\u{1b}[31mred");
            let _span = span.enter();
            tracing::info!(target: "fake_module", error = %"bell\u{7}\u{9b}2J", "\u{1b}]0;title\u{7}");
        });
        file.write_line("tab\tcr\rdel\u{7f}\n");
        let text = read(&dir.path().join(FILE_NAME));
        assert!(
            !text
                .bytes()
                .any(|b| (b < 0x20 && b != b'\t' && b != b'\n') || b == 0x7f),
            "{text:?}"
        );
        assert!(!text.contains('\u{9b}'), "{text:?}");
        assert!(text.contains("name=\\x1b[31mred"), "{text}");
        assert!(text.contains("error=bell\\x07\\u{9b}2J"), "{text}");
        assert!(text.ends_with("tab\tcr\\x0ddel\\x7f\n"), "{text}");
        assert_eq!(
            escape_controls(b"plain \xc3\xa9\n"),
            &b"plain \xc3\xa9\n"[..]
        );
    }

    /// Review J2: a handle whose file another process renamed away stops
    /// writing into it; the next line goes into a new active file.
    #[cfg(unix)]
    #[test]
    fn a_renamed_active_file_is_reopened() {
        let dir = tempfile::tempdir().unwrap();
        let f = RotatingFile::open(dir.path()).unwrap();
        f.write_line("before\n");
        let moved = dir.path().join("moved.log");
        fs::rename(dir.path().join(FILE_NAME), &moved).unwrap();
        f.write_line("after\n");
        assert_eq!(read(&moved), "before\n");
        assert_eq!(read(&dir.path().join(FILE_NAME)), "after\n");
    }

    /// Review J2: same for a removed active file.
    #[cfg(unix)]
    #[test]
    fn a_removed_active_file_is_reopened() {
        let dir = tempfile::tempdir().unwrap();
        let f = RotatingFile::open(dir.path()).unwrap();
        f.write_line("before\n");
        fs::remove_file(dir.path().join(FILE_NAME)).unwrap();
        f.write_line("after\n");
        assert_eq!(read(&dir.path().join(FILE_NAME)), "after\n");
        f.write_line("again\n");
        assert_eq!(read(&dir.path().join(FILE_NAME)), "after\nagain\n");
    }

    /// Without the file, standard output gets the same events as before.
    #[test]
    fn stdout_alone_is_unchanged() {
        let stdout = Buffer::default();
        let sub = subscriber(&filter(None), stdout.clone(), None);
        tracing::subscriber::with_default(sub, log_every_level);
        let out = strip_ansi(&String::from_utf8(stdout.0.lock().unwrap().clone()).unwrap());
        assert!(out.contains("info event") && out.contains("error event"));
        assert!(!out.contains("debug event") && !out.contains("resume-token"));
    }
}
