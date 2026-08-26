use std::collections::VecDeque;
use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use spotify_playback_engine::protocol::{HistoryItem, HistoryRow, TrackRef};

const HISTORY_FILE: &str = "listening_history.json";
const HISTORY_VERSION: u32 = 1;
const MAX_HISTORY_ITEMS: usize = 2_000;

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct PersistedActive {
    item: HistoryItem,
    duration_ms: u32,
}

struct ActivePlay {
    persisted: PersistedActive,
    playing_since: Option<Instant>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct StoredHistory {
    version: u32,
    finalized: VecDeque<HistoryItem>,
    active: Option<PersistedActive>,
}

#[derive(Serialize)]
struct StoredHistoryRef<'a> {
    version: u32,
    finalized: &'a VecDeque<HistoryItem>,
    active: Option<&'a PersistedActive>,
}

/// A bounded, memory-resident listening history backed by one atomic snapshot.
///
/// The file always contains finalized rows and the recoverable active row
/// together. It is loaded once; reads never revisit disk. An unreadable or
/// invalid snapshot leaves the store read-only so a later playback event cannot
/// replace user data with a fresh empty history.
pub struct ListeningHistory {
    root: PathBuf,
    finalized: VecDeque<HistoryItem>,
    active: Option<ActivePlay>,
    load_error: Option<String>,
}

impl ListeningHistory {
    pub fn new(root: PathBuf) -> Self {
        if root.as_os_str().is_empty() {
            return Self {
                root,
                finalized: VecDeque::new(),
                active: None,
                load_error: None,
            };
        }

        let path = root.join(HISTORY_FILE);
        match fs::read(&path) {
            Ok(bytes) => match parse_snapshot(&path, &bytes) {
                Ok(stored) => Self {
                    root,
                    finalized: stored.finalized,
                    active: stored.active.map(|persisted| ActivePlay {
                        persisted,
                        playing_since: None,
                    }),
                    load_error: None,
                },
                Err(error) => {
                    eprintln!("listening history persistence disabled: {error}");
                    Self::read_only(root, error)
                }
            },
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Self {
                root,
                finalized: VecDeque::new(),
                active: None,
                load_error: None,
            },
            Err(error) => {
                let error = format!("could not read {}: {error}", path.display());
                eprintln!("listening history persistence disabled: {error}");
                Self::read_only(root, error)
            }
        }
    }

    fn read_only(root: PathBuf, error: String) -> Self {
        Self {
            root,
            finalized: VecDeque::new(),
            active: None,
            load_error: Some(error),
        }
    }

    /// Begins a logical play only after the authoritative `Playing` event.
    /// Replacing an active play finalizes it and installs the new active row in
    /// the same durable snapshot.
    pub fn start(&mut self, track: &TrackRef) {
        if self.root.as_os_str().is_empty() || !self.writable() {
            return;
        }

        self.accrue_active();
        if let Some(previous) = self.active.take() {
            self.finalized.push_back(previous.persisted.item);
        }
        self.active = Some(ActivePlay {
            persisted: PersistedActive {
                item: HistoryItem {
                    row: HistoryRow {
                        track_id: track.id.clone(),
                        started_at: now_millis(),
                        ms_played: 0,
                        completed: false,
                        context: compact_context(&track.context),
                    },
                    track: sanitize_track(track),
                },
                duration_ms: track.duration_ms,
            },
            playing_since: Some(Instant::now()),
        });
        self.enforce_bound();
        if let Err(error) = self.persist() {
            // Keep the complete new state in memory. Pause, finalize, or the
            // next track transition will retry the same atomic snapshot.
            eprintln!("could not persist active listening history row: {error}");
        }
    }

    pub fn resume(&mut self) {
        let Some(active) = self.active.as_mut() else {
            return;
        };
        if active.playing_since.is_some() {
            return;
        }
        active.playing_since = Some(Instant::now());
        if let Err(error) = self.persist() {
            eprintln!("could not persist resumed listening history row: {error}");
        }
    }

    pub fn start_or_resume(&mut self, track: &TrackRef) {
        let same_track = self
            .active
            .as_ref()
            .is_some_and(|active| active.persisted.item.row.track_id == track.id);
        if same_track {
            self.resume();
        } else {
            self.start(track);
        }
    }

