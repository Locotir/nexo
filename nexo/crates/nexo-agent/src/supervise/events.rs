//! The event ribbon: a short, typed history of what a session did, shared by every source. Sources keep
//! reporting the observations they always did; [`derive`] turns each into at most one [`Ev`], so no source
//! reports a thing twice. A source adds an `ObsKind::Ev` only for what the older kinds cannot say (test
//! results, commits, compactions, retries, the todo list).

use super::{ObsKind, Secs};

/// What happened, in the panel's own words.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum EvKind {
    SessionStart,
    SessionEnd,
    TurnStart,
    TurnEnd,
    Thinking,
    Read,
    Search,
    Edit,
    Run,
    Web,
    Tool,
    PermissionWait,
    PermissionDone,
    Question,
    Answer,
    SubSpawn,
    SubDone,
    MsgOut,
    MsgIn,
    MsgRead,
    Compact,
    Error,
    Retry,
    RateLimit,
    FileWritten,
    TestsPass,
    TestsFail,
    GitCommit,
    GitPush,
    Todo,
    JobStart,
    JobDone,
    Idle,
    Stall,
    Loop,
    ModelSwitch,
    Cost,
}

/// What an event carries besides its kind.
#[derive(Clone, Debug, PartialEq, Default)]
pub enum EvDetail {
    #[default]
    None,
    Progress {
        done: u16,
        total: u16,
    },
    Tests {
        pass: u32,
        fail: u32,
    },
    Usd(f64),
    /// Tokens produced by one model call.
    Tokens(u32),
    /// The other session of a message.
    Peer(String),
    /// A few redacted words (at most 40 characters).
    Text(String),
    Wait {
        secs: u32,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub struct Ev {
    pub at: Secs,
    pub kind: EvKind,
    /// Repeats of the same kind within one second, this one included.
    pub n: u32,
    pub detail: EvDetail,
}

impl Ev {
    pub fn new(at: Secs, kind: EvKind) -> Ev {
        Ev { at, kind, n: 1, detail: EvDetail::None }
    }

    pub fn with(at: Secs, kind: EvKind, detail: EvDetail) -> Ev {
        Ev { at, kind, n: 1, detail }
    }
}

/// How many events a track keeps.
pub const RIBBON_KEEP: usize = 32;

/// Push `ev`, folding it into the last one when it is the same kind within a second.
pub fn push(ribbon: &mut std::collections::VecDeque<Ev>, ev: Ev) {
    if let Some(last) = ribbon.back_mut() {
        if last.kind == ev.kind && (ev.at - last.at).abs() <= 1 && last.detail == ev.detail {
            last.n += 1;
            last.at = last.at.max(ev.at);
            return;
        }
    }
    ribbon.push_back(ev);
    while ribbon.len() > RIBBON_KEEP {
        ribbon.pop_front();
    }
}

/// The kind of event a tool call is, by its name (nexo's and other programs' names alike).
pub fn kind_of_tool(name: &str) -> EvKind {
    match name.to_ascii_lowercase().as_str() {
        "read" | "view" | "readfile" | "read_file" | "ls" | "list" | "notebookread" => EvKind::Read,
        "grep" | "glob" | "search" | "find" | "toolsearch" | "codesearch" => EvKind::Search,
        "edit" | "write" | "multiedit" | "notebookedit" | "apply_patch" | "patch" | "str_replace" => EvKind::Edit,
        "bash" | "exec" | "shell" | "run" | "powershell" | "bashoutput" | "killshell" => EvKind::Run,
        "webfetch" | "websearch" | "fetch" | "web_fetch" | "web_search" | "http" => EvKind::Web,
        _ => EvKind::Tool,
    }
}

/// Each old observation as the one event it stands for, when it stands for one.
pub fn derive(kind: &ObsKind, at: Secs) -> Option<Ev> {
    let ev = |k: EvKind| Some(Ev::new(at, k));
    match kind {
        ObsKind::UserInput => ev(EvKind::TurnStart),
        ObsKind::ToolPending { name } if name == "AskUserQuestion" => None,
        ObsKind::ToolPending { name } => ev(kind_of_tool(name)),
        ObsKind::TurnEnd { .. } => ev(EvKind::TurnEnd),
        ObsKind::AskPending => ev(EvKind::Question),
        ObsKind::AskResolved => ev(EvKind::Answer),
        ObsKind::PermissionPending { .. } => ev(EvKind::PermissionWait),
        ObsKind::PermissionResolved => ev(EvKind::PermissionDone),
        ObsKind::Loop { .. } => ev(EvKind::Loop),
        ObsKind::SystemError => ev(EvKind::Error),
        ObsKind::Done | ObsKind::Dead => ev(EvKind::SessionEnd),
        ObsKind::MsgOut { to, .. } => Some(Ev::with(at, EvKind::MsgOut, EvDetail::Peer(to.clone()))),
        ObsKind::MsgIn { from, .. } => Some(Ev::with(at, EvKind::MsgIn, EvDetail::Peer(from.clone()))),
        ObsKind::MsgStage { stage, .. } if *stage == nexo_core::mail::Stage::Read => ev(EvKind::MsgRead),
        ObsKind::Ev(e) => Some(Ev { at: if e.at == 0 { at } else { e.at }, ..e.clone() }),
        _ => None,
    }
}

/// What a shell command is, when it is one the ribbon marks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CmdKind {
    Tests,
    Commit,
    Push,
}

/// Whether `cmd` runs tests or makes a commit or a push. Looks at every command of a `&&` / `;` / `|` chain.
pub fn command_kind(cmd: &str) -> Option<CmdKind> {
    const TESTS: [&str; 9] =
        ["cargo test", "cargo nextest", "pytest", "npm test", "go test", "vitest", "jest", "ctest", "dotnet test"];
    for part in cmd.split(['\n', ';', '&', '|']) {
        let p = part.trim();
        if p.starts_with("git commit") || p.contains(" git commit") {
            return Some(CmdKind::Commit);
        }
        if p.starts_with("git push") || p.contains(" git push") {
            return Some(CmdKind::Push);
        }
        let words = p.split_whitespace().collect::<Vec<_>>().join(" ");
        if TESTS.iter().any(|t| words.starts_with(t) || words.contains(&format!(" {t}")))
            || words.starts_with("yarn test")
            || words.starts_with("pnpm test")
        {
            return Some(CmdKind::Tests);
        }
    }
    None
}

/// `N passed` and `N failed` counts from the output of a test run; the last of each wins.
pub fn test_counts(text: &str) -> Option<(u32, u32)> {
    let (mut pass, mut fail, mut seen) = (0u32, 0u32, false);
    let words: Vec<&str> =
        text.split(|c: char| c.is_whitespace() || c == ',' || c == ';').filter(|w| !w.is_empty()).collect();
    for w in words.windows(2) {
        let Ok(n) = w[0].parse::<u32>() else { continue };
        match w[1].trim_end_matches(['.', ';', ',']) {
            "passed" => {
                pass = pass.max(n);
                seen = true;
            }
            "failed" | "failures" => {
                fail = fail.max(n);
                seen = true;
            }
            _ => {}
        }
    }
    seen.then_some((pass, fail))
}

/// The events a finished shell command stands for: tests with their outcome, a commit, a push.
pub fn after_command(cmd: &str, output: &str, is_error: bool, at: Secs) -> Option<Ev> {
    // a command that ended with a non-zero code is not an error result, but its tests did not pass
    let is_error = is_error || nexo_core::shell::exit_code_in(output).is_some_and(|c| c != 0);
    let kind = command_kind(cmd)?;
    Some(match kind {
        CmdKind::Tests => {
            let (pass, fail) = test_counts(output).unwrap_or((0, u32::from(is_error)));
            let failed = is_error || fail > 0;
            Ev::with(at, if failed { EvKind::TestsFail } else { EvKind::TestsPass }, EvDetail::Tests { pass, fail })
        }
        CmdKind::Commit if !is_error => Ev::new(at, EvKind::GitCommit),
        CmdKind::Push if !is_error => Ev::new(at, EvKind::GitPush),
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repeats_within_a_second_fold_into_one() {
        let mut r = std::collections::VecDeque::new();
        push(&mut r, Ev::new(10, EvKind::Read));
        push(&mut r, Ev::new(10, EvKind::Read));
        push(&mut r, Ev::new(11, EvKind::Read));
        push(&mut r, Ev::new(14, EvKind::Read));
        push(&mut r, Ev::new(14, EvKind::Edit));
        let v: Vec<_> = r.iter().map(|e| (e.kind, e.n)).collect();
        assert_eq!(v, vec![(EvKind::Read, 3), (EvKind::Read, 1), (EvKind::Edit, 1)]);
        for i in 0..100 {
            push(&mut r, Ev::new(100 + i * 5, EvKind::Run));
        }
        assert_eq!(r.len(), RIBBON_KEEP);
    }

    #[test]
    fn tools_map_to_kinds() {
        assert_eq!(kind_of_tool("Grep"), EvKind::Search);
        assert_eq!(kind_of_tool("MultiEdit"), EvKind::Edit);
        assert_eq!(kind_of_tool("bash"), EvKind::Run);
        assert_eq!(kind_of_tool("WebFetch"), EvKind::Web);
        assert_eq!(kind_of_tool("mcp__x__y"), EvKind::Tool);
    }

    #[test]
    fn commands_and_counts() {
        assert_eq!(command_kind("cd x && cargo test -p a"), Some(CmdKind::Tests));
        assert_eq!(command_kind("npm test"), Some(CmdKind::Tests));
        assert_eq!(command_kind("git commit -m x"), Some(CmdKind::Commit));
        assert_eq!(command_kind("git add . && git push origin main"), Some(CmdKind::Push));
        assert_eq!(command_kind("ls -la"), None);
        assert_eq!(test_counts("test result: ok. 12 passed; 0 failed; 0 ignored"), Some((12, 0)));
        assert_eq!(test_counts("== 3 failed, 40 passed in 2s =="), Some((40, 3)));
        assert_eq!(test_counts("nothing"), None);
        let e = after_command("cargo test", "12 passed; 0 failed", false, 5).unwrap();
        assert_eq!((e.kind, e.detail), (EvKind::TestsPass, EvDetail::Tests { pass: 12, fail: 0 }));
        let e = after_command("pytest", "boom", true, 5).unwrap();
        assert_eq!(e.kind, EvKind::TestsFail);
        // a non-zero exit is no error result, but the tests did not pass
        let e = after_command("pytest", "boom\n\n[exit code 1]", false, 5).unwrap();
        assert_eq!(e.kind, EvKind::TestsFail);
        assert!(after_command("git commit -m x", "", true, 5).is_none());
    }
}
