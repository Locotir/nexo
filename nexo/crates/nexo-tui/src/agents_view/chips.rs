//! The glyph, the label and the colour of a state, and how long a session has been quiet.

use super::*;

/// The glyph, the label and the order of a state in the header.
pub(super) fn chip(state: State) -> (&'static str, &'static str) {
    match state {
        State::WaitingPermission => ("◈", "permission"),
        State::WaitingAnswer => ("◆", "question"),
        State::Stalled => ("◐", "stalled"),
        State::Looping => ("↻", "looping"),
        State::Working => ("●", "working"),
        State::Idle { .. } => ("○", "idle"),
        State::Done => ("✓", "done"),
    }
}

pub(super) fn attention(state: State) -> bool {
    matches!(state, State::WaitingPermission | State::WaitingAnswer | State::Stalled)
}

pub(super) fn chip_color(theme: &Theme, state: State) -> ratatui::style::Color {
    draw::legible(theme, chip_hue(theme, state))
}

fn chip_hue(theme: &Theme, state: State) -> ratatui::style::Color {
    match state {
        State::WaitingPermission => theme.warn2,
        State::WaitingAnswer => theme.accent2,
        State::Stalled => theme.warn,
        State::Looping => theme.error,
        State::Working | State::Done => theme.success,
        State::Idle { .. } => theme.dim,
    }
}

/// `42s`, `7m`, `2h`, `3d`: one unit, rounded down, never more than four columns.
pub(super) fn age(secs: u64) -> String {
    match secs {
        0..=59 => format!("{secs}s"),
        60..=3599 => format!("{}m", secs / 60),
        3600..=86399 => format!("{}h", secs / 3600),
        _ => format!("{}d", secs / 86400),
    }
}
