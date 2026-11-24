//! The sub-agent view's own events: state of the runs, transcript answers, streamed items and the person's
//! actions while a local run is open.

use super::*;
use crate::render::blocks::TASK_HEAD;
use nexo_core::subs::{SubInfo, SubItem, SubTranscript};

impl App {
    // ------------------------------------------------------------ events

    /// One of the four `Sub` events or the answer to a request. Nothing here draws unless the run is open.
    pub(in crate::app) fn handle_sub(&mut self, ev: AgentEvent) {
        match ev {
            AgentEvent::SubState { info } => self.sub_state(info),
            AgentEvent::Sub { id, item } => {
                let Some(r) = self.sub_runs.iter_mut().find(|r| r.info.id == id) else { return };
                r.seen += 1;
                let (count, exact) = (r.seen, r.exact);
                let Some(v) = self.sub_view.as_mut().filter(|v| !v.is_remote() && v.id == id && !v.main_in) else {
                    return;
                };
                if matches!(item, SubItem::Person { .. }) && v.pending > 0 {
                    v.pending -= 1;
                    self.dirty = true;
                }
                if !exact {
                    return self.sub_ask();
                }
                // before the answer, and up to the count it holds, the item is in the view already
                if !v.answered || count <= v.base {
                    if v.asking.is_some() {
                        v.again = true;
                    }
                    return;
                }
                v.grown += 1;
                self.sub_apply(item);
                // Byte eviction can happen after any item, not only after KEEP_ITEMS.
                self.sub_ask();
            }
            AgentEvent::SubDelta { id, .. } => {
                // Deltas and snapshot answers travel on different queues. Re-read the
                // authoritative bounded partial, coalescing events while a read is pending.
                if self.sub_view.as_ref().is_some_and(|v| !v.is_remote() && v.id == id && !v.main_in) {
                    self.sub_ask();
                }
            }
            AgentEvent::SubNotDelivered { id, why, text } => {
                if let Some(v) = self.sub_view.as_mut().filter(|v| !v.is_remote() && v.id == id && !v.main_in) {
                    v.pending = v.pending.saturating_sub(1);
                    let draft = self.input.full_text();
                    let recovered = recover_rejected(&draft, &text);
                    self.input.clear();
                    self.input.set_text(&recovered);
                    self.dirty = true;
                    self.toast(Kind::Error, &format!("not delivered: {why} · restored in editor"));
                } else {
                    self.input.push_history(&text);
                    self.toast(Kind::Error, &format!("not delivered: {why} · recover from input history"));
                }
            }
            AgentEvent::SubTranscript { id, request, transcript }
                if self
                    .sub_view
                    .as_ref()
                    .is_some_and(|v| !v.is_remote() && v.id == id && !v.main_in && v.asking == Some(request)) =>
            {
                self.sub_answer(&id, transcript)
            }
            _ => {}
        }
    }

    /// A run's line: the first state with a target adds it, the later ones redraw it in place.
    fn put_agent_line(&mut self, run: crate::render::blocks::AgentRun) {
        if let Some(card) = self.task_card_for_run(&run.id) {
            self.invalidate(card);
        }
        match self.tasks.by_run.get(&run.id).copied() {
            Some(i) => {
                self.blocks[i] = Block::Agent(run);
                self.cache[i] = None;
            }
            None => {
                self.push(Block::Agent(run));
            }
        }
    }

    pub(super) fn sub_state(&mut self, info: SubInfo) {
        let id = info.id.clone();
        let line = agent_run(&info);
        // the state of a run the app knows, unchanged: the agent sends that after a lag
        let mut resync = false;
        match self.sub_runs.iter_mut().find(|r| r.info.id == id) {
            Some(r) => {
                resync = r.info.state == info.state;
                if resync {
                    r.exact = false;
                }
                r.info = info;
            }
            None => {
                let exact = info.state.running() && info.tokens_in == 0 && info.tokens_out == 0;
                self.sub_runs.push(RunRec { info, seen: 0, exact });
            }
        }
        if self.sub_runs.len() > MAX_RUNS {
            let open = self.sub_view.as_ref().map(|v| v.id.clone());
            let gone = self
                .sub_runs
                .iter()
                .filter(|r| !r.info.state.running() && Some(&r.info.id) != open.as_ref())
                .min_by_key(|r| r.info.ended_ms)
                .map(|r| r.info.id.clone());
            if let Some(g) = gone {
                self.sub_runs.retain(|r| r.info.id != g);
            }
        }
        // A launch may come from task, a workflow, or ultracode. Announce it in the main
        // transcript as soon as its effective target is known, even when its view is open.
        // Its line then follows the run: a level resolved late, and dim once it ends.
        if let Some(run) = line {
            self.on_main(|a| a.put_agent_line(run));
            self.dirty = true;
        }
        let runs = self.sub_job_runs();
        if let Some(Modal::Jobs(v)) = &mut self.modal {
            v.set_runs(runs);
            self.dirty = true;
        }
        if self.sub_view.as_ref().is_some_and(|v| !v.is_remote() && v.id == id && !v.main_in) {
            self.dirty = true;
            if resync {
                self.sub_ask();
            }
        }
    }

