use std::time::{Duration, Instant};

use crate::analysis::slicer::Slice;
use crate::config;
use crate::engine::effects::{self, StepEffects};
use crate::engine::state::{PlayMode, PlaybackState};

pub struct SequencerState {
    pub beats: Vec<String>,
    pub beat_sequence: Vec<Option<usize>>,
    pub effect_sequence: Vec<StepEffects>,
    pub total_steps: usize,
    pub current_step: usize,
    pub playing: bool,
    pub stop_at_step: Option<usize>,
    pub last_step: Instant,
    pub step_duration: Duration,
    pub base_step_secs: f64,
    pub original_bpm: f64,
    pub stutter_len: u32,
    pub dirty: bool,
}

impl SequencerState {
    pub fn new(beats: Vec<String>, bpm: f64, duration_secs: f64, sample_rate: u32, stutter_divs: u32) -> Self {
        let beat_sequence = recompute_sequence(&beats);
        let effect_sequence = effects::compute_effect_sequence(&beats);
        let total_steps = beat_sequence.len();
        let base_step_secs = duration_secs / (total_steps as f64 * 2.0);
        let step_duration = Duration::from_secs_f64(base_step_secs);
        let beat_samples = (60.0 / bpm * sample_rate as f64) as u32;
        let stutter_len = beat_samples / stutter_divs;

        Self {
            beats,
            beat_sequence,
            effect_sequence,
            total_steps,
            current_step: 0,
            playing: true,
            stop_at_step: None,
            last_step: Instant::now(),
            step_duration,
            base_step_secs,
            original_bpm: bpm,
            stutter_len,
            dirty: false,
        }
    }

    /// Recompute beat_sequence, effect_sequence, total_steps from current beats.
    pub fn rebuild_sequences(&mut self) {
        self.beat_sequence = recompute_sequence(&self.beats);
        self.effect_sequence = effects::compute_effect_sequence(&self.beats);
        self.total_steps = self.beat_sequence.len();
        if self.current_step >= self.total_steps && self.total_steps > 0 {
            self.current_step = 0;
        }
    }

    /// Update timing when BPM or stutter changes.
    pub fn update_timing(&mut self, bpm: f64, sample_rate: u32, stutter_divs: u32) {
        self.step_duration = Duration::from_secs_f64(self.base_step_secs * (self.original_bpm / bpm));
        let beat_samples = (60.0 / bpm * sample_rate as f64) as u32;
        self.stutter_len = beat_samples / stutter_divs;
    }

    /// Advance the sequencer by one step, triggering audio. Returns true if a step was advanced.
    pub fn advance_step(
        &mut self,
        state: &PlaybackState,
        slices: &[Slice],
        num_slices: usize,
    ) -> bool {
        if !self.playing || self.total_steps == 0 || self.last_step.elapsed() < self.step_duration {
            return false;
        }

        // Set effects for this step
        if self.current_step < self.effect_sequence.len() {
            state.set_effects(&self.effect_sequence[self.current_step]);
        }

        match self.beat_sequence[self.current_step] {
            Some(base_idx) => {
                let fx = if self.current_step < self.effect_sequence.len() {
                    &self.effect_sequence[self.current_step]
                } else {
                    &StepEffects::default()
                };

                let idx = base_idx + (fx.bank as usize * crate::analysis::slicer::SLICES_PER_BANK);

                if idx < num_slices {
                    let slice = &slices[idx];

                    if fx.reverse {
                        state.trigger_slice(idx as u8, (slice.end - 1) as u32, PlayMode::FreeRun);
                    } else if fx.stutter {
                        state.trigger_slice(idx as u8, slice.start as u32, PlayMode::FreeRun);
                        state.trigger_stutter(self.stutter_len);
                    } else {
                        state.trigger_slice(idx as u8, slice.start as u32, PlayMode::FreeRun);
                    }
                } else {
                    state.stop();
                }
            }
            _ => {
                state.stop();
            }
        }

        self.current_step = (self.current_step + 1) % self.total_steps;
        if self.stop_at_step == Some(self.current_step) {
            self.playing = false;
            self.stop_at_step = None;
            state.stop();
        }
        self.last_step += self.step_duration;
        true
    }

    /// Play only the line at line_idx, then stop.
    pub fn play_line(&mut self, line_idx: usize) {
        let mut line_start = 0usize;
        for i in 0..line_idx {
            let p = config::ParsedBeatLine::parse(&self.beats[i]);
            line_start += p.step_count();
        }
        let p = config::ParsedBeatLine::parse(&self.beats[line_idx]);
        let line_end = line_start + p.step_count();
        self.current_step = line_start;
        self.stop_at_step = Some(line_end);
        self.playing = true;
        self.last_step = Instant::now();
    }

    /// Restart from beginning.
    pub fn restart(&mut self) {
        self.current_step = 0;
        self.stop_at_step = None;
        self.playing = true;
        self.last_step = Instant::now();
    }

    /// Toggle play/pause.
    pub fn toggle(&mut self, state: &PlaybackState) {
        self.playing = !self.playing;
        self.stop_at_step = None;
        if self.playing {
            self.last_step = Instant::now();
        } else {
            state.stop();
        }
    }
}

pub fn recompute_sequence(beats: &[String]) -> Vec<Option<usize>> {
    beats
        .iter()
        .flat_map(|raw| {
            config::ParsedBeatLine::parse(raw)
                .notes
                .into_iter()
                .map(config::char_to_slice)
                .collect::<Vec<_>>()
        })
        .collect()
}
