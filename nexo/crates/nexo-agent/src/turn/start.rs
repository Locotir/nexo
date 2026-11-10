//! Part of the agent loop; see the parent module.

use super::*;

pub(super) fn summary_of(tools: &ToolSet, name: &str, args: &Value) -> String {
    match tools.get(name) {
        Some(t) => t.summary(args),
        None => {
            let a = args.to_string();
            let a: String = a.chars().take(80).collect();
            format!("{name} {a}")
        }
    }
}

/// The name a session goes by in the mailbox: its title, else the folder it works in.
pub(super) fn mail_name(title: &str, cwd: &Path) -> String {
    let t = title.trim();
    if !t.is_empty() {
        return t.chars().take(40).collect();
    }
    cwd.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "nexo".into())
}

/// The mailbox follows the session when it changes (resumed, forked, cleared, renamed).
pub(super) fn mail_sync(st: &State) {
    if let Some(m) = &st.mail {
        m.rebind(st.session.id(), &mail_name(&st.session.meta().title, &st.cwd));
    }
}

/// Items done out of the total in a `todo` call's `items`; None for a call that only shows the list.
pub(super) fn todo_counts(args: &Value) -> Option<(u16, u16)> {
    let items = args.get("items")?.as_array()?;
    let done = items
        .iter()
        .filter(|i| matches!(i.get("status").and_then(Value::as_str), Some("done") | Some("completed")))
        .count();
    Some((done.min(u16::MAX as usize) as u16, items.len().min(u16::MAX as usize) as u16))
}

/// Hands mails to the model as one message that is not the person's: the host has them as delivered (receipts go
/// out, `MailDelivered`), and the first answer of the turn marks them read.
pub(super) async fn deliver_mails(state: &Shared, mails: Vec<Mail>, cancel: CancellationToken) -> JoinHandle<TurnEnd> {
    let text = nexo_core::mail::render(&mails);
    {
        let mut st = state.lock().await;
        if let Some(m) = st.mail.clone() {
            emit(&st.events, m.delivered(&mails));
        }
        st.mail_turn = mails;
    }
    start_turn(state, text, Vec::new(), cancel).await
}

/// Starts a request the person wrote: the UserPromptSubmit hooks first. One may stop it (nothing is
/// sent; the person is told why) or add text for the model.
pub(super) async fn start_prompt(
    state: &Shared,
    text: String,
    attachments: Vec<Attachment>,
    cancel: CancellationToken,
) -> Option<JoinHandle<TurnEnd>> {
    let o = fire(state, "user_prompt_submit", None, serde_json::json!({"prompt": text})).await;
    if let Some(why) = o.blocked {
        let st = state.lock().await;
        notice(&st.events, Level::Warn, format!("your message was not sent: a hook stopped it ({why})"));
        emit(&st.events, AgentEvent::Idle { running_jobs: st.jobs.running() });
        return None;
    }
    state.lock().await.hook_context.extend(o.context);
    Some(start_turn(state, text, attachments, cancel).await)
}

