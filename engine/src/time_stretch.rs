use librespot_playback::{NUM_CHANNELS, SAMPLE_RATE};
use spotify_playback_engine::protocol::{TimeRange, TrackEdit};

const CHANNELS: usize = NUM_CHANNELS as usize;
const WINDOW_FRAMES: usize = 2_048;
const HOP_FRAMES: usize = WINDOW_FRAMES / 2;
const SEARCH_FRAMES: usize = 192;
const CORRELATION_STRIDE: usize = 8;
const COMPACT_AFTER_FRAMES: usize = 8_192;

#[derive(Clone, Debug)]
pub struct PipelineConfig {
    pub edit: Option<TrackEdit>,
    pub speed: f32,
    pub position_ms: u32,
}

impl PipelineConfig {
    pub fn active(&self) -> bool {
        self.edit.as_ref().is_some_and(|edit| !edit.is_empty()) || self.speed != 1.0
    }
}

pub struct AudioPipeline {
    edit: Option<SampleEdit>,
    stretcher: Option<Wsola>,
    filtered: Vec<f32>,
    loop_sent: bool,
}

impl AudioPipeline {
    pub fn new(config: &PipelineConfig) -> Self {
        Self {
            edit: config
                .edit
                .as_ref()
                .map(|edit| SampleEdit::new(edit, config.position_ms)),
            stretcher: (config.speed != 1.0).then(|| Wsola::new(config.speed)),
            filtered: Vec::new(),
            loop_sent: false,
        }
    }

    pub fn process(&mut self, input: &[f32], output: &mut Vec<f32>) -> Option<u32> {
        output.clear();
        let loop_to = if let Some(edit) = &mut self.edit {
            self.filtered.clear();
            let loop_to = edit.process(input, &mut self.filtered);
            if let Some(stretcher) = &mut self.stretcher {
                stretcher.process(&self.filtered, output);
                if loop_to.is_some() {
                    stretcher.finish(output);
                }
            } else {
                output.extend_from_slice(&self.filtered);
            }
            loop_to
        } else if let Some(stretcher) = &mut self.stretcher {
            stretcher.process(input, output);
            None
        } else {
            output.extend_from_slice(input);
            None
        };

        if self.loop_sent {
            None
        } else if loop_to.is_some() {
            self.loop_sent = true;
            loop_to
        } else {
            None
        }
    }

    pub fn reset_buffers(&mut self) {
        self.filtered.clear();
        if let Some(stretcher) = &mut self.stretcher {
            stretcher.reset();
        }
    }
}

struct SampleEdit {
    frame: u64,
    cuts: Vec<SampleRange>,
    cut_index: usize,
    loop_range: Option<SampleRange>,
}

#[derive(Clone, Copy)]
struct SampleRange {
    start: u64,
    end: u64,
}

impl SampleEdit {
    fn new(edit: &TrackEdit, position_ms: u32) -> Self {
        let cuts: Vec<_> = edit.cuts.iter().copied().map(sample_range).collect();
        let frame = ms_to_frame(position_ms);
        let cut_index = cuts.partition_point(|cut| cut.end <= frame);
        Self {
            frame,
            cuts,
            cut_index,
            loop_range: edit.loop_range.map(sample_range),
        }
    }

    fn process(&mut self, input: &[f32], output: &mut Vec<f32>) -> Option<u32> {
        debug_assert!(input.len().is_multiple_of(CHANNELS));
        output.reserve(input.len());
        for source in input.chunks_exact(CHANNELS) {
            if let Some(loop_range) = self.loop_range {
                if self.frame >= loop_range.end {
                    return Some(frame_to_ms(loop_range.start));
                }
            }
            while self
                .cuts
                .get(self.cut_index)
                .is_some_and(|cut| self.frame >= cut.end)
            {
                self.cut_index += 1;
            }
            let cut = self
                .cuts
                .get(self.cut_index)
                .is_some_and(|cut| self.frame >= cut.start && self.frame < cut.end);
            if !cut {
                output.extend_from_slice(source);
            }
            self.frame += 1;
        }
        if let Some(loop_range) = self.loop_range {
            if self.frame >= loop_range.end {
                return Some(frame_to_ms(loop_range.start));
            }
        }
        None
    }
}

fn sample_range(range: TimeRange) -> SampleRange {
    SampleRange {
        start: ms_to_frame(range.start_ms),
        end: ms_to_frame(range.end_ms),
    }
}

fn ms_to_frame(ms: u32) -> u64 {
    (u64::from(ms) * u64::from(SAMPLE_RATE) + 500) / 1_000
}

fn frame_to_ms(frame: u64) -> u32 {
    ((frame * 1_000 + u64::from(SAMPLE_RATE) / 2) / u64::from(SAMPLE_RATE)).min(u64::from(u32::MAX))
        as u32
}

