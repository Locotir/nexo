//! The call graph: `path` (how does A reach B) and `tree` (callers or callees to N levels).
//!
//! The graph is read from the references on demand: a node is a function start (or an import slot);
//! an edge is a direct call, a jump out of the function (a tail call), and with `--ptrs` also a function
//! whose address is taken. Indirect calls have no target, so a missing route is never reported without
//! saying how many calls could not be followed.

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};

use serde_json::json;

use crate::disasm::describe;
use crate::ops::command;
use crate::page::{cut, shell_word, Output, Row};
use crate::refs::Kind;
use crate::xops::{indirect_line, stub_slot, target_addr};
use crate::{Error, Image, Result};

/// The deepest tree.
pub const MAX_DEPTH: usize = 12;
/// The most lines a tree is built to; the rest is named, not built.
const MAX_TREE_NODES: usize = 4000;
/// The most functions a path search visits.
const MAX_VISITED: usize = 400_000;
/// The most routes a path answer lists.
pub const MAX_ROUTES: usize = 20;

/// How an edge got there.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Via {
    Call,
    Jump,
    /// The function's address is taken (`lea`, an immediate): it may be called through a pointer.
    Addr,
}

impl Via {
    fn word(self) -> &'static str {
        match self {
            Via::Call => "call",
            Via::Jump => "jump",
            Via::Addr => "addr",
        }
    }
}

/// One edge as seen from the node being expanded: the other end, how many sites, the first site.
#[derive(Clone, Debug)]
struct Edge {
    node: u64,
    sites: usize,
    site: u64,
    via: Via,
}

/// The node an address belongs to: an import slot, or the start of the function that holds it.
fn node_of(img: &Image, a: u64) -> u64 {
    if img.slots.contains_key(&a) {
        return a;
    }
    img.func_at(a).map_or(a, |f| f.start)
}

fn node_name(img: &Image, n: u64) -> String {
    if let Some(&i) = img.slots.get(&n) {
        return img.imports[i].name.clone();
    }
    match img.func_at(n) {
        Some(f) => img.func_name(f).to_string(),
        None => describe(img, n, false).unwrap_or_else(|| format!("{n:#x}")),
    }
}

fn kind_via(k: Kind, ptrs: bool) -> Option<Via> {
    match k {
        Kind::Call => Some(Via::Call),
        Kind::Jump => Some(Via::Jump),
        Kind::Addr | Kind::Imm if ptrs => Some(Via::Addr),
        _ => None,
    }
}

/// Merge raw (node, site, via) hits into edges: one per node, the lowest site first.
fn merge(mut hits: Vec<(u64, u64, Via)>) -> Vec<Edge> {
    hits.sort_by_key(|&(n, s, v)| (n, s, v as u8));
    let mut out: Vec<Edge> = Vec::new();
    for (node, site, via) in hits {
        match out.last_mut() {
            Some(e) if e.node == node => {
                e.sites += 1;
                // a call is stronger evidence than a jump or an address taken
                if via == Via::Call && e.via != Via::Call {
                    e.via = via;
                }
            }
            _ => out.push(Edge { node, sites: 1, site, via }),
        }
    }
    out.sort_by_key(|e| e.site);
    out
}

/// What a function calls (and with `ptrs` takes the address of), by first call site.
fn callees(img: &Image, node: u64, ptrs: bool) -> Vec<Edge> {
    let Some(f) = img.funcs().binary_search_by_key(&node, |f| f.start).ok().map(|i| &img.funcs()[i]) else {
        return Vec::new();
    };
    let starts = img.funcs();
    let hits = img
        .refs()
        .from(f.start, f.end)
        .filter_map(|r| {
            let via = kind_via(r.kind, ptrs)?;
            if via == Via::Addr && starts.binary_search_by_key(&r.target, |x| x.start).is_err() {
                return None;
            }
            Some((node_of(img, r.target), r.src, via))
        })
        .filter(|&(n, _, via)| via != Via::Jump || n != node)
        .collect();
    merge(hits)
}

/// Who calls a node (and with `ptrs` takes its address), by the function that holds the site.
fn callers(img: &Image, node: u64, ptrs: bool) -> Vec<Edge> {
    let refs = img.refs();
    let lists = std::iter::once(refs.at(node)).chain(stub_slot(img, node).map(|s| refs.at(s)));
    let hits = lists
        .flat_map(|l| l.iter())
        .filter_map(|r| {
            let via = kind_via(r.kind, ptrs)?;
            // a stub's own jump through its slot is not a caller
            if img.func_at(r.src).is_some_and(|h| h.start == node) && via == Via::Jump {
                return None;
            }
            Some((node_of(img, r.src), r.src, via))
        })
        .collect();
    merge(hits)
}

