use librespot_playback::{NUM_CHANNELS, SAMPLE_RATE};
use renderer_engine::protocol::{TimeRange, TrackEdit};

const CHANNELS: usize = NUM_CHANNELS as usize;
/// OLA analysis window (Chromium AudioRendererAlgorithm: 20 ms).
const OLA_WINDOW_MS: i64 = 20;
/// Candidate-start search span (Chromium: 30 ms).
const SEARCH_INTERVAL_MS: i64 = 30;
/// Half-width, in frames, of the exclusion band around the previously chosen
/// block; re-picking it repeats audio and reads as buzz. Heuristic constant
/// from Chromium.
const EXCLUDE_HALF_FRAMES: i64 = 80;
/// Candidate positions and correlation samples are both decimated during the
/// broad search. The winning neighbourhood is then evaluated at full
/// resolution, reducing the normal search from roughly 2.3 million stereo
/// multiply-accumulates to under 170,000 without quantising the final offset.
const SEARCH_DECIMATION: usize = 4;

#[derive(Clone, Debug)]
pub struct PipelineConfig {
    pub edit: Option<TrackEdit>,
    pub speed: f32,
    pub position_ms: u32,
    /// One-based audible pass through a finite loop. Fresh loads and user
    /// seeks use pass one before the loop end and the final pass at or after
    /// it; an internal loop jump increments it.
    pub loop_pass: u32,
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
                .map(|edit| SampleEdit::new(edit, config.position_ms, config.loop_pass)),
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
    /// Completes a natural stream boundary. Unlike `reset_buffers`, this makes
    /// the stretcher's delayed overlap region audible before resetting it.
    pub fn finish(&mut self, output: &mut Vec<f32>) {
        output.clear();
        if let Some(stretcher) = &mut self.stretcher {
            stretcher.finish(output);
        }
    }

    pub fn reset_buffers(&mut self) {
        self.filtered.clear();
        self.loop_sent = false;
        if let Some(stretcher) = &mut self.stretcher {
            stretcher.reset();
        }
    }
}

struct SampleEdit {
    frame: u64,
    cuts: Vec<SampleRange>,
    cut_index: usize,
    loop_range: Option<SampleLoop>,
}

#[derive(Clone, Copy)]
struct SampleRange {
    start: u64,
    end: u64,
}

#[derive(Clone, Copy)]
struct SampleLoop {
    range: SampleRange,
    play_count: u32,
    pass: u32,
}

impl SampleEdit {
    fn new(edit: &TrackEdit, position_ms: u32, loop_pass: u32) -> Self {
        let cuts: Vec<_> = edit.cuts.iter().copied().map(sample_range).collect();
        let frame = ms_to_frame(position_ms);
        let cut_index = cuts.partition_point(|cut| cut.end <= frame);
        Self {
            frame,
            cuts,
            cut_index,
            loop_range: edit.loop_range.map(|loop_range| {
                let pass = if position_ms >= loop_range.end_ms {
                    loop_range.play_count.max(1)
                } else {
                    loop_pass.max(1)
                };
                SampleLoop {
                    range: sample_range(TimeRange {
                        start_ms: loop_range.start_ms,
                        end_ms: loop_range.end_ms,
                    }),
                    play_count: loop_range.play_count,
                    pass,
                }
            }),
        }
    }

