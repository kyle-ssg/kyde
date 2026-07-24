//! Local history — IntelliJ-style per-file snapshots, independent of git (issue #7).
//!
//! Every store lives in its own per-project directory (see [`project_dir`]) holding:
//!
//! - `blobs/<aa>/<rest-of-hash>` — **content-addressed** snapshot bodies (SHA-256).
//!   Identical content is stored once, ever; re-saving an unchanged file writes zero
//!   bytes. Blobs are written to a temp file then `rename`d, so a crash can never
//!   leave a half-written blob under its final name.
//! - `events.jsonl` — an **append-only** journal, one JSON event per line
//!   (`ts_ms`, `path`, `hash`, `kind`, optional `label`). Append-only means a crash
//!   at worst loses the line being written; corrupt/foreign lines are skipped on
//!   load, never fatal.
//!
//! Pruning ([`Store::prune`]) drops events older than a retention window, rewrites
//! the journal atomically (temp + rename) and garbage-collects unreferenced blobs.
//! A `prune.lock` file keeps two Kyde instances from pruning the same store at once.
//!
//! The crate is pure model code: no timers, no threads, no UI. Timestamps are passed
//! in by the caller (testable); throttling/debouncing is the app's policy.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::io::Write as _;
use std::path::{Path, PathBuf};

/// Errors from the local-history store.
#[derive(Debug, thiserror::Error)]
pub enum HistoryError {
    /// An underlying filesystem operation failed; the string says which.
    #[error("local history: {context}: {source}")]
    Io {
        /// What the store was doing (e.g. `"appending event journal"`).
        context: String,
        /// The failing IO error.
        #[source]
        source: std::io::Error,
    },
    /// A snapshot body referenced by the journal is missing from `blobs/`.
    #[error("local history: snapshot {0} is missing")]
    MissingBlob(String),
}

type Result<T> = std::result::Result<T, HistoryError>;

fn io_err(context: impl Into<String>) -> impl FnOnce(std::io::Error) -> HistoryError {
    let context = context.into();
    move |source| HistoryError::Io { context, source }
}

/// What produced a snapshot — drives the timeline row label.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EventKind {
    /// A regular in-app save (autosave or ⌘S).
    Change,
    /// The file changed on disk outside Kyde (detected on open/reload).
    External,
    /// A labeled system snapshot ("Before rollback", "Commit: …", …).
    Label,
}

/// One timeline entry: `path` had content `hash` at `ts_ms`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Event {
    /// Milliseconds since the Unix epoch, supplied by the caller.
    pub ts_ms: u64,
    /// Project-relative path of the snapshotted file.
    pub path: PathBuf,
    /// SHA-256 (hex) of the snapshot body in `blobs/`.
    pub hash: String,
    /// What produced this snapshot.
    pub kind: EventKind,
    /// Human label for [`EventKind::Label`] events (e.g. `"Before checkout main"`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}

/// Counters returned by [`Store::prune`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PruneStats {
    /// Journal events dropped (older than the retention window).
    pub events_dropped: usize,
    /// Unreferenced blob files deleted.
    pub blobs_deleted: usize,
}

/// SHA-256 of `content`, lowercase hex — the store's content address.
///
/// ```
/// let h = kyde_local_history::content_hash("hello\n");
/// assert_eq!(h.len(), 64);
/// assert_eq!(h, kyde_local_history::content_hash("hello\n"));
/// assert_ne!(h, kyde_local_history::content_hash("hello"));
/// ```
#[must_use]
pub fn content_hash(content: &str) -> String {
    let mut hex = String::with_capacity(64);
    for b in Sha256::digest(content.as_bytes()) {
        // Infallible: writing hex digits into a String cannot fail.
        let _ = std::fmt::Write::write_fmt(&mut hex, format_args!("{b:02x}"));
    }
    hex
}

