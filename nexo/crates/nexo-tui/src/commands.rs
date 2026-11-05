//! Slash commands: the table the palette shows (names, aliases, hints) and
//! how a line is parsed.

use nexo_core::permission::PermissionMode;

/// One command: its primary name, the other names it answers to, the
/// argument hint and what it does. The order is the order of the palette
/// and decides ties.
#[derive(Debug, Clone, Copy)]
pub struct Spec {
    pub name: &'static str,
    pub aliases: &'static [&'static str],
    pub hint: &'static str,
    pub description: &'static str,
}

const fn spec(
    name: &'static str,
    aliases: &'static [&'static str],
    hint: &'static str,
    description: &'static str,
) -> Spec {
    Spec { name, aliases, hint, description }
}

pub const COMMANDS: &[Spec] = &[
    spec("help", &["h", "?"], "", "keys and commands"),
    spec("model", &["m"], "[name]", "switch model, or set one up step by step (checked against the server)"),
    spec("provider", &["providers"], "", "the same as /model"),
    spec("effort", &["reasoning", "think"], "[level|default]", "how much the model reasons, among the levels it takes"),
    spec("permissions", &["permission", "mode"], "[ask|accept-edits|plan|auto|read-only]", "set the permission mode"),
    spec("yolo", &[], "", "never ask for permission (the auto mode)"),
    spec("plan", &["planning"], "[text|off]", "plan mode: reads only, then a plan to approve; with text: sends it"),
    spec(
        "prompt",
        &["system"],
        "[edit]",
        "show the system prompt as the model receives it; `edit` opens it in $EDITOR",
    ),
    spec(
        "pricing",
        &["price", "prices"],
        "[in=… out=… cache=… write=…]",
        "what is known of the model; with values: its prices per million tokens",
    ),
    spec("mouse", &[], "", "mouse capture on or off (off: the terminal's own selection works)"),
    spec("timestamps", &[], "", "the HH:MM on each message and reply, and a reply's duration, on or off"),
    spec("goal", &["g"], "[text|off]", "keep working towards a goal across turns; `off` clears it"),
    spec("compact", &[], "", "replace the history with a summary"),
    spec("clear", &["new"], "", "start a new session here"),
    spec("resume", &["sessions"], "[session]", "pick a session to resume; with an id or a title: that one"),
    spec("fork", &[], "", "continue in a new session copied from an earlier point of this one"),
    spec("tree", &[], "", "the sessions as a tree of forks; jump to one"),
    spec("rewind", &["back"], "", "go back to before an earlier message, files included (also Esc Esc)"),
    spec("undo", &["u"], "", "undo the last request: its messages and the files it wrote"),
    spec(
        "shared-memory",
        &["memory-shared", "share"],
        "[id|off] [focus]",
        "bring what another session knows into this one, with your next message; `list` shows what waits",
    ),
    spec("undelete", &["restore"], "[id]", "bring back a deleted session (the last one by default)"),
    spec("init", &[], "", "have the agent study the project and write AGENTS.md"),
    spec("status", &["info"], "", "version, model, key, endpoint, session, configuration files"),
    spec("tools", &["mcp"], "", "the tools the model has, by kind"),
    spec("vim", &[], "", "the vim keys in the editor on or off"),
    spec(
        "budget",
        &["limit", "spend"],
        "[dollars|off]",
        "what this session and today cost; with a number: this session's limit",
    ),
    spec("jobs", &["bg"], "[kill [id]]", "the background jobs (commands and sub-agents); `kill` stops one, or all"),
    spec("export", &["save"], "[path]", "write the conversation as markdown"),
    spec("rename", &["title", "name"], "<name>", "give this session a name"),
    spec("diff", &[], "", "browse staged, unstaged and new files"),
    spec("panel", &[], "", "agent sessions by program, nexo first (s filters), read-only"),
    spec("cost", &["usage"], "", "window: tokens, cost, calls"),
    spec("context", &["ctx"], "", "window: what the next call sends"),
    spec("theme", &[], "[name]", "change the colours (a picker without a name)"),
    spec("keys", &["keybindings"], "", "the key bindings"),
    spec("copy", &[], "[code]", "copy the last reply to the clipboard; `code`: its last code block"),
    spec("context-live", &["live", "map"], "", "a live map of the context beside the transcript: what each call sends"),
    spec("quit", &["exit", "q"], "", "leave nexo"),
    spec("todo", &["tasks"], "", "the todo list, every item with its state"),
    spec("queue", &["queued"], "", "the requests waiting for the turn to end; pick one to edit it"),
];

