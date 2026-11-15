//! The text of MCP tools in a small window. A server's schemas are the person's choice and as long as its
//! author wrote them, so past a share of the window the tools beyond it are offered by name and one line, and
//! the model is told which (see [`McpShortened::note`]). A shortened tool still runs: the server checks its
//! arguments, and the caller that offered it short adds the whole schema to an error (see [`with_schema`]).

use std::sync::Arc;

use super::{Tool, ToolOutput, ToolSet};
use crate::provider::ToolSpec;

/// The tokens the MCP tools may take together in a request of a small window (32k and under), as offered
/// whole and shortened. It is an eighth of 8 000 tokens: a bare 8k session has 1 912 tokens of fixed text and
/// asks 2 000 for the reply, so 4 088 are left for the conversation (measured), and this share leaves it 3 088
/// of them, three quarters of what it had. A larger window keeps more room than that with the same share.
pub const MCP_SMALL_TOKENS: u64 = 1_000;

/// The tokens the MCP tools may take together in a request of a small window of `window` tokens: an eighth of it,
/// [`MCP_SMALL_TOKENS`] at 8k, or that when the window is not known.
pub fn mcp_share(window: Option<u64>) -> u64 {
    window.map_or(MCP_SMALL_TOKENS, |w| w / 8)
}

/// `out` of a call to `tool`, for a caller that offered the tools in `short` by name and one line: when the call
/// failed and `tool` was one of them, the tool's whole input schema follows, to retry with. The schema goes by
/// what this caller offered, since one MCP server serves agents on different windows at once.
pub fn with_schema(tool: &dyn Tool, short: &[String], mut out: ToolOutput) -> ToolOutput {
    if out.is_error && tool.server().is_some() && short.iter().any(|n| n == tool.name()) {
        out.content = format!(
            "{}\nThe tool was offered by name only; its whole input schema: {}",
            out.content,
            tool.spec().parameters
        );
    }
    out
}

/// The characters of the one line a shortened tool keeps, from the longest to none: the longest line that lets
/// the shortened tools fit the share is used.
const LINES: [usize; 3] = [80, 40, 0];

/// nexo's own estimate of a tool as the wire carries it: four characters a token.
fn tokens(spec: &ToolSpec) -> u64 {
    serde_json::to_string(spec).map_or(0, |s| s.chars().count() as u64 / 4)
}

/// `spec` by name and one line of at most `line` characters, with no parameters listed.
fn short_spec(spec: &ToolSpec, line: usize) -> ToolSpec {
    let first = spec.description.lines().next().unwrap_or("").trim();
    ToolSpec {
        name: spec.name.clone(),
        description: first.chars().take(line).collect(),
        parameters: serde_json::json!({"type": "object"}),
    }
}

/// What was shortened in one request, for the model to be told.
#[derive(Debug, Clone, PartialEq)]
pub struct McpShortened {
    /// Per server, the tools offered by name and one line.
    pub servers: Vec<(String, Vec<String>)>,
    /// How many MCP tools there are in all.
    pub tools: usize,
    /// The tokens of all of them whole, and of what is offered.
    pub whole: u64,
    pub offered: u64,
    /// The share they may take.
    pub budget: u64,
}

impl McpShortened {
    /// The tools offered by name and one line, of every server.
    pub fn names(&self) -> Vec<String> {
        self.servers.iter().flat_map(|(_, t)| t.iter().cloned()).collect()
    }

    /// The fact the model gets about it, for a model with context window `window`.
    pub fn note(&self, window: Option<u64>) -> String {
        let cut: usize = self.servers.iter().map(|(_, t)| t.len()).sum();
        let for_window = window.map_or(String::new(), |w| format!(" for a {w}-token window"));
        let mut out = format!(
            "MCP tools: {cut} of {} are offered by name and one line{for_window} (all of them whole take {} tokens, \
             offered {} of a share of {}). A call with arguments the server refuses answers with the tool's whole schema.",
            self.tools, self.whole, self.offered, self.budget
        );
        for (server, names) in &self.servers {
            const SHOWN: usize = 12;
            let list: Vec<&str> = names.iter().take(SHOWN).map(String::as_str).collect();
            let more = names.len().saturating_sub(SHOWN);
            let tail = if more > 0 { format!(", and {more} more") } else { String::new() };
            out.push_str(&format!("\n- {server}: {}{tail}", list.join(", ")));
        }
        out
    }
}

