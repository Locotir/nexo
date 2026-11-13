use super::*;

/// The command a shell says it did not find on `line`, if it does: `sh: 1: X: not found`,
/// `bash: [line N: ]X: command not found`, `zsh: command not found: X`, `X: command not found`
/// the Windows `'X' is not recognized as an internal or external command`, and PowerShell's
/// `The term 'X' is not recognized as the name of a cmdlet` (5.1) or `as a name of a cmdlet` (7).
pub(super) fn not_found_name(line: &str) -> Option<String> {
    let line = line.trim_end();
    let name = if let Some((_, rest)) = line.split_once("The term '") {
        // the line may wrap after "recognized as" in a narrow host, so the rest is not required
        rest.split_once("' is not recognized as")?.0
    } else if let Some(rest) = line.strip_prefix('\'') {
        rest.split_once("' is not recognized as an internal or external command")?.0
    } else if let Some((_, name)) = line.rsplit_once("command not found: ") {
        name
    } else if let Some(before) = line.strip_suffix(": command not found") {
        before.rsplit(": ").next()?
    } else {
        // dash and ash: `sh: 1: X: not found`; other tools' "not found" are not the shell's
        let before = line.strip_suffix(": not found")?;
        if !before.split(": ").next()?.ends_with("sh") {
            return None;
        }
        before.rsplit(": ").next()?
    };
    (!name.is_empty() && !name.contains(char::is_whitespace)).then(|| name.to_string())
}

/// For a failed `a && b && c`: the note to give when a link before the last is `ls`, `cat` or
/// `stat` of a plain path that does not exist (resolved after any `cd` links). `&&` stops there,
/// so what follows never ran, which the model takes for an answer about the rest. Anything
/// not plainly that (quotes, variables, globs, `;`, `||`, pipes) gets no note.
pub(super) fn skipped_by_probe(command: &str, cwd: &std::path::Path) -> Option<String> {
    let links: Vec<&str> = command.split("&&").collect();
    let mut dir = cwd.to_path_buf();
    for link in &links[..links.len().checked_sub(1)?] {
        if link.contains(|c: char| "|;&<(){}$`'\"*?[~\n".contains(c)) {
            return None;
        }
        let words: Vec<&str> = link.split_whitespace().filter(|w| !w.contains('>')).collect();
        match words.split_first() {
            Some((&"cd", [to])) => dir = dir.join(to),
            Some((&"cd", _)) => return None,
            Some((&("ls" | "cat" | "stat"), args)) => {
                if let Some(missing) = args.iter().find(|a| !a.starts_with('-') && !dir.join(a).exists()) {
                    return Some(format!(
                        "[note: `{missing}` does not exist, so && skipped the commands after it; join with ; (or `|| true`) for paths that may not exist]"
                    ));
                }
            }
            _ => {}
        }
    }
    None
}

/// The refusal for a foreground command that starts with a fixed wait of 2 seconds or more
/// (`sleep 45; ...`, `sleep 25 && ...`, `sleep 0.5m` on a line of its own): it wastes the time when the
/// thing is ready and is too short when it is not. Only a leading `sleep` followed by `;`, `&&`, a
/// newline or the end counts. Left alone: `sleep 1` or less; a `sleep` in a loop body, a quoted
/// `sh -c '...'`, a heredoc or a background job (`sleep 20 &`), because none of those is first at
/// top level; and a `sleep` after other commands (`cmd; sleep 5; cmd2`), because there the wait may
/// be a pause between steps that the model cannot poll for, and refusing it would be a false positive.
pub(super) fn blind_sleep(command: &str) -> Option<String> {
    if nexo_core::shell::shell().kind == "powershell" {
        return None;
    }
    let after = command.trim_start().strip_prefix("sleep")?;
    if !after.starts_with([' ', '\t']) {
        return None;
    }
    let (mut rest, mut secs, mut args) = (after, 0.0_f64, 0);
    loop {
        rest = rest.trim_start_matches([' ', '\t']);
        let end = rest.find(|c: char| c.is_whitespace() || ";&|<>()`$'\"\\".contains(c)).unwrap_or(rest.len());
        if end == 0 {
            break;
        }
        secs += sleep_seconds(&rest[..end])?;
        args += 1;
        rest = &rest[end..];
    }
    let waited = format!("sleep{}", after[..after.len() - rest.len()].trim_end());
    let rest = match rest {
        "" => "",
        r if r.starts_with("&&") => &r[2..],
        r if r.starts_with([';', '\n']) => &r[1..],
        _ => return None,
    };
    if args == 0 || secs < 2.0 {
        return None;
    }
    let rest = rest.trim();
    let cap = ((secs * 2.0).ceil() as u64).clamp(2, MAX_TIMEOUT_S);
    let cond = poll_condition(rest).unwrap_or_else(|| "<what shows it is ready>".into());
    let (tail, rest_json) = match rest {
        "" => (String::new(), "\"true\"".to_string()),
        r if !r.contains('\n') && r.len() <= 200 => (format!("; {r}"), serde_json::to_string(r).unwrap_or_default()),
        _ => ("; <the rest of your command>".into(), "\"<the rest of your command>\"".into()),
    };
    let cond_json = serde_json::to_string(&cond).unwrap_or_default();
    Some(format!(
        "bash: not run. A fixed wait (`{}`) wastes time when the thing is already ready and is too short when it is not.\n\
         Wait for the condition instead: {{\"wait_until\": {cond_json}, \"command\": {rest_json}, \"timeout\": {cap}}}. \
         It is a command that succeeds once ready: `[ -e FILE ]`, `curl -sf URL`, `grep -q READY LOG`. \
         In one command: timeout {cap} sh -c 'until {cond}; do sleep 1; done'{tail}\n\
         To wait for a command you started with `background: true`, use jobs {{\"action\": \"output\", \"id\": N, \"wait\": 60}}.",
        one_line(&waited, 60)
    ))
}

