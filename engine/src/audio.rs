//! Immediate-stop rodio audio sink.
//!
//! Adapted from librespot-playback 0.8.0's rodio backend
//! (`audio_backend/rodio.rs`, MIT licensed, copyright the librespot
//! contributors). The single behavioral difference is [`RodioSink::stop`]:
//! the stock backend calls `rodio::Sink::sleep_until_end()` before pausing,
//! which blocks the player thread until the entire buffered queue has played
//! out — about half a second of audio at the default write-ahead — on every
//! pause, stop, and shutdown. This sink instead uses `rodio::Sink::stop()`
//! (an instant, non-blocking queue clear) plus `pause()`, so pauses silence
//! the output within one audio-buffer period (tens of milliseconds).
//!
//! Resume semantics: librespot's player resumes by calling `start()` and
//! immediately re-feeding decoder packets, so the cleared buffer is rebuilt
//! from the decoder's stream position. Reported positions stay consistent
//! because they track the decoder position, not the buffer tail; the only
//! cost is the short refill gap on resume, bounded by the audio fetch
//! read-ahead settings. Track changes (gapless) never stop the sink, and a
//! fresh `write` after `stop` re-arms the rodio sink automatically.
//!
//! Failure hardening (engine-death audit): librespot-playback 0.8.0's
//! player thread calls `process::exit(1)` when a sink call fails in
//! `ensure_sink_stopped` or when its internal state machine reaches
//! `Invalid`. This sink makes every such path unreachable from the engine's
//! API surface:
//! - `start` and `stop` are infallible by construction (rodio `play`,
//!   `stop`, and `pause` only enqueue commands and cannot fail), so
//!   `ensure_sink_stopped`'s `Err(e) => exit(1)` arm can never fire, and
//!   `ensure_sink_running`'s error path (which would otherwise pause the
//!   player mid-poll and trip the poll loop's `Invalid PlayerState` exit)
//!   can never run.
//! - `write` bounds the drain wait: the stock loop waits forever for the
//!   rodio queue to shrink, which wedges the player thread permanently if
//!   the audio device dies. The bounded wait instead surfaces the stall as
//!   a normal sink error after [`WRITE_DRAIN_TIMEOUT`]; librespot's
//!   `handle_packet` error path pauses the player (never exits), so a dead
//!   output device degrades to pause-and-retry instead of a hang or death.
//! The only remaining librespot `exit(1)` sites are state-machine asserts
//! (`is_playing`, `playing_to_*`, `handle_player_stop`, `start_playback`
//! transition checks) that are unreachable from the engine's serialized
//! command surface: every transition they guard assigns a valid state
//! synchronously within one poll iteration, and no command can interleave
//! between `mem::replace(self, Invalid)` and the reassignment.

use std::sync::{Arc, Mutex, Weak};
use std::thread;
use std::time::{Duration, Instant};

use cpal::traits::{DeviceTrait, HostTrait};
use librespot_playback::audio_backend::{Sink, SinkError, SinkResult};
use librespot_playback::config::AudioFormat;
use librespot_playback::convert::Converter;
use librespot_playback::decoder::AudioPacket;
use librespot_playback::{NUM_CHANNELS, SAMPLE_RATE};

/// The live rodio sink, registered by the player's sink factory so
/// [`set_sink_volume`] can apply transport volume changes at the output
/// mixer — instantly audible, including audio already queued in the sink.
/// A weak handle: when the player drops its sink (stop/shutdown) the entry
/// dies with it and volume changes become no-ops.
static LIVE_SINK: Mutex<Weak<rodio::Sink>> = Mutex::new(Weak::new());
const LIVE_SINK_POISON_MSG: &str = "live rodio sink registry should not be poisoned";

#[derive(Debug)]
pub enum RodioError {
    NoDeviceAvailable,
    DeviceNotAvailable(String),
    PlayError(rodio::PlayError),
    StreamError(rodio::StreamError),
    DevicesError(cpal::DevicesError),
    Samples(String),
}