#[derive(Debug, Clone, PartialEq)]
pub enum Command {
    Help,
    Model(Option<String>),
    Provider,
    Effort(Option<String>),
    Undo,
    Init,
    Status,
    Tools,
    Vim,
    Budget(Option<String>),
    /// `/jobs` lists, `/jobs kill` stops every one, `/jobs kill 3` stops job 3.
    Jobs(Option<Option<u32>>),
    /// The supervisor's panel: one card per agent session.
    Panel,
    Permissions(Option<PermissionMode>),
    Yolo,
    /// `/plan` turns plan mode on, `/plan text` also sends the text, `/plan off` leaves it.
    Plan(Option<String>),
    Prompt,
    PromptEdit,
    Pricing(Option<String>),
    Mouse,
    Timestamps,
    /// `/goal text` sets, `/goal` shows, `/goal off` clears.
    Goal(GoalArg),
    Compact,
    Clear,
    Sessions,
    /// `/resume <id or title>`: that session, without the picker.
    Resume(String),
    /// Bring a session back from the trash (None: the last one deleted here).
    Undelete(Option<String>),
    /// Share another session's memory with this one: `id [focus]`, `off`, or None to pick.
    Mem(Option<String>),
    /// `/todo`: the todo list.
    Todo,
    /// `/queue`: the requests waiting for the turn to end.
    Queue,
    Rewind,
    Fork,
    Tree,
    Export(Option<String>),
    Rename(Option<String>),
    Diff,
    Cost,
    Context,
    ContextLive,
    Theme(Option<String>),
    Keys,
    Copy(Option<String>),
    Quit,
    Unknown(String),
    /// `/permissions nonsense`
    BadArgument(String, String),
}

#[derive(Debug, Clone, PartialEq)]
pub enum GoalArg {
    Show,
    Set(String),
    Clear,
}

/// Names that work but are listed nowhere (not in /help, not in the palette): `(alias, command)`.
const HIDDEN: &[(&str, &str)] = &[("agents", "panel")];

/// The spec a name or alias stands for.
pub fn lookup(name: &str) -> Option<&'static Spec> {
    let name = HIDDEN.iter().find(|(a, _)| *a == name).map_or(name, |(_, n)| n);
    COMMANDS.iter().find(|s| s.name == name || s.aliases.contains(&name))
}

