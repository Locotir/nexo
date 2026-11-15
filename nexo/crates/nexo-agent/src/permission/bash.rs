//! The shell-word reading the rules lean on: simple commands, words, wrappers, code-running forms.

/// The simple commands of a shell line, split on `;`, `&&`, `||`, `|`, `&`, newlines and parentheses
/// outside quotes, and whether it is more than the parser can vouch for: a substitution (`$(..)`,
/// backticks, `<(..)`) that runs something else, an ANSI-C string (`$'..'`), a `${..}` with an `@`
/// operator (`${x@P}` runs what it expands), or a redirection that writes a file other than
/// `/dev/null`. With `subst` the substitutions are split out as commands too (for deny rules).
pub(super) fn simple_commands(cmd: &str, subst: bool) -> (Vec<String>, bool) {
    use nexo_core::shell::{lex, End, Redirect};
    let lx = lex(cmd, false);
    let mut complex = !lx.substs.is_empty() || lx.ansi || lx.braced_at;
    let mut parts: Vec<String> = Vec::new();
    let push = |parts: &mut Vec<String>, text: String| {
        let t = text.trim();
        if !t.is_empty() {
            parts.push(t.to_string());
        }
    };
    let harmless = |r: &Redirect| -> bool {
        if r.op.starts_with('>') || r.op.starts_with('&') {
            r.raw_target == "/dev/null" || r.dup
        } else {
            r.op != "<>"
        }
    };
    let mut from: Option<usize> = None;
    let mut s = 0;
    let mut bodies: Vec<(String, String)> = Vec::new();
    for c in &lx.cmds {
        for r in &c.redirects {
            complex |= !harmless(r);
        }
        if let Some(h) = &c.heredoc {
            if let Some(b) = &h.body {
                bodies.push((b.clone(), h.delim.clone()));
            }
        }
        let start = from.take().unwrap_or(c.span.start);
        if c.end == End::Subst {
            let sub = lx.substs[s];
            s += 1;
            if subst {
                push(&mut parts, lx.text(start..c.span.end));
            } else if sub.backtick || sub.quoted {
                from = Some(start);
                continue;
            } else {
                push(&mut parts, lx.text(start..c.span.end + 1));
            }
        } else {
            push(&mut parts, lx.text(start..c.span.end));
        }
        if c.end == End::Newline {
            for (b, d) in bodies.drain(..) {
                let (p, cx) = simple_commands(&b, subst);
                parts.extend(p);
                complex |= cx;
                parts.push(d);
            }
        }
    }
    (parts, complex)
}

/// The words of a simple command as the program receives them: quotes and backslashes removed,
/// `$'..'` decoded, redirections (`2>/dev/null`, `> out`) left out.
pub(super) fn words(s: &str) -> Vec<String> {
    let chars: Vec<char> = s.chars().collect();
    let (mut out, mut cur, mut quoted) = (Vec::new(), String::new(), false);
    let mut i = 0;
    let end_word = |out: &mut Vec<String>, cur: &mut String, quoted: &mut bool| {
        if *quoted || !cur.is_empty() {
            out.push(std::mem::take(cur));
        }
        *quoted = false;
    };
    while i < chars.len() {
        let c = chars[i];
        let next = chars.get(i + 1).copied();
        match c {
            '\'' => {
                quoted = true;
                i += 1;
                while i < chars.len() && chars[i] != '\'' {
                    cur.push(chars[i]);
                    i += 1;
                }
            }
            '"' => {
                quoted = true;
                i += 1;
                while i < chars.len() && chars[i] != '"' {
                    if chars[i] == '\\' && matches!(chars.get(i + 1), Some('"' | '\\' | '$' | '`' | '\n')) {
                        i += 1;
                    }
                    cur.push(chars[i]);
                    i += 1;
                }
            }
            '$' if next == Some('\'') => {
                quoted = true;
                i += 2;
                while i < chars.len() && chars[i] != '\'' {
                    if chars[i] == '\\' && i + 1 < chars.len() {
                        i = ansi_escape(&chars, i + 1, &mut cur);
                    } else {
                        cur.push(chars[i]);
                        i += 1;
                    }
                }
            }
            '\\' => {
                if let Some(n) = next {
                    if n != '\n' {
                        cur.push(n);
                    }
                    i += 1;
                }
            }
            '<' | '>' => {
                // a redirection: its descriptor number, the operator and its target are no words
                if quoted || !cur.chars().all(|d| d.is_ascii_digit()) {
                    end_word(&mut out, &mut cur, &mut quoted);
                }
                cur.clear();
                while i < chars.len() && matches!(chars[i], '<' | '>' | '&' | '|') {
                    i += 1;
                }
                while i < chars.len() && chars[i].is_whitespace() {
                    i += 1;
                }
                while i < chars.len() && !chars[i].is_whitespace() {
                    if let Some(q @ ('\'' | '"')) = chars.get(i).copied() {
                        i += 1;
                        while i < chars.len() && chars[i] != q {
                            i += 1;
                        }
                    }
                    i += 1;
                }
                continue;
            }
            '&' if next == Some('>') => {
                end_word(&mut out, &mut cur, &mut quoted);
                i += 1;
                continue;
            }
            c if c.is_whitespace() => end_word(&mut out, &mut cur, &mut quoted),
            c => cur.push(c),
        }
        i += 1;
    }
    end_word(&mut out, &mut cur, &mut quoted);
    out
}

