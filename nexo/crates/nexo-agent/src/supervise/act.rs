//! What the supervisor does about what it sees: the rule rounds ([`Supervisor::act`]), the requests that come
//! in over the socket ([`Supervisor::handle`]) and the three hooks of an external session. A nudge is never
//! typed into a window: it waits in the session's ledger until the session's own stop hook asks, and goes
//! back as the hook's answer. Everything done is written to the audit log.

use serde_json::{json, Value};

use super::alert::run_alert;
use super::audit::Entry as AuditEntry;
use super::classify::{classify, ends_in_question};
use super::decide::{decide, Action, Ctx, Queued, EFFECT_SECS};
use super::model::{ambiguous_loop, stamp, Ask, Case, Ruling};

use super::policy::Verdict;
use super::run::{display_name, Entry, Supervisor};
use super::socket::{HookKind, Reply, Request};
use super::{AgentTrack, ObsKind, Observation, Secs, SourceKind, State};

/// How long an open decision waits for a sign of effect before it is closed as having had none.
const OUTCOME_WAIT: Secs = 600;
/// The same alert is not sent again within this time.
const ALERT_AGAIN: Secs = 3600;
const MAX_NUDGE: usize = 2000;

impl Supervisor {
    /// Whether the supervisor leaves this session alone.
    pub fn paused(&self, t: &AgentTrack) -> bool {
        self.paused_all
            || self.ledgers.get(&t.key).is_some_and(|l| l.paused)
            || self.policy.for_agent(t.name.as_deref().unwrap_or(""), t.cwd.as_deref().unwrap_or("")).paused
    }

    /// The text a nudge to this session would start from (the panel offers it for editing).
    pub fn template(&self, key: &str, now: Secs) -> String {
        let Some(e) = self.entries.get(key) else { return self.policy.nudge.stalled.clone() };
        let t = match e.state {
            State::Looping => &self.policy.nudge.looping,
            State::WaitingAnswer => &self.policy.nudge.question,
            _ => &self.policy.nudge.stalled,
        };
        super::decide::fill(t, &e.track, now)
    }

    #[allow(clippy::too_many_arguments)]
    fn record(&mut self, now: Secs, key: &str, source: &str, action: &str, rule: &str, text: &str, open: bool) {
        let state = self.entries.get(key).map_or("unknown", |e| e.state.label());
        if let Some(a) = &self.audit {
            let _ = a.append(&AuditEntry::new(now, key, source, state, action, rule, text));
        }
        if open {
            self.open.insert(key.to_string(), now);
        }
    }

    fn close(&mut self, key: &str, outcome: &str) {
        self.open.remove(key);
        if let Some(a) = &self.audit {
            let _ = a.close_outcome(key, outcome);
        }
    }

    fn say(&mut self, key: &str, text: String, now: Secs) {
        if let Some(e) = self.entries.get_mut(key) {
            e.action = Some((text, now));
        }
    }

    fn alert(&mut self, key: &str, rule: &str, why: &str, now: Secs, via: &str) -> bool {
        let again = self
            .ledgers
            .get(key)
            .and_then(|l| l.alerted.as_ref())
            .is_some_and(|(r, at)| r == rule && now - *at < ALERT_AGAIN);
        if again {
            return false;
        }
        let (name, state) =
            self.entries.get(key).map_or((key.to_string(), "unknown"), |e| (display_name(&e.track), e.state.label()));
        // The audit line goes first: whoever sees the command's effect can then rely on the log already holding it.
        self.ledgers.entry(key.to_string()).or_default().alerted = Some((rule.to_string(), now));
        self.record(now, key, via, "alert", rule, why, true);
        let _ = run_alert(&self.policy.alert.command, &name, state, why);
        self.say(key, format!("alerted: {why}"), now);
        true
    }

    /// A live session of this program that the supervisor may speak to directly: not a sub-agent.
    fn nexo_target(&self, key: &str) -> bool {
        self.nexo.is_some() && !key.rsplit_once('.').is_some_and(|(p, _)| self.entries.contains_key(p))
    }

    /// Cases where a session of this program is better left to its person: a question dialog is open, or the
    /// work is simply finished.
    fn nexo_leaves_alone(&self, key: &str, state: State) -> bool {
        let t = &self.entries[key].track;
        let finished =
            t.turn_end.as_ref().is_some_and(|(_, tail)| !ends_in_question(tail, &self.policy.thresholds.ask_patterns));
        t.ask_pending.is_some() || (finished && state == State::Stalled)
    }

