use std::collections::{BTreeMap, BTreeSet};
use std::io::Write;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use spotify_playback_engine::protocol::{
    LoopRange, TimeRange, TrackEdit, TrackEditDefinition, TrackEditStatus,
};

const STORE_VERSION: u32 = 1;
const STORE_FILE: &str = "track_edits.json";

#[derive(Debug, Default, Deserialize, Serialize)]
#[serde(default)]
struct StoredEdits {
    version: u32,
    definitions: BTreeMap<String, StoredDefinition>,
    playlist_enablement: BTreeMap<String, BTreeSet<String>>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct StoredDefinition {
    duration_ms: u32,
    cuts: Vec<TimeRange>,
    loop_range: Option<LoopRange>,
}

impl StoredDefinition {
    fn into_definition(self, track_id: String) -> TrackEditDefinition {
        TrackEditDefinition {
            track_id,
            duration_ms: self.duration_ms,
            edit: TrackEdit {
                cuts: self.cuts,
                loop_range: self.loop_range,
            },
        }
    }

    fn edit(&self) -> TrackEdit {
        TrackEdit {
            cuts: self.cuts.clone(),
            loop_range: self.loop_range,
        }
    }
}

pub struct TrackEditStore {
    path: PathBuf,
    data: StoredEdits,
    load_error: Option<String>,
}

impl TrackEditStore {
    pub fn load(state_directory: &Path) -> Result<Self, String> {
        let path = state_directory.join(STORE_FILE);
        let data = match std::fs::read(&path) {
            Ok(bytes) => {
                let data: StoredEdits = serde_json::from_slice(&bytes)
                    .map_err(|error| format!("could not parse {}: {error}", path.display()))?;
                if data.version != STORE_VERSION {
                    return Err(format!(
                        "unsupported track edit store version {} (expected {STORE_VERSION})",
                        data.version
                    ));
                }
                for (track_id, definition) in &data.definitions {
                    validate_definition(
                        track_id,
                        definition.duration_ms,
                        &definition.cuts,
                        definition.loop_range,
                    )?;
                }
                data
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => StoredEdits {
                version: STORE_VERSION,
                ..StoredEdits::default()
            },
            Err(error) => {
                return Err(format!("could not read {}: {error}", path.display()));
            }
        };
        Ok(Self {
            path,
            data,
            load_error: None,
        })
    }
    pub fn load_or_empty(state_directory: &Path) -> Self {
        match Self::load(state_directory) {
            Ok(store) => store,
            Err(error) => {
                eprintln!("track edit persistence disabled for the unreadable snapshot: {error}");
                Self {
                    path: state_directory.join(STORE_FILE),
                    data: StoredEdits {
                        version: STORE_VERSION,
                        ..StoredEdits::default()
                    },
                    load_error: Some(error),
                }
            }
        }
    }

    pub fn status(&self, track_id: &str, playlist_id: Option<&str>) -> TrackEditStatus {
        let definition = self
            .data
            .definitions
            .get(track_id)
            .cloned()
            .map(|definition| definition.into_definition(track_id.to_owned()));
        let enabled = definition.is_some()
            && playlist_id.is_some_and(|playlist| self.is_enabled(playlist, track_id));
        TrackEditStatus {
            definition,
            enabled,
        }
    }

    pub fn save_definition(
        &mut self,
        track_id: String,
        duration_ms: u32,
        cuts: Vec<TimeRange>,
        loop_range: Option<LoopRange>,
    ) -> Result<TrackEditDefinition, String> {
        self.ensure_writable()?;
        validate_definition(&track_id, duration_ms, &cuts, loop_range)?;
        let stored = StoredDefinition {
            duration_ms,
            cuts,
            loop_range,
        };
        let result = stored.clone().into_definition(track_id.clone());
        let previous = self.data.definitions.insert(track_id.clone(), stored);
        if let Err(error) = self.persist() {
            match previous {
                Some(definition) => {
                    self.data.definitions.insert(track_id, definition);
                }
                None => {
                    self.data.definitions.remove(&track_id);
                }
            }
            return Err(error);
        }
        Ok(result)
    }

    pub fn delete_definition(&mut self, track_id: &str) -> Result<(), String> {
        self.ensure_writable()?;
        let previous_definition = self.data.definitions.remove(track_id);
        let previous_enablement = self.data.playlist_enablement.clone();
        self.data.playlist_enablement.retain(|_, tracks| {
            tracks.remove(track_id);
            !tracks.is_empty()
        });
        if let Err(error) = self.persist() {
            if let Some(definition) = previous_definition {
                self.data
                    .definitions
                    .insert(track_id.to_owned(), definition);
            }
            self.data.playlist_enablement = previous_enablement;
            return Err(error);
        }
        Ok(())
    }

    pub fn set_enabled(
        &mut self,
        playlist_id: &str,
        track_id: &str,
        enabled: bool,
    ) -> Result<(), String> {
        self.ensure_writable()?;
        validate_identifier("playlist id", playlist_id)?;
        validate_identifier("track id", track_id)?;
        if enabled && !self.data.definitions.contains_key(track_id) {
            return Err(
                "cannot enable an edited version before its definition is saved".to_owned(),
            );
        }
        let previous = self.data.playlist_enablement.clone();
        if enabled {
            self.data
                .playlist_enablement
                .entry(playlist_id.to_owned())
                .or_default()
                .insert(track_id.to_owned());
        } else if let Some(tracks) = self.data.playlist_enablement.get_mut(playlist_id) {
            tracks.remove(track_id);
            if tracks.is_empty() {
                self.data.playlist_enablement.remove(playlist_id);
            }
        }
        if let Err(error) = self.persist() {
            self.data.playlist_enablement = previous;
            return Err(error);
        }
        Ok(())
    }

    pub fn resolve(&self, track_id: &str, duration_ms: u32, context: &str) -> Option<TrackEdit> {
        let playlist_id = context.strip_prefix("playlist:")?;
        if playlist_id.is_empty() || !self.is_enabled(playlist_id, track_id) {
            return None;
        }
        let definition = self.data.definitions.get(track_id)?;
        validate_definition(
            track_id,
            duration_ms,
            &definition.cuts,
            definition.loop_range,
        )
        .ok()?;
        Some(definition.edit())
    }

    fn is_enabled(&self, playlist_id: &str, track_id: &str) -> bool {
        self.data
            .playlist_enablement
            .get(playlist_id)
            .is_some_and(|tracks| tracks.contains(track_id))
    }

    fn ensure_writable(&self) -> Result<(), String> {
        match &self.load_error {
            Some(error) => Err(format!(
                "track edit store is read-only because its snapshot could not be loaded: {error}"
            )),
            None => Ok(()),
        }
    }

    fn persist(&self) -> Result<(), String> {
        self.ensure_writable()?;
        let parent = self
            .path
            .parent()
            .ok_or_else(|| "track edit store has no parent directory".to_owned())?;
        std::fs::create_dir_all(parent)
            .map_err(|error| format!("could not create {}: {error}", parent.display()))?;
        let bytes = serde_json::to_vec_pretty(&self.data)
            .map_err(|error| format!("could not serialize track edits: {error}"))?;
        let temporary = self.path.with_extension("json.tmp");
        let mut file = std::fs::File::create(&temporary)
            .map_err(|error| format!("could not create {}: {error}", temporary.display()))?;
        file.write_all(&bytes)
            .and_then(|()| file.sync_all())
            .map_err(|error| format!("could not write {}: {error}", temporary.display()))?;
        replace_file_atomically(&temporary, &self.path)
            .map_err(|error| format!("could not install {}: {error}", self.path.display()))
    }
}

#[cfg(not(windows))]
fn replace_file_atomically(source: &Path, destination: &Path) -> std::io::Result<()> {
    std::fs::rename(source, destination)
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

/// A transport view of one source track. Cut ranges are absent from this
/// timeline; positions at a cut seam resolve to the first source millisecond
/// after that cut.
///
/// Callers construct this only after [`validate_definition`] has accepted the
/// cuts, so mapping can stay allocation-free and linear in the usually tiny
/// edit list.
#[derive(Clone, Copy, Debug)]
pub struct EditTimeline<'a> {
    source_duration_ms: u32,
    compiled_duration_ms: u32,
    cuts: &'a [TimeRange],
}

impl<'a> EditTimeline<'a> {
    pub fn new(source_duration_ms: u32, cuts: &'a [TimeRange]) -> Self {
        let compiled_duration_ms = cuts.iter().fold(source_duration_ms, |duration, cut| {
            duration - (cut.end_ms - cut.start_ms)
        });
        Self {
            source_duration_ms,
            compiled_duration_ms,
            cuts,
        }
    }

