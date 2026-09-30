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
//!    shutdown. This sink pauses immediately: user pauses retain audio, while
//!    discontinuous stops discard it. Both silence output within one buffer.
//! 2. [`SampleRing`]/[`LiveSource`]: the stock backend appends every decoded
//!    packet to the sink as its own `rodio::Source`. That is the cause of a
//!    measured playback-rate error; see "Playback rate and fidelity" below.
//!
//! Resume semantics: a user pause retains the consumer's in-flight packet,
//! queued packets, WSOLA overlap and resampler history. Resume continues those
//! samples before decoding ahead, and the consumer clock excludes paused wall
//! time. Loads and seeks explicitly invalidate this retained audio; a speed
//! change does not. Natural track boundaries remain gapless.
//!
//! # Playback rate and fidelity
//!
//! librespot decodes at 44.1 kHz, and the device rate is not negotiable: cpal
//! opens WASAPI in shared mode only and does not set
//! `AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM`, so `IsFormatSupported` accepts nothing
//! but the rate Windows is configured for. On a machine set to 48 kHz — the
//! common case — [`select_output_config`] cannot find 44.1 kHz to ask for, and
//! every sample is resampled on the way out. Two separate defects came out of
//! that: one in *how many* frames arrived, one in *what was in them*.
//!
//! ## Frame count
//!
//! rodio's mixer wraps its queue in one `UniformSourceIterator`, which rebuilds
//! its `SampleRateConverter` at every *span* boundary, and each rebuild loses
//! the converter's rational phase and treats the last input frame as a new
//! endpoint. `SourcesQueueOutput` derives that span from the current source's
//! `current_span_len` or `size_hint`, so appending every decoded packet as its
//! own `SamplesBuffer` — what the stock backend does — makes every packet a
//! span. Measured on the reference rig by instrumenting input frames handed to
//! rodio against output frames the device consumed, differencing two interior
//! marks (3 s and 23 s) so start-up and drain cancel, against the correct ratio
//! 48000/44100 = 160/147:
//!
//! | samples per packet | ratio     | error    |
//! |--------------------|-----------|----------|
//! | 256                | 1.0937486 | +0.4882% |
//! | 368                | 1.0917131 | +0.3011% |
//! | 912                | 1.0896387 | +0.1106% |
//! | 1628               | 1.0884514 | +0.0015% |
//! | one long source    | 1.0884402 | +0.0004% |
//!
//! Spotify's Ogg Vorbis blocks arrive as 256-2048 interleaved samples per
//! `write`, so the +0.223% (33.5 ms over 15 s) originally reported against the
//! official client sits inside that range. [`LiveSource`] is the answer: one
//! endless source, appended once, so decoder packets are not visible to rodio
//! at all. What remained after that was a much smaller residue from rodio's own
//! forced spans — about three frames in fifteen seconds.
//!
//! ## Sample values
//!
//! That residue is now zero, because rodio no longer resamples at all.
//!
//! `SampleRateConverter` is, in its own documentation's words, "simple linear
//! interpolation for up-sampling": a straight line drawn between adjacent
//! samples. It is a poor reconstruction of a band-limited signal, and its error
//! grows with the square of frequency — invisible in the bass, severe in the
//! treble. Measured against the analytically exact resampling of a tone (the
//! same measurement the [`crate::resample`] tests make):
//!
//! | tone   | rodio linear | [`crate::resample`] |
//! |--------|--------------|---------------------|
//! | 100 Hz | -94.6 dB     | -133.5 dB           |
//! | 1 kHz  | -54.6 dB     | -120.9 dB           |
//! | 5 kHz  | -26.8 dB     | -118.8 dB           |
//! | 10 kHz | -15.0 dB     | -115.8 dB           |
//! | 15 kHz |  -8.5 dB     | -131.2 dB           |
//!
//! An error 15 dB below the music at 10 kHz is far louder than anything the
//! 320 kbps bitrate choice is there to protect; it was the dominant artefact in
//! the entire signal path. [`RodioSink::write`] therefore resamples to the
//! device rate itself, through [`crate::resample`], before the samples reach the
//! ring. [`LiveSource`] then reports the *device* rate, which sends both of
//! rodio's converters down their `from == to` branches — exact pass-throughs
//! that consume no priming frames. Nothing downstream touches the audio.
//!
//! This retires the frame-count problem by construction rather than by
//! management: [`crate::resample`] tracks position as an exact rational, so the
//! output frame count is determined for any input length and no longer depends
//! on rodio's span behaviour.
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
//!   or death. The sink also reports that stall as
//!   [`AudioSignal::OutputStalled`], because the pause is all librespot says
//!   about it and the engine has to know the device is what stopped.
//! - the device is opened by [`SinkOpener`] rather than from a librespot sink
//!   builder, which cannot report a failure at all: a machine with no output
//!   device — a fresh boot whose dongle the driver has not recognised yet —
//!   used to panic on the player thread from inside that builder, and the
//!   engine answered by shutting the session down. [`open`] returns the typed
//!   [`RodioError`] instead, and `auth::create_playback` turns it into a state
//!   the engine recovers from without losing the session.
//! The only remaining librespot `exit(1)` sites are state-machine asserts
//! (`is_playing`, `playing_to_*`, `handle_player_stop`, `start_playback`
//! transition checks) that are unreachable from the engine's serialized
//! command surface: every transition they guard assigns a valid state
//! synchronously within one poll iteration, and no command can interleave
//! between `mem::replace(self, Invalid)` and the reassignment.

use std::collections::VecDeque;
use std::fmt;
use std::sync::atomic::{AtomicBool, AtomicU16, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, PoisonError, Weak};
use std::time::{Duration, Instant};

use cpal::traits::{DeviceTrait, HostTrait};
use librespot_playback::audio_backend::{Sink, SinkError, SinkResult};
use librespot_playback::config::AudioFormat;
use librespot_playback::convert::Converter;
use librespot_playback::decoder::AudioPacket;
use librespot_playback::{NUM_CHANNELS, SAMPLE_RATE};
use renderer_engine::protocol::TrackEdit;
use tokio::sync::mpsc;

use crate::resample::Resampler;
use crate::time_stretch::{AudioPipeline, PipelineConfig};

/// The audible pipeline: the rodio sink [`set_sink_volume`] applies transport
/// volume to, the ring a discontinuity clears, and the processing state a
/// natural boundary flushes through. All three name one [`RodioSink`] and are
/// claimed together by [`RodioSink::start`] — the moment that sink becomes the
/// one feeding the device — and released together by its `Drop`.
///
/// Ownership has to be that explicit because two sinks exist at once whenever a
/// player is replaced while another is still playing: a session reconnect that
/// preserves playback, and a normalisation rebuild, both construct the
/// replacement before the incumbent is stopped. Claiming at construction would
/// point every one of these at the silent newcomer, and a newcomer that is then
/// discarded (a stale rebuild) would leave them dangling at a pipeline nothing
/// can hear — for the rest of the process. The `Drop` release is therefore
/// conditional: a sink only ever clears an entry that is still its own.
static LIVE_SINK: Mutex<Weak<rodio::Sink>> = Mutex::new(Weak::new());
/// Retained even before the rodio sink is connected, because auth restores the
/// cached transport volume while the librespot sink is still logically Closed.
static SINK_VOLUME: AtomicU16 = AtomicU16::new(u16::MAX);
const LIVE_SINK_POISON_MSG: &str = "live rodio sink registry should not be poisoned";
static LIVE_RING: Mutex<Weak<SampleRing>> = Mutex::new(Weak::new());
const LIVE_RING_POISON_MSG: &str = "live sample ring should not be poisoned";
static LIVE_PROCESSING: Mutex<Weak<Mutex<AudioProcessing>>> = Mutex::new(Weak::new());
const LIVE_PROCESSING_POISON_MSG: &str = "live audio processing registry should not be poisoned";
static CUSTOMIZATION_REVISION: AtomicU64 = AtomicU64::new(0);
static CUSTOMIZATION_SPEED: AtomicU32 = AtomicU32::new(1.0f32.to_bits());
static CUSTOMIZATION: Mutex<Customization> = Mutex::new(Customization {
    config: None,
    discontinuous: false,
});
static AUDIO_SIGNAL_SENDER: Mutex<Option<mpsc::UnboundedSender<AudioSignal>>> = Mutex::new(None);

#[derive(Clone, Copy, Debug)]
pub enum AudioSignal {
    /// Delivered at the first frame using the new rate, after old-rate queued
    /// audio. Its callback timestamp anchors the transport, not decode-ahead.
    SpeedBoundary {
        speed: f32,
        revision: u64,
        at: Instant,
    },
    LoopBoundary {
        position_ms: u32,
        revision: u64,
    },
    /// The decoder handed this pipeline its first packet, i.e. the track the
    /// engine last configured is genuinely producing audio.
    ///
    /// This exists because librespot's events cannot answer the question. It
    /// reports a track `loaded` and then `Playing` before a single sample has
    /// been decoded, so a track whose audio key the service refused — which
    /// librespot deliberately loads anyway, undecrypted (`player.rs`: "Unable
    /// to load key, continuing without decryption") — announces itself exactly
    /// like a track that works. Everything downstream of that announcement,
    /// the playhead included, is then a fiction until the decoder chokes on
    /// ciphertext seconds later. Only the sink knows the difference, so
    /// the sink is what says so.
    Output {
        revision: u64,
    },
    /// The output stopped draining: a packet waited out the whole
    /// [`WRITE_DRAIN_TIMEOUT`] without the device consuming the ring, which
    /// on a machine whose dongle was just unplugged is the only evidence the
    /// playback side ever gets. librespot turns the write error that follows
    /// into a pause and never mentions the device, so the engine has to hear
    /// it from here.
    ///
    /// Revision-gated like [`AudioSignal::Output`], and for the same reason:
    /// this arrives on the player thread, two seconds after the write began,
    /// and a player the engine has already replaced must not act on its
    /// replacement.
    OutputStalled {
        revision: u64,
    },
}

