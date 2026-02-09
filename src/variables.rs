use crate::config::BreakConfig;
use crate::engine::state::PlaybackState;

/// Configurable variables for the engine.
pub struct Variables {
    pub bpm: f64,
    pub lp: f32,
    pub hp: f32,
    pub dist: f32,
    pub fade: f32,
    pub slow: f64,
    pub fast: f64,
    pub stutter: u32,
}

impl Variables {
    pub fn from_config(config: &BreakConfig) -> Self {
        Self {
            bpm: config.bpm,
            lp: config.lp.unwrap_or(800.0),
            hp: config.hp.unwrap_or(2000.0),
            dist: config.dist.unwrap_or(0.2),
            fade: config.fade.unwrap_or(0.5),
            slow: config.slow.unwrap_or(0.5),
            fast: config.fast.unwrap_or(2.0),
            stutter: config.stutter.unwrap_or(16),
        }
    }

    pub fn sync_to_state(&self, state: &PlaybackState) {
        use std::sync::atomic::Ordering::Relaxed;
        state.lp_cutoff.store(self.lp.to_bits(), Relaxed);
        state.hp_cutoff.store(self.hp.to_bits(), Relaxed);
        state.dist_amount.store(self.dist.to_bits(), Relaxed);
        state.fade_point.store(self.fade.to_bits(), Relaxed);
        state.slow_ratio.store((self.slow as f32).to_bits(), Relaxed);
        state.fast_ratio.store((self.fast as f32).to_bits(), Relaxed);
    }

    pub fn apply_to_config(&self, config: &mut BreakConfig) {
        config.bpm = self.bpm;
        config.lp = if (self.lp - 800.0).abs() > f32::EPSILON { Some(self.lp) } else { None };
        config.hp = if (self.hp - 2000.0).abs() > f32::EPSILON { Some(self.hp) } else { None };
        config.dist = if (self.dist - 0.2).abs() > f32::EPSILON { Some(self.dist) } else { None };
        config.fade = if (self.fade - 0.5).abs() > f32::EPSILON { Some(self.fade) } else { None };
        config.slow = if (self.slow - 0.5).abs() > f64::EPSILON { Some(self.slow) } else { None };
        config.fast = if (self.fast - 2.0).abs() > f64::EPSILON { Some(self.fast) } else { None };
        config.stutter = if self.stutter != 16 { Some(self.stutter) } else { None };
    }
}

pub enum VarResult {
    Show(String),
    Set(String),
    Error(String),
}

pub fn set_var_f32(
    name: &str,
    value_str: Option<&str>,
    field: &mut f32,
    validate: impl Fn(f32) -> bool,
    range_msg: &str,
    suffix: &str,
) -> VarResult {
    match value_str {
        None => VarResult::Show(format!("{} = {}{}", name, field, suffix)),
        Some(s) => match s.parse::<f32>() {
            Ok(v) if validate(v) => {
                *field = v;
                VarResult::Set(format!("{} = {}{}", name, v, suffix))
            }
            Ok(_) => VarResult::Error(format!("{}: {}", name, range_msg)),
            Err(e) => VarResult::Error(format!("{}: {}", name, e)),
        },
    }
}

pub fn set_var_f64(
    name: &str,
    value_str: Option<&str>,
    field: &mut f64,
    validate: impl Fn(f64) -> bool,
    range_msg: &str,
) -> VarResult {
    match value_str {
        None => VarResult::Show(format!("{} = {}", name, field)),
        Some(s) => match s.parse::<f64>() {
            Ok(v) if validate(v) => {
                *field = v;
                VarResult::Set(format!("{} = {}", name, v))
            }
            Ok(_) => VarResult::Error(format!("{}: {}", name, range_msg)),
            Err(e) => VarResult::Error(format!("{}: {}", name, e)),
        },
    }
}

pub fn try_variable_command(cmd: &str, vars: &mut Variables) -> Option<VarResult> {
    let (name, value_str) = match cmd.find('=') {
        Some(pos) => (&cmd[..pos], Some(cmd[pos + 1..].trim())),
        None => (cmd.trim(), None),
    };

    let result = match name {
        "bpm" => set_var_f64("bpm", value_str, &mut vars.bpm, |v| v >= 1.0, "must be >= 1.0"),
        "lp" => set_var_f32("lp", value_str, &mut vars.lp, |v| v > 0.0, "must be > 0.0", " Hz"),
        "hp" => set_var_f32("hp", value_str, &mut vars.hp, |v| v > 0.0, "must be > 0.0", " Hz"),
        "dist" => set_var_f32("dist", value_str, &mut vars.dist, |v| (0.0..=1.0).contains(&v), "must be 0.0-1.0", ""),
        "fade" => set_var_f32("fade", value_str, &mut vars.fade, |v| v > 0.0 && v <= 1.0, "must be > 0.0 and <= 1.0", ""),
        "slow" => set_var_f64("slow", value_str, &mut vars.slow, |v| v > 0.0 && v < 1.0, "must be > 0.0 and < 1.0"),
        "fast" => set_var_f64("fast", value_str, &mut vars.fast, |v| (1.0..=10.0).contains(&v), "must be 1.0-10.0"),
        "stutter" => {
            return Some(match value_str {
                None => VarResult::Show(format!("stutter = {}", vars.stutter)),
                Some(s) => match s.parse::<u32>() {
                    Ok(v) if (2..=16).contains(&v) => {
                        vars.stutter = v;
                        VarResult::Set(format!("stutter = {}", v))
                    }
                    Ok(_) => VarResult::Error("stutter: must be 2-16".to_string()),
                    Err(e) => VarResult::Error(format!("stutter: {}", e)),
                },
            });
        }
        _ => return None,
    };

    Some(result)
}