fn edges_of(img: &Image, node: u64, up: bool, ptrs: bool) -> Vec<Edge> {
    if up {
        callers(img, node, ptrs)
    } else {
        callees(img, node, ptrs)
    }
}

fn code_node(img: &Image, target: &str) -> Result<u64> {
    let a = target_addr(img, target)?;
    if img.slots.contains_key(&a) {
        return Ok(a);
    }
    match img.func_at(a) {
        Some(f) => Ok(f.start),
        None => Err(Error::Bad(format!(
            "{a:#x} is not inside a function found in this file; give a function name or an address in code"
        ))),
    }
}

fn row(text: String, j: Option<serde_json::Value>) -> Row {
    Row { key: 0, text, json: j }
}

/// The ends of a tree line: `name 0x401000`, with a mark for imports.
fn label(img: &Image, n: u64) -> String {
    let import = if img.slots.contains_key(&n) || img.func_at(n).is_some_and(|f| f.name.ends_with("@plt")) {
        " [import]"
    } else {
        ""
    };
    format!("{} {n:#x}{import}", node_name(img, n))
}

/// Callers or callees of a function to `depth` levels, as an indented tree. A function that was
/// already expanded higher up is shown again without its subtree.
pub fn tree_window(
    img: &Image,
    target: &str,
    up: bool,
    depth: usize,
    ptrs: bool,
    from: Option<u64>,
    budget: usize,
) -> Result<Output> {
    let _ = img.refs();
    let root = code_node(img, target)?;
    let depth = depth.clamp(1, MAX_DEPTH);
    let mut lines: Vec<Row> = Vec::new();
    let mut expanded: HashSet<u64> = HashSet::new();
    let mut capped = false;
    let mut distinct: HashSet<u64> = HashSet::new();
    // (node, level, edge that reached it)
    let mut stack: Vec<(u64, usize, Option<Edge>)> = vec![(root, 0, None)];
    while let Some((n, level, edge)) = stack.pop() {
        if lines.len() >= MAX_TREE_NODES {
            capped = true;
            break;
        }
        distinct.insert(n);
        let kids = if level < depth && !expanded.contains(&n) { edges_of(img, n, up, ptrs) } else { Vec::new() };
        let seen = expanded.contains(&n);
        let total_kids = if level < depth { 0 } else { edges_of(img, n, up, ptrs).len() };
        expanded.insert(n);
        let mut text = format!("{}{}", "  ".repeat(level), label(img, n));
        if let Some(e) = &edge {
            let times = if e.sites > 1 { format!(" x{}", e.sites) } else { String::new() };
            text.push_str(&format!("  ({} at {:#x}{times})", e.via.word(), e.site));
        }
        if seen && level > 0 {
            text.push_str("  ; expanded above");
        } else if level == depth && total_kids > 0 {
            text.push_str(&format!("  ; +{total_kids} more {}", if up { "callers" } else { "callees" }));
        } else if level < depth && kids.is_empty() && !seen && !img.slots.contains_key(&n) {
            let is_stub = img.func_at(n).is_some_and(|f| f.name.ends_with("@plt"));
            if !is_stub {
                text.push_str(if up { "  ; no caller found" } else { "  ; calls nothing" });
            }
        }
        lines.push(row(
            text,
            Some(
                json!({"level": level, "addr": n, "name": node_name(img, n), "via": edge.as_ref().map(|e| e.via.word()),
                "site": edge.as_ref().map(|e| e.site), "sites": edge.as_ref().map(|e| e.sites),
                "expanded_above": seen && level > 0}),
            ),
        ));
        for e in kids.into_iter().rev() {
            stack.push((e.node, level + 1, Some(e)));
        }
    }
    let head = format!(
        "tree {} of {} {root:#x}  ; depth {depth}, {} lines, {} distinct functions{}",
        if up { "callers" } else { "callees" },
        node_name(img, root),
        lines.len(),
        distinct.len(),
        if ptrs { ", address-taken functions included" } else { "" }
    );
    let mut items: Vec<Row> = vec![row(head, None)];
    items.extend(lines);
    if capped {
        items.push(row(
            format!("the tree was cut at {MAX_TREE_NODES} lines; ask for a smaller --depth or start lower down"),
            None,
        ));
    }
    if let Some(l) = indirect_line(img) {
        items.push(row(format!("{l}, so calls through pointers are missing from the tree"), None));
    }
    let (rows, tail) = cut(&items, from, budget, |r| r.clone());
    let mut rest = shell_word(target.trim());
    if up {
        rest.push_str(" --up");
    }
    if depth != 3 {
        rest.push_str(&format!(" --depth {depth}"));
    }
    if ptrs {
        rest.push_str(" --ptrs");
    }
    let meta = json!({"root": root, "name": node_name(img, root), "direction": if up { "callers" } else { "callees" },
        "depth": depth, "functions": distinct.len(), "capped": capped, "ptrs": ptrs});
    Ok(Output { op: "tree", meta, rows, tail, command: command(img, "tree", &rest) })
}

