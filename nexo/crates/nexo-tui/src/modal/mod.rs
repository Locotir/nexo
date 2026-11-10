//! Modals: a permission ask, a picker (model, session, theme, mode), help,
//! the context and usage window.
//! A modal takes every key and click until it closes.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use nexo_agent::session_scan::Period;
use nexo_core::permission::{PermissionDecision, PermissionRequest, Question};
use nexo_core::tool::ToolDisplay;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use crate::fuzzy;

pub mod setup;

use crate::render::diff;
use crate::render::wrap::{truncate, wrap_plain};
use crate::theme::Theme;
pub use setup::{Setup, SetupOut};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PickKind {
    Model,
    Session,
    /// A session whose memory joins this one.
    Memory,
    Trash,
    Theme,
    PermissionMode,
    Rewind,
    /// Where a fork starts.
    Fork,
    /// The tree of sessions.
    Tree,
    Effort,
    History,
    /// The requests queued behind the running turn: picking one edits it.
    Queue,
    /// The memory of other sessions waiting to join the next message (read only).
    Shared,
}

#[derive(Debug, Clone)]
pub struct PickItem {
    pub label: String,
    pub value: String,
    pub hint: String,
}

#[derive(Debug, Clone)]
pub struct Picker {
    pub kind: PickKind,
    pub title: String,
    pub query: String,
    pub items: Vec<PickItem>,
    pub ranked: Vec<usize>,
    pub selected: usize,
    /// The value a first Delete marked; a second Delete on it removes it (sessions only).
    pub armed: Option<String>,
}

impl Picker {
    pub fn new(kind: PickKind, title: &str, items: Vec<PickItem>, current: Option<&str>) -> Picker {
        let mut p = Picker {
            kind,
            title: title.to_string(),
            query: String::new(),
            items,
            ranked: Vec::new(),
            selected: 0,
            armed: None,
        };
        p.refilter();
        if let Some(c) = current {
            if let Some(pos) = p.ranked.iter().position(|&i| p.items[i].value == c) {
                p.selected = pos;
            }
        }
        p
    }

    /// Takes `value` out of the list, keeping the selection on the row that took its place.
    pub fn remove(&mut self, value: &str) {
        let at = self.selected;
        self.items.retain(|i| i.value != value);
        let labels: Vec<String> = self.items.iter().map(|i| format!("{} {}", i.label, i.hint)).collect();
        self.ranked = fuzzy::rank(&self.query, &labels);
        self.selected = at.min(self.ranked.len().saturating_sub(1));
    }

    pub(crate) fn refilter(&mut self) {
        let labels: Vec<String> = self.items.iter().map(|i| format!("{} {}", i.label, i.hint)).collect();
        self.ranked = fuzzy::rank(&self.query, &labels);
        self.selected = 0;
    }

    pub fn current(&self) -> Option<&PickItem> {
        self.ranked.get(self.selected).map(|&i| &self.items[i])
    }
}

#[derive(Debug, Clone)]
pub struct Permission {
    pub req: PermissionRequest,
    pub scroll: usize,
    /// Button rectangles from the last draw: (rect, key that the button stands for).
    pub buttons: Vec<(Rect, char)>,
    /// How many of the suggested rules the last draw showed (the digit keys only take those).
    pub rules_shown: usize,
}

/// A question from the model (`ask_user`), or a plan waiting for approval (`exit_plan`):
/// the options, then a last row to write an answer of one's own.
#[derive(Debug, Clone)]
pub struct Ask {
    pub q: Question,
    /// The row under the cursor; `q.options.len()` is the row to write one's own answer.
    pub selected: usize,
    /// For a question that takes several options: the ones ticked.
    pub picked: Vec<bool>,
    /// One's own answer, and whether the keys go into it.
    pub other: String,
    pub typing: bool,
    /// The question and the plan above the options scroll.
    pub scroll: usize,
}

impl Ask {
    pub fn new(q: Question) -> Ask {
        let picked = vec![false; q.options.len()];
        Ask { q, selected: 0, picked, other: String::new(), typing: false, scroll: 0 }
    }

    /// The rows: the options and the one for one's own answer.
    pub fn rows(&self) -> usize {
        self.q.options.len() + 1
    }

    fn on_other(&self) -> bool {
        self.selected == self.q.options.len()
    }

    fn answer(&self, answer: Option<String>) -> ModalOut {
        ModalOut::SendAndClose(nexo_core::agent::AgentCommand::Answer { id: self.q.id.clone(), answer })
    }

    /// Enter: the option under the cursor, the ticked ones, or what was written.
    fn choose(&mut self) -> ModalOut {
        let own = self.other.trim().to_string();
        if self.q.multi {
            let mut chosen: Vec<String> =
                self.q.options.iter().zip(&self.picked).filter(|(_, &on)| on).map(|(o, _)| o.label.clone()).collect();
            if !own.is_empty() {
                chosen.push(own);
            }
            if chosen.is_empty() {
                match self.q.options.get(self.selected) {
                    Some(o) => chosen.push(o.label.clone()),
                    None => {
                        self.typing = true;
                        return ModalOut::Nothing;
                    }
                }
            }
            return self.answer(Some(chosen.join(", ")));
        }
        match self.q.options.get(self.selected) {
            Some(o) => self.answer(Some(o.label.clone())),
            None if !own.is_empty() => self.answer(Some(own)),
            None => {
                self.typing = true;
                ModalOut::Nothing
            }
        }
    }

    fn mv(&mut self, delta: i32) {
        let n = self.rows() as i32;
        self.selected = (self.selected as i32 + delta).rem_euclid(n) as usize;
        self.typing = self.on_other() && !self.other.is_empty();
    }