    /// Say it to the live session: the host hands it to the session as evidence for its own check-in.
    fn nudge_nexo(&mut self, key: &str, text: &str, rule: &str, now: Secs) -> bool {
        let Some(n) = self.nexo.clone() else { return false };
        let before = self.entries.get(key).and_then(|e| e.track.last_activity);
        n.send_nudge(key, text, false);
        self.ledgers.entry(key.to_string()).or_default().note_delivery(before, now);
        self.record(now, key, "internal", "nudge", rule, text, true);
        self.say(key, format!("nudged: {rule}"), now);
        true
    }

    /// Take in the model's finished answers: they are logged with what they cost.
    fn drain_model(&mut self, now: Secs) -> bool {
        let Some(a) = self.advisor.as_mut() else { return false };
        let replies = a.poll(now);
        let changed = !replies.is_empty();
        for r in replies {
            let (action, text) = match &r.ruling {
                Ruling::Nudge(t) => ("model_nudge", t.clone()),
                Ruling::Alert(t) => ("model_alert", t.clone()),
                Ruling::Wait => ("model_wait", r.error.clone().unwrap_or_default()),
            };
            let state = self.entries.get(&r.key).map_or("unknown", |e| e.state.label());
            if let Some(au) = &self.audit {
                let mut e = AuditEntry::new(now, &r.key, "model", state, action, &r.model, &text);
                e.cost_usd = r.cost_usd;
                let _ = au.append(&e);
            }
        }
        if changed {
            self.version += 1;
        }
        changed
    }

    /// Where the rules would nudge for a question or an unclear loop, ask the cheap model what to say. With
    /// `wait`, an answer still on its way holds the nudge back; without it (a hook cannot wait) the rules' own
    /// text goes out. Over the hourly cap the rules act alone and the person gets one alert.
    fn refine(&mut self, key: &str, action: Action, wait: bool, now: Secs) -> Action {
        let Action::Nudge { rule, .. } = &action else { return action };
        if self.advisor.is_none() {
            return action;
        }
        let (case, st, track) = {
            let Some(e) = self.entries.get(key) else { return action };
            let case = match *rule {
                "question" => Case::Question,
                "looping" if ambiguous_loop(&e.track) => Case::Loop,
                _ => return action,
            };
            (case, stamp(&e.track, case), e.track.clone())
        };
        let ask = self.advisor.as_mut().map(|a| a.ask(key, st, case, &track, now));
        match ask {
            Some(Ask::Cached(Ruling::Nudge(text))) => Action::Nudge { text, rule: "model" },
            Some(Ask::Cached(Ruling::Alert(why))) => Action::Alert { why, rule: "model" },
            Some(Ask::Cached(Ruling::Wait)) => Action::Nothing,
            Some(Ask::Pending) if wait => Action::Nothing,
            Some(Ask::Refused(why)) => {
                self.alert(key, "budget", why, now, "model");
                action
            }
            _ => action,
        }
    }

    /// One round of the rules over every external session and the live one of this program. True when a row
    /// changed.
    pub(super) fn act(&mut self, now: Secs) -> bool {
        let mut changed = self.drain_model(now);
        if self.last_act.is_some_and(|t| now - t < self.policy.interval_secs as Secs) {
            return changed;
        }
        self.last_act = Some(now);
        let keys: Vec<String> = self
            .entries
            .iter()
            .filter(|(k, e)| match e.source {
                SourceKind::External => true,
                SourceKind::Nexo => self.nexo_target(k),
            })
            .map(|(k, _)| k.clone())
            .collect();
        for key in keys {
            let (last, state, since, source) = {
                let e = &self.entries[&key];
                (e.track.last_activity, e.state, e.since, e.source)
            };
            if source == SourceKind::Nexo && self.nexo_leaves_alone(&key, state) {
                continue;
            }
            self.ledgers.entry(key.clone()).or_default().note_activity(last);
            if let Some(at) = self.open.get(&key).copied() {
                if last.is_some_and(|a| a > at + EFFECT_SECS) {
                    self.close(&key, "resumed");
                } else if now - at > OUTCOME_WAIT {
                    self.close(&key, "no_effect");
                }
            }
            let action = {
                let e = &self.entries[&key];
                decide(&Ctx {
                    track: &e.track,
                    state,
                    in_state: (now - since).max(0) as u64,
                    policy: &self.policy,
                    ledger: &self.ledgers[&key],
                    paused_all: self.paused_all,
                    now,
                })
            };
            let action = self.refine(&key, action, true, now);
            match action {
                Action::Nudge { text, rule } if source == SourceKind::Nexo => {
                    changed |= self.nudge_nexo(&key, &text, rule, now)
                }
                Action::Nudge { text, rule } => {
                    let l = self.ledgers.entry(key.clone()).or_default();
                    if l.queued.is_none() {
                        l.queued = Some(Queued { text: text.clone(), rule: rule.to_string(), manual: false, at: now });
                        self.record(now, &key, "rules", "queue_nudge", rule, &text, true);
                        self.say(&key, format!("nudge queued: {rule}"), now);
                        changed = true;
                    }
                }
                Action::Alert { why, rule } => changed |= self.alert(&key, rule, &why, now, "rules"),
                Action::Nothing => {
                    let l = self.ledgers.entry(key.clone()).or_default();
                    if l.queued.as_ref().is_some_and(|q| !q.manual) {
                        l.queued = None;
                    }
                }
            }
        }
        changed
    }

