use std::path::Path;

use anyhow::Result;

/// Tracks what audio file is loaded in a bank range.
#[derive(Clone)]
pub struct BankEntry {
    pub file_name: String,
    pub start_slice: usize,
    pub slice_count: usize,
}

pub fn is_audio_file(path: &Path) -> bool {
    matches!(
        path.extension().and_then(|e| e.to_str()).map(|s| s.to_lowercase()).as_deref(),
        Some("wav" | "mp3" | "flac" | "ogg" | "aac" | "m4a")
    )
}

pub fn list_audio_files(dir: &str) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut files: Vec<String> = entries
        .filter_map(|e| e.ok())
        .filter(|e| e.path().is_file() && is_audio_file(&e.path()))
        .filter_map(|e| e.file_name().into_string().ok())
        .collect();
    files.sort();
    files
}

/// Load an audio file into the next available bank slots.
pub fn load_bank(
    path: &Path,
    target_sample_rate: u32,
    audio_buf: &mut crate::audio::buffer::AudioBuffer,
    slices: &mut Vec<crate::analysis::slicer::Slice>,
    bank_entries: &mut Vec<BankEntry>,
) -> Result<usize> {
    use crate::analysis::slicer::{make_slices, MAX_SLICES};

    // Check if there's room for more slices
    let current_slices = slices.len();
    if current_slices >= MAX_SLICES {
        anyhow::bail!("No free banks available (all {} slices used)", MAX_SLICES);
    }

    // Load the new audio file
    let mut new_buf = crate::audio::loader::load_audio(path)?;

    // Resample if sample rates don't match
    if new_buf.sample_rate != target_sample_rate {
        let ratio = new_buf.sample_rate as f64 / target_sample_rate as f64;
        new_buf.resample(ratio);
        new_buf.sample_rate = target_sample_rate;
    }

    // Detect onsets and create slices for the new audio
    let onsets = crate::analysis::onset::detect_onsets(&new_buf.samples, target_sample_rate);
    let new_slices = make_slices(&onsets, &new_buf.samples, 1);

    // Limit slices to available space
    let available_slots = MAX_SLICES - current_slices;
    let slices_to_add = new_slices.len().min(available_slots);

    if slices_to_add == 0 {
        anyhow::bail!("No slices could be created from the audio file");
    }

    // Offset for the new samples in the combined buffer
    let sample_offset = audio_buf.samples.len();

    // Add new slices with offset
    for i in 0..slices_to_add {
        let s = &new_slices[i];
        slices.push(crate::analysis::slicer::Slice {
            start: s.start + sample_offset,
            end: s.end + sample_offset,
        });
    }

    // Append new samples to the buffer
    audio_buf.samples.extend_from_slice(&new_buf.samples);

    // Track this bank entry
    bank_entries.push(BankEntry {
        file_name: new_buf.file_name,
        start_slice: current_slices,
        slice_count: slices_to_add,
    });

    // Update file_name to show multiple files
    if bank_entries.len() > 1 {
        audio_buf.file_name = format!("{} files", bank_entries.len());
    }

    Ok(slices_to_add)
}
