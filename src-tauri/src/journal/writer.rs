//! Background writer: one JSONL file per local day, size-capped, pruned by age.
//!
//! Callers only ever `try_send` onto a bounded channel, so a slow or failing
//! disk can never block (or fail) a dictation — a full channel drops the line.
//! All I/O happens on the writer thread; I/O errors are logged (rate-limited)
//! and the file is reopened on the next line.

use super::record::Entry;
use chrono::{Local, NaiveDate, TimeZone};
use log::{debug, warn};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, SyncSender, TrySendError};
use std::time::{Duration, Instant};

/// Lines buffered between the hot path and the writer thread.
const CHANNEL_CAPACITY: usize = 512;
/// Safety valve: a day file never grows past this.
pub(crate) const MAX_BYTES_PER_DAY: u64 = 32 * 1024 * 1024;
/// Minimum spacing between two logged write errors.
const ERROR_LOG_INTERVAL: Duration = Duration::from_secs(60);

enum Msg {
    Entry(Box<Entry>),
    SetRetention(u32),
    #[cfg(test)]
    Flush(SyncSender<()>),
}

/// Handle to the writer thread. Cheap to share; every method is non-blocking
/// except [`JournalWriter::flush`] (tests only).
pub(crate) struct JournalWriter {
    tx: SyncSender<Msg>,
}

impl JournalWriter {
    pub fn spawn(dir: PathBuf, retention_days: u32, max_bytes: u64) -> Self {
        let (tx, rx) = mpsc::sync_channel(CHANNEL_CAPACITY);
        let spawned = std::thread::Builder::new()
            .name("dictation-journal".into())
            .spawn(move || run(rx, DayFiles::new(dir, retention_days, max_bytes)));
        if let Err(err) = spawned {
            // The receiver dropped with the closure: every send fails fast.
            warn!("journal: writer thread failed to start: {}", err);
        }
        Self { tx }
    }

    /// Queue a line. `false` when it was dropped (channel full or writer gone).
    pub fn send(&self, entry: Entry) -> bool {
        match self.tx.try_send(Msg::Entry(Box::new(entry))) {
            Ok(()) => true,
            Err(TrySendError::Full(_)) => {
                debug!("journal: writer busy, line dropped");
                false
            }
            Err(TrySendError::Disconnected(_)) => false,
        }
    }

    pub fn set_retention(&self, days: u32) {
        let _ = self.tx.try_send(Msg::SetRetention(days));
    }

    /// Wait until every line queued before this call was handled.
    #[cfg(test)]
    pub fn flush(&self, timeout: Duration) -> bool {
        let (done_tx, done_rx) = mpsc::sync_channel(1);
        if self.tx.send(Msg::Flush(done_tx)).is_err() {
            return false;
        }
        done_rx.recv_timeout(timeout).is_ok()
    }
}

fn run(rx: Receiver<Msg>, mut files: DayFiles) {
    files.prune(local_day(super::now_ms()));
    let mut last_error_log: Option<Instant> = None;
    for msg in rx {
        match msg {
            Msg::Entry(entry) => {
                let line = match serde_json::to_string(&entry) {
                    Ok(line) => line,
                    Err(err) => {
                        warn!("journal: failed to serialize a line: {}", err);
                        continue;
                    }
                };
                if let Err(err) = files.write_line(local_day(entry.ts), entry.ts, &line) {
                    if last_error_log.is_none_or(|at| at.elapsed() >= ERROR_LOG_INTERVAL) {
                        warn!("journal: write failed ({}): {}", files.dir.display(), err);
                        last_error_log = Some(Instant::now());
                    }
                }
            }
            Msg::SetRetention(days) => {
                files.retention_days = clamp_retention(days);
                files.prune(local_day(super::now_ms()));
            }
            #[cfg(test)]
            Msg::Flush(done) => {
                let _ = done.send(());
            }
        }
    }
}

pub(crate) fn clamp_retention(days: u32) -> u32 {
    days.clamp(1, 365)
}

fn local_day(epoch_ms: u64) -> NaiveDate {
    Local
        .timestamp_millis_opt(epoch_ms as i64)
        .single()
        .map(|dt| dt.date_naive())
        .unwrap_or_else(|| Local::now().date_naive())
}

pub(crate) fn file_name(day: NaiveDate) -> String {
    format!("{}.jsonl", day.format("%Y-%m-%d"))
}

/// The day a journal file name stands for; `None` for any foreign file.
fn parse_file_day(name: &str) -> Option<NaiveDate> {
    let stem = name.strip_suffix(".jsonl")?;
    if stem.len() != 10 {
        return None;
    }
    NaiveDate::parse_from_str(stem, "%Y-%m-%d").ok()
}