pub(super) use nexo_core::shell::ansi_escape;

/// `rm` of `/bin/rm`.
pub(super) fn basename(w: &str) -> &str {
    w.rsplit('/').next().unwrap_or(w)
}

pub(super) use nexo_core::shell::is_assignment;

/// The simple commands of `cmd` for deny rules: each as written, and as the program it runs sees it
/// (wrappers like `sudo`, `env` and `nice` skipped, `/bin/rm` as `rm`), and the scripts handed to a
/// shell (`bash -c '..'`, `eval`, `env -S`) taken apart the same way.
pub(super) fn deny_forms(cmd: &str, depth: usize, out: &mut Vec<String>) {
    let (parts, _) = simple_commands(cmd, true);
    for p in parts {
        let (ws, scripts) = unwrap_command(words(&p));
        out.push(p);
        if !ws.is_empty() {
            out.push(ws.join(" "));
        }
        if depth < 4 {
            for s in scripts {
                deny_forms(&s, depth + 1, out);
            }
        }
    }
}

/// The command under its wrappers, its program as a bare name, and the scripts it hands a shell.
pub(super) fn unwrap_command(ws: Vec<String>) -> (Vec<String>, Vec<String>) {
    let mut scripts = Vec::new();
    let mut i = 0;
    loop {
        while ws.get(i).is_some_and(|w| is_assignment(w)) {
            i += 1;
        }
        let Some(w) = ws.get(i) else { break };
        let name = basename(w);
        // options of each wrapper that take a value
        let takes: &[&str] = match name {
            "command" | "builtin" | "nohup" | "time" => &[],
            "exec" => &["-a"],
            "nice" => &["-n", "--adjustment"],
            "sudo" => &["-u", "-g", "-C", "-D", "-h", "-p", "-r", "-t", "-T", "-U", "--user", "--group", "--chdir"],
            "doas" => &["-u", "-C"],
            "env" => &["-u", "-C", "--unset", "--chdir"],
            _ => break,
        };
        let env = name == "env";
        i += 1;
        while let Some(o) = ws.get(i) {
            if !o.starts_with('-') {
                break;
            }
            i += 1;
            if o == "--" {
                break;
            }
            if env && (o == "-S" || o == "--split-string") {
                scripts.extend(ws.get(i).cloned());
                i += 1;
            } else if let Some(s) = o.strip_prefix("--split-string=").or(o.strip_prefix("-S")).filter(|_| env) {
                scripts.push(s.to_string());
            } else if takes.contains(&o.as_str()) {
                i += 1;
            }
        }
    }
    let mut ws = ws.into_iter().skip(i).collect::<Vec<_>>();
    if let Some(first) = ws.first_mut() {
        *first = basename(first).to_string();
    }
    match ws.first().map(String::as_str) {
        Some("sh" | "bash" | "zsh" | "dash" | "ksh" | "mksh" | "ash" | "fish") => {
            let mut c = false;
            for w in &ws[1..] {
                if w.starts_with('-') && !w.starts_with("--") {
                    c |= w.contains('c');
                } else if w.starts_with('-') || w.starts_with('+') {
                    continue;
                } else {
                    if c {
                        scripts.push(w.clone());
                    }
                    break;
                }
            }
        }
        Some("eval") => scripts.push(ws[1..].join(" ")),
        _ => {}
    }
    (ws, scripts)
}

/// A simple command whose options make its program run other code or delete what it finds, so an
/// allow rule for the program does not cover it: `git -c alias.x='!sh'`, `find -exec`, `xargs`,
/// `env -S`.
pub(super) fn runs_code(part: &str) -> bool {
    let ws = words(part);
    let mut it = ws.iter().map(String::as_str).skip_while(|w| is_assignment(w));
    let Some(prog) = it.next() else { return false };
    let rest: Vec<&str> = it.collect();
    match basename(prog) {
        "git" => {
            // options before the subcommand: `-c` and `--config-env` set config, which can name programs
            let mut global = rest.iter();
            while let Some(w) = global.next() {
                if !w.starts_with('-') {
                    break;
                }
                if w.starts_with("-c") || w.starts_with("--config-env") {
                    return true;
                }
                if matches!(*w, "-C" | "--git-dir" | "--work-tree" | "--namespace" | "--super-prefix") {
                    global.next();
                }
            }
            const RUNS: &[&str] = &["--exec-path", "--upload-pack", "--receive-pack", "--exec", "--extcmd"];
            rest.iter().any(|w| RUNS.contains(&w.split('=').next().unwrap_or(w)))
        }
        "find" => rest.iter().any(|w| {
            matches!(
                *w,
                "-exec" | "-execdir" | "-ok" | "-okdir" | "-delete" | "-fprint" | "-fprint0" | "-fprintf" | "-fls"
            )
        }),
        "xargs" => true,
        "env" => rest.iter().take_while(|w| w.starts_with('-') || is_assignment(w)).any(|w| {
            w.starts_with("--split-string") || (w.starts_with('-') && !w.starts_with("--") && w.contains('S'))
        }),
        _ => false,
    }
}
