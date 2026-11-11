//! The transcript: blocks, layout, scrolling, folding and search.

use super::*;

impl App {
    // ------------------------------------------------------------ blocks

    pub fn push(&mut self, b: Block) -> usize {
        // a block that is final is indexed now; a streaming one when it closes
        match &b {
            Block::Assistant { text, streaming: false, .. } => self.words.feed(text, crate::complete::Src::Other),
            Block::Tool { args, .. } => self.words.feed_json(args, crate::complete::Src::Other),
            // a command the person ran with `!`: its words are theirs, its output like a tool's
            Block::Shell { command, output, .. } => {
                self.words.feed(command, crate::complete::Src::User);
                self.words.feed(output, crate::complete::Src::Other);
            }
            _ => {}
        }
        let is_call = self.opts.group_tools && matches!(b, Block::Tool { .. });
        self.blocks.push(b);
        self.cache.push(None);
        let born = self.opts.animations.then(|| self.now());
        self.looks.push(Look { born, ..Look::default() });
        self.dirty = true;
        let at = self.blocks.len() - 1;
        if is_call {
            return self.join_group(at);
        }
        at
    }

    /// The call just pushed at `at` joins the run of calls before it (thinking in between is part of
    /// the run): the second call of a run puts a group block in front of the first. Returns where the
    /// call is now.
    pub(super) fn join_group(&mut self, at: usize) -> usize {
        let mut first = at;
        while first > 0 && matches!(self.blocks[first - 1], Block::Reasoning { .. }) {
            first -= 1;
        }
        let Some(prev) = first.checked_sub(1).filter(|p| matches!(self.blocks[*p], Block::Tool { .. })) else {
            return at;
        };
        if let Some(back) = self.looks[prev].group_back {
            let m = prev - back as usize;
            if let Block::Group { len, .. } = &mut self.blocks[m] {
                *len = at - m;
            }
            for k in first..=at {
                self.looks[k].group_back = Some((k - m) as u32);
            }
            self.cache[m] = None;
            return at;
        }
        // what the inline mode already wrote cannot take a block in front of it
        if prev < self.inline_flushed {
            return at;
        }
        let born = self.looks[prev].born;
        self.blocks.insert(prev, Block::Group { folded: true, len: at + 1 - prev });
        self.cache.insert(prev, None);
        self.looks.insert(prev, Look { born, ..Look::default() });
        for k in prev + 1..=at + 1 {
            self.looks[k].group_back = Some((k - prev) as u32);
        }
        // the indices at and after the new block moved up by one
        let shift = |i: &mut usize| {
            if *i >= prev {
                *i += 1;
            }
        };
        self.selected.iter_mut().for_each(shift);
        self.reveal.iter_mut().for_each(shift);
        if matches!(self.hover, Hover::Head(_)) {
            self.hover = Hover::Nothing;
        }
        at + 1
    }

    /// The group block that block `i` belongs to.
    pub(super) fn group_of(&self, i: usize) -> Option<usize> {
        let back = self.looks.get(i)?.group_back?;
        i.checked_sub(back as usize).filter(|m| matches!(self.blocks.get(*m), Some(Block::Group { .. })))
    }

    /// Whether block `i` is left out of the transcript: a finished call of a folded group, which
    /// its group's line stands for. A call still running or waiting stays in sight.
    pub fn hidden(&self, i: usize) -> bool {
        match self.group_of(i) {
            Some(m) => {
                let kept = match &self.blocks[i] {
                    // the agreed plan and thinking the user opened stay in sight
                    Block::Tool { name, output, .. } => {
                        // what nexo saved for later sessions is said in sight, not folded away
                        name == "exit_plan"
                            || matches!(output, Some(o) if matches!(o.display, ToolDisplay::Remembered { .. }))
                    }
                    Block::Reasoning { folded, text, .. } => !*folded && !text.trim().is_empty(),
                    _ => false,
                };
                matches!(self.blocks[m], Block::Group { folded: true, .. }) && !self.blocks[i].is_live() && !kept
            }
            None => false,
        }
    }