    /// The answer: the view is built from it.
    pub(super) fn sub_answer(&mut self, id: &str, t: Option<SubTranscript>) {
        let n = t.as_ref().map_or(0, |t| (t.dropped + t.items.len()) as u64);
        let again = {
            let Some(v) = self.sub_view.as_mut() else { return };
            v.asking = None;
            std::mem::take(&mut v.again)
        };
        // Compare the old prefix only when there is interaction state to preserve.
        // Snapshots can arrive frequently while a sub-agent streams; formatting every
        // historical block twice otherwise copies the entire transcript needlessly.
        let old_selection = self.selection.take();
        let old_selected = self.selected;
        let old_reveal = self.reveal;
        let preserve = old_selection.is_some()
            || old_selected.is_some()
            || old_reveal.is_some()
            || self.looks.iter().any(|l| l.folded)
            || self
                .blocks
                .iter()
                .any(|b| matches!(b, Block::Tool { folded: false, .. } | Block::Group { folded: false, .. }));
        let old_blocks = if preserve { std::mem::take(&mut self.blocks) } else { Vec::new() };
        let old_looks = if preserve { std::mem::take(&mut self.looks) } else { Vec::new() };
        let old_offsets = if old_selection.is_some() { self.offsets.clone() } else { Vec::new() };
        let old_total_lines = self.total_lines;
        self.read_back = None;
        self.blocks.clear();
        self.tasks = TaskRows::default();
        self.cache.clear();
        self.looks.clear();
        self.offsets.clear();
        self.total_lines = 0;
        self.selected = None;
        self.reveal = None;
        self.selection = None;
        // Keep the search query and focus; matches are recomputed against the new layout.
        self.inline_flushed = 0;
        match t {
            Some(t) => {
                if t.dropped > 0 {
                    self.push(Block::Notice {
                        level: Level::Info,
                        text: format!("{} earlier conversation items no longer kept", t.dropped),
                    });
                }
                for item in t.items {
                    self.sub_apply(item);
                }
                if !t.partial.is_empty() {
                    self.sub_delta(&t.partial);
                }
            }
            None => {
                self.push(Block::Notice { level: Level::Info, text: "this conversation is no longer kept".into() });
            }
        }
        // history is not an arrival: nothing fades in
        for l in &mut self.looks {
            l.born = None;
        }
        if self.sub_view.as_ref().is_some_and(|v| !v.is_remote()) {
            if let Some(r) = self.sub_runs.iter_mut().find(|r| r.info.id == id) {
                if !r.exact || r.seen > n {
                    r.exact = false;
                    r.seen = n;
                }
            }
        }
        if let Some(v) = self.sub_view.as_mut() {
            v.answered = true;
            v.base = n;
            v.grown = 0;
        }
        // Compare content separately from identity: a completed call is still the
        // same call, but its changed output may invalidate a text selection.
        let same_content = |old: &Block, new: &Block| {
            let mut comparable = new.clone();
            match (old, &mut comparable) {
                (
                    Block::Tool { started, elapsed_ms, folded, .. },
                    Block::Tool { started: s, elapsed_ms: e, folded: f, .. },
                ) => {
                    *s = *started;
                    *e = *elapsed_ms;
                    *f = *folded;
                }
                (Block::Assistant { started, .. }, Block::Assistant { started: s, .. }) => *s = *started,
                (Block::Group { folded, .. }, Block::Group { folded: f, .. }) => *f = *folded,
                _ => {}
            }
            format!("{old:?}") == format!("{comparable:?}")
        };
        let mut unchanged = old_blocks.iter().zip(&self.blocks).take_while(|(a, b)| same_content(a, b)).count();
        // A group's header depends on all its members, not just its own fields.
        // If any member differs, selection must not extend into that header.
        for (i, block) in old_blocks.iter().enumerate().take(unchanged) {
            if let Block::Group { len, .. } = block {
                if i.saturating_add(1).saturating_add(*len) > unchanged {
                    unchanged = i;
                    break;
                }
            }
        }
        // A group is anchored by its first call, not its changing member count.
        // Check the anchor before mutably borrowing the rebuilt blocks.
        let group_anchors: Vec<bool> = old_blocks
            .iter()
            .zip(&self.blocks)
            .enumerate()
            .map(|(i, (old, new))| {
                matches!((old, new), (Block::Group { .. }, Block::Group { .. }))
                    && matches!((old_blocks.get(i + 1), self.blocks.get(i + 1)),
                        (Some(Block::Tool { id: a, .. }), Some(Block::Tool { id: b, .. })) if a == b)
            })
            .collect();
        for (i, ((old, old_look), (new, look))) in
            old_blocks.iter().zip(&old_looks).zip(self.blocks.iter_mut().zip(&mut self.looks)).enumerate()
        {
            let same_identity = match (old, &*new) {
                (Block::Tool { id: a, .. }, Block::Tool { id: b, .. }) => a == b,
                (Block::Group { .. }, Block::Group { .. }) => group_anchors[i],
                _ => same_content(old, new),
            };
            if !same_identity {
                break;
            }
            if let (Block::Group { folded, .. }, Block::Group { folded: f, .. }) = (old, &mut *new) {
                *f = *folded;
            }
            if !matches!(old, Block::Tool { .. }) {
                look.folded = old_look.folded;
            }
        }
        // Tool identities survive inserted group headers, reordering and changed output.
        // Ambiguous or empty IDs must not transfer interaction state to another call.
        let mut old_tools = std::collections::HashMap::new();
        for (i, block) in old_blocks.iter().enumerate() {
            if let Block::Tool { id, .. } = block {
                if !id.is_empty() {
                    old_tools.entry(id.as_str()).and_modify(|v| *v = None).or_insert(Some(i));
                }
            }
        }
        let mut new_counts = std::collections::HashMap::new();
        for block in &self.blocks {
            if let Block::Tool { id, .. } = block {
                *new_counts.entry(id.as_str()).or_insert(0usize) += 1;
            }
        }
        // Own the unique IDs before borrowing blocks mutably.
        let unique_new: std::collections::HashSet<String> =
            new_counts.into_iter().filter(|(_, n)| *n == 1).map(|(id, _)| id.to_owned()).collect();
        for (new, look) in self.blocks.iter_mut().zip(&mut self.looks) {
            if let Block::Tool { id, .. } = &*new {
                if !unique_new.contains(id) {
                    continue;
                }
                if let Some(Some(i)) = old_tools.get(id.as_str()) {
                    let old = &old_blocks[*i];
                    let content_unchanged = same_content(old, new);
                    if let (
                        Block::Tool { started, elapsed_ms, folded, .. },
                        Block::Tool { started: s, elapsed_ms: e, folded: f, .. },
                    ) = (old, new)
                    {
                        *s = *started;
                        if content_unchanged {
                            *e = *elapsed_ms;
                        }
                        *f = *folded;
                        look.folded = old_looks[*i].folded;
                    }
                }
            }
        }
        // A newly inserted group must not hide a previously visible open tool.
        for i in 0..self.blocks.len() {
            let Block::Tool { id, folded: false, .. } = &self.blocks[i] else { continue };
            let Some(Some(old_i)) = old_tools.get(id.as_str()) else { continue };
            if !unique_new.contains(id) || old_looks[*old_i].group_back.is_some() {
                continue;
            }
            if let Some(group) = self.group_of(i) {
                if let Block::Group { folded, .. } = &mut self.blocks[group] {
                    *folded = false;
                }
            }
        }
        // Content equality alone is insufficient if ambiguous identities prevented
        // restoring a fold: all subsequent text offsets may have moved.
        for i in 0..unchanged {
            let fold_changed = match (&old_blocks[i], &self.blocks[i]) {
                (Block::Tool { folded: a, .. }, Block::Tool { folded: b, .. })
                | (Block::Group { folded: a, .. }, Block::Group { folded: b, .. }) => a != b,
                _ => false,
            };
            if fold_changed || old_looks[i].folded != self.looks[i].folded {
                unchanged = i;
                break;
            }
        }
        if self.search.is_some() {
            self.layout(self.last_width);
            self.rematch();
        }
        self.selected = old_selected.filter(|&i| i < unchanged);
        self.reveal = old_reveal.filter(|&i| i < unchanged);
        self.selection = old_selection.filter(|sel| {
            let last = sel.anchor.line.max(sel.head.line);
            let end = old_offsets.get(unchanged).copied().unwrap_or(old_total_lines);
            last < end
        });
        self.dirty = true;
        if again {
            if let Some(v) = self.sub_view.as_mut() {
                v.again = true;
            }
            self.sub_ask();
        }
    }