pub(super) async fn start_turn(
    state: &Shared,
    text: String,
    attachments: Vec<Attachment>,
    cancel: CancellationToken,
) -> JoinHandle<TurnEnd> {
    let mut attachments = attachments;
    // @server:uri mentions become the resources they name (read before the state is taken)
    let hub = state.lock().await.mcp.clone();
    let typed = text.clone();
    let text = match &hub {
        Some(h) => h.expand_mentions(text).await,
        None => text,
    };
    // the expansion only appends: what follows the typed words is what nexo added
    let mut added: Vec<AddedPart> = Vec::new();
    let mut after = Vec::new();
    if let Some(extra) = text.strip_prefix(typed.as_str()).map(|e| e.trim_matches('\n')).filter(|e| !e.is_empty()) {
        after.push(AddedPart::new(AddedKind::Mention, "MCP resources", extra));
    }
    // the project's check runs once, before the request is built, so its result comes with it
    let check = orient::start_check(state, &cancel).await;
    {
        let mut st = state.lock().await;
        let named = named_files(&st, &text, &attachments);
        attachments.extend(named);
        let small = orient::budget_files(&st, &text, &attachments);
        attachments.extend(small.clone());
        // a server changed its tools since the last turn: they take effect now, not in the middle of one
        if let Some(h) = st.mcp.clone() {
            let st = &mut *st;
            h.sync_tools(&mut st.tools, &mut st.mcp_gen);
        }
        // the prompt file may have changed since the last request
        // a turn is under way: mail waits for the next idle point
        if let Some(m) = &st.mail {
            m.set_idle(false);
        }
        st.system = full_system(&st);
        st.turn_start = st.history.len();
        st.noticed.clear();
        st.turns += 1;
        st.ck_stop = None;
        st.reply_boost = 1;
        let ended = st.jobs.take_ended();
        let text = if ended.is_empty() || text.starts_with("[background jobs ended]") {
            text
        } else {
            let note = nexo_core::jobs::ended_note(&ended);
            added.push(AddedPart::new(AddedKind::Jobs, "background jobs that ended", note.clone()));
            format!("{note}\n\n{text}")
        };
        // what other sessions shared goes first, once: it is part of this request from now on
        let text = if st.hook_context.is_empty() {
            text
        } else {
            let block = format!(
                "{}\n{}\n{}",
                nexo_core::agent::HOOK_CONTEXT_START,
                std::mem::take(&mut st.hook_context).join("\n\n"),
                nexo_core::agent::HOOK_CONTEXT_END
            );
            added.insert(0, AddedPart::new(AddedKind::Hook, "hook context", block.clone()));
            format!("{block}\n\n{text}")
        };
        let text = if st.shared.is_empty() {
            text
        } else {
            let shared = std::mem::take(&mut st.shared);
            emit(&st.events, AgentEvent::MemoryShared { pending: Vec::new() });
            let at = shared.iter().enumerate().map(|(i, (id, title, b))| {
                let label = if title.is_empty() { id.clone() } else { title.clone() };
                (i, AddedPart::new(AddedKind::Memory, format!("memory from {label}"), b.clone()))
            });
            for (i, part) in at {
                added.insert(i, part);
            }
            let blocks: Vec<String> = shared.into_iter().map(|(_, _, b)| b).collect();
            format!("{}\n\n{text}", blocks.join("\n\n"))
        };
        // the typed words sit between what comes before them (shared memory, hooks, jobs) and what follows
        added.append(&mut after);
        let note = orient::start_note(&small, &st.cwd, check.as_deref());
        let text = if note.is_empty() {
            text
        } else {
            added.push(AddedPart::new(AddedKind::StartNote, "project at the start of the request", note.clone()));
            format!("{text}\n\n{note}")
        };
        push_user(&mut st, text, &attachments, added);
    }
    let state = state.clone();
    tokio::spawn(async move { run_turn(state, cancel).await })
}

/// The largest reply the next request asks for: `agent.max_tokens`, times the boost after a reply cut
/// while thinking, within what the model and its window allow.
pub(super) fn reply_tokens(st: &State) -> u32 {
    st.limits.reply_room(st.config.agent.max_tokens, st.reply_boost)
}

/// Adds a request of the person to the conversation, its attachments expanded. `added` is what nexo put in
/// the message besides the person's words, in the order of the message; the files it expands now are told
/// after it, and the interface hears of all (`AgentEvent::Added`) right after the message.
pub(super) fn push_user(st: &mut State, text: String, attachments: &[Attachment], mut added: Vec<AddedPart>) {
    let (text, images, whole) = expand_attachments(text, attachments, &st.cwd, &mut added);
    for (path, bytes, header) in whole {
        st.reads.attached(path, &bytes, header);
    }
    let m = if images.is_empty() {
        Message::user(text)
    } else {
        let mut parts = vec![Part::text(text)];
        parts.extend(images);
        Message::new(Role::User, parts)
    };
    st.history.push(m.clone());
    if let Err(e) = st.session.append(&m) {
        notice(&st.events, Level::Warn, format!("cannot save the session: {e}"));
    }
    let index = st.history.len() - 1;
    emit(&st.events, AgentEvent::History { index, message: m });
    if !added.is_empty() {
        emit(&st.events, AgentEvent::Added { index, parts: added });
    }
}

