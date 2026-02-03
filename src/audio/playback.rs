use std::sync::atomic::Ordering;
use std::sync::Arc;

use anyhow::{Context, Result};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{SampleFormat, Stream};

use super::buffer::AudioBuffer;
use crate::analysis::slicer::Slice;
use crate::engine::effects::StepEffects;
use crate::engine::state::{PlayMode, PlaybackState};

/// Start the cpal output stream. Returns the Stream (must be kept alive).
pub fn start_playback(
    audio: &AudioBuffer,
    slices: &[Slice],
    state: Arc<PlaybackState>,
) -> Result<Stream> {
    let host = cpal::default_host();
    let device = host
        .default_output_device()
        .context("No audio output device found")?;

    let supported = device
        .default_output_config()
        .context("No supported output config")?;

    let out_channels = supported.channels() as usize;
    let out_sample_rate = supported.sample_rate().0;

    // Clone data for the audio callback
    let samples: Arc<[f32]> = audio.samples.clone().into();
    let slice_ranges: Arc<[(usize, usize)]> = slices
        .iter()
        .map(|s| (s.start, s.end))
        .collect::<Vec<_>>()
        .into();
    let total_samples = samples.len();

    let config = cpal::StreamConfig {
        channels: out_channels as u16,
        sample_rate: cpal::SampleRate(out_sample_rate),
        buffer_size: cpal::BufferSize::Default,
    };

    let stream = match supported.sample_format() {
        SampleFormat::F32 => build_stream::<f32>(
            &device,
            &config,
            samples,
            slice_ranges,
            total_samples,
            out_channels,
            out_sample_rate,
            state,
        )?,
        SampleFormat::I16 => build_stream::<i16>(
            &device,
            &config,
            samples,
            slice_ranges,
            total_samples,
            out_channels,
            out_sample_rate,
            state,
        )?,
        SampleFormat::U16 => build_stream::<u16>(
            &device,
            &config,
            samples,
            slice_ranges,
            total_samples,
            out_channels,
            out_sample_rate,
            state,
        )?,
        _ => anyhow::bail!("Unsupported sample format"),
    };

    stream.play().context("Failed to start audio stream")?;
    Ok(stream)
}

trait SampleConvert {
    fn from_f32(s: f32) -> Self;
}

impl SampleConvert for f32 {
    fn from_f32(s: f32) -> Self {
        s
    }
}

impl SampleConvert for i16 {
    fn from_f32(s: f32) -> Self {
        (s.clamp(-1.0, 1.0) * i16::MAX as f32) as i16
    }
}

impl SampleConvert for u16 {
    fn from_f32(s: f32) -> Self {
        ((s.clamp(-1.0, 1.0) * 0.5 + 0.5) * u16::MAX as f32) as u16
    }
}