    pub fn invalidate(&mut self, i: usize) {
        if let Some(c) = self.cache.get_mut(i) {
            *c = None;
        }
        // a group line is made from its calls
        if let Some(m) = self.group_of(i) {
            self.cache[m] = None;
        }
        self.dirty = true;
    }

    /// The price of the output of tool call `id` in the last request: its tokens at the cached or the
    /// full rate, from `Pricing::cost`. None unless the model's prices are all given (input, output,
    /// cache read), the mark says the output was sent whole, and its tokens are known.
    pub(super) fn call_cost(&self, id: &str, m: CallMark) -> Option<f64> {
        let p = self.limits.as_ref()?.pricing.filter(|p| p.input > 0.0 && p.output > 0.0 && p.cache_read > 0.0)?;
        let tokens = *self.call_tokens.get(id)?;
        let usage = match m {
            CallMark::Cached => nexo_core::stream::Usage { cache_read: tokens, ..Default::default() },
            CallMark::Sent => nexo_core::stream::Usage { input: tokens, ..Default::default() },
            _ => return None,
        };
        Some(p.cost(&usage))
    }

    pub fn invalidate_all(&mut self) {
        self.cache.fill(None);
        self.dirty = true;
    }

    pub fn render_opts(&self) -> RenderOpts {
        let since = self.now().saturating_duration_since(self.opened).as_millis() as u64;
        RenderOpts {
            show_reasoning: self.show_reasoning,
            fold_after: self.fold_after,
            spinner: self.spinner,
            intro_ms: (self.opts.animations && since < crate::render::blocks::INTRO_MS).then_some(since),
            interpreted: self.interpreted,
            timestamps: self.timestamps,
            fx: self.fx_on(),
            detail: self.opts.detail,
        }
    }

    /// How many lines block `i` has at `width`; kept when its lines are evicted.
    pub fn height(&mut self, i: usize, width: usize) -> usize {
        match &self.cache[i] {
            Some((w, h, _, _)) if *w == width => *h,
            _ => self.lines(i, width).len(),
        }
    }

    /// Drops the lines of blocks more than three screens from the view, so a long session keeps
    /// only what it can show soon; the heights stay, and a block comes back when scrolled to.
    pub fn evict_far(&mut self, top: usize, height: usize) {
        let reach = 3 * height.max(1);
        let lo = top.min(self.scroll).saturating_sub(reach);
        let hi = top.max(self.scroll) + height + reach;
        let now = self.now();
        for i in 0..self.cache.len().min(self.offsets.len()) {
            let Some((_, h, lines @ Some(_), meta)) = self.cache[i].as_mut() else { continue };
            let (start, end) = (self.offsets[i], self.offsets[i] + *h);
            let busy = self.blocks[i].is_live()
                || self.looks.get(i).is_some_and(|l| {
                    l.born.is_some_and(|t| now.saturating_duration_since(t).as_millis() <= FADE_MS as u128 + 100)
                        || l.opened.is_some_and(|(t, _)| t.elapsed().as_millis() <= OPEN_MS as u128 + 100)
                });
            if (end < lo || start > hi) && !busy {
                *lines = None;
                *meta = Default::default();
            }
        }
    }

