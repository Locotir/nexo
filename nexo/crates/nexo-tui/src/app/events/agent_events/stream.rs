use super::*;

impl App {
    pub(super) fn on_preparing(&mut self, what: String) {
        // the agent took the request: the work line runs from now, not from its first model call
        if !self.busy {
            self.busy = true;
            self.turn_started = Some(self.now());
            self.streamed_chars = 0;
        }
        self.prep = Some(what);
    }

    pub(super) fn on_turn_start(&mut self) {
        // announced by `Preparing`: the clock already runs, and the agent's own `Queued` counts hold
        let announced = self.busy && self.prep.is_some();
        self.todo_done_live = false;
        self.suggestion = None;
        self.busy = true;
        if !announced || self.turn_started.is_none() {
            self.turn_started = Some(self.now());
        }
        self.streamed_chars = 0;
        self.turn_tally = Default::default();
        self.turn_usage = Usage::default();
        self.turn_cost_start = self.cost;
        if !announced {
            self.queued = self.queued.saturating_sub(1);
            self.trim_queue();
        }
    }

    /// The block a reply or thinking delta goes on: the last one, past the notices that came while it
    /// streamed (a `/goal` set during the turn, a warning). Those sit under it, and the reply or the
    /// thinking stays one block.
    fn stream_tail(&self) -> Option<usize> {
        self.blocks.iter().rposition(|b| !matches!(b, Block::Notice { .. }))
    }

    pub(super) fn on_assistant_start(&mut self) {
        self.close_streaming();
        self.reply_started = Some(self.now());
    }

    pub(super) fn on_text_delta(&mut self, text: String) {
        self.prep = None;
        self.streamed_chars += text.len();
        if self.fx_on() {
            let now = self.now();
            self.fx.ink.note(now, text.chars().filter(|c| *c != '\n').count());
        }
        self.close_reasoning();
        match self.stream_tail() {
            Some(i) if matches!(self.blocks[i], Block::Assistant { streaming: true, .. }) => {
                if let Block::Assistant { text: t, .. } = &mut self.blocks[i] {
                    t.push_str(&text);
                }
                self.invalidate(i);
            }
            _ => {
                let (at, started) = (Some(self.stamp_now()), Some(self.now()));
                self.push(Block::Assistant { text, streaming: true, at, started, took_ms: None });
            }
        }
    }

    pub(super) fn on_reasoning_delta(&mut self, text: String) {
        self.prep = None;
        self.streamed_chars += text.len();
        match self.stream_tail() {
            Some(i) if matches!(self.blocks[i], Block::Reasoning { streaming: true, .. }) => {
                if let Block::Reasoning { text: t, .. } = &mut self.blocks[i] {
                    t.push_str(&text);
                }
                self.invalidate(i);
            }
            _ => {
                self.push(Block::Reasoning {
                    text,
                    streaming: true,
                    // the time the model spent is from the request to the first answer or call, not
                    // between reasoning events (some providers send those together, at the end)
                    started: self.reply_started.or(Some(self.now())),
                    elapsed_ms: None,
                    folded: true,
                });
            }
        }
    }

    pub(super) fn on_tool_call(&mut self, id: String, name: String, args: serde_json::Value, summary: String) {
        self.prep = None;
        self.close_streaming();
        // a replayed call already has its card from the message: give it the tool's summary
        if let Some(i) = self.tool_index(&id) {
            if let Block::Tool { summary: s, .. } = &mut self.blocks[i] {
                *s = summary;
            }
            self.invalidate(i);
            return;
        }
        self.turn_tally.count_call(&name);
        // a plan stays open in the transcript: it is what was agreed
        let folded = name != "exit_plan";
        self.push(Block::Tool {
            id,
            name,
            summary,
            args,
            status: ToolStatus::Pending,
            progress: Vec::new(),
            output: None,
            started: None,
            elapsed_ms: None,
            folded,
        });
    }

    pub(super) fn on_tool_start(&mut self, id: String) {
        if let Some(i) = self.tool_index(&id) {
            let now = self.now();
            if let Block::Tool { status, started, .. } = &mut self.blocks[i] {
                *status = ToolStatus::Running;
                *started = Some(now);
            }
            self.invalidate(i);
        }
    }

    pub(super) fn on_tool_progress(&mut self, id: String, line: String) {
        if let Some(i) = self.tool_index(&id) {
            if let Block::Tool { progress, .. } = &mut self.blocks[i] {
                progress.push(line);
                if progress.len() > MAX_PROGRESS {
                    progress.remove(0);
                }
            }
            self.invalidate(i);
        }
    }

