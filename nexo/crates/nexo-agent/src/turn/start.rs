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

/// The line of a call about to run: as `summary_of`, and a call about a background job names the job.
pub(super) fn live_summary_of(tools: &ToolSet, jobs: &nexo_core::jobs::Jobs, name: &str, args: &Value) -> String {
    match tools.get(name) {
        Some(t) => t.summary_live(args, jobs),
        None => summary_of(tools, name, args),
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
    ultracode: bool,
    cancel: CancellationToken,
) -> Option<JoinHandle<TurnEnd>> {
    start_prompt_item(state, text, attachments, ultracode, cancel, None).await
}

pub(super) async fn start_prompt_item(
    state: &Shared,
    text: String,
    attachments: Vec<Attachment>,
    ultracode: bool,
    cancel: CancellationToken,
    item: Option<InboxItem>,
) -> Option<JoinHandle<TurnEnd>> {
    // taken: said at once, before the hooks and the start of the turn (they can take seconds)
    let events = state.lock().await.events.clone();
    preparing(&events, PREP_REQUEST);
    let o = fire(state, "user_prompt_submit", None, serde_json::json!({"prompt": text})).await;
    if let Some(why) = o.blocked {
        let st = state.lock().await;
        notice(&st.events, Level::Warn, format!("your message was not sent: a hook stopped it ({why})"));
        emit(&st.events, AgentEvent::Idle { running_jobs: st.jobs.running() });
        return None;
    }
    state.lock().await.hook_context.extend(o.context);
    Some(begin_turn(state, text, attachments, true, ultracode, cancel, item).await)
}

/// Starts a turn on a message that is not the person's own words (mail, the goal loop, a job note, the
/// supervisor's resume): what nexo adds follows the text, as it always did.
pub(super) async fn start_turn(
    state: &Shared,
    text: String,
    attachments: Vec<Attachment>,
    cancel: CancellationToken,
) -> JoinHandle<TurnEnd> {
    begin_turn(state, text, attachments, false, false, cancel, None).await
}

/// Starts a turn on ``text``. When the person wrote it (``person``), everything nexo adds (shared memory, hook
/// context, ended jobs, MCP resources, the start note, the files attached) goes first and their words come last,
/// under `PERSON_MARK`: the request is the last thing the model reads, and nothing a file says comes after it.
///
/// The work before the first model call (the mentions, the project's check, the message) runs in the turn's own
/// task: the loop takes commands meanwhile, so a message sent then is queued (and said to be) and Esc stops it.
async fn begin_turn(
    state: &Shared,
    text: String,
    attachments: Vec<Attachment>,
    person: bool,
    ultracode: bool,
    cancel: CancellationToken,
    item: Option<InboxItem>,
) -> JoinHandle<TurnEnd> {
    let events = state.lock().await.events.clone();
    let what = if person {
        PREP_REQUEST
    } else if text.starts_with(nexo_core::agent::GOAL_SET_MARK) {
        PREP_GOAL
    } else if text.trim_start().starts_with(nexo_core::agent::GOAL_CONTINUE_MARK) {
        PREP_GOAL_NEXT
    } else {
        PREP_REQUEST
    };
    preparing(&events, what);
    let state = state.clone();
    tokio::spawn(async move {
        let from = SystemTime::now();
        if !prepare_turn(&state, text, attachments, person, ultracode, &cancel, item).await {
            return TurnEnd::Interrupted;
        }
        let end = run_turn(state.clone(), cancel).await;
        orient::worked(&mut state.lock().await.worked, from, SystemTime::now());
        end
    })
}

/// What `begin_turn` does before the turn's rounds: the message the model reads, in the history.
async fn prepare_turn(
    state: &Shared,
    text: String,
    attachments: Vec<Attachment>,
    person: bool,
    ultracode: bool,
    cancel: &CancellationToken,
    mut item: Option<InboxItem>,
) -> bool {
    // @server:uri mentions become the resources they name (read before the state is taken)
    let hub = state.lock().await.mcp.clone();
    if let Some(h) = &hub {
        // servers still connecting from the start of the session: their tools come with this request
        tokio::select! {
            _ = h.ready() => {}
            _ = cancel.cancelled() => return false,
        }
    }
    let typed = text.clone();
    let text = match &hub {
        Some(h) => h.expand_mentions(text).await,
        None => text,
    };
    // the expansion only appends: what follows the typed words is what nexo added
    let mut added: Vec<AddedPart> = Vec::new();
    let mut after = Vec::new();
    // what nexo adds to the person's words, in the order the message gives it, before them
    let mut first: Vec<String> = Vec::new();
    if let Some(extra) = text.strip_prefix(typed.as_str()).map(|e| e.trim_matches('\n')).filter(|e| !e.is_empty()) {
        after.push(AddedPart::new(AddedKind::Mention, "MCP resources", extra));
        if person {
            first.push(extra.to_string());
        }
    }
    let text = if person { typed.clone() } else { text };
    // the project's check runs once, before the request is built, so its result comes with it
    let check = orient::start_check(state, cancel).await;
    // what the machine has for the programs and config the person names (nothing while its index is not built)
    let machine =
        if person && !marked_by_nexo(&typed) { orient::machine_found(state, &typed).await } else { Vec::new() };
    {
        let mut st = state.lock().await;
        if let Some(item) = &item {
            if cancel.is_cancelled() {
                if let Some((_, reply)) = &item.directed {
                    reply.resolve(nexo_core::agent::DirectedResult::Rejected("turn cancelled"));
                }
                return false;
            }
            if let Some((target, reply)) = &item.directed {
                if !st.target_matches(target) {
                    reply.resolve(nexo_core::agent::DirectedResult::Rejected("observation changed"));
                    return false;
                }
            }
        }
        if ultracode {
            let (provider, _, model) = st.target.get();
            let now = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).unwrap_or_default().as_secs() as i64;
            first.push(ultracode::facts(&st.config, &provider, &model, now));
        }
        // the project files nexo includes by itself: the ones the message names, then the small ones in the budget
        let (mut own, mut known) = named_files(&st, &text, &attachments);
        let named: Vec<Attachment> = attachments.iter().chain(&own).cloned().collect();
        let (mapped, lines) = orient::machine_files(&mut st, &text, &named, machine);
        own.extend(mapped);
        known.extend(lines);
        let with: Vec<Attachment> = attachments.iter().chain(&own).cloned().collect();
        own.extend(orient::budget_files(&st, &text, &with));
        let included = orient::include(&own, &st.cwd);
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
        st.request_in = false;
        st.goal_idle.changed = false;
        st.noticed.clear();
        st.turns += 1;
        st.ck_stop = None;
        st.reply_boost = 1;
        let ended = st.jobs.take_ended();
        let text = if ended.is_empty() || text.starts_with("[background jobs ended]") {
            text
        } else {
            let note = room::ended_note(&st, &ended);
            added.push(AddedPart::new(AddedKind::Jobs, "background jobs that ended", note.clone()));
            if person {
                first.insert(0, note);
                text
            } else {
                format!("{note}\n\n{text}")
            }
        };
        // what other sessions shared goes first, once: it is part of this request from now on
        let text = if st.hook_context.is_empty() {
            text
        } else {
            // a hook's text is as long as the hook wrote it: it is held to the room one result may take of the window
            let hooked = std::mem::take(&mut st.hook_context).join("\n\n");
            let hooked =
                room::held(hooked, room::result_room(&st), "hook context", "the rest of the hook's output is not kept");
            let block =
                format!("{}\n{}\n{}", nexo_core::agent::HOOK_CONTEXT_START, hooked, nexo_core::agent::HOOK_CONTEXT_END);
            added.insert(0, AddedPart::new(AddedKind::Hook, "hook context", block.clone()));
            if person {
                first.insert(0, block);
                text
            } else {
                format!("{block}\n\n{text}")
            }
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
            if person {
                first.splice(0..0, blocks);
                text
            } else {
                format!("{}\n\n{text}", blocks.join("\n\n"))
            }
        };
        // the person stopped the request before this one: the line that says so comes first, so the model reads
        // what happened before it reads anything this request brings; the goal's and the supervisor's marks are
        // read back as they are, so the request after them carries it
        let marked = marked_by_nexo(&typed);
        let stop = st.session.stop().filter(|_| person || !marked).map(str::to_string);
        let text = match stop {
            None => text,
            Some(line) => {
                added.insert(0, AddedPart::new(AddedKind::Stopped, "stop", line.clone()));
                if person {
                    first.insert(0, line);
                    text
                } else {
                    format!("{line}\n\n{text}")
                }
            }
        };
        // the typed words sit between what comes before them (shared memory, hooks, jobs) and what follows
        added.append(&mut after);
        // the supervisor's resume mark is read back as it is, so it carries no note; the next request does
        let tree = if typed.trim() == SUPERVISOR_RESUME_MARK { None } else { prefix::tree_note(&mut st) };
        // a session that goes on past the day it began: its system text keeps that day, so today comes here
        let tree = match (prefix::date_line(&st), tree) {
            (Some(d), Some(t)) => Some(format!("{d}\n{t}")),
            (d, t) => d.or(t),
        };
        let ask = ask_line(&mut st, person, &text);
        let note = orient::start_note(&included, &known, check.as_deref(), tree.as_deref(), ask.as_deref());
        let text = if note.is_empty() {
            text
        } else {
            added.push(AddedPart::new(AddedKind::StartNote, "project at the start of the request", note.clone()));
            if person {
                first.push(note);
                text
            } else {
                format!("{text}\n\n{note}")
            }
        };
        // a goal that is not running is said before the person's words, so an old one is not taken for the work
        if person && !typed.starts_with(GOAL_CONTINUE_MARK) && !typed.starts_with(GOAL_SET_MARK) {
            if let Some(line) = goal_state_line(&st) {
                added.push(AddedPart::new(AddedKind::StartNote, "goal state", line.clone()));
                first.push(line);
            }
        }
        let clock = if person { person_clock_line(&st, &typed) } else { None };
        if let Some((Some(line), _)) = &clock {
            first.push(line.clone());
        }
        if person {
            if !push_user_checked(
                &mut st,
                first.join("\n\n"),
                Some(text),
                included,
                &attachments,
                added,
                item.as_ref().map(|i| (i, cancel)),
            ) {
                return false;
            }
            // the next gap is measured from this message only once the model has it
            if let Some((_, at)) = clock {
                st.last_person_at = Some(at);
            }
        } else {
            if !push_user_checked(
                &mut st,
                text,
                None,
                included,
                &attachments,
                added,
                item.as_ref().map(|i| (i, cancel)),
            ) {
                return false;
            }
        }
        st.request_in = true;
        if let Some((_, reply)) = item.as_mut().and_then(|i| i.directed.take()) {
            reply.resolve(nexo_core::agent::DirectedResult::Accepted);
        }
    }
    true
}

