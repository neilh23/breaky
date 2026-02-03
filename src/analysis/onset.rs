use rustfft::{num_complex::Complex, FftPlanner};
use std::f32::consts::PI;

const FFT_SIZE: usize = 1024;
const HOP_SIZE: usize = 512;
const MIN_ONSET_INTERVAL_SECS: f32 = 0.05; // 50ms minimum between onsets
const THRESHOLD_MULTIPLIER: f32 = 1.5;
const THRESHOLD_OFFSET: f32 = 0.01;
const MEDIAN_WINDOW: usize = 11;

/// Detect onset times (in sample indices) using spectral flux.
pub fn detect_onsets(samples: &[f32], sample_rate: u32) -> Vec<usize> {
    let flux = spectral_flux(samples);
    if flux.is_empty() {
        return vec![0];
    }

    let threshold = adaptive_threshold(&flux);
    let min_interval_frames =
        (MIN_ONSET_INTERVAL_SECS * sample_rate as f32 / HOP_SIZE as f32).ceil() as usize;

    let mut onsets: Vec<usize> = Vec::new();

    for i in 1..flux.len() {
        if flux[i] > threshold[i] && flux[i] > flux[i - 1] {
            let sample_pos = i * HOP_SIZE;
            if let Some(&last) = onsets.last() {
                if i * HOP_SIZE - last < (min_interval_frames * HOP_SIZE) {
                    continue;
                }
            }
            onsets.push(sample_pos);
        }
    }

    // Always include sample 0 as the first onset if not already there
    if onsets.is_empty() || onsets[0] != 0 {
        onsets.insert(0, 0);
    }

    onsets
}

/// Compute spectral flux from audio samples.
fn spectral_flux(samples: &[f32]) -> Vec<f32> {
    let mut planner = FftPlanner::new();
    let fft = planner.plan_fft_forward(FFT_SIZE);

    let window = hann_window(FFT_SIZE);
    let num_frames = if samples.len() >= FFT_SIZE {
        (samples.len() - FFT_SIZE) / HOP_SIZE + 1
    } else {
        return Vec::new();
    };

    let mut magnitudes: Vec<Vec<f32>> = Vec::with_capacity(num_frames);
    let mut buffer = vec![Complex::new(0.0f32, 0.0f32); FFT_SIZE];

    for frame_idx in 0..num_frames {
        let start = frame_idx * HOP_SIZE;
        for i in 0..FFT_SIZE {
            let sample = if start + i < samples.len() {
                samples[start + i]
            } else {
                0.0
            };
            buffer[i] = Complex::new(sample * window[i], 0.0);
        }

        fft.process(&mut buffer);

        // Only use bins up to Nyquist
        let half = FFT_SIZE / 2 + 1;
        let mag: Vec<f32> = buffer[..half]
            .iter()
            .map(|c| (c.norm() + 1e-10).ln())
            .collect();
        magnitudes.push(mag);
    }

    // Half-wave rectified spectral flux
    let mut flux = vec![0.0f32; magnitudes.len()];
    for i in 1..magnitudes.len() {
        let mut sum = 0.0f32;
        for bin in 0..magnitudes[i].len() {
            let diff = magnitudes[i][bin] - magnitudes[i - 1][bin];
            if diff > 0.0 {
                sum += diff;
            }
        }
        flux[i] = sum;
    }

    flux
}

/// Adaptive threshold using running median * multiplier + offset.
fn adaptive_threshold(flux: &[f32]) -> Vec<f32> {
    let half = MEDIAN_WINDOW / 2;
    let mut threshold = vec![0.0f32; flux.len()];

    for i in 0..flux.len() {
        let start = i.saturating_sub(half);
        let end = (i + half + 1).min(flux.len());
        let mut local: Vec<f32> = flux[start..end].to_vec();
        local.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        let median = local[local.len() / 2];
        threshold[i] = median * THRESHOLD_MULTIPLIER + THRESHOLD_OFFSET;
    }

    threshold
}

/// Generate a Hann window of given size.
fn hann_window(size: usize) -> Vec<f32> {
    (0..size)
        .map(|i| 0.5 * (1.0 - (2.0 * PI * i as f32 / size as f32).cos()))
        .collect()
}