/// The per-project store directory under `base`: `<project-dir-name>-<hash-of-full-path>`.
/// Readable (the name survives) yet collision-free across same-named projects.
///
/// ```
/// use std::path::Path;
/// let base = Path::new("/data");
/// let a = kyde_local_history::project_dir(base, Path::new("/w/alpha"));
/// let b = kyde_local_history::project_dir(base, Path::new("/x/alpha"));
/// assert_ne!(a, b, "same dir name, different projects");
/// assert_eq!(a, kyde_local_history::project_dir(base, Path::new("/w/alpha")), "stable");
/// ```
#[must_use]
pub fn project_dir(base: &Path, project_root: &Path) -> PathBuf {
    // FNV-1a over the full path — tiny, dependency-free, and stability matters more
    // than crypto strength here (content addressing is what uses SHA-256).
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in project_root.to_string_lossy().as_bytes() {
        h ^= u64::from(*b);
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    let name = project_root
        .file_name()
        .map_or_else(|| "root".to_string(), |n| n.to_string_lossy().into_owned());
    // Keep the dir name filesystem-safe: path separators and other exotica out.
    let safe: String = name
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '-' || c == '_' || c == '.' {
                c
            } else {
                '_'
            }
        })
        .collect();
    base.join(format!("{safe}-{h:016x}"))
}

/// The default base directory for all projects' local history:
/// `$XDG_DATA_HOME/kyde/local-history`, falling back to `~/.local/share`.
#[must_use]
pub fn default_base_dir() -> PathBuf {
    let base = std::env::var("XDG_DATA_HOME").map_or_else(
        |_| {
            let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
            PathBuf::from(home).join(".local").join("share")
        },
        PathBuf::from,
    );
    base.join("kyde").join("local-history")
}

/// Format a Unix-epoch millisecond timestamp as `DD/MM/YYYY HH:MM` in a timezone
/// `tz_offset_minutes` east of UTC (the caller supplies the local offset — this
/// crate stays clock- and libc-free).
///
/// ```
/// // 2026-07-24 13:27 UTC, shown in UTC+1:
/// assert_eq!(kyde_local_history::format_ts(1_784_899_620_000, 60), "24/07/2026 14:27");
/// assert_eq!(kyde_local_history::format_ts(0, 0), "01/01/1970 00:00");
/// ```
#[must_use]
pub fn format_ts(ts_ms: u64, tz_offset_minutes: i32) -> String {
    let secs = i64::try_from(ts_ms / 1000).unwrap_or(i64::MAX) + i64::from(tz_offset_minutes) * 60;
    let days = secs.div_euclid(86_400);
    let tod = secs.rem_euclid(86_400);
    let (hour, minute) = (tod / 3600, (tod % 3600) / 60);
    // Civil-from-days (Howard Hinnant's algorithm) — days since 1970-01-01 → y/m/d.
    let shifted = days + 719_468;
    let era = shifted.div_euclid(146_097);
    let doe = shifted.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!("{day:02}/{month:02}/{year:04} {hour:02}:{minute:02}")
}

/// Human relative time between `now_ms` and an earlier `ts_ms` ("just now",
/// "5 min ago", "3 h ago", "2 days ago").
///
/// ```
/// assert_eq!(kyde_local_history::relative_ts(100_000, 70_000), "just now");
/// assert_eq!(kyde_local_history::relative_ts(600_000, 0), "10 min ago");
/// ```
#[must_use]
pub fn relative_ts(now_ms: u64, ts_ms: u64) -> String {
    let s = now_ms.saturating_sub(ts_ms) / 1000;
    match s {
        0..=59 => "just now".to_string(),
        60..=3599 => format!("{} min ago", s / 60),
        3600..=86_399 => format!("{} h ago", s / 3600),
        _ => {
            let days = s / 86_400;
            if days == 1 {
                "yesterday".to_string()
            } else {
                format!("{days} days ago")
            }
        }
    }
}

/// One project's local history: the journal index in memory + blobs on disk.
#[derive(Debug)]
pub struct Store {
    dir: PathBuf,
    events: Vec<Event>,
    /// Hash of the most recent event per path — the dedup check.
    last_by_path: HashMap<PathBuf, String>,
}

impl Store {
    /// Open (creating if needed) the store rooted at `dir`, loading the journal.
    /// Corrupt or foreign journal lines are skipped — never fatal.
    pub fn open(dir: PathBuf) -> Result<Self> {
        std::fs::create_dir_all(dir.join("blobs"))
            .map_err(io_err(format!("creating store dir {dir:?}")))?;
        let mut events: Vec<Event> = Vec::new();
        if let Ok(raw) = std::fs::read_to_string(dir.join("events.jsonl")) {
            events.extend(
                raw.lines()
                    .filter_map(|l| serde_json::from_str::<Event>(l).ok()),
            );
        }
        let mut last_by_path = HashMap::new();
        for e in &events {
            last_by_path.insert(e.path.clone(), e.hash.clone());
        }
        Ok(Self {
            dir,
            events,
            last_by_path,
        })
    }