    pub fn compiled_duration_ms(self) -> u32 {
        self.compiled_duration_ms
    }

    pub fn source_to_compiled(self, position_ms: u32) -> u32 {
        let position_ms = position_ms.min(self.source_duration_ms);
        let mut removed_ms = 0;
        for cut in self.cuts {
            if position_ms < cut.start_ms {
                break;
            }
            if position_ms < cut.end_ms {
                return cut.start_ms - removed_ms;
            }
            removed_ms += cut.end_ms - cut.start_ms;
        }
        position_ms - removed_ms
    }

    pub fn compiled_to_source(self, position_ms: u32) -> u32 {
        let position_ms = position_ms.min(self.compiled_duration_ms());
        let mut removed_ms = 0;
        for cut in self.cuts {
            let seam_ms = cut.start_ms - removed_ms;
            if position_ms < seam_ms {
                break;
            }
            removed_ms += cut.end_ms - cut.start_ms;
        }
        position_ms
            .saturating_add(removed_ms)
            .min(self.source_duration_ms)
    }
}

pub fn validate_definition(
    track_id: &str,
    duration_ms: u32,
    cuts: &[TimeRange],
    loop_range: Option<LoopRange>,
) -> Result<(), String> {
    validate_identifier("track id", track_id)?;
    if duration_ms == 0 {
        return Err("track duration must be greater than zero".to_owned());
    }
    if cuts.is_empty() && loop_range.is_none() {
        return Err("an edit definition must contain a cut or a loop".to_owned());
    }

    let mut previous_end = 0;
    for (index, range) in cuts.iter().copied().enumerate() {
        validate_range("cut", range, duration_ms)?;
        if index > 0 && range.start_ms < previous_end {
            return Err("cut ranges must be sorted and non-overlapping".to_owned());
        }
        previous_end = range.end_ms;
    }
    let removed_ms: u64 = cuts
        .iter()
        .map(|range| u64::from(range.end_ms - range.start_ms))
        .sum();
    if removed_ms >= u64::from(duration_ms) {
        return Err("cut ranges must leave at least one millisecond of the track".to_owned());
    }

    if let Some(loop_range) = loop_range {
        let loop_time = TimeRange {
            start_ms: loop_range.start_ms,
            end_ms: loop_range.end_ms,
        };
        validate_range("loop", loop_time, duration_ms)?;
        if !(2..=32).contains(&loop_range.play_count) {
            return Err("loop play count must be between 2 and 32".to_owned());
        }
        if cuts.iter().any(|cut| ranges_overlap(*cut, loop_time)) {
            return Err("the loop range cannot overlap a cut range".to_owned());
        }
    }
    Ok(())
}

fn validate_identifier(label: &str, value: &str) -> Result<(), String> {
    if value.trim().is_empty() {
        Err(format!("{label} cannot be empty"))
    } else {
        Ok(())
    }
}

fn validate_range(label: &str, range: TimeRange, duration_ms: u32) -> Result<(), String> {
    if range.start_ms >= range.end_ms {
        return Err(format!("{label} range must have positive duration"));
    }
    if range.end_ms > duration_ms {
        return Err(format!("{label} range exceeds the track duration"));
    }
    Ok(())
}

fn ranges_overlap(left: TimeRange, right: TimeRange) -> bool {
    left.start_ms < right.end_ms && right.start_ms < left.end_ms
}
#[cfg(test)]
mod tests {
    use super::*;

