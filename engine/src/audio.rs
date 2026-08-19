//! Immediate-stop rodio audio sink.
//!
//! Adapted from librespot-playback 0.8.0's rodio backend
//! (`audio_backend/rodio.rs`, MIT licensed, copyright the librespot
//! contributors). Two behavioral differences from the stock backend:
//!
//! 1. [`RodioSink::stop`]: the stock backend calls
//!    `rodio::Sink::sleep_until_end()` before pausing, which blocks the player
//!    thread until the entire buffered queue has played out — about half a
//!    second of audio at the default write-ahead — on every pause, stop, and
//!    shutdown. This sink drops the buffered audio and pauses instead, so
//!    pauses silence the output within one audio-buffer period.
//! 2. [`SampleRing`]/[`LiveSource`]: the stock backend appends every decoded
//!    packet to the sink as its own `rodio::Source`. That is the cause of a
//!    measured playback-rate error; see "Playback rate" below.
//!
//! Resume semantics: librespot's player resumes by calling `start()` and
//! immediately re-feeding decoder packets, so the dropped buffer is rebuilt
//! from the decoder's stream position. Reported positions stay consistent
//! because they track the decoder position, not the buffer tail; the only
//! cost is the short refill gap on resume, bounded by the audio fetch
//! read-ahead settings. Track changes (gapless) never stop the sink, and a
//! fresh `write` after `stop` refills the ring automatically.
//!
//! # Playback rate
//!
//! The output device on the reference rig exposes only 48 kHz, so rodio
//! resamples librespot's 44.1 kHz packets. rodio's mixer wraps its input in
//! one `UniformSourceIterator`, which rebuilds its `SampleRateConverter` at
//! every *span* boundary, and each rebuild costs a fraction of a frame
//! (the converter primes on two input frames and drains its interpolation
//! state when the span ends). Spans are not free-running: rodio's
//! `SourcesQueueOutput::current_span_len` reports the current source's
//! `size_hint`, and `SamplesBuffer::size_hint` returns the buffer's *full*
//! length regardless of how much has been read, so appending one
//! `SamplesBuffer` per decoded packet produces roughly one converter rebuild
//! per packet.
//!
//! This was measured on the reference rig itself, by instrumenting the graph
//! with two counters — input frames handed to rodio, and output frames the
//! device consumed — and differencing them between two interior marks (3 s and
//! 23 s of audio) so start-up and drain effects cancel. The correct ratio is
//! 48000/44100 = 1.0884354 output frames per input frame:
//!
//! | samples per packet | ratio     | error    |
//! |--------------------|-----------|----------|
//! | 256                | 1.0937486 | +0.4882% |
//! | 368                | 1.0917131 | +0.3011% |
//! | 912                | 1.0896387 | +0.1106% |
//! | 1628               | 1.0884514 | +0.0015% |
//! | one long source    | 1.0884402 | +0.0004% |
//!
//! The error is a true, steady ratio error — the extra frames arrive at packet
//! rate (~100-200/s), not in bursts — which is why re-aligning a recording
//! anywhere never nulls and why playing it against the reference produces
//! continuous phasing. It is not caused by any single mis-set rate: the stream
//! opens at exactly 48 kHz, rodio's mixer is built from that same config, and
//! 48000/44100 reduces to exactly 160/147. It is emergent from restarting the
//! converter ~100-200 times a second.
//!
//! Spotify's Ogg Vorbis uses blocksizes 256/2048, so symphonia hands librespot
//! 128-, 576-, or 1024-frame packets (256/1152/2048 samples) and `write` is
//! called once per packet. The +0.223% (33.5 ms over 15 s) reported against
//! the official client sits between the 368- and 912-sample rows, i.e. squarely
//! inside that range. The exact figure depends on the short/long block mix of
//! the material, which is why it is a range and not a single number.
//!
//! With one continuous source the ratio is 1.08844 and, importantly, no longer
//! depends on packet size at all: 256-sample and 912-sample packets both
//! measure +0.0004%.
//!
//! The fix is to hand rodio a *single* long-lived source ([`LiveSource`]) fed
//! through a bounded ring ([`SampleRing`]), so the converter keeps continuous
//! state. Note that `current_span_len() == None` is not sufficient on its own:
//! rodio's queue then falls back to `size_hint().0`, and the `Iterator`
//! default of `0` makes it report its 512-sample `THRESHOLD` instead, which
//! measures *worse* (+0.1294%) than per-packet appending. The span length is
//! load-bearing, which is why [`LiveSource::size_hint`] reports
//! [`RODIO_MAX_SPAN_SAMPLES`].
//!
//! Ruled out while diagnosing this, recorded so it is not re-investigated:
//! - *Queue underrun silence.* rodio's queue does splice in 512 samples of
//!   silence when it runs dry (`SourcesQueueOutput::go_next`), but with the
//!   write-ahead budget below it never ran dry in measurement; inserted
//!   silence was zero in every run.
//! - *Mixer/stream rate mismatch.* `rodio::stream::OutputStream::open` builds
//!   the mixer with `mixer(config.channel_count, config.sample_rate)` from the
//!   same config it hands `build_output_stream`, so the mixer's conversion
//!   target can never disagree with the rate the device opened at — including
//!   on the `open_stream_or_fallback` path, which rebuilds both together.
//!
//! Failure hardening (engine-death audit): librespot-playback 0.8.0's
//! player thread calls `process::exit(1)` when a sink call fails in
//! `ensure_sink_stopped` or when its internal state machine reaches
//! `Invalid`. This sink makes every such path unreachable from the engine's
//! API surface:
//! - `start` and `stop` are infallible by construction (rodio `play` and
//!   `pause` only set an atomic, and clearing the ring cannot fail), so
//!   `ensure_sink_stopped`'s `Err(e) => exit(1)` arm can never fire, and
//!   `ensure_sink_running`'s error path (which would otherwise pause the
//!   player mid-poll and trip the poll loop's `Invalid PlayerState` exit)
//!   can never run.
//! - `write` bounds the wait for buffer space: the stock loop waits forever
//!   for the rodio queue to shrink, which wedges the player thread
//!   permanently if the audio device dies. The bounded wait instead surfaces
//!   the stall as a normal sink error after [`WRITE_DRAIN_TIMEOUT`];
//!   librespot's `handle_packet` error path pauses the player (never exits),
//!   so a dead output device degrades to pause-and-retry instead of a hang
//!   or death.
//! The only remaining librespot `exit(1)` sites are state-machine asserts
//! (`is_playing`, `playing_to_*`, `handle_player_stop`, `start_playback`
//! transition checks) that are unreachable from the engine's serialized
//! command surface: every transition they guard assigns a valid state
//! synchronously within one poll iteration, and no command can interleave
//! between `mem::replace(self, Invalid)` and the reassignment.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, PoisonError, Weak};
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

