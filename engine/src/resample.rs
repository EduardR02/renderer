//! Rational-ratio polyphase resampling for the output path.
//!
//! librespot decodes at 44.1 kHz. Windows' shared-mode audio engine runs at
//! whatever rate the user configured — 48 kHz on most machines — and cpal only
//! ever opens shared-mode streams, so the device rate is not negotiable and
//! every sample is resampled on the way out.
//!
//! rodio does that conversion with `SampleRateConverter`, which its own
//! documentation describes as "simple linear interpolation for up-sampling".
//! Straight-line interpolation between adjacent samples is a poor
//! reconstruction of a band-limited signal: the error grows with the square of
//! frequency, so it is inaudible in the bass and severe in the treble. Measured
//! against the analytically exact answer at 44.1 -> 48 kHz it lands at -94.6 dB
//! at 100 Hz but only -15.0 dB at 10 kHz and -8.5 dB at 15 kHz. That is the
//! loudest thing in the signal path by a wide margin — far above the Ogg Vorbis
//! quantisation noise the player exists to deliver faithfully.
//!
//! This module replaces it. Output frame `p` is reconstructed at input time
//! `p * input_rate / output_rate` by a windowed-sinc kernel, evaluated through
//! a precomputed polyphase bank so the per-sample cost is [`TAPS`] multiply-adds
//! and no transcendental calls. The same measurement puts it at -133 dB at
//! 100 Hz, -116 dB at 10 kHz and -110 dB at 19 kHz — below 16-bit resolution
//! across the whole audible band, and far below what the codec itself leaves.
//!
//! Two properties beyond fidelity fall out of doing it here:
//!
//! - **No drift.** Position is tracked as an exact rational (`phase` over
//!   `phase_count`, advanced by `step`), so the output frame count is
//!   determined to the frame for any input length. The previous arrangement had
//!   to stop rodio's converter from being rebuilt mid-stream, because each
//!   rebuild leaked a fraction of a frame; that whole class of bug is gone
//!   rather than managed.
//! - **rodio stops resampling.** The sink reports the device rate, which sends
//!   both of rodio's converters down their `from == to` pass-through branches,
//!   so nothing downstream touches the samples.

use std::f64::consts::PI;

/// Input frames spanned by each output sample. With [`KAISER_BETA`] this sets
/// how steeply the filter can turn over at the cutoff, and 64 is comfortably
/// enough: the measured reconstruction error stays below -109 dB across the
/// whole audible band, which is two orders of magnitude under 16-bit
/// resolution. The cost is 64 multiply-adds per output sample per channel —
/// 6.1 M/s at 48 kHz stereo, and on the player thread rather than the audio
/// callback, so it cannot contribute to an underrun.
const TAPS: usize = 64;

/// Taps that sit before the output instant. The kernel is symmetric about the
/// instant it reconstructs, so this centres the window on the interval the
/// output falls in and the filter adds no delay — only `TAPS - CENTER` frames
/// of look-ahead (~0.75 ms) before the first output can be emitted.
const CENTER: usize = TAPS / 2 - 1;

/// Kaiser shape parameter: how deep the stopband goes, and therefore — for a
/// fixed [`TAPS`] — how wide the transition around the cutoff has to be.
///
/// Swept against the measurement in
/// `reconstruction_error_stays_below_the_codec_floor`. Lower values keep the
/// passband flat a little closer to Nyquist but raise the floor everywhere:
/// 9.5 measures -103 dB at 10 kHz where 11.0 measures -116 dB. The only thing
/// 11.0 gives up is 0.009 dB of droop at 20 kHz, in a band Spotify's Ogg Vorbis
/// has already discarded.
const KAISER_BETA: f64 = 11.0;

/// Cap on precomputed phases, bounding the coefficient table at 1 MB.
///
/// The phase count is `output_rate / gcd(input_rate, output_rate)`: 160 for
/// 44.1 -> 48 kHz, 320 for 96 kHz, 640 for 192 kHz. Every rate a sound device
/// actually exposes is a multiple of 100, so the cap cannot bind in practice.
/// If some device ever did report a rate near-coprime with 44100, only the
/// *coefficient lookup* quantises — position stays exact rational arithmetic,
/// so the rate stays exact and only reconstruction loses a little accuracy.
const MAX_PHASES: usize = 4096;

