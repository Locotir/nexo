//! `/mcp`: the MCP servers in a window. Each row shows the server, how it is reached (stdio, http, sse),
//! where it stands (connected, needs login, failed with the reason, off) and how many tools, resources and
//! prompts it offers. Keys: Space turns the server off or on for this session, `p` does it and also
//! writes it to the configuration, `l` signs in, `r` connects again.

use crossterm::event::{KeyCode, KeyEvent};
use nexo_core::agent::{AgentCommand, McpServerInfo, McpStatus};
use ratatui::style::Modifier;
use ratatui::text::{Line, Span};

use crate::modal::ModalOut;
use crate::render::wrap::{truncate, width};
use crate::theme::Theme;

#[derive(Debug, Clone)]
pub struct McpView {
    servers: Vec<McpServerInfo>,
    pub selected: usize,
    pub scroll: usize,
}

impl McpView {
    pub fn new(servers: Vec<McpServerInfo>) -> McpView {
        McpView { servers, selected: 0, scroll: 0 }
    }

    pub fn len(&self) -> usize {
        self.servers.len()
    }

    pub fn is_empty(&self) -> bool {
        self.servers.is_empty()
    }

    pub fn current(&self) -> Option<&McpServerInfo> {
        self.servers.get(self.selected)
    }

    /// The agent listed the servers again. The cursor stays on the same server when it can.
    pub fn set_servers(&mut self, servers: Vec<McpServerInfo>) {
        let name = self.current().map(|s| s.name.clone());
        self.servers = servers;
        self.selected = name.and_then(|n| self.servers.iter().position(|s| s.name == n)).unwrap_or(0);
        self.selected = self.selected.min(self.len().saturating_sub(1));
    }

    pub fn key(&mut self, key: KeyEvent) -> ModalOut {
        let last = self.len().saturating_sub(1);
        let Some(cur) = self.current().cloned() else {
            return match key.code {
                KeyCode::Esc | KeyCode::Char('q') | KeyCode::Enter => ModalOut::Close,
                _ => ModalOut::Nothing,
            };
        };
        let server = cur.name.clone();
        let on = cur.status == McpStatus::Disabled;
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => return ModalOut::Close,
            KeyCode::Up | KeyCode::Char('k') => self.selected = self.selected.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => self.selected = (self.selected + 1).min(last),
            KeyCode::Home => self.selected = 0,
            KeyCode::End => self.selected = last,
            KeyCode::Char(' ') | KeyCode::Enter | KeyCode::Char('t') => {
                return ModalOut::Send(AgentCommand::McpToggle { server, on, persist: false });
            }
            KeyCode::Char('p') => return ModalOut::Send(AgentCommand::McpToggle { server, on, persist: true }),
            KeyCode::Char('l') => return ModalOut::Send(AgentCommand::McpLogin { server }),
            KeyCode::Char('r') => return ModalOut::Send(AgentCommand::McpReconnect { server }),
            _ => {}
        }
        ModalOut::Nothing
    }

    /// A click on the row `i` selects it.
    pub fn click_row(&mut self, i: usize) {
        if i < self.len() {
            self.selected = i;
        }
    }

    pub fn scroll_by(&mut self, delta: i32) {
        if !self.is_empty() {
            self.selected = (self.selected as i32 + delta).clamp(0, self.len() as i32 - 1) as usize;
        }
    }

    pub fn title(&self) -> String {
        let up = self.servers.iter().filter(|s| s.status == McpStatus::Connected).count();
        format!(" MCP servers · {up}/{} connected ", self.servers.len())
    }

    /// The keys, in the row pinned under the body; the longest set that fits `w`.
    pub fn keys(&self, w: usize, theme: &Theme) -> Line<'static> {
        let long: &[(&str, &str)] = &[
            ("↑↓", "move"),
            ("Space", "on/off"),
            ("p", "on/off and save"),
            ("l", "login"),
            ("r", "reconnect"),
            ("Esc", "close"),
        ];
        let mid: &[(&str, &str)] = &[("Space", "on/off"), ("p", "save"), ("l", "login"), ("r", "retry"), ("Esc", "")];
        let short: &[(&str, &str)] = &[("Space", ""), ("p", ""), ("l", ""), ("r", ""), ("Esc", "")];
        for set in [long, mid, short] {
            let mut spans: Vec<Span<'static>> = Vec::new();
            for (k, what) in set {
                if !spans.is_empty() {
                    spans.push(Span::styled(" · ", theme.dim2()));
                }
                spans.push(Span::styled((*k).to_string(), theme.style(theme.accent)));
                if !what.is_empty() {
                    spans.push(Span::styled(format!(" {what}"), theme.dim()));
                }
            }
            if spans.iter().map(|s| width(&s.content)).sum::<usize>() <= w {
                return Line::from(spans);
            }
        }
        Line::from(Span::styled("Esc closes", theme.dim()))
    }

    /// The body at `w` columns: the lines, the line each server's row starts at and the line to keep in sight.
    pub fn lines(&self, w: usize, theme: &Theme) -> (Vec<Line<'static>>, Vec<usize>, usize) {
        if self.is_empty() {
            let empty = vec![
                Line::from(Span::styled("No MCP servers", theme.dim())).centered(),
                Line::from(Span::styled("add one with `nexo mcp add`", theme.dim2())).centered(),
            ];
            return (empty, Vec::new(), 0);
        }
        let mut lines = Vec::new();
        let mut starts = Vec::new();
        for (i, s) in self.servers.iter().enumerate() {
            starts.push(lines.len());
            let sel = i == self.selected;
            lines.push(head(s, w, theme, sel));
            lines.push(Line::from(Span::styled(
                format!("      {}", truncate(&detail(s), w.saturating_sub(6))),
                theme.dim(),
            )));
        }
        let at = starts.get(self.selected).copied().unwrap_or(0);
        (lines, starts, at)
    }
}