/// Streaming waveform-similarity overlap-add stretcher. Both stereo channels
/// always use the same selected grain offset and crossfade coefficient; the
/// correlation score includes both channels, so phase cannot drift between
/// them. The exact 1.0 transport path never constructs this type.
pub struct Wsola {
    speed: f32,
    input: Vec<f32>,
    input_start: usize,
    next_frame: f64,
    previous_tail: Vec<f32>,
    started: bool,
}

impl Wsola {
    pub fn new(speed: f32) -> Self {
        assert!(speed.is_finite() && speed > 0.0);
        Self {
            speed,
            input: Vec::with_capacity((WINDOW_FRAMES + SEARCH_FRAMES * 2) * CHANNELS),
            input_start: 0,
            next_frame: 0.0,
            previous_tail: vec![0.0; HOP_FRAMES * CHANNELS],
            started: false,
        }
    }

    pub fn reset(&mut self) {
        self.input.clear();
        self.input_start = 0;
        self.next_frame = 0.0;
        self.previous_tail.fill(0.0);
        self.started = false;
    }

    pub fn process(&mut self, input: &[f32], output: &mut Vec<f32>) {
        debug_assert!(input.len().is_multiple_of(CHANNELS));
        self.input.extend_from_slice(input);
        let analysis_hop = HOP_FRAMES as f64 * f64::from(self.speed);

        loop {
            let available_frames = self.input.len() / CHANNELS;
            let predicted = self.next_frame.round().max(self.input_start as f64) as usize;
            if predicted + WINDOW_FRAMES + SEARCH_FRAMES > available_frames {
                break;
            }
            let selected = if self.started {
                self.best_match(predicted, available_frames)
            } else {
                predicted
            };
            self.emit_grain(selected, output);
            self.started = true;
            self.next_frame += analysis_hop;
            self.input_start = selected.saturating_sub(SEARCH_FRAMES + 1);
            self.compact();
        }
    }

    pub fn finish(&mut self, output: &mut Vec<f32>) {
        if self.started {
            output.extend_from_slice(&self.previous_tail);
        } else {
            // A loop shorter than one WSOLA window still needs to be audible.
            output.extend_from_slice(&self.input);
        }
        self.reset();
    }

    fn best_match(&self, predicted: usize, available_frames: usize) -> usize {
        let first = predicted
            .saturating_sub(SEARCH_FRAMES)
            .max(self.input_start);
        let last = (predicted + SEARCH_FRAMES).min(available_frames - WINDOW_FRAMES);
        let mut best = predicted.clamp(first, last);
        let mut best_score = f64::NEG_INFINITY;
        for candidate in first..=last {
            let score = self.correlation(candidate);
            if score > best_score {
                best_score = score;
                best = candidate;
            }
        }
        best
    }

    fn correlation(&self, candidate: usize) -> f64 {
        let mut dot = 0.0f64;
        let mut old_energy = 0.0f64;
        let mut new_energy = 0.0f64;
        for frame in (0..HOP_FRAMES).step_by(CORRELATION_STRIDE) {
            let old = frame * CHANNELS;
            let new = (candidate + frame) * CHANNELS;
            for channel in 0..CHANNELS {
                let a = f64::from(self.previous_tail[old + channel]);
                let b = f64::from(self.input[new + channel]);
                dot += a * b;
                old_energy += a * a;
                new_energy += b * b;
            }
        }
        dot / (old_energy * new_energy).sqrt().max(1.0e-12)
    }

    fn emit_grain(&mut self, start: usize, output: &mut Vec<f32>) {
        let first = start * CHANNELS;
        let middle = first + HOP_FRAMES * CHANNELS;
        let end = first + WINDOW_FRAMES * CHANNELS;
        output.reserve(HOP_FRAMES * CHANNELS);
        if !self.started {
            output.extend_from_slice(&self.input[first..middle]);
        } else {
            for frame in 0..HOP_FRAMES {
                let incoming = frame as f32 / HOP_FRAMES as f32;
                let outgoing = 1.0 - incoming;
                let offset = frame * CHANNELS;
                for channel in 0..CHANNELS {
                    output.push(
                        self.previous_tail[offset + channel] * outgoing
                            + self.input[first + offset + channel] * incoming,
                    );
                }
            }
        }
        self.previous_tail.copy_from_slice(&self.input[middle..end]);
    }