/// Hands one signal to the engine. Called from the player thread and from the
/// audio callback, both of which own no engine state: the lock is only ever
/// held for the send itself, and a signal nobody is listening for is dropped.
fn signal(signal: AudioSignal) {
    let sender = AUDIO_SIGNAL_SENDER
        .lock()
        .expect("audio signal sender should not be poisoned");
    if let Some(sender) = sender.as_ref() {
        let _ = sender.send(signal);
    }
}

struct Customization {
    config: Option<PipelineConfig>,
    /// Sticky until the sink consumes the newest revision: if a seek/config
    /// reset is followed by a natural track setup before another decoder
    /// packet arrives, the old filter state still must not survive.
    discontinuous: bool,
}

pub fn install_signal_sender(sender: mpsc::UnboundedSender<AudioSignal>) {
    *AUDIO_SIGNAL_SENDER
        .lock()
        .expect("audio signal sender should not be poisoned") = Some(sender);
}

/// Installs a discontinuous customization at a known finite-loop pass.
/// The engine supplies the pass for loads, seeks, and internal loop jumps.
pub fn configure_customization_at_loop_pass(
    edit: Option<TrackEdit>,
    speed: f32,
    position_ms: u32,
    loop_pass: u32,
) -> u64 {
    set_customization(edit, speed, position_ms, true, loop_pass)
}

/// Installs the next gapless track without discarding audio already queued from
/// the natural boundary. The outgoing pipeline is flushed separately by
/// [`finish_natural_boundary`].
pub fn configure_customization_after_natural_boundary(
    edit: Option<TrackEdit>,
    speed: f32,
    position_ms: u32,
) -> u64 {
    set_customization(edit, speed, position_ms, false, 1)
}

/// Changes rate without clearing the ring, resetting edits or seeking the
/// decoder. The player thread applies it at its next packet boundary.
pub fn set_customization_speed(speed: f32) {
    let mut customization = CUSTOMIZATION
        .lock()
        .expect("audio customization should not be poisoned");
    let config = customization.config.get_or_insert(PipelineConfig {
        edit: None,
        speed,
        position_ms: 0,
        loop_pass: 1,
    });
    config.speed = speed;
    CUSTOMIZATION_SPEED.store(speed.to_bits(), Ordering::Release);
}

fn set_customization(
    edit: Option<TrackEdit>,
    speed: f32,
    position_ms: u32,
    discontinuous: bool,
    loop_pass: u32,
) -> u64 {
    let config = PipelineConfig {
        edit,
        speed,
        position_ms,
        loop_pass: loop_pass.max(1),
    };
    let active = config.active();
    let mut customization = CUSTOMIZATION
        .lock()
        .expect("audio customization should not be poisoned");
    customization.config = active.then_some(config);
    CUSTOMIZATION_SPEED.store(speed.to_bits(), Ordering::Release);
    customization.discontinuous |= discontinuous;
    let revision = CUSTOMIZATION_REVISION
        .fetch_add(1, Ordering::Release)
        .wrapping_add(1);
    drop(customization);

    if discontinuous {
        if let Some(ring) = LIVE_RING.lock().expect(LIVE_RING_POISON_MSG).upgrade() {
            ring.clear();
        }
    }

    revision
}

pub fn customization_revision() -> u64 {
    CUSTOMIZATION_REVISION.load(Ordering::Acquire)
}

/// Flushes the delayed WSOLA overlap region at decoder EOF without resetting
/// the output queue. This is intentionally separate from `stop`: a natural
/// boundary must drain audibly, while pause/seek/config changes are
/// discontinuities and discard stale queued audio.
pub fn finish_natural_boundary() -> Result<(), String> {
    let Some(processing) = LIVE_PROCESSING
        .lock()
        .expect(LIVE_PROCESSING_POISON_MSG)
        .upgrade()
    else {
        return Ok(());
    };
    let Some(ring) = LIVE_RING.lock().expect(LIVE_RING_POISON_MSG).upgrade() else {
        return Ok(());
    };

    let mut queued = Vec::new();
    let timing;
    {
        let mut processing = processing.lock().unwrap_or_else(PoisonError::into_inner);
        let mut tail = Vec::new();
        if let Some(pipeline) = &mut processing.pipeline {
            pipeline.finish(&mut tail);
        }
        match &mut processing.resampler {
            Some(resampler) => resampler.process(&tail, &mut queued),
            None => queued = tail,
        }
        timing = (processing.pipeline_revision, processing.speed, processing.output_rate);
    }
    if queued.is_empty() {
        return Ok(());
    }
    ring.push_timed_at_generation(ring.generation.load(Ordering::Acquire),
        queued, None, 0, WRITE_DRAIN_TIMEOUT, Some(timing))
        .map_err(|()| "rodio sink stalled while flushing the natural EOF tail".to_owned())
}

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
            StreamError(_) | PlayError(_) | Samples(_) => SinkError::OnWrite(error_string(&error)),
            NoDeviceAvailable | DeviceNotAvailable(_) => {
                SinkError::ConnectionRefused(error_string(&error))
            }
            DevicesError(_) => SinkError::InvalidParams(error_string(&error)),
        }
    }
}

impl fmt::Display for RodioError {
    /// What went wrong, for a reader who is not a librespot maintainer: this
    /// string ends up in the engine's state event, which is the only thing the
    /// owner sees when the machine has no output device. The library-style
    /// rendering in [`error_string`] stays where librespot's own error type
    /// carries it, but it leads with the sink's name and says "Not Available",
    /// which is not a sentence to hand a user.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        use RodioError::*;
        match self {
            NoDeviceAvailable => formatter.write_str("no output device is present"),
            DeviceNotAvailable(name) => {
                write!(formatter, "output device \"{name}\" is not available")
            }
            PlayError(error) => write!(formatter, "output device playback failed: {error}"),
            StreamError(error) => write!(formatter, "output device could not be opened: {error}"),
            DevicesError(error) => write!(formatter, "audio devices could not be listed: {error}"),
            Samples(text) => formatter.write_str(text),
        }
    }
}