/// Which MCP tools are offered whole and which shortened, under a share of `budget` tokens.
struct Plan {
    /// The MCP tools in order, as (server, spec): the first `whole` are offered as they are.
    tools: Vec<(String, ToolSpec)>,
    whole: usize,
    line: usize,
}

impl ToolSet {
    /// The specs as [`ToolSet::specs_for`] gives them, with the MCP tools held to `mcp_tokens` (when given): the
    /// ones past it are offered by name and one line.
    pub fn specs_with(&self, compact: bool, mcp_tokens: Option<u64>) -> Vec<ToolSpec> {
        let mut specs = self.specs_base(compact);
        let Some(budget) = mcp_tokens else { return specs };
        let Some(plan) = self.mcp_plan(&specs, budget) else { return specs };
        let short: Vec<String> = plan.tools[plan.whole..].iter().map(|(_, s)| s.name.clone()).collect();
        for s in specs.iter_mut().filter(|s| short.contains(&s.name)) {
            *s = short_spec(s, plan.line);
        }
        specs
    }

    /// What [`ToolSet::specs_with`] shortens under `budget` tokens, when it does.
    pub fn mcp_shortened(&self, compact: bool, budget: u64) -> Option<McpShortened> {
        // most sessions have no MCP tools: nothing to build the specs for
        if !self.entries().any(|t| t.server().is_some()) {
            return None;
        }
        let specs = self.specs_base(compact);
        let plan = self.mcp_plan(&specs, budget)?;
        let mut servers: Vec<(String, Vec<String>)> = vec![];
        let mut offered = 0;
        for (i, (server, spec)) in plan.tools.iter().enumerate() {
            if i < plan.whole {
                offered += tokens(spec);
                continue;
            }
            offered += tokens(&short_spec(spec, plan.line));
            match servers.iter_mut().find(|(s, _)| s == server) {
                Some((_, names)) => names.push(spec.name.clone()),
                None => servers.push((server.clone(), vec![spec.name.clone()])),
            }
        }
        let whole = plan.tools.iter().map(|(_, s)| tokens(s)).sum();
        Some(McpShortened { servers, tools: plan.tools.len(), whole, offered, budget })
    }

    /// The specs of every tool, as [`ToolSet::specs_for`] always gave them.
    fn specs_base(&self, compact: bool) -> Vec<ToolSpec> {
        self.entries()
            .map(|t| match t.compact() {
                Some(c) if compact || t.rare() => {
                    let mut spec = c.apply(t.spec());
                    t.trim_compact(&mut spec);
                    spec
                }
                _ => t.spec(),
            })
            .collect()
    }

    fn entries(&self) -> impl Iterator<Item = &Arc<dyn Tool>> {
        self.tools.values()
    }

    /// The MCP tools among `specs` (in the order of the set) and how many may stay whole, or `None` when all of
    /// them fit in `budget` tokens as they are. The line a shortened tool keeps is the longest that lets them
    /// all fit; when none does, the shortest and every tool is shortened.
    fn mcp_plan(&self, specs: &[ToolSpec], budget: u64) -> Option<Plan> {
        let tools: Vec<(String, ToolSpec)> =
            self.entries().zip(specs).filter_map(|(t, s)| Some((t.server()?.to_string(), s.clone()))).collect();
        if tools.iter().map(|(_, s)| tokens(s)).sum::<u64>() <= budget {
            return None;
        }
        let mut best = Plan { tools: vec![], whole: 0, line: LINES[LINES.len() - 1] };
        for line in LINES {
            // the most tools that stay whole while the rest, shortened, still fit
            let short: Vec<u64> = tools.iter().map(|(_, s)| tokens(&short_spec(s, line))).collect();
            let whole_cost: Vec<u64> = tools.iter().map(|(_, s)| tokens(s)).collect();
            let fits = |k: usize| whole_cost[..k].iter().sum::<u64>() + short[k..].iter().sum::<u64>() <= budget;
            if fits(0) {
                let whole = (0..=tools.len()).take_while(|k| fits(*k)).last().unwrap_or(0);
                return Some(Plan { tools, whole, line });
            }
            best.line = line;
        }
        best.tools = tools;
        Some(best)
    }
}
