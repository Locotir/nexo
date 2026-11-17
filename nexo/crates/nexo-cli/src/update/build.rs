use super::*;

/// The `cargo install` arguments before `--root <stage>`. With the release `tag` the status check reported
/// (only a plain one, so nothing odd reaches cargo) the build is of that release, not of whatever the default
/// branch holds now, which may be ahead of it or never have passed the checks a release waits for. Without a
/// tag (GitHub could not be asked) it builds the default branch, so the fallback still works.
pub(super) fn install_args(tag: Option<&str>) -> Vec<String> {
    let mut a: Vec<String> = ["install", "--locked", "--git", REPO, "nexo-cli"].map(String::from).into();
    if let Some(t) = tag.filter(|t| plain_tag(t)) {
        // one argument, so a tag can never be read as a separate option
        a.push(format!("--tag={t}"));
    }
    a.push("--root".to_string());
    a
}

/// Runs `cargo install` from the repository into `stage`, keeping cargo's output out of the way: one live
/// line shows the crate it is on. Its messages are shown only if it fails. Gives cargo's exit status and
/// the commit it built (when it said which). `tag` is the release to build, when it is known.
pub(super) fn build(
    ui: &Ui,
    stage: &Path,
    tag: Option<&str>,
) -> std::io::Result<(std::process::ExitStatus, Option<String>)> {
    use std::io::BufRead;
    use std::process::Stdio;
    use std::sync::{Arc, Mutex};
    let mut child =
        std::process::Command::new("cargo").args(install_args(tag)).arg(stage).stderr(Stdio::piped()).spawn()?;
    let seen = Arc::new(Mutex::new(Seen::default()));
    let reader = child.stderr.take().map(|err| {
        let seen = Arc::clone(&seen);
        std::thread::spawn(move || {
            let mut err = std::io::BufReader::new(err);
            let mut raw = Vec::new();
            // bytes, not `lines()`: a line that is not UTF-8 must not stop the reading and fill the pipe
            while err.read_until(b'\n', &mut raw).is_ok_and(|n| n > 0) {
                seen.lock().unwrap_or_else(|e| e.into_inner()).take(&String::from_utf8_lossy(&raw));
                raw.clear();
            }
        })
    });
    let status = loop {
        if let Some(s) = child.try_wait()? {
            break s;
        }
        ui.detail(&seen.lock().unwrap_or_else(|e| e.into_inner()).krate);
        std::thread::sleep(Duration::from_millis(100));
    };
    if let Some(r) = reader {
        let _ = r.join();
    }
    let seen = seen.lock().unwrap_or_else(|e| e.into_inner());
    if !status.success() {
        ui.abort();
        for l in &seen.tail {
            eprintln!("{l}");
        }
    }
    Ok((status, seen.rev.clone()))
}

/// What is read from cargo's messages: the crate it is on, the commit it builds, and the last lines (shown on failure).
#[derive(Default)]
pub(super) struct Seen {
    krate: String,
    rev: Option<String>,
    tail: std::collections::VecDeque<String>,
}

impl Seen {
    const TAIL: usize = 60;

    fn take(&mut self, line: &str) {
        let line = line.trim_end();
        if let Some(k) = compiling(line) {
            self.krate = k.to_string();
        }
        if let Some(r) = git_rev(line) {
            self.rev = Some(r.to_string());
        }
        if self.tail.len() == Self::TAIL {
            self.tail.pop_front();
        }
        self.tail.push_back(line.to_string());
    }
}

/// The crate in a cargo `Compiling <name> v<version> ...` line.
pub(super) fn compiling(line: &str) -> Option<&str> {
    line.trim_start().strip_prefix("Compiling ")?.split_whitespace().next()
}

/// The short commit in a cargo line that names the repository (`... (https://github.com/Locotir/nexo#4b4f255a)`).
pub(super) fn git_rev(line: &str) -> Option<&str> {
    let rest = &line[line.find(REPO)? + REPO.len()..];
    let hex = rest.strip_prefix('#')?;
    let end = hex.find(|c: char| !c.is_ascii_hexdigit()).unwrap_or(hex.len());
    (end >= 7).then(|| &hex[..7])
}
