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
    spec("model", "common", 0, &["m", "models"], "[name]", "switch model, or set one up step by step (checked against the server)"),
    spec("logout", "model", 17, &["signout"], "[provider]", "forget the key or sign-in stored for a provider (the current one)"),
    spec("effort", "common", 8, &["reasoning", "think"], "[level|default]", "how much the model reasons, among the levels it takes"),
    spec("fast", "model", 18, &[], "[on|off]", "switch to the model's fast version, and back"),
    spec("permissions", "common", 1, &["permission", "mode"], "[ask|accept-edits|plan|yolo|read-only]", "set the permission mode"),
    spec(
        "sandbox", "settings", 43,
        &[],
        "[off|workspace-write|read-only]",
        "run bash commands in an OS sandbox (no argument: switch it on or off)",
    ),
    spec("add-dir", "settings", 44, &["adddir"], "<folder>", "let the file tools read and write in one more folder"),
    spec("plan", "common", 3, &["planning"], "[text|off]", "plan mode: reads only, then a plan to approve; with text: sends it"),
    spec("ultracode", "common", 4, &[], "<task>", "plan a coordinated multi-agent task for approval"),
    spec(
        "prompt", "model", 20,
        &["system"],
        "[edit]",
        "show the system prompt as the model receives it; `edit` opens it in $VISUAL or $EDITOR",
    ),
    spec(
        "memory", "model", 21,
        &["instructions"],
        "[user|add <text>|share [id]]",
        "edit the instructions the agent reads (the project's file, or with `user` your own); `add` writes a line, `share` brings in what another session knows",
    ),
    spec(
        "pricing", "model", 19,
        &["price", "prices"],
        "[in=… out=… cache=… write=…]",
        "what is known of the model; with values: its prices per million tokens",
    ),
    spec(
        "goal", "session", 15,
        &["g"],
        "[text|pause|resume|off]",
        "keep working towards a goal across turns; `pause` holds it, `off` clears it",
    ),
    spec("compact", "common", 5, &[], "", "replace the history with a summary"),
    spec("clear", "common", 6, &["new"], "", "start a new session here"),
    spec(
        "resume", "common", 2,
        &["sessions"],
        "[session|tree|undelete]",
        "pick a session to resume; with an id or a title: that one; `tree` shows the forks, `undelete` brings one back",
    ),
    spec("fork", "session", 9, &[], "", "continue in a new session copied from an earlier point of this one"),
    spec("rewind", "session", 10, &["back"], "", "go back to before an earlier message, files included (also Esc Esc)"),
    spec("undo", "session", 11, &["u"], "", "undo the last request: its messages and the files it wrote"),
    spec("init", "context and tools", 37, &[], "", "have the agent study the project and write AGENTS.md"),
    spec("review", "context and tools", 38, &[], "[base|commit|--staged]", "review the diff for bugs, security and missing tests (no edits)"),
    spec("commit", "context and tools", 39, &[], "[hint]", "commit the staged changes (or all) with a message in the repo's style"),
    spec("pr", "context and tools", 40, &["pull-request"], "[base]", "push the branch and open a pull request with gh"),
    spec("status", "context and tools", 41, &["info"], "", "version, model, key, endpoint, session, configuration files"),
    spec("settings", "settings", 42, &[], "[mouse|timestamps|vim]", "switch the mouse capture, the message times or the vim keys on or off; alone: as /status"),
    spec("doctor", "nexo", 46, &[], "", "check the setup: files, provider, model, key"),
    spec("tools", "context and tools", 26, &[], "", "the tools the model has, by kind"),
    spec("hooks", "context and tools", 27, &[], "", "the hooks that are loaded, and where they come from"),
    spec("mcp", "context and tools", 28, &[], "", "the MCP servers: state, tools, resources and prompts; turn on or off, log in, reconnect"),
    spec("chrome", "context and tools", 29, &["browser"], "", "connect your own Chrome, Edge or Brave to nexo: set it up, check it, use it here"),
    spec("skills", "context and tools", 30, &[], "", "the skills found (name, what it does, where from); /skill-name [args] runs one"),
    spec("plugins", "context and tools", 31, &[], "", "the installed plugins and what each adds (nexo plugin install adds one)"),
    spec(
        "budget", "model", 22,
        &["limit", "spend"],
        "[dollars|off]",
        "what this session and today cost; with a number: this session's limit",
    ),
    spec(
        "jobs", "context and tools", 32,
        &["bg"],
        "[kill [id]]",
        "a window with the background jobs and their output; `kill` stops one, or all",
    ),
    spec("export", "session", 14, &["save"], "[html|json] [path]", "write the conversation as markdown, HTML or JSON"),
    spec("rename", "session", 12, &["title", "name"], "<name>", "give this session a name"),
    spec("msg", "session", 13, &["mail", "tell"], "<session> <text>", "write to another running session; it reads it when it is idle"),
    spec("diff", "context and tools", 35, &[], "", "browse staged, unstaged and new files"),
    spec("panel", "context and tools", 33, &[], "", "live agent sessions by program: events, todo, mail; message and pause them (s filters)"),
    spec("cost", "context and tools", 25, &["usage"], "", "window: tokens, cost, calls"),
    spec("sent", "context and tools", 24, &["request"], "", "window: the last request exactly as sent, by section; what nexo added, what the curator put in"),
    spec("context", "context and tools", 23, &["ctx"], "[live]", "window: what the next call sends; `live`: a map of it beside the transcript"),
    spec("theme", "settings", 45, &[], "[name]", "change the colours (a picker without a name)"),
    spec("copy", "nexo", 47, &[], "[code]", "copy the last reply to the clipboard; `code`: its last code block"),
    spec("quit", "nexo", 48, &["exit", "q"], "", "leave nexo"),
    spec("shared-memory", "context and tools", 36, &[], "[read [epoch]|write <text>]", "read or append coordination notes shared with agents"),
    spec("todo", "context and tools", 34, &["tasks"], "", "the todo list, every item with its state"),
    spec("queue", "session", 16, &["queued"], "", "the requests waiting for the turn to end; pick one to edit it"),
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