    /// The lines of block `i` at `width`, rendered once per width.
    pub fn lines(&mut self, i: usize, width: usize) -> &[Line<'static>] {
        let fresh = match &self.cache[i] {
            Some((w, _, l, _)) => *w != width || l.is_none(),
            None => true,
        };
        if fresh {
            let indent = indent_of(&self.blocks[i], width);
            let mut meta = crate::regions::Meta::default();
            let mut lines = match &self.blocks[i] {
                // a group's line is made from the calls it stands for
                Block::Group { folded, len } => {
                    let end = (i + 1 + len).min(self.blocks.len());
                    let head = crate::render::group::header_for(
                        &self.blocks[i + 1..end],
                        *folded,
                        self.spinner,
                        (width - indent).max(8),
                        &self.theme,
                        self.opts.detail,
                    );
                    vec![head]
                }
                b => {
                    let (l, m) = b.render_meta(width - indent, &self.theme, self.render_opts());
                    meta = m;
                    l
                }
            };
            crate::render::wrap::clean_lines(&mut lines);
            if let Block::Tool { id, output: Some(_), .. } = &self.blocks[i] {
                if let (Some(m), Some(head)) = (self.marks.get(id), lines.first_mut()) {
                    let cost = self.call_cost(id, *m);
                    mark_head(head, *m, cost, width - indent, &self.theme);
                }
            }
            if self.opts.animations && matches!(self.blocks[i], Block::Tool { .. } | Block::Group { .. }) {
                let now = self.now();
                let card = if self.blocks[i].is_live() {
                    Some(crate::motion::Card::Running {
                        now_ms: now.saturating_duration_since(self.opened).as_millis() as u64,
                    })
                } else {
                    self.looks.get(i).and_then(|l| l.done_at).map(|t| crate::motion::Card::Done {
                        since_ms: now.saturating_duration_since(t).as_millis() as u64,
                    })
                };
                if let Some(card) = card {
                    crate::motion::card_fx(&mut lines, card, &self.theme);
                }
            }
            if self.fx_on() {
                self.stream_fx(i, &mut lines, meta.stamped);
            }
            if self.looks.get(i).is_some_and(|l| l.folded) && folds_alone(&self.blocks[i]) && lines.len() > 1 {
                let hidden = lines.len() - 1;
                lines.truncate(1);
                meta.truncate(1);
                lines.push(Line::from(Span::styled(format!("  ▸ {hidden} more lines"), self.theme.dim())));
            }
            debug_assert!(meta.conts.iter().all(|c| (c.line as usize) < lines.len()));
            if indent > 0 {
                meta.add_deco(indent);
                meta.margin = indent as u16;
                let margin = " ".repeat(indent);
                for l in &mut lines {
                    l.spans.insert(0, Span::raw(margin.clone()));
                }
            }
            self.cache[i] = Some((width, lines.len(), Some(lines), meta));
        }
        self.cache[i].as_ref().and_then(|(_, _, l, _)| l.as_deref()).unwrap_or(&[])
    }

    /// Recomputes offsets at `width`. A blank line follows every non-empty block, two come
    /// before a message of the person (a new turn), and none sits between the one-line steps
    /// of a turn, which read as a list.
    pub fn layout(&mut self, width: usize) {
        // a new width re-wraps every line: keep the block and the line inside it at the top of the
        // view, so the text the person was reading stays under their eyes
        let anchor = (width != self.last_width && !self.follow && !self.offsets.is_empty()).then(|| {
            let bi = match self.offsets.binary_search(&self.scroll) {
                Ok(i) => i,
                Err(i) => i.saturating_sub(1),
            };
            (bi, self.scroll.saturating_sub(self.offsets[bi]))
        });
        self.last_width = width;
        // a call that finished inside a folded group leaves the view: its group takes the selection
        if let Some(m) = self.selected.filter(|s| self.hidden(*s)).and_then(|s| self.group_of(s)) {
            self.selected = Some(m);
        }
        let mut off = 0;
        self.offsets.clear();
        for i in 0..self.blocks.len() {
            self.offsets.push(off);
            if i < self.inline_flushed {
                continue;
            }
            let n = self.shown(i, width);
            off += n + self.gap_after(i, n);
        }
        self.total_lines = off;
        if let Some((bi, j)) = anchor.filter(|(bi, _)| *bi < self.offsets.len()) {
            let end = self.offsets.get(bi + 1).copied().unwrap_or(self.total_lines);
            let top = self.offsets[bi];
            self.scroll = (top + j).min(end.saturating_sub(1).max(top));
            // the view lands at once: no glide across a re-wrapped text
            self.scroll_fx.snap(self.scroll as f64);
        }
    }