/// How long [`RodioSink::write`] waits for ring space before declaring the
/// output stalled. Healthy playback frees a chunk every few tens of
/// milliseconds, so this only fires when the audio thread is dead (device
/// unplugged, driver failure) — the case where the stock librespot loop would
/// otherwise spin forever and wedge the player thread mid-track-change.
const WRITE_DRAIN_TIMEOUT: Duration = Duration::from_secs(2);

/// Decoded-audio write-ahead budget, in interleaved samples (~150 ms). This is
/// the engine's *output* latency: every seek, volume change, and track switch
/// waits for this much already-decoded audio to play out first, and the
/// reported position leads the sound by the same amount. librespot's stock
/// budget is ~0.5 s, which made pause/seek/volume/next feel half a second
/// behind. Network stalls are covered separately by the audio-fetch read-ahead
/// window (3 s), not by this ring, so keeping it small does not weaken stall
/// protection.
const WRITE_AHEAD_SAMPLES: usize =
    SAMPLE_RATE as usize * NUM_CHANNELS as usize * 150 / 1000;

/// The largest span rodio will read before rebuilding its rate converter:
/// `UniformSourceIterator::bootstrap` clamps the reported span with
/// `.map(|x| x.min(32768))`. [`LiveSource`] reports this from `size_hint` so
/// rodio always takes the longest span it is willing to take, which minimises
/// converter rebuilds (see "Playback rate" above). Reporting less — including
/// the `Iterator` default of `0`, which makes rodio's queue substitute its own
/// 512-sample `THRESHOLD` — measurably increases drift.
const RODIO_MAX_SPAN_SAMPLES: usize = 32_768;

