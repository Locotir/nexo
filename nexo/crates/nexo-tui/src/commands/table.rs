//! The command table: names, aliases, hints, the palette's groups and the hidden old names.

/// One command: its primary name, the other names it answers to, the
/// argument hint and what it does. The order is the order of the palette
/// and decides ties.
#[derive(Debug, Clone, Copy)]
pub struct Spec {
    pub name: &'static str,
    /// The palette section it sits under when nothing is typed.
    pub group: &'static str,
    /// Its place in the palette: sections list their commands in this order.
    pub slot: u8,
    pub aliases: &'static [&'static str],
    pub hint: &'static str,
    pub description: &'static str,
}

const fn spec(
    name: &'static str,
    group: &'static str,
    slot: u8,
    aliases: &'static [&'static str],
    hint: &'static str,
    description: &'static str,
) -> Spec {
    Spec { name, group, slot, aliases, hint, description }
}

pub const COMMANDS: &[Spec] = &[
    spec("help", "common", 7, &["h", "?"], "", "keys and commands"),
    spec("model", "common", 0, &["m", "models"], "[name]", "switch model, or set one up"),
    spec("logout", "model", 18, &["signout"], "[provider]", "forget a provider's stored key or sign-in"),
    spec("effort", "common", 8, &["reasoning", "think"], "[level|default]", "how much the model reasons"),
    spec("fast", "model", 19, &[], "[on|off]", "use the model's fast version, and back"),
    spec(
        "permissions",
        "common",
        1,
        &["permission", "mode"],
        "[ask|accept-edits|plan|yolo|read-only]",
        "set the permission mode",
    ),
    spec("sandbox", "settings", 45, &[], "[off|workspace-write|read-only]", "run commands in an OS sandbox, on or off"),
    spec("add-dir", "settings", 46, &["adddir"], "<folder>", "let the file tools use one more folder"),
    spec("plan", "common", 3, &["planning"], "[text|off]", "plan mode: reads only, plan to approve"),
    spec("ultracode", "common", 4, &[], "<task>", "plan a multi-agent task for approval"),
    spec("prompt", "model", 21, &["system"], "[edit]", "show the system prompt; `edit` opens it"),
    spec(
        "memory",
        "model",
        22,
        &["instructions"],
        "[user|add <text>|share [id]]",
        "edit the instructions the agent reads",
    ),
    spec(
        "pricing",
        "model",
        20,
        &["price", "prices"],
        "[in=… out=… cache=… write=…]",
        "the model's prices per million tokens",
    ),
    spec(
        "loop",
        "session",
        17,
        &[],
        "<interval> <prompt>|list|cancel <id>",
        "repeat a prompt in this session (min 60s)",
    ),
    spec("goal", "session", 15, &["g"], "[text|pause|resume|off]", "keep working towards a goal across turns"),
    spec("compact", "common", 5, &[], "", "replace the history with a summary"),
    spec("clear", "common", 6, &["new"], "", "start a new session here"),
    spec("resume", "common", 2, &["sessions"], "[session|tree|undelete]", "pick a session, or `tree` for its forks"),
    spec("fork", "session", 9, &[], "", "new session copied from an earlier point"),
    spec("rewind", "session", 10, &["back"], "", "go back to before a message (also Esc Esc)"),
    spec("undo", "session", 11, &["u"], "", "undo the last request and its file edits"),
    spec("init", "context and tools", 39, &[], "", "have the agent write AGENTS.md"),
    spec("review", "context and tools", 40, &[], "[base|commit|--staged]", "review the diff for bugs (no edits)"),
    spec("commit", "context and tools", 41, &[], "[hint]", "commit the staged changes with a message"),
    spec("pr", "context and tools", 42, &["pull-request"], "[base]", "push the branch and open a pull request"),
    spec("status", "context and tools", 43, &["info"], "", "version, model, key, endpoint, session"),
    spec("settings", "settings", 44, &[], "[mouse|timestamps|vim]", "a window: mouse, times, vim keys"),
    spec("doctor", "nexo", 48, &[], "", "check the setup: files, provider, key"),
    spec("tools", "context and tools", 27, &[], "", "the tools the model has, by kind"),
    spec("hooks", "context and tools", 28, &[], "", "the hooks loaded, and where from"),
    spec("mcp", "context and tools", 29, &[], "", "the MCP servers: state, tools, log in"),
    spec("chrome", "context and tools", 30, &["browser"], "", "connect your own Chrome, Edge or Brave"),
    spec("skills", "context and tools", 31, &[], "", "skills found; /skill-name runs one"),
    spec("plugins", "context and tools", 32, &[], "", "the installed plugins and what each adds"),
    spec("budget", "model", 23, &["limit", "spend"], "[dollars|off]", "what this session and today cost"),
    spec("jobs", "context and tools", 33, &["bg"], "[kill [id]]", "the background jobs and their output"),
    spec("export", "session", 14, &["save"], "[html|json] [path]", "save the chat as markdown, HTML or JSON"),
    spec("rename", "session", 12, &["title", "name"], "<name>", "give this session a name"),
    spec("msg", "session", 13, &["mail", "tell"], "<session> <text>", "write to another running session"),
    spec("diff", "context and tools", 37, &[], "", "browse staged, unstaged and new files"),
    spec("panel", "context and tools", 34, &[], "", "sub-agents, lanes and sessions: watch, act"),
    spec("cost", "context and tools", 26, &["usage"], "", "window: tokens, cost, calls"),
    spec("sent", "context and tools", 25, &["request"], "", "window: last prepared request summary"),
    spec("context", "context and tools", 24, &["ctx"], "[live]", "window: what the next call sends"),
    spec("theme", "settings", 47, &[], "[name]", "change the colours"),
    spec("copy", "nexo", 49, &[], "[code]", "copy the last reply, or its code block"),
    spec("quit", "nexo", 50, &["exit", "q"], "", "leave nexo"),
    spec(
        "shared-memory",
        "context and tools",
        38,
        &[],
        "[<id>|list|off|read|write]",
        "other sessions' memory, shared notes",
    ),
    spec("todo", "context and tools", 36, &["tasks"], "", "the todo list, every item with its state"),
    spec("team", "context and tools", 35, &["lanes"], "", "the panel on its team lanes"),
    spec("queue", "session", 16, &["queued"], "", "requests waiting for the turn to end"),
];