    pub fn pause(&mut self) {
        if !self.writable() {
            return;
        }
        self.accrue_active();
        if self.active.is_some() {
            if let Err(error) = self.persist() {
                eprintln!("could not persist paused listening history row: {error}");
            }
        }
    }

    /// Finalizes the current play. A failed atomic replace restores the active
    /// row in memory, allowing a later transition to retry without duplication.
    pub fn finalize(&mut self, completed: bool) -> bool {
        if !self.writable() {
            return false;
        }
        if self.active.is_none() {
            return true;
        }

        self.accrue_active();
        let mut active = self.active.take().expect("active play checked above");
        if completed {
            active.persisted.item.row.completed = true;
            active.persisted.item.row.ms_played = u64::from(active.persisted.duration_ms);
        }

        let dropped = if self.finalized.len() >= MAX_HISTORY_ITEMS {
            self.finalized.pop_front()
        } else {
            None
        };
        self.finalized.push_back(active.persisted.item.clone());
        if let Err(error) = self.persist() {
            self.finalized.pop_back();
            if let Some(item) = dropped {
                self.finalized.push_front(item);
            }
            self.active = Some(active);
            eprintln!("could not persist listening history row: {error}");
            return false;
        }
        true
    }

    /// Returns one capped snapshot, newest first, including the current play.
    pub fn snapshot(&self) -> Result<Vec<HistoryItem>, String> {
        if let Some(error) = &self.load_error {
            return Err(format!("listening history is read-only: {error}"));
        }
        if self.root.as_os_str().is_empty() {
            return Ok(Vec::new());
        }

        let mut items = Vec::with_capacity(
            (self.finalized.len() + usize::from(self.active.is_some())).min(MAX_HISTORY_ITEMS),
        );
        if let Some(active) = &self.active {
            let mut item = active.persisted.item.clone();
            if active.playing_since.is_some() {
                item.row.ms_played = item
                    .row
                    .ms_played
                    .saturating_add(elapsed_ms(active.playing_since))
                    .min(u64::from(active.persisted.duration_ms));
            }
            items.push(item);
        }
        items.extend(self.finalized.iter().rev().cloned());
        items.truncate(MAX_HISTORY_ITEMS);
        Ok(items)
    }

    pub fn clear(&mut self) -> Result<(), String> {
        self.ensure_writable()?;
        if self.root.as_os_str().is_empty() {
            return Ok(());
        }

        let finalized = std::mem::take(&mut self.finalized);
        let active = self.active.take();
        if let Err(error) = self.persist() {
            self.finalized = finalized;
            self.active = active;
            return Err(error);
        }
        Ok(())
    }

    fn writable(&self) -> bool {
        if let Some(error) = &self.load_error {
            eprintln!("listening history mutation rejected: {error}");
            false
        } else {
            true
        }
    }

    fn ensure_writable(&self) -> Result<(), String> {
        match &self.load_error {
            Some(error) => Err(format!("listening history is read-only: {error}")),
            None => Ok(()),
        }
    }

    fn accrue_active(&mut self) {
        let Some(active) = self.active.as_mut() else {
            return;
        };
        let Some(since) = active.playing_since.take() else {
            return;
        };
        active.persisted.item.row.ms_played = active
            .persisted
            .item
            .row
            .ms_played
            .saturating_add(elapsed_ms(Some(since)))
            .min(u64::from(active.persisted.duration_ms));
    }

    fn enforce_bound(&mut self) {
        let finalized_limit = MAX_HISTORY_ITEMS - usize::from(self.active.is_some());
        while self.finalized.len() > finalized_limit {
            self.finalized.pop_front();
        }
    }

    fn persist(&self) -> Result<(), String> {
        if self.root.as_os_str().is_empty() {
            return Ok(());
        }
        self.ensure_writable()?;
        let snapshot = StoredHistoryRef {
            version: HISTORY_VERSION,
            finalized: &self.finalized,
            active: self.active.as_ref().map(|active| &active.persisted),
        };
        write_json_atomic(&self.root.join(HISTORY_FILE), &snapshot)
    }
}

