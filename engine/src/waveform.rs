use std::collections::HashMap;
use std::fs::{self, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::SystemTime;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use librespot_audio::{AudioDecrypt, AudioFile, StreamLoaderController};
use librespot_core::{FileId, Session, SpotifyId, SpotifyUri, cache::Cache};
use librespot_metadata::audio::{AudioFileFormat, AudioFiles, AudioItem};
use librespot_playback::decoder::{AudioDecoder, SymphoniaDecoder};
use spotify_playback_engine::protocol::TrackWaveform;
use symphonia::core::io::MediaSource;
use symphonia::core::probe::Hint;
use tokio::sync::{Semaphore, mpsc};

const SAMPLE_RATE: u64 = 44_100;
const CHANNELS: usize = 2;
pub const INTERVAL_MS: u16 = 1;
const SPOTIFY_OGG_HEADER_END: u64 = 0xa7;
const WAVEFORM_CACHE_VERSION: u16 = 2;
const WAVEFORM_HEADER_LEN: usize = 28;
const MAX_ARTIFACT_BYTES: u64 = 16 * 1024 * 1024;
const MAX_CACHE_BYTES: u64 = 64 * 1024 * 1024;
static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(1);

const SUPPORTED_FORMATS: &[(AudioFileFormat, usize)] = &[
    (AudioFileFormat::OGG_VORBIS_96, 12 * 1_024),
    (AudioFileFormat::MP3_96, 12 * 1_024),
    (AudioFileFormat::OGG_VORBIS_160, 20 * 1_024),
    (AudioFileFormat::MP3_160, 20 * 1_024),
    (AudioFileFormat::MP3_256, 32 * 1_024),
    (AudioFileFormat::OGG_VORBIS_320, 40 * 1_024),
    (AudioFileFormat::MP3_320, 40 * 1_024),
];

#[derive(Debug)]
pub struct WorkerOutcome {
    track_id: String,
    generation: u64,
    result: Result<TrackWaveform, String>,
}

struct Job {
    generation: u64,
    cancellation: Arc<AtomicBool>,
    request_ids: Vec<String>,
}

#[derive(Default)]
struct JobBook {
    next_generation: u64,
    jobs: HashMap<String, Job>,
}

impl JobBook {
    fn join_or_insert(
        &mut self,
        track_id: &str,
        request_id: String,
    ) -> Option<(u64, Arc<AtomicBool>)> {
        if let Some(job) = self.jobs.get_mut(track_id) {
            job.request_ids.push(request_id);
            return None;
        }
        self.next_generation = self.next_generation.wrapping_add(1).max(1);
        let generation = self.next_generation;
        let cancellation = Arc::new(AtomicBool::new(false));
        self.jobs.insert(
            track_id.to_owned(),
            Job {
                generation,
                cancellation: cancellation.clone(),
                request_ids: vec![request_id],
            },
        );
        Some((generation, cancellation))
    }

    fn finish(&mut self, track_id: &str, generation: u64) -> Vec<String> {
        if self.jobs.get(track_id).map(|job| job.generation) != Some(generation) {
            return Vec::new();
        }
        self.jobs
            .remove(track_id)
            .map_or_else(Vec::new, |job| job.request_ids)
    }

    fn cancel(&mut self, track_id: &str) -> Vec<String> {
        let Some(job) = self.jobs.remove(track_id) else {
            return Vec::new();
        };
        job.cancellation.store(true, Ordering::Release);
        job.request_ids
    }

    fn cancel_all(&mut self) -> Vec<String> {
        let mut requests = Vec::new();
        for (_, job) in self.jobs.drain() {
            job.cancellation.store(true, Ordering::Release);
            requests.extend(job.request_ids);
        }
        requests
    }
}

/// Owns all waveform jobs for the engine process. Unique tracks share one
/// process-wide permit; requests for the same track share one generation and
/// fan its eventual result out to every waiter.
pub struct WaveformService {
    cache: Cache,
    cache_directory: PathBuf,
    worker: Arc<Semaphore>,
    outcomes: mpsc::UnboundedSender<WorkerOutcome>,
    jobs: JobBook,
}

impl WaveformService {
    pub fn new(
        cache: Cache,
        state_directory: &Path,
    ) -> Result<(Self, mpsc::UnboundedReceiver<WorkerOutcome>), String> {
        let cache_directory = state_directory.join("waveforms").join("v1");
        fs::create_dir_all(&cache_directory)
            .map_err(|error| format!("could not create waveform cache: {error}"))?;
        remove_temporary_files(&cache_directory);
        prune_cache(&cache_directory, MAX_CACHE_BYTES)?;
        let (outcomes, receiver) = mpsc::unbounded_channel();
        Ok((
            Self {
                cache,
                cache_directory,
                worker: Arc::new(Semaphore::new(1)),
                outcomes,
                jobs: JobBook::default(),
            },
            receiver,
        ))
    }

    pub fn request(&mut self, request_id: String, track_id: String, session: Session) {
        let Some((generation, cancellation)) = self.jobs.join_or_insert(&track_id, request_id)
        else {
            return;
        };
        let cache = self.cache.clone();
        let cache_directory = self.cache_directory.clone();
        let worker = self.worker.clone();
        let outcomes = self.outcomes.clone();
        tokio::spawn(async move {
            let result = run_job(
                session,
                cache,
                cache_directory,
                track_id.clone(),
                cancellation,
                worker,
            )
            .await;
            let _ = outcomes.send(WorkerOutcome {
                track_id,
                generation,
                result,
            });
        });
    }

    /// Returns get-request ids that should immediately receive cancellation.
    pub fn cancel(&mut self, track_id: &str) -> Vec<String> {
        self.jobs.cancel(track_id)
    }

    /// Ignores stale outcomes from an earlier, cancelled generation.
    pub fn complete(
        &mut self,
        outcome: WorkerOutcome,
    ) -> Option<(Vec<String>, Result<TrackWaveform, String>)> {
        let request_ids = self.jobs.finish(&outcome.track_id, outcome.generation);
        (!request_ids.is_empty()).then_some((request_ids, outcome.result))
    }

    pub fn shutdown(&mut self) -> Vec<String> {
        self.worker.close();
        self.jobs.cancel_all()
    }
}

async fn run_job(
    session: Session,
    cache: Cache,
    cache_directory: PathBuf,
    requested_track_id: String,
    cancellation: Arc<AtomicBool>,
    worker: Arc<Semaphore>,
) -> Result<TrackWaveform, String> {
    check_cancelled(&cancellation)?;
    let _permit = worker
        .acquire_owned()
        .await
        .map_err(|_| "waveform service is shutting down".to_owned())?;
    check_cancelled(&cancellation)?;

    let uri = SpotifyUri::from_uri(&format!("spotify:track:{requested_track_id}"))
        .map_err(|error| format!("invalid Spotify track id: {error}"))?;
    let item = resolve_audio_item(&session, uri, &cancellation).await?;
    check_cancelled(&cancellation)?;
    let resolved_spotify_id: SpotifyId = (&item.track_id)
        .try_into()
        .map_err(|error| format!("invalid resolved Spotify track id: {error}"))?;
    let (format, file_id, bytes_per_second) = select_file(&item.files, &cache)?;
    let duration_ms = item.duration_ms;

    let path = cache_path(&cache_directory, file_id)?;
    let cached_path = path.clone();
    let cached =
        tokio::task::spawn_blocking(move || read_or_invalidate_artifact(&cached_path, duration_ms))
            .await
            .map_err(|error| format!("waveform cache reader failed: {error}"))??;
    check_cancelled(&cancellation)?;
    if let Some(payload) = cached {
        return waveform_response(requested_track_id, duration_ms, payload);
    }

    check_cancelled(&cancellation)?;
    let encrypted = AudioFile::open(&session, file_id, bytes_per_second)
        .await
        .map_err(|error| format!("could not open track audio: {error}"))?;
    check_cancelled(&cancellation)?;
    let controller = encrypted
        .get_stream_loader_controller()
        .map_err(|error| format!("could not control track audio stream: {error}"))?;
    check_cancelled(&cancellation)?;
    let key = session
        .audio_key()
        .request(resolved_spotify_id, file_id)
        .await
        .map_err(|error| format!("could not decrypt track audio: {error}"))?;
    check_cancelled(&cancellation)?;

    let decoded_cancel = cancellation.clone();
    let decoded = tokio::task::spawn_blocking(move || {
        decode_audio(
            AudioDecrypt::new(Some(key), encrypted),
            controller,
            format,
            duration_ms,
            &decoded_cancel,
        )
    })
    .await
    .map_err(|error| format!("waveform decoder worker failed: {error}"))??;
    check_cancelled(&cancellation)?;

    let write_path = path.clone();
    let decoded = tokio::task::spawn_blocking(move || {
        write_artifact_atomic(&write_path, duration_ms, &decoded)?;
        let directory = write_path
            .parent()
            .ok_or_else(|| "waveform cache path has no parent".to_owned())?;
        prune_cache(directory, MAX_CACHE_BYTES)?;
        Ok::<_, String>(decoded)
    })
    .await
    .map_err(|error| format!("waveform cache writer failed: {error}"))??;
    check_cancelled(&cancellation)?;
    waveform_response(requested_track_id, duration_ms, decoded)
}

async fn resolve_audio_item(
    session: &Session,
    uri: SpotifyUri,
    cancellation: &AtomicBool,
) -> Result<AudioItem, String> {
    check_cancelled(cancellation)?;
    let mut item = AudioItem::get_file(session, uri)
        .await
        .map_err(|error| format!("could not load track audio metadata: {error}"))?;
    check_cancelled(cancellation)?;
    if let Err(error) = &item.availability {
        return Err(format!("track audio is unavailable: {error}"));
    }
    if !item.files.is_empty() {
        return Ok(item);
    }
    let alternatives = item
        .alternatives
        .take()
        .ok_or_else(|| "track has no supported audio file or alternative".to_owned())?;
    for alternative in alternatives.0 {
        check_cancelled(cancellation)?;
        let candidate = AudioItem::get_file(session, alternative).await;
        check_cancelled(cancellation)?;
        if let Ok(candidate) = candidate {
            if candidate.availability.is_ok() && !candidate.files.is_empty() {
                return Ok(candidate);
            }
        }
    }
    Err("track has no available alternative with supported audio".to_owned())
}

fn select_file(
    files: &AudioFiles,
    cache: &Cache,
) -> Result<(AudioFileFormat, FileId, usize), String> {
    let find = |cached_only: bool| {
        SUPPORTED_FORMATS.iter().find_map(|(format, rate)| {
            let file_id = files.get(format).copied()?;
            let is_cached = cache.file_path(file_id).is_some_and(|path| path.is_file());
            (!cached_only || is_cached).then_some((*format, file_id, *rate))
        })
    };
    find(true)
        .or_else(|| find(false))
        .ok_or_else(|| "track has no supported audio file for waveform extraction".to_owned())
}

fn decode_audio(
    decrypted: AudioDecrypt<AudioFile>,
    controller: StreamLoaderController,
    format: AudioFileFormat,
    duration_ms: u32,
    cancellation: &AtomicBool,
) -> Result<Vec<u8>, String> {
    check_cancelled(cancellation)?;
    let offset = if AudioFiles::is_ogg_vorbis(format) {
        SPOTIFY_OGG_HEADER_END
    } else {
        0
    };
    let source = Subfile::new(decrypted, offset, controller.len() as u64)
        .map_err(|error| format!("could not open decoded audio: {error}"))?;
    let mut hint = Hint::new();
    if let Some(mime) = AudioFiles::mime_type(format) {
        hint.mime_type(mime);
    }
    let mut decoder = SymphoniaDecoder::new(source, hint)
        .map_err(|error| format!("could not decode track audio: {error}"))?;
    decoder
        .seek(0)
        .map_err(|error| format!("could not seek decoded audio: {error}"))?;
    controller.set_stream_mode();

    let mut envelope = Envelope::new(duration_ms)?;
    loop {
        check_cancelled(cancellation)?;
        let Some((_position, packet)) = decoder
            .next_packet()
            .map_err(|error| format!("could not decode track waveform: {error}"))?
        else {
            break;
        };
        let samples = packet
            .samples()
            .map_err(|error| format!("decoded waveform was not PCM: {error}"))?;
        envelope.push_interleaved_stereo(samples)?;
        if envelope.is_full() {
            break;
        }
    }
    check_cancelled(cancellation)?;
    Ok(envelope.finish())
}

#[derive(Clone, Copy)]
struct Peak {
    min: i16,
    max: i16,
    occupied: bool,
}

struct Envelope {
    peaks: Vec<Peak>,
    target_frames: u64,
    frames: u64,
    bin_index: usize,
    bin_phase: u64,
    bin_phase_step: u64,
}

impl Envelope {
    fn new(duration_ms: u32) -> Result<Self, String> {
        let bin_count = bin_count(duration_ms);
        let payload_len = bin_count
            .checked_mul(4)
            .ok_or_else(|| "waveform is too large".to_owned())?;
        let artifact_len = WAVEFORM_HEADER_LEN
            .checked_add(payload_len)
            .ok_or_else(|| "waveform artifact length overflowed".to_owned())?;
        if u64::try_from(artifact_len).map_or(true, |length| length > MAX_ARTIFACT_BYTES) {
            return Err("waveform exceeds the 16 MiB artifact limit".to_owned());
        }
        let bin_phase_step = u64::from(INTERVAL_MS).saturating_mul(1_000);
        Ok(Self {
            peaks: vec![
                Peak {
                    min: 0,
                    max: 0,
                    occupied: false
                };
                bin_count
            ],
            target_frames: u64::from(duration_ms).saturating_mul(SAMPLE_RATE) / 1_000,
            frames: 0,
            bin_index: 0,
            bin_phase: 0,
            bin_phase_step,
        })
    }

    fn push_interleaved_stereo(&mut self, samples: &[f64]) -> Result<(), String> {
        if samples.len() % CHANNELS != 0 {
            return Err("decoded stereo packet ended with a partial frame".to_owned());
        }
        for frame in samples.chunks_exact(CHANNELS) {
            if self.is_full() {
                break;
            }
            debug_assert!(self.bin_index < self.peaks.len());
            let bin = self.bin_index;
            let low = quantize(frame[0].min(frame[1]));
            let high = quantize(frame[0].max(frame[1]));
            let peak = &mut self.peaks[bin];
            if peak.occupied {
                peak.min = peak.min.min(low);
                peak.max = peak.max.max(high);
            } else {
                peak.min = low;
                peak.max = high;
                peak.occupied = true;
            }
            self.frames += 1;
            self.bin_phase += self.bin_phase_step;
            if self.bin_phase >= SAMPLE_RATE {
                self.bin_phase -= SAMPLE_RATE;
                self.bin_index += 1;
            }
        }
        Ok(())
    }

    fn is_full(&self) -> bool {
        self.frames >= self.target_frames
    }

    fn finish(self) -> Vec<u8> {
        let mut payload = Vec::with_capacity(self.peaks.len() * 4);
        for peak in self.peaks {
            let (min, max) = if peak.occupied {
                (peak.min, peak.max)
            } else {
                (0, 0)
            };
            payload.extend_from_slice(&min.to_le_bytes());
            payload.extend_from_slice(&max.to_le_bytes());
        }
        payload
    }
}

fn quantize(sample: f64) -> i16 {
    let sample = if sample.is_finite() { sample } else { 0.0 };
    if sample <= -1.0 {
        i16::MIN
    } else if sample >= 1.0 {
        i16::MAX
    } else {
        (sample * f64::from(i16::MAX)).round() as i16
    }
}

fn bin_count(duration_ms: u32) -> usize {
    usize::try_from(u64::from(duration_ms).div_ceil(u64::from(INTERVAL_MS))).unwrap_or(usize::MAX)
}

#[cfg(test)]
fn bin_for_frame(frame: u64) -> usize {
    usize::try_from(frame.saturating_mul(1_000) / (SAMPLE_RATE * u64::from(INTERVAL_MS)))
        .unwrap_or(usize::MAX)
}

fn waveform_response(
    track_id: String,
    duration_ms: u32,
    payload: Vec<u8>,
) -> Result<TrackWaveform, String> {
    validate_payload(duration_ms, &payload)?;
    Ok(TrackWaveform {
        track_id,
        duration_ms,
        interval_ms: INTERVAL_MS,
        bin_count: u32::try_from(payload.len() / 4)
            .map_err(|_| "waveform contains too many bins".to_owned())?,
        peaks_base64: BASE64.encode(payload),
    })
}

fn cache_path(directory: &Path, file_id: FileId) -> Result<PathBuf, String> {
    let key = file_id
        .to_base16()
        .map_err(|error| format!("could not encode waveform cache key: {error}"))?;
    Ok(directory.join(format!("{key}.wfm")))
}

fn read_or_invalidate_artifact(
    path: &Path,
    expected_duration_ms: u32,
) -> Result<Option<Vec<u8>>, String> {
    match read_artifact(path, expected_duration_ms) {
        Ok(result) => Ok(result),
        Err(error) => {
            if let Err(remove_error) = fs::remove_file(path) {
                if remove_error.kind() != io::ErrorKind::NotFound {
                    return Err(format!(
                        "malformed waveform cache could not be removed ({error}): {remove_error}"
                    ));
                }
            }
            eprintln!(
                "regenerating malformed waveform cache {}: {error}",
                path.display()
            );
            Ok(None)
        }
    }
}

fn read_artifact(path: &Path, expected_duration_ms: u32) -> Result<Option<Vec<u8>>, String> {
    let metadata = match fs::metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(format!("could not inspect waveform cache: {error}")),
    };
    if metadata.len() > MAX_ARTIFACT_BYTES || metadata.len() < WAVEFORM_HEADER_LEN as u64 {
        return Err("waveform cache artifact has an invalid size".to_owned());
    }
    let bytes =
        fs::read(path).map_err(|error| format!("could not read waveform cache: {error}"))?;
    if u64::try_from(bytes.len()).map_or(true, |length| length > MAX_ARTIFACT_BYTES)
        || bytes.len() < WAVEFORM_HEADER_LEN
    {
        return Err("waveform cache artifact changed size while being read".to_owned());
    }
    if &bytes[0..4] != b"WFM1" {
        return Err("waveform cache magic is invalid".to_owned());
    }
    if le_u16(&bytes[4..6]) != WAVEFORM_CACHE_VERSION
        || usize::from(le_u16(&bytes[6..8])) != WAVEFORM_HEADER_LEN
    {
        return Err("waveform cache version is unsupported".to_owned());
    }
    let duration_ms = le_u32(&bytes[8..12]);
    let interval_ms = le_u16(&bytes[12..14]);
    let reserved = le_u16(&bytes[14..16]);
    let stored_bins = usize::try_from(le_u32(&bytes[16..20]))
        .map_err(|_| "waveform cache bin count is too large".to_owned())?;
    let payload_len = usize::try_from(le_u32(&bytes[20..24]))
        .map_err(|_| "waveform cache payload length is too large".to_owned())?;
    let checksum = le_u32(&bytes[24..28]);
    if duration_ms != expected_duration_ms || interval_ms != INTERVAL_MS || reserved != 0 {
        return Err("waveform cache metadata does not match the track".to_owned());
    }
    let expected_payload_len = stored_bins
        .checked_mul(4)
        .ok_or_else(|| "waveform cache payload length overflowed".to_owned())?;
    let expected_artifact_len = WAVEFORM_HEADER_LEN
        .checked_add(payload_len)
        .ok_or_else(|| "waveform cache artifact length overflowed".to_owned())?;
    if payload_len != expected_payload_len || expected_artifact_len != bytes.len() {
        return Err("waveform cache length is invalid".to_owned());
    }
    let payload = bytes[WAVEFORM_HEADER_LEN..].to_vec();
    if crc32(&payload) != checksum {
        return Err("waveform cache checksum is invalid".to_owned());
    }
    validate_payload(duration_ms, &payload)?;
    Ok(Some(payload))
}