    pub fn key(&mut self, key: KeyEvent) -> ModalOut {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        if self.typing {
            match key.code {
                KeyCode::Enter => return self.choose(),
                KeyCode::Esc => self.typing = false,
                KeyCode::Backspace => {
                    self.other.pop();
                }
                KeyCode::Char('u') if ctrl => self.other.clear(),
                KeyCode::Up => self.mv(-1),
                KeyCode::Down | KeyCode::Tab => self.mv(1),
                KeyCode::Char(c) if !ctrl => self.other.push(c),
                _ => {}
            }
            return ModalOut::Nothing;
        }
        match key.code {
            KeyCode::Esc => return self.answer(None),
            KeyCode::Enter => return self.choose(),
            KeyCode::Up | KeyCode::Char('k') | KeyCode::BackTab => self.mv(-1),
            KeyCode::Down | KeyCode::Char('j') | KeyCode::Tab => self.mv(1),
            KeyCode::PageUp => self.scroll = self.scroll.saturating_sub(10),
            KeyCode::PageDown => self.scroll += 10,
            KeyCode::Char(' ') if self.q.multi && !self.on_other() => {
                self.picked[self.selected] = !self.picked[self.selected];
            }
            KeyCode::Char(' ') if self.on_other() => self.typing = true,
            KeyCode::Char(c) if c.is_ascii_digit() && c != '0' => {
                let i = c as usize - '1' as usize;
                if i < self.q.options.len() {
                    self.selected = i;
                    if self.q.multi {
                        self.picked[i] = !self.picked[i];
                    } else {
                        return self.choose();
                    }
                } else if i == self.q.options.len() {
                    self.selected = i;
                    self.typing = true;
                }
            }
            // on the last row, writing starts the answer
            KeyCode::Char(c) if self.on_other() && !ctrl => {
                self.typing = true;
                self.other.push(c);
            }
            _ => {}
        }
        ModalOut::Nothing
    }

    /// Text pasted while the row for one's own answer is under the cursor: the answer is one line, so each
    /// run of line breaks and tabs inside it becomes one space, none at the end. Dropped on the option rows.
    pub fn paste(&mut self, pasted: &str) {
        if !self.on_other() {
            return;
        }
        self.typing = true;
        let mut gap = false;
        for c in pasted.chars() {
            if c.is_control() {
                gap = true;
                continue;
            }
            if gap && !self.other.is_empty() {
                self.other.push(' ');
            }
            gap = false;
            self.other.push(c);
        }
    }
}

/// "Do you trust the files in this folder?": asked once, before the first prompt, in a folder with
/// project files that trust gates. Two rows: yes (the first) and no.
#[derive(Debug, Clone, PartialEq)]
pub struct Trust {
    pub path: String,
    /// One short line on what a yes enables.
    pub enables: String,
    /// Set apart when trusting the folder means more than a project (the home folder).
    pub warning: Option<String>,
    /// 0: yes, 1: no.
    pub selected: usize,
}