/// Names that still work but have no line of their own (not in /help, the menu or
/// completion): `(old name, command, arguments put before the ones typed)`. Typing
/// the old name is the same as typing the command with those arguments.
pub(super) const HIDDEN: &[(&str, &str, &str)] = &[
    ("agents", "panel", ""),
    ("approvals", "permissions", ""),
    ("provider", "model", ""),
    ("providers", "model", ""),
    ("login", "model", ""),
    ("signin", "model", ""),
    ("keys", "help", ""),
    ("keybindings", "help", ""),
    ("config", "status", ""),
    ("yolo", "permissions", "yolo"),
    ("mouse", "settings", "mouse"),
    ("timestamps", "settings", "timestamps"),
    ("vim", "settings", "vim"),
    ("context-live", "context", "live"),
    ("live", "context", "live"),
    ("map", "context", "live"),
    ("tree", "resume", "tree"),
    ("undelete", "resume", "undelete"),
    ("restore", "resume", "undelete"),
    ("mem", "memory", "share"),
    ("memory-shared", "memory", "share"),
    ("share", "memory", "share"),
    ("note", "memory", "add"),
    ("remember", "memory", "add"),
];

pub(super) fn hidden(name: &str) -> Option<&'static (&'static str, &'static str, &'static str)> {
    HIDDEN.iter().find(|(a, _, _)| *a == name)
}

/// Whether `name` is an old name with no line of its own.
pub fn is_hidden(name: &str) -> bool {
    hidden(name).is_some()
}

/// The spec a name or alias stands for.
pub fn lookup(name: &str) -> Option<&'static Spec> {
    let name = hidden(name).map_or(name, |(_, n, _)| n);
    COMMANDS.iter().find(|s| s.name == name || s.aliases.contains(&name))
}

/// Command names for the palette (without the slash), in the table's order.
pub fn names() -> Vec<String> {
    COMMANDS.iter().map(|s| s.name.to_string()).collect()
}
