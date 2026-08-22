use std::io::{self, Read, Seek, SeekFrom};

use librespot_audio::{AudioDecrypt, AudioFile};
use librespot_core::{Session, SpotifyId, SpotifyUri};
use librespot_metadata::audio::{AudioFileFormat, AudioFiles, AudioItem};
use librespot_playback::decoder::{AudioDecoder, SymphoniaDecoder};
use spotify_playback_engine::protocol::{TrackWaveform, WaveformPeak};
use symphonia::core::io::MediaSource;
use symphonia::core::probe::Hint;

const SPOTIFY_OGG_HEADER_END: u64 = 0xa7;
const MIN_POINTS: u16 = 64;
const MAX_POINTS: u16 = 4_096;

pub async fn extract(
    session: &Session,
    track_id: &str,
    requested_points: u16,
) -> Result<TrackWaveform, String> {
    let uri = SpotifyUri::from_uri(&format!("spotify:track:{track_id}"))
        .map_err(|error| format!("invalid Spotify track id: {error}"))?;
    let spotify_id: SpotifyId = (&uri)
        .try_into()
        .map_err(|error| format!("invalid Spotify track id: {error}"))?;
    let item = AudioItem::get_file(session, uri)
        .await
        .map_err(|error| format!("could not load track audio metadata: {error}"))?;
    if let Err(error) = &item.availability {
        return Err(format!("track audio is unavailable: {error}"));
    }
    let (format, file_id, bytes_per_second) = select_file(&item)?;
    let encrypted = AudioFile::open(session, file_id, bytes_per_second)
        .await
        .map_err(|error| format!("could not open track audio: {error}"))?;
    let controller = encrypted
        .get_stream_loader_controller()
        .map_err(|error| format!("could not control track audio stream: {error}"))?;
    let key = session
        .audio_key()
        .request(spotify_id, file_id)
        .await
        .map_err(|error| format!("could not decrypt track audio: {error}"))?;
    let decrypted = AudioDecrypt::new(Some(key), encrypted);
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

    let points = usize::from(requested_points.clamp(MIN_POINTS, MAX_POINTS));
    let mut peaks = vec![
        WaveformPeak {
            min: 1.0,
            max: -1.0
        };
        points
    ];
    let total_frames = (u64::from(item.duration_ms) * 44_100 / 1_000).max(1);
    let mut frame_index = 0u64;
    while let Some((_position, packet)) = decoder
        .next_packet()
        .map_err(|error| format!("could not decode track waveform: {error}"))?
    {
        let samples = packet
            .samples()
            .map_err(|error| format!("decoded waveform was not PCM: {error}"))?;
        for frame in samples.chunks_exact(2) {
            let bin = ((frame_index * points as u64) / total_frames)
                .min(points.saturating_sub(1) as u64) as usize;
            let low = frame[0].min(frame[1]).clamp(-1.0, 1.0) as f32;
            let high = frame[0].max(frame[1]).clamp(-1.0, 1.0) as f32;
            peaks[bin].min = peaks[bin].min.min(low);
            peaks[bin].max = peaks[bin].max.max(high);
            frame_index += 1;
        }
    }
    for peak in &mut peaks {
        if peak.min > peak.max {
            *peak = WaveformPeak { min: 0.0, max: 0.0 };
        }
    }
    Ok(TrackWaveform {
        track_id: track_id.to_owned(),
        duration_ms: item.duration_ms,
        peaks,
    })
}

fn select_file(
    item: &AudioItem,
) -> Result<(AudioFileFormat, librespot_core::FileId, usize), String> {
    const FORMATS: &[(AudioFileFormat, usize)] = &[
        (AudioFileFormat::OGG_VORBIS_320, 40 * 1_024),
        (AudioFileFormat::MP3_320, 40 * 1_024),
        (AudioFileFormat::MP3_256, 32 * 1_024),
        (AudioFileFormat::OGG_VORBIS_160, 20 * 1_024),
        (AudioFileFormat::MP3_160, 20 * 1_024),
        (AudioFileFormat::OGG_VORBIS_96, 12 * 1_024),
        (AudioFileFormat::MP3_96, 12 * 1_024),
    ];
    FORMATS
        .iter()
        .find_map(|(format, rate)| {
            item.files
                .get(format)
                .copied()
                .map(|id| (*format, id, *rate))
        })
        .ok_or_else(|| "track has no supported audio file for waveform extraction".to_owned())
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
        self.stream.read(buffer)
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

    #[test]
    fn requested_envelope_size_is_strictly_bounded() {
        assert_eq!(usize::from(0u16.clamp(MIN_POINTS, MAX_POINTS)), 64);
        assert_eq!(usize::from(u16::MAX.clamp(MIN_POINTS, MAX_POINTS)), 4_096);
    }

    #[test]
    fn subfile_never_seeks_outside_its_visible_window() {
        let cursor = std::io::Cursor::new(vec![0u8; 100]);
        let mut subfile = Subfile::new(cursor, 10, 90).unwrap();
        assert_eq!(subfile.seek(SeekFrom::End(50)).unwrap(), 80);
        assert_eq!(subfile.seek(SeekFrom::Current(-200)).unwrap(), 0);
    }
}