impl Trust {
    pub const ROWS: [&'static str; 2] = ["Yes, trust this folder", "No, don't trust"];

    pub fn new(path: String, enables: String, warning: Option<String>) -> Trust {
        Trust { path, enables, warning, selected: 0 }
    }

    pub fn key(&mut self, key: KeyEvent) -> ModalOut {
        if key.modifiers.contains(KeyModifiers::CONTROL) {
            return ModalOut::Nothing;
        }
        match key.code {
            KeyCode::Char('y') | KeyCode::Char('Y') => ModalOut::Trust(true),
            KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => ModalOut::Trust(false),
            KeyCode::Enter => ModalOut::Trust(self.selected == 0),
            KeyCode::Char('1') => ModalOut::Trust(true),
            KeyCode::Char('2') => ModalOut::Trust(false),
            KeyCode::Up | KeyCode::Down | KeyCode::Tab | KeyCode::BackTab | KeyCode::Char('k') | KeyCode::Char('j') => {
                self.selected = 1 - self.selected;
                ModalOut::Nothing
            }
            _ => ModalOut::Nothing,
        }
    }
}

#[derive(Debug, Clone)]
pub enum Modal {
    Permission(Permission),
    /// The model asks something, or shows a plan to approve.
    Question(Box<Ask>),
    Picker(Picker),
    Help {
        scroll: usize,
    },
    /// `/model`: the guided setup.
    Setup(Box<Setup>),
    /// `/context`, `/usage`: a window with tabs.
    Info(Box<crate::overlay::Info>),
    /// `/panel`: the agent sessions, read-only.
    Agents(Box<crate::agents_view::AgentsView>),
    /// `/jobs`: the background jobs and their output.
    Jobs(Box<crate::jobs_view::JobsView>),
    /// `/todo`: the task list, grouped by state.
    Todo(Box<crate::todo_view::TodoView>),
    /// `/mcp`: the MCP servers, with their state and controls.
    Mcp(Box<crate::mcp_view::McpView>),
    /// A click on `◎ goal`: the goal, its stops and a Resume button.
    Goal(Box<crate::goal_view::GoalView>),
    /// `/sent`: the last request as it was sent, by section.
    Sent(Box<crate::sent_view::SentView>),
    /// The start in a folder not trusted yet.
    Trust(Trust),
}

/// What a key or click in a modal asks the app to do.
#[derive(Debug, Clone, PartialEq)]
pub enum ModalOut {
    Nothing,
    Close,
    Permission {
        id: String,
        decision: PermissionDecision,
    },
    Pick {
        kind: PickKind,
        value: String,
    },
    /// A command for the agent; the modal stays.
    Send(nexo_core::agent::AgentCommand),
    /// A command for the agent, and the modal closes.
    SendAndClose(nexo_core::agent::AgentCommand),
    /// Text for the clipboard; the modal stays.
    Copy(String),
    /// The answer to the trust question; the modal closes.
    Trust(bool),
}

impl From<SetupOut> for ModalOut {
    fn from(o: SetupOut) -> ModalOut {
        match o {
            SetupOut::Nothing => ModalOut::Nothing,
            SetupOut::Close => ModalOut::Close,
            SetupOut::Send(c) => ModalOut::Send(c),
            SetupOut::SendAndClose(c) => ModalOut::SendAndClose(c),
            SetupOut::Copy(t) => ModalOut::Copy(t),
        }
    }
}

/// Rows one PageUp or PageDown moves the help.
const HELP_PAGE: usize = 8;

/// The keys, in groups, each line verb first (docs/READABLE-TEXT.md).
pub const HELP: &[(&str, &[(&str, &str)])] = &[
    (
        "Stop",
        &[
            ("Esc", "stop the turn · again: clear the draft or close"),
            ("Ctrl+C", "stop · twice in 2 s: quit"),
            ("Esc Esc", "rewind to an earlier message (files too; empty draft, idle)"),
        ],
    ),
    (
        "Write",
        &[
            ("Enter", "send"),
            ("Ctrl+J / Alt+Enter", "new line"),
            ("→", "complete the word shown in grey"),
            ("↑ / ↓", "go through past requests (kept between runs)"),
            ("Ctrl+R", "search past requests"),
            ("Ctrl+Z / Alt+Z", "undo / redo"),
            ("Ctrl+A / E", "go to line start / end"),
            ("Ctrl+K / W", "delete to the end / the word before"),
            ("Ctrl+U", "cut to the start of the line"),
            ("Alt+B / F", "move one word back / forward"),
            ("Ctrl+V", "paste text, or attach a copied image"),
            ("Ctrl+G", "write in $VISUAL or $EDITOR"),
            ("Click / drag", "place the cursor / select text (copied on release)"),
        ],
    ),
    (
        "Open",
        &[
            ("/", "open commands"),
            ("@", "pick a file"),
            ("!", "run a shell command"),
            ("Ctrl+Space / Alt+P", "open commands over a draft (the draft comes back)"),
            ("Ctrl+B", "show the side panel: goal, todo, changed files"),
            ("F3", "list what the curator added to a model call (again: the call before)"),
            ("F4", "open or close the side diff panel (selected edit, else the latest)"),
            ("?  / F1", "show this help (empty draft)"),
        ],
    ),
    (
        "Read",
        &[
            ("PgUp / PgDn", "scroll"),
            ("Home / End", "go to top / bottom (empty draft)"),
            ("Ctrl+F", "search · Enter: next · Shift+Enter: previous"),
            ("Ctrl+O", "fold or open every tool group, tool card and thinking block"),
            ("Ctrl+T", "show or hide thinking"),
            ("Ctrl+P", "show code as source or as rendered"),
            ("Ctrl+Y", "copy the last reply"),
            ("Drag", "select a code block, output or diff alone; start in prose to cross blocks (copied on release)"),
            ("Double / triple click", "word / whole block or line"),
            ("Alt+Drag / Ctrl+Drag", "select freely, with no box limits"),
            ("Ctrl+Click", "open the link under the pointer in the browser"),
            ("Shift+Drag", "select with the terminal · /settings mouse turns the mouse off"),
            ("Scrollbar", "drag it, or click to jump"),
            ("Ctrl+L", "redraw the screen"),
        ],
    ),
    (
        "Blocks",
        &[
            ("Shift+↑ / Tab", "select the last block (empty draft)"),
            ("↑ / ↓", "select the previous / next block"),
            ("→ / ←", "open / fold the block · Enter: toggle"),
            ("Click", "select a block · on its first line: fold or open"),
            ("Esc", "leave the blocks"),
        ],
    ),
    (
        "Diff viewer (/diff)",
        &[
            ("↑ ↓ / j k", "file in the list · scroll when the diff has focus"),
            ("Enter / Tab", "move focus between the list and the diff"),
            ("n / p", "next / previous hunk"),
            ("z or Space / f", "fold a hunk / fold all"),
            ("s", "side by side (160 columns or more)"),
            ("PgUp PgDn Home End", "scroll the diff · ← → move sideways"),
            ("y", "copy the file's diff"),
            ("q / Esc", "close"),
        ],
    ),
    (
        "Mode",
        &[
            ("Shift+Tab", "switch mode: ask → accept-edits → plan"),
            ("/plan [text]", "plan first: the model reads, you approve, then it writes"),
            ("Status bar", "click the model: switch · the gauge: /context · the cost: /usage · the goal: its window"),
        ],
    ),
];

impl Modal {
    pub fn permission(req: PermissionRequest) -> Modal {
        Modal::Permission(Permission { req, scroll: 0, buttons: Vec::new(), rules_shown: usize::MAX })
    }

    pub fn id(&self) -> Option<&str> {
        match self {
            Modal::Permission(p) => Some(&p.req.id),
            Modal::Question(a) => Some(&a.q.id),
            _ => None,
        }
    }

    pub fn question(q: Question) -> Modal {
        Modal::Question(Box::new(Ask::new(q)))
    }

