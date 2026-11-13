//! The page as a tree the model can read. Two views share one page-side walker (`tree.js`): the compact
//! snapshot (visible actionable elements only, each with a stable ref `e1`, `e2`, ...) that follows every
//! action, and the full tree of `read_page` (headings, landmarks, lists, tables, images, text, elements that
//! are not visible). Both hand out the same refs, so a ref from either works for click, fill and the rest.

use serde::Deserialize;

/// One actionable element: the ref to act on, its role (or tag) and the name a person would call it.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Element {
    #[serde(rename = "ref")]
    pub reference: String,
    pub role: String,
    pub name: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Snapshot {
    pub elements: Vec<Element>,
}

impl Snapshot {
    /// One line per element, e.g. `e3 button "Submit"`, for the model to read.
    pub fn render(&self) -> String {
        let mut out = String::new();
        for e in &self.elements {
            if !out.is_empty() {
                out.push('\n');
            }
            out.push_str(&format!("{} {} {}", e.reference, e.role, quote(&e.name)));
        }
        out
    }

    /// The first element with this ref.
    pub fn find(&self, reference: &str) -> Option<&Element> {
        self.elements.iter().find(|e| e.reference == reference)
    }
}

/// The page-side walker: a function of `{filter, depth, refId}` returning the tree as JSON text. An element keeps
/// the ref it was given on an earlier walk (the attribute stays on the node); new ones continue after the
/// highest number in use.
pub(crate) const TREE_JS: &str = include_str!("tree.js");

/// Default `max_chars` of [`ReadPageOptions`].
pub const DEFAULT_MAX_CHARS: usize = 50_000;
/// Default `depth` of [`ReadPageOptions`].
pub const DEFAULT_DEPTH: usize = 15;
/// Smallest `max_chars` honoured, so the note that says what was cut always fits.
const MIN_MAX_CHARS: usize = 200;

/// Which elements [`crate::Browser::read_page`] keeps.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Filter {
    /// The whole tree: headings, landmarks, lists, tables, images, text, and elements that are not visible
    /// (marked `(not visible)`).
    #[default]
    All,
    /// Only the visible actionable elements, one flat line each, like the snapshot that follows every action.
    Interactive,
}

/// What [`crate::Browser::read_page`] reads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadPageOptions {
    pub filter: Filter,
    /// Tree levels shown (an element adds one, wrappers such as `div` add none). Deeper content is marked.
    pub depth: usize,
    /// Read only the subtree of this element (a ref such as `e12`, from any earlier snapshot).
    pub ref_id: Option<String>,
    /// Longest answer in characters; it is cut at a line boundary with a note.
    pub max_chars: usize,
}

impl Default for ReadPageOptions {
    fn default() -> Self {
        Self { filter: Filter::All, depth: DEFAULT_DEPTH, ref_id: None, max_chars: DEFAULT_MAX_CHARS }
    }
}

/// One record of the walk: an element (with a ref when it can be pointed at) or a run of text.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub(crate) struct Node {
    #[serde(rename = "d")]
    pub depth: usize,
    #[serde(rename = "ref", default)]
    pub reference: Option<String>,
    pub role: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub attrs: Vec<String>,
    /// Not visible on the page (display none, zero size, visibility hidden); its children are not marked again.
    #[serde(default)]
    pub hidden: bool,
    /// Content below the depth limit (or past the options of a select) was left out.
    #[serde(default)]
    pub more: bool,
    /// Inside an open shadow root: shown, but the actions that take refs cannot reach it, so it has none.
    #[serde(default)]
    pub noref: bool,
}

#[derive(Debug, Deserialize)]
pub(crate) struct TreeReply {
    pub found: bool,
    #[serde(default)]
    pub nodes: Vec<Node>,
    #[serde(default)]
    pub capped: bool,
}