/// rodio bootstraps the mixer's first span from an empty queue, whose
/// `current` is a `rodio::source::Empty` claiming 1 channel at 48 kHz. That
/// first span is therefore read with the wrong channel count and no
/// resampling, and it is created inside `Sink::connect_new` before anything
/// can be appended. [`LiveSource`] opens with exactly this many samples of
/// silence so the mangled span consumes silence instead of the first moments
/// of the first track. It is a one-off at sink open, and if rodio ever changes
/// its `THRESHOLD` the only cost is a few milliseconds of misrendered audio at
/// start-up — never drift, because the second span onwards reads from this
/// source and sees the correct rate and channel count.
const RODIO_BOOTSTRAP_SPAN_SAMPLES: usize = 512;

/// Audio handed from the player thread to the audio callback.
///
/// The producer ([`RodioSink::write`]) pushes whole decoded packets and blocks
/// with a deadline when the ring is full; the consumer ([`LiveSource`]) takes
/// one packet at a time. Locking once per packet (~20 ms) rather than once per
/// sample keeps the audio callback's cost negligible, and rodio's own sink
/// already takes several mutexes on the audio thread every 5 ms
/// (`Sink::append`'s `periodic_access` closure), so this adds no new class of
/// contention.
struct SampleRing {
    state: Mutex<RingState>,
    /// Signalled when the consumer frees space, and when [`SampleRing::clear`]
    /// empties the ring, so a blocked producer wakes promptly instead of
    /// polling. This replaces the stock backend's 10 ms sleep loop, which
    /// burned idle CPU in an app whose whole point is not to.
    space_freed: Condvar,
    /// Bumped by [`SampleRing::clear`]. [`LiveSource`] reads it on every frame
    /// boundary and drops the packet it is mid-way through when it changes, so
    /// `stop()` silences audio already handed to the audio thread rather than
    /// letting up to a packet of stale sound trail the stop.
    generation: AtomicU64,
}

struct RingState {
    packets: VecDeque<Vec<f32>>,
    /// Kept alongside `packets` so the producer's fullness check is a field
    /// read rather than a walk of the deque.
    queued_samples: usize,
}

impl SampleRing {
    fn new() -> Arc<Self> {
        Arc::new(SampleRing {
            state: Mutex::new(RingState {
                packets: VecDeque::with_capacity(64),
                queued_samples: 0,
            }),
            space_freed: Condvar::new(),
            generation: AtomicU64::new(0),
        })
    }

    /// A poisoned ring must not take the audio callback or the player thread
    /// down with it: every field is updated in one uninterruptible step, so the
    /// state behind a poisoned lock is always consistent and safe to reuse.
    fn lock(&self) -> std::sync::MutexGuard<'_, RingState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Test-only: the production paths read the field under a lock they
    /// already hold, so taking a second one here would be wasted work.
    #[cfg(test)]
    fn queued_samples(&self) -> usize {
        self.lock().queued_samples
    }