    /// Open the store for `project_root` under the default base dir.
    pub fn for_project(project_root: &Path) -> Result<Self> {
        Self::open(project_dir(&default_base_dir(), project_root))
    }

    /// Record a snapshot of `path` with `content` at `now_ms`. Returns `Ok(false)`
    /// (and writes nothing) when a `Change`/`External` snapshot matches the path's
    /// latest recorded content — saving an unchanged file is free. `Label` events
    /// always append: their value is the timeline *marker*, even at unchanged content
    /// (the blob is still deduped).
    pub fn record(
        &mut self,
        path: &Path,
        content: &str,
        kind: EventKind,
        label: Option<String>,
        now_ms: u64,
    ) -> Result<bool> {
        let hash = content_hash(content);
        if kind != EventKind::Label && self.last_by_path.get(path) == Some(&hash) {
            return Ok(false);
        }
        self.write_blob(&hash, content)?;
        let event = Event {
            ts_ms: now_ms,
            path: path.to_path_buf(),
            hash: hash.clone(),
            kind,
            label,
        };
        self.append_journal(&event)?;
        self.last_by_path.insert(event.path.clone(), hash);
        self.events.push(event);
        Ok(true)
    }

    /// All events for `path`, newest first.
    #[must_use]
    pub fn events_for(&self, path: &Path) -> Vec<Event> {
        let mut v: Vec<Event> = self
            .events
            .iter()
            .filter(|e| e.path == path)
            .cloned()
            .collect();
        v.reverse();
        v
    }

    /// Hash of the latest snapshot of `path`, if any — the external-change check.
    #[must_use]
    pub fn last_hash(&self, path: &Path) -> Option<&str> {
        self.last_by_path.get(path).map(String::as_str)
    }

    /// Total number of events in the journal.
    #[must_use]
    pub fn event_count(&self) -> usize {
        self.events.len()
    }

    /// Read a snapshot body back by its content hash.
    pub fn content(&self, hash: &str) -> Result<String> {
        std::fs::read_to_string(self.blob_path(hash))
            .map_err(|_| HistoryError::MissingBlob(hash.to_string()))
    }

    /// Drop events older than `retention_ms` (relative to `now_ms`), rewrite the
    /// journal atomically, and delete blobs no kept event references. Skipped
    /// (returning zeros) when another instance holds the prune lock.
    pub fn prune(&mut self, now_ms: u64, retention_ms: u64) -> Result<PruneStats> {
        let cutoff = now_ms.saturating_sub(retention_ms);
        if self.events.iter().all(|e| e.ts_ms >= cutoff) {
            return Ok(PruneStats::default()); // nothing to do — don't touch the disk
        }
        let Some(_lock) = PruneLock::acquire(&self.dir) else {
            return Ok(PruneStats::default());
        };
        let (kept, dropped): (Vec<Event>, Vec<Event>) =
            self.events.drain(..).partition(|e| e.ts_ms >= cutoff);
        let events_dropped = dropped.len();
        self.events = kept;

        // Atomic journal rewrite: full new contents to a temp file, then rename over.
        let mut out = String::new();
        for e in &self.events {
            // Serializing a plain struct of strings/ints cannot fail.
            if let Ok(line) = serde_json::to_string(e) {
                out.push_str(&line);
                out.push('\n');
            }
        }
        let tmp = self.dir.join("events.jsonl.tmp");
        std::fs::write(&tmp, out).map_err(io_err("writing pruned journal"))?;
        std::fs::rename(&tmp, self.dir.join("events.jsonl"))
            .map_err(io_err("replacing journal"))?;

        // Blob GC: anything no kept event references goes.
        let referenced: std::collections::HashSet<&str> =
            self.events.iter().map(|e| e.hash.as_str()).collect();
        let mut blobs_deleted = 0;
        let blobs = self.dir.join("blobs");
        if let Ok(shards) = std::fs::read_dir(&blobs) {
            for shard in shards.flatten() {
                let Ok(files) = std::fs::read_dir(shard.path()) else {
                    continue;
                };
                for f in files.flatten() {
                    let hash = format!(
                        "{}{}",
                        shard.file_name().to_string_lossy(),
                        f.file_name().to_string_lossy()
                    );
                    if !referenced.contains(hash.as_str()) && std::fs::remove_file(f.path()).is_ok()
                    {
                        blobs_deleted += 1;
                    }
                }
            }
        }
        // Rebuild the dedup index — the latest event for a path may have been dropped.
        self.last_by_path.clear();
        for e in &self.events {
            self.last_by_path.insert(e.path.clone(), e.hash.clone());
        }
        Ok(PruneStats {
            events_dropped,
            blobs_deleted,
        })
    }

