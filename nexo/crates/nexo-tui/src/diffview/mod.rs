//! `/diff`: a full-screen viewer of what changed. Staged, unstaged and untracked files of the
//! repository (or, outside one, the session's own edits) in a list on the left and the selected
//! file's diff on the right, with folding, hunk jumps and an optional side-by-side mode.
//! The git calls run on a thread of their own; the view polls for the answer on every tick.

pub mod collect;
pub mod render;

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::mpsc::{channel, Receiver, TryRecvError};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use nexo_core::tool::ToolDisplay;
use ratatui::layout::Rect;
use ratatui::text::Span;

use crate::app::App;
use crate::render::blocks::Block;
use collect::{Collected, FileDiff};
use render::Row;

/// Columns at which the side-by-side mode becomes available.
pub const SIDE_MIN_W: u16 = 100;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    List,
    Body,
}

/// What a key or a click asks of the app around the view.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Out {
    None,
    Close,
    Copy(String),
}

pub type Highlighted = Vec<Vec<Vec<Span<'static>>>>;

pub struct DiffView {
    pub files: Vec<FileDiff>,
    pub note: Option<String>,
    pub error: Option<String>,
    pub loading: bool,
    pub root: PathBuf,
    rx: Option<Receiver<Collected>>,
    pub sel: usize,
    pub list_top: usize,
    pub focus: Focus,
    pub scroll: usize,
    /// The hunk n/p and the fold keys act on.
    pub hunk: usize,
    pub hscroll: usize,
    /// The wish for side by side; it applies only at [`SIDE_MIN_W`] columns.
    pub side: bool,
    pub eff_side: bool,
    pub folded: HashSet<(usize, usize)>,
    /// A one-off message that takes the footer's place until the next key.
    pub hint: Option<String>,
    pub list_area: Rect,
    pub body_area: Rect,
    pub hl: HashMap<usize, Highlighted>,
}

impl DiffView {
    fn empty() -> DiffView {
        DiffView {
            files: Vec::new(),
            note: None,
            error: None,
            loading: false,
            root: PathBuf::new(),
            rx: None,
            sel: 0,
            list_top: 0,
            focus: Focus::List,
            scroll: 0,
            hunk: 0,
            hscroll: 0,
            side: false,
            eff_side: false,
            folded: HashSet::new(),
            hint: None,
            list_area: Rect::default(),
            body_area: Rect::default(),
            hl: HashMap::new(),
        }
    }

    /// A view over data that is already there (the tests, a finished collection).
    pub fn from_collected(c: Collected) -> DiffView {
        let mut v = DiffView::empty();
        v.set(c);
        v
    }

    /// Starts the collection on a thread; the view shows "Reading changes" until it answers.
    pub fn spawn(cwd: PathBuf, session: Vec<FileDiff>) -> DiffView {
        let (tx, rx) = channel();
        let mut v = DiffView::empty();
        v.loading = true;
        v.rx = Some(rx);
        std::thread::spawn(move || {
            let _ = tx.send(collect::collect(&cwd, session));
        });
        v
    }

    fn set(&mut self, c: Collected) {
        self.files = c.files;
        self.note = c.note;
        self.error = c.error;
        self.root = c.root;
        self.loading = false;
        self.sel = 0;
        self.scroll = 0;
        self.folded.clear();
        self.hl.clear();
    }

    /// Takes the collection's answer when it came. True when the screen changed.
    pub fn poll(&mut self) -> bool {
        let Some(rx) = &self.rx else { return false };
        match rx.try_recv() {
            Ok(c) => {
                self.rx = None;
                self.set(c);
                true
            }
            Err(TryRecvError::Empty) => false,
            Err(TryRecvError::Disconnected) => {
                self.rx = None;
                self.loading = false;
                self.error = Some("could not read the changes".to_string());
                true
            }
        }
    }

    pub fn current(&self) -> Option<&FileDiff> {
        self.files.get(self.sel)
    }

