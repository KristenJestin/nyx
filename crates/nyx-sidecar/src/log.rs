//! THE SIDECAR'S LOG FILE — `<data_dir>/logs/sidecar.log`.
//!
//! Until now the sidecar spoke only to `stderr`, which Electron inherits. That is fine while you
//! are watching a terminal and useless the moment you are not: a user reporting "my shell died"
//! has nothing to hand over, and neither does an agent asked to find out why. The point of this
//! module is that the sidecar's own account of what it did OUTLIVES the run that produced it.
//!
//! `eprintln!` IS KEPT EVERYWHERE. This is an addition, not a redirection: the supervisor's live
//! view of its child is unchanged, and [`line`] writes to both. A failure to open or write the
//! file is swallowed — a sidecar must not refuse to serve because it could not log.
//!
//! # No logging crate
//!
//! `std` covers this: one append handle behind a `Mutex`, one size check at boot, one timestamp
//! formatter. `tracing` would buy levels, spans and subscriber plumbing that nothing here asks
//! for, plus a dependency and a build-time cost, to write the same lines to the same file.
//!
//! # Rotation
//!
//! Deliberately the simplest thing that cannot grow without bound: at OPEN, if the file is over
//! [`MAX_LOG_BYTES`], it is renamed to `sidecar.log.1` (replacing any previous `.1`) and a fresh
//! one is started. So there are at most two files and at most one run's worth of overflow, and the
//! check costs one `stat` per process — nothing on the hot path ever looks at the size.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

/// The log file's size ceiling — **5 MiB**, checked once, at open. Past it the current file is
/// rolled to `.1` and a fresh one is started.
pub const MAX_LOG_BYTES: u64 = 5 * 1024 * 1024;

/// The subdirectory of the data dir the log lives in, and the file's name.
const LOG_DIR: &str = "logs";
const LOG_FILE: &str = "sidecar.log";

/// The open append handle, or `None` until [`init`] has run (and forever after, if it failed).
/// A `Mutex` because the PTY pumps, the MCP dispatcher and the control loop all log from their
/// own threads and a torn line would be worse than no line.
static SINK: OnceLock<Option<Mutex<File>>> = OnceLock::new();

/// The path the log is written to, for a `data_dir`.
pub fn log_path(data_dir: &Path) -> PathBuf {
    data_dir.join(LOG_DIR).join(LOG_FILE)
}

/// The path the PREVIOUS log is rolled to.
pub fn rolled_log_path(data_dir: &Path) -> PathBuf {
    data_dir.join(LOG_DIR).join(format!("{LOG_FILE}.1"))
}

/// OPEN the log for this run — create `<data_dir>/logs/`, roll an oversized previous file, and
/// keep an append handle for the life of the process.
///
/// Idempotent by construction (the handle lives in a `OnceLock`), so a second call is a no-op and
/// the FIRST data dir wins. Best-effort: everything it does can fail on a read-only or missing
/// path, and a sidecar that cannot log still serves.
pub fn init(data_dir: &Path) {
    SINK.get_or_init(|| open_sink(data_dir));
}

/// Build the append handle for `data_dir`, rolling first. `None` on any filesystem failure.
fn open_sink(data_dir: &Path) -> Option<Mutex<File>> {
    let path = log_path(data_dir);
    std::fs::create_dir_all(path.parent()?).ok()?;
    roll_if_oversized(&path, &rolled_log_path(data_dir), MAX_LOG_BYTES);
    let file = OpenOptions::new().create(true).append(true).open(&path).ok()?;
    Some(Mutex::new(file))
}

