use std::path::PathBuf;
use std::time::Instant;

use crate::banks::{list_audio_files, BankEntry};
use crate::config::BreakConfig;
use crate::editor::EditorState;
use crate::sequencer::SequencerState;
use crate::variables::{try_variable_command, VarResult, Variables};

/// What the user wants to confirm with y/N.
#[derive(Clone)]
pub enum PendingAction {
    Quit,
    Reload,
}

pub struct CommandState {
    pub active: bool,
    pub buffer: String,
    pub history: Vec<String>,
    pub history_index: Option<usize>,
    pub status_message: String,
    pub status_time: Option<Instant>,
    pub confirm_prompt: String,
    pub pending_action: Option<PendingAction>,
}

impl CommandState {
    pub fn new() -> Self {
        Self {
            active: false,
            buffer: String::new(),
            history: Vec::new(),
            history_index: None,
            status_message: String::new(),
            status_time: None,
            confirm_prompt: String::new(),
            pending_action: None,
        }
    }

    pub fn enter(&mut self) {
        self.active = true;
        self.buffer.clear();
    }

    pub fn cancel(&mut self) {
        self.active = false;
        self.buffer.clear();
        self.history_index = None;
    }

    pub fn set_status(&mut self, msg: String) {
        self.status_message = msg;
        self.status_time = Some(Instant::now());
    }

    /// Expire status message after 3 seconds. Call each frame.
    pub fn expire_status(&mut self) {
        if let Some(t) = self.status_time {
            if t.elapsed() > std::time::Duration::from_secs(3) {
                self.status_message.clear();
                self.status_time = None;
            }
        }
    }

    /// Clear status on keypress.
    pub fn clear_status(&mut self) {
        if self.status_time.is_some() {
            self.status_message.clear();
            self.status_time = None;
        }
    }

    pub fn history_up(&mut self) {
        if !self.history.is_empty() {
            self.history_index = Some(match self.history_index {
                None => self.history.len() - 1,
                Some(0) => 0,
                Some(i) => i - 1,
            });
            self.buffer = self.history[self.history_index.unwrap()].clone();
        }
    }

    pub fn history_down(&mut self) {
        if let Some(i) = self.history_index {
            if i + 1 < self.history.len() {
                self.history_index = Some(i + 1);
                self.buffer = self.history[i + 1].clone();
            } else {
                self.history_index = None;
                self.buffer.clear();
            }
        }
    }

    pub fn submit(&mut self) -> String {
        self.active = false;
        let cmd = self.buffer.clone();
        self.buffer.clear();
        if !cmd.is_empty() {
            if self.history.last() != Some(&cmd) {
                self.history.push(cmd.clone());
            }
        }
        self.history_index = None;
        cmd
    }
}

