/// Calculate BPM from onset positions.
/// Uses the median inter-onset interval for robustness.
pub fn calculate_bpm(onsets: &[usize], sample_rate: u32) -> f64 {
    if onsets.len() < 2 {
        return 120.0; // default fallback
    }

    let mut intervals: Vec<f64> = Vec::new();
    for i in 1..onsets.len() {
        let diff = onsets[i] as f64 - onsets[i - 1] as f64;
        if diff > 0.0 {
            intervals.push(diff);
        }
    }

    if intervals.is_empty() {
        return 120.0;
    }

    intervals.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let median_ioi = intervals[intervals.len() / 2];

    let secs_per_beat = median_ioi / sample_rate as f64;
    let mut bpm = 60.0 / secs_per_beat;

    // Clamp to 60-200 BPM range by halving or doubling
    while bpm > 200.0 {
        bpm /= 2.0;
    }
    while bpm < 60.0 {
        bpm *= 2.0;
    }

    bpm
}
