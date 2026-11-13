//! Drawing one frame from the app's state.

use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block as Box_, BorderType, Borders, Clear, Paragraph};
use ratatui::Frame;

use crate::anim;
use crate::app::{
    status_life, App, HintHit, Hover, StatusHit, FILES_PANEL_MIN_W, FILES_PANEL_W, MAP_FLASH_MS, STATUS_FADE_MS,
};
use crate::modal::{Modal, PickKind};
use crate::overlay;
use crate::palette::{PaletteKind, MAX_ROWS};
use crate::render::blocks::{fmt_k, todo_counts, SPINNER};
use crate::render::markdown::{inline_md, text_of, tint_plain};
use crate::render::wrap::{truncate, width, wrap_plain};
use crate::scrollbar::{self, Bar, Target};
use crate::select::style_cols;
use crate::theme::{mix, Theme};
use nexo_core::agent::{GoalState, Level};
use nexo_core::permission::PermissionMode;

mod command_palette;
mod hints;
mod info_panels;
mod input;
mod lock_screen;
mod modal;
mod question;
mod separator;
mod side_panel;
mod title_bar;
mod transcript;

use command_palette::*;
pub use hints::*;
use info_panels::*;
pub use input::*;
pub use lock_screen::*;
pub use modal::*;
pub use question::*;
pub use separator::*;
pub(crate) use side_panel::*;
pub use title_bar::*;
use transcript::*;

pub const MAX_INPUT_LINES: u16 = 8;
/// Queued requests shown above the draft; the rest are one click away on the bar.
pub const MAX_QUEUE_ROWS: usize = 3;

/// Button labels of a permission dialog with the key each one stands for.
type Buttons = (Vec<(String, char)>, usize);

pub fn draw(f: &mut Frame, app: &mut App) {
    draw_all(f, app);
    // a highlight noted while drawing (a list row, a block) starts its glide in this frame, after
    // the tick: the loop must know, or at rest the next frame waits for the next key and the
    // list shows the row before the one chosen
    let now = app.now();
    let fx = &app.fx;
    if fx.pick_sel.moving(now)
        || fx.pal_sel.moving(now)
        || fx.block_sel.moving(now)
        || fx.diff.moving(now)
        || app.diffview.as_ref().is_some_and(|d| d.fx.moving(now))
    {
        app.moving = true;
    }
}