/// Bills the usage of an attempt the turn goes on from without keeping its reply.
pub(super) async fn bill_attempt(state: &Shared, events: &EventTx, model: &str, usage: Usage, call: Usage) {
    if !usage.is_empty() {
        add_usage(&mut *state.lock().await, events, model, usage, call, UsageKind::Main);
    }
}

/// Messages the person sent while the turn ran join it now, right after the tool
/// results, so the next model call already sees them. Tells whether a message of the person joined.
pub(super) async fn take_inbox(state: &Shared) -> bool {
    let mut st = state.lock().await;
    // jobs that ended while the tools ran: the model hears of them now
    let ended = st.jobs.take_ended();
    if !ended.is_empty() {
        push_user(&mut st, nexo_core::jobs::ended_note(&ended), &[], Vec::new());
    }
    if st.inbox.is_empty() {
        return false;
    }
    while let Some((text, mut attachments)) = st.inbox.pop_front() {
        let named = named_files(&st, &text, &attachments);
        attachments.extend(named);
        push_user(&mut st, text, &attachments, Vec::new());
    }
    person_spoke(&mut st);
    // the message shows in the transcript where it joined: nothing else to say
    emit(&st.events, AgentEvent::Queued { pending: 0 });
    true
}

/// The media type of an image file nexo attaches as an image, by its extension.
pub(super) fn image_type(p: &Path) -> Option<&'static str> {
    match p.extension()?.to_str()?.to_ascii_lowercase().as_str() {
        "png" => Some("image/png"),
        "jpg" | "jpeg" => Some("image/jpeg"),
        "gif" => Some("image/gif"),
        "webp" => Some("image/webp"),
        _ => None,
    }
}

/// The largest image attached (the vendors take about 5 MB).
pub(super) const IMAGE_BYTES: usize = 5_000_000;

/// A text file attached whole: its path, its bytes and the line that heads it.
pub(super) type Whole = (PathBuf, Vec<u8>, String);

/// The text of a request with its `@file` attachments as fenced blocks, the images
/// among them as image parts, and the text files attached whole.
pub(super) fn expand_attachments(
    text: String,
    attachments: &[Attachment],
    cwd: &Path,
    added: &mut Vec<AddedPart>,
) -> (String, Vec<Part>, Vec<Whole>) {
    use base64::Engine;
    let mut out = text;
    let mut images = Vec::new();
    let mut whole = Vec::new();
    // what each attachment appended to the text is what nexo added for it: told as the next one starts
    let mut open: Option<(usize, String)> = None;
    let close = |out: &str, open: &mut Option<(usize, String)>, added: &mut Vec<AddedPart>| {
        if let Some((from, label)) = open.take() {
            if out.len() > from {
                added.push(AddedPart::new(AddedKind::File, label, out[from..].trim_matches('\n')));
            }
        }
    };
    for a in attachments {
        close(&out, &mut open, added);
        let p = if a.path.is_absolute() { a.path.clone() } else { cwd.join(&a.path) };
        let shown = a.path.display();
        open = Some((out.len(), shown.to_string()));
        if let Some(media_type) = image_type(&p) {
            match std::fs::read(&p) {
                Ok(bytes) if bytes.len() > IMAGE_BYTES => out.push_str(&format!(
                    "\n\n@{shown}: not attached, the image is {} bytes (5 MB at most)",
                    bytes.len()
                )),
                Ok(bytes) => {
                    let data = base64::engine::general_purpose::STANDARD.encode(&bytes);
                    images.push(Part::Image { source: ImageSource::Base64 { media_type: media_type.into(), data } });
                    out.push_str(&format!("\n\n@{shown}: attached as an image"));
                }
                Err(e) => out.push_str(&format!("\n\n@{shown}: not attached ({e})")),
            }
            continue;
        }
        match std::fs::read(&p) {
            Ok(bytes) if bytes.len() > ATTACHMENT_BYTES => {
                out.push_str(&format!("\n\n@{shown}: not attached, it is {} bytes (read it in parts)", bytes.len()))
            }
            Ok(bytes) => match String::from_utf8(bytes) {
                Ok(s) => {
                    let fence = if s.contains("```") { "````" } else { "```" };
                    let ext = p.extension().map(|e| e.to_string_lossy().to_string()).unwrap_or_default();
                    let header = attached_header(&shown.to_string());
                    out.push_str(&format!("\n\n{header}\n{fence}{ext}\n{}\n{fence}", s.trim_end_matches('\n')));
                    whole.push((p.clone(), s.into_bytes(), header));
                }
                Err(_) => out.push_str(&format!("\n\n@{shown}: not attached, it is not text")),
            },
            Err(e) => out.push_str(&format!("\n\n@{shown}: not attached ({e})")),
        }
    }
    close(&out, &mut open, added);
    (out, images, whole)
}

