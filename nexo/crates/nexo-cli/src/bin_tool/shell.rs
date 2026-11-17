use super::clap_facts::op_flags;

/// One op of a call, or a shell part that is not run (with why).
pub(super) struct Seg {
    pub(super) text: String,
    pub(super) skip: Option<String>,
}

pub(super) struct Scan {
    pub(super) segs: Vec<Seg>,
    /// Whether any shell syntax was found.
    pub(super) found: bool,
    /// The fact, once per call.
    pub(super) note: Option<String>,
}

/// The detection is nexo-dbg's (`shell_pieces`: the lowest crate `debug` and `bin` share, `nexo-cli` already
/// depends on it): each unit is cut at `&&`/`||`, a pipeline and redirections come off, and a part that starts with a
/// shell word, an assignment or holds `$var` is not run.
pub(super) fn scan_shell(units: &[String]) -> Scan {
    let mut segs: Vec<Seg> = Vec::new();
    let (mut items, mut vars): (Vec<String>, Vec<String>) = (Vec::new(), Vec::new());
    let (mut sep, mut found) = (None::<String>, false);
    let mut pipe_note: Option<String> = None;
    let add = |v: &mut Vec<String>, s: String| {
        if !v.contains(&s) {
            v.push(s)
        }
    };
    for u in units {
        for p in nexo_dbg::shell_pieces(u, true) {
            let mut first = p.first().to_string();
            let mut words = p.text.split_whitespace().skip(1).collect::<Vec<_>>();
            // `nexo bin decomp ...` is the same op
            while matches!(first.as_str(), "nexo" | "bin") && !words.is_empty() {
                first = words.remove(0).to_string();
            }
            if let Some(j) = p.joiner {
                found = true;
                add(&mut items, format!("`{j}`"));
            }
            for r in &p.redirects {
                found = true;
                add(&mut items, format!("`{}`", r.op));
            }
            if let Some(f) = &p.pipe {
                found = true;
                add(&mut items, "`|`".into());
                pipe_note.get_or_insert_with(|| pipe_fact(&first, f));
            }
            let skip = if nexo_dbg::assignment(&first).is_some() {
                add(&mut vars, format!("`{}`", nexo_dbg::assignment(&first).unwrap_or_default()));
                Some("an assignment, nothing is expanded here".to_string())
            } else if let Some(v) = nexo_dbg::variable_in(&p.text) {
                add(&mut vars, format!("`{v}`"));
                Some(format!("`{v}` is not expanded, write the path"))
            } else if nexo_dbg::is_shell_word(&first) {
                let separator = matches!(first.to_ascii_lowercase().as_str(), "echo" | "printf")
                    && words
                        .iter()
                        .all(|w| w.trim_matches(|c| c == '"' || c == '\'').chars().all(|c| "-=#_~*".contains(c)));
                if separator {
                    sep.get_or_insert_with(|| p.text.clone());
                } else {
                    add(&mut items, format!("`{first}`"));
                }
                Some("a shell command".to_string())
            } else {
                None
            };
            if skip.is_some() {
                found = true;
            }
            segs.push(Seg { text: p.text, skip });
        }
    }
    if !found {
        // no shell syntax: the units as they are (their own quoting, comments, `;` inside notes)
        return Scan {
            segs: units.iter().map(|u| Seg { text: u.clone(), skip: None }).collect(),
            found: false,
            note: None,
        };
    }
    let mut note: Vec<String> = Vec::new();
    if !items.is_empty() {
        let list = match items.as_slice() {
            [one] => format!("{one} is shell"),
            [head @ .., last] => format!("{} and {last} are shell", head.join(", ")),
            [] => String::new(),
        };
        note.push(format!("{list}; bin runs no shell, one op per `;`, no pipes, redirects or `&&`"));
    }
    if let Some(p) = pipe_note {
        note.push(p);
    }
    if let Some(t) = sep {
        note.push(format!("results already come one per op, so a separator (`{t}`) is not needed"));
    }
    if !vars.is_empty() {
        note.push(format!("{} not expanded: write the path in each op", vars.join(", ")));
    }
    Scan { segs, found: true, note: Some(format!("note: {}", note.join("; "))) }
}

/// What does the job of `| head` and `| grep` here: the flags the op really has.
pub(super) fn pipe_fact(op: &str, filter: &str) -> String {
    let flags = op_flags(op).unwrap_or_default();
    let has = |f: &str| flags.split(", ").any(|x| x == f);
    let word = filter.split_whitespace().next().unwrap_or("");
    let mut use_: Vec<&str> = Vec::new();
    if (matches!(word, "head" | "tail" | "wc" | "less" | "more") || !matches!(word, "grep" | "egrep")) && has("--lines")
    {
        use_.push("`--lines N`");
    }
    if (matches!(word, "grep" | "egrep" | "awk" | "sed") || use_.is_empty()) && has("--grep") {
        use_.push("`--grep RE`");
    }
    let ask = if use_.is_empty() {
        String::new()
    } else {
        format!("; use {} after the op instead of `| {word}`", use_.join(" or "))
    };
    format!("output is already bounded{ask}")
}
