//! The shell-word reading the rules lean on: simple commands, words, wrappers, code-running forms.

/// The simple commands of a shell line, split on `;`, `&&`, `||`, `|`, `&`, newlines and parentheses
/// outside quotes, and whether it is more than the parser can vouch for: a substitution (`$(..)`,
/// backticks, `<(..)`) that runs something else, an ANSI-C string (`$'..'`), a `${..}` with an `@`
/// operator (`${x@P}` runs what it expands), or a redirection that writes a file other than
/// `/dev/null`. With `subst` the substitutions are split out as commands too (for deny rules).
pub(super) fn simple_commands(cmd: &str, subst: bool) -> (Vec<String>, bool) {
    let chars: Vec<char> = cmd.chars().collect();
    let (mut parts, mut cur, mut complex) = (Vec::new(), String::new(), false);
    let (mut single, mut double, mut ansi) = (false, false, false);
    let mut i = 0;
    let cut = |cur: &mut String, parts: &mut Vec<String>| {
        let t = cur.trim();
        if !t.is_empty() {
            parts.push(t.to_string());
        }
        cur.clear();
    };
    while i < chars.len() {
        let c = chars[i];
        let next = chars.get(i + 1).copied();
        if single {
            single = c != '\'';
            cur.push(c);
        } else if ansi {
            // inside `$'..'` a backslash escapes the next character, `\'` included
            cur.push(c);
            if c == '\\' {
                if let Some(n) = next {
                    cur.push(n);
                    i += 1;
                }
            } else {
                ansi = c != '\'';
            }
        } else if !double && c == '$' && next == Some('\'') {
            ansi = true;
            complex = true;
            cur.push_str("$'");
            i += 1;
        } else if c == '$' && next == Some('{') {
            complex |= braced_has_at(&chars, i + 2);
            cur.push(c);
        } else if c == '\\' {
            cur.push(c);
            if let Some(n) = next {
                cur.push(n);
                i += 1;
            }
        } else if c == '`'
            || (c == '$' && next == Some('('))
            || (!double && matches!(c, '<' | '>') && next == Some('('))
        {
            complex = true;
            if subst {
                cut(&mut cur, &mut parts);
                i += usize::from(c != '`');
            } else {
                cur.push(c);
            }
        } else if double {
            double = c != '"';
            cur.push(c);
        } else if c == '\'' {
            single = true;
            cur.push(c);
        } else if c == '"' {
            double = true;
            cur.push(c);
        } else if c == '>' || (c == '<' && next == Some('>')) {
            complex |= !harmless_redirect(&chars, i);
            cur.push(c);
            if c == '>' && next == Some('|') {
                // `>|` is one operator, not a pipe
                cur.push('|');
                i += 1;
            }
        } else if matches!(c, ';' | '|' | '\n' | '(' | ')') {
            cut(&mut cur, &mut parts);
        } else if c == '&' {
            // `2>&1` and `&>file` are redirections
            let prev = i.checked_sub(1).map(|p| chars[p]);
            if matches!(prev, Some('>') | Some('<')) || next == Some('>') {
                cur.push(c);
            } else {
                cut(&mut cur, &mut parts);
            }
        } else {
            cur.push(c);
        }
        i += 1;
    }
    cut(&mut cur, &mut parts);
    (parts, complex)
}

/// Whether the `${` whose inside starts at `from` holds an `@` operator (`${x@P}`, `${a[@]}`);
/// unclosed counts as holding one.
pub(super) fn braced_has_at(chars: &[char], from: usize) -> bool {
    let mut depth = 1;
    for &c in chars.iter().skip(from) {
        match c {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return false;
                }
            }
            '@' => return true,
            _ => {}
        }
    }
    true
}

/// The redirection at `at` (`>`, `>>`, `>|`, `>&`, `<>`) only duplicates a descriptor (`2>&1`)
/// or writes to `/dev/null`.
pub(super) fn harmless_redirect(chars: &[char], at: usize) -> bool {
    if chars[at] == '<' {
        return false;
    }
    let mut j = at + 1;
    if matches!(chars.get(j), Some('>' | '|')) {
        j += 1;
    }
    let dup = chars.get(j) == Some(&'&');
    j += usize::from(dup);
    while chars.get(j).is_some_and(|c| *c == ' ' || *c == '\t') {
        j += 1;
    }
    let target: String =
        chars[j.min(chars.len())..].iter().take_while(|c| !c.is_whitespace() && !";|&<>()".contains(**c)).collect();
    target == "/dev/null"
        || (dup && (target == "-" || (!target.is_empty() && target.chars().all(|c| c.is_ascii_digit()))))
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

/// Decodes the escape of `$'..'` whose letter is at `at` into `out`; the index after it.
pub(super) fn ansi_escape(chars: &[char], at: usize, out: &mut String) -> usize {
    let radix_run = |from: usize, radix: u32, max: usize| {
        let digits: String = chars[from..].iter().take(max).take_while(|c| c.is_digit(radix)).collect();
        let n = digits.len();
        (u32::from_str_radix(&digits, radix).ok().and_then(char::from_u32), n)
    };
    let simple = match chars[at] {
        'n' => Some('\n'),
        't' => Some('\t'),
        'r' => Some('\r'),
        'a' => Some('\x07'),
        'b' => Some('\x08'),
        'e' | 'E' => Some('\x1b'),
        'f' => Some('\x0c'),
        'v' => Some('\x0b'),
        '\\' | '\'' | '"' | '?' => Some(chars[at]),
        _ => None,
    };
    if let Some(c) = simple {
        out.push(c);
        return at + 1;
    }
    let (decoded, used) = match chars[at] {
        'x' => radix_run(at + 1, 16, 2),
        'u' => radix_run(at + 1, 16, 4),
        'U' => radix_run(at + 1, 16, 8),
        '0'..='7' => {
            let (c, n) = radix_run(at, 8, 3);
            (c, n.saturating_sub(1))
        }
        'c' if at + 1 < chars.len() => (char::from_u32(chars[at + 1] as u32 & 0x1f), 1),
        _ => (None, 0),
    };
    match decoded {
        Some(c) if used > 0 || chars[at].is_digit(8) => {
            out.push(c);
            at + 1 + used
        }
        _ => {
            out.push('\\');
            out.push(chars[at]);
            at + 1
        }
    }
}

/// `rm` of `/bin/rm`.
pub(super) fn basename(w: &str) -> &str {
    w.rsplit('/').next().unwrap_or(w)
}

/// `NAME=value`, a variable set for the command that follows.
pub(super) fn is_assignment(w: &str) -> bool {
    w.split_once('=').is_some_and(|(n, _)| {
        n.chars().next().is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
            && n.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
    })
}

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