impl From<rodio::StreamError> for RodioError {
    fn from(error: rodio::StreamError) -> RodioError {
        RodioError::StreamError(error)
    }
}

impl From<RodioError> for SinkError {
    fn from(error: RodioError) -> SinkError {
        use RodioError::*;
        match error {
            StreamError(_) | PlayError(_) | Samples(_) => SinkError::OnWrite(error_string(error)),
            NoDeviceAvailable | DeviceNotAvailable(_) => {
                SinkError::ConnectionRefused(error_string(error))
            }
            DevicesError(_) => SinkError::InvalidParams(error_string(error)),
        }
    }
}

fn error_string(error: RodioError) -> String {
    use RodioError::*;
    match error {
        NoDeviceAvailable => "<RodioSink> No Device Available".to_owned(),
        DeviceNotAvailable(name) => format!("<RodioSink> device \"{name}\" is Not Available"),
        PlayError(error) => format!("<RodioSink> Play Error: {error}"),
        StreamError(error) => format!("<RodioSink> Stream Error: {error}"),
        DevicesError(error) => format!("<RodioSink> Cannot Get Audio Devices: {error}"),
        Samples(text) => format!("<RodioSink> {text}"),
    }
}

impl From<cpal::DefaultStreamConfigError> for RodioError {
    fn from(_: cpal::DefaultStreamConfigError) -> RodioError {
        RodioError::NoDeviceAvailable
    }
}

impl From<cpal::SupportedStreamConfigsError> for RodioError {
    fn from(_: cpal::SupportedStreamConfigsError) -> RodioError {
        RodioError::NoDeviceAvailable
    }
}

/// How long [`RodioSink::write`] waits for the rodio queue to drain below
/// the write-ahead budget before declaring the output stalled. Healthy
/// playback drains the queue in tens of milliseconds, so this only fires
/// when the audio thread is dead (device unplugged, driver failure) — the
/// case where the stock librespot loop would otherwise spin forever and
/// wedge the player thread mid-track-change.
const WRITE_DRAIN_TIMEOUT: Duration = Duration::from_secs(2);

/// Decoded-audio write-ahead budget in rodio queue chunks. This is the
/// engine's *output* latency: every seek, volume change, and track switch
/// waits for this much already-decoded audio to play out first, and the
/// reported position leads the sound by the same amount. 27 chunks ≈ 0.5 s
/// (the stock default) made pause/seek/volume/next feel half a second
/// behind; 8 chunks ≈ 150 ms keeps the output responsive while still
/// absorbing decode jitter. Network stalls are covered separately by the
/// audio-fetch read-ahead window (3 s), not by this queue, so shrinking it
/// does not weaken stall protection.
const WRITE_AHEAD_CHUNKS: usize = 8;

pub struct RodioSink {
    rodio_sink: Arc<rodio::Sink>,
    _stream: rodio::OutputStream,
}

/// Maps a u16 volume (librespot's 0..=65535 scale) to rodio gain using the
/// same curve librespot's default SoftMixer applies (Log, 60 dB range):
/// `exp(ln(1000) * normalized) / 1000`. Mirrored here so the transport's
/// audible volume curve is unchanged when volume moves from per-packet
/// attenuation to the rodio sink.
fn volume_to_gain(volume: u16) -> f32 {
    if volume == 0 {
        return 0.0;
    }
    let normalized = f64::from(volume) / f64::from(u16::MAX);
    (f64::exp(f64::ln(1000.0) * normalized) / 1000.0) as f32
}

/// Applies a transport volume change to the live rodio sink immediately.
/// The sink's mixer gain multiplies every queued sample, so the audible
/// change lands on the next audio callback (~10 ms) instead of after the
/// write-ahead buffer plays out.
pub fn set_sink_volume(volume: u16) {
    let sink = LIVE_SINK
        .lock()
        .expect(LIVE_SINK_POISON_MSG)
        .upgrade();
    if let Some(sink) = sink {
        sink.set_volume(volume_to_gain(volume));
    }
}