    /// Appends a packet, waiting up to `timeout` for the consumer to make
    /// room. Returns `Err` if the ring never drained, which the caller
    /// surfaces as a sink error rather than blocking the player thread
    /// forever.
    fn push(&self, packet: Vec<f32>, timeout: Duration) -> Result<(), ()> {
        let deadline = Instant::now() + timeout;
        let mut state = self.lock();
        while state.queued_samples >= WRITE_AHEAD_SAMPLES {
            let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
                return Err(());
            };
            // `wait_timeout` can wake spuriously, so the fullness test above is
            // the loop condition rather than the timeout result.
            let (guard, _) = self
                .space_freed
                .wait_timeout(state, remaining)
                .unwrap_or_else(PoisonError::into_inner);
            state = guard;
        }
        state.queued_samples += packet.len();
        state.packets.push_back(packet);
        Ok(())
    }

    /// Takes the next packet, or `None` when the producer has fallen behind.
    fn pop(&self) -> Option<Vec<f32>> {
        let mut state = self.lock();
        let packet = state.packets.pop_front()?;
        state.queued_samples -= packet.len();
        drop(state);
        self.space_freed.notify_one();
        Some(packet)
    }

    /// Drops every queued packet and tells [`LiveSource`] to drop the one it
    /// holds. Non-blocking: this is what makes `stop()` instant.
    fn clear(&self) {
        let mut state = self.lock();
        state.packets.clear();
        state.queued_samples = 0;
        self.generation.fetch_add(1, Ordering::Release);
        drop(state);
        self.space_freed.notify_all();
    }
}

/// The single `rodio::Source` the sink appends, for the lifetime of the sink.
///
/// It never ends and never yields `None`: an empty ring produces silence, so
/// rodio's queue is never asked for a next source and its converter keeps
/// running with continuous state.
struct LiveSource {
    ring: Arc<SampleRing>,
    /// The generation this source is playing; a mismatch means `stop()` ran.
    generation: u64,
    packet: Vec<f32>,
    pos: usize,
    /// Samples of silence still owed. Silence is always emitted in whole
    /// frames so an underrun (or the start-up priming) can never shift the
    /// channel interleave and swap left with right.
    silence_remaining: usize,
}

impl LiveSource {
    fn new(ring: Arc<SampleRing>) -> Self {
        let generation = ring.generation.load(Ordering::Acquire);
        LiveSource {
            ring,
            generation,
            packet: Vec::new(),
            pos: 0,
            silence_remaining: RODIO_BOOTSTRAP_SPAN_SAMPLES,
        }
    }
}

impl Iterator for LiveSource {
    type Item = rodio::Sample;

    fn next(&mut self) -> Option<Self::Item> {
        if self.silence_remaining > 0 {
            self.silence_remaining -= 1;
            return Some(0.0);
        }

        // Dropping buffered audio part-way through a frame would swap the
        // channels, so only act on a stop once the frame is complete. The
        // check costs one uncontended atomic load per sample.
        if self.pos % NUM_CHANNELS as usize == 0 {
            let generation = self.ring.generation.load(Ordering::Acquire);
            if generation != self.generation {
                self.generation = generation;
                self.packet.clear();
                self.pos = 0;
            }
        }

        if self.pos == self.packet.len() {
            match self.ring.pop() {
                Some(packet) => {
                    self.packet = packet;
                    self.pos = 0;
                }
                None => {
                    // Underrun, or simply paused/stopped. Packets hold whole
                    // frames, so this is always a frame boundary.
                    self.silence_remaining = NUM_CHANNELS as usize - 1;
                    return Some(0.0);
                }
            }
        }

        let sample = self.packet[self.pos];
        self.pos += 1;
        Some(sample)
    }

    /// rodio's queue derives its span from this (see [`RODIO_MAX_SPAN_SAMPLES`]);
    /// it is not an iterator-length promise, and nothing in the playback path
    /// treats it as one.
    fn size_hint(&self) -> (usize, Option<usize>) {
        (RODIO_MAX_SPAN_SAMPLES, None)
    }
}

impl rodio::Source for LiveSource {
    /// `None` means "the rate and channel count never change", which is true:
    /// this source outlives every track. The actual span length rodio reads
    /// before rebuilding its converter comes from `size_hint`.
    fn current_span_len(&self) -> Option<usize> {
        None
    }

    fn channels(&self) -> rodio::ChannelCount {
        NUM_CHANNELS as rodio::ChannelCount
    }

    fn sample_rate(&self) -> rodio::SampleRate {
        SAMPLE_RATE
    }

    fn total_duration(&self) -> Option<Duration> {
        None
    }
}