/// Streaming resampler for one fixed rate pair.
pub struct Resampler {
    /// Polyphase bank, `table_phases * TAPS` coefficients, oldest tap first.
    phases: Vec<f32>,
    /// Phases actually stored: `min(phase_count, MAX_PHASES)`.
    table_phases: usize,
    /// Input frames advanced per output frame, as `step` over `phase_count`.
    step: usize,
    phase_count: usize,
    channels: usize,
    /// Interleaved input: `CENTER` frames of left context, then everything not
    /// yet consumed.
    history: Vec<f32>,
    /// Frame index into `history` that the next output sample sits on.
    cursor: usize,
    /// Fractional part of that position, over `phase_count`.
    phase: usize,
}

impl Resampler {
    /// Builds a resampler for `input_rate` to `output_rate`, or `None` when the
    /// rates match and the samples should pass through untouched.
    pub fn new(input_rate: u32, output_rate: u32, channels: u16) -> Option<Resampler> {
        if input_rate == output_rate || input_rate == 0 || output_rate == 0 || channels == 0 {
            return None;
        }

        let divisor = gcd(input_rate as usize, output_rate as usize);
        let step = input_rate as usize / divisor;
        let phase_count = output_rate as usize / divisor;
        let table_phases = phase_count.min(MAX_PHASES);

        // The cutoff belongs at the lower of the two Nyquist limits, in cycles
        // per input sample. Upsampling must reject the input's images, which
        // begin just above its own Nyquist; downsampling must additionally keep
        // the input's own content from folding into the shorter output band.
        // The transition band straddles it: content below stays flat, the
        // images above are attenuated by the stopband depth [`KAISER_BETA`]
        // buys.
        let cutoff = 0.5 * f64::from(input_rate.min(output_rate)) / f64::from(input_rate);

        let mut resampler = Resampler {
            phases: build_bank(table_phases, cutoff),
            table_phases,
            step,
            phase_count,
            channels: channels as usize,
            history: Vec::new(),
            cursor: 0,
            phase: 0,
        };
        resampler.reset();
        Some(resampler)
    }

    /// Discards filter state, so audio from before a seek or a track change
    /// cannot bleed into what follows, and restarts the phase at zero.
    pub fn reset(&mut self) {
        self.history.clear();
        // A track begins from silence, so zero-filling the left context is not
        // an approximation — it is what precedes the first sample.
        self.history.resize(CENTER * self.channels, 0.0);
        self.cursor = CENTER;
        self.phase = 0;
    }

    /// Upper bound on the output `input_samples` can produce, for sizing the
    /// destination. Deliberately generous rather than tight: the exact count
    /// depends on how much history is already buffered.
    fn output_capacity(&self, input_samples: usize) -> usize {
        input_samples * self.phase_count / self.step + self.channels
    }

    /// Consumes `input` (interleaved) and appends every output frame it
    /// completes to `out`. Frames the filter still needs look-ahead for stay
    /// buffered until the next call.
    pub fn process(&mut self, input: &[f32], out: &mut Vec<f32>) {
        out.reserve(self.output_capacity(input.len()));
        self.history.extend_from_slice(input);

        let frames = self.history.len() / self.channels;
        while self.cursor + TAPS - CENTER <= frames {
            let base = (self.cursor - CENTER) * self.channels;
            let window = &self.history[base..base + TAPS * self.channels];
            let index = if self.table_phases == self.phase_count {
                self.phase
            } else {
                self.phase * self.table_phases / self.phase_count
            };
            let coefficients = &self.phases[index * TAPS..(index + 1) * TAPS];

            for channel in 0..self.channels {
                let mut sum = 0.0f32;
                for (tap, coefficient) in coefficients.iter().enumerate() {
                    sum += window[tap * self.channels + channel] * coefficient;
                }
                out.push(sum);
            }

            let advanced = self.phase + self.step;
            self.cursor += advanced / self.phase_count;
            self.phase = advanced % self.phase_count;
        }

        // Everything before the current left context is spent.
        let spent = self.cursor - CENTER;
        self.history.drain(..spent * self.channels);
        self.cursor = CENTER;
    }
}

fn gcd(mut left: usize, mut right: usize) -> usize {
    while right != 0 {
        (left, right) = (right, left % right);
    }
    left
}

