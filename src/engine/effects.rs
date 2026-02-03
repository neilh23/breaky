use crate::config::{command_to_pitch_cents, ParsedBeatLine};

/// Per-step effect state, precomputed from command sequences.
#[derive(Debug, Clone, Copy, Default)]
pub struct StepEffects {
    pub reverse: bool,
    pub stutter: bool,
    pub distortion: bool,
    pub lowpass: bool,
    pub highpass: bool,
    pub half_speed: bool,
    pub double_speed: bool,
    pub distortion_mix: f32,
    pub lowpass_mix: f32,
    pub highpass_mix: f32,
    pub fade_in: f32,
    pub fade_out: f32,
    pub pitch_cents: i32,
}

impl StepEffects {
    /// Pack into a u64 for atomic transfer to the audio thread.
    pub fn pack(&self) -> u64 {
        let mut v: u64 = 0;
        if self.reverse {
            v |= 1 << 0;
        }
        if self.stutter {
            v |= 1 << 1;
        }
        if self.distortion {
            v |= 1 << 2;
        }
        if self.lowpass {
            v |= 1 << 3;
        }
        if self.highpass {
            v |= 1 << 4;
        }
        if self.half_speed {
            v |= 1 << 5;
        }
        if self.double_speed {
            v |= 1 << 6;
        }
        v |= ((self.distortion_mix * 255.0) as u64 & 0xFF) << 7;
        v |= ((self.lowpass_mix * 255.0) as u64 & 0xFF) << 15;
        v |= ((self.highpass_mix * 255.0) as u64 & 0xFF) << 23;
        v |= ((self.fade_in * 255.0) as u64 & 0xFF) << 31;
        v |= ((self.fade_out * 255.0) as u64 & 0xFF) << 39;
        v |= ((self.pitch_cents as i8 as u8 as u64) & 0xFF) << 47;
        v
    }

    /// Unpack from a u64.
    pub fn unpack(v: u64) -> Self {
        StepEffects {
            reverse: v & (1 << 0) != 0,
            stutter: v & (1 << 1) != 0,
            distortion: v & (1 << 2) != 0,
            lowpass: v & (1 << 3) != 0,
            highpass: v & (1 << 4) != 0,
            half_speed: v & (1 << 5) != 0,
            double_speed: v & (1 << 6) != 0,
            distortion_mix: ((v >> 7) & 0xFF) as f32 / 255.0,
            lowpass_mix: ((v >> 15) & 0xFF) as f32 / 255.0,
            highpass_mix: ((v >> 23) & 0xFF) as f32 / 255.0,
            fade_in: ((v >> 31) & 0xFF) as f32 / 255.0,
            fade_out: ((v >> 39) & 0xFF) as f32 / 255.0,
            pitch_cents: ((v >> 47) & 0xFF) as u8 as i8 as i32,
        }
    }
}

/// Compute the per-step effect state for all beats.
/// The returned Vec has the same length as the flattened note sequence.
pub fn compute_effect_sequence(beats: &[String]) -> Vec<StepEffects> {
    let mut result = Vec::new();

    for raw in beats {
        let parsed = ParsedBeatLine::parse(raw);
        let step_count = parsed.step_count();
        let mut line_effects: Vec<StepEffects> = vec![StepEffects::default(); step_count];

        for cmd_seg in &parsed.commands {
            process_command_segment(cmd_seg, &mut line_effects);
        }

        result.extend(line_effects);
    }

    result
}