    fn rows(&self) -> (Vec<Row>, Vec<usize>) {
        match self.current() {
            Some(f) => render::build_rows(f, self.sel, &self.folded, self.eff_side),
            None => (Vec::new(), Vec::new()),
        }
    }

    fn body_h(&self) -> usize {
        (self.body_area.height as usize).max(1)
    }

    fn clamp_scroll(&mut self) {
        let (rows, _) = self.rows();
        let max = rows.len().saturating_sub(self.body_h());
        self.scroll = self.scroll.min(max);
    }

    fn select(&mut self, to: usize) {
        if self.files.is_empty() {
            return;
        }
        let to = to.min(self.files.len() - 1);
        if to != self.sel {
            self.sel = to;
            self.scroll = 0;
            self.hscroll = 0;
            self.hunk = 0;
        }
    }

    fn move_sel(&mut self, d: i32) {
        let to = (self.sel as i64 + d as i64).clamp(0, self.files.len().saturating_sub(1) as i64);
        self.select(to as usize);
    }

    fn scroll_by(&mut self, d: i32) {
        self.scroll = (self.scroll as i64 + d as i64).max(0) as usize;
        self.clamp_scroll();
        self.sync_hunk();
    }

    /// After a plain scroll: the hunk the top of the body is in becomes the current one.
    fn sync_hunk(&mut self) {
        let (_, starts) = self.rows();
        self.hunk = starts.iter().rposition(|s| *s <= self.scroll).unwrap_or(0);
    }

    fn jump(&mut self, forward: bool) {
        let (_, starts) = self.rows();
        if starts.is_empty() {
            return;
        }
        self.hunk = if forward { (self.hunk + 1).min(starts.len() - 1) } else { self.hunk.saturating_sub(1) };
        self.scroll = starts[self.hunk];
        self.clamp_scroll();
    }

    fn toggle_fold(&mut self) {
        let (_, starts) = self.rows();
        if starts.is_empty() {
            return;
        }
        let h = self.hunk.min(starts.len() - 1);
        if !self.folded.remove(&(self.sel, h)) {
            self.folded.insert((self.sel, h));
        }
        let (_, starts) = self.rows();
        self.scroll = starts.get(h).copied().unwrap_or(0);
        self.clamp_scroll();
    }

    fn fold_all(&mut self) {
        let Some(f) = self.current() else { return };
        let n = f.hunks.len();
        let all = (0..n).all(|h| self.folded.contains(&(self.sel, h)));
        for h in 0..n {
            if all {
                self.folded.remove(&(self.sel, h));
            } else {
                self.folded.insert((self.sel, h));
            }
        }
        self.scroll = 0;
        self.hunk = 0;
    }

    pub fn key(&mut self, k: KeyEvent) -> Out {
        self.hint = None;
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        let page = self.body_h().saturating_sub(1).max(1) as i32;
        match k.code {
            KeyCode::Esc | KeyCode::Char('q') => return Out::Close,
            KeyCode::Char('c') if ctrl => return Out::Close,
            KeyCode::Down | KeyCode::Char('j') => match self.focus {
                Focus::List => self.move_sel(1),
                Focus::Body => self.scroll_by(1),
            },
            KeyCode::Up | KeyCode::Char('k') => match self.focus {
                Focus::List => self.move_sel(-1),
                Focus::Body => self.scroll_by(-1),
            },
            KeyCode::Tab | KeyCode::Char(']') => self.move_sel(1),
            KeyCode::BackTab | KeyCode::Char('[') => self.move_sel(-1),
            KeyCode::Enter => {
                self.focus = if self.focus == Focus::List { Focus::Body } else { Focus::List };
            }
            KeyCode::Char('n') => self.jump(true),
            KeyCode::Char('p') => self.jump(false),
            KeyCode::Char(' ') | KeyCode::Char('z') => self.toggle_fold(),
            KeyCode::Char('f') => self.fold_all(),
            KeyCode::Char('s') => {
                self.side = !self.side;
                if self.side && self.body_area.width + self.list_area.width < SIDE_MIN_W {
                    self.hint = Some(format!("side by side needs {SIDE_MIN_W} columns"));
                }
                self.scroll = 0;
            }
            KeyCode::PageDown => self.scroll_by(page),
            KeyCode::PageUp => self.scroll_by(-page),
            KeyCode::Home => {
                self.scroll = 0;
                self.hunk = 0;
            }
            KeyCode::End => {
                self.scroll = usize::MAX;
                self.clamp_scroll();
                self.sync_hunk();
            }
            KeyCode::Right | KeyCode::Char('l') => self.hscroll += 8,
            KeyCode::Left | KeyCode::Char('h') => self.hscroll = self.hscroll.saturating_sub(8),
            KeyCode::Char('y') => {
                if let Some(f) = self.current() {
                    return Out::Copy(f.as_patch());
                }
            }
            _ => {}
        }
        Out::None
    }

