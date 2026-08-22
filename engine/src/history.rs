use std::collections::HashMap;
use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use spotify_playback_engine::protocol::{HistoryItem, HistoryPage, HistoryRow, TrackRef};

const HISTORY_FILE: &str = "listening_history.jsonl";
const CURRENT_FILE: &str = "listening_history.current.json";
const METADATA_FILE: &str = "listening_history_tracks.json";
/// The file is a bounded append-only journal. Compaction happens only after a
/// finalized transition crosses this limit, never on a position heartbeat.
const MAX_HISTORY_ROWS: usize = 2_000;
const MAX_METADATA_ENTRIES: usize = 2_500;
const MAX_PAGE_LIMIT: usize = 100;

struct ActivePlay {
    row: HistoryRow,
    duration_ms: u32,
    playing_since: Option<Instant>,
}

/// Crash-safe local listening history.
///
/// Finalized rows are individual JSON lines. The currently active row is an
/// atomic single-row file, so a heartbeat never rewrites the history journal.
/// Pause/finalize/track transitions update that small file; finalization then
/// appends one complete row and removes the active marker.
pub struct ListeningHistory {
    root: PathBuf,
    active: Option<ActivePlay>,
}

impl ListeningHistory {
    pub fn new(root: PathBuf) -> Self {
        let mut history = Self { root, active: None };
        if !history.root.as_os_str().is_empty() {
            history.recover_active();
        }
        history
    }

    /// Begins a logical play only after the player emitted its authoritative
    /// `Playing` event. A pause/resume never calls this method again.
    pub fn start(&mut self, track: &TrackRef) {
        if self.root.as_os_str().is_empty() {
            return;
        }
        let _ = self.finalize(false);
        let row = HistoryRow {
            track_id: track.id.clone(),
            started_at: now_millis(),
            ms_played: 0,
            completed: false,
            context: compact_context(&track.context),
        };
        self.remember_track(track);
        self.active = Some(ActivePlay {
            row,
            duration_ms: track.duration_ms,
            playing_since: Some(Instant::now()),
        });
        self.persist_active();
    }

    /// Arms elapsed-time accounting for a resumed play. This is intentionally
    /// in-memory until a meaningful transition persists the active row.
    pub fn resume(&mut self) {
        if let Some(active) = self.active.as_mut() {
            if active.playing_since.is_none() {
                active.playing_since = Some(Instant::now());
            }
        }
    }

    /// Starts a new row for a different track, or resumes the existing row
    /// when librespot reports Playing again after a pause/seek.
    pub fn start_or_resume(&mut self, track: &TrackRef) {
        let same_track = self
            .active
            .as_ref()
            .is_some_and(|active| active.row.track_id == track.id);
        if same_track {
            self.resume();
        } else {
            self.start(track);
        }
    }

    /// Captures the time spent playing before a user pause without creating a
    /// second row. The active marker remains on disk for crash recovery.
    pub fn pause(&mut self) {
        self.accrue_active();
        self.persist_active();
    }

    /// Finalizes the current logical play. Natural end marks it completed and
    /// uses the known track duration; every other transition keeps the
    /// measured elapsed duration and leaves `completed` false.
    pub fn finalize(&mut self, completed: bool) -> bool {
        if self.active.is_none() {
            return true;
        }
        self.accrue_active();
        let active = self.active.as_mut().expect("active play checked above");
        if completed {
            active.row.completed = true;
            active.row.ms_played = u64::from(active.duration_ms);
        }
        let row = active.row.clone();
        if let Err(error) = self.append_row(&row) {
            eprintln!("could not persist listening history row: {error}");
            self.persist_active();
            return false;
        }
        self.active = None;
        let _ = fs::remove_file(self.current_path());
        true
    }