pub struct RodioSink {
    rodio_sink: Arc<rodio::Sink>,
    ring: Arc<SampleRing>,
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

/// Picks the stereo output config to open, in descending order of how little
/// conversion it forces on the audio.
///
/// The stock backend took the *first* stereo config the device listed and
/// asked it for 44.1 kHz. That is arbitrary: on the reference dongle the first
/// stereo entry is U8, and it only happens to be harmless because
/// `SupportedStreamConfig::config()` drops the sample format and
/// `with_sample_format` overrides it from librespot's `AudioFormat`. A device
/// that exposes different rate ranges per format would get a rate chosen by
/// list order. Preferring a config that actually supports 44.1 kHz matters
/// most: at the native rate rodio skips resampling entirely.
fn select_output_config(
    device: &cpal::Device,
    sample_format: cpal::SampleFormat,
    default_config: &cpal::SupportedStreamConfig,
) -> Result<cpal::SupportedStreamConfig, RodioError> {
    let stereo: Vec<cpal::SupportedStreamConfigRange> = device
        .supported_output_configs()?
        .filter(|c| c.channels() == NUM_CHANNELS as cpal::ChannelCount)
        .collect();

    let at_rate = |rate: cpal::SampleRate, want_format: bool| {
        stereo
            .iter()
            .filter(|c| !want_format || c.sample_format() == sample_format)
            .find_map(|c| c.clone().try_with_sample_rate(rate))
    };

    let native = cpal::SampleRate(SAMPLE_RATE);
    let device_rate = default_config.sample_rate();

    Ok(
        // 1. Native 44.1 kHz in the format librespot will feed: no rate
        //    conversion and no sample-format conversion.
        at_rate(native, true)
            // 2. Native 44.1 kHz in some other format. Still no resampling,
            //    which is what drives drift; cpal converts the sample type.
            .or_else(|| at_rate(native, false))
            // 3. The device's own rate in the requested format. rodio resamples,
            //    which the single continuous source keeps rate-accurate.
            .or_else(|| at_rate(device_rate, true))
            // 4. The device's own rate in any format.
            .or_else(|| at_rate(device_rate, false))
            // 5. Whatever the device defaults to, which may not be stereo.
            .unwrap_or_else(|| default_config.clone()),
    )
}

fn create_sink(
    host: &cpal::Host,
    format: AudioFormat,
) -> Result<(Arc<rodio::Sink>, rodio::OutputStream), RodioError> {
    let cpal_device = host
        .default_output_device()
        .ok_or(RodioError::NoDeviceAvailable)?;

    let sample_format = match format {
        AudioFormat::F64 => cpal::SampleFormat::F64,
        AudioFormat::F32 => cpal::SampleFormat::F32,
        AudioFormat::S32 => cpal::SampleFormat::I32,
        AudioFormat::S24 | AudioFormat::S24_3 => cpal::SampleFormat::I24,
        AudioFormat::S16 => cpal::SampleFormat::I16,
    };

    let default_config = cpal_device.default_output_config()?;
    let config = select_output_config(&cpal_device, sample_format, &default_config)?;

    // The fallback cannot introduce a rate error: rodio builds the mixer from
    // the same config it hands cpal (`OutputStream::open`), so whatever config
    // wins here, the mixer's conversion target matches the rate the device
    // actually opened at.
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

    // One source, appended once, for the lifetime of the sink: this is what
    // keeps rodio's rate converter running with continuous state.
    let ring = SampleRing::new();
    sink.append(LiveSource::new(ring.clone()));

    RodioSink {
        rodio_sink: sink,
        ring,
        _stream: stream,
    }
}

impl Sink for RodioSink {
    fn start(&mut self) -> SinkResult<()> {
        self.rodio_sink.play();
        Ok(())
    }