    pub fn key(&mut self, key: KeyEvent) -> ModalOut {
        match self {
            Modal::Question(a) => a.key(key),
            Modal::Permission(p) => {
                let id = p.req.id.clone();
                let rules = p.req.suggested_rules.clone();
                let answer = |decision: PermissionDecision| ModalOut::Permission { id: id.clone(), decision };
                match key.code {
                    KeyCode::Char('y') | KeyCode::Char('Y') | KeyCode::Enter => answer(PermissionDecision::Allow),
                    KeyCode::Char('a') => {
                        let rule = rules.first().cloned().unwrap_or_else(|| p.req.tool.clone());
                        answer(PermissionDecision::AllowAlways { rule, persist: false })
                    }
                    KeyCode::Char('A') => {
                        let rule = rules.first().cloned().unwrap_or_else(|| p.req.tool.clone());
                        answer(PermissionDecision::AllowAlways { rule, persist: true })
                    }
                    KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => {
                        answer(PermissionDecision::Deny { reason: None })
                    }
                    KeyCode::Char(c) if c.is_ascii_digit() && c != '0' => {
                        let i = c.to_digit(10).unwrap_or(1) as usize - 1;
                        match rules.get(i).filter(|_| i < p.rules_shown) {
                            Some(rule) => {
                                answer(PermissionDecision::AllowAlways { rule: rule.clone(), persist: false })
                            }
                            None => ModalOut::Nothing,
                        }
                    }
                    KeyCode::Up | KeyCode::Char('k') => {
                        p.scroll = p.scroll.saturating_sub(1);
                        ModalOut::Nothing
                    }
                    KeyCode::Down | KeyCode::Char('j') => {
                        p.scroll += 1;
                        ModalOut::Nothing
                    }
                    KeyCode::PageUp => {
                        p.scroll = p.scroll.saturating_sub(10);
                        ModalOut::Nothing
                    }
                    KeyCode::PageDown => {
                        p.scroll += 10;
                        ModalOut::Nothing
                    }
                    _ => ModalOut::Nothing,
                }
            }
            Modal::Picker(p) => match key.code {
                // a delete asks twice: the first press marks the row, the second removes it
                KeyCode::Delete | KeyCode::Char('d')
                    if p.kind == PickKind::Session
                        && (key.code == KeyCode::Delete || key.modifiers.contains(KeyModifiers::CONTROL)) =>
                {
                    let Some(v) = p.current().map(|it| it.value.clone()) else { return ModalOut::Nothing };
                    if p.armed.as_deref() == Some(v.as_str()) {
                        p.armed = None;
                        p.remove(&v);
                        return ModalOut::Send(nexo_core::agent::AgentCommand::DeleteSession { id: v });
                    }
                    p.armed = Some(v);
                    ModalOut::Nothing
                }
                KeyCode::Esc if p.armed.is_some() => {
                    p.armed = None;
                    ModalOut::Nothing
                }
                KeyCode::Esc => ModalOut::Close,
                KeyCode::Enter | KeyCode::Tab => match p.current() {
                    Some(it) => ModalOut::Pick { kind: p.kind, value: it.value.clone() },
                    None => ModalOut::Close,
                },
                KeyCode::Up => {
                    if !p.ranked.is_empty() {
                        p.selected = (p.selected + p.ranked.len() - 1) % p.ranked.len();
                    }
                    ModalOut::Nothing
                }
                KeyCode::Down => {
                    if !p.ranked.is_empty() {
                        p.selected = (p.selected + 1) % p.ranked.len();
                    }
                    ModalOut::Nothing
                }
                KeyCode::Backspace => {
                    p.query.pop();
                    p.refilter();
                    ModalOut::Nothing
                }
                KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    p.query.clear();
                    p.refilter();
                    ModalOut::Nothing
                }
                KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                    p.query.push(c);
                    p.refilter();
                    ModalOut::Nothing
                }
                _ => ModalOut::Nothing,
            },
            Modal::Setup(s) => s.key(key).into(),
            Modal::Agents(a) => a.key(key),
            Modal::Jobs(j) => j.key(key),
            Modal::Todo(t) => t.key(key),
            Modal::Mcp(m) => m.key(key),
            Modal::Goal(g) => g.key(key),
            Modal::Sent(v) => v.key(key),
            Modal::Trust(t) => t.key(key),
            Modal::Info(i) => match key.code {
                KeyCode::Esc | KeyCode::Enter | KeyCode::Char('q') => ModalOut::Close,
                KeyCode::Tab | KeyCode::Right | KeyCode::Char('l') => {
                    i.next(1);
                    ModalOut::Nothing
                }
                KeyCode::BackTab | KeyCode::Left | KeyCode::Char('h') => {
                    i.next(-1);
                    ModalOut::Nothing
                }
                KeyCode::Char(c @ '1'..='3') => {
                    i.set_tab(c as usize - '1' as usize);
                    ModalOut::Nothing
                }
                // the days the usage tab's insights cover
                KeyCode::Char(c)
                    if i.tab == 1
                        && !key.modifiers.contains(KeyModifiers::CONTROL)
                        && Period::from_key(c).is_some() =>
                {
                    i.period = Period::from_key(c).unwrap_or(i.period);
                    ModalOut::Nothing
                }
                KeyCode::Up | KeyCode::Char('k') => {
                    i.scroll = i.scroll.saturating_sub(1);
                    ModalOut::Nothing
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    i.scroll += 1;
                    ModalOut::Nothing
                }
                KeyCode::PageUp => {
                    i.scroll = i.scroll.saturating_sub(10);
                    ModalOut::Nothing
                }
                KeyCode::PageDown => {
                    i.scroll += 10;
                    ModalOut::Nothing
                }
                _ => ModalOut::Nothing,
            },
            Modal::Help { scroll } => match key.code {
                KeyCode::Esc | KeyCode::Enter | KeyCode::Char('q') | KeyCode::Char('?') => ModalOut::Close,
                KeyCode::Up | KeyCode::Char('k') => {
                    *scroll = scroll.saturating_sub(1);
                    ModalOut::Nothing
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    *scroll += 1;
                    ModalOut::Nothing
                }
                KeyCode::PageUp => {
                    *scroll = scroll.saturating_sub(HELP_PAGE);
                    ModalOut::Nothing
                }
                KeyCode::PageDown => {
                    *scroll += HELP_PAGE;
                    ModalOut::Nothing
                }
                KeyCode::Home => {
                    *scroll = 0;
                    ModalOut::Nothing
                }
                // the painter cuts the offset to the end
                KeyCode::End => {
                    *scroll = usize::MAX / 2;
                    ModalOut::Nothing
                }
                _ => ModalOut::Nothing,
            },
        }
    }

    /// A click at (x, y): buttons of a permission dialog, rows of a picker.
    pub fn click(&mut self, x: u16, y: u16, rows: &[(Rect, usize)]) -> ModalOut {
        match self {
            Modal::Permission(p) => {
                let hit = p.buttons.iter().find(|(r, _)| contains(*r, x, y)).map(|(_, k)| *k);
                match hit {
                    Some(k) => self.key(KeyEvent::new(KeyCode::Char(k), KeyModifiers::NONE)),
                    None => ModalOut::Nothing,
                }
            }
            Modal::Question(a) => {
                if let Some((_, i)) = rows.iter().find(|(r, _)| contains(*r, x, y)) {
                    a.selected = *i;
                    if a.q.multi && *i < a.q.options.len() {
                        a.picked[*i] = !a.picked[*i];
                        return ModalOut::Nothing;
                    }
                    return a.key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
                }
                ModalOut::Nothing
            }
            Modal::Picker(p) => {
                if let Some((_, i)) = rows.iter().find(|(r, _)| contains(*r, x, y)) {
                    p.selected = *i;
                    return self.key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
                }
                ModalOut::Nothing
            }
            Modal::Setup(s) => match rows.iter().find(|(r, _)| contains(*r, x, y)) {
                Some((_, i)) => s.click_row(*i).into(),
                None => ModalOut::Nothing,
            },
            Modal::Trust(t) => match rows.iter().find(|(r, _)| contains(*r, x, y)) {
                Some((_, i)) => {
                    t.selected = *i;
                    t.key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
                }
                None => ModalOut::Nothing,
            },
            Modal::Help { .. } => ModalOut::Close,
            Modal::Agents(a) => a.click(x, y),
            Modal::Jobs(j) => {
                if let Some((_, i)) = rows.iter().find(|(r, _)| contains(*r, x, y)) {
                    j.click_row(*i);
                }
                ModalOut::Nothing
            }
            Modal::Todo(t) => match rows.iter().find(|(r, _)| contains(*r, x, y)) {
                Some((_, i)) => t.click_row(*i),
                None => ModalOut::Nothing,
            },
            Modal::Mcp(m) => {
                if let Some((_, i)) = rows.iter().find(|(r, _)| contains(*r, x, y)) {
                    m.click_row(*i);
                }
                ModalOut::Nothing
            }
            // the Resume button
            Modal::Goal(g) => match rows.iter().find(|(r, _)| contains(*r, x, y)) {
                Some(_) if g.can_resume() => g.resume(),
                _ => ModalOut::Nothing,
            },
            // a read-only window: the wheel and the keys move it
            Modal::Sent(_) => ModalOut::Nothing,
            Modal::Info(i) => {
                if contains(i.close, x, y) || !contains(i.window, x, y) {
                    return ModalOut::Close;
                }
                if let Some(t) = i.tabs.iter().position(|r| contains(*r, x, y)) {
                    i.set_tab(t);
                }
                ModalOut::Nothing
            }
        }
    }

    /// The pointer moved to (x, y): a button under it lights up. True when that changed.
    pub fn hover(&mut self, x: u16, y: u16) -> bool {
        match self {
            Modal::Agents(a) => a.on_hover(x, y),
            Modal::Info(i) => {
                let on = contains(i.close, x, y);
                std::mem::replace(&mut i.hover, on) != on
            }
            _ => false,
        }
    }

    pub fn scroll_by(&mut self, delta: i32) {
        match self {
            Modal::Permission(p) => p.scroll = (p.scroll as i32 + delta).max(0) as usize,
            Modal::Question(a) => a.scroll = (a.scroll as i32 + delta).max(0) as usize,
            Modal::Help { scroll } => *scroll = (*scroll as i32 + delta).max(0) as usize,
            Modal::Picker(p) => {
                if !p.ranked.is_empty() {
                    p.selected = ((p.selected as i32 + delta).rem_euclid(p.ranked.len() as i32)) as usize;
                }
            }
            Modal::Setup(s) => s.scroll_by(delta),
            Modal::Info(i) => i.scroll = (i.scroll as i32 + delta).max(0) as usize,
            Modal::Agents(a) => a.scroll_by(delta),
            Modal::Jobs(j) => j.scroll_by(delta),
            Modal::Todo(t) => t.scroll_by(delta),
            Modal::Mcp(m) => m.scroll_by(delta),
            Modal::Goal(g) => g.scroll_by(delta),
            Modal::Sent(v) => v.scroll_by(delta),
            Modal::Trust(_) => {}
        }
    }

    /// Scrolls the body so that its line `off` is the first in sight (a dragged scrollbar).
    /// A list scrolls by its selection, so the selection moves to the row that brings the
    /// view there; `viewport` is the body's height, `first` the body line the list starts at.
    pub fn set_scroll(&mut self, off: usize, viewport: usize, first: usize) {
        fn pick(list: &mut Picker, off: usize, viewport: usize, first: usize) {
            if list.ranked.is_empty() {
                return;
            }
            let row = if off == 0 {
                list.selected.min(viewport.saturating_sub(first + 1))
            } else {
                (off + viewport).saturating_sub(first + 1)
            };
            list.selected = row.min(list.ranked.len() - 1);
        }
        match self {
            Modal::Permission(p) => p.scroll = off,
            Modal::Question(a) => a.scroll = off,
            Modal::Help { scroll } => *scroll = off,
            Modal::Info(i) => i.scroll = off,
            Modal::Agents(a) => a.scroll = off,
            Modal::Jobs(j) => j.scroll = off,
            Modal::Todo(t) => t.scroll_to_line(off),
            Modal::Mcp(m) => m.scroll = off,
            Modal::Goal(g) => g.scroll = off,
            Modal::Sent(v) => v.scroll = off,
            Modal::Picker(p) => pick(p, off, viewport, first),
            Modal::Setup(s) => pick(&mut s.list, off, viewport, first),
            Modal::Trust(_) => {}
        }
    }

    /// The body lines of a permission dialog at `w` columns.
    pub fn permission_lines(req: &PermissionRequest, w: usize, theme: &Theme) -> Vec<Line<'static>> {
        Self::permission_lines_with(req, w, theme, false, true)
    }

    /// `compact`: without the blank lines between the parts, for a screen too short to show all.
    pub fn permission_lines_with(
        req: &PermissionRequest,
        w: usize,
        theme: &Theme,
        compact: bool,
        with_rules: bool,
    ) -> Vec<Line<'static>> {
        let gap = |lines: &mut Vec<Line<'static>>| {
            if !compact {
                lines.push(Line::default());
            }
        };
        let mut lines = vec![Line::from(vec![
            Span::styled(req.tool.clone(), Style::default().add_modifier(Modifier::BOLD)),
            Span::raw("  "),
            Span::raw(truncate(
                req.summary
                    .strip_prefix(req.tool.as_str())
                    .map(|r| r.trim_start_matches(':').trim_start())
                    .unwrap_or(req.summary.as_str()),
                w.saturating_sub(req.tool.len() + 2),
            )),
        ])];
        gap(&mut lines);
        match &req.detail {
            ToolDisplay::Diff { path, old, new, start_line } => {
                let (added, removed) = diff::stats(old, new);
                lines.push(Line::from(vec![
                    Span::styled(truncate(path, w.saturating_sub(12)), theme.style(theme.accent)),
                    Span::raw("  "),
                    Span::styled(format!("+{added}"), theme.style(theme.success)),
                    Span::raw(" "),
                    Span::styled(format!("-{removed}"), theme.style(theme.error)),
                ]));
                lines.extend(diff::unified(old, new, *start_line, theme));
            }
            ToolDisplay::Shell { command, .. } => {
                // All of it, wrapped: what is being allowed must be seen whole.
                let dollar = [Span::styled("$ ", theme.style(theme.accent))];
                for (i, l) in command.lines().enumerate() {
                    let first: &[Span<'static>] = if i == 0 { &dollar } else { &[Span::raw("  ")] };
                    lines.extend(crate::render::wrap::wrap_runs(
                        &[(l.to_string(), Style::default())],
                        w,
                        first,
                        &[Span::raw("  ")],
                    ));
                }
            }
            ToolDisplay::File { path, text, .. } => {
                lines.push(Line::from(Span::styled(path.clone(), theme.style(theme.accent))));
                if is_markdown_path(path) {
                    lines.extend(crate::render::markdown::md_lines(text, w, theme, Style::default(), false, &[]));
                } else {
                    lines.extend(wrap_plain(text, w, Style::default(), &[]));
                }
            }
            ToolDisplay::List { items } | ToolDisplay::Remembered { items, .. } => {
                for it in items {
                    lines.extend(wrap_plain(it, w, Style::default(), &[]));
                }
            }
            ToolDisplay::Todo { items } => {
                for it in items {
                    lines.extend(crate::render::todo::todo_row(it, w, theme, Default::default()));
                }
            }
            ToolDisplay::Plain => {}
        }
        if with_rules {
            gap(&mut lines);
            lines.extend(Self::permission_rules(req, theme, true));
        }
        lines
    }

    /// The rules the window offers for "allow always": a header (when `header`) and one line each.
    pub fn permission_rules(req: &PermissionRequest, theme: &Theme, header: bool) -> Vec<Line<'static>> {
        let mut lines = Vec::new();
        if req.suggested_rules.is_empty() {
            return lines;
        }
        if header {
            lines.push(Line::from(Span::styled("Allow always with a rule:", theme.dim())));
        }
        for (i, r) in req.suggested_rules.iter().take(9).enumerate() {
            lines.push(Line::from(vec![
                Span::styled(format!("  {}  ", i + 1), theme.style(theme.accent)),
                Span::raw(r.clone()),
            ]));
        }
        lines
    }

    /// The help at `w` columns: keys and commands in two columns, the descriptions wrapped
    /// under their own column (a long command takes a line of its own).
    pub fn help_lines(theme: &Theme, w: usize) -> Vec<Line<'static>> {
        let bold = Style::default().add_modifier(Modifier::BOLD);
        let mut lines = Vec::new();
        for (i, (group, keys)) in HELP.iter().enumerate() {
            if i > 0 {
                lines.push(Line::default());
            }
            lines.push(Line::from(Span::styled(group.to_string(), bold)));
            for (k, d) in keys.iter() {
                lines.extend(two_columns(
                    Span::styled(k.to_string(), theme.style(theme.accent)),
                    d,
                    &[],
                    (w / 3).clamp(12, 19),
                    w,
                    theme,
                ));
            }
        }
        lines.push(Line::default());
        lines.push(Line::from(Span::styled("Commands", bold)));
        for s in crate::commands::COMMANDS {
            let left = if s.hint.is_empty() { format!("/{}", s.name) } else { format!("/{} {}", s.name, s.hint) };
            let also = if s.aliases.is_empty() {
                Vec::new()
            } else {
                let a: Vec<String> = s.aliases.iter().map(|a| format!("/{a}")).collect();
                vec![Span::styled(format!("  ({})", a.join(" ")), theme.dim())]
            };
            lines.extend(two_columns(
                Span::styled(left, theme.style(theme.accent)),
                s.description,
                &also,
                24,
                w,
                theme,
            ));
        }
        lines
    }
}