    /// Blank lines after block `i`, which shows `n` lines.
    pub(crate) fn gap_after(&self, i: usize, n: usize) -> usize {
        if n == 0 {
            return 0;
        }
        // the banner is its own frame: one blank line under it is enough
        if matches!(self.blocks[i], Block::Welcome { .. }) {
            return 1;
        }
        // the calls a folded group leaves out are not there to be next to
        let mut at = i + 1;
        while self.hidden(at) {
            at += 1;
        }
        match self.blocks.get(at) {
            Some(Block::User { .. }) => 2,
            Some(next) if n == 1 && is_step(&self.blocks[i]) && is_step(next) => 0,
            _ => 1,
        }
    }

    pub fn max_scroll(&self, height: usize) -> usize {
        self.total_lines.saturating_sub(height)
    }

    /// Settles the scroll after a layout: follows the end, or counts what arrived.
    pub fn settle(&mut self, height: usize) {
        if self.follow {
            self.scroll = self.max_scroll(height);
            self.seen_lines = self.total_lines;
        } else {
            self.scroll = self.scroll.min(self.max_scroll(height));
        }
    }

    pub fn unseen(&self) -> usize {
        if self.follow {
            0
        } else {
            self.total_lines.saturating_sub(self.seen_lines)
        }
    }

    pub fn scroll_by(&mut self, delta: i32) {
        let h = self.transcript_area.height as usize;
        let max = self.max_scroll(h);
        let next = (self.scroll as i64 + delta as i64).clamp(0, max as i64) as usize;
        self.scroll = next;
        self.follow = next >= max;
        if self.follow {
            self.seen_lines = self.total_lines;
        }
        self.wished = true;
        self.dirty = true;
    }

    pub fn scroll_to_top(&mut self) {
        self.scroll = 0;
        self.follow = self.total_lines <= self.transcript_area.height as usize;
        self.wished = true;
        self.dirty = true;
    }

    pub fn scroll_to_end(&mut self) {
        self.follow = true;
        self.seen_lines = self.total_lines;
        self.wished = true;
        self.dirty = true;
    }

    /// The block that holds absolute line `line`.
    pub fn block_at_line(&self, line: usize) -> Option<usize> {
        if self.offsets.is_empty() || line >= self.total_lines {
            return None;
        }
        // blocks that show nothing share their offset with the next one: the last of them is the one
        // that holds the line
        let i = self.offsets.partition_point(|o| *o <= line).checked_sub(1)?;
        if self.hidden(i) {
            return None;
        }
        // the blank separator after a block belongs to nobody
        let n = self.cache.get(i).and_then(|c| c.as_ref()).map(|(_, h, _, _)| *h).unwrap_or(0);
        if line < self.offsets[i] + n {
            Some(i)
        } else {
            None
        }
    }

    /// How many of block `i`'s lines are in sight: all, except while it is opening.
    pub fn shown(&mut self, i: usize, width: usize) -> usize {
        if self.hidden(i) {
            return 0;
        }
        let n = self.height(i, width);
        match self.looks.get(i).and_then(|l| l.opened) {
            Some((at, from)) if from < n => {
                let t = (at.elapsed().as_millis() as f32 / OPEN_MS as f32).min(1.0);
                let eased = 1.0 - (1.0 - t).powi(3);
                from + ((n - from) as f32 * eased).ceil() as usize
            }
            _ => n,
        }
    }

    /// How far block `i` has faded in (1 once it is settled).
    pub fn fade(&self, i: usize) -> f32 {
        match self.looks.get(i).and_then(|l| l.born) {
            Some(at) => (self.now().saturating_duration_since(at).as_millis() as f32 / FADE_MS as f32).min(1.0),
            None => 1.0,
        }
    }