/// The open day file plus rotation / retention / cap state.
pub(crate) struct DayFiles {
    dir: PathBuf,
    retention_days: u32,
    max_bytes: u64,
    day: Option<NaiveDate>,
    file: Option<File>,
    bytes: u64,
    capped: bool,
}

impl DayFiles {
    pub fn new(dir: PathBuf, retention_days: u32, max_bytes: u64) -> Self {
        Self {
            dir,
            retention_days: clamp_retention(retention_days),
            max_bytes,
            day: None,
            file: None,
            bytes: 0,
            capped: false,
        }
    }

    /// Append `line` to `day`'s file. Rolls over (and prunes) when the day
    /// changed; past the size cap writes one `cap` marker, then drops lines.
    pub fn write_line(&mut self, day: NaiveDate, ts: u64, line: &str) -> io::Result<()> {
        if self.day != Some(day) {
            self.day = Some(day);
            self.file = None;
            self.capped = false;
            self.prune(day);
        }
        if self.file.is_none() {
            fs::create_dir_all(&self.dir)?;
            let file = OpenOptions::new()
                .create(true)
                .append(true)
                .open(self.dir.join(file_name(day)))?;
            self.bytes = file.metadata().map(|m| m.len()).unwrap_or(0);
            self.file = Some(file);
        }

        let needed = line.len() as u64 + 1;
        if self.bytes + needed > self.max_bytes {
            if self.capped {
                return Ok(());
            }
            self.capped = true;
            let marker = format!(
                "{{\"v\":{},\"ts\":{},\"type\":\"cap\",\"max_bytes\":{}}}",
                super::SCHEMA_VERSION,
                ts,
                self.max_bytes
            );
            return self.append(&marker);
        }
        self.append(line)
    }

    fn append(&mut self, line: &str) -> io::Result<()> {
        let Some(file) = self.file.as_mut() else {
            return Err(io::Error::other("journal file not open"));
        };
        // One write per line keeps lines whole even if a reader tails the file.
        let mut buf = Vec::with_capacity(line.len() + 1);
        buf.extend_from_slice(line.as_bytes());
        buf.push(b'\n');
        match file.write_all(&buf) {
            Ok(()) => {
                self.bytes += buf.len() as u64;
                Ok(())
            }
            Err(err) => {
                // Reopen on the next line (disk freed, dir recreated, …).
                self.file = None;
                Err(err)
            }
        }
    }

    /// Delete day files older than the retention window. Foreign files in the
    /// directory are never touched.
    pub fn prune(&self, today: NaiveDate) -> usize {
        prune_dir(&self.dir, today, self.retention_days)
    }
}

