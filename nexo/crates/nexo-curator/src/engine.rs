//! The goal engine: what of the conversation the next model call sees.

use std::sync::Arc;

use indexmap::IndexMap;
use rustc_hash::{FxHashMap, FxHashSet};
use serde_json::Value;

use crate::goal::Threads;
use crate::hindsight::{Interner, Set, Sym, Tracer};
use crate::learn::Featurizer;
use crate::ledger::Names;
use crate::rules::Rules;

use self::cache::{Pocket, Probe};
use self::dump::Dump;
use self::dups::Dup;
use self::files::Wrote;
use self::recall::ReadMade;
use self::steps::Step;
use self::view::{Built, Visible};

mod added;
mod anchor;
mod cache;
mod cap;
mod decide;
mod dump;
mod dups;
mod files;
mod fixes;
mod idle;
mod ingest;
mod named;
mod notes;
mod own;
mod params;
mod recall;
mod stale;
mod steps;
mod view;

pub use added::{keywords, label, Added, AddedKind, AddedState};
pub use params::Params;

/// The name of the tool whose input is a note (`Finding:`/`Plan:`/`Rule:` lines kept by the curator).
pub const NOTE_TOOL: &str = "note";

/// The tool the host may offer to bring a message back exactly, by the `m#####` name the curator gives it.
pub const RECALL_TOOL: &str = "recall";

/// What a silent agent is asked for (`Params.nudge`), at the end of the last tool result: a `note` call,
/// since a model that only calls tools answers a tool, not a request for text.
pub const NUDGE: &str = "[curator] Before the next step, keep what these results settled with the `note` tool: one line beginning with \"Finding:\" for each fact (values, formulas, formats, causes) and one line beginning with \"Plan:\" with what is left; then go on with what you were doing (a note records what you read, it does not stand in for reading the rest). The older results will leave this context; the notes stay.";

/// The same request for a host that offers no `note` tool: the lines go at the start of the reply, where
/// the curator reads them as well.
pub const NUDGE_SAID: &str = "[curator] Before the next step, keep what these results settled: start your next reply with one line beginning with \"Finding:\" for each fact (values, formulas, formats, causes) and one line beginning with \"Plan:\" with what is left; then go on with what you were doing (a note records what you read, it does not stand in for reading the rest). The older results will leave this context; the lines stay.";

