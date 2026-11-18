//! `api`: `nexo api` as a tool, so a model that wants the API behind a website asks for it in one call: read a
//! HAR capture (`analyze`), look at a saved map (`describe`), call an endpoint of it (`call`). The command line is
//! the one `nexo api` takes, parsed by the same clap definition, and the answer is the text the CLI prints (already
//! scrubbed of secrets).
//!
//! What the model never gets: a `watch` that is not bounded (`--times 1` to 3, inside one call's time, no
//! `--on-change`, no channel), `check`, `--reveal` (the raw answer with
//! its secrets), a secret value in the command (it names an environment variable instead: `--auth bearer=MY_TOKEN`,
//! `-p password=@env:PW`), and a body read from a file.
//!
//! The call has a kind decided from the parsed command (`call_kind`), so the permission policy asks as it does for
//! the tool that does the same thing by hand:
//!
//! | call | kind | asked like |
//! | --- | --- | --- |
//! | `analyze`, `describe`, `call --dry-run` | read | a file read |
//! | `analyze --save/--openapi` | write | `edit` and `write` |
//! | `call` of a GET or HEAD endpoint | network | `web_fetch` |
//! | `call` of any other method, or with `--login` or `--via-browser` | exec | a shell command that changes things |
//! | `watch --times N` of a GET or HEAD endpoint | network | `web_fetch` (exec with `--state`, which writes) |

use async_trait::async_trait;
use clap::error::ErrorKind;
use clap::Parser;
use nexo_api::exchange;
use nexo_core::tool::Check;
use nexo_core::{Compact, Tool, ToolContext, ToolKind, ToolOutput, ToolSpec};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

use crate::api_cmd::{ApiArgs, ApiCmd, Target};
use crate::re_mcp::split_words;

#[cfg(test)]
mod tests;

/// Most characters one answer carries; the rest is counted.
const MAX_CHARS: usize = 12_000;

