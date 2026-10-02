//! Daily-rolling log writer with bounded retention.
//!
//! `tracing_appender::rolling::daily` rotates but never deletes: a long-lived
//! installation accumulates one file per calendar day forever. This module is
//! the drop-in replacement — same `muta.log.YYYY-MM-DD` naming (so existing
//! files keep their place in the sort order), plus a retention sweep on every
//! rollover and once at startup, keeping only the newest
//! [`MAX_LOG_FILES_DEFAULT`] files (override with `MUTA_LOG_RETENTION`).
//!
//! The writer implements [`std::io::Write`] and is handed to
//! `tracing_appender::non_blocking`, exactly like the stock appender was.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// How many rotated log files to keep when `MUTA_LOG_RETENTION` is unset.
pub const MAX_LOG_FILES_DEFAULT: usize = 14;

/// The active file plus the day it belongs to. The day is compared on every
/// write so the rollover happens on the first write past local midnight.
struct Active {
    day: String,
    file: Option<File>,
}

/// A daily-rolling, retention-bounded log file.
pub(crate) struct RetainedRollingFile {
    dir: PathBuf,
    prefix: String,
    max_files: usize,
    active: Mutex<Active>,
}

impl RetainedRollingFile {
    /// Open (or create) today's `prefix.YYYY-MM-DD` in `dir` and prune
    /// leftovers beyond `max_files`.
    pub(crate) fn new(dir: PathBuf, prefix: &str, max_files: usize) -> Self {
        let day = today();
        let file = open_day(&dir, prefix, &day);
        let writer = Self {
            dir,
            prefix: prefix.to_string(),
            max_files,
            active: Mutex::new(Active { day, file }),
        };
        writer.prune();
        writer
    }

    fn prune(&self) {
        prune_rotated(&self.dir, &self.prefix, self.max_files);
    }
}

impl Write for RetainedRollingFile {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let mut active = self.active.lock().unwrap_or_else(|e| e.into_inner());
        // Rollover check on every write: the first write past local midnight
        // closes yesterday's file, opens today's, and sweeps retention.
        let today = today();
        if active.day != today {
            active.day = today;
            let day = active.day.clone();
            active.file = open_day(&self.dir, &self.prefix, &day);
            self.prune();
        }
        match &mut active.file {
            Some(file) => file.write(buf),
            None => Ok(buf.len()), // logging must never take the process down
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        let mut active = self.active.lock().unwrap_or_else(|e| e.into_inner());
        match &mut active.file {
            Some(file) => file.flush(),
            None => Ok(()),
        }
    }
}

/// Local-time `YYYY-MM-DD`, matching `tracing_appender`'s filename component.
fn today() -> String {
    chrono::Local::now().format("%Y-%m-%d").to_string()
}

fn open_day(dir: &Path, prefix: &str, day: &str) -> Option<File> {
    let _ = fs::create_dir_all(dir);
    OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join(format!("{prefix}.{day}")))
        .ok()
}

/// Delete the oldest rotated files beyond `max_files`. Files are selected by
/// the `prefix.` name convention; `YYYY-MM-DD` sorts lexicographically, so a
/// name sort is a chronological sort. Non-matching files are never touched.
fn prune_rotated(dir: &Path, prefix: &str, max_files: usize) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    // Only regular files matching `prefix.` count toward retention: a
    // same-prefix *directory* (or anything else) must neither be deleted nor
    // consume a retention slot — with the naive name sort it would sort
    // between date suffixes and evict a log that should have been kept.
    let mut names: Vec<String> = entries
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            let matches =
                name.starts_with(&format!("{prefix}.")) && e.file_type().is_ok_and(|t| !t.is_dir());
            matches.then_some(name)
        })
        .collect();
    if names.len() <= max_files {
        return;
    }
    names.sort();
    let excess = names.len() - max_files;
    for name in names.into_iter().take(excess) {
        let _ = fs::remove_file(dir.join(name));
    }
}

/// Retention count: `MUTA_LOG_RETENTION` if it parses as `usize >= 1`,
/// otherwise [`MAX_LOG_FILES_DEFAULT`].
pub(crate) fn retention_from_env() -> usize {
    std::env::var("MUTA_LOG_RETENTION")
        .ok()
        .and_then(|v| v.trim().parse::<usize>().ok())
        .filter(|n| *n >= 1)
        .unwrap_or(MAX_LOG_FILES_DEFAULT)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prune_keeps_newest_n_and_never_touches_other_files() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        for day in ["2026-01-01", "2026-01-02", "2026-01-03", "2026-01-04"] {
            fs::write(root.join(format!("muta.log.{day}")), b"x").unwrap();
        }
        // A foreign file and a directory sharing the prefix dir: must survive.
        fs::write(root.join("unrelated.txt"), b"x").unwrap();
        fs::create_dir(root.join("muta.log.subdir")).unwrap();

        prune_rotated(root, "muta.log", 2);

        let mut remaining: Vec<String> = fs::read_dir(root)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        remaining.sort();
        // Keeps the newest 2 rotated files (01-03, 01-04); the foreign file
        // and the same-prefix directory are never touched.
        assert_eq!(
            remaining,
            vec![
                "muta.log.2026-01-03".to_string(),
                "muta.log.2026-01-04".to_string(),
                "muta.log.subdir".to_string(),
                "unrelated.txt".to_string(),
            ]
        );
    }

    #[test]
    fn prune_is_noop_at_or_below_limit() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fs::write(root.join("muta.log.2026-01-01"), b"x").unwrap();
        prune_rotated(root, "muta.log", 3);
        assert!(root.join("muta.log.2026-01-01").exists());
    }

    #[test]
    fn writer_rolls_on_day_change_and_writes_through() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let mut writer = RetainedRollingFile {
            dir: root.to_path_buf(),
            prefix: "muta.log".to_string(),
            max_files: 7,
            active: Mutex::new(Active {
                // Simulate yesterday as the active day so the next write rolls.
                day: "2000-01-01".to_string(),
                file: None,
            }),
        };
        let n = writer.write(b"hello\n").unwrap();
        assert_eq!(n, 6);
        writer.flush().unwrap();
        let today_path = root.join(format!("muta.log.{}", today()));
        let content = fs::read_to_string(&today_path).unwrap();
        assert_eq!(content, "hello\n");
        // The day field advanced, so a second write must not re-roll.
        let day_after = writer.active.lock().unwrap().day.clone();
        assert_eq!(day_after, today());
    }

    #[test]
    fn retention_env_parses_with_fallback() {
        assert_eq!(retention_parse("30"), 30);
        assert_eq!(retention_parse("0"), MAX_LOG_FILES_DEFAULT);
        assert_eq!(retention_parse("junk"), MAX_LOG_FILES_DEFAULT);
        assert_eq!(retention_parse(" 8 "), 8);
    }

    fn retention_parse(raw: &str) -> usize {
        raw.trim()
            .parse::<usize>()
            .ok()
            .filter(|n| *n >= 1)
            .unwrap_or(MAX_LOG_FILES_DEFAULT)
    }
}