/// The time line before a message the person typed, and when they typed it: `None` for a goal mark, which is not
/// the person's message. A message nexo joined from several carries a time on each part already, so it has the time
/// and no line. The gap is measured from `last_person_at`, which the caller sets only after the message is in.
fn person_clock_line(st: &State, typed: &str) -> Option<(Option<String>, SystemTime)> {
    if typed.starts_with(GOAL_CONTINUE_MARK) || typed.starts_with(GOAL_SET_MARK) {
        return None;
    }
    let now = SystemTime::now();
    if typed.starts_with(nexo_core::agent::JOINED_MARK) {
        return Some((None, now));
    }
    let gap = st.last_person_at.map(|t| now.duration_since(t).unwrap_or_default());
    Some((Some(nexo_core::agent::person_clock(&nexo_core::clock::local_hms(now), gap)), now))
}

/// Whether `typed` is a line nexo writes by itself (the supervisor's resume, a goal's start or continuation), not the person's words.
fn marked_by_nexo(typed: &str) -> bool {
    typed.trim() == SUPERVISOR_RESUME_MARK || typed.starts_with(GOAL_SET_MARK) || typed.starts_with(GOAL_CONTINUE_MARK)
}

/// The line about compiled programs for a message of the person's, once per session (see `ask_hint.rs`), and again
/// after a compaction took it out.
fn ask_line(st: &mut State, person: bool, text: &str) -> Option<String> {
    if !person {
        return None;
    }
    if let Some(again) = st.ask_again.take() {
        return Some(again);
    }
    if st.ask_told {
        return None;
    }
    let line = crate::ask_hint::line(text, &st.cwd, offer_of(st))?;
    st.ask_told = true;
    Some(line)
}