/// Windowed-sinc coefficients for every phase, each normalised to unity gain.
///
/// `cutoff` is in cycles per *input* sample. Normalising per phase rather than
/// globally matters: without it the bank's DC gain varies slightly from phase
/// to phase, which modulates the signal at the phase-cycle rate — an audible
/// artefact exactly where a resampler is supposed to be silent.
fn build_bank(table_phases: usize, cutoff: f64) -> Vec<f32> {
    let half_width = (TAPS / 2) as f64;
    let beta_scale = bessel_i0(KAISER_BETA);
    let mut bank = Vec::with_capacity(table_phases * TAPS);

    for phase in 0..table_phases {
        let fraction = phase as f64 / table_phases as f64;
        let start = bank.len();
        let mut sum = 0.0f64;

        for tap in 0..TAPS {
            // Distance, in input samples, from this tap to the instant being
            // reconstructed. Positive means the tap precedes it.
            let offset = CENTER as f64 - tap as f64 + fraction;
            let value = kernel(offset, half_width, cutoff, beta_scale);
            sum += value;
            bank.push(value as f32);
        }

        if sum.abs() > f64::EPSILON {
            for coefficient in &mut bank[start..] {
                *coefficient = (f64::from(*coefficient) / sum) as f32;
            }
        }
    }

    bank
}

fn kernel(offset: f64, half_width: f64, cutoff: f64, beta_scale: f64) -> f64 {
    let ratio = offset / half_width;
    if ratio.abs() >= 1.0 {
        return 0.0;
    }
    let window = bessel_i0(KAISER_BETA * (1.0 - ratio * ratio).sqrt()) / beta_scale;
    let x = 2.0 * cutoff * offset;
    let sinc = if x.abs() < 1e-12 {
        1.0
    } else {
        (PI * x).sin() / (PI * x)
    };
    2.0 * cutoff * sinc * window
}

/// Modified Bessel function of the first kind, order zero, by its power series.
/// The Kaiser window only ever needs `0 ..= KAISER_BETA`, where this converges
/// in a handful of terms.
fn bessel_i0(x: f64) -> f64 {
    let quarter_square = x * x / 4.0;
    let mut term = 1.0f64;
    let mut sum = 1.0f64;
    for k in 1..64 {
        term *= quarter_square / (k as f64 * k as f64);
        sum += term;
        if term < sum * 1e-17 {
            break;
        }
    }
    sum
}

#[cfg(test)]
mod tests {
    use super::*;

    const IN_RATE: u32 = 44_100;
    const OUT_RATE: u32 = 48_000;
    const CHANNELS: u16 = 2;

    fn resample_all(resampler: &mut Resampler, input: &[f32], chunk: usize) -> Vec<f32> {
        let mut out = Vec::new();
        for piece in input.chunks(chunk) {
            resampler.process(piece, &mut out);
        }
        out
    }

    /// A tone is the one signal whose resampled form is known exactly, so the
    /// error against it is the reconstruction error with no alignment or
    /// windowing ambiguity anywhere in the measurement.
    fn error_db(resampled: &[f32], freq: f64) -> f64 {
        let frames = resampled.len() / CHANNELS as usize;
        let skip = OUT_RATE as usize / 10;
        let (mut signal, mut noise) = (0.0f64, 0.0f64);
        for p in skip..frames - skip {
            let want = (std::f64::consts::TAU * freq * p as f64 / f64::from(OUT_RATE)).sin();
            let got = f64::from(resampled[p * CHANNELS as usize]);
            signal += want * want;
            noise += (got - want) * (got - want);
        }
        10.0 * (noise / signal).log10()
    }

    fn tone(freq: f64, seconds: usize) -> Vec<f32> {
        let frames = IN_RATE as usize * seconds;
        let mut out = Vec::with_capacity(frames * CHANNELS as usize);
        for n in 0..frames {
            let s = (std::f64::consts::TAU * freq * n as f64 / f64::from(IN_RATE)).sin() as f32;
            out.push(s);
            out.push(s);
        }
        out
    }

    /// The whole point of the module: reconstruction must sit far below the
    /// codec noise it carries, at every frequency — not just in the bass, which
    /// is the one band linear interpolation also gets right.
    /// For reference, the same measurement of what rodio does instead:
    /// -94.6 dB at 100 Hz, -54.6 dB at 1 kHz, -26.8 dB at 5 kHz, -15.0 dB at
    /// 10 kHz, -8.5 dB at 15 kHz.
    #[test]
    fn reconstruction_error_stays_below_the_codec_floor() {
        for freq in [100.0, 1_000.0, 5_000.0, 10_000.0, 15_000.0, 19_000.0] {
            let mut resampler =
                Resampler::new(IN_RATE, OUT_RATE, CHANNELS).expect("rates differ");
            let out = resample_all(&mut resampler, &tone(freq, 2), 1024);
            let db = error_db(&out, freq);
            assert!(
                db < -105.0,
                "reconstruction error at {freq} Hz was {db:.1} dB, expected below -105 dB"
            );
        }
    }

