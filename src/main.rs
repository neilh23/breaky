mod analysis;
mod audio;
mod config;
mod engine;
mod ui;

use std::io;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use clap::Parser;
use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use crossterm::ExecutableCommand;
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;

use analysis::onset::detect_onsets;
use analysis::slicer::make_slices;
use analysis::tempo::calculate_bpm;
use audio::loader::load_audio;
use audio::playback::start_playback;
use config::BreakConfig;
use engine::state::{PlayMode, PlaybackState};
use ui::app::App;
use ui::input::{is_shift, key_to_beat_char, key_to_command_char, key_to_slice};

#[derive(Parser)]
#[command(name = "breaky", about = "Console drum loop slicer/player")]
struct Cli {
    /// Path to an audio file (WAV, MP3, FLAC, OGG) or a .yaml config
    audio_file: PathBuf,
}

fn is_yaml_file(path: &PathBuf) -> bool {
    matches!(
        path.extension().and_then(|e| e.to_str()),
        Some("yaml" | "yml")
    )
}

fn recompute_sequence(beats: &[String]) -> Vec<Option<usize>> {
    beats
        .iter()
        .flat_map(|raw| {
            config::ParsedBeatLine::parse(raw)
                .notes
                .into_iter()
                .map(config::char_to_slice)
                .collect::<Vec<_>>()
        })
        .collect()
}

/// What the user wants to confirm with y/N.
#[derive(Clone)]
enum PendingAction {
    Quit,
    Reload,
}