    /// A request from the socket (a hook, or the panel of another process).
    pub fn handle(&mut self, req: Request, now: Secs) -> Reply {
        match req {
            Request::Ping => Reply::ok(json!("pong")),
            Request::Nudge { key, text } => {
                self.queue_manual(&key, &text, now);
                Reply::ok(Value::Null)
            }
            Request::Pause { key, on } => {
                self.set_pause(key.as_deref().filter(|k| !k.is_empty()), on, now);
                Reply::ok(Value::Null)
            }
            Request::Hook { kind, payload } => Reply::ok(self.hook(kind, &payload, now)),
        }
    }

    /// The person's own nudge: it goes out at the session's next stop, whatever the rate limits say.
    pub fn queue_manual(&mut self, key: &str, text: &str, now: Secs) {
        let text: String = text.chars().take(MAX_NUDGE).collect();
        if text.trim().is_empty() {
            return;
        }
        if self.nexo_target(key) && self.entries.get(key).is_some_and(|e| e.source == SourceKind::Nexo) {
            // the live session takes it at once, as a message
            if let Some(n) = self.nexo.clone() {
                n.send_nudge(key, &text, true);
                self.record(now, key, "panel", "nudge", "manual", &text, true);
                self.say(key, "your message was sent".into(), now);
                self.version += 1;
                return;
            }
        }
        self.ledgers.entry(key.to_string()).or_default().queued =
            Some(Queued { text: text.clone(), rule: "manual".into(), manual: true, at: now });
        self.record(now, key, "panel", "queue_nudge", "manual", &text, true);
        self.say(key, "your nudge is queued".into(), now);
        self.version += 1;
    }

    pub fn set_pause(&mut self, key: Option<&str>, on: bool, now: Secs) {
        let word = if on { "pause" } else { "resume" };
        match key {
            Some(k) => {
                self.ledgers.entry(k.to_string()).or_default().paused = on;
                self.record(now, k, "panel", word, "manual", "", false);
            }
            None => {
                self.paused_all = on;
                self.record(now, "*", "panel", word, "manual", "all sessions", false);
            }
        }
        self.version += 1;
    }

    fn ensure(&mut self, key: &str, now: Secs) -> &mut Entry {
        self.entries.entry(key.to_string()).or_insert_with(|| Entry {
            track: AgentTrack::new(key),
            source: SourceKind::External,
            program: super::source::EXTERNAL.to_string(),
            state: State::Working,
            since: now,
            action: None,
        })
    }

    fn hook(&mut self, kind: HookKind, p: &Value, now: Secs) -> Value {
        let text = |k: &str| p.get(k).and_then(Value::as_str).filter(|s| !s.is_empty());
        let Some(key) = text("session_id").map(String::from) else { return json!({}) };
        let cwd = text("cwd").map(String::from);
        let mode = text("permission_mode").map(String::from);
        let e = self.ensure(&key, now);
        if cwd.is_some() || mode.is_some() {
            e.track.apply(&Observation {
                agent_key: key.clone(),
                at: now,
                kind: ObsKind::Meta { name: None, cwd: cwd.clone(), permission_mode: mode },
            });
        }
        let out = match kind {
            HookKind::Notify => self.hook_notify(&key, text("notification_type").unwrap_or(""), now),
            HookKind::Permission => self.hook_permission(&key, p, now),
            HookKind::Stop => {
                self.hook_stop(&key, p.get("stop_hook_active").and_then(Value::as_bool).unwrap_or(false), now)
            }
        };
        self.version += 1;
        out
    }

    fn feed(&mut self, key: &str, now: Secs, kind: ObsKind) {
        self.ensure(key, now).track.apply(&Observation { agent_key: key.to_string(), at: now, kind });
    }

