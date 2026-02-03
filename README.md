# breaky

A console drum loop slicer and step sequencer. Loads a drum break, detects
beats, slices it into up to 16 pads, and lets you play, sequence, and edit
patterns from the terminal.

Built entirely in Rust with no C dependencies.

## Features

- Automatic onset detection and beat slicing (spectral flux algorithm)
- BPM detection with automatic resampling to match target tempo
- Keyboard-triggered pad playback mapped to two rows of keys
- Three play modes: slice, free-run, and stutter
- Built-in step sequencer with live pattern editing
- Per-step command sequences for effects (filters, reverse, distortion, pitch shift)
- Vi-style command mode for save/load/quit
- YAML config files for storing patterns and settings
- Supports WAV, MP3, FLAC, OGG, and AAC audio formats

## Building

Requires Rust 1.85+ and ALSA development headers on Linux:

```sh
# Debian/Ubuntu
sudo apt install libasound2-dev

# Fedora
sudo dnf install alsa-lib-devel
```

Then build:

```sh
cargo build --release
```

## Usage

### From an audio file

Point breaky at a drum loop. It will detect beats, calculate the BPM, slice
the audio into 16 pads, and write a default YAML config alongside the file:

```sh
breaky amen_break.wav
```

This creates `amen_break.yaml` with a default two-bar pattern that plays all
16 slices in order.

### From a YAML config

Load a previously saved pattern:

```sh
breaky amen_break.yaml
```

The config references the audio file by name and stores the BPM and beat
pattern. If the target BPM differs from the detected BPM, the audio is
resampled automatically.

### YAML format

```yaml
sample: amen_break.wav
bpm: 136.5
beats:
  - qwertyui
  - asdfghjk:--*-----:----R---
```

Each character in a beat string maps to a slice pad. `_` represents silence.

Command sequences are appended after `:` separators. In the example above, the
second line has two command sequences: step 3 has distortion (`*`) and step 5
plays in reverse (`R`).

## Keyboard Controls

### Normal Mode

| Key | Action |
|---|---|
| `q` `w` `e` `r` `t` `y` `u` `i` | Play slice 1-8 |
| `a` `s` `d` `f` `g` `h` `j` `k` | Play slice 9-16 |
| `Shift` + key | Free-run (play from slice to end of buffer) |
| `Tab` | Stutter current slice (1/16 beat retrigger) |
| `Space` | Start/stop sequencer |
| `Down` | Enter edit mode |
| `Ctrl-N` | Add new empty sequence line |
| `:` | Enter command mode |
| `Esc` | Quit |

### Edit Mode

| Key | Action |
|---|---|
| Note key (`q`-`k`, `_`) | Replace current step and preview the sound |
| Command char (in cmd seq) | Replace current step with effect command |
| `Left` / `Right` | Move cursor (skips `:` separators) |
| `Up` / `Down` | Move between lines (Up from first line exits edit) |
| `Insert` | Toggle insert mode (auto-advance after each note) |
| `Ctrl-C` | Copy current line |
| `Ctrl-V` | Paste line after current line |
| `Ctrl-N` | Add new empty sequence line after current |
| `Ctrl-U` | Add command sequence (`--------`) to current line |
| `Ctrl-I` | Remove empty command sequences from current line |
| `Space` | Start/stop sequencer |
| `Tab` | Stutter |
| `:` | Enter command mode |
| `Esc` | Exit edit mode |

### Command Mode

Type `:` followed by a command and press `Enter`:

| Command | Action |
|---|---|
| `:w` | Save the YAML config |
| `:wq` | Save and quit |
| `:q` | Quit (prompts if unsaved changes) |
| `:q!` | Force quit without saving |
| `:e` | Reload config from disk (prompts if unsaved changes) |
| `:e!` | Force reload without prompting |

Unsaved changes are indicated by `[+]` in the header bar.

## Command Sequences