fn error_string(error: &RodioError) -> String {
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

/// Decoded-audio write-ahead budget, in milliseconds. This is the engine's
/// *output* latency: every seek, volume change, and track switch waits for this
/// much already-decoded audio to play out first, and the reported position
/// leads the sound by the same amount. librespot's stock budget is ~0.5 s,
/// which made pause/seek/volume/next feel half a second behind. Network stalls
/// are covered separately by the audio-fetch read-ahead window (3 s), not by
/// this ring, so keeping it small does not weaken stall protection.
const WRITE_AHEAD_MS: usize = 150;

/// [`WRITE_AHEAD_MS`] as interleaved samples at `rate`. The ring holds
/// device-rate audio (the resampler runs before it), so the budget is derived
/// from the rate that is actually queued rather than from librespot's.
fn write_ahead_samples(rate: rodio::SampleRate) -> usize {
    rate as usize * NUM_CHANNELS as usize * WRITE_AHEAD_MS / 1000
}

/// The largest span rodio will read before rebuilding its rate converter:
/// `UniformSourceIterator::bootstrap` clamps the queue's reported span with
/// `.map(|x| x.min(32768))`. [`LiveSource`] reports it so rodio takes the
/// longest span it is willing to take. It is only an efficiency knob now —
/// with the source at the device rate, a rebuild neither drops nor
/// interpolates a sample — but there is no reason to make rodio do the work
/// more often than it must.
const RODIO_SPAN_SAMPLES: usize = 32_768;

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
/// The producer ([`RodioSink::write`]) pushes each decoded packet's processed
/// samples as one ring packet, omitting packets removed entirely by cuts, and
/// blocks with a deadline when the ring is full. The consumer ([`LiveSource`])
/// takes one packet at a time. Locking once per packet (~20 ms) rather than
/// once per sample keeps the callback cost negligible. Rodio's own sink takes
/// several mutexes on the audio thread every 5 ms
/// (`Sink::append`'s `periodic_access` closure), so this adds no new class of
/// contention.
struct SampleRing {
    /// Interleaved samples the ring will hold before [`SampleRing::push`]
    /// blocks. See [`write_ahead_samples`].
    capacity: usize,
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
    retaining_pause: AtomicBool,
}

#[derive(Default)]
struct RingPacket {
    samples: Vec<f32>,
    loop_to_ms: Option<u32>,
    /// The customization revision that produced the marker. A packet can
    /// outlive a seek/track change, so reading the current global revision at
    /// delivery time would incorrectly retag stale audio as current.
    loop_revision: u64,
    boundary_id: u64,
    timing: Option<PacketTiming>,
}

#[derive(Clone, Copy)]
struct PacketTiming {
    revision: u64,
    start_ms: f64,
    duration_ms: f64,
    speed: f32,
}

#[derive(Clone, Copy)]
struct AudibleClock {
    timing: PacketTiming,
    at: Instant,
}

struct RingState {
    packets: VecDeque<RingPacket>,
    /// Kept alongside `packets` so the producer's fullness check is a field
    /// read rather than a walk of the deque.
    queued_samples: usize,
    next_boundary_id: u64,
    consumed_boundary_id: u64,
    timeline_revision: u64,
    timeline_ms: f64,
    audible: Option<AudibleClock>,
    paused_elapsed_ms: Option<f64>,
}

impl SampleRing {
    fn new(capacity: usize) -> Arc<Self> {
        Arc::new(SampleRing {
            capacity,
            state: Mutex::new(RingState {
                packets: VecDeque::with_capacity(64),
                queued_samples: 0,
                next_boundary_id: 0,
                consumed_boundary_id: 0,
                timeline_revision: 0,
                timeline_ms: 0.0,
                audible: None,
                paused_elapsed_ms: None,
            }),
            space_freed: Condvar::new(),
            generation: AtomicU64::new(0),
            retaining_pause: AtomicBool::new(false),
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

    #[cfg(test)]
    fn push(&self, packet: Vec<f32>, timeout: Duration) -> Result<(), ()> {
        self.push_marked(packet, None, 0, timeout)
    }

    #[cfg(test)]
    fn push_marked(
        &self,
        samples: Vec<f32>,
        loop_to_ms: Option<u32>,
        loop_revision: u64,
        timeout: Duration,
    ) -> Result<(), ()> {
        let generation = self.generation.load(Ordering::Acquire);
        self.push_marked_at_generation(generation, samples, loop_to_ms, loop_revision, timeout)
    }

    #[cfg(test)]
    fn push_marked_at_generation(
        &self,
        generation: u64,
        samples: Vec<f32>,
        loop_to_ms: Option<u32>,
        loop_revision: u64,
        timeout: Duration,
    ) -> Result<(), ()> {
        self.push_timed_at_generation(
            generation, samples, loop_to_ms, loop_revision, timeout, None,
        )
    }

    fn push_timed_at_generation(
        &self,
        generation: u64,
        samples: Vec<f32>,
        loop_to_ms: Option<u32>,
        loop_revision: u64,
        timeout: Duration,
        timing: Option<(u64, f32, u32)>,
    ) -> Result<(), ()> {
        let deadline = Instant::now() + timeout;
        let mut state = self.lock();
        if self.generation.load(Ordering::Acquire) != generation {
            return Ok(());
        }
        while state.queued_samples >= self.capacity
            && !self.retaining_pause.load(Ordering::Acquire)
        {
            let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
                return Err(());
            };
            let (guard, _) = self
                .space_freed
                .wait_timeout(state, remaining)
                .unwrap_or_else(PoisonError::into_inner);
            state = guard;
            if self.generation.load(Ordering::Acquire) != generation {
                return Ok(());
            }
        }
        let boundary_id = if loop_to_ms.is_some() {
            state.next_boundary_id = state.next_boundary_id.wrapping_add(1);
            state.next_boundary_id
        } else {
            0
        };
        let timing = timing.filter(|_| !samples.is_empty()).map(|(revision, speed, rate)| {
            if state.timeline_revision != revision {
                state.timeline_revision = revision;
                state.timeline_ms = 0.0;
            }
            let duration_ms = samples.len() as f64 * 1_000.0
                / (f64::from(rate) * f64::from(NUM_CHANNELS));
            let timing = PacketTiming {
                revision,
                start_ms: state.timeline_ms,
                duration_ms,
                speed,
            };
            state.timeline_ms += duration_ms * f64::from(speed);
            timing
        });
        state.queued_samples += samples.len();
        state.packets.push_back(RingPacket {
            samples,
            loop_to_ms,
            loop_revision,
            boundary_id,
            timing,
        });

        // A loop marker is flow control, not a notification attached to an
        // otherwise ordinary packet. Hold the decoder here until the audio
        // callback reaches it; this bounds decode/network run-ahead even when
        // every packet beyond the loop is filtered to empty.
        while boundary_id != 0
            && state.consumed_boundary_id < boundary_id
            && self.generation.load(Ordering::Acquire) == generation
            && !self.retaining_pause.load(Ordering::Acquire)
        {
            let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
                return Err(());
            };
            let (guard, _) = self
                .space_freed
                .wait_timeout(state, remaining)
                .unwrap_or_else(PoisonError::into_inner);
            state = guard;
        }
        Ok(())
    }

    /// Takes the next packet, or `None` when the producer has fallen behind.
    fn pop(&self) -> Option<RingPacket> {
        let mut state = self.lock();
        let packet = state.packets.pop_front()?;
        state.queued_samples -= packet.samples.len();
        let speed_change = packet.timing.and_then(|timing| {
            let previous = state.audible;
            let at = Instant::now();
            state.audible = Some(AudibleClock { timing, at });
            previous.is_none_or(|previous| previous.timing.revision != timing.revision
                || previous.timing.speed != timing.speed)
                .then_some(AudioSignal::SpeedBoundary {
                    speed: timing.speed,
                    revision: timing.revision,
                    at,
                })
        });
        drop(state);
        if let Some(change) = speed_change {
            signal(change);
        }
        self.space_freed.notify_one();
        Some(packet)
    }


    fn acknowledge_boundary(&self, boundary_id: u64) {
        if boundary_id == 0 {
            return;
        }
        let mut state = self.lock();
        state.consumed_boundary_id = state.consumed_boundary_id.max(boundary_id);
        drop(state);
        self.space_freed.notify_all();
    }

    fn audible_elapsed_ms(&self, revision: u64) -> Option<f64> {
        self.audible_elapsed_at(revision, Instant::now())
    }

    fn audible_elapsed_at(&self, revision: u64, at: Instant) -> Option<f64> {
        let state = self.lock();
        let Some(clock) = state.audible.filter(|clock| clock.timing.revision == revision) else {
            // Before the first callback, queued audio is still at its initial
            // source offset; a decoder-head event is not audible progress.
            return state.packets.iter().find_map(|packet|
                packet.timing.filter(|timing| timing.revision == revision)
                    .map(|timing| timing.start_ms));
        };
        Some(clock.timing.start_ms
            + state.paused_elapsed_ms.unwrap_or_else(||
                (at.saturating_duration_since(clock.at).as_secs_f64() * 1_000.0)
                    .min(clock.timing.duration_ms)) * f64::from(clock.timing.speed))
    }

    fn pause_at(&self, at: Instant) {
        let mut state = self.lock();
        if state.paused_elapsed_ms.is_none() {
            state.paused_elapsed_ms = Some(state.audible.map_or(0.0, |clock|
                (at.saturating_duration_since(clock.at).as_secs_f64() * 1_000.0)
                    .min(clock.timing.duration_ms)));
        }
        self.retaining_pause.store(true, Ordering::Release);
        drop(state);
        // A producer may be inside write() when pause arrives. Admit that one
        // in-flight packet so librespot can poll the pause command instead of
        // waiting on a device we have deliberately stopped.
        self.space_freed.notify_all();
    }

    fn resume_at(&self, at: Instant) {
        let mut state = self.lock();
        if let Some(elapsed) = state.paused_elapsed_ms.take() {
            if let Some(clock) = &mut state.audible {
                clock.at = at - Duration::from_secs_f64(elapsed / 1_000.0);
            }
        }
        self.retaining_pause.store(false, Ordering::Release);
    }

    fn stop_processing(&self, processing: &Mutex<AudioProcessing>) {
        if !self.retaining_pause.load(Ordering::Acquire) {
            self.clear();
            processing.lock().unwrap_or_else(PoisonError::into_inner).reset();
        }
    }

    /// Drops every queued packet and tells [`LiveSource`] to drop the one it
    /// holds. Non-blocking: this is what makes `stop()` instant.
    fn clear(&self) {
        let mut state = self.lock();
        state.packets.clear();
        state.queued_samples = 0;
        state.consumed_boundary_id = state.next_boundary_id;
        state.audible = None;
        state.paused_elapsed_ms = None;
        self.retaining_pause.store(false, Ordering::Release);
        state.timeline_ms = 0.0;
        state.timeline_revision = 0;
        self.generation.fetch_add(1, Ordering::Release);
        drop(state);
        self.space_freed.notify_all();
    }
}

/// Source-time elapsed on the packet actually feeding the device, excluding
/// startup/underrun silence and decode-ahead. Cuts map through Engine's timeline.
pub fn audible_elapsed_ms(revision: u64) -> Option<f64> {
    LIVE_RING.lock().expect(LIVE_RING_POISON_MSG).upgrade()?
        .audible_elapsed_ms(revision)
}

pub fn audible_speed(revision: u64) -> Option<f32> {
    let ring = LIVE_RING.lock().expect(LIVE_RING_POISON_MSG).upgrade()?;
    let state = ring.lock();
    state.audible.filter(|clock| clock.timing.revision == revision)
        .map(|clock| clock.timing.speed)
        .or_else(|| state.packets.iter().find_map(|packet|
            packet.timing.filter(|timing| timing.revision == revision)
                .map(|timing| timing.speed)))
}

/// A user pause retains the audible packet, queued audio and processing
/// lookahead. Seeks/loads still clear all three through customization.
pub fn pause_output() {
    if let Some(ring) = LIVE_RING.lock().expect(LIVE_RING_POISON_MSG).upgrade() {
        ring.pause_at(Instant::now());
    }
    if let Some(sink) = LIVE_SINK.lock().expect(LIVE_SINK_POISON_MSG).upgrade() {
        sink.pause();
    }
}

/// The single `rodio::Source` the sink appends, for the lifetime of the sink.
///
/// It never ends and never yields `None`: an empty ring produces silence, so
/// rodio's queue never advances to another source. Decoder packet boundaries
/// therefore cannot reset the converter; only rodio's configured spans remain.
struct LiveSource {
    ring: Arc<SampleRing>,
    /// The generation this source is playing; a mismatch means `stop()` ran.
    generation: u64,
    packet: RingPacket,
    pos: usize,
    /// Samples of silence still owed. Silence is always emitted in whole
    /// frames so an underrun (or the start-up priming) can never shift the
    /// channel interleave and swap left with right.
    silence_remaining: usize,
    /// Rate of the audio in the ring, which [`RodioSink::write`] has already
    /// resampled to the device's rate.
    rate: rodio::SampleRate,
}

impl LiveSource {
    fn new(ring: Arc<SampleRing>, rate: rodio::SampleRate) -> Self {
        let generation = ring.generation.load(Ordering::Acquire);
        LiveSource {
            ring,
            generation,
            packet: RingPacket::default(),
            pos: 0,
            silence_remaining: RODIO_BOOTSTRAP_SPAN_SAMPLES,
            rate,
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
                self.packet = RingPacket::default();
                self.pos = 0;
            }
        }

        while self.pos == self.packet.samples.len() {
            if let Some(position_ms) = self.packet.loop_to_ms.take() {
                let revision = self.packet.loop_revision;
                signal(AudioSignal::LoopBoundary {
                    position_ms,
                    revision,
                });
                let boundary_id = std::mem::take(&mut self.packet.boundary_id);
                self.ring.acknowledge_boundary(boundary_id);
            }
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

        let sample = self.packet.samples[self.pos];
        self.pos += 1;
        Some(sample)
    }

    /// rodio's queue derives its span from this (see [`RODIO_SPAN_SAMPLES`]).
    /// The lower bound is valid because this iterator is endless; it is not an
    /// iterator-length promise, and nothing in the playback path treats it as
    /// one.
    fn size_hint(&self) -> (usize, Option<usize>) {
        (RODIO_SPAN_SAMPLES, None)
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

    /// The device's rate, not librespot's: [`RodioSink::write`] has already
    /// resampled. Reporting it is what makes rodio's `SampleRateConverter` and
    /// `ChannelCountConverter` take their `from == to` pass-through branches.
    fn sample_rate(&self) -> rodio::SampleRate {
        self.rate
    }

    fn total_duration(&self) -> Option<Duration> {
        None
    }
}

fn attach_live_source(sink: &rodio::Sink, ring: &Arc<SampleRing>, rate: rodio::SampleRate) {
    sink.append(LiveSource::new(ring.clone(), rate));
}

/// Points one live-pipeline entry (the ring, the processing state) at this
/// sink's half of it. [`LIVE_SINK`] is claimed by [`claim_live_sink`] instead,
/// which hands the newcomer the transport volume in the same critical section.
fn claim_live<T>(slot: &Mutex<Weak<T>>, value: &Arc<T>, poison: &str) {
    *slot.lock().expect(poison) = Arc::downgrade(value);
}

/// Registers the sink that is now feeding the device and hands it the
/// remembered transport volume before releasing the registry.
///
/// Claimed and read in one critical section, which is the same one
/// [`set_sink_volume`] stores and applies in. Reading the value first — as this
/// used to — let a volume change land in between: the newcomer is not in the
/// registry yet, so that change was not applied to it, and the gain it did
/// read is then written over the newer one the change had just applied to
/// whatever was live. The result is a sink audibly one step behind the volume
/// every other component already agrees on.
fn claim_live_sink(sink: &Arc<rodio::Sink>) {
    let mut live = LIVE_SINK.lock().expect(LIVE_SINK_POISON_MSG);
    *live = Arc::downgrade(sink);
    sink.set_volume(volume_to_gain(SINK_VOLUME.load(Ordering::Acquire)));
}

/// Clears one live-pipeline entry, but only while it still names this sink. A
/// sink outlived by its replacement must not deregister the pipeline that has
/// already taken over from it.
fn release_live<T>(slot: &Mutex<Weak<T>>, value: &Arc<T>, poison: &str) {
    let mut slot = slot.lock().expect(poison);
    if slot.ptr_eq(&Arc::downgrade(value)) {
        *slot = Weak::new();
    }
}

struct AudioProcessing {
    resampler: Option<Resampler>,
    pipeline_revision: u64,
    pipeline: Option<AudioPipeline>,
    speed: f32,
    output_rate: u32,
}

impl AudioProcessing {
    fn synchronize_pipeline(&mut self, revision: u64) {
        if self.pipeline_revision == revision {
            return;
        }
        let (config, discontinuous) = {
            let mut customization = CUSTOMIZATION
                .lock()
                .expect("audio customization should not be poisoned");
            let config = customization.config.clone();
            let discontinuous = std::mem::take(&mut customization.discontinuous);
            (config, discontinuous)
        };
        self.pipeline = config.as_ref().map(AudioPipeline::new);
        self.speed = config.as_ref().map_or(1.0, |config| config.speed);
        self.pipeline_revision = revision;
        if discontinuous {
            if let Some(resampler) = &mut self.resampler {
                resampler.reset();
            }
        }
    }

    fn synchronize_speed(&mut self, pending: &mut Vec<f32>) -> Option<f32> {
        let speed = f32::from_bits(CUSTOMIZATION_SPEED.load(Ordering::Acquire));
        if self.speed == speed {
            return None;
        }
        if let Some(pipeline) = &mut self.pipeline {
            pipeline.set_speed(speed, pending);
        } else if speed != 1.0 {
            let mut pipeline = AudioPipeline::new(&PipelineConfig {
                edit: None,
                speed: 1.0,
                position_ms: 0,
                loop_pass: 1,
            });
            pipeline.set_speed(speed, pending);
            self.pipeline = Some(pipeline);
        }
        if speed == 1.0 && self.pipeline.as_ref().is_some_and(AudioPipeline::is_passthrough) {
            self.pipeline = None;
        }
        self.speed = speed;
        Some(speed)
    }

    /// Shared by the real sink and device-free audio regression tests. Exact
    /// 1x/no edit returns the converter's allocation untouched.
    fn render_packet(
        &mut self,
        samples: Vec<f32>,
        pipeline_scratch: &mut Vec<f32>,
        resampler_scratch: &mut Vec<f32>,
    ) -> (Vec<f32>, Option<u32>) {
        if self.pipeline.is_none() && self.resampler.is_none() {
            return (samples, None);
        }
        pipeline_scratch.clear();
        resampler_scratch.clear();
        let (filtered, loop_to) = match &mut self.pipeline {
            Some(pipeline) => {
                let loop_to = pipeline.process(&samples, pipeline_scratch);
                (&pipeline_scratch[..], loop_to)
            }
            None => (&samples[..], None),
        };
        match &mut self.resampler {
            Some(resampler) => {
                resampler.process(filtered, resampler_scratch);
                (resampler_scratch.clone(), loop_to)
            }
            None => (pipeline_scratch.clone(), loop_to),
        }
    }

    fn reset(&mut self) {
        if let Some(resampler) = &mut self.resampler {
            resampler.reset();
        }
        if let Some(pipeline) = &mut self.pipeline {
            pipeline.reset_buffers();
        }
    }
}

pub struct RodioSink {
    rodio_sink: Option<Arc<rodio::Sink>>,
    ring: Arc<SampleRing>,
    output_rate: rodio::SampleRate,
    processing: Arc<Mutex<AudioProcessing>>,
    /// Per-packet staging reused across writes. The final clone deliberately
    /// gives the ring a tight allocation: its budget counts audible samples,
    /// not capacity, so moving decoder-sized scratch would inflate retained
    /// memory at high rates or with sparse cuts. Backpressure never holds
    /// processing; only the player thread touches these buffers.
    pipeline_scratch: Vec<f32>,
    resampler_scratch: Vec<f32>,
    /// The customization revision [`AudioSignal::Output`] was last sent for.
    /// One signal per revision is all the engine needs — it asks whether this
    /// load produced audio at all, not how much — and a revision changes on
    /// every load, seek and loop jump, so this re-arms wherever the engine
    /// starts caring about a new stretch of audio. Only `write` touches it.
    signalled_revision: Option<u64>,
    _stream: rodio::OutputStream,
}

/// Maps a u16 volume (librespot's 0..=65535 scale) to the audible gain used
/// by librespot's `Cubic(60)` volume control: `(0.1 + 0.9 * normalized)^3`.
/// Keep both endpoints explicit: cubic's 0.1 floor is useful for control
/// granularity, but transport zero must still be exact mute.
fn volume_to_gain(volume: u16) -> f32 {
    if volume == 0 {
        return 0.0;
    }
    if volume == u16::MAX {
        return 1.0;
    }

    let normalized = f64::from(volume) / f64::from(u16::MAX);
    let gain = (0.1 + 0.9 * normalized).powi(3);
    if gain.is_finite() {
        gain.clamp(0.0, 1.0) as f32
    } else {
        0.0
    }
}

/// Applies a transport volume change to the live rodio sink immediately.
/// The sink's mixer gain multiplies every queued sample, so the audible
/// change lands on the next audio callback (~10 ms) instead of after the
/// write-ahead buffer plays out.
///
/// The remembered value and the apply happen inside the registry lock, which
/// is also where [`claim_live_sink`] registers the next sink and reads the
/// value it applies. Stored outside it, a sink that was being claimed could
/// miss this change — it is not in the registry yet — and then hand the device
/// the older gain it had already read: one step behind `state.volume`, the
/// cache and the slider until the next change. Inside it, the two always move
/// together.
pub fn set_sink_volume(volume: u16) {
    let live = LIVE_SINK.lock().expect(LIVE_SINK_POISON_MSG);
    SINK_VOLUME.store(volume, Ordering::Release);
    if let Some(sink) = live.upgrade() {
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
            // 2. Native 44.1 kHz in some other format. Still no resampling;
            //    cpal converts the sample type.
            .or_else(|| at_rate(native, false))
            // 3. The device's own rate in the requested format. The sink
            //    resamples (see `crate::resample`), which costs a little CPU
            //    but nothing audible.
            .or_else(|| at_rate(device_rate, true))
            // 4. The device's own rate in any format.
            .or_else(|| at_rate(device_rate, false))
            // 5. Whatever the device defaults to, which may not be stereo.
            .unwrap_or_else(|| default_config.clone()),
    )
}

fn create_stream(
    host: &cpal::Host,
    format: AudioFormat,
) -> Result<rodio::OutputStream, RodioError> {
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
    let builder = rodio::OutputStreamBuilder::default()
        .with_device(cpal_device)
        .with_config(&config.config())
        .with_sample_format(sample_format);
    let mut stream = builder.open_stream_or_fallback()?;

    // Disable logging on stream drop.
    stream.log_on_drop(false);

    Ok(stream)
}

/// Opens the output device for one player: a sink, or the typed reason there
/// is none.
///
/// The engine holds one of these instead of calling [`open`] directly, for two
/// reasons that are the same reason seen from both ends. librespot's sink
/// builder cannot report a failure — it runs on the player thread and must
/// return a `Sink`, so the only way it can say "no device" is by killing that
/// thread — so the device step has to happen where its answer can travel
/// somewhere, and a machine with no output device is a state the engine has to
/// reach on demand, which real hardware cannot be asked to arrange.
pub type SinkOpener = Arc<dyn Fn(AudioFormat) -> Result<Box<dyn Sink>, RodioError> + Send + Sync>;

/// The opener the engine uses in production: whatever output device Windows
/// currently calls the default, in the format the player feeds.
pub fn default_sink_opener() -> SinkOpener {
    Arc::new(|format| Ok(Box::new(open(cpal::default_host(), format)?)))
}

/// Answers whether the machine currently has a default output device, and
/// nothing else.
///
/// One entry in this machine's device list, with no stream, mixer or player
/// thread behind it — the question a device probe asks before deciding that a
/// full construction is worth starting. Held next to [`SinkOpener`], and
/// injectable for the same reason: a machine with no output device is a state
/// the engine has to reach on demand, and no real hardware can be asked to
/// provide one.
pub type DevicePresence = Arc<dyn Fn() -> bool + Send + Sync>;

/// The presence check the engine uses in production: whether Windows has a
/// default output device at all right now.
///
/// Deliberately not the whole truth — a device that is present can still
/// refuse to open, and that failure keeps its typed reason and its message by
/// going through [`SinkOpener`]. All this answers is whether a construction
/// could find anything to open.
pub fn default_device_presence() -> DevicePresence {
    Arc::new(|| cpal::default_host().default_output_device().is_some())
}

/// The sink librespot is handed when the device step has already failed.
///
/// It exists because the failure cannot travel back through the builder: the
/// player thread calls it and needs a `Sink` to store. Nothing is ever played
/// into it — `auth::create_playback` drops the player the moment it reads the
/// device error beside it — so every packet is discarded.
pub struct SilentSink;

impl Sink for SilentSink {
    fn write(&mut self, _packet: AudioPacket, _converter: &mut Converter) -> SinkResult<()> {
        Ok(())
    }
}

/// Opens the default host's output device with an immediate-stop sink.
///
/// Infallible in the sense that it never panics, which it used to: a machine
/// whose dongle the driver has not recognised yet has no default output
/// device, and that was an `expect` on librespot's player thread. It is an
/// ordinary [`RodioError`] now, and the caller decides what to do about a
/// machine that cannot currently play anything.
pub fn open(host: cpal::Host, format: AudioFormat) -> Result<RodioSink, RodioError> {
    let stream = create_stream(&host, format)?;
    let output_rate = stream.config().sample_rate();
    let resampler = Resampler::new(SAMPLE_RATE, output_rate, NUM_CHANNELS as u16);
    let resampling = resampler.is_some();
    let ring = SampleRing::new(write_ahead_samples(output_rate));
    let processing = Arc::new(Mutex::new(AudioProcessing {
        resampler,
        pipeline_revision: CUSTOMIZATION_REVISION
            .load(Ordering::Acquire)
            .wrapping_sub(1),
        pipeline: None,
        speed: 1.0,
        output_rate,
    }));

    // The one fact that decides output fidelity, and the one that is otherwise
    // invisible: which rate the device actually opened at, and therefore
    // whether anything is being resampled at all.
    eprintln!(
        "audio output: {} Hz, {} channels, {:?}; decoder {} Hz stereo, {}",
        output_rate,
        stream.config().channel_count(),
        stream.config().sample_format(),
        SAMPLE_RATE,
        if resampling {
            "resampling"
        } else {
            "no resampling (device is at the decoder's rate)"
        }
    );

    // Opening the cpal stream validates the device while failure is still
    // recoverable by the sink factory, but do not connect rodio's keep-alive
    // SourcesQueueOutput yet. librespot creates this sink in logical Closed
    // state; connecting early makes the active output callback allocate
    // 512-sample Zero sources and rebuild converters forever while idle. The
    // cpal callback itself remains active because rodio does not expose its
    // stream handle, but an empty mixer has no queue/source work to perform.
    Ok(RodioSink {
        rodio_sink: None,
        ring,
        output_rate,
        processing,
        pipeline_scratch: Vec::new(),
        resampler_scratch: Vec::new(),
        signalled_revision: None,
        _stream: stream,
    })
}

impl Drop for RodioSink {
    fn drop(&mut self) {
        if let Some(sink) = &self.rodio_sink {
            release_live(&LIVE_SINK, sink, LIVE_SINK_POISON_MSG);
        }
        release_live(&LIVE_RING, &self.ring, LIVE_RING_POISON_MSG);
        release_live(
            &LIVE_PROCESSING,
            &self.processing,
            LIVE_PROCESSING_POISON_MSG,
        );
    }
}

impl Sink for RodioSink {
    fn start(&mut self) -> SinkResult<()> {
        if self.rodio_sink.is_none() {
            let sink = Arc::new(rodio::Sink::connect_new(self._stream.mixer()));
            sink.pause();
            // This sink is about to be the one feeding the device, so the whole
            // pipeline changes hands here rather than at construction — and the
            // claim hands it the remembered transport volume as it registers
            // it, before anything can change that volume again.
            claim_live_sink(&sink);
            attach_live_source(&sink, &self.ring, self.output_rate);
            claim_live(&LIVE_RING, &self.ring, LIVE_RING_POISON_MSG);
            claim_live(
                &LIVE_PROCESSING,
                &self.processing,
                LIVE_PROCESSING_POISON_MSG,
            );
            self.rodio_sink = Some(sink);
        }

        self.ring.resume_at(Instant::now());
        self.rodio_sink
            .as_ref()
            .expect("rodio sink was connected above")
            .play();
        Ok(())
    }

    /// Stops without draining. A requested user pause retains samples and
    /// processing state; seek/load/stop discontinuities discard them instead.
    /// Both pause the device immediately without waiting on queued audio.
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
        self.ring.stop_processing(&self.processing);
        if let Some(sink) = &self.rodio_sink {
            sink.pause();
        }
        Ok(())
    }
    fn write(&mut self, packet: AudioPacket, converter: &mut Converter) -> SinkResult<()> {
        let ring_generation = self.ring.generation.load(Ordering::Acquire);
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

        let revision = CUSTOMIZATION_REVISION.load(Ordering::Acquire);
        // Before any processing, because the question this answers is about
        // the decoder, not about what the edit pipeline does with its output:
        // a packet the cuts remove entirely still proves the track decodes.
        if self.signalled_revision != Some(revision) {
            self.signalled_revision = Some(revision);
            signal(AudioSignal::Output { revision });
        }

        let processing = Arc::clone(&self.processing);
        let mut processing = processing.lock().unwrap_or_else(PoisonError::into_inner);
        processing.synchronize_pipeline(revision);
        self.pipeline_scratch.clear();
        if let Some(speed) = processing.synchronize_speed(&mut self.pipeline_scratch) {
            // The retiring stretcher hands its lookahead to exact 1x, so these
            // samples follow the marker. Never wait with processing locked.
            self.resampler_scratch.clear();
            match &mut processing.resampler {
                Some(resampler) => {
                    resampler.process(&self.pipeline_scratch, &mut self.resampler_scratch);
                }
                None => std::mem::swap(&mut self.pipeline_scratch, &mut self.resampler_scratch),
            }
            if !self.resampler_scratch.is_empty() {
                let tail = self.resampler_scratch.clone();
                drop(processing);
                self.queue(ring_generation, revision, tail, None, speed)?;
                processing = self.processing.lock().unwrap_or_else(PoisonError::into_inner);
            }
        }

        let (queued, loop_to_ms) = processing.render_packet(
            samples_f32, &mut self.pipeline_scratch, &mut self.resampler_scratch,
        );
        let speed = processing.speed;
        drop(processing);

        // Do not pace an empty customized packet. The ring's queued audible
        // samples already provide normal backpressure; sleeping for removed
        // source frames makes LiveSource emit underrun silence for the cut's
        // original duration, turning a cut into a mute.
        if queued.is_empty() && loop_to_ms.is_none() {
            return Ok(());
        }

        self.queue(ring_generation, revision, queued, loop_to_ms, speed)
    }
}

impl RodioSink {
    /// Hands one finished packet to the ring; the shared backpressure tail of
    /// every write path. The wait is bounded so a dead audio thread cannot
    /// wedge the player thread forever: after WRITE_DRAIN_TIMEOUT the write
    /// fails with a normal sink error and librespot pauses playback (its
    /// `handle_packet` error path), which stops further writes and leaves the
    /// engine alive and recoverable.
    fn queue(
        &mut self,
        ring_generation: u64,
        revision: u64,
        samples: Vec<f32>,
        loop_to_ms: Option<u32>,
        speed: f32,
    ) -> SinkResult<()> {
        let queued = self.ring.push_timed_at_generation(
            ring_generation,
            samples,
            loop_to_ms,
            loop_to_ms.map_or(0, |_| revision),
            WRITE_DRAIN_TIMEOUT,
            Some((revision, speed, self.output_rate)),
        );
        if queued.is_err() {
            // The only way this wait expires is that nothing consumed the ring
            // for the whole timeout — `stop()` clears it, which returns `Ok` —
            // so the device that was taking audio is not taking it any more.
            // The decode side cannot act on that on its own, and librespot only
            // ever reports the resulting write error as a pause.
            signal(AudioSignal::OutputStalled { revision });
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
    use crate::auth::percent_to_volume;
    use rodio::Source as _;

    /// Every test here models the reference rig: a 48 kHz device, which is what
    /// the ring is sized for once the resampler has run.
    const OUT_RATE: rodio::SampleRate = 48_000;
    const TEST_RING_CAPACITY: usize =
        OUT_RATE as usize * NUM_CHANNELS as usize * WRITE_AHEAD_MS / 1000;

    fn test_ring() -> Arc<SampleRing> {
        SampleRing::new(TEST_RING_CAPACITY)
    }

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
    ///
    /// It must also come out the same in wall-clock terms whatever rate the
    /// device runs at, since the ring holds device-rate audio.
    #[test]
    fn write_ahead_stays_in_latency_budget_at_every_device_rate() {
        for rate in [SAMPLE_RATE, 48_000, 96_000, 192_000] {
            let ms = write_ahead_samples(rate) as f64 / f64::from(rate) / f64::from(NUM_CHANNELS)
                * 1000.0;
            assert!(
                (100.0..=200.0).contains(&ms),
                "at {rate} Hz the write-ahead is {ms:.0} ms, not near 150 ms"
            );
        }
    }

    /// A full ring must not block the player thread forever. This is the
    /// property that keeps a dead audio device recoverable.
    #[test]
    fn push_gives_up_when_the_ring_never_drains() {
        let ring = test_ring();
        while ring.queued_samples() < TEST_RING_CAPACITY {
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
        let ring = test_ring();
        while ring.queued_samples() < TEST_RING_CAPACITY {
            ring.push(packet(512), Duration::from_millis(50)).unwrap();
        }
        let popped = ring.pop().expect("a packet is queued");
        assert_eq!(popped.samples.len(), 512);
        ring.push(packet(512), Duration::from_millis(50))
            .expect("popping made room");
    }

    /// `stop()` must drop queued audio instantly, including the packet the
    /// audio thread is part-way through, so a pause is not trailed by up to a
    /// packet of stale sound.
    #[test]
    fn clear_drops_queued_audio_and_the_packet_in_flight() {
        let ring = test_ring();
        ring.push(packet(8), Duration::from_millis(50)).unwrap();
        ring.push(packet(8), Duration::from_millis(50)).unwrap();

        let mut source = LiveSource::new(ring.clone(), SAMPLE_RATE);
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

    #[test]
    fn a_packet_processed_before_stop_is_not_requeued_after_stop() {
        let ring = test_ring();
        let generation = ring.generation.load(Ordering::Acquire);
        ring.clear();

        ring.push_marked_at_generation(generation, packet(8), None, 0, Duration::from_millis(50))
            .unwrap();
        assert_eq!(ring.queued_samples(), 0);
    }

    /// Underrun silence must be a whole number of frames. A single stray
    /// sample would shift the interleave for the rest of the session and swap
    /// left with right.
    #[test]
    fn underrun_silence_is_frame_aligned() {
        let ring = test_ring();
        // Two frames of audio, then nothing.
        ring.push(vec![1.0, 2.0, 3.0, 4.0], Duration::from_millis(50))
            .unwrap();
        let mut source = LiveSource::new(ring.clone(), SAMPLE_RATE);
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
        ring.push(vec![5.0, 6.0], Duration::from_millis(50))
            .unwrap();
        assert_eq!(
            source.next(),
            Some(5.0),
            "audio resumes on the left channel"
        );
        assert_eq!(source.next(), Some(6.0));
    }

    /// The start-up priming exists so rodio's first span — bootstrapped from
    /// an empty queue that claims 1 channel at 48 kHz — consumes silence
    /// instead of the first moments of the first track.
    #[test]
    fn source_primes_with_rodios_bootstrap_span_of_silence() {
        let ring = test_ring();
        ring.push(vec![1.0; 4], Duration::from_millis(50)).unwrap();
        let mut source = LiveSource::new(ring, SAMPLE_RATE);
        for i in 0..RODIO_BOOTSTRAP_SPAN_SAMPLES {
            assert_eq!(
                source.next(),
                Some(0.0),
                "priming sample {i} must be silent"
            );
        }
        assert_eq!(source.next(), Some(1.0));
        assert!(
            RODIO_BOOTSTRAP_SPAN_SAMPLES.is_multiple_of(NUM_CHANNELS as usize),
            "priming silence must be a whole number of frames"
        );
    }

    /// The source must describe itself to rodio as an endless stream *at the
    /// device's rate*. That is the load-bearing claim of the whole arrangement:
    /// it is what puts rodio's converters on their `from == to` pass-through
    /// branches, so nothing downstream resamples or reinterleaves the audio.
    #[test]
    fn source_reports_the_device_rate_so_rodio_passes_samples_through() {
        let source = LiveSource::new(test_ring(), OUT_RATE);
        assert_eq!(
            source.sample_rate(),
            OUT_RATE,
            "reporting 44.1 kHz here would hand resampling back to rodio"
        );
        assert_eq!(source.channels(), NUM_CHANNELS as rodio::ChannelCount);
        assert_eq!(source.current_span_len(), None, "the format never changes");
        assert_eq!(source.total_duration(), None, "the source never ends");
        assert_eq!(source.size_hint().0, RODIO_SPAN_SAMPLES);
    }

    /// End-to-end rate accuracy and transparency through the real rodio graph.
    ///
    /// Builds the same `Sink` + `Mixer` pair first playback connects to the
    /// opened stream (minus the cpal device, which only supplies a clock) at
    /// the reference rig's 48 kHz, pushes 44.1 kHz audio through the sink's own
    /// resampler in realistically varying packet sizes, and checks what comes
    /// out the far end. This graph is a faithful model of the device path: run
    /// against the real dongle it reproduced the same figures to four decimals
    /// (256-sample packets measured +0.4882% both offline and on hardware).
    ///
    /// Two claims, and the second is the one that retired the drift:
    /// the frame count must be the exact rational conversion regardless of
    /// packet boundaries, and rodio must hand back the sample values it was
    /// given, bit for bit, because at a matching rate its converters are
    /// pass-throughs. See "Playback rate and fidelity" in the module docs.
    #[test]
    fn the_rodio_graph_is_transparent_at_the_device_rate() {
        const BLOCK_FRAMES: usize = 480;
        const SECONDS: usize = 15;

        let (sink, queue) = rodio::Sink::new();
        let (mixer_in, mut mixer_out) =
            rodio::mixer::mixer(NUM_CHANNELS as rodio::ChannelCount, OUT_RATE);
        mixer_in.add(queue);

        let ring = test_ring();
        sink.append(LiveSource::new(ring.clone(), OUT_RATE));

        let mut resampler =
            Resampler::new(SAMPLE_RATE, OUT_RATE, NUM_CHANNELS as u16).expect("rates differ");

        // Packet sizes cycle through what symphonia hands librespot for
        // Spotify's Ogg Vorbis (blocksizes 256/2048 => 128/576/1024 frames).
        let sizes = [256usize, 2048, 1152, 2048, 256, 1024, 2048, 512];
        let total_samples = SECONDS * SAMPLE_RATE as usize * NUM_CHANNELS as usize;

        // A ramp rather than a constant: a stuck or repeated sample is
        // invisible against DC, and the mixer's own `from == to` path would
        // hide a dropped frame too.
        let mut phase = 0usize;
        let mut source_sample = || {
            phase += 1;
            (phase % 1024) as f32 / 1024.0 - 0.5
        };

        let mut fed_samples = 0usize;
        let mut next_size = 0usize;
        let mut frame = 0usize;
        let mut queued_total = 0usize;
        let mut rendered_values: Vec<f32> = Vec::new();
        let mut expected_values: Vec<f32> = Vec::new();
        let (mut first, mut last) = (None, None);
        let mut silent_blocks = 0usize;
        let mut interior_silent_frames = 0usize;

        loop {
            while fed_samples < total_samples && ring.queued_samples() < TEST_RING_CAPACITY {
                let n = sizes[next_size % sizes.len()].min(total_samples - fed_samples);
                next_size += 1;
                let input: Vec<f32> = (0..n).map(|_| source_sample()).collect();
                let mut resampled = Vec::new();
                resampler.process(&input, &mut resampled);
                fed_samples += n;
                if resampled.is_empty() {
                    continue;
                }
                queued_total += resampled.len();
                expected_values.extend_from_slice(&resampled);
                ring.push(resampled, Duration::from_millis(50)).unwrap();
            }
            let mut block_had_audio = false;
            for _ in 0..BLOCK_FRAMES {
                let left = mixer_out.next().unwrap_or(0.0);
                let right = mixer_out.next().unwrap_or(0.0);
                if left != 0.0 || right != 0.0 {
                    first.get_or_insert(frame);
                    last = Some(frame);
                    block_had_audio = true;
                    rendered_values.push(left);
                    rendered_values.push(right);
                } else if first.is_some() {
                    interior_silent_frames += 1;
                }
                frame += 1;
            }
            // An empty ring is not an empty pipeline: the source still holds
            // the packet it is playing. The source never ends, so the only way
            // to know the tail has been rendered is to keep pulling until the
            // output goes and stays quiet. If the tail were genuinely dropped
            // rather than merely un-pulled, no amount of extra pulling would
            // recover it and the counts below would come up short.
            silent_blocks = if block_had_audio {
                0
            } else {
                silent_blocks + 1
            };
            if fed_samples == total_samples && silent_blocks >= 4 {
                break;
            }
        }
        // Silence counted after the last real frame is drain, not a gap.
        interior_silent_frames -= frame - 1 - last.expect("audio was rendered");

        let rendered = last.expect("audio was rendered") - first.expect("audio was rendered") + 1;
        let fed_frames = total_samples / NUM_CHANNELS as usize;
        let ideal = fed_frames * OUT_RATE as usize / SAMPLE_RATE as usize;

        assert_eq!(
            interior_silent_frames, 0,
            "rodio's queue splices in silence when it runs dry; the write-ahead \
             budget must keep it fed"
        );
        assert_eq!(
            rendered * NUM_CHANNELS as usize,
            queued_total,
            "rodio must render every sample the sink queued and no others"
        );
        assert_eq!(
            rendered_values, expected_values,
            "at a matching rate rodio must not alter a single sample value"
        );
        // The only permitted shortfall is the filter look-ahead still holding
        // the last few input frames — a fixed handful, not a growing fraction.
        let shortfall = ideal - rendered;
        assert!(
            shortfall <= 64,
            "15 s produced {rendered} frames against an ideal {ideal}: the \
             conversion must be exactly rational, not merely close"
        );
    }

    /// The endless source must not be attached while librespot still considers
    /// the sink Closed, and rodio's pause must stop polling it after the short
    /// control-update period. Otherwise idle playback takes the ring mutex at
    /// audio rate.
    #[test]
    fn live_source_is_idle_before_start_and_while_paused() {
        const TWENTY_MS_SAMPLES: usize = OUT_RATE as usize * NUM_CHANNELS as usize * 20 / 1000;

        let (sink, queue) = rodio::Sink::new();
        let (mixer_in, mut mixer_out) =
            rodio::mixer::mixer(NUM_CHANNELS as rodio::ChannelCount, OUT_RATE);
        mixer_in.add(queue);
        sink.pause();

        let ring = test_ring();
        ring.push(packet(1024), Duration::from_millis(50)).unwrap();

        for _ in 0..TWENTY_MS_SAMPLES {
            mixer_out.next();
        }
        assert_eq!(
            ring.queued_samples(),
            1024,
            "a Closed sink must not poll LiveSource before start"
        );

        attach_live_source(&sink, &ring, OUT_RATE);
        sink.play();
        for _ in 0..TWENTY_MS_SAMPLES * 4 {
            mixer_out.next();
        }
        assert_eq!(
            ring.queued_samples(),
            0,
            "start must attach and poll the source"
        );

        sink.pause();
        for _ in 0..TWENTY_MS_SAMPLES {
            mixer_out.next();
        }
        ring.push(packet(1024), Duration::from_millis(50)).unwrap();
        for _ in 0..TWENTY_MS_SAMPLES * 4 {
            mixer_out.next();
        }
        assert_eq!(
            ring.queued_samples(),
            1024,
            "a paused sink must emit silence without polling LiveSource"
        );

        sink.play();
        for _ in 0..TWENTY_MS_SAMPLES * 4 {
            mixer_out.next();
        }
        assert_eq!(
            ring.queued_samples(),
            0,
            "resume must poll the existing source"
        );
    }

    /// `configure_customization` owns process-wide statics, so the tests that
    /// drive it take turns rather than racing each other's flag and ring.
    static CUSTOMIZATION_TEST_LOCK: Mutex<()> = Mutex::new(());

    fn customization_guard() -> std::sync::MutexGuard<'static, ()> {
        CUSTOMIZATION_TEST_LOCK
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    fn live_ring() -> Option<Arc<SampleRing>> {
        LIVE_RING.lock().expect(LIVE_RING_POISON_MSG).upgrade()
    }

    /// Two sinks exist at once whenever a player is replaced while another is
    /// still playing (a preserved-playback reconnect, a normalisation rebuild).
    /// The registry has to follow the audible one through that overlap in both
    /// orders, and the retiring sink must never take its replacement's entry
    /// with it: an entry cleared by the wrong sink stays empty for the rest of
    /// the process, so volume changes, seek discontinuities and the natural
    /// boundary flush all silently stop reaching the audio that is playing.
    #[test]
    fn the_live_pipeline_follows_the_audible_sink_through_a_handover() {
        let _guard = customization_guard();
        let old = test_ring();
        claim_live(&LIVE_RING, &old, LIVE_RING_POISON_MSG);

        // Replacement becomes audible first, incumbent is torn down after.
        let new = test_ring();
        claim_live(&LIVE_RING, &new, LIVE_RING_POISON_MSG);
        release_live(&LIVE_RING, &old, LIVE_RING_POISON_MSG);
        assert!(
            live_ring().is_some_and(|ring| Arc::ptr_eq(&ring, &new)),
            "the retired sink deregistered its replacement"
        );

        // Incumbent is torn down first, replacement becomes audible after.
        let newest = test_ring();
        release_live(&LIVE_RING, &new, LIVE_RING_POISON_MSG);
        assert!(live_ring().is_none());
        claim_live(&LIVE_RING, &newest, LIVE_RING_POISON_MSG);
        assert!(live_ring().is_some_and(|ring| Arc::ptr_eq(&ring, &newest)));

        release_live(&LIVE_RING, &newest, LIVE_RING_POISON_MSG);
        assert!(live_ring().is_none());
    }

    /// A player built for a preference change that is superseded before it is
    /// installed is constructed, never started, and dropped. Because the claim
    /// happens at `start`, that sink never owned the pipeline, and its drop
    /// leaves the audible one exactly where it was.
    #[test]
    fn a_sink_that_never_became_audible_leaves_the_live_pipeline_alone() {
        let _guard = customization_guard();
        let playing = test_ring();
        claim_live(&LIVE_RING, &playing, LIVE_RING_POISON_MSG);

        let discarded = test_ring();
        release_live(&LIVE_RING, &discarded, LIVE_RING_POISON_MSG);

        assert!(
            live_ring().is_some_and(|ring| Arc::ptr_eq(&ring, &playing)),
            "a discarded replacement must not unregister the playing pipeline"
        );
        release_live(&LIVE_RING, &playing, LIVE_RING_POISON_MSG);
    }


    #[test]
    fn exact_one_customization_returns_the_converter_packet_without_stretcher_or_copy() {
        let _guard = customization_guard();
        let revision = configure_customization_at_loop_pass(Some(TrackEdit::default()), 1.0, 0, 1);
        let mut processing = AudioProcessing {
            resampler: None,
            pipeline_revision: revision.wrapping_sub(1),
            pipeline: None,
            speed: 1.0,
            output_rate: SAMPLE_RATE,
        };
        processing.synchronize_pipeline(revision);
        let samples = vec![0.0, -0.0, f32::MIN_POSITIVE, -0.125, 0.75, -1.0];
        let allocation = samples.as_ptr();
        let bits: Vec<_> = samples.iter().map(|sample| sample.to_bits()).collect();
        let mut pipeline_scratch = Vec::new();
        let mut resampler_scratch = Vec::new();
        let (output, loop_to) = processing.render_packet(
            samples, &mut pipeline_scratch, &mut resampler_scratch,
        );
        assert_eq!(output.as_ptr(), allocation);
        assert_eq!(output.iter().map(|sample| sample.to_bits()).collect::<Vec<_>>(), bits);
        assert_eq!(loop_to, None);
        assert!(processing.pipeline.is_none());
        assert_eq!(pipeline_scratch.capacity(), 0);
        assert_eq!(resampler_scratch.capacity(), 0);
    }

    #[test]
    fn queued_audio_keeps_old_rate_until_the_new_packet_is_audible() {
        let ring = test_ring();
        let revision = 19;
        let generation = ring.generation.load(Ordering::Acquire);
        let frames = OUT_RATE as usize / 10;
        for speed in [1.0, 4.0] {
            ring.push_timed_at_generation(generation, packet(frames * NUM_CHANNELS as usize),
                None, 0, Duration::from_millis(50), Some((revision, speed, OUT_RATE))).unwrap();
        }
        let first = ring.lock().packets.pop_front().unwrap();
        let second = ring.lock().packets.pop_front().unwrap();
        let at = Instant::now();
        ring.lock().audible = Some(AudibleClock { timing: first.timing.unwrap(), at });
        assert_eq!(ring.audible_elapsed_at(revision, at + Duration::from_millis(50)), Some(50.0));
        assert_eq!(ring.audible_elapsed_at(revision, at + Duration::from_millis(150)), Some(100.0));
        ring.lock().audible = Some(AudibleClock {
            timing: second.timing.unwrap(), at: at + Duration::from_millis(100),
        });
        assert_eq!(ring.audible_elapsed_at(revision, at + Duration::from_millis(100)), Some(100.0));
        assert_eq!(ring.audible_elapsed_at(revision, at + Duration::from_millis(150)), Some(300.0));
        assert_eq!(ring.audible_elapsed_at(revision, at + Duration::from_millis(250)), Some(500.0));
        assert_eq!(ring.audible_elapsed_at(revision + 1, at), None);
        ring.clear();
        assert_eq!(ring.audible_elapsed_at(revision, at), None);
    }

    #[test]
    fn pause_retains_inflight_audio_and_freezes_source_time_until_resume() {
        let ring = test_ring();
        let samples = vec![1.0, -1.0, 2.0, -2.0, 3.0, -3.0];
        ring.push(samples.clone(), Duration::from_millis(50)).unwrap();
        ring.push(vec![4.0, -4.0], Duration::from_millis(50)).unwrap();
        let mut source = LiveSource::new(ring.clone(), OUT_RATE);
        source.silence_remaining = 0;
        assert_eq!(source.next(), Some(1.0));
        assert_eq!(source.next(), Some(-1.0));
        let at = Instant::now();
        let revision = 31;
        ring.lock().audible = Some(AudibleClock {
            timing: PacketTiming { revision, start_ms: 13_000.0, duration_ms: 100.0, speed: 1.0 },
            at,
        });
        let processing = Mutex::new(AudioProcessing {
            resampler: None, pipeline_revision: revision,
            pipeline: Some(AudioPipeline::new(&PipelineConfig {
                edit: None, speed: 4.0, position_ms: 0, loop_pass: 1,
            })),
            speed: 4.0, output_rate: SAMPLE_RATE,
        });
        let mut pipeline_scratch = Vec::new();
        let mut resampler_scratch = Vec::new();
        let prefix = SAMPLE_RATE as usize / 50;
        let suffix = SAMPLE_RATE as usize * 2 / 25;
        let (first, _) = processing.lock().unwrap_or_else(PoisonError::into_inner).render_packet(
            vec![0.25; prefix * NUM_CHANNELS as usize],
            &mut pipeline_scratch, &mut resampler_scratch,
        );
        ring.pause_at(at + Duration::from_millis(50));
        ring.stop_processing(&processing);
        assert_eq!(ring.audible_elapsed_at(revision, at + Duration::from_secs(5)), Some(13_050.0));
        ring.resume_at(at + Duration::from_secs(5));
        assert_eq!(ring.audible_elapsed_at(revision, at + Duration::from_secs(5)), Some(13_050.0));
        assert_eq!(ring.audible_elapsed_at(revision,
            at + Duration::from_secs(5) + Duration::from_millis(25)), Some(13_075.0));
        assert_eq!((0..6).map(|_| source.next().unwrap()).collect::<Vec<_>>(),
            vec![2.0, -2.0, 3.0, -3.0, 4.0, -4.0]);
        let (last, _) = processing.lock().unwrap_or_else(PoisonError::into_inner).render_packet(
            vec![0.25; suffix * NUM_CHANNELS as usize],
            &mut pipeline_scratch, &mut resampler_scratch,
        );
        let mut tail = Vec::new();
        processing.lock().unwrap_or_else(PoisonError::into_inner)
            .pipeline.as_mut().unwrap().finish(&mut tail);
        assert_eq!((first.len() + last.len() + tail.len()) / NUM_CHANNELS as usize,
            ((prefix + suffix) as f64 / 4.0).round() as usize,
            "pause must not lose the stretcher's un-emitted source lookahead");
        ring.clear();
        assert_eq!(ring.audible_elapsed_at(revision, at), None);
    }

    #[test]
    fn pause_releases_a_capacity_wait_without_losing_the_inflight_packet() {
        let ring = SampleRing::new(2);
        ring.push(vec![1.0, -1.0], Duration::from_millis(50)).unwrap();
        let producer = ring.clone();
        let (done, result) = std::sync::mpsc::channel();
        let writer = std::thread::spawn(move || {
            done.send(producer.push(vec![2.0, -2.0], Duration::from_secs(1))).unwrap();
        });
        ring.pause_at(Instant::now());
        assert_eq!(result.recv_timeout(Duration::from_secs(1)).unwrap(), Ok(()));
        writer.join().unwrap();
        assert_eq!(ring.pop().unwrap().samples, vec![1.0, -1.0]);
        assert_eq!(ring.pop().unwrap().samples, vec![2.0, -2.0]);
    }

    /// Natural gapless setup must preserve the outgoing queue, while every
    /// discontinuous seek/config reset must discard it.
    #[test]
    fn natural_and_discontinuous_boundaries_have_distinct_queue_semantics() {
        let _guard = customization_guard();
        let ring = test_ring();
        *LIVE_RING.lock().expect(LIVE_RING_POISON_MSG) = Arc::downgrade(&ring);

        ring.push(packet(1024), Duration::from_millis(50)).unwrap();
        configure_customization_after_natural_boundary(None, 1.0, 0);
        assert_eq!(
            ring.queued_samples(),
            1024,
            "a natural track boundary must leave the audible tail queued"
        );

        configure_customization_at_loop_pass(None, 1.25, 0, 1);
        assert_eq!(
            ring.queued_samples(),
            0,
            "a configuration discontinuity must discard stale queued samples"
        );
    }

    #[test]
    fn natural_eof_flushes_wsola_tail_into_the_live_queue() {
        let _guard = customization_guard();
        let ring = test_ring();
        let speed = 0.5;
        let frames = SAMPLE_RATE as usize * 37 / 1_000;
        let mut pipeline = AudioPipeline::new(&PipelineConfig {
            edit: None,
            speed,
            position_ms: 0,
            loop_pass: 1,
        });
        let input = vec![0.25; frames * NUM_CHANNELS as usize];
        let mut already_emitted = Vec::new();
        pipeline.process(&input, &mut already_emitted);
        let processing = Arc::new(Mutex::new(AudioProcessing {
            resampler: None,
            pipeline_revision: CUSTOMIZATION_REVISION.load(Ordering::Acquire),
            pipeline: Some(pipeline),
            speed,
            output_rate: SAMPLE_RATE,
        }));
        *LIVE_RING.lock().expect(LIVE_RING_POISON_MSG) = Arc::downgrade(&ring);
        *LIVE_PROCESSING.lock().expect(LIVE_PROCESSING_POISON_MSG) = Arc::downgrade(&processing);

        finish_natural_boundary().expect("the output queue has room");
        let expected_total =
            (frames as f64 / speed as f64).round() as usize * NUM_CHANNELS as usize;
        assert_eq!(
            ring.queued_samples(),
            expected_total - already_emitted.len(),
            "every delayed WSOLA sample must be queued before natural advance"
        );
    }

    /// A loop end that lands on a packet boundary produces a marker with no
    /// samples. `LiveSource` must still deliver it — and must not index past
    /// the end of the empty packet on the way.
    #[test]
    fn a_marker_without_samples_is_delivered_and_does_not_panic() {
        let ring = test_ring();
        let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
        install_signal_sender(sender);
        let producer_ring = Arc::clone(&ring);
        let producer = std::thread::spawn(move || {
            producer_ring
                .push_marked(Vec::new(), Some(4_200), 0, Duration::from_secs(1))
                .unwrap();
        });
        while ring.lock().next_boundary_id == 0 {
            std::thread::yield_now();
        }

        let mut source = LiveSource::new(Arc::clone(&ring), OUT_RATE);
        for _ in 0..RODIO_BOOTSTRAP_SPAN_SAMPLES {
            source.next();
        }
        // Pulling once drains the empty marker, acknowledges its backpressure,
        // and returns underrun silence without indexing the empty vector.
        assert_eq!(source.next(), Some(0.0));
        producer.join().unwrap();
        match receiver.try_recv() {
            Ok(AudioSignal::LoopBoundary {
                position_ms,
                revision: 0,
            }) => assert_eq!(position_ms, 4_200),
            other => panic!("expected the loop boundary, got {other:?}"),
        }
    }

    #[test]
    fn loop_marker_blocks_the_decoder_until_it_is_audible() {
        let ring = test_ring();
        let producer_ring = Arc::clone(&ring);
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        let producer = std::thread::spawn(move || {
            producer_ring
                .push_marked(Vec::new(), Some(900), 0, Duration::from_secs(1))
                .unwrap();
            done_tx.send(()).unwrap();
        });
        assert!(
            done_rx.recv_timeout(Duration::from_millis(20)).is_err(),
            "the decoder ran past a queued loop boundary"
        );
        while ring.lock().next_boundary_id == 0 {
            std::thread::yield_now();
        }

        let mut source = LiveSource::new(ring, OUT_RATE);
        for _ in 0..=RODIO_BOOTSTRAP_SPAN_SAMPLES {
            source.next();
        }
        done_rx
            .recv_timeout(Duration::from_millis(100))
            .expect("audible boundary releases decoder backpressure");
        producer.join().unwrap();
    }

    /// The rodio gain curve must mirror librespot's `Cubic(60)` volume
    /// control so switching volume from per-packet attenuation to the sink
    /// does not change the audible volume curve.
    #[test]
    fn volume_to_gain_matches_the_softmixer_cubic_curve() {
        let expected = |volume: u16| {
            let normalized = f64::from(volume) / f64::from(u16::MAX);
            (0.1 + 0.9 * normalized).powi(3) as f32
        };
        assert_eq!(volume_to_gain(0), 0.0, "mute is exactly zero");
        assert_eq!(volume_to_gain(u16::MAX), 1.0, "max volume is unity");

        let table = [
            (1u16, 0.0010004121),
            (655u16, 0.0012948577),
            (6553u16, 0.0068582564),
            (16384u16, 0.034329213),
            (32768u16, 0.16638123),
            (49151u16, 0.46547818),
            (58982u16, 0.7535881),
        ];
        for (volume, expected_gain) in table {
            let actual = volume_to_gain(volume);
            assert!(
                (actual - expected_gain).abs() <= 1e-6,
                "mapping mismatch at raw volume {volume}: {actual} != {expected_gain}"
            );
            assert!(
                (actual - expected(volume)).abs() <= 1e-6,
                "mapping formula mismatch at raw volume {volume}"
            );
        }
    }

    #[test]
    fn volume_to_gain_is_monotonic_and_bounded() {
        let mut previous = volume_to_gain(0);
        for volume in 1u16..=u16::MAX {
            let gain = volume_to_gain(volume);
            assert!(
                gain.is_finite(),
                "gain at raw volume {volume} is not finite"
            );
            assert!(
                (0.0..=1.0).contains(&gain),
                "gain at raw volume {volume} escaped [0, 1]: {gain}"
            );
            assert!(
                gain >= previous,
                "gain decreased at raw volume {volume}: {gain} < {previous}"
            );
            previous = gain;
        }
    }

    /// The sink that joins the registry is handed the remembered transport
    /// volume as it is claimed, and a change afterwards lands on the same sink:
    /// the two are one critical section, which is what keeps the audible gain
    /// and the volume every other component agrees on from drifting apart.
    ///
    /// Both readings are taken under the registry lock on purpose: a change
    /// made by another test in this binary is serialized by that lock, so the
    /// pair can be compared without a torn read.
    #[test]
    fn the_claimed_sink_is_handed_the_volume_the_registry_remembers() {
        let (sink, _queue) = rodio::Sink::new();
        let sink = Arc::new(sink);
        assert_eq!(
            sink.volume(),
            1.0,
            "a fresh rodio sink starts at full gain, so a claim that does not \
             apply the remembered volume is audible as a jump to full volume"
        );

        set_sink_volume(percent_to_volume(17));
        claim_live_sink(&sink);
        {
            let live = LIVE_SINK.lock().expect(LIVE_SINK_POISON_MSG);
            assert_eq!(
                sink.volume(),
                volume_to_gain(SINK_VOLUME.load(Ordering::Acquire)),
                "the claim must hand the newcomer the remembered volume"
            );
            drop(live);
        }

        set_sink_volume(percent_to_volume(83));
        {
            let live = LIVE_SINK.lock().expect(LIVE_SINK_POISON_MSG);
            assert_eq!(
                sink.volume(),
                volume_to_gain(SINK_VOLUME.load(Ordering::Acquire)),
                "and a change after the claim reaches this sink"
            );
            drop(live);
        }

        // Leave the registry as this test found it.
        release_live(&LIVE_SINK, &sink, LIVE_SINK_POISON_MSG);
    }
}