    fn hook_notify(&mut self, key: &str, what: &str, now: Secs) -> Value {
        match what {
            "permission_prompt" => self.feed(key, now, ObsKind::PermissionPending { tool: "a tool".into() }),
            "idle_prompt" => self.feed(key, now, ObsKind::AskPending),
            _ => self.feed(key, now, ObsKind::Activity),
        }
        self.reclassify(key, now);
        json!({})
    }

    fn reclassify(&mut self, key: &str, now: Secs) {
        let policy = self.policy.clone();
        if let Some(e) = self.entries.get_mut(key) {
            let s = classify(&e.track, now, &policy);
            if std::mem::discriminant(&s) != std::mem::discriminant(&e.state) {
                e.since = now;
                e.state = s;
            }
        }
    }

    fn hook_permission(&mut self, key: &str, p: &Value, now: Secs) -> Value {
        let tool = p.get("tool_name").and_then(Value::as_str).unwrap_or("").to_string();
        let input = p.get("tool_input").cloned().unwrap_or(Value::Null);
        self.feed(key, now, ObsKind::PermissionPending { tool: tool.clone() });
        let (paused, cwd) = {
            let t = &self.entries[key].track;
            (self.paused(t) || !self.policy.enabled, t.cwd.clone().unwrap_or_else(|| ".".into()))
        };
        if paused || tool.is_empty() {
            return json!({});
        }
        let line = crate::supervise::source::tool_step(&tool, &input);
        match self.policy.allows(&tool, &input, std::path::Path::new(&cwd)) {
            Verdict::Approve => {
                self.feed(key, now, ObsKind::PermissionResolved);
                self.record(now, key, "permission_hook", "approve", "policy", &line, false);
                self.say(key, format!("approved: {line}"), now);
                if let Some(a) = &self.audit {
                    let _ = a.close_outcome(key, "approved");
                }
                json!({"hookSpecificOutput": {"hookEventName": "PermissionRequest", "decision": {"behavior": "allow"}}})
            }
            Verdict::Deny(why) => {
                // refused: the person decides, as if the supervisor were not there
                self.record(now, key, "permission_hook", "refuse", "policy", &format!("{line} ({why})"), false);
                if let Some(a) = &self.audit {
                    let _ = a.close_outcome(key, "denied");
                }
                self.say(key, format!("left to you: {why}"), now);
                json!({})
            }
            Verdict::Pass => json!({}),
        }
    }

    fn hook_stop(&mut self, key: &str, stop_active: bool, now: Secs) -> Value {
        let before = self.entries[key].track.last_activity;
        let reply = self.stop_decision(key, stop_active, now, before);
        self.feed(key, now, ObsKind::Activity);
        reply
    }

    fn stop_decision(&mut self, key: &str, stop_active: bool, now: Secs, before: Option<Secs>) -> Value {
        if !self.policy.enabled || self.paused(&self.entries[key].track) {
            return json!({});
        }
        let ledger = self.ledgers.entry(key.to_string()).or_default();
        let manual = ledger.queued.clone().filter(|q| q.manual);
        if let Some(q) = manual {
            if stop_active {
                return json!({});
            }
            return self.deliver(key, q.text, "manual", "panel", before, now);
        }
        // the state as of this stop: an unanswered question counts at once, without the usual wait
        let (state, in_state) = {
            let e = &self.entries[key];
            let t = &e.track;
            let asking = t.ask_pending.is_none()
                && t.turn_end
                    .as_ref()
                    .is_some_and(|(_, tail)| ends_in_question(tail, &self.policy.thresholds.ask_patterns));
            let s = if asking { State::WaitingAnswer } else { classify(t, now, &self.policy) };
            let same = std::mem::discriminant(&s) == std::mem::discriminant(&e.state);
            (s, if same { (now - e.since).max(0) as u64 } else { 0 })
        };
        let action = {
            let e = &self.entries[key];
            decide(&Ctx {
                track: &e.track,
                state,
                in_state,
                policy: &self.policy,
                ledger: &self.ledgers[key],
                paused_all: self.paused_all,
                now,
            })
        };
        let action = self.refine(key, action, false, now);
        match action {
            Action::Nudge { text, rule } if !stop_active => self.deliver(key, text, rule, "stop_hook", before, now),
            Action::Alert { why, rule } => {
                self.alert(key, rule, &why, now, "stop_hook");
                json!({})
            }
            _ => json!({}),
        }
    }

    fn deliver(&mut self, key: &str, text: String, rule: &str, via: &str, before: Option<Secs>, now: Secs) -> Value {
        self.ledgers.entry(key.to_string()).or_default().note_delivery(before, now);
        self.record(now, key, via, "nudge", rule, &text, true);
        self.say(key, format!("nudged: {rule}"), now);
        json!({"decision": "block", "reason": text})
    }
}