fn write_artifact_atomic(path: &Path, duration_ms: u32, payload: &[u8]) -> Result<(), String> {
    validate_payload(duration_ms, payload)?;
    let total_len = WAVEFORM_HEADER_LEN
        .checked_add(payload.len())
        .ok_or_else(|| "waveform artifact length overflowed".to_owned())?;
    if u64::try_from(total_len).map_or(true, |length| length > MAX_ARTIFACT_BYTES) {
        return Err("waveform exceeds the 16 MiB artifact limit".to_owned());
    }
    let stored_bins = u32::try_from(payload.len() / 4)
        .map_err(|_| "waveform contains too many bins".to_owned())?;
    let payload_len =
        u32::try_from(payload.len()).map_err(|_| "waveform payload is too large".to_owned())?;
    let parent = path
        .parent()
        .ok_or_else(|| "waveform cache path has no parent".to_owned())?;
    fs::create_dir_all(parent)
        .map_err(|error| format!("could not create waveform cache: {error}"))?;
    let sequence = TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let temp = parent.join(format!(".wfm-{}-{sequence}.tmp", std::process::id()));
    let write_result = (|| -> Result<(), String> {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp)
            .map_err(|error| format!("could not create waveform cache artifact: {error}"))?;
        file.write_all(b"WFM1")
            .and_then(|_| file.write_all(&WAVEFORM_CACHE_VERSION.to_le_bytes()))
            .and_then(|_| file.write_all(&(WAVEFORM_HEADER_LEN as u16).to_le_bytes()))
            .and_then(|_| file.write_all(&duration_ms.to_le_bytes()))
            .and_then(|_| file.write_all(&INTERVAL_MS.to_le_bytes()))
            .and_then(|_| file.write_all(&0u16.to_le_bytes()))
            .and_then(|_| file.write_all(&stored_bins.to_le_bytes()))
            .and_then(|_| file.write_all(&payload_len.to_le_bytes()))
            .and_then(|_| file.write_all(&crc32(payload).to_le_bytes()))
            .and_then(|_| file.write_all(payload))
            .and_then(|_| file.sync_all())
            .map_err(|error| format!("could not write waveform cache artifact: {error}"))?;
        replace_file_atomically(&temp, path)
            .map_err(|error| format!("could not commit waveform cache artifact: {error}"))?;
        Ok(())
    })();
    if write_result.is_err() {
        let _ = fs::remove_file(&temp);
    }
    write_result
}

