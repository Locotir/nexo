//! From blocks to nested statements: if/else, loops, switch, break/continue, and goto when nothing
//! else fits.

use std::collections::{BTreeSet, HashMap, HashSet};

use super::flow::preds_of;
use super::ir::*;
use super::lift::Lifted;

#[derive(Clone, Debug, PartialEq)]
pub enum Node {
    Stmt(u64, Stmt),
    If(u64, Expr, Vec<Node>, Vec<Node>),
    /// `while (1) { ... }`
    Loop(Vec<Node>),
    While(Expr, Vec<Node>),
    DoWhile(Vec<Node>, Expr),
    /// init, condition, step, body
    For(Box<Node>, Expr, Box<Node>, Vec<Node>),
    /// value, arms (case values, body), default
    Switch(u64, Expr, Vec<(Vec<u64>, Vec<Node>)>, Option<Vec<Node>>),
    Break,
    Continue,
    Goto(usize),
    Label(usize),
    Return(u64, Option<Expr>),
    TailCall(u64, Expr),
    Indirect(u64, Expr),
    Stop(u64),
}

impl Node {
    pub fn is_terminal(&self) -> bool {
        match self {
            Node::Break
            | Node::Continue
            | Node::Goto(_)
            | Node::Return(..)
            | Node::TailCall(..)
            | Node::Indirect(..)
            | Node::Stop(_) => true,
            Node::If(_, _, a, b) => ends_terminal(a) && ends_terminal(b),
            _ => false,
        }
    }
}

pub fn ends_terminal(v: &[Node]) -> bool {
    v.last().is_some_and(Node::is_terminal)
}

// ---------------------------------------------------------------------------------------------
// Conditions: merge `a && b` and `a || b` that were compiled to two branches.