    /// Drops both finalized and active rows. A play that was cleared while it
    /// is still running is intentionally no longer resurrected on its later
    /// pause/end transition; its next actual track start may create a row.
    pub fn clear(&mut self) -> Result<(), String> {
        if self.root.as_os_str().is_empty() {
            return Ok(());
        }
        self.active = None;
        remove_if_present(&self.history_path())?;
        remove_if_present(&self.current_path())?;
        remove_if_present(&self.metadata_path())?;
        Ok(())
    }

    pub fn page(&self, offset: usize, limit: usize) -> Result<HistoryPage, String> {
        if self.root.as_os_str().is_empty() {
            return Ok(HistoryPage::default());
        }
        let limit = limit.clamp(1, MAX_PAGE_LIMIT);
        let mut rows = self.read_rows()?;
        if let Some(active) = &self.active {
            let mut current = active.row.clone();
            if active.playing_since.is_some() {
                current.ms_played = current
                    .ms_played
                    .saturating_add(elapsed_ms(active.playing_since))
                    .min(u64::from(active.duration_ms));
            }
            rows.push(current);
        }
        rows.reverse();
        let metadata = self.read_metadata();
        let total = rows.len();
        let entries = rows
            .into_iter()
            .skip(offset)
            .take(limit)
            .map(|row| HistoryItem {
                track: metadata.get(&row.track_id).cloned(),
                row,
            })
            .collect();
        let next_offset = (offset + limit < total).then_some(offset + limit);
        Ok(HistoryPage {
            entries,
            next_offset,
        })
    }

    fn recover_active(&mut self) {
        let Ok(bytes) = fs::read(self.current_path()) else {
            return;
        };
        let Ok(row) = serde_json::from_slice::<HistoryRow>(&bytes) else {
            let _ = fs::remove_file(self.current_path());
            return;
        };
        // A process can die after appending but before removing the marker.
        // Compare the last valid row before appending so recovery is idempotent.
        if self.last_row().as_ref() != Some(&row) {
            let _ = self.append_row(&row);
        }
        let _ = fs::remove_file(self.current_path());
    }

    fn accrue_active(&mut self) {
        let Some(active) = self.active.as_mut() else {
            return;
        };
        let Some(since) = active.playing_since.take() else {
            return;
        };
        active.row.ms_played = active
            .row
            .ms_played
            .saturating_add(elapsed_ms(Some(since)))
            .min(u64::from(active.duration_ms));
    }

    fn persist_active(&self) {
        let Some(active) = &self.active else {
            return;
        };
        if let Err(error) = write_json_atomic(&self.current_path(), &active.row) {
            eprintln!("could not persist active listening history row: {error}");
        }
    }