    fn compact(&mut self) {
        if self.input_start < COMPACT_AFTER_FRAMES {
            return;
        }
        let samples = self.input_start * CHANNELS;
        self.input.drain(..samples);
        self.next_frame -= self.input_start as f64;
        self.input_start = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stereo_tone(frames: usize, frequency: f32, right_scale: f32) -> Vec<f32> {
        let mut samples = Vec::with_capacity(frames * CHANNELS);
        for frame in 0..frames {
            let value =
                (std::f32::consts::TAU * frequency * frame as f32 / SAMPLE_RATE as f32).sin();
            samples.push(value);
            samples.push(value * right_scale);
        }
        samples
    }

    fn stretch_in_packets(input: &[f32], speed: f32, packet_frames: &[usize]) -> Vec<f32> {
        let mut stretcher = Wsola::new(speed);
        let mut output = Vec::new();
        let mut frame = 0;
        let total = input.len() / CHANNELS;
        let mut packet = 0;
        while frame < total {
            let count = packet_frames[packet % packet_frames.len()].min(total - frame);
            stretcher.process(
                &input[frame * CHANNELS..(frame + count) * CHANNELS],
                &mut output,
            );
            frame += count;
            packet += 1;
        }
        stretcher.finish(&mut output);
        output
    }

    fn positive_crossing_frequency(samples: &[f32]) -> f32 {
        let mut crossings = 0usize;
        let frames = samples.len() / CHANNELS;
        for frame in 1..frames {
            if samples[(frame - 1) * CHANNELS] <= 0.0 && samples[frame * CHANNELS] > 0.0 {
                crossings += 1;
            }
        }
        crossings as f32 * SAMPLE_RATE as f32 / frames as f32
    }

    #[test]
    fn duration_changes_by_the_inverse_speed_ratio() {
        let input = stereo_tone(SAMPLE_RATE as usize * 8, 440.0, 1.0);
        for speed in [0.75, 1.25, 1.75] {
            let output = stretch_in_packets(&input, speed, &[128, 576, 1_024, 287]);
            let expected = input.len() as f64 / f64::from(speed);
            let error = (output.len() as f64 - expected).abs() / expected;
            assert!(
                error < 0.02,
                "speed {speed} duration error was {:.2}%",
                error * 100.0
            );
        }
    }

    #[test]
    fn a_tone_keeps_its_pitch_when_speed_changes() {
        let input = stereo_tone(SAMPLE_RATE as usize * 8, 440.0, 1.0);
        for speed in [0.7, 1.4, 2.0] {
            let output = stretch_in_packets(&input, speed, &[256, 1_152, 2_048]);
            let measured = positive_crossing_frequency(&output);
            assert!(
                (measured - 440.0).abs() < 4.0,
                "speed {speed} measured {measured} Hz"
            );
        }
    }

    #[test]
    fn stereo_channels_remain_sample_aligned() {
        let input = stereo_tone(SAMPLE_RATE as usize * 4, 997.0, -0.5);
        let output = stretch_in_packets(&input, 1.37, &[128, 512, 997]);
        for frame in output.chunks_exact(CHANNELS) {
            assert!((frame[1] + frame[0] * 0.5).abs() < 1.0e-6);
        }
    }

    #[test]
    fn packet_boundaries_do_not_change_the_stream() {
        let input = stereo_tone(SAMPLE_RATE as usize * 5, 733.0, 0.25);
        let whole = stretch_in_packets(&input, 0.83, &[input.len() / CHANNELS]);
        let packeted = stretch_in_packets(&input, 0.83, &[128, 576, 1_024, 333]);
        assert_eq!(whole, packeted);
    }

    #[test]
    fn reset_discards_every_sample_from_the_old_position() {
        let old = stereo_tone(SAMPLE_RATE as usize, 220.0, 1.0);
        let fresh = stereo_tone(SAMPLE_RATE as usize * 2, 880.0, 1.0);
        let mut stretcher = Wsola::new(1.5);
        let mut discarded = Vec::new();
        stretcher.process(&old, &mut discarded);
        stretcher.reset();
        let mut output = Vec::new();
        stretcher.process(&fresh, &mut output);
        stretcher.finish(&mut output);
        assert!((positive_crossing_frequency(&output) - 880.0).abs() < 8.0);
    }

    #[test]
    fn cut_ranges_remove_samples_without_inserting_silence() {
        let edit = TrackEdit {
            cuts: vec![TimeRange {
                start_ms: 10,
                end_ms: 20,
            }],
            loop_range: None,
        };
        let mut pipeline = AudioPipeline::new(&PipelineConfig {
            edit: Some(edit),
            speed: 1.0,
            position_ms: 0,
        });
        let frames = ms_to_frame(30) as usize;
        let input: Vec<f32> = (0..frames)
            .flat_map(|frame| [frame as f32, -(frame as f32)])
            .collect();
        let mut output = Vec::new();
        assert_eq!(pipeline.process(&input, &mut output), None);
        assert_eq!(
            output.len() / CHANNELS,
            frames - (ms_to_frame(20) - ms_to_frame(10)) as usize
        );
        assert!(
            output
                .chunks_exact(CHANNELS)
                .all(|frame| frame[0] != 0.0 || frame[1] == 0.0)
        );
    }
}
