//! Terminal events (keys, mouse, paste, resize) applied to the app.

use crate::keymap::Action;
use crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};

use crate::app::{App, WHEEL_LINES};
use crate::input::Edit;
use crate::modal::contains;

/// A key that typing makes: a character (AltGr's included), Enter, Tab. Esc too, which
/// starts the markers of a bracketed paste that comes as keys.
fn typed(k: &KeyEvent) -> Option<char> {
    let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
    let alt = k.modifiers.contains(KeyModifiers::ALT);
    if ctrl != alt {
        return None;
    }
    match k.code {
        KeyCode::Char(c) => Some(c),
        KeyCode::Enter => Some('\n'),
        KeyCode::Tab => Some('\t'),
        KeyCode::Esc if k.modifiers.is_empty() => Some('\x1b'),
        _ => None,
    }
}

/// Whether this event may start a paste that comes as keys (a terminal without
/// bracketed paste, the Windows console): the run loop then gathers what follows it.
pub fn may_start_paste(ev: &Event) -> bool {
    matches!(ev, Event::Key(k) if k.kind != KeyEventKind::Release && typed(k).is_some())
}

/// Keys pressed in a batch (releases, which Windows reports, left out).
pub fn presses(batch: &[Event]) -> usize {
    batch.iter().filter(|e| matches!(e, Event::Key(k) if k.kind != KeyEventKind::Release)).count()
}

/// Bracketed paste's markers, which arrive as plain keys where the terminal sends them
/// but crossterm reads key records (the Windows console).
const MARKS: [&str; 2] = ["\x1b[200~", "\x1b[201~"];

/// Keys that came faster than hands type: at least [`PASTE_KEYS`] typed keys (any number
/// right after a paste, whose tail this is) and no other key. That is a paste, and every
/// Enter in it is a new line, never a send. Other events in the batch (the mouse moving)
/// do not count; the run loop handles them apart.
pub fn paste_burst(batch: &[Event], after_paste: bool) -> Option<String> {
    let mut text = String::new();
    for e in batch {
        match e {
            Event::Key(k) if k.kind == KeyEventKind::Release => {}
            Event::Key(k) => text.push(typed(k)?),
            _ => {}
        }
    }
    let marked = text.contains(MARKS[0]);
    // an Esc that is no marker's is a key, not a paste; within a paste it is a marker cut
    // between two pieces, dropped with the rest at its end
    let stray = MARKS.iter().fold(text.clone(), |t, m| t.replace(m, "")).contains('\x1b');
    if text.is_empty() || text == "\x1b" || (stray && !after_paste && !marked) {
        return None;
    }
    (marked || after_paste || unmark(&text).chars().count() >= PASTE_KEYS).then_some(text)
}

/// A paste's text without bracketed paste's markers.
pub fn unmark(text: &str) -> String {
    let mut t = text.to_string();
    for m in MARKS {
        t = t.replace(m, "");
    }
    t.replace('\x1b', "")
}

/// Typed keys within a few milliseconds of each other that make a paste.
pub const PASTE_KEYS: usize = 3;

/// How long the keys of one paste may pause (the Windows console hands a big paste over
/// in pieces) and still be that paste.
pub const PASTE_GAP: std::time::Duration = std::time::Duration::from_millis(150);

pub fn handle(app: &mut App, ev: Event) {
    match ev {
        Event::Key(k) if k.kind != KeyEventKind::Release => {
            key(app, k);
            app.draft_check();
        }
        Event::Mouse(m) => {
            mouse(app, m);
            app.draft_check();
        }
        Event::Paste(s) => {
            if app.diffview.is_some() {
                return;
            }
            match &mut app.modal {
                None => {
                    // typed fast, `/help` arrives as a paste: a command is still a command here
                    let command = app.input.full_text().starts_with('/') || s.trim_start().starts_with('/');
                    if app.locked && !command {
                        return app.open_setup();
                    }
                    app.input.insert_paste(&s);
                    app.refresh_palette();
                }
                // a key or an address pasted into the model setup
                Some(crate::modal::Modal::Setup(st)) => {
                    st.paste(&s);
                    app.dirty = true;
                }
                // a message to a session is one line: breaks in what is pasted become spaces
                Some(crate::modal::Modal::Agents(v)) => {
                    v.paste(&s);
                    app.dirty = true;
                }
                // the filter of a picker is one line: line breaks become spaces, the words stay apart
                Some(m @ crate::modal::Modal::Picker(_)) => {
                    for c in s.chars().filter(|c| !c.is_control() || matches!(c, '\n' | '\r' | '\t')) {
                        let c = if c.is_control() { ' ' } else { c };
                        m.key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
                    }
                    app.dirty = true;
                }
                // the answer of one's own in a question
                Some(crate::modal::Modal::Question(a)) => {
                    a.paste(&s);
                    app.dirty = true;
                }
                Some(_) => {}
            }
        }
        Event::Resize(_, _) => {
            // the lines re-wrap: a finished selection no longer maps to the text it covered
            if app.selection.as_ref().is_some_and(|s| !s.dragging) {
                app.selection = None;
            }
            app.dirty = true;
        }
        _ => {}
    }
}

