//! Shell-word helpers of the hint reader: variable assignments, `sh -c` scripts and wrapper programs.
/// The program a command word names, without its folder or `.exe`, in lower case.
pub fn program(word: &str) -> String {
    let name = word.rsplit(['/', '\\']).next().unwrap_or(word).to_ascii_lowercase();
    name.strip_suffix(".exe").map(str::to_string).unwrap_or(name)
}

pub(super) use nexo_core::shell::is_assignment;

/// The script of `sh -c '...'` (also `-lc`, `-ec`).
pub(super) fn shell_script(words: &[String]) -> Option<String> {
    let at = words
        .iter()
        .skip(1)
        .position(|a| a.starts_with('-') && !a.starts_with("--") && a.ends_with('c') && a.len() <= 4)?;
    words.get(at + 2).cloned()
}

/// The command a line of wrappers runs: `env A=1`, `sudo -u x`, `time`, `nohup`, `exec`, `nice -n 5`,
/// `timeout -s KILL 10`, `stdbuf -oL`.
pub(super) fn strip_wrappers(mut w: Vec<String>) -> Vec<String> {
    loop {
        let Some(first) = w.first() else { return w };
        let rest = &w[1..];
        let opts = |valued: &[&str]| {
            let mut n = 0;
            while let Some(o) = rest.get(n).filter(|o| o.starts_with('-') && o.len() > 1) {
                n += 1 + valued.contains(&o.as_str()) as usize;
            }
            // a valued option at the end of the line (`sudo -u`) has no value to skip
            n.min(rest.len())
        };
        let skip = match program(first).as_str() {
            "env" => rest.iter().take_while(|a| a.starts_with('-') || is_assignment(a)).count(),
            "sudo" | "doas" => opts(&["-u", "-g", "-h", "-p", "-C", "-D", "-R", "-T", "-U"]),
            "time" | "nohup" | "exec" | "setsid" | "catchsegv" | "unbuffer" | "stdbuf" => opts(&[]),
            "nice" => opts(&["-n"]),
            "timeout" => {
                let n = opts(&["-s", "-k", "--signal", "--kill-after"]);
                let duration = |d: &String| {
                    !d.is_empty() && d.chars().all(|c| c.is_ascii_digit() || c == '.' || "smhd".contains(c))
                };
                n + rest.get(n).is_some_and(duration) as usize
            }
            _ => return w,
        };
        w.drain(..(1 + skip).min(w.len()));
        if w.first().is_some_and(|x| is_assignment(x)) {
            w.remove(0);
        }
    }
}