    fn blob_path(&self, hash: &str) -> PathBuf {
        let (shard, rest) = hash.split_at(2.min(hash.len()));
        self.dir.join("blobs").join(shard).join(rest)
    }

    /// Write a blob if absent: temp file + rename, so a crash mid-write can never
    /// leave truncated content under the final content-addressed name.
    fn write_blob(&self, hash: &str, content: &str) -> Result<()> {
        let path = self.blob_path(hash);
        if path.exists() {
            return Ok(()); // content-addressed → identical by construction
        }
        let parent = path.parent().unwrap_or(&self.dir);
        std::fs::create_dir_all(parent).map_err(io_err("creating blob shard"))?;
        // Unique-enough temp name: pid guards against a concurrent instance writing
        // the same blob (both write, one rename wins — same bytes either way).
        let tmp = parent.join(format!(".tmp-{}-{hash}", std::process::id()));
        std::fs::write(&tmp, content).map_err(io_err("writing snapshot blob"))?;
        std::fs::rename(&tmp, &path).map_err(io_err("publishing snapshot blob"))?;
        Ok(())
    }

    fn append_journal(&self, event: &Event) -> Result<()> {
        let line = serde_json::to_string(event)
            .map_err(|e| io_err("encoding event")(std::io::Error::other(e)))?;
        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .create(true)
            .open(self.dir.join("events.jsonl"))
            .map_err(io_err("opening event journal"))?;
        // One write call per line: on POSIX, small O_APPEND writes land intact even
        // with a second Kyde instance appending to the same journal.
        f.write_all(format!("{line}\n").as_bytes())
            .map_err(io_err("appending event journal"))?;
        Ok(())
    }
}

/// Held for the duration of a prune; the file is removed on drop. A leftover lock
/// older than ten minutes (crash) is stolen.
struct PruneLock(PathBuf);

impl PruneLock {
    fn acquire(dir: &Path) -> Option<Self> {
        let path = dir.join("prune.lock");
        if std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .is_ok()
        {
            return Some(Self(path));
        }
        // Stale (crashed holder)? Steal locks older than 10 minutes.
        let stale = std::fs::metadata(&path)
            .and_then(|m| m.modified())
            .ok()
            .and_then(|m| m.elapsed().ok())
            .is_some_and(|age| age.as_secs() > 600);
        if stale && std::fs::remove_file(&path).is_ok() {
            Self::acquire(dir)
        } else {
            None
        }
    }
}

impl Drop for PruneLock {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_store(tag: &str) -> Store {
        static SEQ: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let seq = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("kyde-lh-{tag}-{}-{seq}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        Store::open(dir).unwrap()
    }

    #[test]
    fn record_roundtrips_content_and_orders_newest_first() {
        let mut s = tmp_store("roundtrip");
        let p = Path::new("src/a.rs");
        assert!(s.record(p, "v1", EventKind::Change, None, 1000).unwrap());
        assert!(s
            .record(p, "v2 — unicode ✓", EventKind::Change, None, 2000)
            .unwrap());
        let ev = s.events_for(p);
        assert_eq!(ev.len(), 2);
        assert_eq!(ev[0].ts_ms, 2000, "newest first");
        assert_eq!(s.content(&ev[0].hash).unwrap(), "v2 — unicode ✓");
        assert_eq!(s.content(&ev[1].hash).unwrap(), "v1");
        assert!(s.events_for(Path::new("other")).is_empty());
    }

    #[test]
    fn unchanged_content_is_deduped_but_labels_always_append() {
        let mut s = tmp_store("dedup");
        let p = Path::new("a.txt");
        assert!(s.record(p, "same", EventKind::Change, None, 1).unwrap());
        assert!(
            !s.record(p, "same", EventKind::Change, None, 2).unwrap(),
            "an unchanged save writes nothing"
        );
        assert!(
            !s.record(p, "same", EventKind::External, None, 3).unwrap(),
            "an 'external change' to identical content is no change"
        );
        assert!(
            s.record(
                p,
                "same",
                EventKind::Label,
                Some("Before rollback".into()),
                4
            )
            .unwrap(),
            "a label is a timeline marker — it appends even at unchanged content"
        );
        assert_eq!(s.event_count(), 2);
        // Same content twice → exactly one blob on disk.
        let ev = s.events_for(p);
        assert_eq!(ev[0].hash, ev[1].hash);
    }