fn draw_all(f: &mut Frame, app: &mut App) {
    let area = f.area();
    let (now0, on0) = (app.now(), app.fx_on());
    app.term_width = area.width;
    app.queue_rows.clear();
    app.todo_area = Rect::default();
    app.todo_shown = false;
    app.hint_hits.clear();
    app.note_popups();
    // the theme's ground under everything: no cell is left to the terminal's own colours
    f.render_widget(Box_::default().style(app.theme.base()), area);
    if let Some(dv) = &mut app.diffview {
        dv.fx.now = Some(now0);
        dv.fx.on = on0;
        app.fx.diff.note(false, &[], now0, on0); // the panel is not shown: reseed when it is back
        crate::diffview::render::draw(f, dv, area, &app.theme);
        // DF7: a dialog (permission, question, info) is always on top, the viewer never hides it
        if app.modal.is_some() {
            draw_modal(f, app, area);
        }
        scrub(f.buffer_mut());
        app.dirty = false;
        return;
    }
    if area.width < MIN_W || area.height < MIN_H {
        app.fx.diff.note(false, &[], now0, on0);
        draw_too_small(f, app, area);
        app.files_area = Rect::default();
        app.dirty = false;
        return;
    }
    let input_h = (crate::input::wrapped_rows(&app.input, area.width.saturating_sub(2) as usize).0.len() as u16)
        .clamp(1, MAX_INPUT_LINES);
    // a short window gives its keys line to the text, unless a window or the commands list is open
    // the keys row keeps its line while a draft is typed (blank then), so the text above never jumps
    // quiet: the row exists only while a list, a window or a mode needs its keys said
    let hints_h = if area.height <= SHORT_H && app.modal.is_none() && app.palette.is_none() {
        0
    } else if app.quiet() {
        u16::from(quiet_hints_on(app))
    } else {
        1
    };
    // the requests waiting behind the turn, a dim row each, right above the draft
    let queue_h = if app.queued_msgs.is_empty() || area.height <= SHORT_H {
        0
    } else {
        app.queued_msgs.len().min(MAX_QUEUE_ROWS) as u16
    };
    // the strip stays out where the list is on screen already: its own window, or the side panel
    let listed = matches!(app.modal, Some(Modal::Todo(_))) || (app.show_files && area.width >= FILES_PANEL_MIN_W);
    let todo_h =
        if app.todo_rows_on() && !listed { crate::todo_strip::strip_height(&app.todo, area.height) } else { 0 };
    // the keys right under the draft, the state (model, context, spend) on the last row
    let [middle, todo_area, queue, sep, input, hints, title] = Layout::vertical([
        Constraint::Min(0),
        Constraint::Length(todo_h),
        Constraint::Length(queue_h),
        Constraint::Length(1),
        Constraint::Length(input_h),
        Constraint::Length(hints_h),
        Constraint::Length(1),
    ])
    .areas(area);
    let files_w = if (app.show_files || app.show_live) && area.width >= FILES_PANEL_MIN_W {
        files_panel_width(area.width)
    } else {
        0
    };
    let [text_area, files] = Layout::horizontal([Constraint::Min(0), Constraint::Length(files_w)]).areas(middle);
    app.fx.diff.note(files_w > 0, &app.changed, now0, on0);
    let transcript = text_area;
    app.files_area = if files_w > 0 { files } else { Rect::default() };
    if files_w == 0 && (app.show_files || app.show_live) {
        if !app.panel_hint_shown {
            app.panel_hint_shown = true;
            app.flash(&format!("panel hidden under {FILES_PANEL_MIN_W} columns"));
        }
    } else {
        app.panel_hint_shown = false;
    }
    app.status_row = title.y;
    draw_title(f, app, title);
    if app.locked {
        app.transcript_area = transcript;
        // the card comes in once per start
        let now = app.now();
        app.note_welcome(now);
        draw_lock(f, app, transcript, 0.0);
    } else {
        draw_transcript(f, app, transcript);
        // the card slides away over the text for a moment after a model works
        if let Some(at) = app.fx.unlock_at {
            let t = anim::progress(Some(at), app.now(), UNLOCK_MS);
            if t < 1.0 {
                draw_lock(f, app, transcript, anim::ease_out(t));
            } else {
                app.fx.unlock_at = None;
            }
        }
    }
    app.fx.goal_spot.set(None);
    if files_w > 0 {
        draw_panel(f, app, files);
        app.fx.diff.draw_open(f.buffer_mut(), files, &app.theme, now0);
    }
    // the stars of an empty session over the transcript (not its scrollbar), then a burst over the whole room
    if !app.locked {
        app.note_home(now0);
        crate::fx_home::draw(f.buffer_mut(), app, Rect { width: transcript.width.saturating_sub(1), ..transcript });
    }
    crate::fx_burst::draw(f.buffer_mut(), app, middle);
    // the count in the bar is the list shown too: the keys row does not say it again
    let chip = app.status_hits.iter().any(|(_, h)| *h == StatusHit::Todo);
    app.todo_shown = todo_area.height > 0 || (files_w > 0 && !app.todo.is_empty()) || chip;
    draw_todo_strip(f, app, todo_area);
    draw_queue(f, app, queue);
    draw_separator(f, app, sep);
    if app.locked && app.input.is_empty() {
        draw_lock_input(f, app, input);
    } else {
        draw_input(f, app, input);
    }
    // the first screen has no key hints, but a message (Ctrl+C again to quit) still needs its row
    if (!app.locked || app.status.is_some()) && !matches!(app.modal, Some(Modal::Permission(_))) {
        draw_hints(f, app, hints);
    }
    // toasts: one row each, top right of the transcript, under every dialog and off the scrollbar column
    let toast_area = Rect { width: transcript.width.saturating_sub(1), ..transcript };
    crate::toast::draw(f, &app.toasts, &app.theme, toast_area, app.now(), app.opts.animations);
    if app.palette.is_some() {
        draw_palette(f, app, transcript);
    }
    if app.modal.is_some() {
        // over the transcript only: the draft and the state line stay in sight
        // the /context window wants 8 rows: on a short screen it also takes the rule's row under the text
        let over = Rect { height: middle.height + sep.height, ..middle };
        let area_for = match app.modal {
            Some(Modal::Info(_) | Modal::Agents(_) | Modal::Question(_)) if middle.height < 8 && over.height >= 6 => {
                over
            }
            _ if middle.height >= 5 => middle,
            _ => area,
        };
        // a permission dialog over the welcome card (the first screen) clears the rows of the card. The to-do and jobs windows get it, so no piece of the welcome card shows beside them; the
        // others keep the text in sight (a theme preview needs it) and clear only the cells beside the box
        if matches!(
            app.modal,
            Some(Modal::Todo(_) | Modal::Jobs(_) | Modal::Mcp(_) | Modal::Goal(_) | Modal::Sent(_) | Modal::Trust(_))
        ) {
            clear_bg(f, area_for, &app.theme);
        } else if app.has_welcome() && matches!(app.modal, Some(Modal::Permission(_))) {
            // only the rows the card holds, so the text around a dialog stays in sight
            let card_end = app.offsets.get(1).copied().unwrap_or(app.total_lines);
            let rows = card_end.saturating_sub(app.view_top()).min(usize::from(area_for.height)) as u16;
            clear_bg(f, Rect { height: rows, ..area_for }, &app.theme);
        }
        draw_modal(f, app, area_for);
        if matches!(app.modal, Some(Modal::Permission(_))) {
            draw_hints(f, app, hints);
        }
    }
    // a dialog or the palette leaving is drawn as the reverse of its pop
    if app.palette.is_some() || app.modal.is_some() {
        app.ghost_save(f.buffer_mut(), middle);
    } else {
        app.ghost_draw(f.buffer_mut());
    }
    scrub(f.buffer_mut());
    app.dirty = false;
}