/// The permission buttons that fit `avail` columns: (label, key), and the gap between them.
/// Full words when there is room, then closer together, then the keys alone.
pub fn permission_buttons(avail: usize) -> (Vec<(String, char)>, usize) {
    let sets: [(&[(&str, char)], usize); 4] = [
        (&[("[y] allow", 'y'), ("[a] always", 'a'), ("[n] deny", 'n')], 3),
        (&[("[y] allow", 'y'), ("[a] always", 'a'), ("[n] deny", 'n')], 1),
        (&[("[y] yes", 'y'), ("[a] always", 'a'), ("[n] no", 'n')], 1),
        (&[("[y]", 'y'), ("[a]", 'a'), ("[n]", 'n')], 1),
    ];
    for (set, gap) in sets {
        let need: usize = set.iter().map(|(l, _)| l.chars().count()).sum::<usize>() + gap * (set.len() - 1);
        if need <= avail {
            return (set.iter().map(|(l, k)| (l.to_string(), *k)).collect(), gap);
        }
    }
    (vec![("[y]".to_string(), 'y'), ("[a]".to_string(), 'a'), ("[n]".to_string(), 'n')], 1)
}

/// Whether `path` names a markdown file.
fn is_markdown_path(path: &str) -> bool {
    let p = path.to_ascii_lowercase();
    p.ends_with(".md") || p.ends_with(".markdown")
}

