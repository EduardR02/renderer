use std::collections::{HashMap, HashSet};
use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use spotify_playback_engine::protocol::{
    HistoryItem, HistoryPage, HistoryRow, HistorySort, TrackRef,
};

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

    /// One page of the journal, filtered and ordered before it is sliced.
    ///
    /// The whole journal is read per page. That is deliberate rather than
    /// careless: the file is capped at [`MAX_HISTORY_ROWS`], the client asks
    /// for a page only when it scrolls into one, and a filter or an order over
    /// anything less than every row is wrong — a partial answer that looks
    /// like a complete one.
    pub fn page(
        &self,
        offset: usize,
        limit: usize,
        query: &str,
        sort: HistorySort,
    ) -> Result<HistoryPage, String> {
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
        let metadata = self.read_metadata();
        let needle = query.trim().to_lowercase();
        let mut items: Vec<HistoryItem> = rows
            .into_iter()
            .map(|row| HistoryItem {
                track: metadata.get(&row.track_id).cloned(),
                row,
            })
            .filter(|item| needle.is_empty() || matches_query(item, &needle))
            .collect();
        sort_items(&mut items, sort);

        let total = items.len();
        let entries = items.into_iter().skip(offset).take(limit).collect();
        let next_offset = (offset + limit < total).then_some(offset + limit);
        Ok(HistoryPage {
            entries,
            next_offset,
            total,
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
        /* One read serves both jobs below — the duplicate guard that makes
           crash recovery idempotent, and the decision to compact. It used to
           read the whole journal twice per append, once for each. */
        let mut rows = self.read_rows()?;
        if rows.last() == Some(row) {
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
            .map_err(|error| format!("could not append listening history: {error}"))?;
        file.write_all(&bytes)
            .and_then(|()| file.write_all(b"\n"))
            .and_then(|()| file.sync_data())
            .map_err(|error| format!("could not append listening history: {error}"))?;
        if rows.len() >= MAX_HISTORY_ROWS {
            rows.push(row.clone());
            self.compact(rows);
        }
        Ok(())
    }

    /// Rewrites the journal with only its newest [`MAX_HISTORY_ROWS`] rows.
    fn compact(&self, mut rows: Vec<HistoryRow>) {
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
            }
        }
        /* Trimmed once at the end rather than per line: an over-long file only
           happens when compaction has not caught up, and shifting the whole
           vector down by one on every line past the cap made reading such a
           file quadratic. */
        if rows.len() > MAX_HISTORY_ROWS {
            rows.drain(..rows.len() - MAX_HISTORY_ROWS);
        }
        Ok(rows)
    }

    fn last_row(&self) -> Option<HistoryRow> {
        self.read_rows().ok()?.pop()
    }

    fn remember_track(&self, track: &TrackRef) {
        let mut metadata = self.read_metadata();
        /* Replaying something already recorded changes nothing, and the write
           this skips is a full rewrite of the sidecar plus an fsync — on the
           engine's own loop, at the moment a track starts. Repeats are the
           common case in a play log. */
        if metadata
            .get(&track.id)
            .is_some_and(|known| same_metadata(known, track))
        {
            return;
        }
        metadata.insert(track.id.clone(), track.clone());
        if metadata.len() > MAX_METADATA_ENTRIES {
            self.evict_metadata(&mut metadata, &track.id);
        }
        if let Err(error) = write_json_atomic(&self.metadata_path(), &metadata) {
            eprintln!("could not persist listening history metadata: {error}");
        }
    }

    /// Drops the sidecar entries no visible row needs any more.
    ///
    /// This used to remove `metadata.keys().next()` until the map fit, which is
    /// an ARBITRARY key: `HashMap` iteration order has nothing to do with age,
    /// so the entry evicted could be — and on a full map one time in
    /// [`MAX_METADATA_ENTRIES`] was — the track that had just started playing,
    /// whose brand new row would then render as "metadata expired". The journal
    /// is the only record of age there is, so it decides.
    fn evict_metadata(&self, metadata: &mut HashMap<String, TrackRef>, keep: &str) {
        let rows = self.read_rows().unwrap_or_default();
        let mut wanted = HashSet::with_capacity(MAX_METADATA_ENTRIES);
        wanted.insert(keep);
        for row in rows.iter().rev() {
            if wanted.len() >= MAX_METADATA_ENTRIES {
                break;
            }
            wanted.insert(row.track_id.as_str());
        }
        metadata.retain(|id, _| wanted.contains(id.as_str()));
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

/// Whether the sidecar already describes this track the way the history view
/// would render it. The volatile fields a `TrackRef` also carries — whether
/// the audio happens to be cached right now, which queue context it arrived
/// in, the edit resolved for that queue entry, the browse surface's play count
/// — say nothing about the track and would otherwise force a full rewrite of
/// the sidecar every time the same song came round again.
fn same_metadata(known: &TrackRef, track: &TrackRef) -> bool {
    known.id == track.id
        && known.uri == track.uri
        && known.name == track.name
        && known.artist_names == track.artist_names
        && known.artist_ids == track.artist_ids
        && known.artist_id == track.artist_id
        && known.album_id == track.album_id
        && known.album_name == track.album_name
        && known.cover_url == track.cover_url
        && known.duration_ms == track.duration_ms
        && known.unavailable == track.unavailable
}

/// A row with no surviving metadata has no title and no artist, so there is
/// nothing for a title/artist filter to match — it drops out rather than
/// matching everything.
fn matches_query(item: &HistoryItem, needle: &str) -> bool {
    let Some(track) = &item.track else {
        return false;
    };
    track.name.to_lowercase().contains(needle)
        || track
            .artist_names
            .iter()
            .any(|name| name.to_lowercase().contains(needle))
}

fn sort_items(items: &mut [HistoryItem], sort: HistorySort) {
    /// Sorting a play log by name still leaves ties — the same song played
    /// twenty times — and those read best newest-first, the same as the
    /// default order.
    fn recency_key(item: &HistoryItem) -> std::cmp::Reverse<i64> {
        std::cmp::Reverse(item.row.started_at)
    }
    fn title_key(item: &HistoryItem) -> String {
        item.track
            .as_ref()
            .map(|track| track.name.to_lowercase())
            .unwrap_or_default()
    }
    fn artist_key(item: &HistoryItem) -> String {
        item.track
            .as_ref()
            .and_then(|track| track.artist_names.first())
            .map(|name| name.to_lowercase())
            .unwrap_or_default()
    }

    match sort {
        HistorySort::Recent => items.sort_by_key(recency_key),
        HistorySort::Oldest => items.sort_by_key(|item| item.row.started_at),
        HistorySort::Title => items.sort_by_key(|item| (title_key(item), recency_key(item))),
        HistorySort::Artist => {
            items.sort_by_key(|item| (artist_key(item), title_key(item), recency_key(item)))
        }
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

    /// Tests run in one process and in parallel, and `now_millis` is not
    /// unique at that resolution — two of them starting in the same
    /// millisecond used to share a directory, so each saw the other's rows and
    /// had its files deleted mid-run. The counter is what actually separates
    /// them.
    fn scratch() -> PathBuf {
        static NEXT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let ordinal = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        std::env::temp_dir().join(format!(
            "spotify-renderer-history-test-{}-{}-{ordinal}",
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

        let page = history.page(0, 40, "", HistorySort::Recent).unwrap();
        assert_eq!(page.entries.len(), 1);
        assert_eq!(page.total, 1);
        assert_eq!(page.entries[0].row.track_id, track.id);
        assert_eq!(page.entries[0].row.context, "playlist:source");
        assert_eq!(page.entries[0].row.ms_played, 180_000);
        assert!(page.entries[0].row.completed);
        assert_eq!(page.entries[0].track.as_ref().unwrap().name, "Track");
        assert!(page.next_offset.is_none());

        history.clear().unwrap();
        assert!(
            history
                .page(0, 40, "", HistorySort::Recent)
                .unwrap()
                .entries
                .is_empty()
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn a_page_filters_and_orders_the_whole_journal_before_slicing() {
        let root = scratch();
        let mut history = ListeningHistory::new(root.clone());
        let plays = [
            ("aaaaaaaaaaaaaaaaaaaaaa", "Zebra", "Bowie"),
            ("bbbbbbbbbbbbbbbbbbbbbb", "Apple", "Aphex"),
            ("cccccccccccccccccccccc", "Mango", "Bowie"),
        ];
        for (id, name, artist) in plays {
            let track = TrackRef {
                id: id.to_owned(),
                uri: format!("spotify:track:{id}"),
                name: name.to_owned(),
                artist_names: vec![artist.to_owned()],
                duration_ms: 1_000,
                ..TrackRef::default()
            };
            history.start(&track);
            assert!(history.finalize(true));
        }

        let names = |page: &HistoryPage| {
            page.entries
                .iter()
                .map(|entry| entry.track.as_ref().unwrap().name.clone())
                .collect::<Vec<_>>()
        };

        // Newest first, and the second page continues the same ordering.
        let recent = history.page(0, 2, "", HistorySort::Recent).unwrap();
        assert_eq!(names(&recent), vec!["Mango", "Apple"]);
        assert_eq!(recent.total, 3);
        assert_eq!(recent.next_offset, Some(2));
        assert_eq!(
            names(&history.page(2, 2, "", HistorySort::Recent).unwrap()),
            vec!["Zebra"]
        );

        assert_eq!(
            names(&history.page(0, 40, "", HistorySort::Title).unwrap()),
            vec!["Apple", "Mango", "Zebra"]
        );

        // The filter runs over every row, not over the first page of them.
        let filtered = history.page(0, 1, "bowie", HistorySort::Title).unwrap();
        assert_eq!(filtered.total, 2);
        assert_eq!(names(&filtered), vec!["Mango"]);

        history.clear().unwrap();
        let _ = fs::remove_dir_all(root);
    }
}
