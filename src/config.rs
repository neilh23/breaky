use std::path::Path;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize)]
pub struct BreakConfig {
    pub sample: String,
    pub bpm: f64,
    pub beats: Vec<String>,
}

impl BreakConfig {
    /// Create a default config that plays all 16 slices in order across two bars.
    pub fn default_for(file_name: &str, bpm: f64) -> Self {
        Self {
            sample: file_name.to_string(),
            bpm,
            beats: vec!["qwertyui".to_string(), "asdfghjk".to_string()],
        }
    }

    pub fn load(path: &Path) -> Result<Self> {
        let contents =
            std::fs::read_to_string(path).with_context(|| format!("Failed to read {}", path.display()))?;
        let config: Self =
            serde_yaml::from_str(&contents).with_context(|| format!("Failed to parse {}", path.display()))?;
        Ok(config)
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        let yaml = serde_yaml::to_string(self).context("Failed to serialize config")?;
        std::fs::write(path, yaml).with_context(|| format!("Failed to write {}", path.display()))?;
        Ok(())
    }
}

/// Map a beat character to a slice index (0-15), or None for silence.
pub fn char_to_slice(c: char) -> Option<usize> {
    match c {
        'q' => Some(0),
        'w' => Some(1),
        'e' => Some(2),
        'r' => Some(3),
        't' => Some(4),
        'y' => Some(5),
        'u' => Some(6),
        'i' => Some(7),
        'a' => Some(8),
        's' => Some(9),
        'd' => Some(10),
        'f' => Some(11),
        'g' => Some(12),
        'h' => Some(13),
        'j' => Some(14),
        'k' => Some(15),
        '_' => None,
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Parsed beat line with command sequences
// ---------------------------------------------------------------------------

/// A parsed beat line: note segment + zero or more command segments.
/// Raw format: "qwertyui:--*^----:--R----R"
#[derive(Debug, Clone)]
pub struct ParsedBeatLine {
    /// The note segment characters (e.g., "qwertyui").
    pub notes: Vec<char>,
    /// Zero or more command segments, each the same length as notes.
    pub commands: Vec<Vec<char>>,
}

impl ParsedBeatLine {
    /// Parse a raw beat string like "qwertyui:--*^----:--R----R".
    pub fn parse(raw: &str) -> Self {
        let mut segments = raw.split(':');
        let notes: Vec<char> = segments.next().unwrap_or("").chars().collect();
        let commands: Vec<Vec<char>> = segments.map(|s| s.chars().collect()).collect();
        ParsedBeatLine { notes, commands }
    }

    /// Serialize back to the colon-separated string form.
    pub fn to_raw(&self) -> String {
        let mut s: String = self.notes.iter().collect();
        for cmd_seg in &self.commands {
            s.push(':');
            s.extend(cmd_seg.iter());
        }
        s
    }

    /// Number of steps in this line (length of note segment).
    pub fn step_count(&self) -> usize {
        self.notes.len()
    }

    /// Total number of segments (1 note + N command).
    pub fn segment_count(&self) -> usize {
        1 + self.commands.len()
    }

    /// Total display width including ':' separators.
    pub fn display_width(&self) -> usize {
        let segs = self.segment_count();
        self.step_count() * segs + segs.saturating_sub(1)
    }
}

/// Given a visual column position in a parsed beat line,
/// return (segment_index, step_index) or None if on a ':' separator.
pub fn visual_col_to_segment_step(parsed: &ParsedBeatLine, col: usize) -> Option<(usize, usize)> {
    let step_count = parsed.step_count();
    if step_count == 0 {
        return None;
    }
    let stride = step_count + 1; // segment chars + one ':'
    let seg_idx = col / stride;
    let within = col % stride;

    if seg_idx >= parsed.segment_count() {
        return None;
    }
    if within >= step_count {
        return None; // on the ':' separator
    }
    Some((seg_idx, within))
}

/// Convert (segment_index, step_index) back to visual column.
pub fn segment_step_to_visual_col(parsed: &ParsedBeatLine, seg: usize, step: usize) -> usize {
    let stride = parsed.step_count() + 1;
    seg * stride + step
}

/// Check if a character is valid in a command segment.
pub fn is_valid_command_char(c: char) -> bool {
    matches!(
        c,
        '~' | '\\' | '/' | 'R' | 'L' | 'H' | '*'
            | '>' | '<' | '^'
            | '(' | ')' | '[' | ']'
            | 'q' | 'w' | 'e' | 'r' | 't' | 'y' | 'u' | 'i' | 'o' | 'p'
            | 'a' | 's' | 'd' | 'f' | 'g' | 'h' | 'j' | 'k' | 'l'
            | '-'
    )
}

/// Map a command-segment character to a pitch shift in cents.
/// Positive = up, negative = down. Returns 0 for non-pitch characters.
pub fn command_to_pitch_cents(c: char) -> i32 {
    match c {
        'q' => 1,
        'w' => 2,
        'e' => 3,
        'r' => 4,
        't' => 5,
        'y' => 6,
        'u' => 7,
        'i' => 8,
        'o' => 9,
        'p' => 10,
        'a' => -1,
        's' => -2,
        'd' => -3,
        'f' => -4,
        'g' => -5,
        'h' => -6,
        'j' => -7,
        'k' => -8,
        'l' => -9,
        _ => 0,
    }
}
