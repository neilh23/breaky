use std::sync::Arc;

use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};
use ratatui::Frame;

use crate::config::{segment_step_to_visual_col, visual_col_to_segment_step, ParsedBeatLine};
use crate::engine::state::PlaybackState;

use super::input::KEY_LABELS;

pub struct App {
    pub file_name: String,
    pub sample_rate: u32,
    pub duration_secs: f64,
    pub bpm: f64,
    pub num_slices: usize,
    pub state: Arc<PlaybackState>,
    pub beats: Vec<String>,
    pub current_step: usize,
    pub total_steps: usize,
    pub sequencer_playing: bool,
    pub edit_mode: bool,
    pub insert_mode: bool,
    pub cursor_line: usize,
    pub cursor_col: usize,
    pub command_mode: bool,
    pub command_buffer: String,
    pub status_message: String,
    pub confirm_prompt: String,
    pub dirty: bool,
    pub current_bank: u8,
}

impl App {
    pub fn render(&self, frame: &mut Frame) {
        let area = frame.area();

        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(3),                            // header
                Constraint::Length(3),                            // BPM info
                Constraint::Length(4),                            // key grid
                Constraint::Length(2 + self.beats.len() as u16), // sequencer
                Constraint::Length(4),                            // footer
            ])
            .split(area);

        self.render_header(frame, chunks[0]);
        self.render_info(frame, chunks[1]);
        self.render_grid(frame, chunks[2]);
        self.render_sequencer(frame, chunks[3]);
        self.render_footer(frame, chunks[4]);
    }

    fn render_header(&self, frame: &mut Frame, area: Rect) {
        let dirty_mark = if self.dirty { " [+]" } else { "" };
        let text = format!(
            "  breaky - {}{}  |  {} Hz  |  {:.1}s",
            self.file_name, dirty_mark, self.sample_rate, self.duration_secs
        );
        let block = Block::default()
            .borders(Borders::ALL)
            .title(" breaky ")
            .style(Style::default().fg(Color::Cyan));
        let paragraph = Paragraph::new(text).block(block);
        frame.render_widget(paragraph, area);
    }

    fn render_info(&self, frame: &mut Frame, area: Rect) {
        let mode_str = if self.state.is_playing() {
            match self.state.get_mode() {
                crate::engine::state::PlayMode::Slice => "SLICE",
                crate::engine::state::PlayMode::FreeRun => "FREE RUN",
                crate::engine::state::PlayMode::Stutter => "STUTTER",
            }
        } else {
            "STOPPED"
        };

        let text = format!(
            "  BPM: {:.1}    Beats: {}    Bank: {}    Mode: {}",
            self.bpm, self.num_slices, self.current_bank, mode_str
        );
        let block = Block::default().borders(Borders::ALL);
        let paragraph = Paragraph::new(text).block(block);
        frame.render_widget(paragraph, area);
    }

    fn render_grid(&self, frame: &mut Frame, area: Rect) {
        let active = if self.state.is_playing() {
            Some(self.state.get_active_slice() as usize)
        } else {
            None
        };

        let row1: Vec<Span> = (0..8)
            .map(|i| {
                let label = if i < KEY_LABELS.len() {
                    KEY_LABELS[i]
                } else {
                    "   "
                };
                let style = if Some(i) == active && i < self.num_slices {
                    Style::default()
                        .fg(Color::Black)
                        .bg(Color::Yellow)
                        .add_modifier(Modifier::BOLD)
                } else if i < self.num_slices {
                    Style::default().fg(Color::Green)
                } else {
                    Style::default().fg(Color::DarkGray)
                };
                Span::styled(format!("  {:>5} ", label), style)
            })
            .collect();

        let row2: Vec<Span> = (8..16)
            .map(|i| {
                let label = if i < KEY_LABELS.len() {
                    KEY_LABELS[i]
                } else {
                    "   "
                };
                let style = if Some(i) == active && i < self.num_slices {
                    Style::default()
                        .fg(Color::Black)
                        .bg(Color::Yellow)
                        .add_modifier(Modifier::BOLD)
                } else if i < self.num_slices {
                    Style::default().fg(Color::Green)
                } else {
                    Style::default().fg(Color::DarkGray)
                };
                Span::styled(format!("  {:>5} ", label), style)
            })
            .collect();

        let block = Block::default().borders(Borders::ALL);
        let paragraph = Paragraph::new(vec![Line::from(row1), Line::from(row2)]).block(block);
        frame.render_widget(paragraph, area);
    }

    fn render_sequencer(&self, frame: &mut Frame, area: Rect) {
        let play_icon = if self.sequencer_playing {
            "\u{25b6}" // ▶
        } else {
            "\u{23f8}" // ⏸
        };

        let mut offset = 0usize;
        let mut lines: Vec<Line> = Vec::new();

        // Determine the cursor's step_idx for column highlighting
        let cursor_step_idx = if self.edit_mode {
            let cursor_parsed = if self.cursor_line < self.beats.len() {
                ParsedBeatLine::parse(&self.beats[self.cursor_line])
            } else {
                ParsedBeatLine::parse("")
            };
            visual_col_to_segment_step(&cursor_parsed, self.cursor_col)
                .map(|(_, step)| step)
        } else {
            None
        };

        for (line_idx, beat_raw) in self.beats.iter().enumerate() {
            let parsed = ParsedBeatLine::parse(beat_raw);
            let mut spans: Vec<Span> = Vec::new();
            let is_cursor_line = self.edit_mode && line_idx == self.cursor_line;

            if line_idx == 0 {
                spans.push(Span::styled(
                    format!("  {} ", play_icon),
                    Style::default().fg(Color::Cyan),
                ));
            } else {
                spans.push(Span::raw("    "));
            }

            for seg_idx in 0..parsed.segment_count() {
                if seg_idx > 0 {
                    // Render ':' separator
                    let sep_visual_col = seg_idx * (parsed.step_count() + 1) - 1;
                    let is_cursor = is_cursor_line
                        && sep_visual_col == self.cursor_col;
                    let style = if is_cursor {
                        Style::default().fg(Color::DarkGray).bg(Color::Cyan)
                    } else {
                        Style::default().fg(Color::DarkGray)
                    };
                    spans.push(Span::styled(":", style));
                }

                let chars = if seg_idx == 0 {
                    &parsed.notes
                } else {
                    &parsed.commands[seg_idx - 1]
                };

                for (step_idx, &ch) in chars.iter().enumerate() {
                    let global_idx = offset + step_idx;
                    let visual_col =
                        segment_step_to_visual_col(&parsed, seg_idx, step_idx);
                    let is_cursor = is_cursor_line
                        && visual_col == self.cursor_col;
                    let is_playhead =
                        global_idx == self.current_step && self.sequencer_playing;
                    let is_col_highlight = is_cursor_line
                        && !is_cursor
                        && cursor_step_idx == Some(step_idx);

                    let style = if is_cursor {
                        Style::default()
                            .fg(Color::Black)
                            .bg(Color::Cyan)
                            .add_modifier(Modifier::BOLD)
                    } else if is_col_highlight {
                        // Column highlight: same step as cursor, lighter shade
                        let base = if seg_idx == 0 {
                            if ch == '_' || ch == '-' {
                                Style::default().fg(Color::DarkGray)
                            } else {
                                Style::default().fg(Color::White)
                            }
                        } else {
                            command_char_style(ch)
                        };
                        base.bg(Color::Indexed(236))
                    } else if is_playhead {
                        Style::default()
                            .fg(Color::Black)
                            .bg(Color::Magenta)
                            .add_modifier(Modifier::BOLD)
                    } else if seg_idx == 0 {
                        // Note segment styling
                        if ch == '_' || ch == '-' {
                            Style::default().fg(Color::DarkGray)
                        } else {
                            Style::default().fg(Color::White)
                        }
                    } else {
                        // Command segment styling
                        command_char_style(ch)
                    };
                    spans.push(Span::styled(ch.to_string(), style));
                }
            }

            offset += parsed.step_count();
            lines.push(Line::from(spans));
        }

        let mut title = format!(" seq {}/{} ", self.current_step + 1, self.total_steps);
        if self.edit_mode {
            title.push_str("EDIT ");
            if self.insert_mode {
                title.push_str("INS ");
            }
        }

        let block = Block::default().borders(Borders::ALL).title(title);
        let paragraph = Paragraph::new(lines).block(block);
        frame.render_widget(paragraph, area);
    }

    fn render_footer(&self, frame: &mut Frame, area: Rect) {
        let block = Block::default().borders(Borders::ALL);

        if self.command_mode {
            let line = Line::from(vec![
                Span::styled(":", Style::default().fg(Color::Yellow)),
                Span::raw(&self.command_buffer),
                Span::styled(
                    "\u{2588}",
                    Style::default().fg(Color::Yellow),
                ),
            ]);
            let paragraph = Paragraph::new(vec![line, Line::from("")]).block(block);
            frame.render_widget(paragraph, area);
        } else if !self.confirm_prompt.is_empty() {
            let line = Line::from(Span::styled(
                format!("  {}", self.confirm_prompt),
                Style::default().fg(Color::Yellow),
            ));
            let paragraph = Paragraph::new(vec![line, Line::from("")]).block(block);
            frame.render_widget(paragraph, area);
        } else {
            let mut lines = Vec::new();

            if !self.status_message.is_empty() {
                lines.push(Line::from(Span::styled(
                    format!("  {}", self.status_message),
                    Style::default().fg(Color::Green),
                )));
            }

            if self.edit_mode {
                if lines.is_empty() {
                    lines.push(Line::from(
                        "  Note keys=edit | Arrows=move | Ins=toggle insert",
                    ));
                }
                lines.push(Line::from(
                    "  C-c=copy | C-v=paste | C-n=new | C-u=cmd seq | :=cmd | Esc=exit",
                ));
            } else {
                if lines.is_empty() {
                    lines.push(Line::from(
                        "  Key=play | Shift+key=free run | Tab=stutter",
                    ));
                }
                lines.push(Line::from(
                    "  Space=seq | Down=edit | C-n=new | :=command | Esc=quit",
                ));
            }

            let paragraph = Paragraph::new(lines)
                .block(block)
                .style(Style::default().fg(Color::DarkGray));
            frame.render_widget(paragraph, area);
        }
    }
}

/// Color scheme for command characters in command segments.
fn command_char_style(ch: char) -> Style {
    match ch {
        '-' => Style::default().fg(Color::DarkGray),
        '~' => Style::default().fg(Color::Yellow),
        '\\' | '/' => Style::default().fg(Color::Blue),
        'R' => Style::default().fg(Color::Red),
        'L' | 'H' => Style::default().fg(Color::Green),
        '*' => Style::default().fg(Color::Magenta),
        '>' | '<' | '^' => Style::default().fg(Color::Cyan),
        '(' | ')' | '[' | ']' => Style::default()
            .fg(Color::White)
            .add_modifier(Modifier::BOLD),
        // Bank selection
        '0'..='9' => Style::default().fg(Color::LightYellow),
        // Pitch up: q-p
        'q' | 'w' | 'e' | 'r' | 't' | 'y' | 'u' | 'i' | 'o' | 'p' => {
            Style::default().fg(Color::LightGreen)
        }
        // Pitch down: a-l
        'a' | 's' | 'd' | 'f' | 'g' | 'h' | 'j' | 'k' | 'l' => {
            Style::default().fg(Color::LightRed)
        }
        _ => Style::default().fg(Color::DarkGray),
    }
}