pub(super) fn reasoning_of(s: &Option<String>) -> Option<ReasoningConfig> {
    let s = s.as_deref()?.trim().to_ascii_lowercase();
    match s.as_str() {
        "" => None,
        "off" | "none" => Some(ReasoningConfig::Off),
        // a model that only thinks or not takes `on` as its middle setting
        "on" => Some(ReasoningConfig::Effort(Effort::Medium)),
        n => Effort::parse(n).map(ReasoningConfig::Effort).or_else(|| n.parse().ok().map(ReasoningConfig::Budget)),
    }
}

/// The context window of the model in use, in tokens, when known.
pub(super) fn window_of(limits: &crate::limits::Limits) -> Option<u64> {
    limits.context_window.map(u64::from)
}

/// The tools the model is offered: `exit_plan` only in plan mode, `recall` only when the policy asks for it.
pub(super) fn offered(st: &State) -> Vec<nexo_core::provider::ToolSpec> {
    let plan = st.perm.mode == PermissionMode::Plan;
    // `recall` answers from the whole history what the curator left out: only the curator leaves anything
    // out, and it offers the tool unless `recall=0` (which keeps the tool list the same as without the
    // curator, so that a comparison measures the view alone)
    let curator = st.policy.recall();
    let specs = if prompt::is_compact(window_of(&st.limits)) { st.tools.compact_specs() } else { st.tools.specs() };
    specs
        .into_iter()
        .filter(|s| {
            (plan || s.name != "exit_plan")
                && (curator || s.name != "recall")
                && (st.goal.is_some() || s.name != "goal_brief")
        })
        .collect()
}

/// The request for the next model call, through the context policy.
pub(super) async fn prepare(
    state: &Shared,
    cancel: &CancellationToken,
    no_tool_call: bool,
) -> anyhow::Result<(ModelRequest, ProviderConfig, Arc<dyn ModelClient>, u32)> {
    let mut guard = state.lock().await;
    let st = &mut *guard;
    let specs = offered(st);
    if let Some(at) = st.policy.compact_at() {
        let at = compact_threshold(at, st.limits.context_window, reply_tokens(st));
        if request_tokens(st, &specs) > at {
            // a compaction is worth its call only when what it replaces holds enough that the last one
            // did not: a request kept over the threshold by its tail, the system prompt or the tools is
            // not summarised again every round
            if let Some(keep) = compact::keep_recent(&st.history) {
                if compact::fresh_tokens(&st.history[..keep]) >= at / COMPACT_MARGIN {
                    compact_locked(st, keep, cancel).await;
                }
            }
        }
    }
    let view = view_now(st).await;
    let (_, cfg, model) = st.target.get();
    let effort = setup::effort_of(&cfg, &model, &st.config.agent.reasoning);
    let mut req = ModelRequest::new(model, st.system.clone(), view.messages);
    if !st.hidden.is_empty() {
        let block = crate::supervise::watch::hidden_block(&st.hidden);
        // the person may read what rides on this request only, apart from what they typed
        if st.hidden.len() != st.hidden_told {
            st.hidden_told = st.hidden.len();
            let mut part = AddedPart::new(AddedKind::Supervisor, "supervisor note", block.clone());
            part.request_only = true;
            emit(&st.events, AgentEvent::Added { index: st.turn_start, parts: vec![part] });
        }
        attach_hidden(&mut req.messages, block);
    }
    req.tools = specs;
    req.no_tool_call = no_tool_call;
    req.max_tokens = reply_tokens(st);
    req.temperature = st.config.agent.temperature;
    req.reasoning = reasoning_of(&effort);
    req.session_id = Some(st.session.id().to_string());
    if let (true, Some((key, value))) = (st.fast, fast::option(&cfg)) {
        req.extra.insert(key.into(), Value::from(value));
    }
    st.last_request = Some(req.clone());
    Ok((req, cfg, st.client.clone(), st.config.agent.retries))
}