pub struct Engine {
    p: Params,
    /// Tokens the view may take in the model's window, after the system prompt, the tools and the reply
    /// (0: no limit). The host gives it with each call ([`Engine::set_budget`]).
    budget: i64,
    int: Interner,
    tr: Tracer,
    fz: Featurizer,
    goal: Threads,
    sizes: Vec<i64>,
    raw_tokens: i64,
    dropped: FxHashMap<usize, FxHashSet<usize>>,
    /// For measurement: where the features of what the view holds, and what each unit came to be used
    /// by, are written (`Engine::dump_to`).
    dump: Option<Dump>,
    own: own::Own,
    /// The outputs of the request still open that no later agent message drew on, and since when
    /// (`Params.live`).
    live_since: FxHashMap<usize, usize>,
    /// The outputs `live_drop` took out, whole or down to their leads.
    led: FxHashSet<usize>,
    /// The lines an output taken out by `live_drop` keeps: its leads.
    lead_lines: FxHashMap<usize, Vec<String>>,
    /// The parts left out because a later output in the view sends every line of them (result, part).
    copied: FxHashSet<(usize, usize)>,
    /// The parts that send the lines of a part left out as a copy (`copied`): they stay while it is out, or
    /// the stub that says a later output sends them would point to nothing.
    carrying: FxHashSet<(usize, usize)>,
    start: usize,
    map_text: String,
    map_ver: u64,
    gone: IndexMap<String, i64>,
    gone_tokens: i64,
    gone_commands: i64,
    checked: i64,
    checked_at: usize,
    cap: i64,
    floor: f64,
    built: FxHashMap<usize, Built>,
    born: Vec<usize>,
    held: FxHashMap<(usize, usize), f64>,
    held_at: usize,
    pub stats: IndexMap<&'static str, i64>,
    names: Option<Names>,
    pool: Vec<(usize, String, Set, f64)>,
    // what the anchor keeps of the messages that left the view, in their own words (`harvest`)
    findings: Vec<(usize, String)>,
    plan: Option<(usize, String)>,
    /// The person's standing instructions: session state, kept for as long as the session lasts.
    rules: Rules,
    /// The anchor without the rules block (the map, the findings, the pinned files and the ledger).
    base_map: String,
    /// The rules block as the anchor has it now.
    rules_shown: String,
    /// file path (`file_key`) -> (read in message, path, text)
    pins: IndexMap<String, (usize, String, String)>,
    /// file path (`file_key`) -> last message that wrote it (a tool or a shell command)
    written: FxHashMap<String, usize>,
    /// file path (`file_key`) -> the agent's last whole write of it, oldest first
    wrote: IndexMap<String, Wrote>,
    /// call id -> the whole writes its write replaced, until its result says whether it wrote the file
    replaced: FxHashMap<String, Vec<Wrote>>,
    /// call id -> what a write tool's call changes (each file, its message, the size of a partial edit), until
    /// its result says whether it changed anything: a failed edit leaves the file as it was
    pending_writes: FxHashMap<String, Vec<(String, usize, Option<usize>)>>,
    /// Agent steps since the last one with a note line, and since the last nudge.
    since_note: usize,
    since_nudge: usize,
    /// The messages whose last tool result carries the nudge (kept there, so the history stays as sent).
    nudged: FxHashSet<usize>,
    /// The oldest message a note put back to how it was sent before the request was nudged: the view changed
    /// from there, outside `decide`, which counts it.
    unnudged: Option<usize>,
    /// The messages of the view last decided, in order: what the provider cached.
    sent_vis: Arc<Vec<usize>>,
    asked: FxHashSet<Sym>,
    kept_tokens: i64,
    frozen: FxHashMap<usize, String>,
    vis: Option<Visible>,
    /// Whether the other topics leave the view: decided once for each request (`split_now`), and kept until the
    /// next change of topic.
    split: bool,
    /// The turn the decision was made for.
    split_turn: usize,
    last_raw: Value,
    /// Whether the host offers a `note` tool: the request for notes names it, or asks for the lines in the reply.
    note_tool: bool,
    /// Whether the host offers the `recall` tool, which brings a message back as it was by its `m#####` name.
    recall_tool: bool,
    /// The calls that read files, each with its message and its files (kept as the units come, so that
    /// `call_files` does not parse every call again on each request), how many units are in it, and how
    /// many distinct files hold each part of a name.
    calls_raw: Vec<(usize, Vec<Vec<String>>)>,
    sf: crate::flat::StepFiles,
    lead_ix: crate::flat::LeadIndex,
    calls_seen: usize,
    files_known: FxHashSet<Vec<String>>,
    part_files: FxHashMap<String, usize>,
    /// Every distinct line of the tool outputs, as a number (`Params.dedup`).
    line_ids: FxHashMap<String, u32>,
    /// The lines of each tool result as sent: `None` for the ones the result itself sends as a pointer.
    shown: FxHashMap<usize, Vec<Option<u32>>>,
    /// Where each window of lines starts in the tool results (result, line), latest last.
    runs_at: FxHashMap<[u32; 3], Vec<(usize, usize)>>,
    /// The lines already sent as a pointer to an earlier output (`asked=2`).
    pointed_lines: FxHashSet<u32>,
    /// The runs of each message's tool results that an earlier output holds too.
    dups: FxHashMap<usize, Vec<Dup>>,
    /// The runs that point to each tool result, as (message, index in `dups`), in the order they came.
    copies: FxHashMap<usize, Vec<(usize, usize)>>,
    /// The lines each message carries for the steps (`Params.close`): which left, and which came back.
    shut: FxHashMap<usize, Vec<String>>,
    /// The calls and outputs of closed requests: out of the view.
    shut_out: FxHashSet<usize>,
    /// The requests looked at for closing so far.
    shut_done: usize,
    /// The steps of the answered requests, in the view or out of it.
    steps: Vec<Step>,
    /// The words of each request, and in how many requests and how many steps each word is: a word in
    /// most of them names nothing in particular.
    requests: Vec<FxHashSet<Sym>>,
    requests_df: FxHashMap<Sym, usize>,
    step_df: FxHashMap<Sym, usize>,
    /// The step each message of the answered requests belongs to (an index into `steps`).
    step_at: FxHashMap<usize, usize>,
    /// The calls that read a file, by the file (`file_key`), and the commands that print files, by the
    /// command: each call's unit and the lines it asked for, oldest first. A call that asks again for what an
    /// earlier one gave, while that output is out of the view, is the agent bringing it back (`asked_back`).
    reads_made: FxHashMap<String, Vec<ReadMade>>,
    /// The edits and writes of files, by a tool or a shell command: (path, message, call id), and whether
    /// each call's result came back without an error (`Params.stale`).
    edits: Vec<(String, usize, String)>,
    edit_ok: FxHashMap<String, bool>,
    /// The files a shell command that only prints files read (`cat`, `sed -n`): by call id while it waits for
    /// its result, then by result unit, so a later edit of one makes the output out of date as a read's.
    shell_read: FxHashMap<String, Vec<String>>,
    shell_reads: FxHashMap<usize, Vec<String>>,
    /// The reads that left the view while a later edit had made them out of date: (path, message of the edit).
    stale_gone: FxHashMap<usize, (String, usize)>,
    /// Seconds, as the host gives them with each view (`Params.pockets`).
    pub now: f64,
    /// The last view sent on each topic, message by message as [`Engine::prints`] gives it, and when.
    pockets: FxHashMap<usize, Pocket>,
    /// The first message of the view the cache no longer holds, while `decide` weighs it (`usize::MAX`: all
    /// of it is held, or new).
    warm: usize,
    /// How long the provider keeps a view, as learned (`learn`): at least `life_lo`, less than `life_hi`
    /// seconds; and whether it ever said it read from its cache.
    life_lo: f64,
    life_hi: f64,
    read_seen: bool,
    /// What the cache gives the view last decided, by how long it keeps views (`learn`).
    probe: Option<Probe>,
    /// The hash of each message as it came (`prints`; 0: not taken yet).
    raw_fp: Vec<u64>,
}

