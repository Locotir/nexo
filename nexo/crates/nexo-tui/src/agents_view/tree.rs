//! A forest built only from unique, acyclic parent keys reported by the supervisor.
use super::view::{needs_you, Group, Kind};
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

/// One line of the tree: a group's header or a row.
#[derive(Debug, Clone)]
pub(super) enum Item {
    Head {
        group: Group,
        count: usize,
        folded: bool,
    },
    Row {
        group: Group,
        row: Box<AgentRow>,
        kind: Kind,
        /// The tree lines in front of the glyph.
        prefix: String,
        /// Rows hang from it, folded away when `folded`.
        kids: usize,
        folded: bool,
    },
}

impl Item {
    /// What the selection holds on to while rows come and go.
    pub(super) fn id(&self) -> String {
        match self {
            Item::Head { group, .. } => format!("h:{}", group.id()),
            Item::Row { group, row, .. } => format!("r:{}:{}", group.id(), row.key),
        }
    }

    pub(super) fn row(&self) -> Option<&AgentRow> {
        match self {
            Item::Row { row, .. } => Some(row),
            Item::Head { .. } => None,
        }
    }

    pub(super) fn kind(&self) -> Option<Kind> {
        match self {
            Item::Row { kind, .. } => Some(*kind),
            Item::Head { .. } => None,
        }
    }

    pub(super) fn group(&self) -> &Group {
        match self {
            Item::Head { group, .. } | Item::Row { group, .. } => group,
        }
    }
}

impl AgentsView {
    /// Whether a group is folded now: other programs start folded, the rest open; a fold key flips it.
    pub(super) fn group_folded(&self, g: &Group) -> bool {
        g.folded_at_first() != self.flipped.contains(&g.id())
    }

    /// The rows both filters and the typed text keep, each made safe to lay out.
    pub(super) fn listed(&self) -> Vec<AgentRow> {
        let needle = self.find.as_deref().map(str::to_lowercase).filter(|s| !s.is_empty());
        let mut v: Vec<AgentRow> = self
            .rows
            .iter()
            .filter(|r| self.filter.keeps(r) && self.show.keeps(r))
            .filter(|r| {
                needle.as_ref().is_none_or(|n| {
                    [Some(&r.name), Some(&r.program), r.last_step.as_ref(), r.cwd.as_ref(), r.goal.as_ref()]
                        .into_iter()
                        .flatten()
                        .any(|s| s.to_lowercase().contains(n.as_str()))
                })
            })
            .cloned()
            .map(tidy)
            .collect();
        v.sort_by_key(|r| (r.source, r.program.to_lowercase()));
        v
    }

    /// The lines of the tree: NEEDS YOU, THIS SESSION, LANES, NEXO SESSIONS, then one group per other program.
    pub(super) fn items(&self) -> Vec<Item> {
        let rows = ordered(self.listed());
        let (parents, _) = relations(&rows);
        let kinds: Vec<Kind> = rows.iter().map(Kind::of).collect();
        let root = |mut i: usize| {
            while let Some(p) = parents[i] {
                i = p;
            }
            i
        };
        let groups: Vec<Group> = (0..rows.len()).map(|i| Group::of(kinds[root(i)], &rows[root(i)])).collect();
        let mut out = Vec::new();
        let needs: Vec<usize> = (0..rows.len()).filter(|&i| needs_you(&rows[i])).collect();
        if !needs.is_empty() {
            let folded = self.group_folded(&Group::Needs);
            out.push(Item::Head { group: Group::Needs, count: needs.len(), folded });
            if !folded {
                for i in needs {
                    out.push(Item::Row {
                        group: Group::Needs,
                        row: Box::new(rows[i].clone()),
                        kind: kinds[i],
                        prefix: String::new(),
                        kids: 0,
                        folded: false,
                    });
                }
            }
        }
        let mut order: Vec<Group> = vec![Group::This, Group::Lanes, Group::Nexo];
        let mut programs: Vec<Group> =
            groups.iter().filter(|g| matches!(g, Group::Program(_))).cloned().collect::<Vec<_>>();
        programs.sort_by_key(|g| g.title());
        programs.dedup();
        order.extend(programs);
        for g in order {
            let members: Vec<AgentRow> = (0..rows.len()).filter(|&i| groups[i] == g).map(|i| rows[i].clone()).collect();
            if members.is_empty() && !(g == Group::Lanes && self.lanes_first) {
                continue;
            }
            let folded = self.group_folded(&g);
            out.push(Item::Head { group: g.clone(), count: members.len(), folded });
            if folded {
                continue;
            }
            let (all_parents, _) = relations(&members);
            let kids_of = |key: &str| {
                members
                    .iter()
                    .position(|r| r.key == key)
                    .map_or(0, |p| all_parents.iter().filter(|q| **q == Some(p)).count())
            };
            let shown = visible(members.clone(), &self.collapsed);
            let marks = prefixes(&shown, self.ascii);
            for (row, prefix) in shown.into_iter().zip(marks) {
                let kids = kids_of(&row.key);
                let prefix = if self.ascii { prefix } else { prefix.replace('└', "╰") };
                out.push(Item::Row {
                    group: g.clone(),
                    kind: Kind::of(&row),
                    folded: kids > 0 && self.collapsed.contains(&row.key),
                    row: Box::new(row),
                    prefix,
                    kids,
                });
            }
        }
        out
    }
}

impl AgentsView {
    /// The tree line a letter for `key` lands on: its row, else the nearest ancestor drawn (a folded parent),
    /// else the header of its folded group. A row that needs the person counts in its own group, as the totals do.
    pub(super) fn landing(&self, key: &str) -> Option<usize> {
        let items = self.items();
        let rows = ordered(self.listed());
        let (parents, _) = relations(&rows);
        let mut i = rows.iter().position(|r| r.key == key)?;
        loop {
            let k = &rows[i].key;
            let drawn =
                items.iter().position(|it| it.group() != &Group::Needs && it.row().is_some_and(|r| &r.key == k));
            if drawn.is_some() {
                return drawn;
            }
            match parents[i] {
                Some(p) => i = p,
                None => break,
            }
        }
        let group = Group::of(Kind::of(&rows[i]), &rows[i]);
        items.iter().position(|it| matches!(it, Item::Head { group: g, .. } if *g == group))
    }
}
