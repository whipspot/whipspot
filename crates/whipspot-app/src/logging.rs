//! The log sink (MD15): the `log` facade (librespot logs through it) into `whipspot.log` in the
//! log directory, the previous run's file kept as `whipspot.1.log`.
//!
//! Levels: targets `whipspot*` at info, everything else (librespot included) at warn. Each line
//! is `<UTC time> <level> <target> <message>`. Our crates log fixed texts only: no `Secret`, login
//! input, client id, library data or provider error text ever reaches a log line. The panic hook
//! logs the place of a panic and never its payload.

use std::fmt::Write as _;
use std::fs::{self, File};
use std::io::Write;
use std::path::Path;
use std::sync::{Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

use log::{Level, LevelFilter, Log, Metadata, Record};

use crate::paths::AppPaths;

const LOG_FILE: &str = "whipspot.log";
const PREVIOUS_FILE: &str = "whipspot.1.log";

/// The most verbose level logged for `target`.
fn level_for(target: &str) -> LevelFilter {
    if target.starts_with("whipspot") {
        LevelFilter::Info
    } else {
        LevelFilter::Warn
    }
}

/// `YYYY-MM-DDTHH:MM:SSZ` for `secs` since the Unix epoch.
fn utc(secs: u64) -> String {
    let days = (secs / 86_400) as i64;
    let rest = secs % 86_400;
    // Civil date from a day count (Howard Hinnant's algorithm), valid for every date we meet.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        rest / 3_600,
        rest % 3_600 / 60,
        rest % 60
    )
}

/// One log line without its newline; line breaks inside the message become spaces, so a message
/// cannot pose as another line.
fn line(secs: u64, level: Level, target: &str, message: &str) -> String {
    let mut out = format!("{} {level} {target} ", utc(secs));
    for c in message.chars() {
        out.push(if c == '\n' || c == '\r' { ' ' } else { c });
    }
    out
}

static LOGGER: OnceLock<FileLogger> = OnceLock::new();

struct FileLogger {
    file: Mutex<File>,
}

impl Log for FileLogger {
    fn enabled(&self, metadata: &Metadata<'_>) -> bool {
        metadata.level() <= level_for(metadata.target())
    }

    fn log(&self, record: &Record<'_>) {
        if !self.enabled(record.metadata()) {
            return;
        }
        let secs = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_secs());
        let mut message = String::new();
        let _ = write!(message, "{}", record.args());
        let mut text = line(secs, record.level(), record.target(), &message);
        text.push('\n');
        let mut file = self.file.lock().unwrap_or_else(|e| e.into_inner());
        // A failed write has nowhere to be reported.
        let _ = file.write_all(text.as_bytes());
    }

    fn flush(&self) {
        let mut file = self.file.lock().unwrap_or_else(|e| e.into_inner());
        let _ = file.flush();
    }
}

/// Makes the log directory, moves the last run's file to `whipspot.1.log` and opens a fresh
/// `whipspot.log`. `None` when any of that fails: the app then runs without a log file.
fn open(dir: &Path) -> Option<File> {
    fs::create_dir_all(dir).ok()?;
    let current = dir.join(LOG_FILE);
    if current.exists() {
        let previous = dir.join(PREVIOUS_FILE);
        // Our own file from the run before; a rename cannot replace it on every OS.
        let _ = fs::remove_file(&previous);
        let _ = fs::rename(&current, &previous);
    }
    File::create(current).ok()
}

/// Installs the logger once, before anything logs. With no `paths`, or when the log directory
/// cannot be made, no logger is installed and no file is written.
pub fn init(paths: Option<&AppPaths>) {
    let Some(file) = paths.and_then(|p| open(&p.log_dir)) else {
        return;
    };
    let logger = LOGGER.get_or_init(|| FileLogger {
        file: Mutex::new(file),
    });
    if log::set_logger(logger).is_err() {
        return;
    }
    log::set_max_level(LevelFilter::Info);
    install_panic_hook();
    log::info!("log started");
}

/// Logs "panic at <file:line>" for a panic and nothing of its payload (which may hold anything).
/// Debug builds still run the default hook, so a developer sees the message on the console.
fn install_panic_hook() {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        match info.location() {
            Some(l) => log::error!("panic at {}:{}", l.file(), l.line()),
            None => log::error!("panic at an unknown place"),
        }
        log::logger().flush();
        if cfg!(debug_assertions) {
            previous(info);
        }
    }));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn utc_formats_known_instants() {
        assert_eq!(utc(0), "1970-01-01T00:00:00Z");
        assert_eq!(utc(1_700_000_000), "2023-11-14T22:13:20Z");
        // A leap day.
        assert_eq!(utc(1_709_164_800), "2024-02-29T00:00:00Z");
    }

    #[test]
    fn levels_follow_the_target() {
        assert_eq!(level_for("whipspot_app"), LevelFilter::Info);
        assert_eq!(level_for("whipspot_spotify::session"), LevelFilter::Info);
        assert_eq!(level_for("librespot_playback::player"), LevelFilter::Warn);
        assert_eq!(level_for("wgpu_core"), LevelFilter::Warn);
    }

    #[test]
    fn a_line_has_time_level_target_message_and_no_line_break() {
        let text = line(0, Level::Warn, "whipspot_app", "one\r\ntwo");
        assert_eq!(text, "1970-01-01T00:00:00Z WARN whipspot_app one  two");
    }

    #[test]
    fn opening_keeps_the_previous_run_as_dot_one() {
        let dir = std::env::temp_dir().join(format!("whipspot-log-rotate-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join(LOG_FILE), b"first run").unwrap();
        fs::write(dir.join(PREVIOUS_FILE), b"older").unwrap();
        let file = open(&dir);
        assert!(file.is_some());
        assert_eq!(fs::read(dir.join(PREVIOUS_FILE)).unwrap(), b"first run");
        assert_eq!(fs::read(dir.join(LOG_FILE)).unwrap(), b"");
        let _ = fs::remove_dir_all(&dir);
    }
}
