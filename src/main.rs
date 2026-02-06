mod analysis;
mod audio;
mod config;
mod engine;
mod ui;

use std::io;
use std::path::{Path, PathBuf};
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

/// Tracks what audio file is loaded in a bank range.
#[derive(Clone)]
struct BankEntry {
    file_name: String,
    start_slice: usize,
    slice_count: usize,
}

/// Configurable variables for the engine.
struct Variables {
    bpm: f64,
    lp: f32,
    hp: f32,
    dist: f32,
    fade: f32,
    slow: f64,
    fast: f64,
    stutter: u32,
}

impl Variables {
    fn defaults(bpm: f64) -> Self {
        Self {
            bpm,
            lp: 800.0,
            hp: 2000.0,
            dist: 0.2,
            fade: 0.5,
            slow: 0.5,
            fast: 2.0,
            stutter: 16,
        }
    }

    fn from_config(config: &BreakConfig) -> Self {
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

    fn sync_to_state(&self, state: &PlaybackState) {
        use std::sync::atomic::Ordering::Relaxed;
        state.lp_cutoff.store(self.lp.to_bits(), Relaxed);
        state.hp_cutoff.store(self.hp.to_bits(), Relaxed);
        state.dist_amount.store(self.dist.to_bits(), Relaxed);
        state.fade_point.store(self.fade.to_bits(), Relaxed);
        state.slow_ratio.store((self.slow as f32).to_bits(), Relaxed);
        state.fast_ratio.store((self.fast as f32).to_bits(), Relaxed);
    }

    fn apply_to_config(&self, config: &mut BreakConfig) {
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

enum VarResult {
    Show(String),
    Set(String),
    Error(String),
}

fn set_var_f32(
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

fn set_var_f64(
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

fn try_variable_command(cmd: &str, vars: &mut Variables) -> Option<VarResult> {
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
    let mut slices = make_slices(&onsets, &audio_buf.samples);
    let original_bpm = target_bpm;
    let mut bpm = target_bpm;
    let mut vars = Variables::from_config(&break_config);
    let mut num_slices = slices.len();

    // Track loaded files in banks
    let mut bank_entries: Vec<BankEntry> = vec![BankEntry {
        file_name: file_name.clone(),
        start_slice: 0,
        slice_count: num_slices,
    }];

    // Compute stutter length based on vars.stutter
    let beat_samples = (60.0 / bpm * sample_rate as f64) as u32;
    let mut stutter_len = beat_samples / vars.stutter;

    // Build the beat sequence and effect sequence from config
    let mut beat_sequence = recompute_sequence(&break_config.beats);
    let mut total_steps = beat_sequence.len();
    let mut effect_sequence =
        engine::effects::compute_effect_sequence(&break_config.beats);

    // Create shared playback state
    let state = PlaybackState::new();
    vars.sync_to_state(&state);

    // Start audio output (wrapped in Option to allow recreation)
    let mut _stream_opt: Option<cpal::Stream> =
        Some(start_playback(&audio_buf, &slices, state.clone()).context("Failed to start playback")?);

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
        current_bank: 0,
    };

    // Sequencer timing: base step duration at original BPM, scaled when BPM changes
    let base_step_secs = duration_secs / (total_steps as f64 * 2.0);
    let mut step_duration = Duration::from_secs_f64(base_step_secs);
    let mut current_step: usize = 0;
    let mut last_step = Instant::now();
    let mut sequencer_playing = true;

    // Edit state
    let mut edit_mode = false;
    let mut insert_mode = false;
    let mut cursor_line: usize = 0;
    let mut cursor_col: usize = 0;
    let mut paste_buffer: Option<String> = None;
    let mut current_bank: u8 = 0;

    // Command / confirm state
    let mut command_mode = false;
    let mut command_buffer = String::new();
    let mut command_history: Vec<String> = Vec::new();
    let mut history_index: Option<usize> = None;
    let mut status_message = String::new();
    let mut status_time: Option<Instant> = None;
    let mut confirm_prompt = String::new();
    let mut pending_action: Option<PendingAction> = None;
    let mut dirty = false;
    let mut bank_load_path: Option<PathBuf> = None;

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
        app.current_bank = current_bank;

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
                Some(base_idx) => {
                    let effects = if current_step < effect_sequence.len() {
                        &effect_sequence[current_step]
                    } else {
                        &engine::effects::StepEffects::default()
                    };

                    // Apply bank offset to slice index
                    let idx = base_idx + (effects.bank as usize * analysis::slicer::SLICES_PER_BANK);

                    if idx < num_slices {
                        let slice = &slices[idx];

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
                    } else {
                        state.stop();
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
                            history_index = None;
                        }
                        KeyCode::Enter => {
                            command_mode = false;
                            let cmd = command_buffer.clone();
                            command_buffer.clear();
                            // Add to history if non-empty and different from last
                            if !cmd.is_empty() {
                                if command_history.last() != Some(&cmd) {
                                    command_history.push(cmd.clone());
                                }
                            }
                            history_index = None;
                            let beats_snapshot = app.beats.clone();
                            execute_command(
                                &cmd,
                                &yaml_path,
                                &sample_name,
                                &mut vars,
                                &beats_snapshot,
                                dirty,
                                num_slices,
                                &bank_entries,
                                &mut bank_load_path,
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
                            // Sync variables to audio thread and update derived values
                            vars.sync_to_state(&state);
                            bpm = vars.bpm;
                            app.bpm = bpm;
                            step_duration = Duration::from_secs_f64(base_step_secs * (original_bpm / bpm));
                            let bs = (60.0 / bpm * sample_rate as f64) as u32;
                            stutter_len = bs / vars.stutter;

                            // Handle bank loading if requested
                            if let Some(load_path) = bank_load_path.take() {
                                match load_bank(
                                    &load_path,
                                    sample_rate,
                                    &mut audio_buf,
                                    &mut slices,
                                    &mut bank_entries,
                                ) {
                                    Ok(new_slice_count) => {
                                        num_slices = slices.len();
                                        app.num_slices = num_slices;
                                        // Recreate audio stream with updated buffer
                                        _stream_opt = None; // Drop old stream
                                        match start_playback(&audio_buf, &slices, state.clone()) {
                                            Ok(new_stream) => {
                                                _stream_opt = Some(new_stream);
                                                status_message = format!(
                                                    "Loaded {} ({} slices)",
                                                    load_path.display(),
                                                    new_slice_count
                                                );
                                            }
                                            Err(e) => {
                                                status_message = format!("Failed to restart audio: {}", e);
                                            }
                                        }
                                    }
                                    Err(e) => {
                                        status_message = format!("Failed to load: {}", e);
                                    }
                                }
                                status_time = Some(Instant::now());
                            }
                        }
                        KeyCode::Up => {
                            if !command_history.is_empty() {
                                history_index = Some(match history_index {
                                    None => command_history.len() - 1,
                                    Some(0) => 0,
                                    Some(i) => i - 1,
                                });
                                command_buffer = command_history[history_index.unwrap()].clone();
                            }
                        }
                        KeyCode::Down => {
                            if let Some(i) = history_index {
                                if i + 1 < command_history.len() {
                                    history_index = Some(i + 1);
                                    command_buffer = command_history[i + 1].clone();
                                } else {
                                    history_index = None;
                                    command_buffer.clear();
                                }
                            }
                        }
                        KeyCode::Backspace => {
                            command_buffer.pop();
                            history_index = None;
                        }
                        KeyCode::Char(c) => {
                            command_buffer.push(c);
                            history_index = None;
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
                                    &mut vars,
                                );
                                // Sync variables to audio thread and update derived values
                                vars.sync_to_state(&state);
                                bpm = vars.bpm;
                                app.bpm = bpm;
                                step_duration = Duration::from_secs_f64(base_step_secs * (original_bpm / bpm));
                                let bs = (60.0 / bpm * sample_rate as f64) as u32;
                                stutter_len = bs / vars.stutter;
                            }
                        }
                    }
                    pending_action = None;
                    confirm_prompt.clear();
                } else if key.code == KeyCode::Enter {
                    // --- Restart sequence from beginning ---
                    current_step = 0;
                    sequencer_playing = true;
                    last_step = Instant::now();
                } else if key.code == KeyCode::Char(':') && !is_ctrl {
                    // --- Enter command mode from any mode ---
                    command_mode = true;
                    command_buffer.clear();
                } else if matches!(key.code, KeyCode::Char('+') | KeyCode::Char('=')) {
                    // --- BPM increase (works in both play and edit mode) ---
                    bpm = bpm.floor() + 1.0;
                    vars.bpm = bpm;
                    step_duration = Duration::from_secs_f64(base_step_secs * (original_bpm / bpm));
                    app.bpm = bpm;
                    let bs = (60.0 / bpm * sample_rate as f64) as u32;
                    stutter_len = bs / vars.stutter;
                } else if key.code == KeyCode::Char('-') && !edit_mode {
                    // --- BPM decrease (play mode only; '-' is used in edit mode) ---
                    let new_bpm = bpm.floor() - if bpm == bpm.floor() { 1.0 } else { 0.0 };
                    if new_bpm >= 1.0 {
                        bpm = new_bpm;
                        vars.bpm = bpm;
                        step_duration = Duration::from_secs_f64(base_step_secs * (original_bpm / bpm));
                        app.bpm = bpm;
                        let bs = (60.0 / bpm * sample_rate as f64) as u32;
                        stutter_len = bs / vars.stutter;
                    }
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
                        &mut current_bank,
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
                            } else {
                                state.stop();
                            }
                        }
                        KeyCode::Tab => {
                            if state.is_playing() {
                                state.trigger_stutter(stutter_len);
                            }
                        }
                        KeyCode::Char('n') if is_ctrl => {
                            app.beats.push("----------------".to_string());
                            beat_sequence = recompute_sequence(&app.beats);
                            effect_sequence = engine::effects::compute_effect_sequence(&app.beats);
                            total_steps = beat_sequence.len();
                            dirty = true;
                        }
                        KeyCode::Char('0') => {
                            current_bank = 0;
                        }
                        KeyCode::Char('1') => {
                            current_bank = 1;
                        }
                        code => {
                            if let Some(base_idx) = key_to_slice(code) {
                                // Apply bank offset
                                let idx = base_idx + (current_bank as usize * analysis::slicer::SLICES_PER_BANK);
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
    vars: &mut Variables,
    beats_for_save: &[String],
    is_dirty: bool,
    num_slices: usize,
    bank_entries: &[BankEntry],
    bank_load_path: &mut Option<PathBuf>,
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
                vars,
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
                vars,
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
        "vars" => {
            *status_message = format!(
                "bpm={} lp={} hp={} dist={} fade={} slow={} fast={} stutter={}",
                vars.bpm, vars.lp, vars.hp, vars.dist, vars.fade, vars.slow, vars.fast, vars.stutter
            );
            *status_time = Some(Instant::now());
        }
        cmd if cmd == "bank" || cmd.starts_with("bank ") => {
            use analysis::slicer::{SLICES_PER_BANK, MAX_SLICES};
            let args = cmd.strip_prefix("bank").unwrap().trim();

            if args.is_empty() {
                // :bank - show bank info using bank_entries
                let mut msg = format!("{} slices total", num_slices);
                for entry in bank_entries {
                    let end_slice = entry.start_slice + entry.slice_count - 1;
                    let bank_num = entry.start_slice / SLICES_PER_BANK;
                    msg.push_str(&format!(
                        " | Bank {}: {} ({}-{})",
                        bank_num, entry.file_name, entry.start_slice, end_slice
                    ));
                }
                *status_message = msg;
                *status_time = Some(Instant::now());
            } else if let Some(path_str) = args.strip_prefix("load ") {
                // :bank load <path> - load file into next free bank
                let path_str = path_str.trim();
                if path_str.is_empty() {
                    *status_message = "Usage: :bank load <path>".to_string();
                    *status_time = Some(Instant::now());
                } else if num_slices >= MAX_SLICES {
                    *status_message = "No free banks available".to_string();
                    *status_time = Some(Instant::now());
                } else {
                    *bank_load_path = Some(PathBuf::from(path_str));
                    *status_message = format!("Loading {}...", path_str);
                    *status_time = Some(Instant::now());
                }
            } else {
                *status_message = format!("Unknown bank command: {}", args);
                *status_time = Some(Instant::now());
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
                vars,
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
                    vars,
                );
            }
        }
        other => {
            if let Some(result) = try_variable_command(other, vars) {
                let msg = match result {
                    VarResult::Show(m) | VarResult::Set(m) | VarResult::Error(m) => m,
                };
                *status_message = msg;
                *status_time = Some(Instant::now());
            } else {
                *status_message = format!("Unknown command: {}", other);
                *status_time = Some(Instant::now());
            }
        }
    }
}

/// Load an audio file into the next available bank slots.
fn load_bank(
    path: &Path,
    target_sample_rate: u32,
    audio_buf: &mut audio::buffer::AudioBuffer,
    slices: &mut Vec<analysis::slicer::Slice>,
    bank_entries: &mut Vec<BankEntry>,
) -> Result<usize> {
    use analysis::slicer::{make_slices, MAX_SLICES};

    // Check if there's room for more slices
    let current_slices = slices.len();
    if current_slices >= MAX_SLICES {
        anyhow::bail!("No free banks available (all {} slices used)", MAX_SLICES);
    }

    // Load the new audio file
    let mut new_buf = audio::loader::load_audio(path)?;

    // Resample if sample rates don't match
    if new_buf.sample_rate != target_sample_rate {
        let ratio = new_buf.sample_rate as f64 / target_sample_rate as f64;
        new_buf.resample(ratio);
        new_buf.sample_rate = target_sample_rate;
    }

    // Detect onsets and create slices for the new audio
    let onsets = analysis::onset::detect_onsets(&new_buf.samples, target_sample_rate);
    let new_slices = make_slices(&onsets, &new_buf.samples);

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
        slices.push(analysis::slicer::Slice {
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

fn do_save(
    yaml_path: &PathBuf,
    sample_name: &str,
    vars: &Variables,
    beats: &[String],
    dirty: &mut bool,
    status_message: &mut String,
    status_time: &mut Option<Instant>,
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
    vars: &mut Variables,
) {
    match BreakConfig::load(yaml_path) {
        Ok(config) => {
            *vars = Variables::from_config(&config);
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
    current_bank: &mut u8,
) {
    use config::{segment_step_to_visual_col, visual_col_to_segment_step, ParsedBeatLine};

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
        KeyCode::Left if is_ctrl => {
            // Ctrl-Left: jump to start of current segment, or previous segment
            let parsed = ParsedBeatLine::parse(&beats[*cursor_line]);
            if let Some((seg_idx, _)) =
                visual_col_to_segment_step(&parsed, *cursor_col)
            {
                let seg_start = segment_step_to_visual_col(&parsed, seg_idx, 0);
                if *cursor_col > seg_start {
                    // Not at start of segment: go to start
                    *cursor_col = seg_start;
                } else if seg_idx > 0 {
                    // At start of segment: go to start of previous segment
                    *cursor_col = segment_step_to_visual_col(&parsed, seg_idx - 1, 0);
                }
            }
        }
        KeyCode::Right if is_ctrl => {
            // Ctrl-Right: jump to end of current segment, or next segment
            let parsed = ParsedBeatLine::parse(&beats[*cursor_line]);
            if let Some((seg_idx, _)) =
                visual_col_to_segment_step(&parsed, *cursor_col)
            {
                let step_count = parsed.step_count();
                let seg_end = segment_step_to_visual_col(&parsed, seg_idx, step_count - 1);
                if *cursor_col < seg_end {
                    // Not at end of segment: go to end
                    *cursor_col = seg_end;
                } else if seg_idx + 1 < parsed.segment_count() {
                    // At end of segment: go to end of next segment
                    *cursor_col =
                        segment_step_to_visual_col(&parsed, seg_idx + 1, step_count - 1);
                }
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
            } else {
                state.stop();
            }
        }
        KeyCode::Tab => {
            if state.is_playing() {
                state.trigger_stutter(stutter_len);
            }
        }
        KeyCode::Char('0') => {
            *current_bank = 0;
        }
        KeyCode::Char('1') => {
            *current_bank = 1;
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

                        // If current bank is not 0, write bank to command sequence
                        if *current_bank != 0 {
                            // Ensure we have at least one command segment
                            if new_parsed.commands.is_empty() {
                                new_parsed.commands.push(vec!['-'; new_parsed.notes.len()]);
                            }
                            // Write bank number to first command segment
                            let bank_char = char::from_digit(*current_bank as u32, 10).unwrap_or('0');
                            new_parsed.commands[0][step_idx] = bank_char;
                        }

                        beats[*cursor_line] = new_parsed.to_raw();
                        *dirty = true;
                        did_edit = true;

                        // Preview the note (with bank offset)
                        if let Some(base_idx) = config::char_to_slice(ch) {
                            let idx = base_idx + (*current_bank as usize * analysis::slicer::SLICES_PER_BANK);
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