const OPS: &str = "\
analyze <har> [--save MAP] [--openapi FILE] [--host H] [--budget N] [--json]  read a HAR file: devtools Save all as HAR, or the browser tool's own capture (capture_start, browse, capture_stop, capture_save with a `path`; in the person's own Chrome it holds only URLs and statuses, so no shapes): one line per endpoint with its path params, JSON shapes, auth and paging; --save keeps the map for the ops below
describe <map> [endpoint] [--budget N] [--flows]  (--flows: the chains, where each value of a call comes from) the endpoints of a map (the summary takes about N characters, default 6000), or one endpoint's params, body, auth and the environment variables it reads
call <map> <endpoint> [-p name=value]... [--body 'JSON'] [--auth NAME=ENVVAR]... [--login] [--via-browser[=ID]] [--dry-run] [--json] [--select PATH,...]  call one endpoint (an id, or \"METHOD /path\" in quotes); secrets in the answer show as <secret>; --select prints only those values as `path<TAB>value` ($.a.b, $.items[0].price, $.items[*].id; no match prints (none))
watch <map> <endpoint> --times 1..3 [--every 10s..] [--select PATH,...] [--when EXPR] [--state FILE]  poll a few times inside one call (at most 120s in all) and compare only the selected values; --when is `PATH OP VALUE` (OP < <= > >= == !=, VALUE a number like 1299, 1,299.00, $1,299.00, a \"string\" or prev) or `PATH changed`, joined by and/or; a true --when or a change prints one `alert:` line and the exit code 4 is added. A longer watch (every 10m, --on-change) is the person's: `nexo api watch`
client <map> --lang ts|py|sh [--out FILE] [--only EP]... [--force] [--date YYYY-MM-DD]  write a small client for the map (one function per endpoint, paging in ts and py); secrets are read from environment variables when it runs and are never in the text; --out writes a file and never replaces one without --force
Never write a token, key or password in a command: name an environment variable (--auth bearer=MY_TOKEN, -p password=@env:PW); a value that looks like a secret is refused. Start with --dry-run: it prints the request and sends nothing.
GET and HEAD calls are asked like web_fetch; POST, PUT, PATCH, DELETE, --login and --via-browser (a signed-in browser sends it) like a shell command that changes things; --save, --openapi and client --out write a file. watch is bounded (--times 1 to 3, no --on-change, no live channel, --state under the session folder) and asked like call; no check, no --reveal, no body from a file";

const COMPACT: &str = "The API behind a website: `analyze cap.har --save map.json`, `describe map.json [EP]`, `call map.json EP -p k=v --auth NAME=ENVVAR [--select $.a.b]` (--dry-run first), `client map.json --lang ts --out c.ts`, `watch map.json EP --times 2 --every 30s --select $.price --when \"$.price < prev\"`. Secrets by environment variable name only. `--help`";

/// The tool. It holds nothing: every call parses its own line.
pub struct Api;

/// What a line parses to.
#[derive(Debug)]
pub(crate) enum Parsed {
    Help(String),
    Cmd(ApiCmd),
}

/// The command line as `nexo api` reads it. A leading `nexo api` or `api` is allowed.
pub(crate) fn parse(line: &str) -> Result<Parsed, String> {
    let mut words = split_words(line)?;
    if words.first().is_some_and(|w| w == "nexo") {
        words.remove(0);
    }
    if words.first().is_some_and(|w| w == "api") {
        words.remove(0);
    }
    if words.first().is_none_or(|w| w == "help") {
        return Ok(Parsed::Help(OPS.to_string()));
    }
    let argv = ["nexo".to_string(), "api".to_string()].into_iter().chain(words);
    match crate::cli::Args::try_parse_from(argv) {
        Ok(a) => match a.cmd {
            Some(crate::cli::Cmd::Api(ApiArgs { what })) => Ok(Parsed::Cmd(what)),
            _ => Err("not an api op".to_string()),
        },
        Err(e) if matches!(e.kind(), ErrorKind::DisplayHelp | ErrorKind::DisplayVersion) => {
            Ok(Parsed::Help(e.to_string()))
        }
        Err(e) => Err(e.to_string()),
    }
}

/// What the model may not ask for, and a value that looks like a secret. `Ok` means the call may go on.
pub(crate) fn vet(cmd: &ApiCmd) -> Result<(), String> {
    match cmd {
        ApiCmd::Watch { target, every, times, state, follow, when, on_change } => {
            vet_watch(target, every.as_deref(), *times, state.as_deref(), follow.as_deref(), when.as_deref(), on_change.as_deref())
        }
        ApiCmd::Check { .. } => Err(
            "check calls many endpoints of a map in one go, so the person runs it: `nexo api check MAP`".into(),
        ),
        ApiCmd::Call { reveal: true, .. } => Err(
            "--reveal prints the raw answer with its secrets, and the model never sees those; the person can run `nexo api call --reveal`"
                .into(),
        ),
        ApiCmd::Call { target, .. } => vet_target(target),
        ApiCmd::Analyze { .. } | ApiCmd::Describe { .. } | ApiCmd::Client { .. } => Ok(()),
    }
}

/// The most a watch of the tool may take in all: the polls, their pauses, and each call's own timeout.
pub(crate) const WATCH_BUDGET_SECS: u64 = 120;
/// Most polls of one watch of the tool.
pub(crate) const WATCH_MAX_TIMES: u32 = 3;
/// What one poll may take at most (the call timeout of `nexo api`).
const POLL_SECS: u64 = 30;

/// A watch the model may run: a few polls of a plain endpoint, inside the time one tool call has. Longer or
/// open-ended watches, channels and `--on-change` are the person's.
fn vet_watch(
    target: &Target,
    every: Option<&str>,
    times: Option<u32>,
    state: Option<&Path>,
    follow: Option<&str>,
    when: Option<&str>,
    on_change: Option<&str>,
) -> Result<(), String> {
    const OWN: &str =
        "the person runs a longer watch: `nexo api watch MAP EP --every 10m --select PATH --when EXPR --state FILE`";
    if on_change.is_some() {
        return Err(format!("--on-change runs a shell command, so only the person may use it; {OWN}"));
    }
    if follow.is_some() {
        return Err(format!("--for follows a live channel for a while; {OWN}"));
    }
    let Some(n) = times else {
        return Err(format!("watch needs --times 1 to {WATCH_MAX_TIMES} here (it would run until stopped); {OWN}"));
    };
    if !(1..=WATCH_MAX_TIMES).contains(&n) {
        return Err(format!("--times {n}: the tool allows 1 to {WATCH_MAX_TIMES} polls; {OWN}"));
    }
    let pause = match every {
        Some(e) => nexo_api::call::duration(e).map_err(|e| format!("--every: {e}"))?,
        None if n == 1 => std::time::Duration::ZERO,
        None => return Err("--every DURATION is needed with more than one poll (at least 10s)".to_string()),
    };
    if pause < nexo_api::call::MIN_EVERY && n > 1 {
        return Err(format!("--every is too short: the least is {}s", nexo_api::call::MIN_EVERY.as_secs()));
    }
    let worst = pause.as_secs().saturating_mul(u64::from(n - 1)) + POLL_SECS * u64::from(n);
    if worst > WATCH_BUDGET_SECS {
        return Err(format!(
            "{n} polls {}s apart can take {worst}s, more than the {WATCH_BUDGET_SECS}s one tool call has; use a shorter --every or fewer --times, or {OWN}",
            pause.as_secs()
        ));
    }
    if let Some(p) = state {
        if p.components().any(|c| matches!(c, std::path::Component::ParentDir)) {
            return Err("--state: no `..` in the path; keep the file under the session's folder".to_string());
        }
    }
    crate::api_watch::Sel::build(&target.select, when, None, state)?;
    vet_target(target)
}

fn vet_target(t: &Target) -> Result<(), String> {
    for p in t.params.iter().map(|p| ("-p", p)).chain(t.login_params.iter().map(|p| ("--login-param", p))) {
        if let Some(why) = param_problem(p.0, p.1) {
            return Err(why);
        }
    }
    for (flag, body) in [("--body", &t.body), ("--login-body", &t.login_body)] {
        if let Some(b) = body {
            if let Some(why) = body_problem(flag, b) {
                return Err(why);
            }
        }
    }
    for a in &t.auth {
        if let Some(why) = auth_problem(a) {
            return Err(why);
        }
    }
    Ok(())
}

const ENV_HINT: &str = "put the value in an environment variable and pass `@env:VAR`";

/// `name=value` where the value looks like a secret, or the name says it is one and the value is not `@env:VAR`.
fn param_problem(flag: &str, arg: &str) -> Option<String> {
    let (name, value) = arg.split_once('=')?;
    let (name, value) = (name.trim(), value.trim());
    if value.is_empty() || value.starts_with("@env:") {
        return None;
    }
    let last = name.rsplit('.').next().unwrap_or(name);
    if exchange::secret_param(name) || exchange::secret_param(last) || exchange::opaque(value) {
        return Some(format!(
            "{flag} {name}=...: the value looks like a secret; {ENV_HINT}, as in `{flag} {name}=@env:VAR`"
        ));
    }
    None
}

/// A body that is read from a file, or holds a secret-looking value or a field with a secret name.
fn body_problem(flag: &str, body: &str) -> Option<String> {
    if body.trim_start().starts_with('@') {
        return Some(format!("{flag}: pass the JSON inline, not @file"));
    }
    let v: Value = serde_json::from_str(body).ok()?;
    let at = secret_in(&v, "")?;
    let at = if at.is_empty() { "the body".to_string() } else { at };
    Some(format!(
        "{flag} {at}: the value looks like a secret; leave it out of the body and {ENV_HINT} with `-p NAME=@env:VAR`"
    ))
}

/// The path of the first string in `v` that looks like a secret.
fn secret_in(v: &Value, path: &str) -> Option<String> {
    match v {
        Value::Object(m) => {
            for (k, x) in m {
                let here = if path.is_empty() { k.clone() } else { format!("{path}.{k}") };
                let named = matches!(x, Value::String(s) if !s.is_empty() && exchange::secret_name(k));
                if named {
                    return Some(here);
                }
                if let Some(p) = secret_in(x, &here) {
                    return Some(p);
                }
            }
            None
        }
        Value::Array(a) => a.iter().enumerate().find_map(|(i, x)| secret_in(x, &format!("{path}[{i}]"))),
        Value::String(s) if exchange::opaque(s) => Some(path.to_string()),
        _ => None,
    }
}

/// `--auth NAME=ENVVAR` must name a variable, not carry the value.
fn auth_problem(arg: &str) -> Option<String> {
    let (name, var) = arg.split_once('=')?;
    let (name, var) = (name.trim(), var.trim());
    let ident = !var.is_empty()
        && !var.starts_with(|c: char| c.is_ascii_digit())
        && var.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
    let token = exchange::opaque(var) && var.chars().any(|c| c.is_ascii_lowercase());
    if !ident || token {
        return Some(format!(
            "--auth {name}=...: give the NAME of an environment variable (MY_TOKEN), never the secret itself"
        ));
    }
    None
}

/// The kind of a parsed call: what the permission policy decides by. A relative map is looked up from `base`.
pub(crate) fn kind_in(cmd: &ApiCmd, base: &Path) -> ToolKind {
    match cmd {
        ApiCmd::Analyze { openapi, save, .. } if openapi.is_some() || save.is_some() => ToolKind::Write,
        ApiCmd::Analyze { .. } | ApiCmd::Describe { .. } => ToolKind::Read,
        // a client that goes to a file is a write; one that is only printed is a read
        ApiCmd::Client { out: Some(_), .. } => ToolKind::Write,
        ApiCmd::Client { .. } => ToolKind::Read,
        ApiCmd::Call { dry_run: true, .. } => ToolKind::Read,
        ApiCmd::Call { target, .. } | ApiCmd::Watch { target, .. } => {
            // a watch that keeps a state file writes: asked like a command that changes things
            if target.login || target.via_browser.is_some() || matches!(cmd, ApiCmd::Watch { state: Some(_), .. }) {
                return ToolKind::Exec;
            }
            let map = if target.map.is_relative() { base.join(&target.map) } else { target.map.clone() };
            match crate::api_call::target_info(&map, &target.endpoint) {
                Ok((m, _)) if m.eq_ignore_ascii_case("GET") || m.eq_ignore_ascii_case("HEAD") => ToolKind::Network,
                // another method, or a map that cannot be read: the stricter kind
                _ => ToolKind::Exec,
            }
        }
        ApiCmd::Check { .. } => ToolKind::Exec,
    }
}

/// The folder this process runs in: where `call_kind`, which has no session folder, looks a relative map up.
fn process_dir() -> PathBuf {
    std::env::current_dir().unwrap_or_default()
}

fn absolute(cwd: &Path, p: &mut PathBuf) {
    if p.is_relative() {
        *p = cwd.join(&*p);
    }
}

/// With `--dry-run` nothing is sent, so a `@env:VAR` value is shown as `<VAR>` and the variable is not read: a
/// dry run needs no approval and must not be a way to read the environment.
fn hide_env(list: &mut [String]) {
    for p in list {
        if let Some((name, value)) = p.split_once('=') {
            if let Some(var) = value.strip_prefix("@env:") {
                *p = format!("{name}=<{var}>");
            }
        }
    }
}

/// The permission rules for a file the call reads: a rule that denies it stops the call, one that would ask is
/// reported (the call was asked about as a whole, not for each file in it).
fn may_read(ctx: &ToolContext, path: &Path) -> Result<(), String> {
    let Some(check) = &ctx.check else { return Ok(()) };
    let p = path.to_string_lossy().to_string();
    match check("read", ToolKind::Read, &json!({ "path": p })) {
        Check::Allow => Ok(()),
        Check::Deny => Err(format!("api: a permission rule denies reading {p}")),
        Check::Ask => {
            Err(format!("api: reading {p} needs the person's OK under the permission rules, so it was not read"))
        }
    }
}

/// A file the call writes: the call was asked about as a write, so only a rule that denies the place stops it.
fn may_write(ctx: &ToolContext, path: &Path) -> Result<(), String> {
    let Some(check) = &ctx.check else { return Ok(()) };
    let p = path.to_string_lossy().to_string();
    if check("edit", ToolKind::Write, &json!({ "path": p })) == Check::Deny {
        return Err(format!("api: a permission rule denies writing {p}"));
    }
    Ok(())
}

/// The answer cut to `max` characters, with how many were left out.
pub(crate) fn capped(text: String, max: usize) -> String {
    let n = text.chars().count();
    if n <= max {
        return text;
    }
    let head: String = text.chars().take(max).collect();
    format!(
        "{head}\n[cut: {} more characters; ask for one endpoint (`describe MAP EP`) or a smaller `--budget`]",
        n - max
    )
}

#[async_trait]
impl Tool for Api {
    fn name(&self) -> &str {
        "api"
    }

    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "api".into(),
            description: format!(
                "Work out the API behind a website from a HAR capture, and call it. One op per call, as `nexo api` takes it. Secrets come only from environment variable names, never from the command.\n{OPS}"
            ),
            parameters: json!({
                "type": "object",
                "properties": {
                    "command": {"type": "string", "description": "One op with its flags, e.g. `analyze cap.har --save map.json`, `describe map.json`, `call map.json \"GET /v1/items\" -p page=2 --dry-run`."}
                },
                "required": ["command"]
            }),
        }
    }

    fn compact(&self) -> Option<Compact<'_>> {
        Some(Compact { description: COMPACT, params: &[] })
    }

    fn rare(&self) -> bool {
        true
    }

    /// The strictest a call can be: a sub-agent that may only read or look things up does not get it.
    fn kind(&self) -> ToolKind {
        ToolKind::Exec
    }

    fn call_kind(&self, args: &Value) -> ToolKind {
        let Some(line) = args.get("command").and_then(Value::as_str) else { return ToolKind::Read };
        match parse(line) {
            Ok(Parsed::Cmd(cmd)) => match vet(&cmd) {
                Ok(()) => kind_in(&cmd, &process_dir()),
                // refused in `run`, which sends nothing
                Err(_) => ToolKind::Read,
            },
            _ => ToolKind::Read,
        }
    }

    fn summary(&self, args: &Value) -> String {
        match args.get("command").and_then(Value::as_str) {
            Some(c) => format!("api: {c}"),
            None => "api".into(),
        }
    }

    async fn run(&self, args: Value, ctx: &ToolContext) -> ToolOutput {
        let Some(line) = args.get("command").and_then(Value::as_str) else {
            return ToolOutput::error(
                "api needs `command`, for example `analyze capture.har --save map.json`; `--help` lists the ops",
            );
        };
        let mut cmd = match parse(line) {
            Ok(Parsed::Help(h)) => return ToolOutput::ok(h),
            Ok(Parsed::Cmd(c)) => c,
            Err(e) => return ToolOutput::error(format!("api: {e}")),
        };
        if let Err(e) = vet(&cmd) {
            return ToolOutput::error(format!("api: {e}"));
        }
        // the call was asked about with the kind `call_kind` gave, which looks a relative map up from this
        // process's folder; if the session's folder gives a stricter kind (another map of that name), stop
        let real = kind_in(&cmd, &ctx.cwd);
        if real == ToolKind::Exec && kind_in(&cmd, &process_dir()) != ToolKind::Exec {
            return ToolOutput::error("api: the map name leads to different files from the session's folder and from the permission check; give its absolute path");
        }
        match run_cmd(&mut cmd, ctx).await {
            Ok(text) => ToolOutput::ok(capped(text, MAX_CHARS)),
            Err(e) => ToolOutput::error(capped(e, MAX_CHARS)),
        }
    }
}

