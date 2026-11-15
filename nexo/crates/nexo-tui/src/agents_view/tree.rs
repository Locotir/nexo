//! A forest built only from unique, acyclic parent keys reported by the supervisor.
use super::*;
use std::collections::{HashMap, HashSet};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Issue {
    None,
    Orphan,
    Cycle,
}

/// Invalid and cyclic edges are detached, but their rows remain visible and labelled.
pub(super) fn relations(rows: &[AgentRow]) -> (Vec<Option<usize>>, Vec<Issue>) {
    let mut keys = HashMap::new();
    for (i, row) in rows.iter().enumerate() {
        keys.entry(row.key.as_str()).and_modify(|v| *v = None).or_insert(Some(i));
    }
    let mut issue = vec![Issue::None; rows.len()];
    let mut parents: Vec<Option<usize>> = rows
        .iter()
        .enumerate()
        .map(|(i, r)| {
            r.parent.as_deref().and_then(|p| match keys.get(p).copied().flatten() {
                Some(j) => Some(j),
                None => {
                    issue[i] = Issue::Orphan;
                    None
                }
            })
        })
        .collect();
    for start in 0..rows.len() {
        let mut path = Vec::new();
        let mut cursor = Some(start);
        while let Some(i) = cursor {
            if let Some(at) = path.iter().position(|&j| j == i) {
                for &j in &path[at..] {
                    issue[j] = Issue::Cycle;
                }
                break;
            }
            path.push(i);
            cursor = parents[i];
        }
    }
    for i in 0..rows.len() {
        if issue[i] == Issue::Cycle {
            parents[i] = None;
        }
    }
    (parents, issue)
}

pub(super) fn ordered(rows: Vec<AgentRow>) -> Vec<AgentRow> {
    let (parents, _) = relations(&rows);
    let mut children = vec![Vec::new(); rows.len()];
    let mut roots = Vec::new();
    for (i, p) in parents.iter().enumerate() {
        if let Some(p) = p {
            children[*p].push(i);
        } else {
            roots.push(i);
        }
    }
    let mut out = Vec::with_capacity(rows.len());
    let mut stack: Vec<_> = roots.into_iter().rev().collect();
    while let Some(i) = stack.pop() {
        out.push(rows[i].clone());
        stack.extend(children[i].iter().rev().copied());
    }
    out
}

pub(super) fn visible(rows: Vec<AgentRow>, collapsed: &HashSet<String>) -> Vec<AgentRow> {
    let (parents, _) = relations(&rows);
    let mut hidden = vec![false; rows.len()];
    for i in 0..rows.len() {
        if let Some(p) = parents[i] {
            hidden[i] = hidden[p] || collapsed.contains(&rows[p].key);
        }
    }
    rows.into_iter().enumerate().filter_map(|(i, r)| (!hidden[i]).then_some(r)).collect()
}

/// Prefixes use the actual forest, including continuation bars for later siblings.
pub(super) fn prefixes(rows: &[AgentRow], ascii: bool) -> Vec<String> {
    let (parents, _) = relations(rows);
    (0..rows.len())
        .map(|i| {
            let mut chain = Vec::new();
            let mut cursor = i;
            while let Some(p) = parents[cursor] {
                let later = (cursor + 1..rows.len()).any(|j| parents[j] == Some(p));
                chain.push(later);
                cursor = p;
            }
            chain.reverse();
            let mut s = String::new();
            for (n, later) in chain.iter().enumerate() {
                let last = n + 1 == chain.len();
                s.push_str(if ascii {
                    if last {
                        if *later {
                            "+-"
                        } else {
                            "`-"
                        }
                    } else if *later {
                        "| "
                    } else {
                        "  "
                    }
                } else if last {
                    if *later {
                        "├ "
                    } else {
                        "└ "
                    }
                } else if *later {
                    "│ "
                } else {
                    "  "
                });
            }
            s
        })
        .collect()
}

impl AgentsView {
    pub(super) fn draw_tree(&mut self, f: &mut Frame, body: Rect, rows: &[AgentRow], theme: &Theme) {
        self.drawn = Layout::List;
        self.cols = 1;
        self.grid = (0..rows.len()).map(|i| vec![i]).collect();
        let height = body.height as usize;
        let max = rows.len().saturating_sub(height);
        self.scroll = self.scroll.min(max);
        if self.follow && height > 0 {
            if self.selected < self.scroll {
                self.scroll = self.selected;
            }
            if self.selected >= self.scroll + height {
                self.scroll = self.selected + 1 - height;
            }
            self.scroll = self.scroll.min(max);
        }
        let prefixes = prefixes(rows, self.ascii);
        let (_, issues) = relations(rows);
        let all = self.tree_rows();
        let (all_parents, _) = relations(&all);
        for (i, row) in rows.iter().enumerate().skip(self.scroll).take(height) {
            let has_children =
                all.iter().position(|r| r.key == row.key).is_some_and(|p| all_parents.contains(&Some(p)));
            let fold = if has_children {
                if self.collapsed.contains(&row.key) {
                    "+ "
                } else {
                    "- "
                }
            } else {
                "  "
            };
            let issue = match issues[i] {
                Issue::None => "",
                Issue::Orphan => " [orphan]",
                Issue::Cycle => " [cycle]",
            };
            let (glyph, state) = chip(row.state);
            // Reserve state and anomaly before consuming space for indentation/name.
            let head = format!("{fold}{glyph} {state}{issue} ");
            let available = (body.width as usize).saturating_sub(width(&head));
            let prefix = truncate(&prefixes[i], available.saturating_sub(3));
            let name_width = available.saturating_sub(width(&prefix));
            let text = format!("{prefix}{head}{}", truncate(&row.name, name_width));
            let r = Rect::new(body.x, body.y + (i - self.scroll) as u16, body.width, 1);
            let style = if i == self.selected {
                theme.bold(theme.accent).add_modifier(Modifier::REVERSED)
            } else {
                theme.style(chip_color(theme, row.state))
            };
            f.render_widget(Paragraph::new(truncate(&text, body.width as usize)).style(style), r);
            self.cards.push((i, r));
        }
    }
}