Each beat line can have command sequences appended after `:` separators to apply
per-step effects. Navigate the cursor into a command sequence (past the first `:`)
and type effect characters.

All effects reset at the start of each beat line. Effects are processed in order:
sample fetch (with reverse/pitch) -> low-pass filter -> high-pass filter ->
distortion -> fade envelope.

### Effect Characters

| Char | Effect | Description |
|---|---|---|
| `~` | Stutter | Rapid retrigger within the slice |
| `\` | Fade out | Volume ramps from 1.0 to 0.0 to end of line |
| `/` | Fade in | Volume ramps from 0.0 to 1.0 to end of line |
| `R` | Reverse | Play slice backwards |
| `L` | Low-pass | Apply low-pass filter (800 Hz cutoff) |
| `H` | High-pass | Apply high-pass filter (2 kHz cutoff) |
| `*` | Distortion | Apply tanh waveshaping distortion |
| `-` | Repeat | Repeat previous command in sequence |

### Effect Modifiers

Modifiers follow `L`, `H`, or `*` to control how the effect is applied:

| Modifier | Description |
|---|---|
| `<` | Fade in: wet/dry mix ramps 0% to 100% |
| `>` | Fade out: wet/dry mix ramps 100% to 0% |
| `^` | Cut: stops effect, prevents `-` from continuing |

Example: `L<------L` ramps low-pass from dry to wet, then `L` at position 8
could start a new ramp.

### Speed Control

| Chars | Effect | Description |
|---|---|---|
| `(` `)` | Half speed | Play at half speed between markers |
| `[` `]` | Double speed | Play at double speed between markers |

### Pitch Shift

Lowercase letters shift pitch:

| Range | Effect |
|---|---|
| `q` through `p` | Pitch up (+1 to +10 semitones) |
| `a` through `l` | Pitch down (-1 to -9 semitones) |

### Example

```yaml
beats:
  - qwertyui:L^------:----R---
```

This line plays slices q-w-e-r-t-y-u-i with:
- Low-pass filter on for the entire line (`L^`)
- Reverse playback on step 5 (`R`)

## UI Layout

```
┌─ breaky ────────────────────────────────────────┐
│  breaky - amen_break.wav  |  44100 Hz  |  7.1s  │
├─────────────────────────────────────────────────┤
│  BPM: 136.5    Beats: 16    Mode: SLICE         │
├─────────────────────────────────────────────────┤
│  [q]1  [w]2  [e]3  [r]4  [t]5  [y]6  [u]7  [i]8│
│  [a]9  [s]10 [d]11 [f]12 [g]13 [h]14 [j]15 [k]16│
├─ seq 1/16 ──────────────────────────────────────┤
│  ▶ qwertyui                                      │
│    asdfghjk                                      │
├─────────────────────────────────────────────────┤
│  Key=play | Shift+key=free run | Tab=stutter     │
│  Space=seq | Down=edit | :=command | Esc=quit    │
└─────────────────────────────────────────────────┘
```

- The **pad grid** highlights the active slice in yellow during playback
- The **sequencer** shows the playhead in magenta; the edit cursor in cyan
- The **footer** shows contextual keybindings and status messages

## Architecture

Two threads communicate via lock-free atomics:

```
Main Thread                        Audio Thread (cpal callback)
 ├─ keyboard events (crossterm)     ├─ reads PlaybackState atomics
 ├─ UI rendering (ratatui ~60fps)   └─ writes samples to output
 └─ writes to PlaybackState
```

No mutexes, no allocations in the audio path.

## Dependencies

| Crate | Purpose |
|---|---|
| `symphonia` | Audio decoding (WAV, MP3, FLAC, OGG, AAC) |
| `cpal` | Cross-platform audio output |
| `rustfft` | FFT for spectral flux onset detection |
| `ratatui` + `crossterm` | Terminal UI and keyboard input |
| `clap` | CLI argument parsing |
| `serde` + `serde_yaml` | YAML config serialization |
| `anyhow` | Error handling |

## License

MIT