fn main() -> Result<()> {
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = disable_raw_mode();
        let _ = io::stdout().execute(LeaveAlternateScreen);
        default_hook(info);
    }));

    let cli = Cli::parse();

    // Resolve the YAML path (used for :w / :e)
    let yaml_path: PathBuf = if is_yaml_file(&cli.audio_file) {
        cli.audio_file.clone()
    } else {
        cli.audio_file.with_extension("yaml")
    };

    // Determine input mode: YAML config or raw audio file
    let (mut audio_buf, break_config) = if is_yaml_file(&cli.audio_file) {
        let config = BreakConfig::load(&cli.audio_file)?;
        let yaml_dir = cli
            .audio_file
            .parent()
            .unwrap_or_else(|| std::path::Path::new("."));
        let audio_path = yaml_dir.join(&config.sample);
        let buf = load_audio(&audio_path)
            .with_context(|| format!("Failed to load sample '{}'", config.sample))?;
        (buf, config)
    } else {
        let buf = load_audio(&cli.audio_file).context("Failed to load audio")?;

        let onsets_pre = detect_onsets(&buf.samples, buf.sample_rate);
        let detected_bpm = calculate_bpm(&onsets_pre, buf.sample_rate);
        let config = BreakConfig::default_for(&buf.file_name, detected_bpm);

        if let Err(e) = config.save(&yaml_path) {
            eprintln!("Warning: could not write {}: {}", yaml_path.display(), e);
        }

        (buf, config)
    };

    let target_bpm = break_config.bpm;
    let sample_name = break_config.sample.clone();

    // Detect original BPM and resample if the config asks for a different tempo
    let onsets_orig = detect_onsets(&audio_buf.samples, audio_buf.sample_rate);
    let detected_bpm = calculate_bpm(&onsets_orig, audio_buf.sample_rate);

    if (detected_bpm - target_bpm).abs() > 0.5 {
        let ratio = detected_bpm / target_bpm;
        audio_buf.resample(ratio);
    }

    // Run full analysis on the (possibly resampled) audio
    let sample_rate = audio_buf.sample_rate;
    let file_name = audio_buf.file_name.clone();
    let duration_secs = audio_buf.duration_secs();
    let onsets = detect_onsets(&audio_buf.samples, sample_rate);
    let slices = make_slices(&onsets, &audio_buf.samples);
    let bpm = target_bpm;
    let num_slices = slices.len();

    // Compute stutter length: 1/16 of a beat in samples
    let beat_samples = (60.0 / bpm * sample_rate as f64) as u32;
    let stutter_len = beat_samples / 16;

    // Build the beat sequence and effect sequence from config
    let mut beat_sequence = recompute_sequence(&break_config.beats);
    let mut total_steps = beat_sequence.len();
    let mut effect_sequence =
        engine::effects::compute_effect_sequence(&break_config.beats);

    // Create shared playback state
    let state = PlaybackState::new();

    // Start audio output
    let _stream =
        start_playback(&audio_buf, &slices, state.clone()).context("Failed to start playback")?;

    // Setup terminal
    enable_raw_mode()?;
    io::stdout().execute(EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(io::stdout());
    let mut terminal = Terminal::new(backend)?;

    let mut app = App {
        file_name,
        sample_rate,
        duration_secs,
        bpm,
        num_slices,
        state: state.clone(),
        beats: break_config.beats.clone(),
        current_step: 0,
        total_steps,
        sequencer_playing: true,
        edit_mode: false,
        insert_mode: false,
        cursor_line: 0,
        cursor_col: 0,
        command_mode: false,
        command_buffer: String::new(),
        status_message: String::new(),
        confirm_prompt: String::new(),
        dirty: false,
    };

    // Sequencer timing: one full cycle = audio duration
    let step_duration = Duration::from_secs_f64(duration_secs / total_steps as f64);
    let mut current_step: usize = 0;
    let mut last_step = Instant::now();
    let mut sequencer_playing = true;

    // Edit state
    let mut edit_mode = false;
    let mut insert_mode = false;
    let mut cursor_line: usize = 0;
    let mut cursor_col: usize = 0;
    let mut paste_buffer: Option<String> = None;

    // Command / confirm state
    let mut command_mode = false;
    let mut command_buffer = String::new();
    let mut status_message = String::new();
    let mut status_time: Option<Instant> = None;
    let mut confirm_prompt = String::new();
    let mut pending_action: Option<PendingAction> = None;
    let mut dirty = false;

    let mut should_quit = false;

    // Main event loop
    loop {
        // Expire status message after 3 seconds
        if let Some(t) = status_time {
            if t.elapsed() > Duration::from_secs(3) {
                status_message.clear();
                status_time = None;
            }
        }

        // Sync display state
        app.current_step = current_step;
        app.sequencer_playing = sequencer_playing;
        app.total_steps = total_steps;
        app.edit_mode = edit_mode;
        app.insert_mode = insert_mode;
        app.cursor_line = cursor_line;
        app.cursor_col = cursor_col;
        app.command_mode = command_mode;
        app.command_buffer.clone_from(&command_buffer);
        app.status_message.clone_from(&status_message);
        app.confirm_prompt.clone_from(&confirm_prompt);
        app.dirty = dirty;

        terminal.draw(|frame| app.render(frame))?;

        if should_quit {
            break;
        }

        // Advance sequencer
        if sequencer_playing && total_steps > 0 && last_step.elapsed() >= step_duration {
            // Set effects for this step
            if current_step < effect_sequence.len() {
                state.set_effects(&effect_sequence[current_step]);
            }

            match beat_sequence[current_step] {
                Some(idx) if idx < num_slices => {
                    let slice = &slices[idx];
                    let effects = if current_step < effect_sequence.len() {
                        &effect_sequence[current_step]
                    } else {
                        &engine::effects::StepEffects::default()
                    };

                    if effects.reverse {
                        // Start from end of slice, audio callback reads backward
                        state.trigger_slice(
                            idx as u8,
                            (slice.end - 1) as u32,
                            PlayMode::FreeRun,
                        );
                    } else if effects.stutter {
                        state.trigger_slice(
                            idx as u8,
                            slice.start as u32,
                            PlayMode::FreeRun,
                        );
                        state.trigger_stutter(stutter_len);
                    } else {
                        state.trigger_slice(
                            idx as u8,
                            slice.start as u32,
                            PlayMode::FreeRun,
                        );
                    }
                }
                _ => {
                    state.stop();
                }
            }
            current_step = (current_step + 1) % total_steps;
            last_step += step_duration;
        }

        if event::poll(Duration::from_millis(1))? {
            if let Event::Key(key) = event::read()? {
                if key.kind != KeyEventKind::Press {
                    continue;
                }

                let is_ctrl = key.modifiers.contains(KeyModifiers::CONTROL);

                // Clear status on any keypress
                if status_time.is_some() {
                    status_message.clear();
                    status_time = None;
                }

                if command_mode {
                    // --- Command line input ---
                    match key.code {
                        KeyCode::Esc => {
                            command_mode = false;
                            command_buffer.clear();
                        }
                        KeyCode::Enter => {
                            command_mode = false;
                            let cmd = command_buffer.clone();
                            command_buffer.clear();
                            let beats_snapshot = app.beats.clone();
                            execute_command(
                                &cmd,
                                &yaml_path,
                                &sample_name,
                                bpm,
                                &beats_snapshot,
                                dirty,
                                &mut should_quit,
                                &mut status_message,
                                &mut status_time,
                                &mut confirm_prompt,
                                &mut pending_action,
                                &mut app.beats,
                                &mut beat_sequence,
                                &mut effect_sequence,
                                &mut total_steps,
                                &mut current_step,
                                &mut dirty,
                                &mut cursor_line,
                                &mut cursor_col,
                            );
                        }
                        KeyCode::Backspace => {
                            command_buffer.pop();
                        }
                        KeyCode::Char(c) => {
                            command_buffer.push(c);
                        }
                        _ => {}
                    }
                } else if pending_action.is_some() {
                    // --- Confirm y/N ---
                    let confirmed = matches!(key.code, KeyCode::Char('y' | 'Y'));
                    if confirmed {
                        match pending_action.clone().unwrap() {
                            PendingAction::Quit => {
                                should_quit = true;
                            }
                            PendingAction::Reload => {
                                do_reload(
                                    &yaml_path,
                                    &mut app.beats,
                                    &mut beat_sequence,
                                    &mut effect_sequence,
                                    &mut total_steps,
                                    &mut current_step,
                                    &mut dirty,
                                    &mut cursor_line,
                                    &mut cursor_col,
                                    &mut status_message,
                                    &mut status_time,
                                );
                            }
                        }
                    }
                    pending_action = None;
                    confirm_prompt.clear();
                } else if key.code == KeyCode::Char(':') && !is_ctrl {
                    // --- Enter command mode from any mode ---
                    command_mode = true;
                    command_buffer.clear();
                } else if edit_mode {
                    // --- Edit mode ---
                    handle_edit_key(
                        key.code,
                        is_ctrl,
                        &mut edit_mode,
                        &mut insert_mode,
                        &mut cursor_line,
                        &mut cursor_col,
                        &mut paste_buffer,
                        &mut app.beats,
                        &mut beat_sequence,
                        &mut effect_sequence,
                        &mut total_steps,
                        &mut current_step,
                        &slices,
                        num_slices,
                        &state,
                        stutter_len,
                        &mut sequencer_playing,
                        &mut last_step,
                        &mut dirty,
                    );
                } else {
                    // --- Normal mode ---
                    match key.code {
                        KeyCode::Esc => {
                            should_quit = true;
                        }
                        KeyCode::Char('c') if is_ctrl => {
                            should_quit = true;
                        }
                        KeyCode::Down => {
                            edit_mode = true;
                            cursor_line = 0;
                            cursor_col = 0;
                        }
                        KeyCode::Char(' ') => {
                            sequencer_playing = !sequencer_playing;
                            if sequencer_playing {
                                last_step = Instant::now();
                            }
                        }
                        KeyCode::Tab => {
                            if state.is_playing() {
                                state.trigger_stutter(stutter_len);
                            }
                        }
                        KeyCode::Char('n') if is_ctrl => {
                            app.beats.push("--------".to_string());
                            beat_sequence = recompute_sequence(&app.beats);
                            effect_sequence = engine::effects::compute_effect_sequence(&app.beats);
                            total_steps = beat_sequence.len();
                            dirty = true;
                        }
                        code => {
                            if let Some(idx) = key_to_slice(code) {
                                if idx < num_slices {
                                    let slice = &slices[idx];
                                    let mode = if is_shift(key.modifiers) {
                                        PlayMode::FreeRun
                                    } else {
                                        PlayMode::Slice
                                    };
                                    state.trigger_slice(idx as u8, slice.start as u32, mode);
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    // Cleanup terminal
    disable_raw_mode()?;
    io::stdout().execute(LeaveAlternateScreen)?;

    Ok(())
}

// ---------------------------------------------------------------------------
// Command execution
// ---------------------------------------------------------------------------

#[allow(clippy::too_many_arguments)]
fn execute_command(
    cmd: &str,
    yaml_path: &PathBuf,
    sample_name: &str,
    bpm: f64,
    beats_for_save: &[String],
    is_dirty: bool,
    should_quit: &mut bool,
    status_message: &mut String,
    status_time: &mut Option<Instant>,
    confirm_prompt: &mut String,
    pending_action: &mut Option<PendingAction>,
    beats: &mut Vec<String>,
    beat_sequence: &mut Vec<Option<usize>>,
    effect_sequence: &mut Vec<engine::effects::StepEffects>,
    total_steps: &mut usize,
    current_step: &mut usize,
    dirty: &mut bool,
    cursor_line: &mut usize,
    cursor_col: &mut usize,
) {
    match cmd.trim() {
        "w" => {
            do_save(
                yaml_path,
                sample_name,
                bpm,
                beats_for_save,
                dirty,
                status_message,
                status_time,
            );
        }
        "wq" => {
            do_save(
                yaml_path,
                sample_name,
                bpm,
                beats_for_save,
                dirty,
                status_message,
                status_time,
            );
            *should_quit = true;
        }
        "q!" => {
            *should_quit = true;
        }
        "q" => {
            if is_dirty {
                *confirm_prompt =
                    "Unsaved changes! Quit anyway? (y/N)".to_string();
                *pending_action = Some(PendingAction::Quit);
            } else {
                *should_quit = true;
            }
        }
        "e!" => {
            do_reload(
                yaml_path,
                beats,
                beat_sequence,
                effect_sequence,
                total_steps,
                current_step,
                dirty,
                cursor_line,
                cursor_col,
                status_message,
                status_time,
            );
        }
        "e" => {
            if is_dirty {
                *confirm_prompt =
                    "Unsaved changes will be lost! Reload? (y/N)".to_string();
                *pending_action = Some(PendingAction::Reload);
            } else {
                do_reload(
                    yaml_path,
                    beats,
                    beat_sequence,
                    effect_sequence,
                    total_steps,
                    current_step,
                    dirty,
                    cursor_line,
                    cursor_col,
                    status_message,
                    status_time,
                );
            }
        }
        other => {
            *status_message = format!("Unknown command: {}", other);
            *status_time = Some(Instant::now());
        }
    }
}

fn do_save(
    yaml_path: &PathBuf,
    sample_name: &str,
    bpm: f64,
    beats: &[String],
    dirty: &mut bool,
    status_message: &mut String,
    status_time: &mut Option<Instant>,
) {
    let config = BreakConfig {
        sample: sample_name.to_string(),
        bpm,
        beats: beats.to_vec(),
    };
    match config.save(yaml_path) {
        Ok(()) => {
            *dirty = false;
            *status_message = format!("Written: {}", yaml_path.display());
            *status_time = Some(Instant::now());
        }
        Err(e) => {
            *status_message = format!("Error writing: {}", e);
            *status_time = Some(Instant::now());
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn do_reload(
    yaml_path: &PathBuf,
    beats: &mut Vec<String>,
    beat_sequence: &mut Vec<Option<usize>>,
    effect_sequence: &mut Vec<engine::effects::StepEffects>,
    total_steps: &mut usize,
    current_step: &mut usize,
    dirty: &mut bool,
    cursor_line: &mut usize,
    cursor_col: &mut usize,
    status_message: &mut String,
    status_time: &mut Option<Instant>,
) {
    match BreakConfig::load(yaml_path) {
        Ok(config) => {
            *beats = config.beats;
            *beat_sequence = recompute_sequence(beats);
            *effect_sequence = engine::effects::compute_effect_sequence(beats);
            *total_steps = beat_sequence.len();
            if *total_steps > 0 && *current_step >= *total_steps {
                *current_step = 0;
            }
            *cursor_line = 0;
            *cursor_col = 0;
            *dirty = false;
            *status_message = format!("Reloaded: {}", yaml_path.display());
            *status_time = Some(Instant::now());
        }
        Err(e) => {
            *status_message = format!("Error reloading: {}", e);
            *status_time = Some(Instant::now());
        }
    }
}

// ---------------------------------------------------------------------------
// Edit mode key handling
// ---------------------------------------------------------------------------

#[allow(clippy::too_many_arguments)]
fn handle_edit_key(
    code: KeyCode,
    is_ctrl: bool,
    edit_mode: &mut bool,
    insert_mode: &mut bool,
    cursor_line: &mut usize,
    cursor_col: &mut usize,
    paste_buffer: &mut Option<String>,
    beats: &mut Vec<String>,
    beat_sequence: &mut Vec<Option<usize>>,
    effect_sequence: &mut Vec<engine::effects::StepEffects>,
    total_steps: &mut usize,
    current_step: &mut usize,
    slices: &[analysis::slicer::Slice],
    num_slices: usize,
    state: &PlaybackState,
    stutter_len: u32,
    sequencer_playing: &mut bool,
    last_step: &mut Instant,
    dirty: &mut bool,
) {
    use config::{visual_col_to_segment_step, ParsedBeatLine};

    match code {
        KeyCode::Esc => {
            *edit_mode = false;
            *insert_mode = false;
        }
        KeyCode::Up => {
            if *cursor_line == 0 {
                *edit_mode = false;
                *insert_mode = false;
            } else {
                *cursor_line -= 1;
                let parsed = ParsedBeatLine::parse(&beats[*cursor_line]);
                let max = parsed.display_width().saturating_sub(1);
                *cursor_col = (*cursor_col).min(max);
                snap_off_separator(&parsed, cursor_col);
            }
        }
        KeyCode::Down => {
            if *cursor_line + 1 < beats.len() {
                *cursor_line += 1;
                let parsed = ParsedBeatLine::parse(&beats[*cursor_line]);
                let max = parsed.display_width().saturating_sub(1);
                *cursor_col = (*cursor_col).min(max);
                snap_off_separator(&parsed, cursor_col);
            }
        }
        KeyCode::Left => {
            let parsed = ParsedBeatLine::parse(&beats[*cursor_line]);
            if *cursor_col > 0 {
                *cursor_col -= 1;
                // Skip ':' separator
                if visual_col_to_segment_step(&parsed, *cursor_col).is_none() && *cursor_col > 0
                {
                    *cursor_col -= 1;
                }
            } else if *cursor_line > 0 {
                *cursor_line -= 1;
                let prev_parsed = ParsedBeatLine::parse(&beats[*cursor_line]);
                *cursor_col = prev_parsed.display_width().saturating_sub(1);
            }
        }
        KeyCode::Right => {
            let parsed = ParsedBeatLine::parse(&beats[*cursor_line]);
            let max = parsed.display_width().saturating_sub(1);
            if *cursor_col < max {
                *cursor_col += 1;
                // Skip ':' separator
                if visual_col_to_segment_step(&parsed, *cursor_col).is_none()
                    && *cursor_col < max
                {
                    *cursor_col += 1;
                }
            } else if *cursor_line + 1 < beats.len() {
                *cursor_line += 1;
                *cursor_col = 0;
            }
        }
        KeyCode::Insert => {
            *insert_mode = !*insert_mode;
        }
        KeyCode::Char('c') if is_ctrl => {
            *paste_buffer = Some(beats[*cursor_line].clone());
        }
        KeyCode::Char('v') if is_ctrl => {
            if let Some(ref line) = paste_buffer {
                beats.insert(*cursor_line + 1, line.clone());
                *cursor_line += 1;
                *cursor_col = 0;
                *beat_sequence = recompute_sequence(beats);
                *effect_sequence = engine::effects::compute_effect_sequence(beats);
                *total_steps = beat_sequence.len();
                if *current_step >= *total_steps && *total_steps > 0 {
                    *current_step = 0;
                }
                *dirty = true;
            }
        }
        KeyCode::Char('n') if is_ctrl => {
            beats.insert(*cursor_line + 1, "--------".to_string());
            *cursor_line += 1;
            *cursor_col = 0;
            *beat_sequence = recompute_sequence(beats);
            *effect_sequence = engine::effects::compute_effect_sequence(beats);
            *total_steps = beat_sequence.len();
            if *current_step >= *total_steps && *total_steps > 0 {
                *current_step = 0;
            }
            *dirty = true;
        }
        // Ctrl-U : add a command sequence to the current beat line
        KeyCode::Char('u') if is_ctrl => {
            let mut parsed = ParsedBeatLine::parse(&beats[*cursor_line]);
            let empty: Vec<char> = vec!['-'; parsed.step_count()];
            parsed.commands.push(empty);
            beats[*cursor_line] = parsed.to_raw();
            *dirty = true;
        }
        // Ctrl-I : remove empty command sequences from the current beat line
        KeyCode::Char('i') if is_ctrl => {
            let mut parsed = ParsedBeatLine::parse(&beats[*cursor_line]);
            let all_dashes = |cmd: &Vec<char>| cmd.iter().all(|&c| c == '-');
            parsed.commands.retain(|cmd| !all_dashes(cmd));
            beats[*cursor_line] = parsed.to_raw();
            // Clamp cursor if it was in a removed segment
            let new_parsed = ParsedBeatLine::parse(&beats[*cursor_line]);
            let max = new_parsed.display_width().saturating_sub(1);
            if *cursor_col > max {
                *cursor_col = max;
            }
            snap_off_separator(&new_parsed, cursor_col);
            *dirty = true;
        }
        KeyCode::Char(' ') => {
            *sequencer_playing = !*sequencer_playing;
            if *sequencer_playing {
                *last_step = Instant::now();
            }
        }
        KeyCode::Tab => {
            if state.is_playing() {
                state.trigger_stutter(stutter_len);
            }
        }
        code => {
            let parsed = ParsedBeatLine::parse(&beats[*cursor_line]);
            if let Some((seg_idx, step_idx)) =
                visual_col_to_segment_step(&parsed, *cursor_col)
            {
                let mut did_edit = false;

                if seg_idx == 0 {
                    // Note segment: accept note characters
                    if let Some(ch) = key_to_beat_char(code) {
                        let mut new_parsed = parsed.clone();
                        new_parsed.notes[step_idx] = ch;
                        beats[*cursor_line] = new_parsed.to_raw();
                        *dirty = true;
                        did_edit = true;

                        // Preview the note
                        if let Some(idx) = config::char_to_slice(ch) {
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
                } else {
                    // Command segment: accept command characters
                    if let Some(ch) = key_to_command_char(code) {
                        let mut new_parsed = parsed.clone();
                        let cmd_idx = seg_idx - 1;
                        if cmd_idx < new_parsed.commands.len()
                            && step_idx < new_parsed.commands[cmd_idx].len()
                        {
                            new_parsed.commands[cmd_idx][step_idx] = ch;
                            beats[*cursor_line] = new_parsed.to_raw();
                            *dirty = true;
                            did_edit = true;
                        }
                    }
                }

                // Insert mode: advance cursor after edit
                if did_edit && *insert_mode {
                    let cur_parsed = ParsedBeatLine::parse(&beats[*cursor_line]);
                    let max = cur_parsed.display_width().saturating_sub(1);
                    if *cursor_col < max {
                        *cursor_col += 1;
                        if visual_col_to_segment_step(&cur_parsed, *cursor_col).is_none()
                            && *cursor_col < max
                        {
                            *cursor_col += 1;
                        }
                    } else if *cursor_line + 1 < beats.len() {
                        *cursor_line += 1;
                        *cursor_col = 0;
                    }
                }

                if did_edit {
                    *beat_sequence = recompute_sequence(beats);
                    *effect_sequence = engine::effects::compute_effect_sequence(beats);
                    *total_steps = beat_sequence.len();
                    if *current_step >= *total_steps && *total_steps > 0 {
                        *current_step = 0;
                    }
                }
            }
        }
    }
}

/// If cursor_col is on a ':' separator, nudge it to the next valid position.
fn snap_off_separator(parsed: &config::ParsedBeatLine, cursor_col: &mut usize) {
    use config::visual_col_to_segment_step;
    if visual_col_to_segment_step(parsed, *cursor_col).is_none() {
        let max = parsed.display_width().saturating_sub(1);
        if *cursor_col < max {
            *cursor_col += 1;
        } else if *cursor_col > 0 {
            *cursor_col -= 1;
        }
    }
}