/// Parses a line that starts with `/`.
pub fn parse(line: &str) -> Option<Command> {
    let line = line.trim();
    let rest = line.strip_prefix('/')?;
    let mut it = rest.splitn(2, char::is_whitespace);
    let typed = it.next().unwrap_or("").trim().to_ascii_lowercase();
    let arg = it.next().map(|a| a.trim().to_string()).filter(|a| !a.is_empty());
    let Some(spec) = lookup(&typed) else { return Some(Command::Unknown(typed)) };
    Some(match spec.name {
        "help" => Command::Help,
        "model" => Command::Model(arg),
        "provider" => Command::Provider,
        "effort" => Command::Effort(arg),
        "undo" => Command::Undo,
        "init" => Command::Init,
        "status" => Command::Status,
        "tools" => Command::Tools,
        "vim" => Command::Vim,
        "budget" => Command::Budget(arg),
        "panel" => Command::Panel,
        "jobs" => match arg.as_deref().map(|a| a.split_whitespace().collect::<Vec<_>>()) {
            None => Command::Jobs(None),
            Some(w) if w.first() == Some(&"kill") || w.first() == Some(&"stop") => match w.get(1) {
                None => Command::Jobs(Some(None)),
                Some(n) => match n.trim_start_matches('#').parse::<u32>() {
                    Ok(id) => Command::Jobs(Some(Some(id))),
                    Err(_) => Command::BadArgument("jobs".into(), n.to_string()),
                },
            },
            Some(_) => Command::BadArgument("jobs".into(), arg.clone().unwrap_or_default()),
        },
        "permissions" => match arg {
            None => Command::Permissions(None),
            Some(a) => match PermissionMode::parse(&a) {
                Some(m) => Command::Permissions(Some(m)),
                None => Command::BadArgument("permissions".into(), a),
            },
        },
        "yolo" => Command::Yolo,
        "plan" => Command::Plan(arg),
        "prompt" => match arg.as_deref() {
            Some("edit") | Some("e") => Command::PromptEdit,
            Some(a) => Command::BadArgument("prompt".into(), a.to_string()),
            None => Command::Prompt,
        },
        "pricing" => Command::Pricing(arg),
        "mouse" => Command::Mouse,
        "timestamps" => Command::Timestamps,
        "goal" => Command::Goal(match arg.as_deref() {
            None => GoalArg::Show,
            Some("off") | Some("clear") | Some("none") | Some("done") => GoalArg::Clear,
            Some(t) => GoalArg::Set(t.to_string()),
        }),
        "compact" => Command::Compact,
        "clear" => Command::Clear,
        "resume" => match arg {
            Some(s) => Command::Resume(s),
            None => Command::Sessions,
        },
        "rewind" => Command::Rewind,
        "fork" => Command::Fork,
        "tree" => Command::Tree,
        "undelete" | "restore" => Command::Undelete(arg),
        "shared-memory" | "memory-shared" | "share" => Command::Mem(arg),
        "todo" => Command::Todo,
        "queue" => Command::Queue,
        "export" => Command::Export(arg),
        "rename" | "title" | "name" => Command::Rename(arg),
        "diff" => Command::Diff,
        "cost" => Command::Cost,
        "context" => Command::Context,
        "context-live" => Command::ContextLive,
        "theme" => Command::Theme(arg),
        "keys" => Command::Keys,
        "copy" => Command::Copy(arg),
        "quit" => Command::Quit,
        other => Command::Unknown(other.to_string()),
    })
}

/// Command names for the palette (without the slash), in the table's order.
pub fn names() -> Vec<String> {
    COMMANDS.iter().map(|s| s.name.to_string()).collect()
}

/// One row of the palette: the table index, the alias the query matched, if
/// any, and the letters it matched (character positions in the alias when
/// one matched, else in the name), for the palette to light them up.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ranked {
    pub index: usize,
    pub via_alias: Option<&'static str>,
    pub hits: Vec<usize>,
}

/// The commands `query` can mean, best first: a prefix of a primary name,
/// then a prefix of an alias, then a fuzzy match of the name, then of an
/// alias; ties keep the table's order. An empty query lists everything, by group.
pub fn rank(query: &str) -> Vec<Ranked> {
    let q = query.trim().to_ascii_lowercase();
    if q.is_empty() {
        return grouped().into_iter().map(|(_, index)| Ranked { index, via_alias: None, hits: Vec::new() }).collect();
    }
    let mut tiers: [Vec<Ranked>; 4] = Default::default();
    for (index, s) in COMMANDS.iter().enumerate() {
        if s.name.starts_with(&q) {
            tiers[0].push(Ranked { index, via_alias: None, hits: hits(&q, s.name) });
        } else if let Some(a) = s.aliases.iter().find(|a| a.starts_with(&q)) {
            tiers[1].push(Ranked { index, via_alias: Some(a), hits: hits(&q, a) });
        } else if subsequence(&q, s.name) {
            tiers[2].push(Ranked { index, via_alias: None, hits: hits(&q, s.name) });
        } else if let Some(a) = s.aliases.iter().find(|a| subsequence(&q, a)) {
            tiers[3].push(Ranked { index, via_alias: Some(a), hits: hits(&q, a) });
        }
    }
    tiers.into_iter().flatten().collect()
}

/// Whether the characters of `q` appear in `s` in order.
fn subsequence(q: &str, s: &str) -> bool {
    let mut chars = s.chars();
    q.chars().all(|c| chars.any(|x| x == c))
}