    /// Whether block `i` folds: cards and thinking always, anything else of more than one line.
    pub fn foldable(&mut self, i: usize) -> bool {
        if self.companion(i).is_some() {
            return true;
        }
        let Some(b) = self.blocks.get(i) else { return false };
        // the turn's line opens to the whole figures in quiet; in full it is whole already
        if matches!(b, Block::Usage { .. }) {
            return self.quiet();
        }
        if b.is_foldable() {
            return true;
        }
        if !folds_alone(b) {
            return false;
        }
        self.looks[i].folded || self.lines(i, self.last_width).len() > 1
    }

    /// The `+ nexo added` line that follows the message of the person at `i`: what opens and folds with it.
    pub fn companion(&self, i: usize) -> Option<usize> {
        (matches!(self.blocks.get(i), Some(Block::User { .. }))
            && matches!(self.blocks.get(i + 1), Some(Block::Added { .. })))
        .then_some(i + 1)
    }

    pub fn toggle_block(&mut self, i: usize) {
        // a message opens to what nexo added to it, and folds back; a long one with it
        if let Some(c) = self.companion(i) {
            let open = self.is_folded(c);
            self.toggle_block(c);
            if self.looks[i].folded == open && self.lines(i, self.last_width).len() > 1 {
                self.looks[i].folded = !open;
                self.invalidate(i);
            }
            self.selected = Some(i);
            return;
        }
        if !self.foldable(i) {
            self.selected = Some(i);
            return;
        }
        if matches!(self.blocks[i], Block::Group { .. }) {
            return self.toggle_group(i);
        }
        let before = self.lines(i, self.last_width).len();
        if !self.blocks[i].toggle_fold() {
            self.looks[i].folded = !self.looks[i].folded;
        }
        self.looks[i].opened = self.opts.animations.then(|| (self.now(), before));
        self.invalidate(i);
        self.selected = Some(i);
    }

    /// A group of calls opens, or folds back to its line. Opening shows the calls it had left out,
    /// growing in; folding keeps the view inside the transcript and the selection on the group.
    pub(super) fn toggle_group(&mut self, m: usize) {
        let Block::Group { folded, len } = self.blocks[m] else { return };
        let end = (m + len + 1).min(self.blocks.len());
        let top = self.offsets.get(m).copied();
        let old_end = self.offsets.get(end).copied().unwrap_or(self.total_lines);
        let was_hidden: Vec<usize> = (m + 1..end).filter(|k| self.hidden(*k)).collect();
        self.blocks[m].toggle_fold();
        self.invalidate(m);
        if folded {
            if self.opts.animations {
                let now = self.now();
                for k in was_hidden {
                    self.looks[k].opened = Some((now, 0));
                }
            }
        } else if !self.follow {
            // the lines under the view may be gone: the view lands on the group instead of past it
            if let Some(top) = top.filter(|t| self.scroll > *t && self.scroll < old_end) {
                self.scroll = top;
                self.scroll_fx.snap(top as f64);
            }
        }
        self.selected = Some(m);
        self.dirty = true;
    }

    pub fn fold_all(&mut self) {
        // groups, tool cards and thinking fold together: one key for every body
        let any_open = self.blocks.iter().any(|b| {
            matches!(
                b,
                Block::Tool { folded: false, .. }
                    | Block::Reasoning { folded: false, .. }
                    | Block::Group { folded: false, .. }
            )
        });
        for (i, b) in self.blocks.iter_mut().enumerate() {
            if matches!(b, Block::Tool { .. } | Block::Reasoning { .. } | Block::Group { .. }) {
                b.set_fold(any_open);
                self.cache[i] = None;
            }
        }
        self.dirty = true;
        self.flash(if any_open {
            "tool groups, tool and thinking bodies folded"
        } else {
            "tool groups, tool and thinking bodies unfolded"
        });
    }

    pub fn toggle_reasoning(&mut self) {
        self.show_reasoning = !self.show_reasoning;
        self.invalidate_all();
        self.flash(if self.show_reasoning { "reasoning shown" } else { "reasoning hidden" });
    }