    fn append_row(&self, row: &HistoryRow) -> Result<(), String> {
        if self.last_row().as_ref() == Some(row) {
            return Ok(());
        }
        fs::create_dir_all(&self.root)
            .map_err(|error| format!("could not create history directory: {error}"))?;
        let bytes = serde_json::to_vec(row)
            .map_err(|error| format!("could not serialize history row: {error}"))?;
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.history_path())
            .map_err(|error| format!("could not open listening history: {error}"))?;
        file.write_all(&bytes)
            .and_then(|()| file.write_all(b"\n"))
            .and_then(|()| file.sync_data())
            .map_err(|error| format!("could not append listening history: {error}"))?;
        self.compact_if_needed();
        Ok(())
    }

    fn compact_if_needed(&self) {
        let Ok(mut rows) = self.read_rows() else {
            return;
        };
        if rows.len() <= MAX_HISTORY_ROWS {
            return;
        }
        rows.drain(..rows.len() - MAX_HISTORY_ROWS);
        let mut bytes = Vec::new();
        for row in rows {
            if let Ok(encoded) = serde_json::to_vec(&row) {
                bytes.extend_from_slice(&encoded);
                bytes.push(b'\n');
            }
        }
        let _ = write_bytes_atomic(&self.history_path(), &bytes);
    }

    fn read_rows(&self) -> Result<Vec<HistoryRow>, String> {
        let file = match File::open(self.history_path()) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(format!("could not read listening history: {error}")),
        };
        let mut rows = Vec::with_capacity(MAX_HISTORY_ROWS.min(128));
        for line in BufReader::new(file).lines() {
            let Ok(line) = line else { break };
            if let Ok(row) = serde_json::from_str::<HistoryRow>(&line) {
                rows.push(row);
                if rows.len() > MAX_HISTORY_ROWS {
                    rows.remove(0);
                }
            }
        }
        Ok(rows)
    }

    fn last_row(&self) -> Option<HistoryRow> {
        self.read_rows().ok()?.pop()
    }

    fn remember_track(&self, track: &TrackRef) {
        let mut metadata = self.read_metadata();
        metadata.insert(track.id.clone(), track.clone());
        while metadata.len() > MAX_METADATA_ENTRIES {
            let Some(key) = metadata.keys().next().cloned() else {
                break;
            };
            metadata.remove(&key);
        }
        if let Err(error) = write_json_atomic(&self.metadata_path(), &metadata) {
            eprintln!("could not persist listening history metadata: {error}");
        }
    }

    fn read_metadata(&self) -> HashMap<String, TrackRef> {
        fs::read(self.metadata_path())
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default()
    }

    fn history_path(&self) -> PathBuf {
        self.root.join(HISTORY_FILE)
    }

    fn current_path(&self) -> PathBuf {
        self.root.join(CURRENT_FILE)
    }

    fn metadata_path(&self) -> PathBuf {
        self.root.join(METADATA_FILE)
    }
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

fn remove_if_present(path: &Path) -> Result<(), String> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(format!("could not remove {}: {error}", path.display())),
    }
}

fn write_json_atomic<T: serde::Serialize>(path: &Path, value: &T) -> Result<(), String> {
    let bytes = serde_json::to_vec(value)
        .map_err(|error| format!("could not serialize {}: {error}", path.display()))?;
    write_bytes_atomic(path, &bytes)
}

fn write_bytes_atomic(path: &Path, bytes: &[u8]) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|error| format!("could not create {}: {error}", parent.display()))?;
    }
    let temp = path.with_extension("json.tmp");
    let mut file = File::create(&temp)
        .map_err(|error| format!("could not create {}: {error}", temp.display()))?;
    file.write_all(bytes)
        .and_then(|()| file.sync_all())
        .map_err(|error| format!("could not write {}: {error}", temp.display()))?;
    replace_file_atomically(&temp, path)
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

    fn scratch() -> PathBuf {
        std::env::temp_dir().join(format!(
            "spotify-renderer-history-test-{}-{}",
            std::process::id(),
            now_millis()
        ))
    }

    #[test]
    fn one_play_survives_pause_resume_and_finalizes_once() {
        let root = scratch();
        let mut history = ListeningHistory::new(root.clone());
        let track = TrackRef {
            id: "0123456789ABCDEFGHIJKL".to_owned(),
            uri: "spotify:track:0123456789ABCDEFGHIJKL".to_owned(),
            name: "Track".to_owned(),
            duration_ms: 180_000,
            context: "playlist:source".to_owned(),
            ..TrackRef::default()
        };

        history.start(&track);
        history.pause();
        history.resume();
        assert!(history.finalize(true));

        let page = history.page(0, 40).unwrap();
        assert_eq!(page.entries.len(), 1);
        assert_eq!(page.entries[0].row.track_id, track.id);
        assert_eq!(page.entries[0].row.context, "playlist:source");
        assert_eq!(page.entries[0].row.ms_played, 180_000);
        assert!(page.entries[0].row.completed);
        assert_eq!(page.entries[0].track.as_ref().unwrap().name, "Track");
        assert!(page.next_offset.is_none());

        history.clear().unwrap();
        assert!(history.page(0, 40).unwrap().entries.is_empty());
        let _ = fs::remove_dir_all(root);
    }
}