/// Where the letters of `q` fall in `s`, the first in order: positions in characters.
pub fn hits(q: &str, s: &str) -> Vec<usize> {
    let mut want = q.chars().map(|c| c.to_ascii_lowercase()).peekable();
    let mut out = Vec::new();
    for (i, c) in s.chars().enumerate() {
        match want.peek() {
            None => break,
            Some(&w) if w == c.to_ascii_lowercase() => {
                out.push(i);
                want.next();
            }
            _ => {}
        }
    }
    out
}

/// The palette's sections when nothing is typed, each with its commands in order.
pub const GROUPS: &[(&str, &[&str])] = &[
    (
        "session",
        &[
            "clear",
            "resume",
            "fork",
            "tree",
            "rewind",
            "undo",
            "undelete",
            "rename",
            "export",
            "compact",
            "goal",
            "plan",
            "shared-memory",
            "queue",
        ],
    ),
    ("model", &["model", "provider", "effort", "pricing", "prompt", "budget"]),
    (
        "context and tools",
        &["context", "context-live", "cost", "tools", "jobs", "panel", "todo", "diff", "init", "status"],
    ),
    ("settings", &["permissions", "yolo", "theme", "mouse", "timestamps", "vim", "keys"]),
    ("nexo", &["help", "copy", "quit"]),
];

/// Every command once, by group: `(group, table index)`. A command no group
/// names (none should be) comes last, under "more".
pub fn grouped() -> Vec<(&'static str, usize)> {
    let mut out: Vec<(&'static str, usize)> = Vec::new();
    for (g, names) in GROUPS {
        for n in names.iter() {
            if let Some(i) = COMMANDS.iter().position(|s| s.name == *n) {
                out.push((g, i));
            }
        }
    }
    for i in 0..COMMANDS.len() {
        if !out.iter().any(|(_, j)| *j == i) {
            out.push(("more", i));
        }
    }
    out
}

/// The argument values a command offers in the palette that never change:
/// `(value, what it does)`. The model, effort and theme lists come from the app.
pub fn fixed_args(name: &str) -> &'static [(&'static str, &'static str)] {
    match name {
        "permissions" => &[
            ("ask", "ask before writes and commands"),
            ("accept-edits", "writes go through; ask before commands"),
            ("plan", "reads, then a plan to approve before any write"),
            ("auto", "never ask"),
            ("read-only", "only reads and searches"),
        ],
        "prompt" => &[("edit", "open the system prompt in $EDITOR")],
        "copy" => &[("code", "the last code block of the last reply")],
        "goal" => &[("off", "clear the goal")],
        "plan" => &[("off", "leave plan mode")],
        "budget" => &[("off", "no limit for this session")],
        "jobs" => &[("kill", "stop every background job (or `kill <id>`)")],
        "shared-memory" => &[("off", "stop sharing another session's memory")],
        _ => &[],
    }
}

/// Whether the command's argument is one of a known set: Enter runs the
/// selected value. Elsewhere the list is only a suggestion and Enter sends what was typed.
pub fn closed_args(name: &str) -> bool {
    matches!(name, "permissions" | "effort" | "theme" | "prompt" | "copy" | "resume")
}

/// Whether the palette offers values for the command's argument at all.
pub fn takes_listed_args(name: &str) -> bool {
    matches!(name, "model" | "effort" | "theme" | "undelete" | "resume" | "rename" | "export")
        || !fixed_args(name).is_empty()
}

/// The command a mistyped name most likely meant: the name or alias at the
/// smallest edit distance, if it is close enough to be a guess worth showing.
pub fn closest(query: &str) -> Option<&'static str> {
    let q = query.trim().to_ascii_lowercase();
    if q.is_empty() {
        return None;
    }
    // names before aliases, so a tie goes to the name
    let words = COMMANDS
        .iter()
        .map(|s| (s.name, s.name))
        .chain(COMMANDS.iter().flat_map(|s| s.aliases.iter().map(move |a| (*a, s.name))));
    let mut best: Option<(usize, &'static str)> = None;
    for (word, name) in words {
        let d = distance(&q, word);
        if best.is_none_or(|(b, _)| d < b) {
            best = Some((d, name));
        }
    }
    let limit = (q.chars().count() / 3).max(1) + 1;
    best.filter(|(d, _)| *d <= limit).map(|(_, n)| n)
}