    /// 20 kHz sits inside the transition band, where the error above is droop
    /// rather than noise. It has to stay small enough to be meaningless: the
    /// whole point of resampling correctly is not to trade one audible defect
    /// for another.
    #[test]
    fn the_top_of_the_band_is_not_rolled_off() {
        let mut resampler = Resampler::new(IN_RATE, OUT_RATE, CHANNELS).expect("rates differ");
        let out = resample_all(&mut resampler, &tone(20_000.0, 2), 1024);
        let error = 10f64.powf(error_db(&out, 20_000.0) / 20.0);
        let droop_db = -20.0 * (1.0 - error).log10();
        assert!(
            droop_db < 0.05,
            "20 kHz is {droop_db:.3} dB down; the passband must stay flat"
        );
    }

    /// Drift is what sent us here in the first place. The frame count must be
    /// exactly the ratio, and must not depend on how the input was chunked —
    /// decoder packet sizes vary with the material.
    #[test]
    fn output_frame_count_is_exact_and_chunk_independent() {
        let input = tone(1_000.0, 10);
        let frames_in = input.len() / CHANNELS as usize;
        let ideal = frames_in as u64 * u64::from(OUT_RATE) / u64::from(IN_RATE);

        for chunk in [256usize, 1152, 4096, 32_768] {
            let mut resampler =
                Resampler::new(IN_RATE, OUT_RATE, CHANNELS).expect("rates differ");
            let out = resample_all(&mut resampler, &input, chunk * CHANNELS as usize);
            let frames_out = (out.len() / CHANNELS as usize) as u64;
            // The only shortfall permitted is the look-ahead the filter has not
            // been fed yet: a fixed handful of frames, not a growing fraction.
            let shortfall = ideal.saturating_sub(frames_out);
            assert!(
                shortfall <= TAPS as u64,
                "chunk {chunk}: produced {frames_out} frames, ideal {ideal}"
            );
            assert!(frames_out <= ideal, "chunk {chunk}: produced more than ideal");
        }
    }

    /// Silence in must be silence out. A bank whose phases did not each sum to
    /// unity would instead ring at the phase-cycle rate.
    #[test]
    fn every_phase_has_unity_gain() {
        let resampler = Resampler::new(IN_RATE, OUT_RATE, CHANNELS).expect("rates differ");
        for phase in 0..resampler.table_phases {
            let sum: f32 = resampler.phases[phase * TAPS..(phase + 1) * TAPS].iter().sum();
            assert!(
                (sum - 1.0).abs() < 1e-5,
                "phase {phase} sums to {sum}, which would modulate the signal"
            );
        }
    }

    /// Matching rates must not build a filter at all — running audio through a
    /// resampler that has nothing to do would only add its passband ripple.
    #[test]
    fn matching_rates_need_no_resampler() {
        assert!(Resampler::new(IN_RATE, IN_RATE, CHANNELS).is_none());
    }

    /// Channels must stay independent: a bug in the interleave arithmetic would
    /// blend left into right, which is inaudible on the mono test tones above.
    #[test]
    fn channels_do_not_bleed_into_each_other() {
        let frames = IN_RATE as usize;
        let mut input = Vec::with_capacity(frames * 2);
        for n in 0..frames {
            input.push((std::f64::consts::TAU * 1_000.0 * n as f64 / 44_100.0).sin() as f32);
            input.push(0.0);
        }
        let mut resampler = Resampler::new(IN_RATE, OUT_RATE, CHANNELS).expect("rates differ");
        let out = resample_all(&mut resampler, &input, 1024);

        let right_peak = out
            .iter()
            .skip(1)
            .step_by(2)
            .fold(0.0f32, |peak, s| peak.max(s.abs()));
        assert!(right_peak < 1e-4, "silent channel picked up {right_peak}");
    }

    /// Rates the reference rig will never see must still resample rather than
    /// panic or silently degrade to nothing.
    #[test]
    fn handles_the_other_common_device_rates() {
        for rate in [48_000u32, 88_200, 96_000, 192_000, 32_000, 22_050] {
            let mut resampler =
                Resampler::new(IN_RATE, rate, CHANNELS).expect("rates differ from 44100");
            let mut out = Vec::new();
            resampler.process(&tone(1_000.0, 1), &mut out);
            let frames = (out.len() / CHANNELS as usize) as f64;
            let expected = f64::from(rate);
            // The filter still holds its look-ahead, which is a fixed number of
            // *input* frames and so scales into more output frames the higher
            // the device rate.
            let lookahead = TAPS as f64 * expected / f64::from(IN_RATE);
            assert!(
                frames <= expected && expected - frames <= lookahead,
                "{rate} Hz produced {frames} frames in one second, expected \
                 {expected} less at most {lookahead:.0} frames of look-ahead"
            );
        }
    }
}