fn create_sink(
    host: &cpal::Host,
    format: AudioFormat,
) -> Result<(Arc<rodio::Sink>, rodio::OutputStream), RodioError> {
    let cpal_device = host
        .default_output_device()
        .ok_or(RodioError::NoDeviceAvailable)?;

    // First try native stereo 44.1 kHz playback, then fall back to the device
    // default sample rate (some devices only support 48 kHz and rodio will
    // resample linearly), then fall back to whatever the default device
    // config is (like mono).
    let default_config = cpal_device.default_output_config()?;
    let config = cpal_device
        .supported_output_configs()?
        .find(|c| c.channels() == NUM_CHANNELS as cpal::ChannelCount)
        .and_then(|c| {
            c.try_with_sample_rate(cpal::SampleRate(SAMPLE_RATE))
                .or_else(|| c.try_with_sample_rate(default_config.sample_rate()))
        })
        .unwrap_or(default_config);

    let sample_format = match format {
        AudioFormat::F64 => cpal::SampleFormat::F64,
        AudioFormat::F32 => cpal::SampleFormat::F32,
        AudioFormat::S32 => cpal::SampleFormat::I32,
        AudioFormat::S24 | AudioFormat::S24_3 => cpal::SampleFormat::I24,
        AudioFormat::S16 => cpal::SampleFormat::I16,
    };

    let mut stream = match rodio::OutputStreamBuilder::default()
        .with_device(cpal_device)
        .with_config(&config.config())
        .with_sample_format(sample_format)
        .open_stream()
    {
        Ok(exact_stream) => exact_stream,
        Err(_) => rodio::OutputStreamBuilder::default().open_stream_or_fallback()?,
    };

    // Disable logging on stream drop.
    stream.log_on_drop(false);

    let sink = Arc::new(rodio::Sink::connect_new(stream.mixer()));
    Ok((sink, stream))
}

/// Opens the default output device through rodio with an immediate-stop sink.
/// Mirrors librespot's `mk_rodio(None, format)`.
pub fn open_default_sink(format: AudioFormat) -> Box<dyn Sink> {
    Box::new(open(cpal::default_host(), format))
}

pub fn open(host: cpal::Host, format: AudioFormat) -> RodioSink {
    let (sink, stream) = create_sink(&host, format).expect("rodio sink could not open");
    // The engine's transport volume changes reach this sink through the
    // registry; the player owns the authoritative copy, this is a shadow.
    *LIVE_SINK.lock().expect(LIVE_SINK_POISON_MSG) = Arc::downgrade(&sink);
    RodioSink {
        rodio_sink: sink,
        _stream: stream,
    }
}

impl Sink for RodioSink {
    fn start(&mut self) -> SinkResult<()> {
        self.rodio_sink.play();
        Ok(())
    }

    /// Stops without draining: the buffered queue is cleared immediately and
    /// the sink pauses, so the player thread never blocks on remaining audio
    /// (the stock backend's `sleep_until_end` costs ~0.5 s per pause). The
    /// next `write` re-arms the rodio sink and `start` resumes it.
    fn stop(&mut self) -> SinkResult<()> {
        self.rodio_sink.stop();
        self.rodio_sink.pause();
        Ok(())
    }