/// Roll `path` onto `rolled` when it is strictly larger than `max_bytes`. Returns whether it
/// rolled. A missing file is not oversized; a rename that fails leaves the log where it was (we
/// would rather keep appending to a big file than lose the run's lines).
///
/// Split out of [`open_sink`] so the rule can be tested on a temp dir without a process-wide sink.
pub fn roll_if_oversized(path: &Path, rolled: &Path, max_bytes: u64) -> bool {
    let Ok(meta) = std::fs::metadata(path) else {
        return false;
    };
    if meta.len() <= max_bytes {
        return false;
    }
    // `rename` REPLACES the destination on Unix but refuses an existing one on Windows, so the
    // previous roll is removed first — otherwise the log would never roll again on Windows.
    let _ = std::fs::remove_file(rolled);
    std::fs::rename(path, rolled).is_ok()
}

/// Write ONE line to `stderr` (as always) and, if the log is open, to the file with a UTC stamp.
///
/// `message` is written as given — call sites keep their `[nyx-sidecar]` prefix, so the file reads
/// exactly like the stderr the supervisor sees, with a date in front.
pub fn line(message: &str) {
    eprintln!("{message}");
    let Some(Some(sink)) = SINK.get() else {
        return;
    };
    append(sink, &stamp(SystemTime::now()), message);
}

/// Append ONE stamped line to an open sink, and FLUSH it.
///
/// Flushed every time on purpose: the lines worth having are the ones written just before
/// something goes wrong, and a buffer that dies with the process would drop exactly those. The
/// volume is a handful of lines per PTY, not a stream.
///
/// Split out of [`line`] so the write can be exercised on a sink built for a temp dir, without
/// the process-wide `SINK` (whose first initialisation wins for the whole run).
fn append(sink: &Mutex<File>, stamp: &str, message: &str) {
    // Poisoning is ignored: a panic while holding the log must not silence every later line.
    let mut file = sink.lock().unwrap_or_else(|e| e.into_inner());
    let _ = writeln!(file, "{stamp} {message}");
    let _ = file.flush();
}

/// `eprintln!` + the log file, in one call. The `[nyx-sidecar]` prefix stays at the call site.
#[macro_export]
macro_rules! slog {
    ($($arg:tt)*) => {
        $crate::log::line(&format!($($arg)*))
    };
}

/// The UTC stamp in front of every logged line — `2026-09-07T18:20:31.123Z`.
fn stamp(now: SystemTime) -> String {
    let d = now.duration_since(UNIX_EPOCH).unwrap_or_default();
    format_utc(d.as_secs(), d.subsec_millis())
}