fn build_stream<T: SampleConvert + cpal::SizedSample + Send + 'static>(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    samples: Arc<[f32]>,
    slice_ranges: Arc<[(usize, usize)]>,
    total_samples: usize,
    out_channels: usize,
    sample_rate: u32,
    state: Arc<PlaybackState>,
) -> Result<Stream> {
    // Filter state persists across callback invocations in the closure
    let mut lp_z1: f32 = 0.0;
    let mut hp_z1: f32 = 0.0;
    let mut hp_prev_in: f32 = 0.0;
    let mut phase_acc: f64 = 0.0;
    let mut last_effects = StepEffects::default();
    let mut effects_changed = false;

    // Precompute filter coefficients
    let lp_alpha = compute_lp_alpha(800.0, sample_rate as f32);
    let hp_alpha = compute_hp_alpha(2000.0, sample_rate as f32);

    let stream = device
        .build_output_stream(
            config,
            move |data: &mut [T], _: &cpal::OutputCallbackInfo| {
                if !state.playing.load(Ordering::Relaxed) {
                    for sample in data.iter_mut() {
                        *sample = T::from_f32(0.0);
                    }
                    return;
                }

                // Check for retrigger
                if state.retrigger.swap(false, Ordering::Acquire) {
                    phase_acc = 0.0;
                    effects_changed = true;
                }

                // Read effects (changes once per sequencer step, much slower than audio rate)
                let effects = state.get_effects();
                if effects_changed
                    || effects.lowpass != last_effects.lowpass
                    || effects.highpass != last_effects.highpass
                {
                    // Reset filter state on effect change to avoid transients
                    if effects.lowpass != last_effects.lowpass {
                        lp_z1 = 0.0;
                    }
                    if effects.highpass != last_effects.highpass {
                        hp_z1 = 0.0;
                        hp_prev_in = 0.0;
                    }
                    effects_changed = false;
                }
                last_effects = effects;

                let mode = PlayMode::from_u8(state.mode.load(Ordering::Relaxed));
                let slice_idx = state.active_slice.load(Ordering::Relaxed) as usize;

                let (slice_start, slice_end) = if slice_idx < slice_ranges.len() {
                    slice_ranges[slice_idx]
                } else {
                    (0, total_samples)
                };

                let mut pos = state.position.load(Ordering::Relaxed) as usize;

                // Compute pitch ratio
                let pitch_ratio = if effects.pitch_cents != 0 {
                    2.0_f64.powf(effects.pitch_cents as f64 / 1200.0)
                } else if effects.half_speed {
                    0.5
                } else if effects.double_speed {
                    2.0
                } else {
                    1.0
                };

                let frames = data.len() / out_channels;
                for frame in 0..frames {
                    // 1. Fetch raw sample
                    let raw = fetch_sample(
                        &samples,
                        total_samples,
                        &mut pos,
                        &mut phase_acc,
                        &effects,
                        pitch_ratio,
                        mode,
                        slice_start,
                        slice_end,
                        &state,
                    );

                    // 2. Low-pass filter
                    let after_lp = if effects.lowpass {
                        let filtered = lp_z1 + lp_alpha * (raw - lp_z1);
                        lp_z1 = filtered;
                        let mix = effects.lowpass_mix;
                        raw * (1.0 - mix) + filtered * mix
                    } else {
                        raw
                    };

                    // 3. High-pass filter
                    let after_hp = if effects.highpass {
                        let filtered = hp_alpha * (hp_z1 + after_lp - hp_prev_in);
                        hp_prev_in = after_lp;
                        hp_z1 = filtered;
                        let mix = effects.highpass_mix;
                        after_lp * (1.0 - mix) + filtered * mix
                    } else {
                        after_lp
                    };

                    // 4. Distortion (with gain compensation for perceived loudness)
                    let after_dist = if effects.distortion {
                        let drive = 8.0_f32;
                        let distorted = (after_hp * drive).tanh();
                        // Compensate for loudness increase from saturation
                        // tanh compression + high drive increases perceived loudness ~3x
                        let compensated = distorted * 0.35;
                        let mix = effects.distortion_mix;
                        after_hp * (1.0 - mix) + compensated * mix
                    } else {
                        after_hp
                    };

                    // 5. Fade envelope (calculated based on position within slice)
                    let mut gain = 1.0_f32;
                    let slice_len = (slice_end - slice_start) as f32;
                    let current_offset = if pos >= slice_start {
                        (pos - slice_start) as f32
                    } else {
                        0.0
                    };
                    let progress = if slice_len > 0.0 {
                        (current_offset / slice_len).clamp(0.0, 1.0)
                    } else {
                        0.0
                    };

                    if effects.fade_in > 0.0 {
                        // Fade in: 0 -> 1 over first half of beat
                        let fade_gain = (progress * 2.0_f32).clamp(0.0, 1.0);
                        gain *= fade_gain;
                    }
                    if effects.fade_out > 0.0 {
                        // Fade out: 1 -> 0 over first half of beat
                        let fade_gain = (1.0_f32 - progress * 2.0_f32).clamp(0.0, 1.0);
                        gain *= fade_gain;
                    }
                    let final_val = after_dist * gain;

                    // Write to all output channels
                    for ch in 0..out_channels {
                        data[frame * out_channels + ch] = T::from_f32(final_val);
                    }
                }

                state.position.store(pos as u32, Ordering::Relaxed);
            },
            |err| {
                eprintln!("Audio stream error: {}", err);
            },
            None,
        )
        .context("Failed to build output stream")?;

    Ok(stream)
}