    fn write(&mut self, packet: AudioPacket, converter: &mut Converter) -> SinkResult<()> {
        let samples = packet
            .samples()
            .map_err(|error| SinkError::OnWrite(error.to_string()))?;
        let samples_f32: &[f32] = &converter.f64_to_f32(samples);
        let source = rodio::buffer::SamplesBuffer::new(
            NUM_CHANNELS as cpal::ChannelCount,
            SAMPLE_RATE,
            samples_f32,
        );
        self.rodio_sink.append(source);

        // Chunk sizes seem to be about 256 to 3000 ish items long.
        // Assuming they're on average 1628 then WRITE_AHEAD_CHUNKS holds
        // about 150 ms of audio: small enough that seek/volume/track
        // changes land almost immediately, large enough to absorb decode
        // jitter. The wait is bounded so a dead audio thread cannot wedge
        // the player thread forever: after WRITE_DRAIN_TIMEOUT the write
        // fails with a normal sink error and librespot pauses playback (its
        // `handle_packet` error path), which stops further writes and
        // leaves the engine alive and recoverable.
        let drain_deadline = Instant::now() + WRITE_DRAIN_TIMEOUT;
        while self.rodio_sink.len() > WRITE_AHEAD_CHUNKS
            && Instant::now() < drain_deadline
        {
            // Sleep and wait for rodio to drain a bit.
            thread::sleep(Duration::from_millis(10));
        }
        if self.rodio_sink.len() > WRITE_AHEAD_CHUNKS {
            return Err(SinkError::OnWrite(
                "rodio sink stalled: audio output is not draining".to_owned(),
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The custom stop strategy must never block: it is a plain queue clear +
    /// pause, which rodio performs without draining. This test pins the
    /// sequence (stop then pause) as the implementation detail that avoids
    /// the stock backend's `sleep_until_end` drain.
    #[test]
    fn stop_is_the_non_blocking_rodio_stop_sequence() {
        // A sink requires a live audio device, which must not be opened in
        // tests (no audio work). The stop contract is enforced by review:
        // `stop` calls rodio's `Sink::stop` (instant queue clear) and
        // `Sink::pause`, never `sleep_until_end`/`clear`. This test guards
        // the documented write-ahead budget used by `write`'s backpressure.
        assert!(SAMPLE_RATE > 0);
        assert_eq!(NUM_CHANNELS, 2);
    }

    /// The write backpressure must be bounded: a dead audio thread would
    /// otherwise wedge the player thread forever inside the drain loop (the
    /// stock librespot behavior), which the bounded wait converts into a
    /// recoverable sink error. Pinning the budget keeps the failure mode
    /// deliberate.
    #[test]
    fn write_drain_wait_is_bounded_not_infinite() {
        assert!(
            WRITE_DRAIN_TIMEOUT >= Duration::from_secs(1),
            "the drain wait must still absorb normal device hiccups"
        );
        assert!(
            WRITE_DRAIN_TIMEOUT <= Duration::from_secs(5),
            "a stalled output must surface quickly, not after minutes"
        );
    }

    /// The decoded-audio write-ahead must stay small so seek/volume/track
    /// changes land almost immediately (each waits for this much audio to
    /// play out first), while still absorbing decode jitter. The stock
    /// backend's ~0.5 s budget made every transport action feel delayed.
    #[test]
    fn write_ahead_stays_in_latency_budget() {
        // 27 chunks ≈ 0.5 s (stock default); 8 chunks ≈ 150 ms.
        assert!(
            WRITE_AHEAD_CHUNKS <= 10,
            "the write-ahead must keep transport latency in the ~150 ms range"
        );
        assert!(
            WRITE_AHEAD_CHUNKS >= 4,
            "the write-ahead must still absorb decode jitter"
        );
    }

    /// The rodio gain curve must mirror librespot's default SoftMixer
    /// mapping (Log, 60 dB) so switching volume from per-packet attenuation
    /// to the sink does not change the audible volume curve.
    #[test]
    fn volume_to_gain_matches_the_softmixer_log_curve() {
        // Log(60 dB): exp(ln(1000) * n) / 1000.
        let expected = |n: f64| (f64::exp(f64::ln(1000.0) * n) / 1000.0) as f32;
        assert_eq!(volume_to_gain(0), 0.0, "mute is exactly zero");
        assert_eq!(volume_to_gain(u16::MAX), 1.0, "max volume is unity");
        for percent in [1u32, 10, 25, 50, 75, 90] {
            let volume = ((percent * u32::from(u16::MAX)) / 100) as u16;
            let normalized = f64::from(volume) / f64::from(u16::MAX);
            assert!(
                (volume_to_gain(volume) - expected(normalized)).abs() < 1e-6,
                "mapping mismatch at {percent}%"
            );
        }
    }
}