#[cfg(not(windows))]
fn replace_file_atomically(source: &Path, destination: &Path) -> io::Result<()> {
    fs::rename(source, destination)
}

#[cfg(windows)]
fn replace_file_atomically(source: &Path, destination: &Path) -> io::Result<()> {
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
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

fn validate_payload(duration_ms: u32, payload: &[u8]) -> Result<(), String> {
    let expected = bin_count(duration_ms)
        .checked_mul(4)
        .ok_or_else(|| "waveform payload length overflowed".to_owned())?;
    let total_len = WAVEFORM_HEADER_LEN
        .checked_add(payload.len())
        .ok_or_else(|| "waveform artifact length overflowed".to_owned())?;
    if payload.len() != expected
        || u64::try_from(total_len).map_or(true, |length| length > MAX_ARTIFACT_BYTES)
    {
        return Err("waveform payload length does not match its duration".to_owned());
    }
    for pair in payload.chunks_exact(4) {
        if i16::from_le_bytes([pair[0], pair[1]]) > i16::from_le_bytes([pair[2], pair[3]]) {
            return Err("waveform cache contains an inverted peak".to_owned());
        }
    }
    Ok(())
}

fn prune_cache(directory: &Path, limit: u64) -> Result<(), String> {
    let mut total = 0u64;
    let mut files = Vec::new();
    for entry in fs::read_dir(directory)
        .map_err(|error| format!("could not read waveform cache: {error}"))?
    {
        let entry =
            entry.map_err(|error| format!("could not read waveform cache entry: {error}"))?;
        let metadata = entry
            .metadata()
            .map_err(|error| format!("could not inspect waveform cache entry: {error}"))?;
        if !metadata.is_file() {
            continue;
        }
        let size = metadata.len();
        total = total.saturating_add(size);
        let accessed = metadata
            .accessed()
            .or_else(|_| metadata.modified())
            .unwrap_or(SystemTime::UNIX_EPOCH);
        files.push((accessed, entry.path(), size));
    }
    files.sort_unstable_by(|left, right| left.0.cmp(&right.0).then_with(|| left.1.cmp(&right.1)));
    for (_, path, size) in files {
        if total <= limit {
            break;
        }
        fs::remove_file(&path).map_err(|error| {
            format!("could not prune waveform cache {}: {error}", path.display())
        })?;
        total = total.saturating_sub(size);
    }
    Ok(())
}

fn remove_temporary_files(directory: &Path) {
    let Ok(entries) = fs::read_dir(directory) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().is_some_and(|extension| extension == "tmp") {
            let _ = fs::remove_file(path);
        }
    }
}