/// Joins what a run said into one text.
fn joined(text: String, notes: Vec<String>) -> String {
    let mut out = text;
    for n in notes {
        if !out.is_empty() && !out.ends_with('\n') {
            out.push('\n');
        }
        out.push_str(&format!("nexo api: {n}\n"));
    }
    out
}

async fn run_cmd(cmd: &mut ApiCmd, ctx: &ToolContext) -> Result<String, String> {
    match cmd {
        ApiCmd::Analyze { har, host, openapi, json, budget, save } => {
            absolute(&ctx.cwd, har);
            may_read(ctx, har)?;
            for out in [openapi.as_mut(), save.as_mut()].into_iter().flatten() {
                absolute(&ctx.cwd, out);
                may_write(ctx, out)?;
            }
            let (har, host, openapi, json, budget, save) =
                (har.clone(), host.clone(), openapi.clone(), *json, *budget, save.clone());
            let done = tokio::task::spawn_blocking(move || {
                crate::api_cmd::analyze_text(&har, &host, openapi.as_deref(), json, budget, save.as_deref())
            });
            match done.await {
                Ok(Ok(text)) => Ok(text),
                Ok(Err(e)) => Err(format!("api: {e}")),
                Err(e) => Err(format!("api stopped working: {e}")),
            }
        }
        ApiCmd::Describe { map, endpoint, budget, flows } => {
            absolute(&ctx.cwd, map);
            may_read(ctx, map)?;
            match crate::api_call::describe_out(map, endpoint.as_deref(), *budget, *flows) {
                Ok(o) => Ok(joined(o.text, o.notes)),
                Err((_, e)) => Err(format!("api: {e}")),
            }
        }
        ApiCmd::Client { map, lang, out, only, force, date } => {
            absolute(&ctx.cwd, map);
            may_read(ctx, map)?;
            if let Some(out) = out.as_mut() {
                absolute(&ctx.cwd, out);
                may_write(ctx, out)?;
            }
            let (map, lang, out, only, force, date) =
                (map.clone(), lang.clone(), out.clone(), only.clone(), *force, date.clone());
            let done = tokio::task::spawn_blocking(move || {
                crate::api_cmd::client_text(&map, &lang, out.as_deref(), &only, force, date.as_deref())
            });
            match done.await {
                Ok(Ok(text)) => Ok(text),
                Ok(Err(e)) => Err(format!("api: {e}")),
                Err(e) => Err(format!("api stopped working: {e}")),
            }
        }
        ApiCmd::Call { target, dry_run, json, .. } => {
            absolute(&ctx.cwd, &mut target.map);
            may_read(ctx, &target.map)?;
            if *dry_run {
                hide_env(&mut target.params);
                hide_env(&mut target.login_params);
            } else if let Ok((_, origin)) = crate::api_call::target_info(&target.map, &target.endpoint) {
                // the rules for a host (`web_fetch`) cover this call too: a rule that denies it stops the call
                if let Some(check) = &ctx.check {
                    if check("web_fetch", ToolKind::Network, &json!({ "url": origin })) == Check::Deny {
                        return Err(format!("api: a permission rule denies {origin}"));
                    }
                }
            }
            match crate::api_call::call_out(target, *dry_run, *json, false).await {
                Ok(o) => Ok(joined(o.text, o.notes)),
                Err((_, e)) => Err(format!("api: {e}")),
            }
        }
        ApiCmd::Watch { target, every, times, state, when, .. } => {
            absolute(&ctx.cwd, &mut target.map);
            may_read(ctx, &target.map)?;
            if let Some(st) = state.as_mut() {
                absolute(&ctx.cwd, st);
                if !st.starts_with(&ctx.cwd) {
                    return Err("api: --state must be a file under the session's folder".to_string());
                }
                may_write(ctx, st)?;
            }
            if let Ok((_, origin)) = crate::api_call::target_info(&target.map, &target.endpoint) {
                if let Some(check) = &ctx.check {
                    if check("web_fetch", ToolKind::Network, &json!({ "url": origin })) == Check::Deny {
                        return Err(format!("api: a permission rule denies {origin}"));
                    }
                }
            }
            let sel = crate::api_watch::Sel::build(&target.select, when.as_deref(), None, state.as_deref())
                .map_err(|e| format!("api: {e}"))?;
            match crate::api_call::watch_out(target, every.as_deref(), *times, state.as_deref(), sel).await {
                Ok(mut o) => {
                    match o.code {
                        0 => {}
                        c if c == crate::api_watch::EXIT_ALERT => {
                            o.notes.push("exit code 4: the alert came true".to_string())
                        }
                        c => o.notes.push(format!("exit code {c}: the last call failed")),
                    }
                    Ok(joined(o.text, o.notes))
                }
                Err((_, e)) => Err(format!("api: {e}")),
            }
        }
        // `vet` refused it before
        ApiCmd::Check { .. } => Err("api: check is not offered to the model".to_string()),
    }
}
