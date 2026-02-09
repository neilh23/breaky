use crossterm::event::KeyCode;

use crate::analysis::slicer::Slice;
use crate::config::{self, segment_step_to_visual_col, visual_col_to_segment_step, ParsedBeatLine};
use crate::engine::state::{PlayMode, PlaybackState};
use crate::sequencer::SequencerState;
use crate::ui::input::{key_to_beat_char, key_to_command_char};

pub struct EditorState {
    pub edit_mode: bool,
    pub insert_mode: bool,
    pub cursor_line: usize,
    pub cursor_col: usize,
    pub paste_buffer: Option<String>,
    pub current_bank: u8,
}

impl EditorState {
    pub fn new() -> Self {
        Self {
            edit_mode: false,
            insert_mode: false,
            cursor_line: 0,
            cursor_col: 0,
            paste_buffer: None,
            current_bank: 0,
        }
    }

    pub fn enter(&mut self) {
        self.edit_mode = true;
        self.cursor_line = 0;
        self.cursor_col = 0;
    }

    pub fn exit(&mut self) {
        self.edit_mode = false;
        self.insert_mode = false;
    }
}

pub fn handle_edit_key(
    code: KeyCode,
    is_ctrl: bool,
    editor: &mut EditorState,
    seq: &mut SequencerState,
    slices: &[Slice],
    num_slices: usize,
    state: &PlaybackState,
) {
    match code {
        KeyCode::Esc => {
            editor.exit();
        }
        KeyCode::Up => {
            if editor.cursor_line == 0 {
                editor.exit();
            } else {
                editor.cursor_line -= 1;
                let parsed = ParsedBeatLine::parse(&seq.beats[editor.cursor_line]);
                let max = parsed.display_width().saturating_sub(1);
                editor.cursor_col = editor.cursor_col.min(max);
                snap_off_separator(&parsed, &mut editor.cursor_col);
            }
        }
        KeyCode::Down => {
            if editor.cursor_line + 1 < seq.beats.len() {
                editor.cursor_line += 1;
                let parsed = ParsedBeatLine::parse(&seq.beats[editor.cursor_line]);
                let max = parsed.display_width().saturating_sub(1);
                editor.cursor_col = editor.cursor_col.min(max);
                snap_off_separator(&parsed, &mut editor.cursor_col);
            }
        }
        KeyCode::Left if is_ctrl => {
            let parsed = ParsedBeatLine::parse(&seq.beats[editor.cursor_line]);
            if let Some((seg_idx, _)) =
                visual_col_to_segment_step(&parsed, editor.cursor_col)
            {
                let seg_start = segment_step_to_visual_col(&parsed, seg_idx, 0);
                if editor.cursor_col > seg_start {
                    editor.cursor_col = seg_start;
                } else if seg_idx > 0 {
                    editor.cursor_col = segment_step_to_visual_col(&parsed, seg_idx - 1, 0);
                }
            }
        }
        KeyCode::Right if is_ctrl => {
            let parsed = ParsedBeatLine::parse(&seq.beats[editor.cursor_line]);
            if let Some((seg_idx, _)) =
                visual_col_to_segment_step(&parsed, editor.cursor_col)
            {
                let step_count = parsed.step_count();
                let seg_end = segment_step_to_visual_col(&parsed, seg_idx, step_count - 1);
                if editor.cursor_col < seg_end {
                    editor.cursor_col = seg_end;
                } else if seg_idx + 1 < parsed.segment_count() {
                    editor.cursor_col =
                        segment_step_to_visual_col(&parsed, seg_idx + 1, step_count - 1);
                }
            }
        }
        KeyCode::Left => {
            let parsed = ParsedBeatLine::parse(&seq.beats[editor.cursor_line]);
            if editor.cursor_col > 0 {
                editor.cursor_col -= 1;
                if visual_col_to_segment_step(&parsed, editor.cursor_col).is_none() && editor.cursor_col > 0
                {
                    editor.cursor_col -= 1;
                }
            } else if editor.cursor_line > 0 {
                editor.cursor_line -= 1;
                let prev_parsed = ParsedBeatLine::parse(&seq.beats[editor.cursor_line]);
                editor.cursor_col = prev_parsed.display_width().saturating_sub(1);
            }
        }
        KeyCode::Right => {
            let parsed = ParsedBeatLine::parse(&seq.beats[editor.cursor_line]);
            let max = parsed.display_width().saturating_sub(1);
            if editor.cursor_col < max {
                editor.cursor_col += 1;
                if visual_col_to_segment_step(&parsed, editor.cursor_col).is_none()
                    && editor.cursor_col < max
                {
                    editor.cursor_col += 1;
                }
            } else if editor.cursor_line + 1 < seq.beats.len() {
                editor.cursor_line += 1;
                editor.cursor_col = 0;
            }
        }
        KeyCode::Insert => {
            editor.insert_mode = !editor.insert_mode;
        }
        KeyCode::Char('c') if is_ctrl => {
            editor.paste_buffer = Some(seq.beats[editor.cursor_line].clone());
        }
        KeyCode::Char('v') if is_ctrl => {
            if let Some(ref line) = editor.paste_buffer {
                seq.beats.insert(editor.cursor_line + 1, line.clone());
                editor.cursor_line += 1;
                editor.cursor_col = 0;
                seq.rebuild_sequences();
                seq.dirty = true;
            }
        }
        KeyCode::Char('n') if is_ctrl => {
            seq.beats.insert(editor.cursor_line + 1, "--------".to_string());
            editor.cursor_line += 1;
            editor.cursor_col = 0;
            seq.rebuild_sequences();
            seq.dirty = true;
        }
        KeyCode::Char('u') if is_ctrl => {
            let mut parsed = ParsedBeatLine::parse(&seq.beats[editor.cursor_line]);
            let empty: Vec<char> = vec!['-'; parsed.step_count()];
            parsed.commands.push(empty);
            seq.beats[editor.cursor_line] = parsed.to_raw();
            seq.dirty = true;
        }
        KeyCode::Char('i') if is_ctrl => {
            let mut parsed = ParsedBeatLine::parse(&seq.beats[editor.cursor_line]);
            let all_dashes = |cmd: &Vec<char>| cmd.iter().all(|&c| c == '-');
            parsed.commands.retain(|cmd| !all_dashes(cmd));
            seq.beats[editor.cursor_line] = parsed.to_raw();
            let new_parsed = ParsedBeatLine::parse(&seq.beats[editor.cursor_line]);
            let max = new_parsed.display_width().saturating_sub(1);
            if editor.cursor_col > max {
                editor.cursor_col = max;
            }
            snap_off_separator(&new_parsed, &mut editor.cursor_col);
            seq.dirty = true;
        }
        KeyCode::Char(' ') => {
            seq.toggle(state);
        }
        KeyCode::Tab => {
            if state.is_playing() {
                state.trigger_stutter(seq.stutter_len);
            }
        }
        KeyCode::Char(c @ '0'..='9') => {
            editor.current_bank = c.to_digit(10).unwrap() as u8;
        }
        code => {
            let parsed = ParsedBeatLine::parse(&seq.beats[editor.cursor_line]);
            if let Some((seg_idx, step_idx)) =
                visual_col_to_segment_step(&parsed, editor.cursor_col)
            {
                let mut did_edit = false;

                if seg_idx == 0 {
                    if let Some(ch) = key_to_beat_char(code) {
                        let mut new_parsed = parsed.clone();
                        new_parsed.notes[step_idx] = ch;

                        if editor.current_bank != 0 {
                            if new_parsed.commands.is_empty() {
                                new_parsed.commands.push(vec!['-'; new_parsed.notes.len()]);
                            }
                            let bank_char = char::from_digit(editor.current_bank as u32, 10).unwrap_or('0');
                            new_parsed.commands[0][step_idx] = bank_char;
                        }

                        seq.beats[editor.cursor_line] = new_parsed.to_raw();
                        seq.dirty = true;
                        did_edit = true;

                        if let Some(base_idx) = config::char_to_slice(ch) {
                            let idx = base_idx + (editor.current_bank as usize * crate::analysis::slicer::SLICES_PER_BANK);
                            if idx < num_slices {
                                state.trigger_slice(
                                    idx as u8,
                                    slices[idx].start as u32,
                                    PlayMode::Slice,
                                );
                            }
                        } else {
                            state.stop();
                        }
                    }
                } else if let Some(ch) = key_to_command_char(code) {
                    let mut new_parsed = parsed.clone();
                    let cmd_idx = seg_idx - 1;
                    if cmd_idx < new_parsed.commands.len()
                        && step_idx < new_parsed.commands[cmd_idx].len()
                    {
                        new_parsed.commands[cmd_idx][step_idx] = ch;
                        seq.beats[editor.cursor_line] = new_parsed.to_raw();
                        seq.dirty = true;
                        did_edit = true;
                    }
                }

                if did_edit && editor.insert_mode {
                    let cur_parsed = ParsedBeatLine::parse(&seq.beats[editor.cursor_line]);
                    let max = cur_parsed.display_width().saturating_sub(1);
                    if editor.cursor_col < max {
                        editor.cursor_col += 1;
                        if visual_col_to_segment_step(&cur_parsed, editor.cursor_col).is_none()
                            && editor.cursor_col < max
                        {
                            editor.cursor_col += 1;
                        }
                    } else if editor.cursor_line + 1 < seq.beats.len() {
                        editor.cursor_line += 1;
                        editor.cursor_col = 0;
                    }
                }

                if did_edit {
                    seq.rebuild_sequences();
                }
            }
        }
    }
}

/// If cursor_col is on a ':' separator, nudge it to the next valid position.
pub fn snap_off_separator(parsed: &ParsedBeatLine, cursor_col: &mut usize) {
    if visual_col_to_segment_step(parsed, *cursor_col).is_none() {
        let max = parsed.display_width().saturating_sub(1);
        if *cursor_col < max {
            *cursor_col += 1;
        } else if *cursor_col > 0 {
            *cursor_col -= 1;
        }
    }
}
