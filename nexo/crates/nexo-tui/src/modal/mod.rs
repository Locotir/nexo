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

    /// Pasted text into the filter: one line (breaks and tabs become spaces, other control
    /// characters go), at most [`MAX_QUERY`] characters, and ranked once, not once per character.
    pub fn paste(&mut self, text: &str) {
        let room = MAX_QUERY.saturating_sub(self.query.chars().count());
        let line = text.chars().filter(|c| !c.is_control() || matches!(c, '\n' | '\r' | '\t'));
        self.query.extend(line.map(|c| if c.is_control() { ' ' } else { c }).take(room));
        self.refilter();
    }
}

/// The longest filter a picker takes from a paste.
const MAX_QUERY: usize = 256;

#[derive(Debug, Clone)]
pub struct Permission {
    pub req: PermissionRequest,
    pub scroll: usize,
    /// Button rectangles from the last draw: (rect, key that the button stands for).
    pub buttons: Vec<(Rect, char)>,
    /// How many of the suggested rules the last draw showed (the digit keys only take those).
    pub rules_shown: usize,
    /// Quiet: the list of rules for "allow always" waits behind the first `a`. Set by the draw (the keys do not know
    /// the detail setting); until then, and in full, `a` allows always at once.
    pub rules_gated: bool,
    /// The first `a` was pressed and the list shows.
    pub rules_open: bool,
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
    /// Open the conversation of the sub-agent run with this id; the modal closes.
    OpenSub(String),
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
        Modal::Permission(Permission {
            req,
            scroll: 0,
            buttons: Vec::new(),
            rules_shown: usize::MAX,
            rules_gated: false,
            rules_open: false,
        })
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
                        // quiet: the first `a` shows the rules, the next `a` (or a digit) picks one
                        if p.rules_gated && !p.rules_open && !rules.is_empty() {
                            p.rules_open = true;
                            return ModalOut::Nothing;
                        }
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
                    return j.click_row(*i);
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
}

pub fn contains(r: Rect, x: u16, y: u16) -> bool {
    // checked: a rect at the edge of the u16 grid must not overflow
    x >= r.x && (x as u32) < r.x as u32 + r.width as u32 && y >= r.y && (y as u32) < r.y as u32 + r.height as u32
}

mod permission_impl;
mod permission_view;

pub use permission_view::*;

#[cfg(test)]
mod tests;