pub fn merge_conds(f: &mut Lifted) {
    loop {
        let preds = preds_of(f);
        let mut done = false;
        for p in 0..f.blocks.len() {
            let Term::Branch(c, t, e) = f.blocks[p].term.clone() else { continue };
            for (x_is_else, x) in [(true, e), (false, t)] {
                if x == p || x == 0 || preds[x].len() != 1 || !f.blocks[x].stmts.is_empty() {
                    continue;
                }
                let Term::Branch(c2, t2, e2) = f.blocks[x].term.clone() else { continue };
                // orient so that x is the "else" side of p
                let (cond, target, _other) = if x_is_else { (c.clone(), t, e) } else { (c.clone().not(), e, t) };
                let new = if t2 == target {
                    Some((Expr::Bin(Bin::LOr, Box::new(cond.clone()), Box::new(c2.clone())), target, e2))
                } else if e2 == target {
                    Some((Expr::Bin(Bin::LOr, Box::new(cond.clone()), Box::new(c2.clone().not())), target, t2))
                } else {
                    None
                };
                let Some((nc, tt, ee)) = new else {
                    // x is on the "then" side of the original orientation: a && b
                    continue;
                };
                f.blocks[p].term = Term::Branch(nc, tt, ee);
                f.blocks[x].term = Term::Stop;
                done = true;
                break;
            }
            if done {
                break;
            }
            // a && b: p: c -> x, !c -> other; x: c2 -> t2, !c2 -> other
            for (x_is_then, x) in [(true, t), (false, e)] {
                if x == p || x == 0 || preds[x].len() != 1 || !f.blocks[x].stmts.is_empty() {
                    continue;
                }
                let Term::Branch(c2, t2, e2) = f.blocks[x].term.clone() else { continue };
                let (cond, other) = if x_is_then { (c.clone(), e) } else { (c.clone().not(), t) };
                let new = if e2 == other {
                    Some((Expr::Bin(Bin::LAnd, Box::new(cond.clone()), Box::new(c2.clone())), t2, other))
                } else if t2 == other {
                    Some((Expr::Bin(Bin::LAnd, Box::new(cond.clone()), Box::new(c2.clone().not())), e2, other))
                } else {
                    None
                };
                if let Some((nc, tt, ee)) = new {
                    f.blocks[p].term = Term::Branch(nc, tt, ee);
                    f.blocks[x].term = Term::Stop;
                    done = true;
                    break;
                }
            }
            if done {
                break;
            }
        }
        super::flow::prune(f);
        if !done {
            break;
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Dominators

/// Immediate dominators by the iterative algorithm; `None` for nodes that cannot be reached.
fn idoms(n: usize, succs: &[Vec<usize>], entry: usize) -> (Vec<Option<usize>>, Vec<usize>) {
    // reverse post order
    let mut order: Vec<usize> = Vec::new();
    let mut seen = vec![false; n];
    let mut stack: Vec<(usize, usize)> = vec![(entry, 0)];
    seen[entry] = true;
    while let Some((b, i)) = stack.pop() {
        if i < succs[b].len() {
            stack.push((b, i + 1));
            let s = succs[b][i];
            if !seen[s] {
                seen[s] = true;
                stack.push((s, 0));
            }
        } else {
            order.push(b);
        }
    }
    order.reverse();
    let mut rpo = vec![usize::MAX; n];
    for (i, &b) in order.iter().enumerate() {
        rpo[b] = i;
    }
    let mut preds: Vec<Vec<usize>> = vec![Vec::new(); n];
    for (b, ss) in succs.iter().enumerate() {
        if rpo[b] == usize::MAX {
            continue;
        }
        for &s in ss {
            preds[s].push(b);
        }
    }
    let mut idom: Vec<Option<usize>> = vec![None; n];
    idom[entry] = Some(entry);
    let mut changed = true;
    while changed {
        changed = false;
        for &b in order.iter().skip(1) {
            let mut new: Option<usize> = None;
            for &p in &preds[b] {
                if idom[p].is_none() {
                    continue;
                }
                new = Some(match new {
                    None => p,
                    Some(q) => {
                        let (mut x, mut y) = (p, q);
                        while x != y {
                            while rpo[x] > rpo[y] {
                                x = idom[x].unwrap_or(entry);
                            }
                            while rpo[y] > rpo[x] {
                                y = idom[y].unwrap_or(entry);
                            }
                        }
                        x
                    }
                });
            }
            if new != idom[b] {
                idom[b] = new;
                changed = true;
            }
        }
    }
    (idom, rpo)
}

struct Ctx<'a> {
    f: &'a Lifted,
    idom: Vec<Option<usize>>,
    ipdom: Vec<Option<usize>>,
    rpo: Vec<usize>,
    loops: HashMap<usize, BTreeSet<usize>>,
    emitted: HashSet<usize>,
    entered: HashSet<usize>,
    /// The loop header whose body is being started: it is not a continue yet.
    opening: Option<usize>,
    labels: HashSet<usize>,
    scopes: Vec<Scope>,
}

#[derive(Clone)]
enum Scope {
    Loop { header: usize, exit: Option<usize> },
    Switch,
}

impl Ctx<'_> {
    fn dominates(&self, a: usize, mut b: usize) -> bool {
        loop {
            if a == b {
                return true;
            }
            match self.idom[b] {
                Some(p) if p != b => b = p,
                _ => return false,
            }
        }
    }

    fn innermost_loop(&self) -> Option<(usize, Option<usize>)> {
        self.scopes.iter().rev().find_map(|s| match s {
            Scope::Loop { header, exit } => Some((*header, *exit)),
            _ => None,
        })
    }

    fn in_loop_set(&self, b: usize) -> bool {
        match self.innermost_loop() {
            Some((h, _)) => self.loops.get(&h).is_some_and(|s| s.contains(&b)),
            None => true,
        }
    }

    /// Where the two arms of a branch meet again, if inside the current region.
    fn follow_of(&self, b: usize) -> Option<usize> {
        let f = self.ipdom[b]?;
        if f >= self.f.blocks.len() {
            return None;
        }
        if let Some((h, exit)) = self.innermost_loop() {
            if f == h || Some(f) == exit || !self.in_loop_set(f) {
                return None;
            }
        }
        Some(f)
    }

    fn jump_to(&mut self, b: usize) -> Option<Node> {
        // Is `b` a break or a continue of the loop we are in?
        for (i, s) in self.scopes.iter().enumerate().rev() {
            if let Scope::Loop { header, exit } = s {
                let innermost = !self.scopes[i + 1..].iter().any(|x| matches!(x, Scope::Loop { .. }));
                let in_switch = self.scopes[i + 1..].iter().any(|x| matches!(x, Scope::Switch));
                if b == *header {
                    return Some(if innermost { Node::Continue } else { self.goto(b) });
                }
                if Some(b) == *exit {
                    return Some(if innermost && !in_switch { Node::Break } else { self.goto(b) });
                }
            }
        }
        None
    }

    fn goto(&mut self, b: usize) -> Node {
        self.labels.insert(b);
        Node::Goto(b)
    }

    fn gen(&mut self, start: usize, follow: Option<usize>) -> Vec<Node> {
        let mut out: Vec<Node> = Vec::new();
        let mut cur = Some(start);
        while let Some(b) = cur {
            if self.opening == Some(b) {
                self.opening = None;
            } else if let Some(n) = self.jump_to(b) {
                out.push(n);
                break;
            }
            if Some(b) == follow {
                break;
            }
            if self.emitted.contains(&b) {
                out.push(self.goto(b));
                break;
            }
            if self.loops.contains_key(&b) && !self.entered.contains(&b) {
                self.entered.insert(b);
                let exit = self.loop_exit(b);
                self.scopes.push(Scope::Loop { header: b, exit });
                self.opening = Some(b);
                let body = self.gen(b, None);
                self.scopes.pop();
                out.push(Node::Loop(body));
                cur = exit;
                continue;
            }
            self.emitted.insert(b);
            out.push(Node::Label(b));
            let bl = &self.f.blocks[b];
            for s in &bl.stmts {
                out.push(Node::Stmt(s.addr, s.st.clone()));
            }
            let ta = bl.term_addr;
            match bl.term.clone() {
                Term::Jump(t) => cur = Some(t),
                Term::Branch(c, t, e) => {
                    let fol = self.follow_of(b);
                    let a = self.arm(t, fol);
                    let z = self.arm(e, fol);
                    out.push(Node::If(ta, c, a, z));
                    cur = fol;
                }
                Term::Switch(v, cases, def) => {
                    let fol = self.follow_of(b);
                    self.scopes.push(Scope::Switch);
                    let mut groups: Vec<(Vec<u64>, usize)> = Vec::new();
                    for (val, t) in cases {
                        match groups.iter_mut().find(|g| g.1 == t) {
                            Some(g) => g.0.push(val),
                            None => groups.push((vec![val], t)),
                        }
                    }
                    let mut arms = Vec::new();
                    for (vals, t) in groups {
                        let body = self.arm(t, fol);
                        arms.push((vals, body));
                    }
                    let d = def.filter(|d| Some(*d) != fol).map(|d| self.arm(d, fol));
                    self.scopes.pop();
                    out.push(Node::Switch(ta, v, arms, d));
                    cur = fol;
                }
                Term::Return(e) => {
                    out.push(Node::Return(ta, e));
                    break;
                }
                Term::TailCall(e) => {
                    out.push(Node::TailCall(ta, e));
                    break;
                }
                Term::Indirect(e) => {
                    out.push(Node::Indirect(ta, e));
                    break;
                }
                Term::Stop => {
                    out.push(Node::Stop(ta));
                    break;
                }
            }
        }
        out
    }

    fn arm(&mut self, t: usize, follow: Option<usize>) -> Vec<Node> {
        if Some(t) == follow {
            return Vec::new();
        }
        self.gen(t, follow)
    }

    /// The block after a loop: the outside block most paths leave to.
    fn loop_exit(&self, h: usize) -> Option<usize> {
        let set = &self.loops[&h];
        let mut count: HashMap<usize, usize> = HashMap::new();
        for &b in set {
            for s in self.f.blocks[b].term.succs() {
                if !set.contains(&s) {
                    *count.entry(s).or_insert(0) += 1;
                }
            }
        }
        // prefer an exit that is not an end of the function, then the most used, then the earliest
        let mut v: Vec<(usize, usize)> = count.into_iter().collect();
        v.sort_by_key(|&(b, c)| {
            let terminal = self.f.blocks[b].term.succs().is_empty();
            (terminal, std::cmp::Reverse(c), self.rpo[b])
        });
        v.first().map(|x| x.0)
    }
}

pub fn structure(f: &mut Lifted) -> Vec<Node> {
    super::flow::prune(f);
    merge_conds(f);
    let n = f.blocks.len();
    if n == 0 {
        return Vec::new();
    }
    let succs: Vec<Vec<usize>> = f.blocks.iter().map(|b| b.term.succs()).collect();
    let (idom, rpo) = idoms(n, &succs, 0);
    // post dominators: reverse graph plus a virtual exit
    let mut rev: Vec<Vec<usize>> = vec![Vec::new(); n + 1];
    for (b, ss) in succs.iter().enumerate() {
        if ss.is_empty() {
            rev[n].push(b);
        }
        for &s in ss {
            rev[s].push(b);
        }
    }
    let (ipdom, _) = idoms(n + 1, &rev, n);
    let ipdom: Vec<Option<usize>> = ipdom.into_iter().take(n).map(|x| x.filter(|_| true)).collect();
    // loops
    let mut loops: HashMap<usize, BTreeSet<usize>> = HashMap::new();
    let ctx0 = Ctx {
        f,
        idom: idom.clone(),
        ipdom: ipdom.clone(),
        rpo: rpo.clone(),
        loops: HashMap::new(),
        emitted: HashSet::new(),
        entered: HashSet::new(),
        opening: None,
        labels: HashSet::new(),
        scopes: Vec::new(),
    };
    let preds = preds_of(f);
    for a in 0..n {
        if rpo[a] == usize::MAX {
            continue;
        }
        for &h in &succs[a] {
            if ctx0.dominates(h, a) {
                let set = loops.entry(h).or_default();
                set.insert(h);
                let mut stack = vec![a];
                while let Some(x) = stack.pop() {
                    if set.insert(x) {
                        stack.extend(preds[x].iter().copied());
                    }
                }
            }
        }
    }
    let mut ctx = ctx0;
    ctx.loops = loops;
    let mut body = ctx.gen(0, None);
    let labels = ctx.labels.clone();
    clean(&mut body, &labels);
    body
}

// ---------------------------------------------------------------------------------------------
// Clean-up of the tree

fn contains_continue(v: &[Node]) -> bool {
    v.iter().any(|n| match n {
        Node::Continue => true,
        Node::If(_, _, a, b) => contains_continue(a) || contains_continue(b),
        Node::Switch(_, _, arms, d) => {
            arms.iter().any(|a| contains_continue(&a.1)) || d.as_ref().is_some_and(|d| contains_continue(d))
        }
        // an inner loop has its own continue
        _ => false,
    })
}

fn clean(v: &mut Vec<Node>, labels: &HashSet<usize>) {
    v.retain(|n| match n {
        Node::Label(b) => labels.contains(b),
        _ => true,
    });
    for n in v.iter_mut() {
        match n {
            Node::If(_, _, a, b) => {
                clean(a, labels);
                clean(b, labels);
            }
            Node::Loop(b) | Node::While(_, b) | Node::DoWhile(b, _) => clean(b, labels),
            Node::For(_, _, _, b) => clean(b, labels),
            Node::Switch(_, _, arms, d) => {
                arms.iter_mut().for_each(|a| clean(&mut a.1, labels));
                if let Some(d) = d {
                    clean(d, labels);
                }
            }
            _ => {}
        }
    }
    // if shapes
    let old = std::mem::take(v);
    for n in old {
        match n {
            Node::If(a, c, t, e) => {
                let (c, t, e) = if t.is_empty() && !e.is_empty() { (c.not(), e, t) } else { (c, t, e) };
                if t.is_empty() && e.is_empty() {
                    continue;
                }
                if !e.is_empty() && ends_terminal(&t) {
                    v.push(Node::If(a, c, t, Vec::new()));
                    v.extend(e);
                } else if !e.is_empty() && ends_terminal(&e) && !ends_terminal(&t) {
                    v.push(Node::If(a, c.not(), e, Vec::new()));
                    v.extend(t);
                } else {
                    v.push(Node::If(a, c, t, e));
                }
            }
            other => v.push(other),
        }
    }
    // loops
    let old = std::mem::take(v);
    for n in old {
        let n = match n {
            Node::Loop(body) => loop_form(body),
            other => other,
        };
        v.push(n);
    }
    // for loops: `i = init; while (cond) { ...; i = step }`
    let mut i = 1;
    while i < v.len() {
        if let Node::While(c, body) = &v[i] {
            if let (Some(Node::Stmt(_, Stmt::Assign(iv, _))), Some(Node::Stmt(_, Stmt::Assign(sv, _)))) =
                (v.get(i - 1), body.last())
            {
                if iv == sv && c.uses(*iv) > 0 && body.len() > 1 && !contains_continue(body) {
                    let init = v[i - 1].clone();
                    let Node::While(c, mut body) = std::mem::replace(&mut v[i], Node::Break) else { unreachable!() };
                    let step = body.pop().unwrap_or(Node::Break);
                    v[i] = Node::For(Box::new(init), c, Box::new(step), body);
                    v.remove(i - 1);
                    continue;
                }
            }
        }
        i += 1;
    }
}

fn loop_form(mut body: Vec<Node>) -> Node {
    // while (cond) { ... }: the first node leaves the loop
    if let Some(Node::If(_, c, a, b)) = body.first() {
        if b.is_empty() && matches!(a.as_slice(), [Node::Break]) {
            let c = c.clone().not();
            body.remove(0);
            return Node::While(c, body);
        }
    }
    // do { ... } while (cond)
    let n = body.len();
    if n >= 2 {
        if let (Node::If(_, c, a, b), Node::Break) = (&body[n - 2], &body[n - 1]) {
            if b.is_empty() && matches!(a.as_slice(), [Node::Continue]) {
                let c = c.clone();
                let mut inner: Vec<Node> = body[..n - 2].to_vec();
                if !contains_continue(&inner) {
                    return Node::DoWhile(std::mem::take(&mut inner), c);
                }
            }
        }
    }
    // do { ... } while (cond) written as `if (!cond) break;` at the end
    if let Some(Node::If(_, c, a, b)) = body.last() {
        if b.is_empty() && matches!(a.as_slice(), [Node::Break]) {
            let c = c.clone().not();
            let mut inner: Vec<Node> = body[..body.len() - 1].to_vec();
            if !contains_continue(&inner) && !inner.is_empty() {
                return Node::DoWhile(std::mem::take(&mut inner), c);
            }
        }
    }
    Node::Loop(body)
}