fn check_cancelled(cancellation: &AtomicBool) -> Result<(), String> {
    if cancellation.load(Ordering::Acquire) {
        Err("waveform request was cancelled".to_owned())
    } else {
        Ok(())
    }
}

fn le_u16(bytes: &[u8]) -> u16 {
    u16::from_le_bytes([bytes[0], bytes[1]])
}

fn le_u32(bytes: &[u8]) -> u32 {
    u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]])
}

fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = !0u32;
    for byte in bytes {
        crc ^= u32::from(*byte);
        for _ in 0..8 {
            crc = (crc >> 1) ^ (0xedb8_8320 & (0u32.wrapping_sub(crc & 1)));
        }
    }
    !crc
}

struct Subfile<T: Read + Seek> {
    stream: T,
    offset: u64,
    length: u64,
}

impl<T: Read + Seek> Subfile<T> {
    fn new(mut stream: T, offset: u64, length: u64) -> io::Result<Self> {
        stream.seek(SeekFrom::Start(offset))?;
        Ok(Self {
            stream,
            offset,
            length: length.saturating_sub(offset),
        })
    }
}

impl<T: Read + Seek> Read for Subfile<T> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        let position = self.stream.stream_position()?.saturating_sub(self.offset);
        let remaining = self.length.saturating_sub(position);
        let allowed = usize::try_from(remaining.min(buffer.len() as u64)).unwrap_or(buffer.len());
        self.stream.read(&mut buffer[..allowed])
    }
}