/// Format a Unix timestamp as RFC-3339 UTC, with `std` alone.
///
/// The date part is the civil-from-days algorithm (Howard Hinnant's, the one every date library
/// implements): shift the epoch to March 1st so the leap day lands at the END of the year, and the
/// month lengths become a regular sequence with no special case for February. Pure and total —
/// which is why it can be pinned by a test rather than trusted.
pub fn format_utc(unix_secs: u64, millis: u32) -> String {
    let days = (unix_secs / 86_400) as i64;
    let secs_of_day = unix_secs % 86_400;
    // Days since 0000-03-01, the epoch this algorithm counts from.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097); // day of era, 0..=146096
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365; // year of era, 0..=399
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // day of year (March-based), 0..=365
    let mp = (5 * doy + 2) / 153; // month, March-based, 0..=11
    let d = doy - (153 * mp + 2) / 5 + 1; // day of month, 1..=31
    let m = if mp < 10 { mp + 3 } else { mp - 9 }; // 1..=12
    let year = if m <= 2 { y + 1 } else { y };
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{:03}Z",
        year,
        m,
        d,
        secs_of_day / 3600,
        (secs_of_day % 3600) / 60,
        secs_of_day % 60,
        millis
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A temp dir of this test's own — the log is a file, and a shared one would make the size
    /// assertions race.
    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "nyx-sidecar-log-{tag}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::create_dir_all(&dir).expect("a temp dir for the log test");
        dir
    }

    /// THE STAMP IS A REAL DATE, not an epoch count: the whole point of the file is to be read
    /// months later. Pinned on known instants, including a leap day (the case the March-shift in
    /// `format_utc` exists for) and the two boundaries.
    #[test]
    fn the_stamp_is_rfc_3339_utc() {
        assert_eq!(format_utc(0, 0), "1970-01-01T00:00:00.000Z");
        // A leap day — the case a naive month table gets wrong.
        assert_eq!(format_utc(1_709_209_496, 7), "2024-02-29T12:24:56.007Z");
        // The last second of a year, and the first of the next.
        assert_eq!(format_utc(1_735_689_599, 999), "2024-12-31T23:59:59.999Z");
        assert_eq!(format_utc(1_735_689_600, 0), "2025-01-01T00:00:00.000Z");
    }

    /// THE ROTATION IS A CEILING, not a policy: under the limit nothing moves, over it the file
    /// is rolled to `.1` and the next open starts clean. A missing file is not oversized.
    #[test]
    fn an_oversized_log_is_rolled_to_dot_one_and_a_small_one_is_left_alone() {
        let dir = temp_dir("roll");
        let path = dir.join("sidecar.log");
        let rolled = dir.join("sidecar.log.1");

        assert!(
            !roll_if_oversized(&path, &rolled, 8),
            "a log that does not exist yet has nothing to roll"
        );

        std::fs::write(&path, b"1234567").expect("a small log");
        assert!(
            !roll_if_oversized(&path, &rolled, 8),
            "a log under the ceiling stays where it is"
        );
        assert!(!rolled.exists());

        std::fs::write(&path, b"123456789").expect("an oversized log");
        assert!(roll_if_oversized(&path, &rolled, 8), "past the ceiling it rolls");
        assert!(!path.exists(), "the current log is moved, not copied");
        assert_eq!(std::fs::read(&rolled).unwrap(), b"123456789");

        // AND IT ROLLS AGAIN. The second roll has to replace the first `.1`, which a bare
        // `rename` refuses to do on Windows — a log that rolls once and then grows for ever is
        // exactly the failure the ceiling exists to prevent.
        std::fs::write(&path, b"abcdefghij").expect("a second oversized log");
        assert!(roll_if_oversized(&path, &rolled, 8), "the roll is repeatable");
        assert_eq!(std::fs::read(&rolled).unwrap(), b"abcdefghij");

        std::fs::remove_dir_all(&dir).ok();
    }

    /// The sink is built from the data dir alone: `<data_dir>/logs/sidecar.log`, directory
    /// created if absent, and it APPENDS (a run never truncates the previous one's tail).
    #[test]
    fn the_sink_appends_under_logs_in_the_data_dir() {
        let dir = temp_dir("sink");
        assert_eq!(log_path(&dir), dir.join("logs").join("sidecar.log"));

        let first = open_sink(&dir).expect("the sink opens on a writable data dir");
        writeln!(first.lock().unwrap(), "one").unwrap();
        drop(first);
        let second = open_sink(&dir).expect("re-opening the same log");
        writeln!(second.lock().unwrap(), "two").unwrap();
        drop(second);

        let body = std::fs::read_to_string(log_path(&dir)).expect("the log exists on disk");
        assert!(
            body.contains("one") && body.contains("two"),
            "a second open must APPEND, not truncate: {body:?}"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    /// A logged line lands IN THE FILE, with its stamp in front and the message untouched — the
    /// whole deliverable in one assertion. (`line` itself reads the process-wide sink, whose first
    /// initialisation wins for the run; the write it delegates to is what is held here.)
    #[test]
    fn a_logged_line_is_stamped_and_flushed_to_the_file() {
        let dir = temp_dir("append");
        let sink = open_sink(&dir).expect("the sink opens");

        append(&sink, "1970-01-01T00:00:00.000Z", "[nyx-sidecar] hello");
        // Read WITHOUT dropping the sink: an unflushed line would be invisible here, which is the
        // failure mode the flush exists for.
        let body = std::fs::read_to_string(log_path(&dir)).expect("the log exists");

        assert_eq!(body, "1970-01-01T00:00:00.000Z [nyx-sidecar] hello\n");

        drop(sink);
        std::fs::remove_dir_all(&dir).ok();
    }
}