    pub fn toggle_interpreted(&mut self) {
        self.interpreted = !self.interpreted;
        self.invalidate_all();
        self.flash(if self.interpreted {
            "code shown for what it means: markdown rendered, json indented, tables aligned"
        } else {
            "code shown as source"
        });
    }

    /// Shift+Tab with no card selected: ask → accept-edits → plan → ask.
    pub fn cycle_mode(&mut self) {
        let next = match self.permission_mode {
            PermissionMode::Ask => PermissionMode::AcceptEdits,
            PermissionMode::AcceptEdits => PermissionMode::Plan,
            _ => PermissionMode::Ask,
        };
        self.send(AgentCommand::SetPermissionMode(next));
    }

    /// Tab / Shift+Tab: the next or previous block that folds, round the transcript; with
    /// none selected, the last one.
    pub fn select_next(&mut self, forward: bool) {
        let n = self.blocks.len();
        if n == 0 {
            return;
        }
        let Some(mut i) = self.selected else {
            self.select_last();
            return;
        };
        for _ in 0..n {
            i = if forward { (i + 1) % n } else { (i + n - 1) % n };
            if self.navigable(i) {
                self.select(i);
                return;
            }
        }
    }

    /// A block the keys stop at: one that folds and shows something.
    /// The turn's time line opens by a click, but is no stop: it would cost one more press per turn.
    pub fn navigable(&mut self, i: usize) -> bool {
        if matches!(self.blocks.get(i), Some(Block::Usage { .. })) {
            return false;
        }
        !self.hidden(i) && self.foldable(i) && !self.lines(i, self.last_width).is_empty()
    }

    pub(super) fn select(&mut self, i: usize) {
        self.selected = Some(i);
        self.scroll_into_view(i);
        self.dirty = true;
    }

    /// Whether there is a block the keys can step into (what the `Shift+↑ blocks` hint promises).
    /// Once anything was said the answer is yes without looking at every block (a long session keeps the
    /// lines of the blocks in view only); before that it is exact, the few notices under the banner asked.
    pub fn has_navigable(&mut self) -> bool {
        !self.fresh() || (0..self.blocks.len()).rev().any(|i| self.navigable(i))
    }

    /// Shift+↑ (or Tab) on an empty draft: the last block, where the eye already is.
    pub fn select_last(&mut self) {
        match (0..self.blocks.len()).rev().find(|&i| self.navigable(i)) {
            Some(i) => self.select(i),
            None => self.flash_level(Level::Warn, "nothing to select yet"),
        }
    }

    /// ↑ / ↓ with a block selected: the previous or next one. ↑ stops at the first; ↓ past
    /// the last leaves the blocks for the draft and follows the end again.
    pub fn select_step(&mut self, down: bool) {
        let Some(mut i) = self.selected else {
            if !down {
                self.select_last();
            }
            return;
        };
        loop {
            if down {
                if i + 1 >= self.blocks.len() {
                    self.selected = None;
                    self.scroll_to_end();
                    return;
                }
                i += 1;
            } else if i == 0 {
                self.dirty = true;
                return;
            } else {
                i -= 1;
            }
            if self.navigable(i) {
                return self.select(i);
            }
        }
    }

    /// → / ← with a block selected: open it or fold it (nothing when it already is).
    pub fn open_selected(&mut self, open: bool) {
        let Some(i) = self.selected else { return };
        let shown = self.companion(i).unwrap_or(i);
        if i < self.blocks.len() && self.foldable(i) && self.is_folded(shown) == open {
            self.toggle_block(i);
        }
        self.scroll_into_view(i);
    }

    /// Whether block `i` shows only its head.
    pub fn is_folded(&self, i: usize) -> bool {
        match self.blocks.get(i) {
            Some(
                Block::Tool { folded, .. }
                | Block::Reasoning { folded, .. }
                | Block::Group { folded, .. }
                | Block::Added { folded, .. }
                | Block::Usage { folded, .. }
                | Block::Problem { folded, .. },
            ) => *folded,
            Some(_) => self.looks.get(i).is_some_and(|l| l.folded),
            None => false,
        }
    }