    /// Stops without draining: the buffered audio is dropped immediately and
    /// the sink pauses, so the player thread never blocks on remaining audio
    /// (the stock backend's `sleep_until_end` costs ~0.5 s per pause). The
    /// next `write` refills the ring and `start` resumes it.
    ///
    /// This deliberately does not call `rodio::Sink::stop`, which the previous
    /// per-packet version used: `Sink::stop` ends the current source for good,
    /// and with a single long-lived source that would tear down the very thing
    /// keeping the rate converter continuous. It would also make the next
    /// `Sink::append` call `sleep_until_end()` — exactly the blocking wait this
    /// sink exists to avoid. Pausing is equally instant and keeps the source
    /// alive; a paused rodio sink stops pulling from the source entirely
    /// (`Pausable` emits silence without polling its input), so the refilled
    /// ring is not consumed while stopped.
    fn stop(&mut self) -> SinkResult<()> {
        self.ring.clear();
        self.rodio_sink.pause();
        Ok(())
    }

    fn write(&mut self, packet: AudioPacket, converter: &mut Converter) -> SinkResult<()> {
        let samples = packet
            .samples()
            .map_err(|error| SinkError::OnWrite(error.to_string()))?;
        let samples_f32 = converter.f64_to_f32(samples);

        // A partial frame would shift the interleave for everything after it
        // and swap the channels, so refuse it rather than corrupt the stream.
        if !samples_f32.len().is_multiple_of(NUM_CHANNELS as usize) {
            return Err(SinkError::OnWrite(format!(
                "decoder produced a partial frame: {} samples is not a multiple of {NUM_CHANNELS}",
                samples_f32.len()
            )));
        }

        // Backpressure: the ring holds about WRITE_AHEAD_SAMPLES (~150 ms),
        // small enough that seek/volume/track changes land almost immediately,
        // large enough to absorb decode jitter. The wait is bounded so a dead
        // audio thread cannot wedge the player thread forever: after
        // WRITE_DRAIN_TIMEOUT the write fails with a normal sink error and
        // librespot pauses playback (its `handle_packet` error path), which
        // stops further writes and leaves the engine alive and recoverable.
        self.ring
            .push(samples_f32, WRITE_DRAIN_TIMEOUT)
            .map_err(|()| {
                SinkError::OnWrite("rodio sink stalled: audio output is not draining".to_owned())
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rodio::Source as _;

    fn packet(samples: usize) -> Vec<f32> {
        vec![1.0; samples]
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
        let ms = WRITE_AHEAD_SAMPLES as f64 / f64::from(SAMPLE_RATE)
            / f64::from(NUM_CHANNELS)
            * 1000.0;
        assert!(
            (100.0..=200.0).contains(&ms),
            "the write-ahead must keep transport latency near 150 ms, got {ms:.0} ms"
        );
    }

    /// A full ring must not block the player thread forever. This is the
    /// property that keeps a dead audio device recoverable.
    #[test]
    fn push_gives_up_when_the_ring_never_drains() {
        let ring = SampleRing::new();
        while ring.queued_samples() < WRITE_AHEAD_SAMPLES {
            ring.push(packet(512), Duration::from_millis(50))
                .expect("space is available");
        }
        let started = Instant::now();
        assert!(
            ring.push(packet(512), Duration::from_millis(50)).is_err(),
            "a ring that never drains must fail the write, not block"
        );
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "the failure must arrive at the deadline, not later"
        );
    }

    /// Popping must free space for the producer, otherwise playback deadlocks
    /// after the first ~150 ms.
    #[test]
    fn popping_frees_space_for_the_producer() {
        let ring = SampleRing::new();
        while ring.queued_samples() < WRITE_AHEAD_SAMPLES {
            ring.push(packet(512), Duration::from_millis(50)).unwrap();
        }
        let popped = ring.pop().expect("a packet is queued");
        assert_eq!(popped.len(), 512);
        ring.push(packet(512), Duration::from_millis(50))
            .expect("popping made room");
    }

    /// `stop()` must drop queued audio instantly, including the packet the
    /// audio thread is part-way through, so a pause is not trailed by up to a
    /// packet of stale sound.
    #[test]
    fn clear_drops_queued_audio_and_the_packet_in_flight() {
        let ring = SampleRing::new();
        ring.push(packet(8), Duration::from_millis(50)).unwrap();
        ring.push(packet(8), Duration::from_millis(50)).unwrap();

        let mut source = LiveSource::new(ring.clone());
        // Consume the start-up priming silence and enter the first packet.
        for _ in 0..RODIO_BOOTSTRAP_SPAN_SAMPLES {
            assert_eq!(source.next(), Some(0.0));
        }
        assert_eq!(source.next(), Some(1.0), "real audio follows the priming");

        ring.clear();
        assert_eq!(ring.queued_samples(), 0, "queued packets are dropped");
        // The in-flight packet is dropped at the next frame boundary: one more
        // sample completes the current frame, then everything is silence.
        let tail: Vec<f32> = (0..16).filter_map(|_| source.next()).collect();
        assert!(
            tail[1..].iter().all(|s| *s == 0.0),
            "audio already handed to the source must stop within one frame: {tail:?}"
        );
    }

    /// Underrun silence must be a whole number of frames. A single stray
    /// sample would shift the interleave for the rest of the session and swap
    /// left with right.
    #[test]
    fn underrun_silence_is_frame_aligned() {
        let ring = SampleRing::new();
        // Two frames of audio, then nothing.
        ring.push(vec![1.0, 2.0, 3.0, 4.0], Duration::from_millis(50))
            .unwrap();
        let mut source = LiveSource::new(ring.clone());
        for _ in 0..RODIO_BOOTSTRAP_SPAN_SAMPLES {
            source.next();
        }
        assert_eq!(
            (0..4).filter_map(|_| source.next()).collect::<Vec<_>>(),
            vec![1.0, 2.0, 3.0, 4.0]
        );

        // Underrun: silence arrives, and when audio resumes it must land on
        // the left channel again.
        let silence: Vec<f32> = (0..6).filter_map(|_| source.next()).collect();
        assert!(silence.iter().all(|s| *s == 0.0));
        assert!(
            silence.len().is_multiple_of(NUM_CHANNELS as usize),
            "silence must be emitted in whole frames"
        );
        ring.push(vec![5.0, 6.0], Duration::from_millis(50)).unwrap();
        assert_eq!(source.next(), Some(5.0), "audio resumes on the left channel");
        assert_eq!(source.next(), Some(6.0));
    }

    /// The start-up priming exists so rodio's first span — bootstrapped from
    /// an empty queue that claims 1 channel at 48 kHz — consumes silence
    /// instead of the first moments of the first track.
    #[test]
    fn source_primes_with_rodios_bootstrap_span_of_silence() {
        let ring = SampleRing::new();
        ring.push(vec![1.0; 4], Duration::from_millis(50)).unwrap();
        let mut source = LiveSource::new(ring);
        for i in 0..RODIO_BOOTSTRAP_SPAN_SAMPLES {
            assert_eq!(source.next(), Some(0.0), "priming sample {i} must be silent");
        }
        assert_eq!(source.next(), Some(1.0));
        assert!(
            RODIO_BOOTSTRAP_SPAN_SAMPLES.is_multiple_of(NUM_CHANNELS as usize),
            "priming silence must be a whole number of frames"
        );
    }

    /// The source must describe itself to rodio exactly as an endless 44.1 kHz
    /// stereo stream, and must report a span at rodio's cap. The span is the
    /// load-bearing part: reporting less makes rodio rebuild its rate
    /// converter more often, and every rebuild costs a fraction of a frame.
    #[test]
    fn source_reports_a_continuous_full_span_stream() {
        let source = LiveSource::new(SampleRing::new());
        assert_eq!(source.sample_rate(), SAMPLE_RATE);
        assert_eq!(source.channels(), NUM_CHANNELS as rodio::ChannelCount);
        assert_eq!(source.current_span_len(), None, "the format never changes");
        assert_eq!(source.total_duration(), None, "the source never ends");
        assert_eq!(
            source.size_hint().0,
            RODIO_MAX_SPAN_SAMPLES,
            "rodio clamps spans to 32768; asking for less means more converter rebuilds"
        );
    }

    /// End-to-end rate accuracy through the real rodio graph.
    ///
    /// Builds the same `Sink` + `Mixer` pair `create_sink` builds (minus the
    /// cpal device, which only supplies a clock) at the reference rig's 48 kHz,
    /// feeds a known number of 44.1 kHz frames in realistically varying packet
    /// sizes, and checks how many frames come out. This graph is a faithful
    /// model of the device path: run against the real dongle it reproduced the
    /// same figures to four decimals (256-sample packets measured +0.4882%
    /// both offline and on hardware). Appending one `SamplesBuffer` per packet
    /// drifts by +0.0015%..+0.4882% depending on packet size; one continuous
    /// source must stay accurate regardless of it. See "Playback rate" in the
    /// module docs.
    #[test]
    fn continuous_source_keeps_the_resampled_rate_accurate() {
        const OUT_RATE: rodio::SampleRate = 48_000;
        const BLOCK_FRAMES: usize = 480;
        const SECONDS: usize = 5;

        let (sink, queue) = rodio::Sink::new();
        let (mixer_in, mut mixer_out) =
            rodio::mixer::mixer(NUM_CHANNELS as rodio::ChannelCount, OUT_RATE);
        mixer_in.add(queue);

        let ring = SampleRing::new();
        sink.append(LiveSource::new(ring.clone()));

        // Packet sizes cycle through what symphonia hands librespot for
        // Spotify's Ogg Vorbis (blocksizes 256/2048 => 128/576/1024 frames).
        let sizes = [256usize, 2048, 1152, 2048, 256, 1024, 2048, 512];
        let total_samples = SECONDS * SAMPLE_RATE as usize * NUM_CHANNELS as usize;

        let mut fed_samples = 0usize;
        let mut next_size = 0usize;
        let mut frame = 0usize;
        let (mut first, mut last) = (None, None);
        let mut silent_blocks = 0usize;
        let mut interior_silent_frames = 0usize;

        loop {
            while fed_samples < total_samples && ring.queued_samples() < WRITE_AHEAD_SAMPLES {
                let n = sizes[next_size % sizes.len()].min(total_samples - fed_samples);
                next_size += 1;
                ring.push(packet(n), Duration::from_millis(50)).unwrap();
                fed_samples += n;
            }
            let mut block_had_audio = false;
            for _ in 0..BLOCK_FRAMES {
                let left = mixer_out.next().unwrap_or(0.0);
                let right = mixer_out.next().unwrap_or(0.0);
                if left != 0.0 || right != 0.0 {
                    first.get_or_insert(frame);
                    last = Some(frame);
                    block_had_audio = true;
                } else if first.is_some() {
                    interior_silent_frames += 1;
                }
                frame += 1;
            }
            // An empty ring is not an empty pipeline: the source still holds
            // the packet it is playing and the converter holds interpolation
            // state. The source never ends, so the only way to know the tail
            // has been rendered is to keep pulling until the output goes and
            // stays quiet. If the tail were genuinely dropped rather than
            // merely un-pulled, no amount of extra pulling would recover it
            // and the frame count below would come up short.
            silent_blocks = if block_had_audio { 0 } else { silent_blocks + 1 };
            if fed_samples == total_samples && silent_blocks >= 4 {
                break;
            }
        }
        // Silence counted after the last real frame is drain, not a gap.
        interior_silent_frames -= frame - 1 - last.expect("audio was rendered");

        let rendered = last.expect("audio was rendered") - first.expect("audio was rendered") + 1;
        let fed_frames = total_samples / NUM_CHANNELS as usize;
        let expected = fed_frames as f64 * f64::from(OUT_RATE) / f64::from(SAMPLE_RATE);
        let error_pct = (rendered as f64 - expected) / expected * 100.0;

        assert_eq!(
            interior_silent_frames, 0,
            "rodio's queue splices in silence when it runs dry; the write-ahead \
             budget must keep it fed"
        );
        assert!(
            error_pct.abs() < 0.01,
            "resampled output drifted {error_pct:+.4}% ({rendered} frames, expected {expected:.1}); \
             per-packet appending measured +0.05%..+0.49% here"
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
