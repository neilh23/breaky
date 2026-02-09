mod analysis;
mod audio;
mod banks;
mod commands;
mod config;
mod editor;
mod engine;
mod sequencer;
mod ui;
mod variables;

use std::io;
use std::path::PathBuf;
use std::time::Duration;

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
use banks::BankEntry;
use commands::PendingAction;
use config::BreakConfig;
use editor::EditorState;
use engine::state::{PlayMode, PlaybackState};
use sequencer::SequencerState;
use ui::app::App;
use ui::input::{is_shift, key_to_slice};
use variables::Variables;

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

fn main() -> Result<()> {
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = disable_raw_mode();
        let _ = io::stdout().execute(LeaveAlternateScreen);
        default_hook(info);
    }));

    let cli = Cli::parse();

    let yaml_path: PathBuf = if is_yaml_file(&cli.audio_file) {
        cli.audio_file.clone()
    } else {
        cli.audio_file.with_extension("yaml")
    };

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

    let onsets_orig = detect_onsets(&audio_buf.samples, audio_buf.sample_rate);
    let detected_bpm = calculate_bpm(&onsets_orig, audio_buf.sample_rate);

    if (detected_bpm - target_bpm).abs() > 0.5 {
        let ratio = detected_bpm / target_bpm;
        audio_buf.resample(ratio);
    }

    let sample_rate = audio_buf.sample_rate;
    let file_name = audio_buf.file_name.clone();
    let duration_secs = audio_buf.duration_secs();
    let onsets = detect_onsets(&audio_buf.samples, sample_rate);
    let mut slices = make_slices(&onsets, &audio_buf.samples, 2);
    let mut num_slices = slices.len();
    let mut vars = Variables::from_config(&break_config);

    let mut bank_entries: Vec<BankEntry> = vec![BankEntry {
        file_name: file_name.clone(),
        start_slice: 0,
        slice_count: num_slices,
    }];

    let mut seq = SequencerState::new(
        break_config.beats.clone(),
        target_bpm,
        duration_secs,
        sample_rate,
        vars.stutter,
    );

    let state = PlaybackState::new();
    vars.sync_to_state(&state);

    let mut _stream_opt: Option<cpal::Stream> =
        Some(start_playback(&audio_buf, &slices, state.clone()).context("Failed to start playback")?);

    enable_raw_mode()?;
    io::stdout().execute(EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(io::stdout());
    let mut terminal = Terminal::new(backend)?;

    let mut app = App {
        file_name,
        sample_rate,
        duration_secs,
        bpm: target_bpm,
        num_slices,
        state: state.clone(),
    };

    let mut editor = EditorState::new();
    let mut cmd_state = commands::CommandState::new();
    let mut bank_load_path: Option<PathBuf> = None;
    let mut should_quit = false;

    // Main event loop
    loop {
        cmd_state.expire_status();

        terminal.draw(|frame| app.render(frame, &seq, &editor, &cmd_state))?;

        if should_quit {
            break;
        }

        seq.advance_step(&state, &slices, num_slices);

        if event::poll(Duration::from_millis(1))? {
            if let Event::Key(key) = event::read()? {
                if key.kind != KeyEventKind::Press {
                    continue;
                }

                let is_ctrl = key.modifiers.contains(KeyModifiers::CONTROL);

                cmd_state.clear_status();

                if cmd_state.active {
                    match key.code {
                        KeyCode::Esc => cmd_state.cancel(),
                        KeyCode::Enter => {
                            let cmd = cmd_state.submit();
                            commands::execute_command(
                                &cmd,
                                &yaml_path,
                                &sample_name,
                                &mut vars,
                                &mut seq,
                                &mut editor,
                                &mut cmd_state,
                                &bank_entries,
                                &mut bank_load_path,
                                &mut should_quit,
                            );
                            vars.sync_to_state(&state);
                            app.bpm = vars.bpm;
                            seq.update_timing(vars.bpm, sample_rate, vars.stutter);

                            if let Some(load_path) = bank_load_path.take() {
                                if load_path.as_os_str().is_empty() {
                                    continue;
                                }
                                match banks::load_bank(
                                    &load_path,
                                    sample_rate,
                                    &mut audio_buf,
                                    &mut slices,
                                    &mut bank_entries,
                                ) {
                                    Ok(new_slice_count) => {
                                        num_slices = slices.len();
                                        app.num_slices = num_slices;
                                        _stream_opt = None;
                                        match start_playback(&audio_buf, &slices, state.clone()) {
                                            Ok(new_stream) => {
                                                _stream_opt = Some(new_stream);
                                                cmd_state.set_status(format!(
                                                    "Loaded {} ({} slices)",
                                                    load_path.display(),
                                                    new_slice_count
                                                ));
                                            }
                                            Err(e) => {
                                                cmd_state.set_status(format!("Failed to restart audio: {}", e));
                                            }
                                        }
                                    }
                                    Err(e) => {
                                        cmd_state.set_status(format!("Failed to load: {}", e));
                                    }
                                }
                            }
                        }
                        KeyCode::Up => cmd_state.history_up(),
                        KeyCode::Down => cmd_state.history_down(),
                        KeyCode::Backspace => {
                            cmd_state.buffer.pop();
                            cmd_state.history_index = None;
                        }
                        KeyCode::Char(c) => {
                            cmd_state.buffer.push(c);
                            cmd_state.history_index = None;
                        }
                        _ => {}
                    }
                } else if cmd_state.pending_action.is_some() {
                    let confirmed = matches!(key.code, KeyCode::Char('y' | 'Y'));
                    if confirmed {
                        match cmd_state.pending_action.clone().unwrap() {
                            PendingAction::Quit => {
                                should_quit = true;
                            }
                            PendingAction::Reload => {
                                commands::do_reload(
                                    &yaml_path,
                                    &mut seq,
                                    &mut editor,
                                    &mut cmd_state,
                                    &mut vars,
                                );
                                vars.sync_to_state(&state);
                                app.bpm = vars.bpm;
                                seq.update_timing(vars.bpm, sample_rate, vars.stutter);
                            }
                        }
                    }
                    cmd_state.pending_action = None;
                    cmd_state.confirm_prompt.clear();
                } else if key.code == KeyCode::Enter {
                    if editor.edit_mode {
                        seq.play_line(editor.cursor_line);
                    } else {
                        seq.restart();
                    }
                } else if key.code == KeyCode::Char(':') && !is_ctrl {
                    cmd_state.enter();
                } else if matches!(key.code, KeyCode::Char('+') | KeyCode::Char('=')) {
                    vars.bpm = vars.bpm.floor() + 1.0;
                    app.bpm = vars.bpm;
                    seq.update_timing(vars.bpm, sample_rate, vars.stutter);
                } else if key.code == KeyCode::Char('-') && !editor.edit_mode {
                    let new_bpm = vars.bpm.floor() - if vars.bpm == vars.bpm.floor() { 1.0 } else { 0.0 };
                    if new_bpm >= 1.0 {
                        vars.bpm = new_bpm;
                        app.bpm = vars.bpm;
                        seq.update_timing(vars.bpm, sample_rate, vars.stutter);
                    }
                } else if editor.edit_mode {
                    editor::handle_edit_key(
                        key.code,
                        is_ctrl,
                        &mut editor,
                        &mut seq,
                        &slices,
                        num_slices,
                        &state,
                    );
                } else {
                    match key.code {
                        KeyCode::Esc => {
                            should_quit = true;
                        }
                        KeyCode::Char('c') if is_ctrl => {
                            should_quit = true;
                        }
                        KeyCode::Down => {
                            editor.enter();
                        }
                        KeyCode::Char(' ') => {
                            seq.toggle(&state);
                        }
                        KeyCode::Tab => {
                            if state.is_playing() {
                                state.trigger_stutter(seq.stutter_len);
                            }
                        }
                        KeyCode::Char('n') if is_ctrl => {
                            seq.beats.push("----------------".to_string());
                            seq.rebuild_sequences();
                            seq.dirty = true;
                        }
                        KeyCode::Char(c @ '0'..='9') => {
                            editor.current_bank = c.to_digit(10).unwrap() as u8;
                        }
                        code => {
                            if let Some(base_idx) = key_to_slice(code) {
                                let idx = base_idx + (editor.current_bank as usize * analysis::slicer::SLICES_PER_BANK);
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

    disable_raw_mode()?;
    io::stdout().execute(LeaveAlternateScreen)?;

    Ok(())
}