    /// Keeps block `i` in sight: its whole height when it fits, its head first when not.
    /// The draw keeps at it while the block grows open.
    pub(super) fn scroll_into_view(&mut self, i: usize) {
        self.reveal = Some(i);
        let w = self.last_width;
        self.layout(w);
        self.reveal_step();
    }

    /// One step of [`scroll_into_view`]: done once the block stopped growing.
    pub fn reveal_step(&mut self) {
        let Some(i) = self.reveal else { return };
        let h = self.transcript_area.height as usize;
        let w = self.last_width;
        let (Some(&off), true) = (self.offsets.get(i), i < self.blocks.len()) else {
            self.reveal = None;
            return;
        };
        let shown = self.shown(i, w);
        let full = self.lines(i, w).len();
        if h > 0 {
            let max = self.max_scroll(h);
            let mut s = self.scroll;
            if off + shown.max(1) > s + h {
                s = off + shown.max(1) - h;
            }
            s = s.min(off).min(max);
            if s != self.scroll || (self.follow && s < max) {
                self.scroll = s;
                self.follow = s >= max;
                self.dirty = true;
            }
        }
        if shown >= full {
            self.reveal = None;
        }
    }

    // ------------------------------------------------------------ search

    pub fn open_search(&mut self) {
        let q = self.search.as_ref().map(|s| s.query.clone()).unwrap_or_default();
        self.search = Some(Search { query: q, focused: true, matches: Vec::new(), current: 0, groups: Vec::new() });
        self.rematch();
        self.dirty = true;
    }

    pub fn close_search(&mut self) {
        self.search = None;
        self.dirty = true;
    }

    pub fn rematch(&mut self) {
        let width = self.last_width;
        let Some(q) = self.search.as_ref().map(|s| s.query.to_lowercase()) else { return };
        let mut matches = Vec::new();
        let mut groups = Vec::new();
        if !q.is_empty() {
            for i in 0..self.blocks.len() {
                let off = self.offsets.get(i).copied().unwrap_or(0);
                let hidden = self.hidden(i);
                let hit_lines: Vec<usize> = self
                    .lines(i, width)
                    .iter()
                    .enumerate()
                    .filter(|(_, l)| crate::render::markdown::text_of(l).to_lowercase().contains(&q))
                    .map(|(j, _)| j)
                    .collect();
                if hidden {
                    // a call the fold leaves out is found at its group's line, once per group
                    if let (false, Some(m)) = (hit_lines.is_empty(), self.group_of(i)) {
                        if !groups.contains(&m) {
                            groups.push(m);
                            matches.push(self.offsets.get(m).copied().unwrap_or(0));
                        }
                    }
                } else {
                    matches.extend(hit_lines.into_iter().map(|j| off + j));
                }
            }
            matches.sort_unstable();
            matches.dedup();
        }
        if let Some(s) = &mut self.search {
            s.matches = matches;
            s.groups = groups;
            s.current = s.current.min(s.matches.len().saturating_sub(1));
        }
        self.dirty = true;
    }

    pub fn search_jump(&mut self, forward: bool) {
        let h = self.transcript_area.height as usize;
        let Some(s) = &mut self.search else { return };
        if s.matches.is_empty() {
            return;
        }
        let n = s.matches.len();
        s.current = if forward { (s.current + 1) % n } else { (s.current + n - 1) % n };
        let line = s.matches[s.current];
        let line = self.open_hit(line);
        self.scroll = line.saturating_sub(h / 3).min(self.max_scroll(h));
        self.follow = false;
        self.dirty = true;
    }