/// Polls the shell condition `cond` about every 0.5 s for up to `timeout`. None once it succeeds
/// (the command may run); otherwise the result to return at once: the condition never held, the
/// call was cancelled, or the shell could not start. Plain polling, not inotify: it works the same
/// for any condition and on every system, and half a second is no wait to speak of.
pub(super) async fn wait_until(
    cond: &str,
    timeout: Duration,
    ctx: &ToolContext,
    sandbox: &nexo_core::sandbox::SandboxSettings,
) -> Option<ToolOutput> {
    const EVERY: Duration = Duration::from_millis(500);
    let start = std::time::Instant::now();
    loop {
        let mut last = Capture::new();
        let left = timeout.saturating_sub(start.elapsed()).max(Duration::from_secs(1));
        match run_command(cond, &ctx.cwd, sandbox, Some(left), &ctx.cancel, Some(&tmp_dir(&ctx.session_id)), |l| {
            last.push(l)
        })
        .await
        {
            End::Exited(0) => return None,
            End::Cancelled => return Some(ToolOutput::error("[cancelled while waiting]")),
            End::Failed(e) => {
                let label = nexo_core::shell::shell().label();
                return Some(ToolOutput::error(format!("{label}: cannot run the condition: {e}")));
            }
            _ => {}
        }
        let waited = start.elapsed();
        if waited >= timeout {
            let mut content = format!("[waited {} s: condition never held]", waited.as_secs());
            let out = last.text();
            if !out.trim().is_empty() {
                content.push_str("\nlast output of the condition:\n");
                content.push_str(&cap_text(out.trim_end(), 2000));
            }
            return Some(ToolOutput::error(content));
        }
        tokio::select! {
            _ = tokio::time::sleep(EVERY.min(timeout - waited)) => {}
            _ = ctx.cancel.cancelled() => return Some(ToolOutput::error("[cancelled while waiting]")),
        }
    }
}

/// Seconds in a `sleep` argument: a number with an optional `s`, `m`, `h` or `d`.
pub(super) fn sleep_seconds(word: &str) -> Option<f64> {
    if !word.starts_with(|c: char| c.is_ascii_digit() || c == '.') {
        return None;
    }
    let (num, unit) = match word.char_indices().find(|(_, c)| c.is_ascii_alphabetic()) {
        Some((i, _)) => word.split_at(i),
        None => (word, "s"),
    };
    let mult = match unit {
        "s" => 1.0,
        "m" => 60.0,
        "h" => 3600.0,
        "d" => 86400.0,
        _ => return None,
    };
    num.parse::<f64>().ok().filter(|n| n.is_finite()).map(|n| n * mult)
}

/// A condition for the poll when the command after the wait plainly shows what it waits for:
/// a `curl` of a URL, or a plain absolute path on a single line. None for anything less sure.
pub(super) fn poll_condition(rest: &str) -> Option<String> {
    if rest.contains('\n') {
        return None;
    }
    let plain = |t: &str| t.chars().all(|c| c.is_ascii_alphanumeric() || "/._-:%@=+,".contains(c));
    let tokens = || rest.split_whitespace().map(|t| t.trim_matches(['"', '\'', ';', '&']));
    if rest.starts_with("curl ") {
        let url = tokens().find(|t| (t.starts_with("http://") || t.starts_with("https://")) && plain(t))?;
        return Some(format!("curl -sf {url} >/dev/null"));
    }
    let path = tokens().find_map(|t| {
        let t = t.split_once('=').map_or(t, |(name, v)| {
            if name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
                v
            } else {
                t
            }
        });
        (t.starts_with('/') && t.len() > 1 && plain(t)).then_some(t)
    })?;
    Some(format!("[ -e {path} ]"))
}