    #[test]
    fn journal_persists_across_reopen() {
        let mut s = tmp_store("reopen");
        let dir = s.dir.clone();
        let p = Path::new("x/y.ts");
        s.record(p, "one", EventKind::Change, None, 10).unwrap();
        s.record(p, "two", EventKind::Label, Some("Commit: init".into()), 20)
            .unwrap();
        drop(s);
        let s2 = Store::open(dir).unwrap();
        let ev = s2.events_for(p);
        assert_eq!(ev.len(), 2);
        assert_eq!(ev[0].label.as_deref(), Some("Commit: init"));
        assert_eq!(ev[0].kind, EventKind::Label);
        assert_eq!(s2.content(&ev[1].hash).unwrap(), "one");
        assert_eq!(s2.last_hash(p), Some(content_hash("two").as_str()));
    }

    #[test]
    fn corrupt_journal_lines_are_skipped_not_fatal() {
        let mut s = tmp_store("corrupt");
        let dir = s.dir.clone();
        s.record(Path::new("a"), "good", EventKind::Change, None, 1)
            .unwrap();
        drop(s);
        // Simulate a crash mid-append + foreign garbage.
        let j = dir.join("events.jsonl");
        let mut raw = std::fs::read_to_string(&j).unwrap();
        raw.push_str("{\"ts_ms\":2,\"path\":\"b\",\"hash\":\"trunc");
        raw.push_str("\nnot json at all\n");
        std::fs::write(&j, raw).unwrap();
        let s2 = Store::open(dir).unwrap();
        assert_eq!(s2.event_count(), 1, "only the intact line survives");
    }

    #[test]
    fn prune_drops_old_events_and_orphaned_blobs_only() {
        let mut s = tmp_store("prune");
        let (a, b) = (Path::new("a"), Path::new("b"));
        s.record(a, "old-only", EventKind::Change, None, 1_000)
            .unwrap();
        s.record(b, "shared", EventKind::Change, None, 2_000)
            .unwrap();
        // Same content recorded again INSIDE the window (label forces the append).
        s.record(b, "shared", EventKind::Label, Some("mark".into()), 90_000)
            .unwrap();
        s.record(a, "fresh", EventKind::Change, None, 95_000)
            .unwrap();

        let stats = s.prune(100_000, 20_000).unwrap(); // cutoff = 80_000
        assert_eq!(stats.events_dropped, 2);
        assert_eq!(stats.blobs_deleted, 1, "only the truly orphaned blob goes");
        assert_eq!(s.event_count(), 2);
        assert_eq!(
            s.content(&content_hash("shared")).unwrap(),
            "shared",
            "a blob still referenced by a kept event survives"
        );
        assert!(
            s.content(&content_hash("old-only")).is_err(),
            "the orphaned blob is gone"
        );
        // The journal on disk matches the pruned state (atomic rewrite).
        let reopened = Store::open(s.dir.clone()).unwrap();
        assert_eq!(reopened.event_count(), 2);
    }

    #[test]
    fn prune_within_retention_is_a_noop_that_never_touches_disk() {
        let mut s = tmp_store("prune-noop");
        s.record(Path::new("a"), "x", EventKind::Change, None, 50)
            .unwrap();
        let stats = s.prune(100, 200).unwrap();
        assert_eq!(stats, PruneStats::default());
        assert_eq!(s.event_count(), 1);
    }

    #[test]
    fn prune_skips_when_lock_held_and_steals_stale_locks() {
        let mut s = tmp_store("prune-lock");
        s.record(Path::new("a"), "old", EventKind::Change, None, 1)
            .unwrap();
        // A live lock (fresh mtime) blocks the prune.
        std::fs::write(s.dir.join("prune.lock"), "").unwrap();
        let stats = s.prune(1_000_000, 10).unwrap();
        assert_eq!(stats, PruneStats::default(), "held lock → skip");
        assert_eq!(s.event_count(), 1);
        std::fs::remove_file(s.dir.join("prune.lock")).unwrap();
        let stats = s.prune(1_000_000, 10).unwrap();
        assert_eq!(stats.events_dropped, 1, "lock released → prune runs");
        assert!(
            !s.dir.join("prune.lock").exists(),
            "the lock is removed after the prune"
        );
    }