/// The arguments of the walker as the JSON it takes.
pub(crate) fn tree_call(opts: &ReadPageOptions) -> String {
    let arg = serde_json::json!({
        "filter": match opts.filter { Filter::All => "all", Filter::Interactive => "interactive" },
        "depth": opts.depth.max(1),
        "refId": opts.ref_id,
    });
    format!("({TREE_JS})({arg})")
}

impl TreeReply {
    /// The actionable elements of the walk, as the compact snapshot lists them.
    pub(crate) fn elements(&self) -> Vec<Element> {
        self.nodes
            .iter()
            .filter_map(|n| {
                n.reference.as_ref().map(|r| Element {
                    reference: r.clone(),
                    role: n.role.clone(),
                    name: n.name.clone(),
                })
            })
            .collect()
    }
}

fn quote(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
}

fn line(n: &Node, flat: bool) -> String {
    let mut out = if flat { String::new() } else { "  ".repeat(n.depth) };
    if let Some(r) = &n.reference {
        out.push_str(r);
        out.push(' ');
    }
    out.push_str(&n.role);
    if !n.name.is_empty() {
        out.push(' ');
        out.push_str(&quote(&n.name));
    }
    for a in &n.attrs {
        out.push(' ');
        out.push_str(a);
    }
    if n.hidden {
        out.push_str(" (not visible)");
    }
    if n.noref {
        out.push_str(" (shadow DOM: no ref)");
    }
    if n.more {
        out.push_str(" (+deeper)");
    }
    out
}

/// What a read of the page came to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PageTree {
    /// The tree, one line per node, indented by level (flat for [`Filter::Interactive`]), ending in a note when
    /// it was cut by `max_chars`, by `depth` or by the size of the page.
    pub text: String,
    /// Characters of the whole tree before any cut by `max_chars`.
    pub total_chars: usize,
    /// True when `max_chars` cut lines off the end.
    pub truncated: bool,
}

/// Renders the walk the way `read_page` shows it, cut at `max_chars` on a line boundary.
pub(crate) fn render_tree(reply: &TreeReply, opts: &ReadPageOptions) -> PageTree {
    let flat = opts.filter == Filter::Interactive;
    let lines: Vec<String> = reply.nodes.iter().map(|n| line(n, flat)).collect();
    let total_chars: usize = lines.iter().map(|l| l.chars().count() + 1).sum::<usize>().saturating_sub(1);
    let budget = opts.max_chars.max(MIN_MAX_CHARS);
    let mut shown = 0;
    let mut used = 0;
    for l in &lines {
        let cost = l.chars().count() + usize::from(shown > 0);
        if used + cost > budget {
            break;
        }
        used += cost;
        shown += 1;
    }
    let mut text = lines[..shown].join("\n");
    // A first line longer than the whole budget is shortened rather than dropped: the answer is never empty.
    let mut shortened = false;
    if shown == 0 && !lines.is_empty() {
        shortened = true;
        text = lines[0].chars().take(budget.saturating_sub(1)).collect::<String>() + "…";
        used = text.chars().count();
        shown = 1;
    }
    let truncated = shown < lines.len() || shortened;
    if truncated {
        let narrow = if flat {
            "a smaller depth or the ref_id of a section"
        } else {
            "filter=interactive, a smaller depth, or the ref_id of a section (see the refs above)"
        };
        text.push_str(&format!(
            "\n... cut after {shown} of {} lines ({used} of {total_chars} characters). Narrow it with {narrow}, or raise max_chars.",
            lines.len()
        ));
    }
    if reply.nodes[..shown].iter().any(|n| n.more) {
        text.push_str("\n(+deeper) marks content left out below that line: raise depth, or read_page it with ref_id.");
    }
    if reply.capped {
        text.push_str(
            "\n... the page is huge: only the first 20000 elements were read. Use ref_id to read a part of it.",
        );
    }
    if lines.is_empty() {
        text.clear();
    }
    PageTree { text, total_chars, truncated }
}
