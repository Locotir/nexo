//! A small shell command splitter: the segments and words of a line, and the program each one runs.

#[derive(Debug)]
pub(super) struct Seg {
    pub(super) words: Vec<String>,
    /// Its output goes into the next segment.
    pub(super) piped: bool,
}

/// The words of a command, split at `;`, `&`, `&&`, `||`, `|` and new lines, quotes removed. Not a shell, but
/// enough to see which programs a line runs.
pub(super) fn segments(cmd: &str) -> Vec<Seg> {
    let mut segs = Vec::new();
    let mut words: Vec<String> = Vec::new();
    let mut cur = String::new();
    let mut has = false;
    let mut quote: Option<char> = None;
    let mut it = cmd.chars().peekable();
    let flush = |cur: &mut String, has: &mut bool, words: &mut Vec<String>| {
        if *has {
            words.push(std::mem::take(cur));
            *has = false;
        }
    };
    while let Some(c) = it.next() {
        match (quote, c) {
            (Some(q), c) if c == q => quote = None,
            (Some('"'), '\\') => {
                if let Some(n) = it.next() {
                    cur.push(n);
                }
            }
            (Some(_), c) => cur.push(c),
            (None, '\'' | '"') => {
                quote = Some(c);
                has = true;
            }
            (None, '\\') => {
                if let Some(n) = it.next() {
                    cur.push(n);
                    has = true;
                }
            }
            (None, ' ' | '\t') => flush(&mut cur, &mut has, &mut words),
            (None, ';' | '&' | '|' | '\n') => {
                flush(&mut cur, &mut has, &mut words);
                let mut piped = false;
                if c == '|' && it.peek() != Some(&'|') {
                    piped = true;
                }
                if matches!(c, '&' | '|') && it.peek() == Some(&c) {
                    it.next();
                }
                if !words.is_empty() {
                    segs.push(Seg { words: std::mem::take(&mut words), piped });
                }
            }
            (None, c) => {
                cur.push(c);
                has = true;
            }
        }
    }
    flush(&mut cur, &mut has, &mut words);
    if !words.is_empty() {
        segs.push(Seg { words, piped: false });
    }
    segs
}

pub(super) fn words(cmd: &str) -> Vec<String> {
    segments(cmd).into_iter().flat_map(|s| s.words).collect()
}

/// A line that runs more than one thing, or something the splitter cannot see into: never approved by a pattern.
pub(super) fn compound(cmd: &str) -> bool {
    cmd.contains(['\n', ';', '&', '|', '`', '>', '<']) || cmd.contains("$(")
}

/// The program of a segment and its arguments, without the variable assignments and wrappers in front.
pub(super) fn program(words: &[String]) -> (&str, &[String]) {
    let mut i = 0;
    while let Some(w) = words.get(i) {
        let assign =
            w.split_once('=').is_some_and(|(k, _)| !k.is_empty() && k.chars().all(|c| c.is_alphanumeric() || c == '_'));
        let name = w.rsplit('/').next().unwrap_or(w);
        if assign || matches!(w.as_str(), "sudo" | "env" | "nohup" | "time" | "exec" | "command" | "nice") {
            i += 1;
        } else if let Some(takes) = mac_wrapper(name) {
            // `caffeinate -i rm`, `arch -arm64 rm`, `xcrun --sdk macosx rm`: the options, then the program they run
            i += 1;
            while let Some(o) = words.get(i).filter(|o| o.starts_with('-')) {
                i += if takes.contains(&o.as_str()) { 2 } else { 1 };
            }
        } else {
            break;
        }
    }
    match words.get(i) {
        Some(w) => (w.rsplit('/').next().unwrap_or(w), &words[i + 1..]),
        None => ("", &[]),
    }
}

/// The macOS programs that run the command after their options, and the options that take a value.
fn mac_wrapper(name: &str) -> Option<&'static [&'static str]> {
    match name {
        "caffeinate" => Some(&["-t", "-w"]),
        "arch" => Some(&["-arch", "-e", "-d"]),
        "xcrun" => Some(&["--sdk", "-sdk", "--toolchain", "-toolchain"]),
        "sandbox-exec" => Some(&["-f", "-p", "-n", "-D"]),
        _ => None,
    }
}