    #[test]
    fn project_dirs_are_stable_unique_and_filesystem_safe() {
        let base = Path::new("/base");
        let a = project_dir(base, Path::new("/w1/proj"));
        let b = project_dir(base, Path::new("/w2/proj"));
        assert_ne!(a, b);
        assert_eq!(a, project_dir(base, Path::new("/w1/proj")));
        let odd = project_dir(base, Path::new("/w/ha ha/we:ird"));
        let name = odd.file_name().unwrap().to_string_lossy().into_owned();
        assert!(
            name.chars()
                .all(|c| c.is_alphanumeric() || "-_.".contains(c)),
            "dir name must be filesystem-safe, got {name}"
        );
    }

    #[test]
    fn format_ts_handles_offsets_leap_years_and_midnight() {
        // 2026-07-24 13:27:00 UTC.
        let ts = 1_784_899_620_000;
        assert_eq!(format_ts(ts, 0), "24/07/2026 13:27");
        assert_eq!(format_ts(ts, 60), "24/07/2026 14:27");
        assert_eq!(format_ts(ts, -330), "24/07/2026 07:57", "negative offsets");
        // Offset crossing midnight moves the DATE too.
        assert_eq!(format_ts(0, -60), "31/12/1969 23:00");
        // 2024-02-29 12:00 UTC — leap day survives the civil conversion.
        assert_eq!(format_ts(1_709_208_000_000, 0), "29/02/2024 12:00");
    }

    #[test]
    fn relative_ts_buckets() {
        let now = 10 * 86_400_000;
        assert_eq!(relative_ts(now, now), "just now");
        assert_eq!(relative_ts(now, now - 59_000), "just now");
        assert_eq!(relative_ts(now, now - 60_000), "1 min ago");
        assert_eq!(relative_ts(now, now - 2 * 3_600_000), "2 h ago");
        assert_eq!(relative_ts(now, now - 86_400_000), "yesterday");
        assert_eq!(relative_ts(now, now - 3 * 86_400_000), "3 days ago");
        assert_eq!(relative_ts(0, 1000), "just now", "clock skew never panics");
    }

    /// Perf guard (see CLAUDE.md "Performance regression tests"): recording runs on
    /// the save path (debounced, but still per-burst) — hashing + one blob write for
    /// a ~4000-line file must stay far under interactive budgets. Loose budget on
    /// purpose: this catches algorithmic blowups, not CI jitter.
    #[test]
    fn perf_record_large_file_stays_fast() {
        let mut s = tmp_store("perf-record");
        let big: String = (0..4000)
            .map(|i| format!("fn line_{i}() {{ let v = {i}; }}\n"))
            .collect();
        let t0 = std::time::Instant::now();
        for i in 0..20 {
            s.record(
                Path::new("big.rs"),
                &format!("{big}// rev {i}\n"),
                EventKind::Change,
                None,
                i,
            )
            .unwrap();
        }
        let dt = t0.elapsed();
        assert!(dt.as_secs_f64() < 2.0, "20 large records took {dt:?}");
    }

    /// Perf guard: opening a store replays the whole journal — 10k events (a heavy
    /// week at default retention) must load well under interactive budgets.
    #[test]
    fn perf_load_10k_event_journal_stays_fast() {
        let mut s = tmp_store("perf-load");
        let dir = s.dir.clone();
        s.record(Path::new("seed"), "seed", EventKind::Change, None, 0)
            .unwrap();
        drop(s);
        let mut raw = String::new();
        for i in 0..10_000u64 {
            let e = Event {
                ts_ms: i,
                path: PathBuf::from(format!("src/file_{}.rs", i % 200)),
                hash: content_hash(&format!("{i}")),
                kind: EventKind::Change,
                label: None,
            };
            raw.push_str(&serde_json::to_string(&e).unwrap());
            raw.push('\n');
        }
        std::fs::write(dir.join("events.jsonl"), raw).unwrap();
        let t0 = std::time::Instant::now();
        let s2 = Store::open(dir).unwrap();
        let dt = t0.elapsed();
        assert_eq!(s2.event_count(), 10_000);
        assert!(dt.as_secs_f64() < 2.0, "10k-event journal load took {dt:?}");
    }
}
