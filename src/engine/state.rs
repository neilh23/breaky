use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicU8, Ordering};
use std::sync::Arc;

use super::effects::StepEffects;

/// Play modes for the engine.
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlayMode {
    /// Play a single slice, stop at its boundary.
    Slice = 0,
    /// Free-run: play from slice start to end of buffer.
    FreeRun = 1,
    /// Stutter: rapid re-trigger within the slice.
    Stutter = 2,
}

impl PlayMode {
    pub fn from_u8(v: u8) -> Self {
        match v {
            1 => PlayMode::FreeRun,
            2 => PlayMode::Stutter,
            _ => PlayMode::Slice,
        }
    }
}

/// Lock-free playback state shared between the UI thread and audio callback.
pub struct PlaybackState {
    /// Whether audio is currently playing.
    pub playing: AtomicBool,
    /// Index of the currently active slice (0-15).
    pub active_slice: AtomicU8,
    /// Current playback position (sample index within the buffer).
    pub position: AtomicU32,
    /// Current play mode.
    pub mode: AtomicU8,
    /// Stutter loop length in samples.
    pub stutter_len: AtomicU32,
    /// Stutter loop start position.
    pub stutter_start: AtomicU32,
    /// Set to true to trigger a new slice play.
    pub retrigger: AtomicBool,
    /// Packed effect flags for the current step.
    pub effect_flags: AtomicU64,
    /// Low-pass filter cutoff frequency (Hz), stored as f32 bits.
    pub lp_cutoff: AtomicU32,
    /// High-pass filter cutoff frequency (Hz), stored as f32 bits.
    pub hp_cutoff: AtomicU32,
    /// Distortion amount (0.0-1.0), stored as f32 bits.
    pub dist_amount: AtomicU32,
    /// Fade point (0.0-1.0), stored as f32 bits.
    pub fade_point: AtomicU32,
    /// Slow speed ratio, stored as f32 bits.
    pub slow_ratio: AtomicU32,
    /// Fast speed ratio, stored as f32 bits.
    pub fast_ratio: AtomicU32,
}

impl PlaybackState {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            playing: AtomicBool::new(false),
            active_slice: AtomicU8::new(0),
            position: AtomicU32::new(0),
            mode: AtomicU8::new(PlayMode::Slice as u8),
            stutter_len: AtomicU32::new(0),
            stutter_start: AtomicU32::new(0),
            retrigger: AtomicBool::new(false),
            effect_flags: AtomicU64::new(0),
            lp_cutoff: AtomicU32::new(800.0_f32.to_bits()),
            hp_cutoff: AtomicU32::new(2000.0_f32.to_bits()),
            dist_amount: AtomicU32::new(0.2_f32.to_bits()),
            fade_point: AtomicU32::new(0.5_f32.to_bits()),
            slow_ratio: AtomicU32::new(0.5_f32.to_bits()),
            fast_ratio: AtomicU32::new(2.0_f32.to_bits()),
        })
    }

    pub fn trigger_slice(&self, slice_index: u8, start_pos: u32, mode: PlayMode) {
        self.active_slice.store(slice_index, Ordering::Relaxed);
        self.position.store(start_pos, Ordering::Relaxed);
        self.mode.store(mode as u8, Ordering::Relaxed);
        self.playing.store(true, Ordering::Relaxed);
        self.retrigger.store(true, Ordering::Release);
    }

    pub fn trigger_stutter(&self, stutter_length: u32) {
        let pos = self.position.load(Ordering::Relaxed);
        self.stutter_start.store(pos, Ordering::Relaxed);
        self.stutter_len.store(stutter_length, Ordering::Relaxed);
        self.mode.store(PlayMode::Stutter as u8, Ordering::Relaxed);
    }

    pub fn stop(&self) {
        self.playing.store(false, Ordering::Relaxed);
    }

    pub fn is_playing(&self) -> bool {
        self.playing.load(Ordering::Relaxed)
    }

    pub fn get_active_slice(&self) -> u8 {
        self.active_slice.load(Ordering::Relaxed)
    }

    pub fn get_mode(&self) -> PlayMode {
        PlayMode::from_u8(self.mode.load(Ordering::Relaxed))
    }

    pub fn set_effects(&self, effects: &StepEffects) {
        self.effect_flags.store(effects.pack(), Ordering::Release);
    }

    pub fn get_effects(&self) -> StepEffects {
        StepEffects::unpack(self.effect_flags.load(Ordering::Acquire))
    }
}