pub fn execute_command(
    cmd: &str,
    yaml_path: &PathBuf,
    sample_name: &str,
    vars: &mut Variables,
    seq: &mut SequencerState,
    editor: &mut EditorState,
    cmd_state: &mut CommandState,
    bank_entries: &[BankEntry],
    bank_load_path: &mut Option<PathBuf>,
    should_quit: &mut bool,
) {
    let num_slices: usize = bank_entries.iter().map(|e| e.start_slice + e.slice_count).max().unwrap_or(0);

    match cmd.trim() {
        "w" => {
            do_save(yaml_path, sample_name, vars, &seq.beats, &mut seq.dirty, cmd_state);
        }
        "wq" => {
            do_save(yaml_path, sample_name, vars, &seq.beats, &mut seq.dirty, cmd_state);
            *should_quit = true;
        }
        "q!" => {
            *should_quit = true;
        }
        "q" => {
            if seq.dirty {
                cmd_state.confirm_prompt = "Unsaved changes! Quit anyway? (y/N)".to_string();
                cmd_state.pending_action = Some(PendingAction::Quit);
            } else {
                *should_quit = true;
            }
        }
        "vars" => {
            cmd_state.set_status(format!(
                "bpm={} lp={} hp={} dist={} fade={} slow={} fast={} stutter={}",
                vars.bpm, vars.lp, vars.hp, vars.dist, vars.fade, vars.slow, vars.fast, vars.stutter
            ));
        }
        cmd_str if cmd_str == "bank" || cmd_str.starts_with("bank ") => {
            use crate::analysis::slicer::{SLICES_PER_BANK, MAX_SLICES};
            let args = cmd_str.strip_prefix("bank").unwrap().trim();

            if args.is_empty() {
                let mut msg = format!("{} slices total", num_slices);
                for entry in bank_entries {
                    let end_slice = entry.start_slice + entry.slice_count - 1;
                    let bank_num = entry.start_slice / SLICES_PER_BANK;
                    msg.push_str(&format!(
                        " | Bank {}: {} ({}-{})",
                        bank_num, entry.file_name, entry.start_slice, end_slice
                    ));
                }
                cmd_state.set_status(msg);
            } else if args == "load" || args.starts_with("load ") {
                if num_slices >= MAX_SLICES {
                    cmd_state.set_status("No free banks available".to_string());
                } else {
                    let path_str = args.strip_prefix("load").unwrap().trim();
                    if path_str.is_empty() {
                        let audio_files = list_audio_files(".");
                        if audio_files.is_empty() {
                            cmd_state.set_status("No audio files found. Use :bank load <path>".to_string());
                        } else {
                            cmd_state.set_status(format!("Files: {}. Use :bank load <file>", audio_files.join(", ")));
                        }
                    } else {
                        *bank_load_path = Some(PathBuf::from(path_str));
                        cmd_state.set_status(format!("Loading {}...", path_str));
                    }
                }
            } else {
                cmd_state.set_status(format!("Unknown bank command: {}", args));
            }
        }
        "e!" => {
            do_reload(yaml_path, seq, editor, cmd_state, vars);
        }
        "e" => {
            if seq.dirty {
                cmd_state.confirm_prompt = "Unsaved changes will be lost! Reload? (y/N)".to_string();
                cmd_state.pending_action = Some(PendingAction::Reload);
            } else {
                do_reload(yaml_path, seq, editor, cmd_state, vars);
            }
        }
        other => {
            if let Some(result) = try_variable_command(other, vars) {
                let msg = match result {
                    VarResult::Show(m) | VarResult::Set(m) | VarResult::Error(m) => m,
                };
                cmd_state.set_status(msg);
            } else {
                cmd_state.set_status(format!("Unknown command: {}", other));
            }
        }
    }
}

pub fn do_save(
    yaml_path: &PathBuf,
    sample_name: &str,
    vars: &Variables,
    beats: &[String],
    dirty: &mut bool,
    cmd_state: &mut CommandState,
) {
    let mut config = BreakConfig {
        sample: sample_name.to_string(),
        bpm: vars.bpm,
        beats: beats.to_vec(),
        lp: None,
        hp: None,
        dist: None,
        fade: None,
        slow: None,
        fast: None,
        stutter: None,
    };
    vars.apply_to_config(&mut config);
    match config.save(yaml_path) {
        Ok(()) => {
            *dirty = false;
            cmd_state.set_status(format!("Written: {}", yaml_path.display()));
        }
        Err(e) => {
            cmd_state.set_status(format!("Error writing: {}", e));
        }
    }
}

pub fn do_reload(
    yaml_path: &PathBuf,
    seq: &mut SequencerState,
    editor: &mut EditorState,
    cmd_state: &mut CommandState,
    vars: &mut Variables,
) {
    match BreakConfig::load(yaml_path) {
        Ok(config) => {
            *vars = Variables::from_config(&config);
            seq.beats = config.beats;
            seq.rebuild_sequences();
            editor.cursor_line = 0;
            editor.cursor_col = 0;
            seq.dirty = false;
            cmd_state.set_status(format!("Reloaded: {}", yaml_path.display()));
        }
        Err(e) => {
            cmd_state.set_status(format!("Error reloading: {}", e));
        }
    }
}
