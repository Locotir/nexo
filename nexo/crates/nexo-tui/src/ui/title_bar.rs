use super::*;

mod draw;
mod gauge;
mod marks;
mod notes;
mod segments;

pub(super) use draw::draw_title;
use draw::segs_width;
use gauge::gauge_segs;
pub use gauge::{gauge_cells, line_gauge, GAUGE_CELLS};
use marks::{is_effort_seg, is_gauge_pct, is_goal_seg, is_todo_seg, mode_name, FAST_MARK, TODO_MARK};
use notes::{note_hits, note_spans, quiet_notes, NOTE_GAP};
pub use segments::{status_pieces, title_segments};

/// One coloured piece of the status bar (the bottom line).
pub struct Seg {
    pub text: String,
    pub style: Style,
}

pub(super) fn share(num: u64, den: u64) -> u64 {
    ((num as f64 / den.max(1) as f64) * 100.0).round().clamp(0.0, 999.0) as u64
}

pub fn short_path(p: &str) -> String {
    let p = nexo_core::paths::shown(std::path::Path::new(p));
    if width(&p) > 40 {
        let parts: Vec<&str> = p.rsplit('/').take(2).collect();
        format!("…/{}", parts.into_iter().rev().collect::<Vec<_>>().join("/"))
    } else {
        p
    }
}