/// `left` in a column `col` wide, `desc` wrapped beside it; `tail` after the last line.
fn two_columns(
    left: Span<'static>,
    desc: &str,
    tail: &[Span<'static>],
    col: usize,
    w: usize,
    theme: &Theme,
) -> Vec<Line<'static>> {
    let desc_w = w.saturating_sub(col).max(12);
    let mut body = crate::render::markdown::md_lines(desc, desc_w, theme, Style::default(), false, &[]);
    if body.is_empty() {
        body.push(Line::default());
    }
    if let Some(last) = body.last_mut() {
        let used: usize = last.spans.iter().map(|s| crate::render::wrap::width(&s.content)).sum();
        let tail_w: usize = tail.iter().map(|s| crate::render::wrap::width(&s.content)).sum();
        if used + tail_w <= desc_w {
            last.spans.extend(tail.iter().cloned());
        } else if !tail.is_empty() {
            body.push(Line::from(tail.to_vec()));
        }
    }
    let lw = crate::render::wrap::width(&left.content);
    let mut out = Vec::new();
    let mut rows = body.into_iter();
    if lw < col {
        let mut first = vec![left.clone(), Span::raw(" ".repeat(col - lw))];
        first.extend(rows.next().map(|l| l.spans).unwrap_or_default());
        out.push(Line::from(first));
    } else {
        out.push(Line::from(left));
    }
    for l in rows {
        let mut v = vec![Span::raw(" ".repeat(col))];
        v.extend(l.spans);
        out.push(Line::from(v));
    }
    out
}