/// The window the view is kept under, in estimated tokens (4 characters a token): `room` provider
/// tokens divided by the ratio learned, with 5% to spare; before any count, 15% to spare.
pub(super) fn view_window(room: u32, ratio: Option<f64>) -> u64 {
    match ratio {
        Some(r) => (f64::from(room) * 0.95 / r) as u64,
        None => u64::from(room) * 17 / 20,
    }
}

/// The ratio of provider tokens to estimated tokens after a request of `est` estimated tokens the
/// provider counted as `real`: the largest of the last ones weighs most, so a request of code after
/// one of prose does not pass the window; small requests, whose count is mostly framing, teach nothing.
pub(super) fn learn_ratio(prev: Option<f64>, est: u64, real: u64) -> Option<f64> {
    if est < 500 || real == 0 {
        return prev;
    }
    let r = (real as f64 / est as f64).clamp(0.5, 3.0);
    Some(match prev {
        None => r,
        Some(p) if r > p => r,
        Some(p) => p * 0.8 + r * 0.2,
    })
}

/// Where the `compact` policy compacts: `at` tokens of request, or less when the model's window
/// (`window`, less the reply's `reply` tokens) would be passed first.
pub(super) fn compact_threshold(at: u64, window: Option<u32>, reply: u32) -> u64 {
    match window {
        Some(w) if w > reply => at.min(u64::from(w - reply)),
        _ => at,
    }
}

/// The tokens of the next request: the provider's count of the last one (prompt and reply) plus an
/// estimate of what came after it, or, before any count, an estimate of the system prompt, the tools
/// and the history (4 characters a token).
pub(super) fn request_tokens(st: &State, specs: &[nexo_core::provider::ToolSpec]) -> u64 {
    match st.last_prompt {
        Some((tokens, len)) if len <= st.history.len() => tokens + st.history.tokens_from(len),
        _ => {
            let tools = serde_json::to_string(specs).map(|s| s.len() as u64).unwrap_or(0);
            (st.system.len() as u64 + tools) / 4 + st.history.tokens()
        }
    }
}

/// What the next call will see, announced to the interface: on every call, and
/// when a conversation is loaded, so its numbers are there before the first call.
pub(super) async fn view_now(st: &mut State) -> View {
    let specs = offered(st);
    let system_tokens =
        (st.system.len() as u64 + serde_json::to_string(&specs).map(|s| s.len() as u64).unwrap_or(0)) / 4;
    let names: Vec<String> = specs.iter().map(|s| s.name.clone()).collect();
    st.policy.tools(&names);
    // the view never passes the model's window: what it takes less the reply, in estimated tokens
    // through the ratio learned from the provider's counts
    let reply = reply_tokens(st);
    if let Some(w) = st.limits.context_window.filter(|w| *w > reply) {
        st.policy.window(view_window(w - reply, st.tok_ratio));
    }
    let (_, cfg, model) = st.target.get();
    if let Some(p) = st.limits.pricing.or_else(|| cfg.pricing_for(&model)) {
        st.policy.prices(p.input, p.cache_read);
    }
    st.policy.history_version(st.history.version());
    let mut view = match st.policy.view(system_tokens, &st.history).await {
        Ok(v) => v,
        Err(e) => {
            notice(&st.events, Level::Warn, format!("context policy failed ({e}); sending everything"));
            View { messages: st.history.to_vec(), tokens: st.history.tokens(), note: None, memory: None }
        }
    };
    if let Some(n) = &view.note {
        notice(&st.events, Level::Warn, n.clone());
    }
    // a request with a call and no result, or a result and no call, fails on every provider: the view
    // is mended (what it holds stays, as text when it must) and each mend is said once
    let (messages, mends) = nexo_core::message::well_formed(std::mem::take(&mut view.messages));
    view.messages = messages;
    for m in mends {
        if st.mended.insert(m.clone()) {
            notice(&st.events, Level::Warn, format!("history mended before sending: {m}"));
        }
    }
    st.reads.note_view(&view.messages);
    let bytes = |ms: &[Message]| -> u64 {
        ms.iter().map(|m| serde_json::to_string(m).map(|s| s.len() as u64).unwrap_or(0)).sum()
    };
    let history_tokens = st.history.tokens();
    st.last_view = (history_tokens, view.tokens);
    st.est_sent = system_tokens + view.tokens;
    let (label, detail) = crate::context::describe(st.policy.as_ref(), history_tokens, view.tokens);
    emit(
        &st.events,
        AgentEvent::Context {
            view_tokens: view.tokens,
            history_tokens,
            history_bytes: st.history.bytes(),
            view_bytes: bytes(&view.messages),
            messages_in_view: view.messages.len(),
            messages_total: st.history.len(),
            policy: st.policy.name().to_string(),
            note: view.note.clone(),
            label,
            detail: Some(detail),
        },
    );
    let (cells, zones) = match view.memory.as_ref().and_then(|m| crate::context_map::from_memory(&st.history, m)) {
        Some(map) => map,
        None => (crate::context_map::map(&st.history, &view.messages), Vec::new()),
    };
    let same = st.last_sent.iter().zip(&view.messages).take_while(|(a, b)| a == b).count();
    let calls = crate::context_map::calls(&st.history, &cells, &view.messages, same);
    st.last_sent = view.messages.clone();
    let call_tokens = crate::context_map::call_tokens(&st.history, &cells);
    emit(&st.events, AgentEvent::ContextMap { cells, zones, calls, call_tokens });
    if let Some(m) = &view.memory {
        emit(&st.events, AgentEvent::Curated { items: m.curated.clone() });
    }
    view
}

