//! The notes the quiet mode moves into the status bar.

use super::*;

/// Columns kept clear between the bar's pieces and the notes at its right.
pub(super) const NOTE_GAP: usize = 2;

/// Quiet: what the keys row carried at its right (a message, queued requests, running jobs, shared notes) rides
/// in the free space of the status bar instead, so the row can go. Returns the pieces that fit `w / 2` of what
/// the clock and the cost leave, and the columns they take with the gap. The session name stays out.
pub(super) fn quiet_notes(app: &mut App, w: usize, right_w: usize) -> (Vec<Piece>, usize) {
    let room = w.saturating_sub(right_w) / 2;
    if !app.quiet() {
        return (Vec::new(), 0);
    }
    // a warning or an error clipped to the room (or with no room at all) is not lost: the whole of it goes to
    // a toast, once
    if let Some((msg, _)) = app.status.clone() {
        if width(&msg) > room.saturating_sub(2) && !app.status_toasted && app.status_level != Level::Info {
            app.status_toasted = true;
            let kind = match app.status_level {
                Level::Error => crate::toast::Kind::Error,
                _ => crate::toast::Kind::Info,
            };
            app.toast(kind, &msg);
        }
    }
    if room < 8 {
        return (Vec::new(), 0);
    }
    let mut pieces = hint_pieces(app, room);
    pieces.retain(|p| !matches!(p.1, Some(HintHit::Session | HintHit::Todo)));
    let span_w = |ps: &[Piece]| -> usize { join_pieces(app, ps).iter().map(|s| width(&s.content)).sum() };
    while !pieces.is_empty() && span_w(&pieces) > room {
        let low = pieces.iter().enumerate().min_by_key(|(_, p)| p.0).map(|(i, _)| i).unwrap_or(0);
        pieces.remove(low);
    }
    let total = if pieces.is_empty() { 0 } else { span_w(&pieces) + NOTE_GAP };
    (pieces, total)
}

/// The spans of the notes, after the gap that sets them apart from the bar's pieces.
pub(super) fn note_spans(app: &App, notes: &[Piece], notes_w: usize) -> Vec<Span<'static>> {
    if notes_w == 0 {
        return Vec::new();
    }
    let mut out = vec![Span::styled(" ".repeat(NOTE_GAP), app.theme.bar())];
    out.extend(join_pieces(app, notes));
    out
}

/// Where each clickable note sits when they start at column `x`.
pub(super) fn note_hits(notes: &[Piece], x: usize, area: Rect) -> Vec<(Rect, HintHit)> {
    let mut at = x;
    let mut hits = Vec::new();
    for (i, (_, hit, parts)) in notes.iter().enumerate() {
        if i > 0 {
            at += width(" ·");
        }
        let w: usize = parts.iter().map(|s| width(&s.content)).sum();
        if let Some(h) = hit {
            hits.push((Rect::new(area.x + at as u16, area.y, w as u16, 1), *h));
        }
        at += w;
    }
    hits
}