/// Result of a breadth-first search: distance and the nodes one step closer to the start.
struct Search {
    dist: HashMap<u64, usize>,
    parents: HashMap<u64, Vec<(u64, Edge)>>,
    visited_cap: bool,
}

/// Breadth-first from `start` until the level that holds `goal` is complete.
fn search(img: &Image, start: u64, goal: u64, ptrs: bool) -> Search {
    let mut dist: HashMap<u64, usize> = HashMap::new();
    let mut parents: HashMap<u64, Vec<(u64, Edge)>> = HashMap::new();
    let mut queue: VecDeque<u64> = VecDeque::new();
    dist.insert(start, 0);
    queue.push_back(start);
    let mut visited_cap = false;
    let mut goal_level: Option<usize> = None;
    while let Some(n) = queue.pop_front() {
        let d = dist[&n];
        if goal_level.is_some_and(|g| d >= g) {
            break;
        }
        if dist.len() > MAX_VISITED {
            visited_cap = true;
            break;
        }
        for e in callees(img, n, ptrs) {
            match dist.get(&e.node) {
                None => {
                    dist.insert(e.node, d + 1);
                    parents.entry(e.node).or_default().push((n, e.clone()));
                    if e.node == goal {
                        goal_level = Some(d + 1);
                    }
                    queue.push_back(e.node);
                }
                Some(&dd) if dd == d + 1 => parents.entry(e.node).or_default().push((n, e.clone())),
                _ => {}
            }
        }
    }
    Search { dist, parents, visited_cap }
}

/// The shortest routes from `start` to `goal`, at most `max`, and how many there are (capped).
fn routes(s: &Search, start: u64, goal: u64, max: usize) -> (Vec<Vec<(u64, Edge)>>, u64) {
    // count by dynamic programming over the shortest-path DAG
    let mut count: HashMap<u64, u64> = HashMap::new();
    fn paths(s: &Search, n: u64, start: u64, count: &mut HashMap<u64, u64>) -> u64 {
        if n == start {
            return 1;
        }
        if let Some(&c) = count.get(&n) {
            return c;
        }
        let c = s
            .parents
            .get(&n)
            .map_or(0, |ps| ps.iter().map(|(p, _)| paths(s, *p, start, count)).fold(0u64, u64::saturating_add));
        count.insert(n, c);
        c
    }
    // depth of recursion is the route length, which is small
    let total = paths(s, goal, start, &mut count);
    let mut out: Vec<Vec<(u64, Edge)>> = Vec::new();
    let mut cur: Vec<(u64, Edge)> = Vec::new();
    fn walk(s: &Search, n: u64, start: u64, cur: &mut Vec<(u64, Edge)>, out: &mut Vec<Vec<(u64, Edge)>>, max: usize) {
        if out.len() >= max {
            return;
        }
        if n == start {
            let mut r = cur.clone();
            r.reverse();
            out.push(r);
            return;
        }
        let mut ps: Vec<&(u64, Edge)> = s.parents.get(&n).map(|v| v.iter().collect()).unwrap_or_default();
        ps.sort_by_key(|(p, e)| (e.via as u8, e.site, *p));
        for (p, e) in ps {
            cur.push((*p, e.clone()));
            walk(s, *p, start, cur, out, max);
            cur.pop();
        }
    }
    walk(s, goal, start, &mut cur, &mut out, max);
    (out, total)
}