pub fn key(app: &mut App, k: KeyEvent) {
    app.dirty = true;
    // a permission or question dialog takes the keys first; the diff viewer waits under it
    if let Some(m) = &mut app.modal {
        // Ctrl+C is never swallowed by a dialog: a permission is denied, a question left
        // unanswered, any other dialog closed; then it acts as at the prompt (stop, arm quit)
        if app.keymap.action(&k) == Some(Action::Cancel) {
            let waits = matches!(
                m,
                crate::modal::Modal::Permission(_) | crate::modal::Modal::Question(_) | crate::modal::Modal::Trust(_)
            );
            if waits {
                let esc = KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE);
                let mut out = m.key(esc);
                if out == crate::modal::ModalOut::Nothing {
                    // a question being typed into leaves the text box first
                    out = m.key(esc);
                }
                app.modal_out(out);
            } else {
                app.modal_out(crate::modal::ModalOut::Close);
            }
            return app.ctrl_c();
        }
        let out = m.key(k);
        app.modal_out(out);
        return;
    }
    if app.diffview.is_some() {
        return app.diffview_key(k);
    }
    if crate::palette_key::is_palette_key(&k) {
        return app.palette_key();
    }
    // no model works yet: Enter or any text opens the setup instead of filling the draft
    // (a draft that starts with `/` is a command being typed: `/help` works here, see submit)
    if app.locked && !app.input.full_text().starts_with('/') {
        let plain = !k.modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT);
        match k.code {
            // `/` opens the commands window as usual, `?` is help
            KeyCode::Char('/') if plain => {}
            KeyCode::Char('?') if plain => return app.run_command(crate::commands::Command::Help),
            KeyCode::Char(_) if plain => return app.open_setup(),
            KeyCode::Enter => return app.open_setup(),
            _ => {}
        }
    }
    let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
    let shift = k.modifiers.contains(KeyModifiers::SHIFT);
    let page = app.transcript_area.height.saturating_sub(1).max(1) as i32;
    match app.keymap.action(&k) {
        Some(Action::Cancel) => return app.ctrl_c(),
        Some(Action::Quit) if app.input.is_empty() => return app.quit(),
        Some(Action::Search) => return app.open_search(),
        Some(Action::FoldAll) => return app.fold_all(),
        Some(Action::Reasoning) => return app.toggle_reasoning(),
        Some(Action::Interpreted) => return app.toggle_interpreted(),
        Some(Action::CopyLast) => return app.copy_last(false),
        Some(Action::History) => return app.open_history_search(),
        Some(Action::Paste) => return app.paste_clipboard(),
        Some(Action::Editor) => {
            app.draft_edit_request = true;
            return;
        }
        Some(Action::Files) => return app.toggle_files(app.term_width),
        Some(Action::Diff) => return app.open_diffview(),
        Some(Action::Curated) => return app.cycle_curated(),
        Some(Action::Redraw) => {
            app.force_clear = true;
            return;
        }
        Some(Action::PageUp) => return app.scroll_by(-page),
        Some(Action::PageDown) => return app.scroll_by(page),
        Some(Action::Help) => return app.run_command(crate::commands::Command::Help),
        // the mode, as long as no card is selected (then it walks the cards back)
        Some(Action::Mode) if app.selected.is_none() && app.palette.is_none() => return app.cycle_mode(),
        _ => {}
    }
    if k.code == KeyCode::Esc {
        // the commands window opened by its key: Esc gives the draft back
        if app.kept_draft.is_some() {
            return app.give_draft_back();
        }
        // with the vim keys, Esc first leaves insert mode
        if app.palette.is_none() && app.input.vim_escape() {
            return;
        }
        return app.esc();
    }
    if app.search.as_ref().is_some_and(|s| s.focused) {
        search_key(app, k);
        return;
    }
    if app.palette.is_some() {
        match k.code {
            KeyCode::Up => {
                if let Some(p) = &mut app.palette {
                    p.up();
                }
                return;
            }
            KeyCode::Down => {
                if let Some(p) = &mut app.palette {
                    p.down();
                }
                return;
            }
            KeyCode::PageUp | KeyCode::PageDown => {
                if let Some(p) = &mut app.palette {
                    p.page(k.code == KeyCode::PageDown);
                }
                return;
            }
            KeyCode::Tab => return app.palette_accept(false),
            KeyCode::Enter => {
                // a Files picker with nothing to pick has no use for Enter: the line goes out
                let is_files = app
                    .palette
                    .as_ref()
                    .is_some_and(|p| p.kind == crate::palette::PaletteKind::Files && p.current().is_some());
                return app.palette_accept(!is_files);
            }
            _ => {}
        }
    }
    // a block picked with a click or Shift+↑ takes the arrows even over a draft: ↑ ↓ walk
    // the blocks, → opens and ← folds the selected one; typing lets it go
    if app.selected.is_some() && app.palette.is_none() && !ctrl {
        match k.code {
            KeyCode::Up | KeyCode::Down => return app.select_step(k.code == KeyCode::Down),
            KeyCode::Right => return app.open_selected(true),
            KeyCode::Left => return app.open_selected(false),
            _ => {}
        }
    }
    // Tab or → on an empty draft that shows a suggested message takes it; with none shown each keeps its own use
    if matches!(k.code, KeyCode::Right | KeyCode::Tab)
        && !ctrl
        && !shift
        && !k.modifiers.contains(KeyModifiers::ALT)
        && app.accept_suggestion()
    {
        return;
    }
    if app.input.is_empty() {
        match k.code {
            KeyCode::Home => return app.scroll_to_top(),
            KeyCode::End => return app.scroll_to_end(),
            KeyCode::Tab => return app.select_next(true),
            KeyCode::BackTab if app.selected.is_some() => return app.select_next(false),
            KeyCode::Char('?') => return app.run_command(crate::commands::Command::Help),
            KeyCode::Char('n') if app.search.is_some() => return app.search_jump(!shift),
            KeyCode::Char('N') if app.search.is_some() => return app.search_jump(false),
            // ↑ with requests queued takes the last one back into the draft to edit it
            KeyCode::Up if !shift && !ctrl && app.selected.is_none() && !app.queued_msgs.is_empty() => {
                let last = app.queued_msgs.len() - 1;
                return app.unqueue(last);
            }
            // the blocks of the transcript: Shift+↑ steps in at the last one
            KeyCode::Up if shift && app.selected.is_none() => return app.select_last(),
            _ => {}
        }
    }
    // typing goes to the draft, and the selected block is let go
    if app.selected.is_some() && matches!(k.code, KeyCode::Char(_)) && !ctrl {
        app.selected = None;
    }
    // → or Tab at the end of a word that shows a ghost takes it (a picked block and the palette had the key
    // above); with no ghost shown each keeps its own use
    if matches!(k.code, KeyCode::Right | KeyCode::Tab)
        && !ctrl
        && !shift
        && !k.modifiers.contains(KeyModifiers::ALT)
        && app.selected.is_none()
        && app.palette.is_none()
    {
        if let Some(s) = app.complete_suffix() {
            app.input.accept_complete(&s);
            return app.refresh_palette();
        }
    }
    match app.input.key(k) {
        Edit::Submit => app.submit(),
        Edit::Changed => app.refresh_palette(),
        // a recalled history entry: the palette stays shut so the next Up keeps browsing
        Edit::Recalled => app.suggestion = None,
        Edit::Ignored => {}
    }
}