    pub(super) fn on_tool_done(&mut self, id: String, output: ToolOutput, elapsed_ms: u64) {
        if let ToolDisplay::Todo { items } = &output.display {
            // the card says what changed; the whole list stays in the window and the strip
            let said = crate::todo_strip::change_line(&self.todo, items);
            if let Some(i) = self.tool_index(&id) {
                if let Block::Tool { summary, .. } = &mut self.blocks[i] {
                    *summary = said;
                }
            }
            self.todo = items.clone();
            self.todo_done_live = !items.is_empty() && crate::todo_strip::done_count(items) == items.len();
            let at = self.now();
            if let Some(Modal::Todo(v)) = &mut self.modal {
                v.set_items(items.clone(), at);
                self.dirty = true;
            }
        }
        if let ToolDisplay::Remembered { scope, items, was, .. } = &output.display {
            let place = if scope == "project" { "this project" } else { "global" };
            let said = match items.as_slice() {
                [] => {
                    format!("✦ forgot ({place}): {}", crate::render::wrap::truncate(was.as_deref().unwrap_or(""), 40))
                }
                [one] => format!("✦ remembered ({place}): {}", crate::render::wrap::truncate(one, 48)),
                more => format!("✦ remembered ({place}): {}", nexo_core::count(more.len(), "rule")),
            };
            self.flash(&said);
        }
        // `browsers` found none of the person's: say how to connect one (not for a replayed call, which
        // never started)
        let no_browser = matches!(output.display, ToolDisplay::NoBrowser)
            && self.tool_index(&id).is_some_and(|i| matches!(&self.blocks[i], Block::Tool { started: Some(_), .. }));
        if let Some(i) = self.tool_index(&id) {
            if let Block::Tool { status, output: o, elapsed_ms: e, .. } = &mut self.blocks[i] {
                *status = status_of(&output);
                self.words.feed(&output.content, crate::complete::Src::Other);
                *o = Some(output);
                *e = (elapsed_ms > 0).then_some(elapsed_ms); // a replay has no timing
            }
            self.note_change(i);
            self.invalidate(i);
        }
        if no_browser {
            self.notice(Level::Info, "Your browser is not connected: /chrome sets it up");
        }
    }

    pub(super) fn on_assistant_done(&mut self, message: Message) {
        let streamed_text = matches!(self.blocks.last(), Some(Block::Assistant { .. }))
            || self
                .blocks
                .iter()
                .rev()
                .take_while(|b| !matches!(b, Block::User { .. }))
                .any(|b| matches!(b, Block::Assistant { .. }));
        self.close_streaming();
        if !streamed_text {
            let text = message.text();
            if !text.trim().is_empty() {
                let at = Some(self.stamp_now());
                self.push(Block::Assistant { text, streaming: false, at, started: None, took_ms: None });
            }
        }
    }

    pub(super) fn on_turn_end(&mut self, reason: TurnEnd) {
        self.todo_done_live = false;
        self.close_streaming();
        self.busy = false;
        self.prep = None;
        if matches!(reason, TurnEnd::Done) {
            self.turn_glow();
        }
        // an error or the turn limit: the red burst (an interrupt is the person's own stop)
        if matches!(reason, TurnEnd::Error | TurnEnd::MaxTurns) {
            self.burst(crate::fx_burst::Kind::Failed);
        }
        self.turns_ended += 1;
        let elapsed_ms = self.turn_started.map(|t| t.elapsed().as_millis() as u64).unwrap_or(0);
        // a long turn rings when it ends: the person may be in another window
        if self.opts.notify_after > 0 && elapsed_ms >= self.opts.notify_after * 1000 {
            let what = match reason {
                TurnEnd::Done => "done",
                TurnEnd::Interrupted => "interrupted",
                TurnEnd::Error => "stopped on an error",
                TurnEnd::MaxTurns => "stopped at the turn limit",
            };
            self.bell_request = Some(format!("nexo: {what} after {}", fmt_ms(elapsed_ms)));
        }
        if !self.turn_usage.is_empty() {
            let usage = self.turn_usage;
            let cost_usd = (self.cost - self.turn_cost_start).max(0.0);
            let tally = std::mem::take(&mut self.turn_tally);
            let plan = self.on_plan();
            self.push(Block::Usage { usage, cost_usd, elapsed_ms, tally, plan, folded: true });
        }
        match reason {
            TurnEnd::Done => {}
            TurnEnd::Interrupted => {
                // the stop is done: "interrupting…" would say it is still going on
                if self.status.as_ref().is_some_and(|(t, _)| t.starts_with("interrupting…")) {
                    self.status = None;
                }
                self.stop_rail();
                // a reply cut in the middle says so: nothing in its own text marks where it stopped
                let cut = self
                    .blocks
                    .iter()
                    .rev()
                    .find(|b| !matches!(b, Block::Usage { .. }))
                    .is_some_and(|b| matches!(b, Block::Assistant { text, .. } if !text.trim().is_empty()));
                let what = if cut {
                    "interrupted · the reply above stops here; send a message to go on"
                } else {
                    "interrupted"
                };
                self.notice(Level::Warn, what)
            }
            TurnEnd::Error => {}
            TurnEnd::MaxTurns => {
                self.notice(Level::Warn, "stopped: the turn limit was reached; send a message to go on")
            }
        }
        for b in self.blocks.iter_mut() {
            if let Block::Tool { status, .. } = b {
                if matches!(status, ToolStatus::Pending | ToolStatus::Running) {
                    *status = ToolStatus::Failed;
                }
            }
        }
        self.invalidate_all();
        // a turn that ended normally asks for a guess at the next message (nothing waits in the queue)
        if matches!(reason, TurnEnd::Done) && self.opts.suggest && self.queued == 0 {
            self.send(AgentCommand::Suggest);
        }
    }

    pub(super) fn on_suggestion(&mut self, text: String) {
        // Keep it between turns even if the user is typing; it shows when the draft is empty.
        // Drop replies during a turn or while a sub-agent's view is open.
        let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
        if self.opts.suggest && !self.busy && self.sub_view.is_none() && !text.is_empty() {
            self.suggestion = Some(text);
            self.dirty = true;
        }
    }

    pub(super) fn on_system_prompt(&mut self, text: String) {
        self.system_prompt = Some(text.clone());
        self.push(Block::Shell { command: "system prompt".to_string(), output: text, exit_code: None });
    }
}