/// How function `start` reaches `goal`: the shortest chains of calls, each hop with its call site.
pub fn path_window(
    img: &Image,
    start: &str,
    goal: &str,
    ptrs: bool,
    max_routes: usize,
    from: Option<u64>,
    budget: usize,
) -> Result<Output> {
    let _ = img.refs();
    let (a, b) = (code_node(img, start)?, code_node(img, goal)?);
    let max_routes = max_routes.clamp(1, MAX_ROUTES);
    let mut used_ptrs = ptrs;
    let mut s = search(img, a, b, ptrs);
    let mut fallback = false;
    if !s.dist.contains_key(&b) && !ptrs {
        let s2 = search(img, a, b, true);
        if s2.dist.contains_key(&b) {
            s = s2;
            used_ptrs = true;
            fallback = true;
        }
    }
    let mut items: Vec<Row> = Vec::new();
    let (na, nb) = (node_name(img, a), node_name(img, b));
    let mut meta = json!({"from": a, "to": b, "ptrs": used_ptrs});
    if a == b {
        items.push(row(format!("path {na} {a:#x} -> {nb}  ; the same function"), None));
    } else if let Some(&len) = s.dist.get(&b) {
        let (rs, total) = routes(&s, a, b, max_routes);
        let kinds = if fallback { "calls and address-taken functions" } else { "calls" };
        items.push(row(
            format!(
                "path {na} {a:#x} -> {nb} {b:#x}  ; {len} hop{}, {} shortest route{} ({kinds}){}",
                if len == 1 { "" } else { "s" },
                total,
                if total == 1 { "" } else { "s" },
                if total as usize > rs.len() { format!(", showing {}", rs.len()) } else { String::new() }
            ),
            None,
        ));
        if fallback {
            items.push(row(
                "no route over direct calls and tail jumps; this one needs a function whose address is taken (addr hop): \
                 it is a possible route through a pointer, not proof that it runs"
                    .into(),
                None,
            ));
        }
        for (i, r) in rs.iter().enumerate() {
            items.push(row(format!("route {}", i + 1), None));
            for (n, e) in r {
                items.push(row(format!("  {:<28} {n:#010x}  --{} at {:#x}-->", node_name(img, *n), e.via.word(), e.site), Some(json!({"route": i + 1, "addr": n, "name": node_name(img, *n), "via": e.via.word(), "site": e.site}))));
            }
            items.push(row(format!("  {:<28} {b:#010x}", nb), Some(json!({"route": i + 1, "addr": b, "name": nb}))));
        }
        meta["hops"] = json!(len);
        meta["routes"] = json!(total);
    } else {
        let reach = s.dist.len();
        items.push(row(format!("path {na} {a:#x} -> {nb} {b:#x}  ; no route found"), None));
        if s.visited_cap {
            items.push(row(format!("the search stopped after {MAX_VISITED} functions, so a route may exist"), None));
        } else {
            items.push(row(
                format!(
                    "{na} reaches {reach} function{} over direct calls{}; none is {nb}",
                    if reach == 1 { "" } else { "s" },
                    if ptrs { " and address-taken functions" } else { " (also tried address-taken functions)" }
                ),
                None,
            ));
        }
        let cs = callers(img, b, true);
        if cs.is_empty() {
            items.push(row(format!("{nb} has no caller and no address taken in the code; it may be called through a table or by the loader"), None));
        } else {
            items.push(row(
                format!(
                    "{nb} is reached from {} function{}; closest to look at:",
                    cs.len(),
                    if cs.len() == 1 { "" } else { "s" }
                ),
                None,
            ));
            for e in cs.iter().take(5) {
                items.push(row(format!("  {}  ({} at {:#x})", label(img, e.node), e.via.word(), e.site), None));
            }
        }
        meta["routes"] = json!(0);
    }
    if let Some(l) = indirect_line(img) {
        items.push(row(format!("{l}: a route through a pointer would not show here"), None));
    }
    let (rows, tail) = cut(&items, from, budget, |r| r.clone());
    let mut rest = format!("{} {}", shell_word(start.trim()), shell_word(goal.trim()));
    if ptrs {
        rest.push_str(" --ptrs");
    }
    if max_routes != 3 {
        rest.push_str(&format!(" --routes {max_routes}"));
    }
    Ok(Output { op: "path", meta, rows, tail, command: command(img, "path", &rest) })
}

/// A map from node to name for tests and callers that need many at once.
#[allow(dead_code)]
pub(crate) fn names(img: &Image, nodes: &[u64]) -> BTreeMap<u64, String> {
    nodes.iter().map(|&n| (n, node_name(img, n))).collect()
}