impl<T: Read + Seek> Seek for Subfile<T> {
    fn seek(&mut self, position: SeekFrom) -> io::Result<u64> {
        let absolute = match position {
            SeekFrom::Start(value) => self.offset.saturating_add(value),
            SeekFrom::Current(value) => self.stream.stream_position()?.saturating_add_signed(value),
            SeekFrom::End(value) => self
                .offset
                .saturating_add(self.length)
                .saturating_add_signed(value),
        };
        let clamped = absolute.clamp(self.offset, self.offset.saturating_add(self.length));
        self.stream.seek(SeekFrom::Start(clamped))?;
        Ok(clamped - self.offset)
    }
}

impl<T> MediaSource for Subfile<T>
where
    T: Read + Seek + Send + Sync,
{
    fn is_seekable(&self) -> bool {
        true
    }

    fn byte_len(&self) -> Option<u64> {
        Some(self.length)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    struct ScratchDir(PathBuf);

    impl ScratchDir {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "spotify-waveform-test-{}-{}",
                std::process::id(),
                TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir_all(&path).unwrap();
            Self(path)
        }
    }

    impl Drop for ScratchDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn peak(payload: &[u8], index: usize) -> (i16, i16) {
        let start = index * 4;
        (
            i16::from_le_bytes([payload[start], payload[start + 1]]),
            i16::from_le_bytes([payload[start + 2], payload[start + 3]]),
        )
    }

    #[test]
    fn aggregation_quantization_and_one_millisecond_bins_are_exact() {
        let mut envelope = Envelope::new(3).unwrap();
        let mut first = vec![0.0; 45 * 2];
        first[0] = -1.0;
        first[1] = 1.0;
        first[2] = -0.5;
        first[3] = 0.25;
        envelope.push_interleaved_stereo(&first).unwrap();
        envelope
            .push_interleaved_stereo(&vec![0.5; 44 * 2])
            .unwrap();
        let payload = envelope.finish();
        assert_eq!(payload.len(), 12);
        assert_eq!(peak(&payload, 0), (i16::MIN, i16::MAX));
        assert_eq!(peak(&payload, 1), (16_384, 16_384));
        assert_eq!(peak(&payload, 2), (0, 0));
        assert_eq!(bin_for_frame(44), 0);
        assert_eq!(bin_for_frame(45), 1);
        assert_eq!(quantize(f64::NAN), 0);
        assert_eq!(quantize(-0.5), -16_384);
    }

    #[test]
    fn short_decode_is_zero_filled_without_stretching() {
        let mut envelope = Envelope::new(3).unwrap();
        envelope.push_interleaved_stereo(&[-0.25, 0.25]).unwrap();
        let payload = envelope.finish();
        assert_eq!(payload.len(), 12);
        assert_eq!(peak(&payload, 0), (-8_192, 8_192));
        assert_eq!(peak(&payload, 1), (0, 0));
        assert_eq!(peak(&payload, 2), (0, 0));
    }

    #[test]
    fn selector_prefers_any_cached_supported_file_then_lowest_rate() {
        let scratch = ScratchDir::new();
        let cache = Cache::new(
            None::<&Path>,
            None::<&Path>,
            Some(scratch.0.as_path()),
            None,
        )
        .unwrap();
        let low = FileId::from_raw(&[1; 20]);
        let high = FileId::from_raw(&[2; 20]);
        let mut files = AudioFiles::default();
        files.insert(AudioFileFormat::OGG_VORBIS_96, low);
        files.insert(AudioFileFormat::OGG_VORBIS_320, high);
        let high_path = cache.file_path(high).unwrap();
        fs::create_dir_all(high_path.parent().unwrap()).unwrap();
        fs::write(&high_path, b"cached").unwrap();
        assert_eq!(select_file(&files, &cache).unwrap().1, high);
        fs::remove_file(high_path).unwrap();
        assert_eq!(select_file(&files, &cache).unwrap().1, low);
    }

    #[test]
    fn cache_round_trip_keying_and_corruption_validation() {
        let scratch = ScratchDir::new();
        let file_id = FileId::from_raw(&[0xab; 20]);
        let path = cache_path(&scratch.0, file_id).unwrap();
        assert!(
            path.file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("abababab")
        );
        let mut envelope = Envelope::new(20).unwrap();
        envelope.push_interleaved_stereo(&[-1.0, 1.0]).unwrap();
        let payload = envelope.finish();
        write_artifact_atomic(&path, 20, &payload).unwrap();
        assert_eq!(read_artifact(&path, 20).unwrap().unwrap(), payload);
        write_artifact_atomic(&path, 20, &payload)
            .expect("atomic cache writes replace an existing artifact");
        let mut coarse = fs::read(&path).unwrap();
        coarse[4..6].copy_from_slice(&1u16.to_le_bytes());
        coarse[12..14].copy_from_slice(&10u16.to_le_bytes());
        fs::write(&path, coarse).unwrap();
        assert!(read_artifact(&path, 20).is_err());
        assert!(read_or_invalidate_artifact(&path, 20).unwrap().is_none());
        assert!(!path.exists(), "coarse cache artifacts are invalidated");
        write_artifact_atomic(&path, 20, &payload).unwrap();
        let mut corrupt = fs::read(&path).unwrap();
        *corrupt.last_mut().unwrap() ^= 1;
        fs::write(&path, corrupt).unwrap();
        assert!(read_artifact(&path, 20).is_err());
        assert!(read_or_invalidate_artifact(&path, 20).unwrap().is_none());
        assert!(
            !path.exists(),
            "malformed artifacts are removed for regeneration"
        );
        write_artifact_atomic(&path, 20, &payload).unwrap();
        assert_eq!(read_artifact(&path, 20).unwrap().unwrap(), payload);
        assert!(read_artifact(&path, 21).is_err());
    }

    #[test]
    fn prune_removes_oldest_files_until_bounded() {
        let scratch = ScratchDir::new();
        let first = scratch.0.join("first.wfm");
        let second = scratch.0.join("second.wfm");
        fs::write(&first, vec![0u8; 8]).unwrap();
        std::thread::sleep(Duration::from_millis(20));
        fs::write(&second, vec![0u8; 8]).unwrap();
        prune_cache(&scratch.0, 8).unwrap();
        assert!(!first.exists());
        assert!(second.exists());
    }

    #[test]
    fn same_track_deduplicates_and_late_cancelled_outcomes_are_generation_safe() {
        let mut jobs = JobBook::default();
        let (first_generation, first_cancel) = jobs
            .join_or_insert("track", "request-1".to_owned())
            .unwrap();
        assert!(
            jobs.join_or_insert("track", "request-2".to_owned())
                .is_none()
        );
        assert_eq!(jobs.cancel("track"), ["request-1", "request-2"]);
        assert!(first_cancel.load(Ordering::Acquire));
        let (second_generation, _) = jobs
            .join_or_insert("track", "request-3".to_owned())
            .unwrap();
        assert_ne!(first_generation, second_generation);
        assert!(jobs.finish("track", first_generation).is_empty());
        assert_eq!(jobs.finish("track", second_generation), ["request-3"]);
    }

    #[tokio::test]
    async fn global_worker_permit_bounds_decode_concurrency_to_one() {
        let worker = Arc::new(Semaphore::new(1));
        let first = worker.clone().acquire_owned().await.unwrap();
        assert!(worker.clone().try_acquire_owned().is_err());
        drop(first);
        assert!(worker.try_acquire_owned().is_ok());
    }

    #[test]
    fn subfile_reads_and_seeks_only_inside_its_window() {
        let cursor = std::io::Cursor::new((0u8..100).collect::<Vec<_>>());
        let mut subfile = Subfile::new(cursor, 10, 90).unwrap();
        let mut contents = Vec::new();
        subfile.read_to_end(&mut contents).unwrap();
        assert_eq!(contents, (10u8..90).collect::<Vec<_>>());
        assert_eq!(subfile.seek(SeekFrom::End(50)).unwrap(), 80);
        assert_eq!(subfile.seek(SeekFrom::Current(-200)).unwrap(), 0);
    }
}