/// The largest reply the next request asks for: `agent.max_tokens`, times the boost after a reply cut
/// while thinking, within what the model and its window allow.
pub(super) fn reply_tokens(st: &State) -> u32 {
    st.limits.reply_room(st.config.agent.max_tokens, st.reply_boost)
}

/// Adds a user message to the conversation: ``text``, then the project files nexo included by itself (``included``,
/// counted by the start note that ends ``text``), then the person's own attachments expanded. With ``typed`` (the
/// words the person wrote) ``text`` is what nexo adds to them, and the words come after it and after the files, under
/// `PERSON_MARK` (alone when nothing was added). `added` is what nexo put in the message besides the person's
/// words, in the order of the message; the files are told after it, and the interface hears of all
/// (`AgentEvent::Added`) right after the message.
pub(super) fn push_user(
    st: &mut State,
    text: String,
    typed: Option<String>,
    included: Vec<orient::Included>,
    attachments: &[Attachment],
    added: Vec<AddedPart>,
) {
    push_user_checked(st, text, typed, included, attachments, added, None);
}

// The cancellation check immediately before history insertion is the linearization point:
// cancellation observed there rejects the directed item; later cancellation cannot undo acceptance.
fn push_user_checked(
    st: &mut State,
    text: String,
    typed: Option<String>,
    included: Vec<orient::Included>,
    attachments: &[Attachment],
    mut added: Vec<AddedPart>,
    guard: Option<(&InboxItem, &CancellationToken)>,
) -> bool {
    let mut text = text;
    let mut own = Vec::new();
    for f in included {
        text.push_str("\n\n");
        text.push_str(&f.block);
        added.push(AddedPart::new(AddedKind::File, f.shown, f.block));
        own.push((f.path, f.bytes, f.header));
    }
    let (text, images, mut whole) = expand_attachments(text, attachments, &st.cwd, offer_of(st), &mut added);
    // a file the person attached too is known by their header: theirs is recorded last
    own.append(&mut whole);
    let whole = own;
    let text = match typed {
        Some(words) if text.trim().is_empty() => words,
        Some(words) => format!("{}\n\n{}\n{words}", text.trim_matches('\n'), nexo_core::agent::PERSON_MARK),
        None => text,
    };
    let m = if images.is_empty() {
        Message::user(text)
    } else {
        let mut parts = vec![Part::text(text)];
        parts.extend(images);
        Message::new(Role::User, parts)
    };
    if !may_incorporate(guard) {
        return false;
    }
    st.history.push(m.clone());
    for (path, bytes, header) in whole {
        st.reads.attached(path, &bytes, header);
    }
    let previous_title = st.session.meta().title.clone();
    let _ = st.session.append(&m);
    if st.session.meta().title != previous_title {
        emit(
            &st.events,
            AgentEvent::SessionChanged {
                session_id: st.session.id().to_string(),
                title: st.session.meta().title.clone(),
            },
        );
    }
    saving(st);
    let index = st.history.len() - 1;
    emit(&st.events, AgentEvent::History { index, message: m });
    if !added.is_empty() {
        emit(&st.events, AgentEvent::Added { index, parts: added });
    }
    true
}