/// Fetch a single sample, handling reverse, pitch shift, and play modes.
fn fetch_sample(
    samples: &[f32],
    total_samples: usize,
    pos: &mut usize,
    phase_acc: &mut f64,
    effects: &StepEffects,
    pitch_ratio: f64,
    mode: PlayMode,
    slice_start: usize,
    slice_end: usize,
    state: &PlaybackState,
) -> f32 {
    // Handle stutter mode (existing mechanism)
    if mode == PlayMode::Stutter {
        let stutter_start = state.stutter_start.load(Ordering::Relaxed) as usize;
        let stutter_len = state.stutter_len.load(Ordering::Relaxed) as usize;
        if stutter_len == 0 || *pos >= total_samples {
            state.playing.store(false, Ordering::Relaxed);
            return 0.0;
        }
        let offset = (*pos - stutter_start) % stutter_len;
        let read_pos = stutter_start + offset;
        let v = if read_pos < total_samples {
            samples[read_pos]
        } else {
            0.0
        };
        *pos += 1;
        return v;
    }

    // Determine bounds
    let bound = match mode {
        PlayMode::Slice => slice_end,
        PlayMode::FreeRun => total_samples,
        PlayMode::Stutter => total_samples, // handled above
    };

    if effects.reverse {
        // Reverse: position starts at end and decrements
        if *pos == 0 || (mode == PlayMode::Slice && *pos <= slice_start) {
            state.playing.store(false, Ordering::Relaxed);
            return 0.0;
        }

        if pitch_ratio != 1.0 {
            // Fractional resampling in reverse
            let fpos = *pos as f64 - *phase_acc;
            let idx = fpos as usize;
            let frac = (fpos - idx as f64) as f32;
            let s0 = if idx < total_samples { samples[idx] } else { 0.0 };
            let s1 = if idx > 0 { samples[idx - 1] } else { s0 };
            *phase_acc += pitch_ratio;
            let steps = *phase_acc as usize;
            *phase_acc -= steps as f64;
            *pos = pos.saturating_sub(steps);
            s0 + (s1 - s0) * frac
        } else {
            let v = samples[*pos];
            *pos -= 1;
            v
        }
    } else {
        // Forward playback
        if *pos >= bound {
            state.playing.store(false, Ordering::Relaxed);
            return 0.0;
        }

        if pitch_ratio != 1.0 {
            // Fractional resampling forward
            let fpos = *pos as f64 + *phase_acc;
            let idx = fpos as usize;
            let frac = (fpos - idx as f64) as f32;
            let s0 = if idx < total_samples { samples[idx] } else { 0.0 };
            let s1 = if idx + 1 < total_samples {
                samples[idx + 1]
            } else {
                s0
            };
            *phase_acc += pitch_ratio;
            let steps = *phase_acc as usize;
            *phase_acc -= steps as f64;
            *pos += steps;
            s0 + (s1 - s0) * frac
        } else {
            let v = samples[*pos];
            *pos += 1;
            v
        }
    }
}

/// Compute low-pass filter coefficient (1-pole IIR).
/// alpha = dt / (rc + dt) where rc = 1/(2*pi*cutoff)
fn compute_lp_alpha(cutoff_hz: f32, sample_rate: f32) -> f32 {
    let dt = 1.0 / sample_rate;
    let rc = 1.0 / (2.0 * std::f32::consts::PI * cutoff_hz);
    dt / (rc + dt)
}

/// Compute high-pass filter coefficient (1-pole IIR).
/// alpha = rc / (rc + dt)
fn compute_hp_alpha(cutoff_hz: f32, sample_rate: f32) -> f32 {
    let dt = 1.0 / sample_rate;
    let rc = 1.0 / (2.0 * std::f32::consts::PI * cutoff_hz);
    rc / (rc + dt)
}