impl Engine {
    pub fn new(p: Params) -> Engine {
        let mut goal = Threads::default();
        goal.link = p.link;
        Engine {
            cap: p.cap,
            floor: p.floor,
            p,
            int: Interner::default(),
            tr: Tracer::default(),
            fz: Featurizer::default(),
            goal,
            sizes: Vec::new(),
            raw_tokens: 0,
            dropped: FxHashMap::default(),
            dump: None,
            own: own::Own::default(),
            live_since: FxHashMap::default(),
            led: FxHashSet::default(),
            lead_lines: FxHashMap::default(),
            copied: FxHashSet::default(),
            carrying: FxHashSet::default(),
            start: 1,
            map_text: String::new(),
            map_ver: 0,
            gone: IndexMap::new(),
            gone_tokens: 0,
            gone_commands: 0,
            budget: 0,
            checked: 0,
            checked_at: 0,
            built: FxHashMap::default(),
            born: Vec::new(),
            held: FxHashMap::default(),
            held_at: 0,
            stats: IndexMap::new(),
            names: None,
            pool: Vec::new(),
            findings: Vec::new(),
            plan: None,
            rules: Rules::default(),
            base_map: String::new(),
            rules_shown: String::new(),
            pins: IndexMap::new(),
            written: FxHashMap::default(),
            wrote: IndexMap::new(),
            replaced: FxHashMap::default(),
            pending_writes: FxHashMap::default(),
            since_note: 0,
            since_nudge: usize::MAX,
            nudged: FxHashSet::default(),
            unnudged: None,
            sent_vis: Arc::default(),
            asked: FxHashSet::default(),
            kept_tokens: 0,
            frozen: FxHashMap::default(),
            vis: None,
            split: false,
            split_turn: usize::MAX,
            last_raw: Value::Null,
            note_tool: true,
            recall_tool: false,
            calls_raw: Vec::new(),
            sf: crate::flat::StepFiles::default(),
            lead_ix: crate::flat::LeadIndex::default(),
            calls_seen: 0,
            files_known: FxHashSet::default(),
            part_files: FxHashMap::default(),
            line_ids: FxHashMap::default(),
            shown: FxHashMap::default(),
            runs_at: FxHashMap::default(),
            pointed_lines: FxHashSet::default(),
            dups: FxHashMap::default(),
            copies: FxHashMap::default(),
            shut: FxHashMap::default(),
            shut_out: FxHashSet::default(),
            shut_done: 0,
            steps: Vec::new(),
            requests: Vec::new(),
            requests_df: FxHashMap::default(),
            step_df: FxHashMap::default(),
            step_at: FxHashMap::default(),
            reads_made: FxHashMap::default(),
            edits: Vec::new(),
            edit_ok: FxHashMap::default(),
            shell_read: FxHashMap::default(),
            shell_reads: FxHashMap::default(),
            stale_gone: FxHashMap::default(),
            now: 0.0,
            pockets: FxHashMap::default(),
            warm: usize::MAX,
            life_lo: 0.0,
            life_hi: f64::INFINITY,
            read_seen: false,
            probe: None,
            raw_fp: Vec::new(),
        }
    }

    /// The tools the host offers the model (their names). Without a `note` tool, the request for notes asks
    /// for the lines in the reply instead: a request for a tool that is not there is never answered, and
    /// in a measured run it stayed in view for nineteen requests.
    pub fn tools(&mut self, names: &[String]) {
        self.note_tool = names.iter().any(|n| n == NOTE_TOOL);
        self.recall_tool = names.iter().any(|n| n == RECALL_TOOL);
    }

    fn bump(&mut self, k: &'static str, v: i64) {
        *self.stats.entry(k).or_insert(0) += v;
    }
}

#[cfg(test)]
mod tests;