    fn process(&mut self, input: &[f32], output: &mut Vec<f32>) -> Option<u32> {
        debug_assert!(input.len().is_multiple_of(CHANNELS));
        output.reserve(input.len());
        for source in input.chunks_exact(CHANNELS) {
            if let Some(loop_range) = self.loop_range {
                if loop_range.pass < loop_range.play_count && self.frame >= loop_range.range.end {
                    return Some(frame_to_ms(loop_range.range.start));
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
            if loop_range.pass < loop_range.play_count && self.frame >= loop_range.range.end {
                return Some(frame_to_ms(loop_range.range.start));
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

fn frames_for_ms(ms: i64) -> usize {
    ((ms * SAMPLE_RATE as i64 + 500) / 1_000) as usize
}

/// Copies `frames` interleaved frames starting at absolute frame `abs_start`
/// from the rolling input buffer, zero-filling anything before the start of
/// the stream. Free function so callers can mutably borrow other `Wsola`
/// scratch buffers without borrow-splitting fights.
fn extract(input: &[f32], input_start: usize, abs_start: i64, frames: usize, dest: &mut Vec<f32>) {
    dest.clear();
    dest.resize(frames * CHANNELS, 0.0);
    let rel = abs_start - input_start as i64;
    if rel < 0 {
        return; // Entirely before the stream: zeros.
    }
    let start = rel as usize;
    let available = input.len() / CHANNELS - start;
    let copy = frames.min(available);
    if copy > 0 {
        dest[..copy * CHANNELS]
            .copy_from_slice(&input[start * CHANNELS..start * CHANNELS + copy * CHANNELS]);
    }
}

fn frame_to_ms(frame: u64) -> u32 {
    ((frame * 1_000 + u64::from(SAMPLE_RATE) / 2) / u64::from(SAMPLE_RATE)).min(u64::from(u32::MAX))
        as u32
}

/// Streaming WSOLA stretcher ported from Chromium's AudioRendererAlgorithm
/// (media/filters/audio_renderer_algorithm.cc + wsola_internals.cc, BSD
/// 3-Clause, Copyright The Chromium Authors). Same structure: a 20 ms OLA
/// window at half-overlap with a periodic-Hann crossfade, a 30 ms search
/// region for the block that best continues the output, an exclusion band
/// around the previously chosen block (re-picking it buzzes), a decimated
/// correlation search, and a target-to-optimal transition blend so the
/// natural continuation always leads. Adapted to a push-based packet stream;
/// both channels share one search and one set of weights, so phase cannot
/// drift between them. The exact 1.0 transport path never constructs this
/// type.
pub struct Wsola {
    speed: f32,
    /// OLA analysis window, frames (20 ms, even).
    window: usize,
    /// Emitted frames per iteration; also the crossfade length (half window).
    hop: usize,
    /// Number of candidate start positions in the search region (30 ms).
    num_candidates: usize,
    /// Offset of the search-region center from its first frame.
    center_offset: i64,
    /// Periodic Hann over `hop`, amplitude-complementary halves.
    hann: Vec<f32>,
    /// Target-to-optimal transition weights over `2 * window`.
    transition: Vec<f32>,
    input: Vec<f32>,
    /// Absolute frame index of `input[0]` since stream start.
    input_start: usize,
    /// Real source frames accepted since the last reset (padding used to flush
    /// EOF is deliberately excluded).
    input_frames: usize,
    /// Frames already returned to the caller.
    emitted_frames: usize,
    /// Output-time position, in output frames.
    output_time: f64,
    /// Absolute frame index of the next target block (natural continuation).
    target_idx: i64,
    /// Absolute frame index of the search region start.
    search_idx: i64,
    /// Rolling overlap-add staging: completed frames, then the staged second
    /// half of the last block awaiting its blend.
    staged: Vec<f32>,
    staged_complete: usize,
    /// Scratch blocks: target continuation, searched optimal, blended result.
    target_buf: Vec<f32>,
    opt_buf: Vec<f32>,
    work_buf: Vec<f32>,
    /// Scalar candidate/target sample pairs evaluated. Kept as a cheap integer
    /// so tests can pin the work reduction instead of timing noisy CI hosts.
    search_comparisons: usize,
    search_count: usize,
}

impl Wsola {
    pub fn new(speed: f32) -> Self {
        assert!(speed.is_finite() && speed > 0.0);
        let window = frames_for_ms(OLA_WINDOW_MS);
        let window = window + (window & 1); // even, so hop is exact
        let hop = window / 2;
        let num_candidates = frames_for_ms(SEARCH_INTERVAL_MS);
        let center_offset = (num_candidates / 2 + (window / 2 - 1)) as i64;

        // Periodic Hann over the full window: its two halves are
        // amplitude-complementary at 50% overlap (COLA) — the first half
        // fades in the incoming block, the second half fades out the staged
        // one.
        let mut hann = vec![0.0f32; window];
        for (n, weight) in hann.iter_mut().enumerate() {
            *weight = 0.5 - 0.5 * (std::f32::consts::TAU * n as f32 / window as f32).cos();
        }
        // Transition weights across `2 * window`: the optimal block rises
        // from 0 while the target block falls from 1.
        let mut transition = vec![0.0f32; window * 2];
        for k in 0..window {
            let rise = 0.5 - 0.5 * (std::f32::consts::PI * k as f32 / window as f32).cos();
            transition[k] = rise;
            transition[window + k] = 1.0 - rise;
        }

        Self {
            speed,
            window,
            hop,
            num_candidates,
            center_offset,
            hann,
            transition,
            input: Vec::new(),
            input_start: 0,
            input_frames: 0,
            emitted_frames: 0,
            output_time: 0.0,
            target_idx: 0,
            search_idx: -center_offset,
            staged: Vec::new(),
            staged_complete: 0,
            target_buf: vec![0.0; window * CHANNELS],
            opt_buf: vec![0.0; window * CHANNELS],
            work_buf: vec![0.0; window * CHANNELS],
            search_comparisons: 0,
            search_count: 0,
        }
    }

    pub fn reset(&mut self) {
        self.input.clear();
        self.input_start = 0;
        self.input_frames = 0;
        self.emitted_frames = 0;
        self.output_time = 0.0;
        self.target_idx = 0;
        self.search_idx = -self.center_offset;
        self.staged.clear();
        self.staged_complete = 0;
        self.search_comparisons = 0;
        self.search_count = 0;
    }

    pub fn process(&mut self, input: &[f32], output: &mut Vec<f32>) {
        debug_assert!(input.len().is_multiple_of(CHANNELS));
        self.input_frames += input.len() / CHANNELS;
        self.input.extend_from_slice(input);
        while self.can_perform() {
            self.iterate();
        }
        self.emit_complete(output);
    }

    pub fn finish(&mut self, output: &mut Vec<f32>) {
        let wanted = (self.input_frames as f64 / self.speed as f64).round() as usize;
        while self.emitted_frames + self.staged.len() / CHANNELS < wanted {
            let required_end = (self.target_idx + self.window as i64)
                .max(self.search_idx + self.search_size() as i64)
                .max(self.input_start as i64);
            let required_frames = (required_end - self.input_start as i64) as usize;
            self.input.resize(required_frames * CHANNELS, 0.0);
            if !self.can_perform() {
                break;
            }
            self.iterate();
        }
        let remaining = wanted.saturating_sub(self.emitted_frames) * CHANNELS;
        output.extend_from_slice(&self.staged[..remaining.min(self.staged.len())]);
        self.reset();
    }

    fn emit_complete(&mut self, output: &mut Vec<f32>) {
        let done = self.staged_complete * CHANNELS;
        output.extend_from_slice(&self.staged[..done]);
        self.staged.drain(..done);
        self.emitted_frames += self.staged_complete;
        self.staged_complete = 0;
    }

    fn can_perform(&self) -> bool {
        let avail = self.input.len() / CHANNELS;
        let origin = self.input_start as i64;
        let rel_target = self.target_idx - origin;
        let rel_search = self.search_idx - origin;
        rel_target >= 0
            && rel_target + self.window as i64 <= avail as i64
            && rel_search + self.search_size() as i64 <= avail as i64
    }

    fn search_size(&self) -> usize {
        self.num_candidates + self.window - 1
    }

    /// One overlap-add step: find the optimal block, blend it against the
    /// target continuation, and stage `hop` completed frames.
    fn iterate(&mut self) {
        let window = self.window;
        let hop = self.hop;

        let optimal_abs = if self.target_within_search_region() {
            // The natural continuation sits inside the search region: no
            // search needed, the target block IS the optimal block.
            extract(
                &self.input,
                self.input_start,
                self.target_idx,
                window,
                &mut self.work_buf,
            );
            self.target_idx
        } else {
            self.extract_target(self.target_idx, window);

            // Exclude the band around the previously chosen block: matching
            // it again repeats audio and reads as buzz. Heuristic constant
            // from Chromium.
            let exclude_center = self.target_idx - hop as i64 - self.search_idx;
            let exclude_low = exclude_center - EXCLUDE_HALF_FRAMES;
            let exclude_high = exclude_center + EXCLUDE_HALF_FRAMES;
            let best = self.find_optimal_candidate(exclude_low, exclude_high);
            let optimal_abs = self.search_idx + best as i64;
            extract(
                &self.input,
                self.input_start,
                optimal_abs,
                window,
                &mut self.opt_buf,
            );

            // Transition blend: the target (best continuation of what was
            // just emitted) leads with falling weight while the optimal block
            // rises, smoothing the join between dissimilar blocks.
            for frame in 0..window {
                let w_opt = self.transition[frame];
                let w_target = self.transition[window + frame];
                for channel in 0..CHANNELS {
                    let index = frame * CHANNELS + channel;
                    self.work_buf[index] =
                        self.opt_buf[index] * w_opt + self.target_buf[index] * w_target;
                }
            }
            optimal_abs
        };

        // Overlap-add into staging: blend the staged second half against the
        // block's first half, then stage the block's second half verbatim.
        let need = (self.staged_complete + window) * CHANNELS;
        if self.staged.len() < need {
            self.staged.resize(need, 0.0);
        }
        let base = self.staged_complete * CHANNELS;
        for n in 0..hop {
            let w_out = self.hann[hop + n];
            let w_in = self.hann[n];
            for channel in 0..CHANNELS {
                let index = base + n * CHANNELS + channel;
                self.staged[index] =
                    self.staged[index] * w_out + self.work_buf[n * CHANNELS + channel] * w_in;
            }
        }
        let source = hop * CHANNELS;
        let destination = base + source;
        self.staged[destination..destination + source]
            .copy_from_slice(&self.work_buf[source..window * CHANNELS]);
        self.staged_complete += hop;

        // Next target is one hop ahead of the chosen block; the search region
        // re-centers on the nominal continuation of the output clock.
        self.target_idx = optimal_abs + hop as i64;
        self.output_time += hop as f64;
        self.search_idx =
            (self.output_time * self.speed as f64).round() as i64 - self.center_offset;

        // Drop input no candidate can reach again.
        let earliest = self.target_idx.min(self.search_idx);
        if earliest > self.input_start as i64 {
            let drop = ((earliest - self.input_start as i64) as usize) * CHANNELS;
            self.input.drain(..drop);
            self.input_start = earliest as usize;
        }
    }

    fn target_within_search_region(&self) -> bool {
        self.target_idx >= self.search_idx
            && self.target_idx + self.window as i64 <= self.search_idx + self.search_size() as i64
    }

    /// Copies `frames` interleaved frames starting at absolute frame
    /// `abs_start`, zero-filling anything before the start of the stream.
    fn extract_target(&mut self, abs_start: i64, frames: usize) {
        extract(
            &self.input,
            self.input_start,
            abs_start,
            frames,
            &mut self.target_buf,
        );
    }

    fn find_optimal_candidate(&mut self, exclude_low: i64, exclude_high: i64) -> usize {
        self.search_count += 1;
        let mut best = None;
        let mut best_similarity = f32::NEG_INFINITY;
        let last = self.num_candidates - 1;
        let mut candidate = 0;
        while candidate < self.num_candidates {
            self.consider_candidate(
                candidate,
                SEARCH_DECIMATION,
                exclude_low,
                exclude_high,
                &mut best,
                &mut best_similarity,
            );
            candidate += SEARCH_DECIMATION;
        }
        if last % SEARCH_DECIMATION != 0 {
            self.consider_candidate(
                last,
                SEARCH_DECIMATION,
                exclude_low,
                exclude_high,
                &mut best,
                &mut best_similarity,
            );
        }

        let coarse = best.unwrap_or(0);
        let low = coarse.saturating_sub(SEARCH_DECIMATION - 1);
        let high = (coarse + SEARCH_DECIMATION - 1).min(last);
        best = None;
        best_similarity = f32::NEG_INFINITY;
        for candidate in low..=high {
            self.consider_candidate(
                candidate,
                1,
                exclude_low,
                exclude_high,
                &mut best,
                &mut best_similarity,
            );
        }
        best.unwrap_or(coarse)
    }

    fn consider_candidate(
        &mut self,
        candidate: usize,
        sample_step: usize,
        exclude_low: i64,
        exclude_high: i64,
        best: &mut Option<usize>,
        best_similarity: &mut f32,
    ) {
        let candidate_i = candidate as i64;
        if candidate_i >= exclude_low && candidate_i <= exclude_high {
            return;
        }
        let (similarity, comparisons) = similarity(
            &self.input,
            self.input_start,
            self.search_idx,
            candidate_i,
            self.window,
            &self.target_buf,
            sample_step,
        );
        self.search_comparisons += comparisons;
        if similarity > *best_similarity {
            *best_similarity = similarity;
            *best = Some(candidate);
        }
    }

    #[cfg(test)]
    fn search_comparisons(&self) -> usize {
        self.search_comparisons
    }
}

/// Per-channel normalized cross-similarity between the target block and a
/// candidate. `sample_step` decimates only the broad measurement; refinement
/// always calls this with one.
fn similarity(
    input: &[f32],
    input_start: usize,
    search_abs: i64,
    candidate: i64,
    window: usize,
    target: &[f32],
    sample_step: usize,
) -> (f32, usize) {
    let origin = input_start as i64;
    let first = search_abs + candidate;
    let zero_frames = ((origin - first).clamp(0, window as i64)) as usize;
    let first_sample = zero_frames.div_ceil(sample_step) * sample_step;
    let mut sum = 0.0f32;
    let mut comparisons = 0;
    for channel in 0..CHANNELS {
        let mut dot = 0.0f32;
        let mut energy_target = 0.0f32;
        let mut energy_candidate = 0.0f32;
        for n in (0..zero_frames).step_by(sample_step) {
            let t = target[n * CHANNELS + channel];
            energy_target += t * t;
            comparisons += 1;
        }
        for n in (first_sample..window).step_by(sample_step) {
            let t = target[n * CHANNELS + channel];
            let index = ((first - origin + n as i64) as usize) * CHANNELS + channel;
            let c = input[index];
            dot += t * c;
            energy_target += t * t;
            energy_candidate += c * c;
            comparisons += 1;
        }
        sum += dot / (energy_target * energy_candidate).sqrt().max(1.0e-12);
    }
    (sum, comparisons)
}

#[cfg(test)]
mod tests {
    use super::*;
    use renderer_engine::protocol::LoopRange;

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

    fn pipeline_in_packets(
        edit: TrackEdit,
        speed: f32,
        position_ms: u32,
        input: &[f32],
        packet_frames: &[usize],
    ) -> (Vec<f32>, Option<u32>) {
        let mut pipeline = AudioPipeline::new(&PipelineConfig {
            edit: Some(edit),
            speed,
            position_ms,
            loop_pass: 1,
        });
        let mut output = Vec::new();
        let mut packet_output = Vec::new();
        let mut frame = 0;
        let total = input.len() / CHANNELS;
        let mut packet = 0;
        let mut loop_to = None;
        while frame < total && loop_to.is_none() {
            let count = packet_frames[packet % packet_frames.len()].min(total - frame);
            loop_to = pipeline.process(
                &input[frame * CHANNELS..(frame + count) * CHANNELS],
                &mut packet_output,
            );
            output.extend_from_slice(&packet_output);
            frame += count;
            packet += 1;
        }
        if loop_to.is_none() {
            pipeline.finish(&mut packet_output);
            output.extend_from_slice(&packet_output);
        }
        (output, loop_to)
    }

    fn source_ramp(start_frame: usize, frames: usize) -> Vec<f32> {
        (start_frame..start_frame + frames)
            .flat_map(|frame| [frame as f32, -(frame as f32)])
            .collect()
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
            // Measured worst case across 0.5x..2x is ±0.11 Hz; ±0.5 Hz is
            // roughly 4x headroom over that.
            assert!(
                (measured - 440.0).abs() < 0.5,
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
        // Measured +0.45 Hz after a mid-stream reset; ±2 Hz is ~4x headroom.
        assert!((positive_crossing_frequency(&output) - 880.0).abs() < 2.0);
    }

    #[test]
    fn cuts_are_exact_and_packet_boundary_independent_at_one_x() {
        let edit = TrackEdit {
            cuts: vec![
                TimeRange {
                    start_ms: 10,
                    end_ms: 30,
                },
                TimeRange {
                    start_ms: 50,
                    end_ms: 70,
                },
                TimeRange {
                    start_ms: 90,
                    end_ms: 100,
                },
            ],
            loop_range: None,
        };
        let frames = ms_to_frame(100) as usize;
        let input = source_ramp(0, frames);
        let expected: Vec<_> = input
            .chunks_exact(CHANNELS)
            .enumerate()
            .filter(|(frame, _)| {
                !edit.cuts.iter().any(|cut| {
                    *frame >= ms_to_frame(cut.start_ms) as usize
                        && *frame < ms_to_frame(cut.end_ms) as usize
                })
            })
            .flat_map(|(_, frame)| frame.iter().copied())
            .collect();

        let (whole, _) = pipeline_in_packets(edit.clone(), 1.0, 0, &input, &[frames]);
        let (packet_aligned, _) =
            pipeline_in_packets(edit.clone(), 1.0, 0, &input, &[ms_to_frame(10) as usize]);
        let (irregular, _) = pipeline_in_packets(edit, 1.0, 0, &input, &[127, 441, 997]);
        assert_eq!(whole, expected);
        assert_eq!(packet_aligned, expected);
        assert_eq!(irregular, expected);
    }

    #[test]
    fn edited_duration_is_source_minus_cuts_at_every_supported_speed_extreme() {
        let edit = TrackEdit {
            cuts: vec![
                TimeRange {
                    start_ms: 20,
                    end_ms: 80,
                },
                TimeRange {
                    start_ms: 120,
                    end_ms: 150,
                },
                TimeRange {
                    start_ms: 180,
                    end_ms: 200,
                },
            ],
            loop_range: None,
        };
        let source_frames = ms_to_frame(200) as usize;
        let removed_frames: usize = edit
            .cuts
            .iter()
            .map(|cut| (ms_to_frame(cut.end_ms) - ms_to_frame(cut.start_ms)) as usize)
            .sum();
        let audible_frames = source_frames - removed_frames;
        let input = stereo_tone(source_frames, 440.0, -0.25);

        for speed in [0.5, 1.0, 2.0] {
            let (output, loop_to) =
                pipeline_in_packets(edit.clone(), speed, 0, &input, &[882, 2_205, 311]);
            assert_eq!(loop_to, None);
            assert_eq!(
                output.len() / CHANNELS,
                (audible_frames as f64 / f64::from(speed)).round() as usize,
                "speed {speed} must apply only after cuts and retain the natural EOF tail"
            );
        }
    }

    #[test]
    fn source_time_seek_inside_a_cut_resumes_at_the_cut_end() {
        let edit = TrackEdit {
            cuts: vec![
                TimeRange {
                    start_ms: 40,
                    end_ms: 70,
                },
                TimeRange {
                    start_ms: 90,
                    end_ms: 110,
                },
            ],
            loop_range: None,
        };
        let position_ms = 50;
        let start = ms_to_frame(position_ms) as usize;
        let end = ms_to_frame(130) as usize;
        let input = source_ramp(start, end - start);
        let (output, _) = pipeline_in_packets(edit, 1.0, position_ms, &input, &[127, 1_024, 333]);
        let audible_source_frames: Vec<_> = output
            .chunks_exact(CHANNELS)
            .map(|frame| frame[0] as usize)
            .collect();

        assert_eq!(
            audible_source_frames.first().copied(),
            Some(ms_to_frame(70) as usize)
        );
        assert!(audible_source_frames
            .iter()
            .all(|frame| *frame < ms_to_frame(90) as usize || *frame >= ms_to_frame(110) as usize));
        assert_eq!(audible_source_frames.last().copied(), Some(end - 1));
    }

    #[test]
    fn loop_boundary_uses_original_time_after_cuts_and_speed() {
        let edit = TrackEdit {
            cuts: vec![TimeRange {
                start_ms: 5,
                end_ms: 10,
            }],
            loop_range: Some(LoopRange {
                start_ms: 20,
                end_ms: 55,
                play_count: 2,
            }),
        };
        let start = ms_to_frame(20) as usize;
        let input = stereo_tone(ms_to_frame(80) as usize - start, 523.25, 1.0);
        let audible_frames = (ms_to_frame(55) - ms_to_frame(20)) as usize;

        for speed in [0.5, 1.0, 2.0] {
            let (output, loop_to) =
                pipeline_in_packets(edit.clone(), speed, 20, &input, &[127, 882, 2_048]);
            assert_eq!(loop_to, Some(20));
            assert_eq!(
                output.len() / CHANNELS,
                (audible_frames as f64 / f64::from(speed)).round() as usize,
                "speed {speed} must flush exactly through the original-time loop end"
            );
        }
    }

    #[test]
    fn reset_buffers_rearms_a_loop_marker_for_the_resumed_pass() {
        let edit = TrackEdit {
            cuts: Vec::new(),
            loop_range: Some(LoopRange {
                start_ms: 20,
                end_ms: 40,
                play_count: 2,
            }),
        };
        let mut pipeline = AudioPipeline::new(&PipelineConfig {
            edit: Some(edit),
            speed: 1.0,
            position_ms: 0,
            loop_pass: 1,
        });
        let mut output = Vec::new();
        let first = source_ramp(0, ms_to_frame(80) as usize);
        assert_eq!(pipeline.process(&first, &mut output), Some(20));

        pipeline.reset_buffers();
        output.clear();
        let resumed = source_ramp(
            ms_to_frame(40) as usize,
            (ms_to_frame(80) - ms_to_frame(40)) as usize,
        );
        assert_eq!(pipeline.process(&resumed, &mut output), Some(20));
    }

    #[test]
    fn finite_loop_emits_exactly_the_requested_number_of_passes() {
        for play_count in [2, 3] {
            let edit = TrackEdit {
                cuts: Vec::new(),
                loop_range: Some(LoopRange {
                    start_ms: 20,
                    end_ms: 40,
                    play_count,
                }),
            };
            let total_frames = ms_to_frame(80) as usize;
            let loop_start = ms_to_frame(20) as usize;
            let input = source_ramp(0, total_frames);

            let mut first = SampleEdit::new(&edit, 0, 1);
            let mut output = Vec::new();
            assert_eq!(first.process(&input, &mut output), Some(20));

            for pass in 2..=play_count {
                let mut repeated = SampleEdit::new(&edit, 20, pass);
                let repeated_input = source_ramp(loop_start, total_frames - loop_start);
                output.clear();
                let marker = repeated.process(&repeated_input, &mut output);
                if pass < play_count {
                    assert_eq!(marker, Some(20));
                } else {
                    assert_eq!(marker, None);
                    assert_eq!(output.len(), (total_frames - loop_start) * CHANNELS);
                }
            }
        }
    }

    #[test]
    fn starting_at_or_after_loop_end_never_emits_a_loop_marker() {
        let edit = TrackEdit {
            cuts: Vec::new(),
            loop_range: Some(LoopRange {
                start_ms: 20,
                end_ms: 40,
                play_count: 3,
            }),
        };
        let total_frames = ms_to_frame(80) as usize;

        for position_ms in [40, 41, 80] {
            let start_frame = ms_to_frame(position_ms) as usize;
            let input = source_ramp(start_frame, total_frames - start_frame);
            let (output, loop_to) =
                pipeline_in_packets(edit.clone(), 1.0, position_ms, &input, &[127, 882, 2_048]);

            assert_eq!(loop_to, None, "position {position_ms} must be a final pass");
            assert_eq!(
                output.len() / CHANNELS,
                total_frames - start_frame,
                "position {position_ms} must retain the natural EOF tail"
            );
        }
    }

    #[test]
    fn coarse_search_materially_reduces_correlation_work() {
        let input = stereo_tone(SAMPLE_RATE as usize * 2, 440.0, 0.7);
        let mut stretcher = Wsola::new(2.0);
        let mut output = Vec::new();
        stretcher.process(&input, &mut output);

        let exhaustive_per_search = stretcher.num_candidates * stretcher.window * CHANNELS;
        assert!(stretcher.search_count > 0, "the 2x path must search");
        let average = stretcher.search_comparisons() / stretcher.search_count;
        assert!(
            average * 8 < exhaustive_per_search,
            "decimation/refinement averaged {average} comparisons versus \
             {exhaustive_per_search} for exhaustive search"
        );
    }

    #[test]
    fn finish_preserves_the_exact_delayed_eof_duration() {
        let frames = frames_for_ms(37);
        let input = stereo_tone(frames, 611.0, -0.25);
        for speed in [0.5, 0.75, 1.25, 2.0] {
            let output = stretch_in_packets(&input, speed, &[127, 293]);
            assert_eq!(
                output.len() / CHANNELS,
                (frames as f64 / speed as f64).round() as usize,
                "speed {speed} dropped or invented the WSOLA EOF tail"
            );
        }
    }
}