pub fn contains(r: Rect, x: u16, y: u16) -> bool {
    // checked: a rect at the edge of the u16 grid must not overflow
    x >= r.x && (x as u32) < r.x as u32 + r.width as u32 && y >= r.y && (y as u32) < r.y as u32 + r.height as u32
}

#[cfg(test)]
mod tests {
    use super::*;
    use nexo_core::tool::ToolKind;

    fn req() -> PermissionRequest {
        PermissionRequest {
            id: "p1".into(),
            tool: "bash".into(),
            kind: ToolKind::Exec,
            summary: "git status".into(),
            detail: ToolDisplay::Shell {
                command: "git status".into(),
                stdout: String::new(),
                stderr: String::new(),
                exit_code: None,
            },
            suggested_rules: vec!["bash(git *)".into(), "bash".into()],
        }
    }

    #[test]
    fn help_files_and_todos_in_the_dialog_show_no_marks() {
        let th = Theme::dark();
        let help =
            Modal::help_lines(&th, 80).iter().map(crate::render::markdown::text_of).collect::<Vec<_>>().join("\n");
        assert!(!help.contains('`'), "{help}");
        let mut r = req();
        r.detail = ToolDisplay::File { path: "notes.md".into(), text: "# Title\nsome **bold**".into(), start_line: 1 };
        let t = Modal::permission_lines(&r, 60, &th)
            .iter()
            .map(crate::render::markdown::text_of)
            .collect::<Vec<_>>()
            .join("\n");
        assert!(t.contains("Title") && !t.contains("# Title") && !t.contains("**"), "{t}");
        r.detail = ToolDisplay::File { path: "a.txt".into(), text: "# Title".into(), start_line: 1 };
        let t = Modal::permission_lines(&r, 60, &th)
            .iter()
            .map(crate::render::markdown::text_of)
            .collect::<Vec<_>>()
            .join("\n");
        assert!(t.contains("# Title"), "{t}");
    }