/// The last line of defence: a cell whose text would move the cursor, ring, recolour or draw
/// differently from the layout (a tab, an escape, a line break, a joiner, a direction override)
/// shows a space instead. Every text is cleaned where it arrives; this catches the one that was not.
pub fn scrub(buf: &mut ratatui::buffer::Buffer) {
    for cell in buf.content.iter_mut() {
        if cell.symbol().chars().any(crate::render::wrap::unsafe_char) {
            cell.set_symbol(" ");
        }
    }
}

/// Clears a rectangle and paints the theme's ground on it (a `Clear` alone
/// would leave the terminal's own background there).
fn clear_bg(f: &mut Frame, r: Rect, theme: &Theme) {
    f.render_widget(Clear, r);
    f.render_widget(Box_::default().style(theme.base()), r);
}

/// One cell of clear ground on each side of a dialog, so the transcript's markers do not peek out beside it.
fn clear_margin(f: &mut Frame, r: Rect, area: Rect, theme: &Theme) {
    let x = r.x.saturating_sub(1).max(area.x);
    let end = (r.x + r.width + 1).min(area.x + area.width);
    clear_bg(f, Rect::new(x, r.y, end - x, r.height), theme);
}

/// Where the welcome card is on the screen (an empty rectangle when it is not in sight).
fn card_rect(app: &App) -> Rect {
    if !app.has_welcome() {
        return Rect::default();
    }
    let t = app.transcript_area;
    let card_end = app.offsets.get(1).copied().unwrap_or(app.total_lines);
    let rows = card_end.saturating_sub(app.view_top()).min(usize::from(t.height)) as u16;
    Rect { height: rows, ..t }
}

/// A window over the welcome card clears the card's rows it spans, across the card: no piece of
/// the card's frame (its edge, its corners) is left beside the window. Rows of the card the window
/// does not reach, and every other kind of text, stay in sight.
fn clear_card_beside(f: &mut Frame, card: Rect, window: Rect, theme: &Theme) {
    let (top, bottom) = (window.y.max(card.y), (window.y + window.height).min(card.y + card.height));
    if top < bottom {
        clear_bg(f, Rect::new(card.x, top, card.width, bottom - top), theme);
    }
}