    /// One item of the run's conversation, drawn by the blocks the main chat uses.
    pub(super) fn sub_apply(&mut self, item: SubItem) {
        match item {
            // the main agent's task, not the person's words: a notice with the task under its head
            SubItem::Task { text } => {
                self.close_streaming();
                self.push(Block::Notice { level: Level::Info, text: format!("{TASK_HEAD}\n{text}") });
            }
            SubItem::Person { text } => {
                self.close_streaming();
                self.push(Block::User { text, at: None });
                self.scroll_to_end();
            }
            // what the agent that started it sent (`send_message` to its job): a message it read, not the person's
            SubItem::Parent { text } => {
                self.close_streaming();
                let stage = nexo_core::mail::Stage::Read;
                self.push(Block::Mail { id: String::new(), outgoing: false, peer: "main agent".into(), text, stage });
                self.scroll_to_end();
            }
            SubItem::Text { text } => {
                if let Some(Block::Assistant { text: t, streaming: true, .. }) = self.blocks.last_mut() {
                    *t = text;
                    self.close_streaming();
                    self.invalidate(self.blocks.len() - 1);
                } else {
                    self.close_streaming();
                    self.push(Block::Assistant { text, streaming: false, at: None, started: None, took_ms: None });
                }
            }
            SubItem::Call { id, name, summary, args } => {
                self.close_streaming();
                let started = Some(self.now());
                self.push(Block::Tool {
                    id,
                    name: safe_tool_text(&name),
                    summary: safe_tool_text(&summary),
                    args: safe_tool_args(&args),
                    status: ToolStatus::Running,
                    progress: Vec::new(),
                    output: None,
                    started,
                    elapsed_ms: None,
                    folded: true,
                });
            }
            SubItem::Result { id, ok, bytes: _, output: content } => {
                let now = self.now();
                if let Some(i) = self.tool_index(&id) {
                    if let Block::Tool { status, output, started, elapsed_ms, .. } = &mut self.blocks[i] {
                        *status = if ok { ToolStatus::Done } else { ToolStatus::Failed };
                        *output = Some(if ok {
                            ToolOutput::ok(safe_tool_text(&content))
                        } else {
                            ToolOutput::error(safe_tool_text(&content))
                        });
                        let ms = started.map_or(0, |s| now.saturating_duration_since(s).as_millis() as u64);
                        *elapsed_ms = (ms > 0).then_some(ms);
                    }
                    self.invalidate(i);
                }
            }
        }
    }