fn parse_snapshot(path: &Path, bytes: &[u8]) -> Result<StoredHistory, String> {
    let stored: StoredHistory = serde_json::from_slice(bytes)
        .map_err(|error| format!("could not parse {}: {error}", path.display()))?;
    if stored.version != HISTORY_VERSION {
        return Err(format!(
            "unsupported listening history version {} (expected {HISTORY_VERSION})",
            stored.version
        ));
    }
    let count = stored.finalized.len() + usize::from(stored.active.is_some());
    if count > MAX_HISTORY_ITEMS {
        return Err(format!(
            "invalid listening history: {count} rows exceeds the {MAX_HISTORY_ITEMS}-row cap"
        ));
    }
    for item in stored
        .finalized
        .iter()
        .chain(stored.active.iter().map(|active| &active.item))
    {
        validate_item(item)?;
    }
    Ok(stored)
}

fn validate_item(item: &HistoryItem) -> Result<(), String> {
    if item.row.track_id.is_empty() {
        return Err("invalid listening history: empty track id".to_owned());
    }
    let track = &item.track;
    if track.id != item.row.track_id {
        return Err("invalid listening history: row and track ids differ".to_owned());
    }
    if track.play_count.is_some()
        || track.added_at.is_some()
        || track.unavailable
        || track.unavailable_reason.is_some()
        || track.cached
        || !track.context.is_empty()
        || track.effective_edit.is_some()
    {
        return Err("invalid listening history: track contains volatile playback data".to_owned());
    }
    Ok(())
}

/// Retains identity and display metadata only. History replay must resolve its
/// current context, edit, availability, and cache state instead of reviving a
/// stale browse/queue snapshot.
fn sanitize_track(track: &TrackRef) -> TrackRef {
    let mut track = track.clone();
    track.play_count = None;
    track.added_at = None;
    track.unavailable = false;
    track.unavailable_reason = None;
    track.cached = false;
    track.context.clear();
    track.effective_edit = None;
    track
}

fn compact_context(context: &str) -> String {
    let context = context.trim();
    if context.len() <= 128 {
        return context.to_owned();
    }
    context.chars().take(128).collect()
}

fn now_millis() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis().min(i64::MAX as u128) as i64)
        .unwrap_or(0)
}

fn elapsed_ms(since: Option<Instant>) -> u64 {
    since
        .and_then(|since| u64::try_from(since.elapsed().as_millis()).ok())
        .unwrap_or(0)
}

fn write_json_atomic<T: Serialize>(path: &Path, value: &T) -> Result<(), String> {
    let bytes = serde_json::to_vec(value)
        .map_err(|error| format!("could not serialize {}: {error}", path.display()))?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|error| format!("could not create {}: {error}", parent.display()))?;
    }
    let temporary = path.with_extension("json.tmp");
    let mut file = File::create(&temporary)
        .map_err(|error| format!("could not create {}: {error}", temporary.display()))?;
    file.write_all(&bytes)
        .and_then(|()| file.sync_all())
        .map_err(|error| format!("could not write {}: {error}", temporary.display()))?;
    replace_file_atomically(&temporary, path)
        .map_err(|error| format!("could not replace {}: {error}", path.display()))
}

#[cfg(not(windows))]
fn replace_file_atomically(source: &Path, destination: &Path) -> std::io::Result<()> {
    fs::rename(source, destination)
}