    fn range(start_ms: u32, end_ms: u32) -> TimeRange {
        TimeRange { start_ms, end_ms }
    }

    fn loop_range(start_ms: u32, end_ms: u32, play_count: u32) -> LoopRange {
        LoopRange {
            start_ms,
            end_ms,
            play_count,
        }
    }

    #[test]
    fn validation_rejects_unsorted_overlap_and_out_of_bounds_ranges() {
        assert!(
            validate_definition(
                "track",
                10_000,
                &[range(4_000, 6_000), range(2_000, 3_000)],
                None
            )
            .is_err()
        );
        assert!(
            validate_definition(
                "track",
                10_000,
                &[range(2_000, 5_000), range(4_000, 6_000)],
                None
            )
            .is_err()
        );
        assert!(validate_definition("track", 10_000, &[range(2_000, 11_000)], None).is_err());
        assert!(
            validate_definition("track", 10_000, &[], Some(loop_range(5_000, 5_000, 2))).is_err()
        );
        assert!(
            validate_definition(
                "track",
                10_000,
                &[range(2_000, 4_000)],
                Some(loop_range(3_000, 6_000, 2))
            )
            .is_err()
        );
    }

    #[test]
    fn validation_rejects_invalid_loop_play_counts() {
        for play_count in [0, 1, 33, u32::MAX] {
            assert!(
                validate_definition(
                    "track",
                    10_000,
                    &[],
                    Some(loop_range(2_000, 4_000, play_count)),
                )
                .is_err()
            );
        }
        assert!(
            validate_definition("track", 10_000, &[], Some(loop_range(2_000, 4_000, 2))).is_ok()
        );
        assert!(
            validate_definition("track", 10_000, &[], Some(loop_range(2_000, 4_000, 32))).is_ok()
        );
    }