fn may_incorporate(guard: Option<(&InboxItem, &CancellationToken)>) -> bool {
    if let Some((item, cancel)) = guard {
        if cancel.is_cancelled() {
            if let Some((_, reply)) = &item.directed {
                reply.resolve(nexo_core::agent::DirectedResult::Rejected("turn cancelled"));
            }
            return false;
        }
    }
    true
}

/// Bills the usage of an attempt the turn goes on from without keeping its reply.
pub(super) async fn bill_attempt(state: &Shared, events: &EventTx, model: &str, usage: Usage, call: Usage) {
    if !usage.is_empty() {
        add_usage(&mut *state.lock().await, events, model, usage, call, UsageKind::Main);
    }
}

/// Messages the person sent while the turn ran join it now, right after the tool
/// results, so the next model call already sees them. Tells whether a message of the person joined.
pub(super) async fn take_inbox(state: &Shared, cancel: &CancellationToken) -> bool {
    let mut st = state.lock().await;
    // jobs that ended while the tools ran: the model hears of them now
    st.detach.clear();
    let ended = st.jobs.take_ended();
    if !ended.is_empty() {
        let note = room::ended_note(&st, &ended);
        push_user(&mut st, note, None, Vec::new(), &[], Vec::new());
    }
    if st.inbox.is_empty() {
        return false;
    }
    let mut consumed = false;
    let mut deferred = Vec::new();
    while let Some(mut item) = st.pop_inbox() {
        let text = std::mem::take(&mut item.text);
        let attachments = std::mem::take(&mut item.attachments);
        // the files the message names come as in a first request: under nexo's own header, counted by a note
        let (named, known) = named_files(&st, &text, &attachments);
        let included = orient::include(&named, &st.cwd);
        let ask = ask_line(&mut st, true, &text);
        let note = orient::start_note(&included, &known, None, None, ask.as_deref());
        let clock = person_clock_line(&st, &text);
        let mut added = Vec::new();
        if !note.is_empty() {
            added.push(AddedPart::new(AddedKind::StartNote, "project at the start of the request", note.clone()));
        }
        // the time of the message follows nexo's note, as in a first request; the interface is not told of it
        let note = match &clock {
            Some((Some(line), _)) if note.is_empty() => line.clone(),
            Some((Some(line), _)) => format!("{note}\n\n{line}"),
            _ => note,
        };
        if !push_user_checked(&mut st, note, Some(text.clone()), included, &attachments, added, Some((&item, cancel))) {
            if item.directed.is_none() {
                item.text = text;
                item.attachments = attachments;
                deferred.push(item);
            }
            continue;
        }
        consumed = true;
        if let Some((_, at)) = clock {
            st.last_person_at = Some(at);
        }
        if let Some((_, reply)) = item.directed.take() {
            reply.resolve(nexo_core::agent::DirectedResult::Accepted);
        }
    }
    for item in deferred.into_iter().rev() {
        st.inbox.push_front(item);
    }
    if consumed {
        person_spoke(&mut st);
    }
    // the message shows in the transcript where it joined: nothing else to say
    emit(&st.events, AgentEvent::Queued { pending: st.inbox.len() });
    consumed
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

/// The fence around `text`: three backticks, or one more than the longest run of them in it, so that no line of
/// a file closes its own block and reads as what follows it.
fn fence_of(text: &str) -> String {
    let (mut longest, mut run) = (0, 0);
    for c in text.chars() {
        run = if c == '`' { run + 1 } else { 0 };
        longest = longest.max(run);
    }
    "`".repeat(longest.max(2) + 1)
}

/// The text `s` of the file at `p` under the line `header`, fenced.
pub(super) fn fenced(header: &str, p: &Path, s: &str) -> String {
    let fence = fence_of(s);
    let ext = p.extension().map(|e| e.to_string_lossy().to_string()).unwrap_or_default();
    format!("{header}\n{fence}{ext}\n{}\n{fence}", s.trim_end_matches('\n'))
}

/// The text of a request with the person's `@file` attachments as fenced blocks, the images
/// among them as image parts, and the text files attached whole.
pub(super) fn expand_attachments(
    text: String,
    attachments: &[Attachment],
    cwd: &Path,
    offer: crate::hint_cmd::Offer,
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
        // a compiled program is told by what it is and the call that reads it, read from its first bytes only
        if let Some(kind) = nexo_binmagic::kind_at(&p) {
            let size = std::fs::metadata(&p).ok().map(|m| m.len());
            let about = nexo_tools::program::about(&kind, size, &shown.to_string(), offer.bin);
            out.push_str(&format!("\n\n@{shown}: not attached: {about}"));
            continue;
        }
        match std::fs::read(&p) {
            Ok(bytes) if bytes.len() > ATTACHMENT_BYTES => {
                out.push_str(&format!("\n\n@{shown}: not attached, it is {} bytes (read it in parts)", bytes.len()))
            }
            Ok(bytes) => match String::from_utf8(bytes) {
                Ok(s) => {
                    let header = attached_header(&shown.to_string());
                    out.push_str(&format!("\n\n{}", fenced(&header, &p, &s)));
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

pub(crate) fn reasoning_of(s: &Option<String>) -> Option<ReasoningConfig> {
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
    let window = window_of(&st.limits);
    // in a small window the MCP tools are held to their share of this model's window
    let compact = prompt::is_compact(window);
    let specs = st.tools.specs_with(compact, compact.then(|| nexo_core::tool::mcp_share(window)));
    specs
        .into_iter()
        .filter(|s| {
            !prompt::left_out(window, &s.name)
                && (plan || s.name != "exit_plan")
                && (st.tools.get("ultracode_execute").is_none() || super::tools::ultracode_planning_tool(&s.name))
                && (curator || s.name != "recall")
                && (st.goal.is_some() || s.name != "goal_brief")
        })
        .collect()
}

/// Notes which MCP tools this request offers by name and one line ([`offered`] in a small window), and tells the
/// person what was shortened, with its numbers, once for as long as that stays the same.
fn mcp_offered_short(st: &mut State) {
    let window = window_of(&st.limits);
    let short = if prompt::is_compact(window) {
        st.tools.mcp_shortened(true, nexo_core::tool::mcp_share(window))
    } else {
        None
    };
    st.mcp_short = short.as_ref().map_or_else(Vec::new, |s| s.names());
    if let Some(s) = short {
        let note = s.note(window);
        if note != st.mcp_told {
            notice(&st.events, Level::Info, note.clone());
            st.mcp_told = note;
        }
    }
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
    mcp_offered_short(st);
    if let Some(at) = st.policy.compact_at() {
        let at = compact_threshold(at, st.limits.context_window, reply_tokens(st));
        if request_tokens(st, &specs) > at {
            // a compaction is worth its call only when what it replaces holds enough that the last one
            // did not: a request kept over the threshold by its tail, the system prompt or the tools is
            // not summarised again every round
            if let Some(keep) = compact::keep_recent(&st.history) {
                let fresh = compact::fresh_tokens(&st.history[..keep]);
                let step = at / COMPACT_MARGIN;
                // after a failed attempt the history is as it was: try again only once it has grown by a step
                let waiting = st.compact_failed_at.is_some_and(|f| fresh >= f && fresh < f + step);
                if fresh >= step && !waiting {
                    st.compact_failed_at = (!compact_locked(st, keep, cancel).await).then_some(fresh);
                }
            }
        }
    }
    // what rides on this request only is worked out first, so the view's budget leaves room for it
    let hidden = (!st.hidden.is_empty()).then(|| nexo_supervise::watch::hidden_block(&st.hidden));
    // an active goal: the counts of its run and, once its work brings nothing new, the facts of that (see `goal_note`)
    let goal_facts = goal_note(st);
    // the task list, as it stands now: with this request only, so a change in it leaves the cached prefix alone
    let todo = todo_note(st);
    let extra = request_only_tokens(&hidden.iter().chain(&goal_facts).chain(&todo).collect::<Vec<_>>());
    let view = view_now(st, extra).await;
    let (_, cfg, model) = st.target.get();
    let effort = setup::effective_effort(&cfg, &model, &st.config.agent.reasoning);
    let mut req = ModelRequest::new(model, st.system.clone(), view.messages);
    if let Some(block) = &hidden {
        // the person may read what rides on this request only, apart from what they typed
        if st.hidden.len() != st.hidden_told {
            st.hidden_told = st.hidden.len();
            let mut part = AddedPart::new(AddedKind::Supervisor, "supervisor note", block.clone());
            part.request_only = true;
            emit(&st.events, AgentEvent::Added { index: st.turn_start, parts: vec![part] });
        }
    }
    // the notes of this request only go as one text part: a provider puts its cache breakpoint before one trailing
    // text part, so with two the breakpoint would land on a note the next request does not carry
    let notes: Vec<String> = hidden.into_iter().chain(goal_facts).chain(todo).collect();
    let notes = (!notes.is_empty()).then(|| notes.join("\n\n"));
    if let Some(n) = &notes {
        attach_hidden(&mut req.messages, n.clone());
    }
    req.tools = specs;
    req.no_tool_call = no_tool_call;
    req.max_tokens = reply_tokens(st);
    // the request and its reply within the window, or the person told why not
    super::window_fit::fit(st, &mut req, notes.as_deref(), extra).await;
    req.temperature = st.config.agent.temperature;
    req.reasoning = reasoning_of(&effort);
    req.session_id = Some(st.session.id().to_string());
    // the hour write is for a conversation that has shown it pauses: the prices and the pauses seen decide, the
    // provider decides whether it has one
    st.pauses.start(Instant::now());
    let prices = st.limits.pricing.or_else(|| cfg.pricing_for(&req.model));
    req.cache_long = super::cache_ttl::wants_long(
        st.pauses.seen(),
        request_tokens(st, &req.tools),
        st.config.agent.long_cache_over,
        prices.as_ref(),
    );
    if let (true, Some((key, value))) = (st.fast, fast::option(&cfg)) {
        req.extra.insert(key.into(), Value::from(value));
    }
    st.last_request = Some(req.clone());
    Ok((req, cfg, st.client.clone(), st.config.agent.retries))
}

/// The window the view is kept under, in estimated tokens (4 characters a token): `room` provider
/// tokens divided by the ratio learned, with 5% to spare; before any count, 15% to spare.
pub(crate) fn view_window(room: u32, ratio: Option<f64>) -> u64 {
    match ratio {
        Some(r) => (f64::from(room) * 0.95 / r) as u64,
        None => u64::from(room) * 17 / 20,
    }
}

/// The ratio of provider tokens to estimated tokens after a request of `est` estimated tokens the
/// provider counted as `real`: the largest of the last ones weighs most, so a request of code after
/// one of prose does not pass the window; small requests, whose count is mostly framing, teach nothing.
pub(crate) fn learn_ratio(prev: Option<f64>, est: u64, real: u64) -> Option<f64> {
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

/// Estimated tokens of what every request carries besides the conversation: the system prompt and the tools.
pub(crate) fn fixed_tokens(system: &str, specs: &[nexo_core::provider::ToolSpec]) -> u64 {
    (system.len() as u64 + serde_json::to_string(specs).map(|s| s.len() as u64).unwrap_or(0)) / 4
}

/// The blocks that ride on this request only, as messages, to count what the view's budget must leave room for.
pub(super) fn request_only_tokens(blocks: &[&String]) -> u64 {
    let msgs: Vec<Message> = blocks.iter().map(|t| Message::user((*t).clone())).collect();
    nexo_core::message::estimate_tokens(&msgs)
}

/// What the next call will see, announced to the interface: on every call, and
/// when a conversation is loaded, so its numbers are there before the first call.
pub(super) async fn view_now(st: &mut State, extra_tokens: u64) -> View {
    let specs = offered(st);
    // what is not the history but rides on the request: the system prompt, the tools and `extra_tokens` (the
    // blocks added to this request only)
    let system_tokens = fixed_tokens(&st.system, &specs) + extra_tokens;
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
        Ok(v) => {
            st.view_failed = false;
            v
        }
        Err(e) => {
            st.view_failed = true;
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
    // what repeats the last request is neither copied nor measured again
    let (same, view_bytes) = st.last_sent.follow(&view.messages);
    let history_tokens = st.history.tokens();
    let last = std::mem::replace(&mut st.last_view, (history_tokens, view.tokens));
    st.est_sent = system_tokens + view.tokens;
    let (label, detail) = crate::context::describe_since(st.policy.as_ref(), history_tokens, view.tokens, Some(last));
    emit(
        &st.events,
        AgentEvent::Context {
            view_tokens: view.tokens,
            history_tokens,
            history_bytes: st.history.bytes(),
            view_bytes,
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
    let calls = crate::context_map::calls(&st.history, &cells, &view.messages, same);
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
/// directory, are text of at most 24 KB and are not in the conversation already: up to three, which nexo
/// includes under its own header (never as the person's `@path`), so the model starts with them and needs no
/// step to read them.
///
/// A named file the view already holds whole and unchanged (a `write` of it, not an `edit`) is not attached again:
/// it comes back as a line saying so, for the start note.
fn named_files(st: &State, text: &str, explicit: &[Attachment]) -> (Vec<Attachment>, Vec<String>) {
    if !st.config.agent.attach_named_files {
        return (Vec::new(), Vec::new());
    }
    let root = st.cwd.canonicalize().unwrap_or_else(|_| st.cwd.clone());
    let mut seen: Vec<PathBuf> = explicit
        .iter()
        .map(|a| {
            let p = if a.path.is_absolute() { a.path.clone() } else { st.cwd.join(&a.path) };
            p.canonicalize().unwrap_or(p)
        })
        .collect();
    let said = orient::carried(&st.history);
    let mut out = Vec::new();
    let mut known = Vec::new();
    for word in text.split_whitespace() {
        if out.len() + known.len() >= NAMED_FILES {
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
        if std::str::from_utf8(&bytes).is_err() {
            continue;
        }
        // the same policy as `read`: key files ask, denied paths stay out
        if st.perm.decide("read", ToolKind::Read, &serde_json::json!({ "path": word }), &st.cwd) != Decision::Allow {
            continue;
        }
        // attached to an earlier request, or included in one: the conversation holds it
        if said.contains(word) {
            continue;
        }
        // the model holds the whole file (it wrote it, or its command made it and printed it) and it is unchanged
        let made = orient::made_in_work(&st.worked, &real);
        if let Some(how) = st.reads.held_whole(&real, &bytes, made) {
            known.push(format!("{word}: {how}, unchanged since; not attached again. Read it if you need its lines."));
            seen.push(real);
            continue;
        }
        seen.push(real);
        out.push(Attachment { path: PathBuf::from(word) });
    }
    (out, known)
}

#[cfg(test)]
mod request_only_tests {
    use super::*;

    #[tokio::test]
    async fn cancelled_directed_item_is_explicitly_rejected_at_incorporation() {
        use nexo_core::agent::{DirectedReply, DirectedResult, ObservationTarget};
        let (tx, rx) = tokio::sync::oneshot::channel();
        let mut item = InboxItem::sent("queued".into(), Vec::new(), true);
        item.directed = Some((
            ObservationTarget { instance_id: "instance".into(), session_id: "session".into(), generation: 0 },
            DirectedReply::new(tx),
        ));
        let cancel = CancellationToken::new();
        assert!(may_incorporate(Some((&item, &cancel))));
        cancel.cancel();
        assert!(!may_incorporate(Some((&item, &cancel))));
        assert_eq!(rx.await.unwrap(), DirectedResult::Rejected("turn cancelled"));
    }

    #[test]
    fn what_rides_on_the_request_only_is_counted_with_the_estimator_of_the_budget() {
        let block = nexo_supervise::watch::hidden_block(&["x".repeat(40_000)]);
        let t = request_only_tokens(&[&block]);
        assert!(t >= 10_000, "{t}");
        assert_eq!(t, nexo_core::message::estimate_tokens(&[Message::user(block)]));
        assert_eq!(request_only_tokens(&[]), 0);
    }

    #[test]
    fn the_levels_above_high_become_a_reasoning_setting_not_none() {
        for (s, e) in [("xhigh", Effort::Xhigh), ("max", Effort::Max), (" MAX ", Effort::Max)] {
            assert_eq!(reasoning_of(&Some(s.into())), Some(ReasoningConfig::Effort(e)), "{s}");
        }
    }
}