pub(crate) fn prune_dir(dir: &Path, today: NaiveDate, retention_days: u32) -> usize {
    let Ok(entries) = fs::read_dir(dir) else {
        return 0;
    };
    let mut removed = 0;
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(day) = name.to_str().and_then(parse_file_day) else {
            continue;
        };
        if (today - day).num_days() >= i64::from(retention_days)
            && fs::remove_file(entry.path()).is_ok()
        {
            removed += 1;
        }
    }
    if removed > 0 {
        debug!("journal: pruned {} day file(s) past retention", removed);
    }
    removed
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::journal::record::{Event, OverlayRecord};

    fn day(s: &str) -> NaiveDate {
        NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap()
    }

    fn overlay_entry(ts: u64) -> Entry {
        Entry {
            v: crate::journal::SCHEMA_VERSION,
            ts,
            event: Event::Overlay(OverlayRecord {
                id: 7,
                state: "recording".into(),
                phase: "first_frame".into(),
                epoch_ms: ts,
                since_press_ms: 42,
            }),
        }
    }

    #[test]
    fn writes_one_line_per_entry_into_the_day_file() {
        let tmp = tempfile::tempdir().unwrap();
        let mut files = DayFiles::new(tmp.path().to_path_buf(), 90, MAX_BYTES_PER_DAY);
        files.write_line(day("2026-10-03"), 1, "{\"a\":1}").unwrap();
        files.write_line(day("2026-10-03"), 2, "{\"a\":2}").unwrap();
        let content = fs::read_to_string(tmp.path().join("2026-10-03.jsonl")).unwrap();
        assert_eq!(content, "{\"a\":1}\n{\"a\":2}\n");
    }

    #[test]
    fn day_change_rotates_to_a_new_file() {
        let tmp = tempfile::tempdir().unwrap();
        let mut files = DayFiles::new(tmp.path().to_path_buf(), 90, MAX_BYTES_PER_DAY);
        files.write_line(day("2026-10-03"), 1, "x").unwrap();
        files.write_line(day("2026-10-04"), 2, "y").unwrap();
        assert_eq!(
            fs::read_to_string(tmp.path().join("2026-10-03.jsonl")).unwrap(),
            "x\n"
        );
        assert_eq!(
            fs::read_to_string(tmp.path().join("2026-10-04.jsonl")).unwrap(),
            "y\n"
        );
    }

    #[test]
    fn rollover_prunes_files_past_retention_and_keeps_foreign_files() {
        let tmp = tempfile::tempdir().unwrap();
        for name in [
            "2026-07-01.jsonl", // 94 days before 2026-10-03 → pruned
            "2026-07-05.jsonl", // exactly 90 days → pruned
            "2026-07-06.jsonl", // 89 days → kept
            "notes.txt",
            "2026-07-01.jsonl.bak",
        ] {
            fs::write(tmp.path().join(name), "old\n").unwrap();
        }
        let mut files = DayFiles::new(tmp.path().to_path_buf(), 90, MAX_BYTES_PER_DAY);
        files.write_line(day("2026-10-03"), 1, "new").unwrap();
        let mut names: Vec<String> = fs::read_dir(tmp.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
        names.sort();
        assert_eq!(
            names,
            vec![
                "2026-07-01.jsonl.bak",
                "2026-07-06.jsonl",
                "2026-10-03.jsonl",
                "notes.txt"
            ]
        );
    }

    #[test]
    fn size_cap_writes_one_marker_then_drops() {
        let tmp = tempfile::tempdir().unwrap();
        let mut files = DayFiles::new(tmp.path().to_path_buf(), 90, 20);
        files
            .write_line(day("2026-10-03"), 1, "0123456789")
            .unwrap(); // 11 bytes
        files
            .write_line(day("2026-10-03"), 2, "0123456789")
            .unwrap(); // over → marker
        files
            .write_line(day("2026-10-03"), 3, "0123456789")
            .unwrap(); // dropped
        let content = fs::read_to_string(tmp.path().join("2026-10-03.jsonl")).unwrap();
        let lines: Vec<&str> = content.lines().collect();
        assert_eq!(lines.len(), 2);
        assert!(lines[1].contains("\"type\":\"cap\""));
        // A new day starts uncapped.
        files.write_line(day("2026-10-04"), 4, "fresh").unwrap();
        assert_eq!(
            fs::read_to_string(tmp.path().join("2026-10-04.jsonl")).unwrap(),
            "fresh\n"
        );
    }

    #[test]
    fn retention_is_clamped() {
        assert_eq!(clamp_retention(0), 1);
        assert_eq!(clamp_retention(90), 90);
        assert_eq!(clamp_retention(10_000), 365);
    }

    #[test]
    fn writer_serializes_entries_as_versioned_jsonl() {
        let tmp = tempfile::tempdir().unwrap();
        let writer = JournalWriter::spawn(tmp.path().to_path_buf(), 90, MAX_BYTES_PER_DAY);
        let ts = crate::journal::now_ms();
        assert!(writer.send(overlay_entry(ts)));
        assert!(writer.flush(Duration::from_secs(5)));
        let path = tmp.path().join(file_name(local_day(ts)));
        let content = fs::read_to_string(path).unwrap();
        let value: serde_json::Value = serde_json::from_str(content.trim()).unwrap();
        assert_eq!(value["v"], crate::journal::SCHEMA_VERSION);
        assert_eq!(value["type"], "overlay");
        assert_eq!(value["id"], 7);
        assert_eq!(value["since_press_ms"], 42);
    }

    #[test]
    fn writer_never_blocks_or_panics_when_the_disk_fails() {
        let tmp = tempfile::tempdir().unwrap();
        // A regular file where the directory should be: every create/open fails.
        let blocked = tmp.path().join("journal");
        fs::write(&blocked, "not a directory").unwrap();
        let writer = JournalWriter::spawn(blocked.clone(), 90, MAX_BYTES_PER_DAY);
        let started = Instant::now();
        for i in 0..5_000 {
            writer.send(overlay_entry(crate::journal::now_ms() + i));
        }
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "sending must stay non-blocking: {:?}",
            started.elapsed()
        );
        // The writer thread survived every failure and still answers.
        assert!(writer.flush(Duration::from_secs(5)));
        assert!(blocked.is_file());
    }

    #[test]
    fn foreign_names_are_not_journal_days() {
        assert_eq!(parse_file_day("2026-10-03.jsonl"), Some(day("2026-10-03")));
        assert_eq!(parse_file_day("2026-10-3.jsonl"), None);
        assert_eq!(parse_file_day("handy.log"), None);
        assert_eq!(parse_file_day("2026-10-03.json"), None);
    }
}
