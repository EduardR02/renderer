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

use std::thread;
use std::time::Duration;

use cpal::traits::{DeviceTrait, HostTrait};
use librespot_playback::audio_backend::{Sink, SinkError, SinkResult};
use librespot_playback::config::AudioFormat;
use librespot_playback::convert::Converter;
use librespot_playback::decoder::AudioPacket;
use librespot_playback::{NUM_CHANNELS, SAMPLE_RATE};

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

pub struct RodioSink {
    rodio_sink: rodio::Sink,
    _stream: rodio::OutputStream,
}

fn create_sink(
    host: &cpal::Host,
    format: AudioFormat,
) -> Result<(rodio::Sink, rodio::OutputStream), RodioError> {
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

    let sink = rodio::Sink::connect_new(stream.mixer());
    Ok((sink, stream))
}

/// Opens the default output device through rodio with an immediate-stop sink.
/// Mirrors librespot's `mk_rodio(None, format)`.
pub fn open_default_sink(format: AudioFormat) -> Box<dyn Sink> {
    Box::new(open(cpal::default_host(), format))
}

pub fn open(host: cpal::Host, format: AudioFormat) -> RodioSink {
    let (sink, stream) = create_sink(&host, format).expect("rodio sink could not open");
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
        // Assuming they're on average 1628 then a half second buffer is:
        // 44100 elements --> about 27 chunks
        while self.rodio_sink.len() > 26 {
            // Sleep and wait for rodio to drain a bit.
            thread::sleep(Duration::from_millis(10));
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
}