    #[test]
    fn permission_keys_answer() {
        let mut m = Modal::permission(req());
        assert_eq!(
            m.key(KeyEvent::new(KeyCode::Char('y'), KeyModifiers::NONE)),
            ModalOut::Permission { id: "p1".into(), decision: PermissionDecision::Allow }
        );
        assert_eq!(
            m.key(KeyEvent::new(KeyCode::Char('2'), KeyModifiers::NONE)),
            ModalOut::Permission {
                id: "p1".into(),
                decision: PermissionDecision::AllowAlways { rule: "bash".into(), persist: false }
            }
        );
        assert_eq!(
            m.key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)),
            ModalOut::Permission { id: "p1".into(), decision: PermissionDecision::Deny { reason: None } }
        );
        let lines = Modal::permission_lines(&req(), 40, &Theme::dark());
        assert!(lines.iter().any(|l| crate::render::markdown::text_of(l).contains("bash(git *)")));
    }

    #[test]
    fn a_long_command_is_shown_whole_in_the_permission_dialog() {
        let mut r = req();
        let cmd = "rm -rf build && cargo build --release 2>&1 | tail -n 5 && echo the-very-end";
        r.detail =
            ToolDisplay::Shell { command: cmd.into(), stdout: String::new(), stderr: String::new(), exit_code: None };
        let lines = Modal::permission_lines(&r, 30, &Theme::dark());
        let text: Vec<String> = lines.iter().map(crate::render::markdown::text_of).collect();
        assert!(text.iter().all(|l| crate::render::wrap::width(l) <= 30));
        let joined = text.join(" ");
        assert!(joined.contains("tail -n 5") && joined.contains("the-very-end"));
        assert_eq!(text.iter().filter(|l| l.starts_with("$ ")).count(), 1);
    }

    #[test]
    fn a_dragged_scrollbar_sets_the_body_or_moves_a_lists_selection() {
        let mut m = Modal::Help { scroll: 0 };
        m.set_scroll(7, 10, 0);
        assert!(matches!(m, Modal::Help { scroll: 7 }));
        let items: Vec<PickItem> = (0..40)
            .map(|i| PickItem { label: format!("item {i}"), value: i.to_string(), hint: String::new() })
            .collect();
        let mut m = Modal::Picker(Picker::new(PickKind::Model, "Model", items, None));
        // a body of 10 rows, the list from row 2: an offset of 5 needs the selection on row 14,
        // which is the list's 12th item
        m.set_scroll(5, 10, 2);
        let Modal::Picker(p) = &m else { unreachable!() };
        assert_eq!(p.selected, 12);
        let row = p.selected + 2;
        assert_eq!(row + 1 - 10, 5, "the draw's own rule puts the view at the offset");
        // back to the top: the selection comes into the first page
        m.set_scroll(0, 10, 2);
        let Modal::Picker(p) = &m else { unreachable!() };
        assert!(p.selected + 2 < 10);
        // far past the end: the last item
        m.set_scroll(500, 10, 2);
        let Modal::Picker(p) = &m else { unreachable!() };
        assert_eq!(p.selected, 39);
    }

    #[test]
    fn the_permission_buttons_shrink_to_fit_and_always_keep_three_keys() {
        for avail in [60usize, 33, 32, 31, 25, 12, 11, 5] {
            let (b, gap) = permission_buttons(avail);
            let used: usize = b.iter().map(|(l, _)| l.chars().count()).sum::<usize>() + gap * (b.len() - 1);
            assert_eq!(b.iter().map(|(_, k)| *k).collect::<String>(), "yan");
            if avail >= 11 {
                assert!(used <= avail, "{avail}: {used}");
            }
        }
        assert_eq!(permission_buttons(60).0[0].0, "[y] allow");
        assert_eq!(permission_buttons(11).0[1].0, "[a]");
    }

    #[test]
    fn a_diff_permission_says_how_many_lines_it_adds_and_removes() {
        let mut r = req();
        r.detail =
            ToolDisplay::Diff { path: "src/a.rs".into(), old: "a\nb\n".into(), new: "a\nc\nd\n".into(), start_line: 1 };
        let text: Vec<String> =
            Modal::permission_lines(&r, 40, &Theme::dark()).iter().map(crate::render::markdown::text_of).collect();
        assert!(text.iter().any(|l| l.starts_with("src/a.rs") && l.contains("+2 -1")), "{text:?}");
        // a long path gives way to the numbers
        r.detail = ToolDisplay::Diff { path: "x/".repeat(40), old: "a\n".into(), new: "b\n".into(), start_line: 1 };
        let text: Vec<String> =
            Modal::permission_lines(&r, 30, &Theme::dark()).iter().map(crate::render::markdown::text_of).collect();
        assert!(text.iter().all(|l| crate::render::wrap::width(l) <= 30), "{text:?}");
        assert!(text.iter().any(|l| l.contains("+1 -1")), "{text:?}");
    }

    #[test]
    fn a_diff_permission_numbers_its_lines_from_where_the_text_sits_in_the_file() {
        let mut r = req();
        r.detail = ToolDisplay::Diff { path: "src/a.rs".into(), old: "b\n".into(), new: "c\n".into(), start_line: 40 };
        let text: Vec<String> =
            Modal::permission_lines(&r, 40, &Theme::dark()).iter().map(crate::render::markdown::text_of).collect();
        assert!(text.iter().any(|l| l.contains("@@ -40,1 +40,1 @@")), "{text:?}");
        assert!(text.iter().any(|l| l.trim_start().starts_with("40 - b")), "{text:?}");
        assert!(text.iter().any(|l| l.trim_start().starts_with("40 + c")), "{text:?}");
    }

    #[test]
    fn picker_filters_and_picks() {
        let items = vec![
            PickItem {
                label: "deepseek/deepseek-chat".into(),
                value: "deepseek/deepseek-chat".into(),
                hint: String::new(),
            },
            PickItem { label: "openai/gpt".into(), value: "openai/gpt".into(), hint: String::new() },
        ];
        let mut m = Modal::Picker(Picker::new(PickKind::Model, "Model", items, Some("openai/gpt")));
        if let Modal::Picker(p) = &m {
            assert_eq!(p.current().unwrap().value, "openai/gpt");
        }
        for c in "deep".chars() {
            m.key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
        }
        assert_eq!(
            m.key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
            ModalOut::Pick { kind: PickKind::Model, value: "deepseek/deepseek-chat".into() }
        );
    }
}