/// The text of a drawn buffer, row by row (tests).
pub fn rows_of(buf: &ratatui::buffer::Buffer) -> Vec<String> {
    let area = buf.area;
    let mut rows = Vec::new();
    for y in 0..area.height {
        let mut s = String::new();
        for x in 0..area.width {
            s.push_str(buf[(x, y)].symbol());
        }
        rows.push(s.trim_end().to_string());
    }
    rows
}

#[allow(dead_code)]
fn _unused(l: &Line) -> String {
    text_of(l)
}

/// Below this the screen cannot hold a draft, a status line and a line of text: a notice is drawn instead.
pub const MIN_W: u16 = 24;
pub const MIN_H: u16 = 6;
/// A window this short or shorter gives its keys line to the transcript.
pub const SHORT_H: u16 = 12;

/// The files panel grows with the window: the narrowest that fits, a wider one on a big screen.
pub fn files_panel_width(total: u16) -> u16 {
    match total {
        0..=119 => FILES_PANEL_W,
        120..=159 => FILES_PANEL_W + 6,
        _ => FILES_PANEL_W + 12,
    }
}

/// A dialog is open but its window does not fit: one line says so (the keys still reach the dialog),
/// instead of a screen that looks frozen.
fn draw_too_narrow(f: &mut Frame, theme: &Theme, area: Rect, narrow: bool) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let msg = if narrow { "make the window wider" } else { "make the window taller" };
    let msg: String = truncate(msg, area.width as usize);
    let y = area.y + area.height / 2;
    let line = Line::from(Span::styled(msg, theme.style(theme.warn))).alignment(ratatui::layout::Alignment::Center);
    f.render_widget(Paragraph::new(line), Rect::new(area.x, y, area.width, 1));
}

/// The notice under the minimum size: what is wrong and what the window has, nothing cut mid-word.
fn draw_too_small(f: &mut Frame, app: &App, area: Rect) {
    let theme = &app.theme;
    let need = format!("needs {MIN_W}x{MIN_H}");
    let have = format!("has {}x{}", area.width, area.height);
    let lines: Vec<Line<'static>> = ["window too small".to_string(), need, have]
        .into_iter()
        .map(|s| s.chars().take(area.width as usize).collect::<String>())
        .enumerate()
        .map(|(i, s)| {
            let style = if i == 0 { theme.style(theme.warn).add_modifier(Modifier::BOLD) } else { theme.dim() };
            Line::from(Span::styled(s, style)).alignment(ratatui::layout::Alignment::Center)
        })
        .collect();
    let h = lines.len().min(area.height as usize) as u16;
    let top = area.y + area.height.saturating_sub(h) / 2;
    f.render_widget(Paragraph::new(Text::from(lines)), Rect { y: top, height: h, ..area });
}

#[cfg(test)]
mod tests {
    #[test]
    fn search_highlight_survives_case_folds_that_move_character_boundaries() {
        use ratatui::style::Style;
        use ratatui::text::Line;
        // "Ⱥ" (2 bytes) folds to 3, "ẞ" (3 bytes) folds to 2: same length, other boundaries
        let l = super::highlight(Line::from("Ⱥẞ ñ"), "ß", Style::default(), true);
        let text: String = l.spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(text, "Ⱥẞ ñ");
        let l = super::highlight(Line::from("año ÑU"), "ñ", Style::default(), false);
        let parts: Vec<&str> = l.spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(parts, ["a", "ñ", "o ", "Ñ", "U"]);
    }

    #[test]
    fn an_argument_hint_is_cut_at_a_bar_never_mid_word() {
        use super::cut_args;
        let h = "[ask|accept-edits|plan|yolo]";
        assert_eq!(cut_args(h, 28), h);
        assert_eq!(cut_args(h, 24), "[ask|accept-edits|plan…");
        assert_eq!(cut_args(h, 20), "[ask|accept-edits…");
        assert_eq!(cut_args(h, 3), "", "not even the first choice fits: no hint");
    }