#[cfg(windows)]
fn replace_file_atomically(source: &Path, destination: &Path) -> std::io::Result<()> {
    use std::os::windows::ffi::OsStrExt;

    #[link(name = "Kernel32")]
    unsafe extern "system" {
        fn MoveFileExW(
            existing_file_name: *const u16,
            new_file_name: *const u16,
            flags: u32,
        ) -> i32;
    }
    const MOVEFILE_REPLACE_EXISTING: u32 = 0x1;
    const MOVEFILE_WRITE_THROUGH: u32 = 0x8;
    let source: Vec<u16> = source.as_os_str().encode_wide().chain(Some(0)).collect();
    let destination: Vec<u16> = destination
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect();
    let moved = unsafe {
        MoveFileExW(
            source.as_ptr(),
            destination.as_ptr(),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    };
    if moved == 0 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use spotify_playback_engine::protocol::{TimeRange, TrackEdit};

    fn scratch() -> PathBuf {
        static NEXT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let ordinal = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        std::env::temp_dir().join(format!(
            "spotify-renderer-history-test-{}-{}-{ordinal}",
            std::process::id(),
            now_millis()
        ))
    }

    fn track(id: &str) -> TrackRef {
        TrackRef {
            id: id.to_owned(),
            uri: format!("spotify:track:{id}"),
            name: format!("Track {id}"),
            artist_names: vec!["Artist".to_owned()],
            duration_ms: 180_000,
            play_count: Some(42),
            added_at: Some(123),
            unavailable: true,
            unavailable_reason: Some("country".to_owned()),
            cached: true,
            context: " playlist:source ".to_owned(),
            effective_edit: Some(TrackEdit {
                cuts: vec![TimeRange {
                    start_ms: 1,
                    end_ms: 2,
                }],
                loop_range: None,
            }),
            ..TrackRef::default()
        }
    }

    #[test]
    fn finalized_and_active_rows_share_one_recoverable_snapshot() {
        let root = scratch();
        let mut history = ListeningHistory::new(root.clone());
        history.start(&track("finalized"));
        assert!(history.finalize(true));
        history.start(&track("active"));
        history.pause();

        let recovered = ListeningHistory::new(root.clone());
        let items = recovered.snapshot().unwrap();
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].row.track_id, "active");
        assert!(!items[0].row.completed);
        assert_eq!(items[1].row.track_id, "finalized");
        assert!(items[1].row.completed);
        assert_eq!(fs::read_dir(&root).unwrap().count(), 1);

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn resume_rewrites_active_snapshot_for_crash_recovery() {
        let root = scratch();
        let mut history = ListeningHistory::new(root.clone());
        history.start(&track("resumed"));
        history.pause();

        let mut resumed = ListeningHistory::new(root.clone());
        fs::remove_file(root.join(HISTORY_FILE)).unwrap();
        resumed.resume();

        let recovered = ListeningHistory::new(root.clone());
        let items = recovered.snapshot().unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].row.track_id, "resumed");

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn snapshot_is_newest_first_and_caps_active_plus_finalized() {
        let root = scratch();
        let mut history = ListeningHistory::new(root.clone());
        for index in 0..MAX_HISTORY_ITEMS {
            let track = track(&format!("track-{index}"));
            history.finalized.push_back(HistoryItem {
                row: HistoryRow {
                    track_id: track.id.clone(),
                    started_at: index as i64,
                    ..HistoryRow::default()
                },
                track: sanitize_track(&track),
            });
        }
        history.start(&track("active"));

        let recovered = ListeningHistory::new(root.clone());
        let items = recovered.snapshot().unwrap();
        assert_eq!(items.len(), MAX_HISTORY_ITEMS);
        assert_eq!(items[0].row.track_id, "active");
        assert_eq!(items[1].row.track_id, "track-1999");
        assert_eq!(items.last().unwrap().row.track_id, "track-1");

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn corrupt_or_legacy_store_is_never_replaced_by_playback() {
        let root = scratch();
        fs::create_dir_all(&root).unwrap();
        let path = root.join(HISTORY_FILE);
        let corrupt = br#"{"legacy":"jsonl-sidecar"}"#;
        fs::write(&path, corrupt).unwrap();

        let mut history = ListeningHistory::new(root.clone());
        assert!(history.snapshot().unwrap_err().contains("read-only"));
        history.start(&track("new"));
        assert!(!history.finalize(true));
        assert!(history.clear().unwrap_err().contains("read-only"));
        assert_eq!(fs::read(&path).unwrap(), corrupt);

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn persisted_track_metadata_is_safe_to_replay() {
        let root = scratch();
        let mut history = ListeningHistory::new(root.clone());
        history.start(&track("sanitized"));
        history.pause();

        let recovered = ListeningHistory::new(root.clone());
        let items = recovered.snapshot().unwrap();
        let item = &items[0];
        assert_eq!(item.row.context, "playlist:source");
        let track = &item.track;
        assert_eq!(track.name, "Track sanitized");
        assert!(track.play_count.is_none());
        assert!(track.added_at.is_none());
        assert!(!track.unavailable);
        assert!(track.unavailable_reason.is_none());
        assert!(!track.cached);
        assert!(track.context.is_empty());
        assert!(track.effective_edit.is_none());

        let _ = fs::remove_dir_all(root);
    }
}
