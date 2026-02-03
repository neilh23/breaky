use crossterm::event::{KeyCode, KeyModifiers};

/// Map a key press to a slice index (0-15).
/// Row 1: q=0 w=1 e=2 r=3 t=4 y=5 u=6 i=7
/// Row 2: a=8 s=9 d=10 f=11 g=12 h=13 j=14 k=15
pub fn key_to_slice(code: KeyCode) -> Option<usize> {
    match code {
        KeyCode::Char('q') => Some(0),
        KeyCode::Char('w') => Some(1),
        KeyCode::Char('e') => Some(2),
        KeyCode::Char('r') => Some(3),
        KeyCode::Char('t') => Some(4),
        KeyCode::Char('y') => Some(5),
        KeyCode::Char('u') => Some(6),
        KeyCode::Char('i') => Some(7),
        KeyCode::Char('a') => Some(8),
        KeyCode::Char('s') => Some(9),
        KeyCode::Char('d') => Some(10),
        KeyCode::Char('f') => Some(11),
        KeyCode::Char('g') => Some(12),
        KeyCode::Char('h') => Some(13),
        KeyCode::Char('j') => Some(14),
        KeyCode::Char('k') => Some(15),
        // Also handle uppercase (Shift held)
        KeyCode::Char('Q') => Some(0),
        KeyCode::Char('W') => Some(1),
        KeyCode::Char('E') => Some(2),
        KeyCode::Char('R') => Some(3),
        KeyCode::Char('T') => Some(4),
        KeyCode::Char('Y') => Some(5),
        KeyCode::Char('U') => Some(6),
        KeyCode::Char('I') => Some(7),
        KeyCode::Char('A') => Some(8),
        KeyCode::Char('S') => Some(9),
        KeyCode::Char('D') => Some(10),
        KeyCode::Char('F') => Some(11),
        KeyCode::Char('G') => Some(12),
        KeyCode::Char('H') => Some(13),
        KeyCode::Char('J') => Some(14),
        KeyCode::Char('K') => Some(15),
        _ => None,
    }
}

/// Check if Shift is held in the modifiers.
pub fn is_shift(modifiers: KeyModifiers) -> bool {
    modifiers.contains(KeyModifiers::SHIFT)
}

/// Map a key press to a beat-pattern character for inline editing.
/// Returns lowercase note char or '_' for silence. None for non-note keys.
pub fn key_to_beat_char(code: KeyCode) -> Option<char> {
    match code {
        KeyCode::Char(c) => {
            let c = c.to_ascii_lowercase();
            match c {
                'q' | 'w' | 'e' | 'r' | 't' | 'y' | 'u' | 'i' | 'a' | 's' | 'd' | 'f'
                | 'g' | 'h' | 'j' | 'k' | '_' => Some(c),
                _ => None,
            }
        }
        _ => None,
    }
}

/// Map a key press to a command-pattern character for inline editing.
/// Accepts all valid command characters from COMMANDS.txt.
/// Lowercase letters = pitch shift, uppercase R/L/H = effects.
pub fn key_to_command_char(code: KeyCode) -> Option<char> {
    match code {
        KeyCode::Char(c) => {
            if crate::config::is_valid_command_char(c) {
                Some(c)
            } else {
                None
            }
        }
        _ => None,
    }
}

/// Key labels for the UI grid.
pub const KEY_LABELS: [&str; 16] = [
    "[q]1", "[w]2", "[e]3", "[r]4", "[t]5", "[y]6", "[u]7", "[i]8", "[a]9", "[s]10", "[d]11",
    "[f]12", "[g]13", "[h]14", "[j]15", "[k]16",
];