    pub fn mouse(&mut self, m: MouseEvent) -> Out {
        let inside = |r: Rect| m.column >= r.x && m.column < r.x + r.width && m.row >= r.y && m.row < r.y + r.height;
        match m.kind {
            MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => {
                let d = if m.kind == MouseEventKind::ScrollUp { -1 } else { 1 };
                if inside(self.list_area) {
                    self.move_sel(d);
                } else {
                    self.scroll_by(d * 3);
                }
            }
            MouseEventKind::Down(MouseButton::Left) => {
                if inside(self.list_area) {
                    self.focus = Focus::List;
                    let i = self.list_top + (m.row - self.list_area.y) as usize;
                    if i < self.files.len() {
                        self.select(i);
                    }
                } else if inside(self.body_area) {
                    self.focus = Focus::Body;
                    let (rows, _) = self.rows();
                    let at = self.scroll + (m.row - self.body_area.y) as usize;
                    if let Some(Row::Hunk(h)) = rows.get(at) {
                        let key = (self.sel, *h);
                        if !self.folded.remove(&key) {
                            self.folded.insert(key);
                        }
                        self.clamp_scroll();
                    }
                }
            }
            _ => {}
        }
        Out::None
    }
}

impl App {
    /// `/diff`: opens the viewer and starts reading the changes.
    pub fn open_diffview(&mut self) {
        let edits: Vec<(String, String, String, usize)> = self
            .blocks
            .iter()
            .filter_map(|b| match b {
                Block::Tool { output: Some(o), .. } if !o.is_error => match &o.display {
                    ToolDisplay::Diff { path, old, new, start_line } => {
                        Some((path.clone(), old.clone(), new.clone(), *start_line))
                    }
                    _ => None,
                },
                _ => None,
            })
            .collect();
        self.diffview = Some(DiffView::spawn(self.cwd.clone(), collect::session_files(&edits)));
        self.dirty = true;
    }

    pub fn diffview_key(&mut self, k: KeyEvent) {
        let out = self.diffview.as_mut().map(|v| v.key(k)).unwrap_or(Out::None);
        self.diffview_out(out);
    }

    pub fn diffview_mouse(&mut self, m: MouseEvent) {
        let out = self.diffview.as_mut().map(|v| v.mouse(m)).unwrap_or(Out::None);
        self.diffview_out(out);
    }

    fn diffview_out(&mut self, out: Out) {
        self.dirty = true;
        match out {
            Out::None => {}
            Out::Close => {
                self.diffview = None;
                self.force_clear = true;
            }
            Out::Copy(text) => {
                let msg = match crate::clipboard::copy(&text) {
                    Ok(_) => "diff copied".to_string(),
                    Err(e) => format!("copy failed: {e}"),
                };
                if let Some(v) = &mut self.diffview {
                    v.hint = Some(msg);
                }
            }
        }
    }

    /// On every tick: the answer of the collection thread.
    pub fn poll_diffview(&mut self) {
        if self.diffview.as_mut().is_some_and(|v| v.poll()) {
            self.dirty = true;
        }
    }
}
