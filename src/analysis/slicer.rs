pub const MAX_SLICES: usize = 320; // 10 banks * 32 slices
pub const SLICES_PER_BANK: usize = 16;

/// Maximum snap distance as a fraction of the ideal slice length.
/// If the nearest onset is further than this, use the ideal position instead.
const SNAP_FRACTION: f64 = 0.4;

/// Maximum samples to search for a zero crossing.
const ZERO_CROSS_WINDOW: usize = 64;

/// A slice of audio defined by sample index range.
#[derive(Debug, Clone, Copy)]
pub struct Slice {
    pub start: usize,
    pub end: usize,
}

/// Build up to `num_banks` banks of slices of roughly equal length, snapping each
/// boundary to a nearby detected onset when one exists within tolerance,
/// then snapping to the nearest zero crossing to avoid clicks.
pub fn make_slices(onsets: &[usize], samples: &[f32], num_banks: usize) -> Vec<Slice> {
    let total_samples = samples.len();
    if total_samples == 0 {
        return Vec::new();
    }

    let target_count = SLICES_PER_BANK * num_banks;
    let ideal_len = total_samples as f64 / target_count as f64;
    let max_snap = (ideal_len * SNAP_FRACTION) as usize;

    let mut boundaries: Vec<usize> = Vec::with_capacity(target_count);

    for i in 0..target_count {
        let ideal_pos = (i as f64 * ideal_len).round() as usize;
        let snapped = if onsets.len() >= 2 {
            snap_to_nearest_onset(ideal_pos, onsets, max_snap)
        } else {
            ideal_pos
        };

        // Snap to nearest zero crossing to avoid clicks
        let zero_crossed = snap_to_zero_crossing(snapped, samples);

        // Only keep if strictly after the previous boundary
        if boundaries.last().map_or(true, |&last| zero_crossed > last) {
            boundaries.push(zero_crossed);
        }
    }

    // Always start from sample 0
    if boundaries.is_empty() || boundaries[0] != 0 {
        boundaries.insert(0, 0);
    }

    // Convert boundaries into slices
    let mut slices = Vec::with_capacity(boundaries.len());
    for i in 0..boundaries.len() {
        let start = boundaries[i];
        let end = if i + 1 < boundaries.len() {
            boundaries[i + 1]
        } else {
            total_samples
        };
        if start < end {
            slices.push(Slice { start, end });
        }
    }

    slices
}

/// Find the onset nearest to `pos`. If the closest onset is within
/// `max_distance`, return it; otherwise return `pos` unchanged.
fn snap_to_nearest_onset(pos: usize, onsets: &[usize], max_distance: usize) -> usize {
    let (closest, dist) = match onsets.binary_search(&pos) {
        Ok(_) => (pos, 0),
        Err(idx) => {
            let before = if idx > 0 {
                Some((onsets[idx - 1], pos - onsets[idx - 1]))
            } else {
                None
            };
            let after = if idx < onsets.len() {
                Some((onsets[idx], onsets[idx] - pos))
            } else {
                None
            };
            match (before, after) {
                (Some((bv, bd)), Some((av, ad))) => {
                    if bd <= ad {
                        (bv, bd)
                    } else {
                        (av, ad)
                    }
                }
                (Some((v, d)), None) => (v, d),
                (None, Some((v, d))) => (v, d),
                (None, None) => (pos, 0),
            }
        }
    };

    if dist <= max_distance {
        closest
    } else {
        pos
    }
}

/// Find the nearest zero crossing to `pos` within ZERO_CROSS_WINDOW samples.
/// A zero crossing is where the waveform crosses through zero (sign change).
/// Returns the position just after the crossing (where the new sign begins).
fn snap_to_zero_crossing(pos: usize, samples: &[f32]) -> usize {
    if pos == 0 || samples.is_empty() {
        return pos;
    }

    let len = samples.len();
    let mut best_pos = pos;
    let mut best_dist = usize::MAX;

    // Search backward from pos
    let start = pos.saturating_sub(ZERO_CROSS_WINDOW);
    for i in (start..pos).rev() {
        if i + 1 < len && is_zero_crossing(samples[i], samples[i + 1]) {
            let dist = pos - (i + 1);
            if dist < best_dist {
                best_dist = dist;
                best_pos = i + 1;
            }
            break;
        }
    }

    // Search forward from pos
    let end = (pos + ZERO_CROSS_WINDOW).min(len.saturating_sub(1));
    for i in pos..end {
        if i + 1 < len && is_zero_crossing(samples[i], samples[i + 1]) {
            let dist = (i + 1) - pos;
            if dist < best_dist {
                best_pos = i + 1;
            }
            break;
        }
    }

    best_pos
}

/// Check if two consecutive samples represent a zero crossing.
#[inline]
fn is_zero_crossing(a: f32, b: f32) -> bool {
    (a <= 0.0 && b > 0.0) || (a >= 0.0 && b < 0.0)
}
