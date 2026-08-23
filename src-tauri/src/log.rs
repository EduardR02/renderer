//! Minimal append-only diagnostic log for the app itself.
//!
//! The app previously logged nowhere: engine lifecycle events (spawn, exit,
//! ready, error, retry) and library failures went to stderr, which is
//! invisible in a GUI launch. This module appends timestamped lines to
//! `%LOCALAPPDATA%\SpotifyRenderer\logs\spotify_renderer.log` with plain
//! `std::fs` — no dependencies, no async, safe to call from the engine
//! reader thread. Timestamps are UTC, matching the engine's own log
//! (`playback_engine.log`), so the two files line up for diagnosis.

use std::fs::OpenOptions;
use std::io::Write;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

/// The log file path once [`init`] has been called; `None` before that (or
/// when the logs directory cannot be created) makes every write a silent
/// no-op.
static LOG_FILE: Mutex<Option<PathBuf>> = Mutex::new(None);

/// Opens `logs/spotify_renderer.log` under the given directory for appends.
/// Idempotent; the last call wins (tests point it at a scratch dir).
pub fn init(logs_dir: PathBuf) {
    let _ = std::fs::create_dir_all(&logs_dir);
    *LOG_FILE.lock().expect("app log lock") = Some(logs_dir.join("spotify_renderer.log"));
}

pub fn info(message: &str) {
    append("INFO", message);
}

pub fn warn(message: &str) {
    append("WARN", message);
}

pub fn error(message: &str) {
    append("ERROR", message);
}

fn append(level: &str, message: &str) {
    // The lock is held across the whole write, not just the path read: two
    // threads appending concurrently (engine supervisor vs. anything else)
    // would otherwise interleave mid-line, because `writeln!` on an unbuffered
    // `File` issues several separate write syscalls per line.
    let _guard = LOG_FILE.lock().expect("app log lock");
    let Some(path) = _guard.as_ref() else {
        return;
    };
    let Ok(mut file) = OpenOptions::new().create(true).append(true).open(path) else {
        return;
    };
    let _ = writeln!(file, "[{}] {level} {message}", format_timestamp());
    let _ = file.flush();
}

/// `YYYY-MM-DD HH:MM:SS.mmm` UTC for now.
pub fn format_timestamp() -> String {
    let duration = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    format_timestamp_utc(duration.as_secs(), duration.subsec_millis())
}

/// Pure civil-date conversion (Howard Hinnant's days-to-civil algorithm) so
/// tests can pin the exact rendering.
fn format_timestamp_utc(secs: u64, millis: u32) -> String {
    let days = (secs / 86_400) as i64;
    let seconds_of_day = secs % 86_400;
    let hours = seconds_of_day / 3_600;
    let minutes = (seconds_of_day % 3_600) / 60;
    let seconds = seconds_of_day % 60;
    let (year, month, day) = civil_from_days(days);
    format!(
        "{year:04}-{month:02}-{day:02} {hours:02}:{minutes:02}:{seconds:02}.{millis:03}"
    )
}

/// Days since the Unix epoch -> (year, month, day), Gregorian, proleptic.
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if month <= 2 { year + 1 } else { year };
    (year, month as u32, day as u32)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timestamp_renders_civil_utc_time() {
        assert_eq!(format_timestamp_utc(0, 0), "1970-01-01 00:00:00.000");
        assert_eq!(format_timestamp_utc(86_400, 5), "1970-01-02 00:00:00.005");
        // 2000-02-29: leap day across a century boundary.
        assert_eq!(format_timestamp_utc(951_782_400, 0), "2000-02-29 00:00:00.000");
        // 2024-07-03 09:46:40 UTC.
        assert_eq!(format_timestamp_utc(1_720_000_000, 999), "2024-07-03 09:46:40.999");
        assert_eq!(
            format_timestamp_utc(1_719_964_800 + 86_399, 1),
            "2024-07-03 23:59:59.001"
        );
    }

    #[test]
    fn append_writes_timestamped_lines_to_the_configured_file() {
        let dir = std::env::temp_dir().join(format!(
            "spotify-renderer-log-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.subsec_nanos())
                .unwrap_or(0)
        ));
        init(dir.clone());
        info("engine spawned (pid 123)");
        warn("engine exited; respawning in 2s");
        let contents = std::fs::read_to_string(dir.join("spotify_renderer.log"))
            .expect("log file written");
        // Other tests (the engine round-trip) may append to the configured
        // log concurrently, so assert presence, not an exact line count.
        assert!(
            contents.contains("INFO engine spawned (pid 123)"),
            "info line present: {contents}"
        );
        assert!(
            contents.contains("WARN engine exited; respawning in 2s"),
            "warn line present: {contents}"
        );
        for line in contents.lines() {
            assert!(
                line.starts_with('['),
                "every line is timestamped: {line:?}"
            );
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