/// Edit distance in characters, a swap of two neighbours counting as one edit.
fn distance(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let mut d = vec![vec![0usize; b.len() + 1]; a.len() + 1];
    for (i, row) in d.iter_mut().enumerate() {
        row[0] = i;
    }
    d[0] = (0..=b.len()).collect();
    for i in 1..=a.len() {
        for j in 1..=b.len() {
            let cost = usize::from(a[i - 1] != b[j - 1]);
            let mut v = (d[i - 1][j] + 1).min(d[i][j - 1] + 1).min(d[i - 1][j - 1] + cost);
            if i > 1 && j > 1 && a[i - 1] == b[j - 2] && a[i - 2] == b[j - 1] {
                v = v.min(d[i - 2][j - 2] + 1);
            }
            d[i][j] = v;
        }
    }
    d[a.len()][b.len()]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_names_aliases_and_arguments() {
        assert_eq!(parse("/model"), Some(Command::Model(None)));
        assert_eq!(
            parse("/model deepseek/deepseek-chat "),
            Some(Command::Model(Some("deepseek/deepseek-chat".into())))
        );
        assert_eq!(parse("/m x"), Some(Command::Model(Some("x".into()))));
        assert_eq!(parse("/permissions auto"), Some(Command::Permissions(Some(PermissionMode::Auto))));
        assert_eq!(parse("/mode auto"), Some(Command::Permissions(Some(PermissionMode::Auto))));
        assert_eq!(parse("/permissions sideways"), Some(Command::BadArgument("permissions".into(), "sideways".into())));
        assert_eq!(parse("/yolo"), Some(Command::Yolo));
        assert_eq!(parse("/system"), Some(Command::Prompt));
        assert_eq!(parse("/prompt edit"), Some(Command::PromptEdit));
        assert_eq!(parse("/pricing in=1 out=2"), Some(Command::Pricing(Some("in=1 out=2".into()))));
        assert_eq!(parse("/prices"), Some(Command::Pricing(None)));
        assert_eq!(parse("/mouse"), Some(Command::Mouse));
        assert_eq!(parse("/timestamps"), Some(Command::Timestamps));
        assert_eq!(parse("/goal"), Some(Command::Goal(GoalArg::Show)));
        assert_eq!(parse("/goal make the tests pass"), Some(Command::Goal(GoalArg::Set("make the tests pass".into()))));
        assert_eq!(parse("/goal off"), Some(Command::Goal(GoalArg::Clear)));
        assert_eq!(parse("/plan"), Some(Command::Plan(None)));
        assert_eq!(parse("/planning add a cache"), Some(Command::Plan(Some("add a cache".into()))));
        assert_eq!(parse("/g x"), Some(Command::Goal(GoalArg::Set("x".into()))));
        assert_eq!(parse("/q"), Some(Command::Quit));
        assert_eq!(parse("/exit"), Some(Command::Quit));
        assert_eq!(parse("/resume"), Some(Command::Sessions));
        assert_eq!(parse("/sessions"), Some(Command::Sessions));
        assert_eq!(parse("/resume parser fix"), Some(Command::Resume("parser fix".into())));
        assert_eq!(parse("/undelete"), Some(Command::Undelete(None)));
        assert_eq!(parse("/shared-memory s1 the parser"), Some(Command::Mem(Some("s1 the parser".into()))));
        assert_eq!(parse("/restore 2026-1"), Some(Command::Undelete(Some("2026-1".into()))));
        assert_eq!(parse("/export notes.md"), Some(Command::Export(Some("notes.md".into()))));
        assert_eq!(parse("/save"), Some(Command::Export(None)));
        assert_eq!(parse("/rename parser de pesos"), Some(Command::Rename(Some("parser de pesos".into()))));
        assert_eq!(parse("/title"), Some(Command::Rename(None)));
        assert_eq!(parse("/diff"), Some(Command::Diff));
        assert_eq!(parse("/back"), Some(Command::Rewind));
        assert_eq!(parse("/fork"), Some(Command::Fork));
        assert_eq!(parse("/tree"), Some(Command::Tree));
        assert_eq!(parse("/ctx"), Some(Command::Context));
        assert_eq!(parse("/context-live"), Some(Command::ContextLive));
        assert_eq!(parse("/live"), Some(Command::ContextLive));
        assert_eq!(parse("/usage"), Some(Command::Cost));
        assert_eq!(parse("/new"), Some(Command::Clear));
        assert_eq!(parse("/?"), Some(Command::Help));
        assert_eq!(parse("/nope"), Some(Command::Unknown("nope".into())));
        assert_eq!(parse("hello"), None);
        assert!(names().contains(&"compact".to_string()));
        assert!(names().contains(&"resume".to_string()) && !names().contains(&"sessions".to_string()));
    }

    fn first(q: &str) -> (&'static str, Option<&'static str>) {
        let r = rank(q);
        (COMMANDS[r[0].index].name, r[0].via_alias)
    }

    #[test]
    fn ranks_primary_names_before_aliases_before_fuzzy_matches() {
        assert_eq!(first("ses"), ("resume", Some("sessions")), "an alias prefix finds its command");
        assert_eq!(first("re"), ("resume", None), "resume before rewind, by table order");
        assert_eq!(rank("re").iter().map(|r| COMMANDS[r.index].name).take(2).collect::<Vec<_>>(), ["resume", "rewind"]);
        assert_eq!(first("mo"), ("model", None));
        assert_eq!(first("mode"), ("model", None), "a primary-name prefix wins even over an exact alias");
        assert_eq!(first("sys"), ("prompt", Some("system")), "an alias prefix beats a fuzzy name");
        assert_eq!(first("exi"), ("quit", Some("exit")));
        assert_eq!(first("q"), ("quit", None), "the name's own prefix, not the alias");
        assert_eq!(first("cmpct"), ("compact", None), "a fuzzy name still matches");
        assert!(rank("zzz").is_empty());
        assert_eq!(rank("").len(), COMMANDS.len());
        assert_eq!(rank("cmpct")[0].hits, vec![0, 2, 3, 5, 6], "the letters it matched");
        assert_eq!(rank("ses")[0].hits, vec![0, 1, 2], "in the alias it matched through");
        let names: Vec<_> = rank("c").iter().map(|r| COMMANDS[r.index].name).collect();
        assert!(names.starts_with(&["compact", "clear", "cost", "context", "copy"]), "{names:?}");
    }

    #[test]
    fn every_command_sits_in_one_group() {
        let g = grouped();
        assert_eq!(g.len(), COMMANDS.len());
        assert!(g.iter().all(|(name, _)| *name != "more"), "a command no group lists: {g:?}");
        for (_, names) in GROUPS {
            for n in names.iter() {
                assert!(lookup(n).is_some_and(|s| s.name == *n), "{n} is not a command");
            }
        }
    }

    #[test]
    fn a_typo_finds_the_command_it_meant() {
        assert_eq!(closest("modle"), Some("model"));
        assert_eq!(closest("exti"), Some("quit"));
        assert_eq!(closest("comapct"), Some("compact"));
        assert_eq!(closest("zzzzzzzz"), None);
        assert_eq!(distance("kitten", "sitting"), 3);
        assert_eq!(distance("modle", "model"), 1);
    }

    #[test]
    fn knows_which_arguments_it_can_list() {
        for name in ["permissions", "prompt", "copy", "model", "effort", "theme", "goal"] {
            assert!(takes_listed_args(name), "{name}");
        }
        assert!(takes_listed_args("rename") && takes_listed_args("export") && !takes_listed_args("compact"));
        for (v, _) in fixed_args("permissions") {
            assert!(PermissionMode::parse(v).is_some(), "{v}");
        }
        assert!(closed_args("permissions") && !closed_args("goal") && !closed_args("model"));
    }
}