    #[test]
    fn a_long_path_keeps_its_file_name() {
        use super::{path_tail, width};
        let (t, h) = path_tail("crates/nexo-tui/src/render/markdown.rs", &[27, 28], 16, false);
        assert_eq!(t, "markdown.rs  …er");
        assert_eq!(width(&t), 16);
        assert_eq!(h, [0, 1], "the marks stay on the m and a of markdown");
        // the folder is what is cut, and as much of it as fits stays
        let (t, _) = path_tail("crates/nexo-bench/src/tasks/inventory.rs", &[], 38, false);
        assert_eq!(t, "inventory.rs  …es/nexo-bench/src/tasks");
        assert_eq!(
            path_tail("crates/nexo-bench/src/tasks/inventory.rs", &[], 30, false).0,
            "inventory.rs  …bench/src/tasks"
        );
        // on a narrow list the short paths take the same form, so the rows line up
        assert_eq!(path_tail("src/mod.rs", &[], 30, true).0, "mod.rs  src");
        assert_eq!(path_tail("a.rs", &[0], 16, true), ("a.rs".to_string(), vec![0]));
        assert_eq!(path_tail("a.rs", &[0], 16, false), ("a.rs".to_string(), vec![0]));
    }

    #[test]
    fn hints_that_do_not_fit_are_dropped_whole() {
        let h = " Enter send · Ctrl+J newline · / commands · @ files";
        assert_eq!(super::fit_hints("", h, "", true, 40), " Enter send · Ctrl+J newline · @ files");
        assert_eq!(super::fit_hints("", h, "", true, 100), h);
        assert!(super::fit_hints("", h, "", true, 5).chars().count() <= 5);
    }

    #[test]
    fn status_pieces_follow_the_configured_order() {
        let v = |l: &[&str]| l.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(status_pieces(&[]).len(), 12);
        assert_eq!(status_pieces(&v(&["Context", "model", "bogus", "context"])), vec!["context", "model"]);
        assert!(status_pieces(&v(&["bogus"])).is_empty());
    }

    use super::*;
    use crate::theme::Theme;

    #[test]
    fn the_comet_runs_along_the_rule_and_keeps_its_width() {
        let t = Theme::nexo();
        for frame in [0, 7, 30, 200] {
            let spans = comet(&t, 40, frame);
            let text: String = spans.iter().map(|s| s.content.as_ref()).collect();
            assert_eq!(text, "─".repeat(40));
        }
        let lit = |frame| comet(&t, 40, frame).iter().any(|s| s.style.fg == Some(t.accent));
        assert!((0..60).any(lit), "the head is drawn in the accent somewhere along the way");
        assert!(comet(&t, 0, 3).is_empty());
    }

    #[test]
    fn the_gauge_colour_follows_the_share() {
        let t = Theme::nexo();
        assert_eq!(t.level_color(0), t.success);
        assert_eq!(t.level_color(49), t.success);
        assert_eq!(t.level_color(50), t.warn);
        assert_eq!(t.level_color(74), t.warn);
        assert_eq!(t.level_color(75), t.warn2);
        assert_eq!(t.level_color(89), t.warn2);
        assert_eq!(t.level_color(90), t.error);
        assert_eq!(t.level_color(130), t.error);
        assert_eq!(share(92, 100), 92);
        assert_eq!(gauge_cells(0.5, 8), ("████".to_string(), 4));
        assert_eq!(gauge_cells(0.11, 8), ("▉".to_string(), 7));
        assert_eq!(gauge_cells(1.0, 8), ("████████".to_string(), 0));
        assert_eq!(gauge_cells(0.0, 8), (String::new(), 8));
        assert_eq!(line_gauge(0.5, 10), ("━━━━━".to_string(), "━━━━━".to_string()));
        assert_eq!(line_gauge(0.25, 10), ("━━╸".to_string(), "━━━━━━━".to_string()));
        assert_eq!(line_gauge(1.0, 4), ("━━━━".to_string(), String::new()));
        assert_eq!(line_gauge(0.0, 4), (String::new(), "━━━━".to_string()));
    }

    #[test]
    fn every_theme_has_an_orange() {
        for n in crate::theme::NAMES {
            let t = Theme::by_name(n).unwrap();
            if !t.mono {
                assert_ne!(t.warn2, t.warn, "{n}");
                assert_ne!(t.warn2, t.error, "{n}");
            }
        }
    }
}