/// `connected`, `needs login`, `failed`, `off`.
pub fn status_word(s: &McpStatus) -> &'static str {
    match s {
        McpStatus::Connected => "connected",
        McpStatus::NeedsLogin => "needs login",
        McpStatus::Failed(_) => "failed",
        McpStatus::Disabled => "off",
    }
}

/// `▸ docs   http   connected   4 tools · 2 resources · 1 prompt`
fn head(s: &McpServerInfo, w: usize, theme: &Theme, sel: bool) -> Line<'static> {
    let counts = if s.status == McpStatus::Connected {
        let res = s.resources.len() + s.templates.len();
        format!(
            "{} {} · {} {} · {} {}",
            s.tools,
            if s.tools == 1 { "tool" } else { "tools" },
            res,
            if res == 1 { "resource" } else { "resources" },
            s.prompts.len(),
            if s.prompts.len() == 1 { "prompt" } else { "prompts" }
        )
    } else {
        String::new()
    };
    let left = format!("{} {:<14} {:<6} ", if sel { "▸" } else { " " }, truncate(&s.name, 14), s.transport);
    let word = status_word(&s.status);
    let tail = format!("{word:<12} {counts}");
    let pad = w.saturating_sub(left.chars().count() + tail.chars().count());
    let base = if sel { theme.selection().add_modifier(Modifier::BOLD) } else { Default::default() };
    let tone = match &s.status {
        McpStatus::Connected => theme.success,
        McpStatus::NeedsLogin => theme.warn,
        McpStatus::Failed(_) => theme.error,
        McpStatus::Disabled => theme.dim,
    };
    let (status_span, rest) = (format!("{word:<12}"), format!(" {counts}"));
    Line::from(vec![
        Span::styled(left, base),
        Span::styled(status_span, if sel { base } else { theme.style(tone) }),
        Span::styled(rest, base),
        Span::styled(" ".repeat(pad), base),
    ])
}

/// The second line of a server: why it failed, what to do, or the commands its prompts make.
fn detail(s: &McpServerInfo) -> String {
    match &s.status {
        McpStatus::Failed(why) => why.clone(),
        McpStatus::NeedsLogin => "press l to sign in".into(),
        McpStatus::Disabled => "turned off; Space turns it on".into(),
        McpStatus::Connected if s.prompts.is_empty() => {
            if s.resources.is_empty() && s.templates.is_empty() {
                "no resources or prompts".into()
            } else {
                format!("@{}: completes resources", s.name)
            }
        }
        McpStatus::Connected => {
            s.prompts.iter().map(|p| format!("/mcp__{}__{}", s.name, p.name)).collect::<Vec<_>>().join("  ")
        }
    }
}