/// The largest file a request attaches by naming it.
const NAMED_FILE_BYTES: u64 = 24_000;
/// The most files one request attaches by naming them.
const NAMED_FILES: usize = 3;

/// The files a request names (a word with a `/` or an extension) that exist under the working
/// directory, are text of at most 24 KB and are not attached already: up to three, attached as if
/// the person had written `@path`, so the model starts with them and needs no step to read them.
fn named_files(st: &State, text: &str, explicit: &[Attachment]) -> Vec<Attachment> {
    if !st.config.agent.attach_named_files {
        return Vec::new();
    }
    let root = st.cwd.canonicalize().unwrap_or_else(|_| st.cwd.clone());
    let mut seen: Vec<PathBuf> = explicit
        .iter()
        .map(|a| {
            let p = if a.path.is_absolute() { a.path.clone() } else { st.cwd.join(&a.path) };
            p.canonicalize().unwrap_or(p)
        })
        .collect();
    let mut out = Vec::new();
    for word in text.split_whitespace() {
        if out.len() >= NAMED_FILES {
            break;
        }
        let word = word.trim_matches(|c: char| {
            matches!(c, '"' | '\'' | '`' | '(' | ')' | '[' | ']' | '<' | '>' | ',' | ';' | ':' | '!' | '?')
        });
        let word = word.trim_end_matches('.');
        let looks_like_path = !word.is_empty()
            && !word.starts_with('@')
            && !word.contains("://")
            && !word.contains('*')
            && (word.contains('/')
                || Path::new(word).extension().is_some_and(|e| !e.is_empty()) && !word.starts_with('.'));
        if !looks_like_path {
            continue;
        }
        let p = if Path::new(word).is_absolute() { PathBuf::from(word) } else { st.cwd.join(word) };
        let Ok(real) = p.canonicalize() else { continue };
        if !real.starts_with(&root) || seen.contains(&real) || image_type(&real).is_some() {
            continue;
        }
        let Ok(meta) = std::fs::metadata(&real) else { continue };
        if !meta.is_file() || meta.len() > NAMED_FILE_BYTES {
            continue;
        }
        let Ok(bytes) = std::fs::read(&real) else { continue };
        if String::from_utf8(bytes).is_err() {
            continue;
        }
        // the same policy as `read`: key files ask, denied paths stay out
        if st.perm.decide("read", ToolKind::Read, &serde_json::json!({ "path": word }), &st.cwd) != Decision::Allow {
            continue;
        }
        // attached to an earlier request: the conversation holds it
        let header = attached_header(word);
        if st.history.iter().any(|m| m.text().contains(&header)) {
            continue;
        }
        seen.push(real);
        out.push(Attachment { path: PathBuf::from(word) });
    }
    out
}
