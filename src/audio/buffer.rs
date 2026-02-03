/// Mono audio buffer with metadata.
pub struct AudioBuffer {
    /// Mono f32 samples, normalized to [-1.0, 1.0].
    pub samples: Vec<f32>,
    /// Sample rate in Hz.
    pub sample_rate: u32,
    /// Original file name.
    pub file_name: String,
}

impl AudioBuffer {
    pub fn duration_secs(&self) -> f64 {
        self.samples.len() as f64 / self.sample_rate as f64
    }

    /// Resample in-place by `ratio` using linear interpolation.
    /// ratio > 1.0 produces more samples (slower playback).
    /// ratio < 1.0 produces fewer samples (faster playback).
    pub fn resample(&mut self, ratio: f64) {
        if (ratio - 1.0).abs() < 0.001 || self.samples.is_empty() {
            return;
        }
        let src = &self.samples;
        let new_len = (src.len() as f64 * ratio).round() as usize;
        let mut out = Vec::with_capacity(new_len);
        for i in 0..new_len {
            let src_pos = i as f64 / ratio;
            let idx = src_pos as usize;
            let frac = (src_pos - idx as f64) as f32;
            let s0 = src[idx.min(src.len() - 1)];
            let s1 = src[(idx + 1).min(src.len() - 1)];
            out.push(s0 + (s1 - s0) * frac);
        }
        self.samples = out;
    }
}
