//! TUI key bindings as a pure function (no TTY required).

/// Flatten is armed with `x`, confirmed with a second `x`. Any other key cancels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyAction {
    Quit,
    Strategy(i32),
    Refresh,
    FlattenArm,
    FlattenConfirm,
    FlattenCancel,
    Ignore,
}

pub fn handle_key(ch: char, flatten_armed: bool) -> KeyAction {
    match ch {
        'q' | 'Q' => KeyAction::Quit,
        '1' => KeyAction::Strategy(1),
        '2' => KeyAction::Strategy(2),
        '3' => KeyAction::Strategy(3),
        '4' => KeyAction::Strategy(4),
        'r' | 'R' => KeyAction::Refresh,
        'x' | 'X' => {
            if flatten_armed {
                KeyAction::FlattenConfirm
            } else {
                KeyAction::FlattenArm
            }
        }
        _ if flatten_armed => KeyAction::FlattenCancel,
        _ => KeyAction::Ignore,
    }
}

pub fn apply_flatten_key(armed: bool, ch: char) -> (bool, bool) {
    // returns (new_armed, confirmed)
    match handle_key(ch, armed) {
        KeyAction::FlattenArm => (true, false),
        KeyAction::FlattenConfirm => (false, true),
        KeyAction::FlattenCancel => (false, false),
        _ => (armed, false),
    }
}
