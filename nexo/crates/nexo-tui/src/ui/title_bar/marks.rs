//! How the bar tells its pieces apart: the marks and the predicates over a segment.

use super::*;

/// The fast-mode marker after the model.
pub(super) const FAST_MARK: &str = " ⚡";

/// The reasoning level's piece of the bar: ` ◆ high`.
pub(super) fn is_effort_seg(s: &Seg) -> bool {
    s.text.starts_with(" ◆ ")
}

/// The words the bar shows for the permission mode.
pub(super) fn mode_name(app: &App) -> String {
    match mode_tint(&app.theme, app.permission_mode) {
        Some((_, label)) => label.split(" · ").next().unwrap_or(label).to_string(),
        None => app.permission_mode.as_str().to_string(),
    }
}

/// The goal's piece of the bar: `◎ goal`, `◎ goal: your turn`, `◎ goal paused`, `✓ goal`.
pub(super) fn is_goal_seg(s: &Seg) -> bool {
    s.text.starts_with("◎ goal") || s.text.starts_with("✓ goal")
}

/// The mark of the to-do count in the bar (the terminal's ASCII fallback folds it like the goal's check).
pub(super) const TODO_MARK: &str = "✓";

/// The to-do count's piece of the bar: `✓ 3/7`.
pub(super) fn is_todo_seg(s: &Seg) -> bool {
    s.text.strip_prefix(TODO_MARK).is_some_and(|r| r.trim_start().starts_with(|c: char| c.is_ascii_digit()))
}

/// The gauge's percentage (" 42%", " ~42%"), not another piece that ends in one.
pub(super) fn is_gauge_pct(s: &Seg) -> bool {
    let t = s.text.trim_start().trim_start_matches('~');
    t.ends_with('%') && t.starts_with(|c: char| c.is_ascii_digit())
}