    /// A hit that stands for the calls of a folded group opens the group; the line to go to is the
    /// first one of those calls that matches.
    pub(super) fn open_hit(&mut self, line: usize) -> usize {
        let Some(m) = self
            .search
            .as_ref()
            .and_then(|s| s.groups.iter().copied().find(|m| self.offsets.get(*m) == Some(&line)))
            .filter(|m| matches!(self.blocks[*m], Block::Group { folded: true, .. }))
        else {
            return line;
        };
        self.toggle_group(m);
        self.layout(self.last_width);
        self.rematch();
        let Some(s) = &mut self.search else { return line };
        // the first hit at or after the old line (the header itself may match too)
        s.current = s.matches.iter().position(|l| *l >= line).unwrap_or(s.matches.len().saturating_sub(1));
        s.matches.get(s.current).copied().unwrap_or(line)
    }

    /// Jumps to the first match without moving the counter (on typing).
    pub fn search_first(&mut self) {
        let h = self.transcript_area.height as usize;
        if let Some(line) = self.search.as_ref().and_then(|s| s.matches.get(s.current).copied()) {
            let line = self.open_hit(line);
            {
                self.scroll = line.saturating_sub(h / 3).min(self.max_scroll(h));
                self.follow = false;
            }
        }
        self.dirty = true;
    }

    /// Sends a git command's prompt as the next request, or says why there is none.
    pub(super) fn git_prompt(&mut self, name: &str, prompt: Result<String, String>) {
        match prompt {
            Ok(p) => {
                if self.session_title.is_empty() {
                    self.send(AgentCommand::Rename { title: name.into() });
                }
                self.input.set_text(&p);
                self.submit();
            }
            Err(e) => self.notice(Level::Warn, format!("{name}: {e}")),
        }
    }

    // ------------------------------------------------------------ status
}

impl App {
    /// The motion of the streaming reply and of the blocks around it, applied to freshly rendered lines (see
    /// `fx_stream`): colour only, never a line more or less.
    fn stream_fx(&self, i: usize, lines: &mut [Line<'static>], stamped: bool) {
        use crate::fx_stream as fx;
        let now = self.now();
        let since = |t: Instant| now.saturating_duration_since(t).as_millis() as u64;
        let look = self.looks.get(i).copied().unwrap_or_default();
        match &self.blocks[i] {
            Block::Assistant { streaming: true, .. } => {
                let segs = self.fx.ink.segments(now);
                if segs.is_empty() {
                    return;
                }
                // the last line with letters on it, and only that one
                if let Some(at) =
                    lines.iter().rposition(|l| l.spans.iter().skip(1).any(|s| !s.content.trim().is_empty()))
                {
                    fx::ink_tail(&mut lines[at], &segs, &self.theme, 1, stamped && at == 0);
                }
            }
            Block::Assistant { .. } | Block::Reasoning { .. } => {
                if let Some(t) = look.stopped.filter(|t| since(*t) < fx::STOP_MS) {
                    let level = fx::hump(since(t), fx::STOP_MS);
                    for l in lines.iter_mut() {
                        if let Some(s) = l.spans.first_mut().filter(|s| s.content.starts_with(['◆', '│'])) {
                            fx::warm(s, &self.theme, level);
                        }
                    }
                }
                if let (Block::Reasoning { .. }, Some(t)) =
                    (&self.blocks[i], look.ended.filter(|t| since(*t) < fx::THINK_MS))
                {
                    if let Some(s) = lines.first_mut().and_then(|l| l.spans.first_mut()) {
                        s.style = s.style.fg(fx::think_colour(&self.theme, since(t)));
                    }
                }
            }
            Block::Tool { status, .. } => {
                let Some(t) = look.done_at else { return };
                match status {
                    ToolStatus::Failed => {
                        if since(t) < fx::FAIL_MS {
                            if let Some(s) = lines.first_mut().and_then(|l| l.spans.first_mut()) {
                                s.style = s.style.fg(fx::fail_colour(&self.theme, since(t)));
                            }
                        }
                    }
                    ToolStatus::Done => {
                        if let Some(head) = lines.first_mut() {
                            fx::light_note(head, &self.theme, since(t));
                        }
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }
}