fn process_command_segment(cmd: &[char], effects: &mut [StepEffects]) {
    let len = cmd.len().min(effects.len());
    // Track the currently active toggle effect and where it started
    let mut active_toggle: Option<(char, usize)> = None; // (effect_char, start_pos)
    // Track positions consumed as fade-out endpoints (these should not restart the effect)
    let mut fade_out_endpoints: Vec<usize> = Vec::new();

    for i in 0..len {
        let c = cmd[i];

        match c {
            '~' => {
                effects[i].stutter = true;
            }
            '\\' => {
                // Fade out volume: ramp from 1.0 to 0.0 to end of line
                let remaining = len - i;
                if remaining > 1 {
                    for s in i..len {
                        let progress = (s - i) as f32 / (remaining - 1) as f32;
                        effects[s].fade_out = 1.0 - progress;
                    }
                } else {
                    effects[i].fade_out = 1.0;
                }
            }
            '/' => {
                // Fade in volume: ramp from 0 to 1 to end of line
                let remaining = len - i;
                if remaining > 1 {
                    for s in i..len {
                        let progress = (s - i) as f32 / (remaining - 1) as f32;
                        effects[s].fade_in = progress;
                    }
                } else {
                    effects[i].fade_in = 1.0;
                }
            }
            'R' => {
                effects[i].reverse = true;
            }
            'L' | 'H' | '*' => {
                // Skip if this position was consumed as a fade-out endpoint
                if fade_out_endpoints.contains(&i) {
                    continue;
                }
                // Turn on the effect at this position only
                set_effect(effects, c, i, true, 1.0);
                active_toggle = Some((c, i));
            }
            '-' => {
                // Continue the active toggle effect (if any)
                if let Some((toggle_char, _)) = active_toggle {
                    set_effect(effects, toggle_char, i, true, 1.0);
                }
            }
            '^' => {
                // Cut - deactivate the current toggle, prevent continuation
                active_toggle = None;
            }
            '<' => {
                // Fade IN the active toggle effect (0.0 -> 1.0)
                // Fade spans from start to endpoint (inclusive), endpoint has mix=1.0
                if let Some((toggle_char, start_pos)) = active_toggle {
                    let marker_pos = find_next_occurrence(cmd, toggle_char, i + 1).unwrap_or(len);
                    // Include endpoint in fade calculation
                    let fade_end = (marker_pos + 1).min(len);
                    let span = (fade_end - start_pos) as f32;
                    for s in start_pos..fade_end {
                        let mix = if span > 1.0 {
                            (s - start_pos) as f32 / (span - 1.0)
                        } else {
                            1.0
                        };
                        set_effect(effects, toggle_char, s, true, mix);
                    }
                    // After fade-in completes, the endpoint marker will restart at full mix
                    active_toggle = None;
                }
            }
            '>' => {
                // Fade OUT the active toggle effect (1.0 -> 0.0)
                // Fade spans from start to endpoint (inclusive), endpoint has mix=0
                if let Some((toggle_char, start_pos)) = active_toggle {
                    let marker_pos = find_next_occurrence(cmd, toggle_char, i + 1).unwrap_or(len);
                    // Include endpoint in fade calculation
                    let fade_end = (marker_pos + 1).min(len);
                    let span = (fade_end - start_pos) as f32;
                    for s in start_pos..fade_end {
                        let mix = if span > 1.0 {
                            1.0 - (s - start_pos) as f32 / (span - 1.0)
                        } else {
                            0.0
                        };
                        set_effect(effects, toggle_char, s, true, mix);
                    }
                    // Mark endpoint as consumed so it doesn't restart the effect
                    if marker_pos < len {
                        fade_out_endpoints.push(marker_pos);
                    }
                    active_toggle = None; // Fade completes, effect ends
                }
            }
            '(' => {
                // Half speed until matching ')'
                for s in i..len {
                    if s > i && cmd[s] == ')' {
                        break;
                    }
                    effects[s].half_speed = true;
                }
            }
            '[' => {
                // Double speed until matching ']'
                for s in i..len {
                    if s > i && cmd[s] == ']' {
                        break;
                    }
                    effects[s].double_speed = true;
                }
            }
            ')' | ']' => {
                // End markers handled by '(' and '['
            }
            _ => {
                // Check for pitch shift
                let cents = command_to_pitch_cents(c);
                if cents != 0 {
                    effects[i].pitch_cents = cents;
                }
            }
        }
    }
}

fn set_effect(effects: &mut [StepEffects], effect_char: char, pos: usize, on: bool, mix: f32) {
    if pos >= effects.len() {
        return;
    }
    match effect_char {
        'L' => {
            effects[pos].lowpass = on;
            effects[pos].lowpass_mix = mix;
        }
        'H' => {
            effects[pos].highpass = on;
            effects[pos].highpass_mix = mix;
        }
        '*' => {
            effects[pos].distortion = on;
            effects[pos].distortion_mix = mix;
        }
        _ => {}
    }
}

fn find_next_occurrence(cmd: &[char], target: char, from: usize) -> Option<usize> {
    for i in from..cmd.len() {
        if cmd[i] == target {
            return Some(i);
        }
    }
    None
}
