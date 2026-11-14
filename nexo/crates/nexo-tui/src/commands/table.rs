//! The command table: names, aliases, hints, the palette's groups and the hidden old names.

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
    spec("model", &["m", "models"], "[name]", "switch model, or set one up step by step (checked against the server)"),
    spec("logout", &["signout"], "[provider]", "forget the key or sign-in stored for a provider (the current one)"),
    spec("effort", &["reasoning", "think"], "[level|default]", "how much the model reasons, among the levels it takes"),
    spec("fast", &[], "[on|off]", "switch to the model's fast version, and back"),
    spec("permissions", &["permission", "mode"], "[ask|accept-edits|plan|yolo|read-only]", "set the permission mode"),
    spec(
        "sandbox",
        &[],
        "[off|workspace-write|read-only]",
        "run bash commands in an OS sandbox (no argument: switch it on or off)",
    ),
    spec("add-dir", &["adddir"], "<folder>", "let the file tools read and write in one more folder"),
    spec("plan", &["planning"], "[text|off]", "plan mode: reads only, then a plan to approve; with text: sends it"),
    spec("ultracode", &[], "<task>", "plan a coordinated multi-agent task for approval"),
    spec(
        "prompt",
        &["system"],
        "[edit]",
        "show the system prompt as the model receives it; `edit` opens it in $VISUAL or $EDITOR",
    ),
    spec(
        "memory",
        &["instructions"],
        "[user|add <text>|share [id]]",
        "edit the instructions the agent reads (the project's file, or with `user` your own); `add` writes a line, `share` brings in what another session knows",
    ),
    spec(
        "pricing",
        &["price", "prices"],
        "[in=… out=… cache=… write=…]",
        "what is known of the model; with values: its prices per million tokens",
    ),
    spec(
        "goal",
        &["g"],
        "[text|pause|resume|off]",
        "keep working towards a goal across turns; `pause` holds it, `off` clears it",
    ),
    spec("compact", &[], "", "replace the history with a summary"),
    spec("clear", &["new"], "", "start a new session here"),
    spec(
        "resume",
        &["sessions"],
        "[session|tree|undelete]",
        "pick a session to resume; with an id or a title: that one; `tree` shows the forks, `undelete` brings one back",
    ),
    spec("fork", &[], "", "continue in a new session copied from an earlier point of this one"),
    spec("rewind", &["back"], "", "go back to before an earlier message, files included (also Esc Esc)"),
    spec("undo", &["u"], "", "undo the last request: its messages and the files it wrote"),
    spec("init", &[], "", "have the agent study the project and write AGENTS.md"),
    spec("review", &[], "[base|commit|--staged]", "review the diff for bugs, security and missing tests (no edits)"),
    spec("commit", &[], "[hint]", "commit the staged changes (or all) with a message in the repo's style"),
    spec("pr", &["pull-request"], "[base]", "push the branch and open a pull request with gh"),
    spec("status", &["info"], "", "version, model, key, endpoint, session, configuration files"),
    spec("settings", &[], "[mouse|timestamps|vim]", "switch the mouse capture, the message times or the vim keys on or off; alone: as /status"),
    spec("doctor", &[], "", "check the setup: files, provider, model, key"),
    spec("tools", &[], "", "the tools the model has, by kind"),
    spec("hooks", &[], "", "the hooks that are loaded, and where they come from"),
    spec("mcp", &[], "", "the MCP servers: state, tools, resources and prompts; turn on or off, log in, reconnect"),
    spec("chrome", &["browser"], "", "connect your own Chrome, Edge or Brave to nexo: set it up, check it, use it here"),
    spec("skills", &[], "", "the skills found (name, what it does, where from); /skill-name [args] runs one"),
    spec("plugins", &[], "", "the installed plugins and what each adds (nexo plugin install adds one)"),
    spec(
        "budget",
        &["limit", "spend"],
        "[dollars|off]",
        "what this session and today cost; with a number: this session's limit",
    ),
    spec(
        "jobs",
        &["bg"],
        "[kill [id]]",
        "a window with the background jobs and their output; `kill` stops one, or all",
    ),
    spec("export", &["save"], "[html|json] [path]", "write the conversation as markdown, HTML or JSON"),
    spec("rename", &["title", "name"], "<name>", "give this session a name"),
    spec("msg", &["mail", "tell"], "<session> <text>", "write to another running session; it reads it when it is idle"),
    spec("diff", &[], "", "browse staged, unstaged and new files"),
    spec("panel", &[], "", "live agent sessions by program: events, todo, mail; message and pause them (s filters)"),
    spec("cost", &["usage"], "", "window: tokens, cost, calls"),
    spec("sent", &["request"], "", "window: the last request exactly as sent, by section; what nexo added, what the curator put in"),
    spec("context", &["ctx"], "[live]", "window: what the next call sends; `live`: a map of it beside the transcript"),
    spec("theme", &[], "[name]", "change the colours (a picker without a name)"),
    spec("copy", &[], "[code]", "copy the last reply to the clipboard; `code`: its last code block"),
    spec("quit", &["exit", "q"], "", "leave nexo"),
    spec("shared-memory", &[], "[read [epoch]|write <text>]", "read or append coordination notes shared with agents"),
    spec("todo", &["tasks"], "", "the todo list, every item with its state"),
    spec("queue", &["queued"], "", "the requests waiting for the turn to end; pick one to edit it"),
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

/// The spec a name or alias stands for.
pub fn lookup(name: &str) -> Option<&'static Spec> {
    let name = hidden(name).map_or(name, |(_, n, _)| n);
    COMMANDS.iter().find(|s| s.name == name || s.aliases.contains(&name))
}

/// Command names for the palette (without the slash), in the table's order.
pub fn names() -> Vec<String> {
    COMMANDS.iter().map(|s| s.name.to_string()).collect()
}

/// The palette's sections when nothing is typed, each with its commands in order.
pub const GROUPS: &[(&str, &[&str])] = &[
    ("common", &["model", "permissions", "resume", "plan", "ultracode", "compact", "clear", "help", "effort"]),
    ("session", &["fork", "rewind", "undo", "rename", "msg", "export", "goal", "queue"]),
    ("model", &["logout", "fast", "pricing", "prompt", "memory", "budget"]),
    (
        "context and tools",
        &[
            "context",
            "sent",
            "cost",
            "tools",
            "hooks",
            "mcp",
            "chrome",
            "skills",
            "plugins",
            "jobs",
            "panel",
            "todo",
            "diff",
            "shared-memory",
            "init",
            "review",
            "commit",
            "pr",
            "status",
        ],
    ),
    ("settings", &["settings", "sandbox", "add-dir", "theme"]),
    ("nexo", &["doctor", "copy", "quit"]),
];