    /// Part of the reply being written, as the main chat shows its own.
    pub(super) fn sub_delta(&mut self, text: &str) {
        match self.blocks.last_mut() {
            Some(Block::Assistant { text: t, streaming: true, .. }) => {
                t.push_str(text);
                self.invalidate(self.blocks.len() - 1);
            }
            _ => {
                let started = Some(self.now());
                self.push(Block::Assistant {
                    text: text.to_string(),
                    streaming: true,
                    at: None,
                    started,
                    took_ms: None,
                });
            }
        }
    }

    // ------------------------------------------------------------ the person's actions

    /// Enter in the view: the text goes to this run and to no one else.
    pub(in crate::app) fn submit_sub(&mut self) {
        let text = self.input.full_text();
        let trimmed = text.trim();
        if trimmed.is_empty() {
            if let Some(i) = self.selected {
                self.toggle_block(i);
            }
            return;
        }
        if trimmed.starts_with('/') && !starts_with_path(trimmed) {
            self.fx.reject_at = self.fx_on().then(|| self.now());
            let back = format!("commands run in the main chat · Esc: back to {}", self.sub_back_target());
            self.toast(Kind::Info, &back);
            return;
        }
        #[cfg(any(unix, test))]
        if self.sub_view.as_ref().is_some_and(|v| v.is_remote()) {
            self.submit_remote(trimmed.to_owned());
            return;
        }
        let Some(id) = self.sub_view.as_ref().map(|v| v.id.clone()) else { return };
        let trimmed = trimmed.to_string();
        self.input.push_history(&trimmed);
        self.input.clear();
        self.send(AgentCommand::SubSubmit { id, text: trimmed });
        if let Some(v) = self.sub_view.as_mut() {
            v.pending += 1;
        }
        self.scroll_to_end();
        self.dirty = true;
    }

    /// Esc in the view: the text goes first, then the view. Nothing is interrupted.
    pub(in crate::app) fn esc_sub(&mut self) {
        if self.input.is_empty() {
            self.close_sub();
            self.stepped_back_at = Some(self.now());
        } else {
            self.keep_draft();
            self.input.clear();
            self.flash("draft saved, ↑ brings it back · Esc again goes back");
        }
        self.dirty = true;
    }
}