fn search_key(app: &mut App, k: KeyEvent) {
    let shift = k.modifiers.contains(KeyModifiers::SHIFT);
    let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
    match k.code {
        KeyCode::Enter | KeyCode::Down => app.search_jump(!shift && k.code != KeyCode::Up),
        KeyCode::Up => app.search_jump(false),
        KeyCode::Char('n') if ctrl => app.search_jump(true),
        KeyCode::Char('p') if ctrl => app.search_jump(false),
        KeyCode::Tab => {
            if let Some(s) = &mut app.search {
                s.focused = false;
            }
        }
        KeyCode::Backspace => {
            if let Some(s) = &mut app.search {
                s.query.pop();
            }
            app.rematch();
            app.search_first();
        }
        KeyCode::Char('u') if ctrl => {
            if let Some(s) = &mut app.search {
                s.query.clear();
            }
            app.rematch();
        }
        KeyCode::Char(c) if !ctrl => {
            if let Some(s) = &mut app.search {
                s.query.push(c);
            }
            app.rematch();
            app.search_first();
        }
        _ => {}
    }
}

pub fn mouse(app: &mut App, m: MouseEvent) {
    if app.modal.is_none() && app.diffview.is_some() {
        return app.diffview_mouse(m);
    }
    match m.kind {
        MouseEventKind::ScrollUp => {
            if let Some(md) = &mut app.modal {
                md.scroll_by(-(WHEEL_LINES as i32));
                app.dirty = true;
            } else if let Some(p) = &mut app.palette {
                p.up();
                app.dirty = true;
            } else {
                app.scroll_by(-(WHEEL_LINES as i32));
                app.extend_after_scroll();
            }
        }
        MouseEventKind::ScrollDown => {
            if let Some(md) = &mut app.modal {
                md.scroll_by(WHEEL_LINES as i32);
                app.dirty = true;
            } else if let Some(p) = &mut app.palette {
                p.down();
                app.dirty = true;
            } else {
                app.scroll_by(WHEEL_LINES as i32);
                app.extend_after_scroll();
            }
        }
        MouseEventKind::Drag(MouseButton::Left) => {
            if app.editor_drag {
                app.editor_drag_to(m.column, m.row);
                return;
            }
            if app.bar_drag_to(m.row) {
                return;
            }
            if app.modal.is_none() && app.palette.is_none() {
                app.drag_to(m.column, m.row);
            }
        }
        MouseEventKind::Up(MouseButton::Left) => {
            if app.editor_drag {
                app.editor_release();
                return;
            }
            if app.bar_release() {
                app.pointer_moved(m.column, m.row);
                return;
            }
            if app.modal.is_none() && app.palette.is_none() {
                app.release_at(m.column, m.row);
            }
        }
        MouseEventKind::Down(MouseButton::Left) => {
            app.dirty = true;
            if app.modal.is_some() && app.bar_press(m.column, m.row) {
                return;
            }
            if let Some(md) = &mut app.modal {
                let rows = app.picker_rows.clone();
                let out = md.click(m.column, m.row, &rows);
                app.modal_out(out);
                return;
            }
            if app.palette.is_some() {
                if let Some((_, i)) = app.palette_rows.iter().find(|(r, _)| contains(*r, m.column, m.row)) {
                    let i = *i;
                    // a Files picker with nothing to pick has no use for Enter: the line goes out
                    let is_files = app
                        .palette
                        .as_ref()
                        .is_some_and(|p| p.kind == crate::palette::PaletteKind::Files && p.current().is_some());
                    if let Some(p) = &mut app.palette {
                        p.selected = i.min(p.len().saturating_sub(1));
                    }
                    app.palette_accept(!is_files);
                    return;
                }
            }
            if app.palette.is_none() && app.bar_press(m.column, m.row) {
                return;
            }
            // the "new below" pill goes to the end
            if app.palette.is_none() && app.unseen() > 0 && contains(app.pill, m.column, m.row) {
                app.scroll_to_end();
                return;
            }
            // a queued row takes that request back into the draft; a piece of the keys row opens its view
            if app.palette.is_none() {
                if let Some((_, i)) = app.queue_rows.iter().find(|(r, _)| contains(*r, m.column, m.row)) {
                    let i = *i;
                    return app.unqueue(i);
                }
                if let Some((_, h)) = app.hint_hits.iter().find(|(r, _)| contains(*r, m.column, m.row)) {
                    let h = *h;
                    return app.open_hint(h);
                }
            }
            // the status bar: each piece with an action runs it; a separator or empty space does nothing
            if m.row == app.status_row && app.palette.is_none() {
                use crate::app::StatusHit;
                use crate::commands::Command;
                let cmd = match app.status_hit(m.column) {
                    Some(StatusHit::Model) => Command::Model(None),
                    Some(StatusHit::Effort) => Command::Effort(None),
                    Some(StatusHit::Fast) => Command::Fast(None),
                    Some(StatusHit::Mode) => Command::Permissions(None),
                    Some(StatusHit::Cost) => Command::Cost,
                    // the goal opens its own window
                    Some(StatusHit::Goal) => {
                        app.open_goal();
                        return;
                    }
                    Some(StatusHit::Gauge) => Command::Context,
                    // a separator or an empty part of the row: nothing to do
                    None => return,
                };
                app.run_command(cmd);
                return;
            }
            // the editor's text: a click puts the cursor there, a drag selects and copies
            if app.modal.is_none()
                && app.palette.is_none()
                && !app.input.is_empty()
                && contains(app.editor_area, m.column, m.row)
            {
                app.editor_press(m.column, m.row);
                return;
            }
            // Ctrl+click on a link opens it; elsewhere Ctrl+click is a free selection as before
            if m.modifiers.contains(KeyModifiers::CONTROL) && app.modal.is_none() && app.palette.is_none() {
                if let Some(url) = app.link_at(m.column, m.row) {
                    crate::links::open(&url);
                    return;
                }
            }
            // a fold toggles when the button comes up without a drag (`release_at`)
            app.press_free = m.modifiers.intersects(KeyModifiers::ALT | KeyModifiers::CONTROL);
            app.press_at(m.column, m.row);
        }
        MouseEventKind::Moved => {
            if let Some(md) = &mut app.modal {
                if md.hover(m.column, m.row) {
                    app.dirty = true;
                }
            }
            app.pointer_moved(m.column, m.row);
        }
        _ => {}
    }
}
