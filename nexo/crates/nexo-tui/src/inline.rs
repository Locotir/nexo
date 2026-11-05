//! The inline mode (`--no-alt-screen`): when the session ends, the whole conversation is written
//! to the normal screen, so it stays in the terminal's scrollback like any program's output.

use crate::app::App;

/// The conversation as plain text wrapped for `width` columns, one `\n` per row, with the same
/// blank lines between blocks as the screen.
pub fn scrollback_text(app: &mut App, width: u16) -> String {
    let w = width.saturating_sub(1).max(1) as usize;
    app.layout(w);
    let mut out = String::new();
    for i in 0..app.blocks.len() {
        let end = app.offsets.get(i + 1).copied().unwrap_or(app.total_lines);
        let room = end - app.offsets[i];
        if room == 0 {
            continue;
        }
        let lines = app.lines(i, w).to_vec();
        for line in lines.iter().take(room) {
            let row: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
            out.push_str(row.trim_end());
            out.push('\n');
        }
        for _ in lines.len()..room {
            out.push('\n');
        }
    }
    out.trim_end_matches('\n').to_string()
}

use ratatui::backend::Backend;
use ratatui::text::Line;
use ratatui::widgets::{Paragraph, Widget};
use ratatui::Terminal;

use crate::render::blocks::Block;

/// How many blocks are settled: all of them while nothing runs; during a turn, every block before
/// the request that started it (the turn in progress stays in the live zone).
pub fn settled_count(app: &App) -> usize {
    if !app.busy {
        return app.blocks.len();
    }
    app.blocks.iter().rposition(|b| matches!(b, Block::User { .. })).unwrap_or(0)
}

/// The rows of the blocks that settled since the last call, wrapped for `width` columns with the
/// blank lines between blocks that the screen has; marks them as written.
pub fn take_settled(app: &mut App, width: u16) -> Vec<Line<'static>> {
    let target = settled_count(app);
    let w = width.saturating_sub(1).max(1) as usize;
    let mut out = Vec::new();
    // the blank lines of a block that was already written belong to the one before it
    for i in app.inline_flushed..target {
        // the calls of a folded group are stood for by its line
        if app.hidden(i) {
            continue;
        }
        let lines = app.lines(i, w).to_vec();
        let gap = app.gap_after(i, lines.len());
        // a block that shows nothing takes no room
        let empty = lines.is_empty();
        out.extend(lines);
        if !empty {
            out.extend(std::iter::repeat_with(Line::default).take(gap));
        }
    }
    app.inline_flushed = app.inline_flushed.max(target);
    out
}

/// Writes the settled rows above the live zone, in the terminal's own scrollback.
pub fn flush<B: Backend>(terminal: &mut Terminal<B>, app: &mut App) -> anyhow::Result<()> {
    let size = terminal.size()?;
    let rows = take_settled(app, size.width);
    if rows.is_empty() {
        return Ok(());
    }
    // chunks that fit the part of the terminal above the live zone
    let chunk = size.height.saturating_sub(crate::terminal::live_rows(size.height)).max(1) as usize;
    for part in rows.chunks(chunk) {
        let part = part.to_vec();
        terminal.insert_before(part.len() as u16, |buf| {
            Paragraph::new(part).render(buf.area, buf);
        })?;
    }
    app.dirty = true;
    Ok(())
}