    #[test]
    fn validation_rejects_cuts_that_remove_the_entire_track() {
        assert!(validate_definition("track", 10_000, &[range(0, 10_000)], None).is_err());
        assert!(
            validate_definition(
                "track",
                10_000,
                &[range(0, 4_000), range(4_000, 10_000)],
                None,
            )
            .is_err()
        );
        assert!(validate_definition("track", 10_000, &[range(0, 9_999)], None).is_ok());
    }

    #[test]
    fn edit_timeline_maps_identity_seams_and_multiple_cuts() {
        let identity = EditTimeline::new(10_000, &[]);
        assert_eq!(identity.compiled_duration_ms(), 10_000);
        for position in [0, 1, 4_999, 10_000, u32::MAX] {
            let clamped = position.min(10_000);
            assert_eq!(identity.source_to_compiled(position), clamped);
            assert_eq!(identity.compiled_to_source(position), clamped);
        }

        let cuts = [
            range(0, 1_000),
            range(3_000, 5_000),
            range(5_000, 6_000),
            range(9_000, 10_000),
        ];
        let timeline = EditTimeline::new(10_000, &cuts);
        assert_eq!(timeline.compiled_duration_ms(), 5_000);
        assert_eq!(
            timeline.source_to_compiled(500),
            0,
            "inside a cut maps to its seam"
        );
        assert_eq!(
            timeline.compiled_to_source(0),
            1_000,
            "a seam maps after its cut"
        );
        assert_eq!(timeline.source_to_compiled(4_000), 2_000);
        assert_eq!(
            timeline.compiled_to_source(2_000),
            6_000,
            "adjacent cuts at one seam are crossed together"
        );
        assert_eq!(
            timeline.compiled_to_source(5_000),
            10_000,
            "an ending cut is skipped"
        );

        for source in [1_000, 1_500, 2_999, 6_000, 7_500, 8_999, 10_000] {
            assert_eq!(
                timeline.compiled_to_source(timeline.source_to_compiled(source)),
                source
            );
        }
    }

    #[test]
    fn edit_timeline_is_monotonic_and_handles_an_all_but_one_ms_cut() {
        let cuts = [range(0, 9_999)];
        let timeline = EditTimeline::new(10_000, &cuts);
        assert_eq!(timeline.compiled_duration_ms(), 1);
        assert_eq!(timeline.compiled_to_source(0), 9_999);
        assert_eq!(timeline.compiled_to_source(1), 10_000);

        let mut previous = 0;
        for source in 0..=10_000 {
            let compiled = timeline.source_to_compiled(source);
            assert!(compiled >= previous);
            previous = compiled;
        }
        let mut previous = 0;
        for compiled in 0..=timeline.compiled_duration_ms() {
            let source = timeline.compiled_to_source(compiled);
            assert!(source >= previous);
            previous = source;
        }
    }

    #[test]
    fn only_an_enabled_playlist_context_resolves_an_edit() {
        let root = std::env::temp_dir().join(format!(
            "spotify-renderer-track-edit-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
        ));
        let _ = std::fs::remove_dir_all(&root);
        let mut store = TrackEditStore::load(&root).unwrap();
        store
            .save_definition("track".to_owned(), 10_000, vec![range(1_000, 2_000)], None)
            .unwrap();
        assert!(store.resolve("track", 10_000, "playlist:one").is_none());
        store.set_enabled("one", "track", true).unwrap();
        assert!(store.resolve("track", 10_000, "playlist:one").is_some());
        assert!(store.resolve("track", 10_000, "playlist:two").is_none());
        assert!(store.resolve("track", 10_000, "album:one").is_none());
        store.delete_definition("track").unwrap();
        assert!(!store.status("track", Some("one")).enabled);
        let _ = std::fs::remove_dir_all(root);
    }
    #[test]
    fn unreadable_snapshot_fails_closed_without_destroying_it() {
        let root = std::env::temp_dir().join(format!(
            "spotify-renderer-track-edit-corrupt-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
        ));
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join(STORE_FILE);
        let corrupt = b"{not valid json";
        std::fs::write(&path, corrupt).unwrap();

        let mut store = TrackEditStore::load_or_empty(&root);
        let save_error = store
            .save_definition("track".to_owned(), 10_000, vec![range(1_000, 2_000)], None)
            .unwrap_err();
        assert!(save_error.contains("read-only"));
        assert!(
            store
                .set_enabled("playlist", "track", true)
                .unwrap_err()
                .contains("read-only")
        );
        assert!(
            store
                .delete_definition("track")
                .unwrap_err()
                .contains("read-only")
        );
        assert_eq!(std::fs::read(&path).unwrap(), corrupt);

        let _ = std::fs::remove_dir_all(root);
    }
}
