//! The `Command` a typed `/line` stands for, and how a line is parsed into one.

use nexo_core::permission::PermissionMode;

use super::table::{hidden, lookup};

#[derive(Debug, Clone, PartialEq)]
pub enum Command {
    Help,
    Loop(Option<String>),
    Model(Option<String>),
    Login,
    /// `/logout [provider]`: the stored key or sign-in of the provider, removed.
    Logout(Option<String>),
    Doctor,
    Effort(Option<String>),
    /// `/fast [on|off]`
    Fast(Option<String>),
    Undo,
    Init,
    /// `/review [base|commit|--staged]`
    Review(Option<String>),
    /// `/commit [hint]`
    Commit(Option<String>),
    /// `/pr [base]`
    Pr(Option<String>),
    Status,
    /// `/settings` alone: the window of switches.
    Settings,
    Tools,
    Hooks,
    /// `/mcp`: the window with the MCP servers.
    Mcp,
    /// `/chrome`: the window that connects the person's own browser.
    Chrome,
    /// `/team`: the panel with the cursor on the lanes of `team.toml`.
    Team,
    /// `/skills`: the Agent Skills found, with their source.
    Skills,
    /// `/plugins`: the installed plugins and what each adds.
    Plugins,
    Vim,
    Budget(Option<String>),
    /// `/jobs` opens the window, `/jobs kill` stops every one, `/jobs kill 3` stops job 3.
    Jobs(Option<Option<u32>>),
    /// The supervisor's panel: one card per agent session.
    Panel,
    Permissions(Option<PermissionMode>),
    /// `/sandbox [mode]`: None switches it on or off.
    Sandbox(Option<nexo_core::sandbox::SandboxMode>),
    /// `/plan` turns plan mode on, `/plan text` also sends the text, `/plan off` leaves it.
    Plan(Option<String>),
    /// Plan a coordinated task before any agents are launched.
    Ultracode(Option<String>),
    Prompt,
    PromptEdit,
    /// `/memory [user]`: the project's instructions file, or the person's own, in $EDITOR.
    Memory {
        user: bool,
    },
    /// `/note <text>` (`/remember`): one more line in the project's instructions file.
    Remember(Option<String>),
    Pricing(Option<String>),
    Mouse,
    Timestamps,
    /// `/goal text` sets, `/goal` shows, `/goal pause` and `/goal resume` hold it and go on, `/goal off` clears.
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
    SharedMemory(Option<String>),
    /// `/todo`: the todo list.
    Todo,
    /// `/queue`: the requests waiting for the turn to end.
    Queue,
    Rewind,
    Fork,
    Tree,
    Export(Option<String>),
    Rename(Option<String>),
    /// `/add-dir <folder>`: one more folder the file tools may use.
    AddDir(Option<String>),
    /// `/msg session text`; without arguments, the sessions that can be written to.
    Msg(Option<String>),
    Diff,
    Cost,
    /// `/sent` (`/request`): a summary of the last prepared request; delivery is not confirmed.
    Sent,
    Context,
    ContextLive,
    Theme(Option<String>),
    Copy(Option<String>),
    Quit,
    Unknown(String),
    /// `/permissions nonsense`
    BadArgument(String, String),
}

impl Command {
    /// Whether the command works on the first screen, before any model is set up. The rest
    /// open the model setup there.
    pub fn works_without_model(&self) -> bool {
        matches!(
            self,
            Command::Help
                | Command::Theme(_)
                | Command::Doctor
                | Command::Quit
                | Command::Login
                | Command::Logout(_)
                | Command::Model(None)
                | Command::Mouse
                | Command::Timestamps
                | Command::Vim
                | Command::Unknown(_)
                | Command::BadArgument(..)
        )
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum GoalArg {
    Show,
    Set(String),
    Clear,
    Pause,
    Resume,
}

/// The subcommand a single word of `/goal` names, in English or Spanish: the whole word, or three letters or more
/// that start only words of one subcommand (`res` is resume, `can` is pause). Case, a leading `¡` or `¿` and a
/// trailing `.`, `!` or `?` do not matter. None: the text is a goal. Every stop-like word pauses, so a goal is
/// never lost by a word that was meant to stop it.
fn goal_word(word: &str) -> Option<GoalArg> {
    const WORDS: [(&str, GoalArg); 26] = [
        ("off", GoalArg::Clear),
        ("clear", GoalArg::Clear),
        ("none", GoalArg::Clear),
        ("done", GoalArg::Clear),
        ("borrar", GoalArg::Clear),
        ("limpiar", GoalArg::Clear),
        ("quitar", GoalArg::Clear),
        ("nada", GoalArg::Clear),
        ("ninguno", GoalArg::Clear),
        ("pause", GoalArg::Pause),
        ("pausa", GoalArg::Pause),
        ("pausar", GoalArg::Pause),
        ("stop", GoalArg::Pause),
        ("para", GoalArg::Pause),
        ("parar", GoalArg::Pause),
        ("detener", GoalArg::Pause),
        ("cancel", GoalArg::Pause),
        ("cancelar", GoalArg::Pause),
        ("end", GoalArg::Pause),
        ("fin", GoalArg::Pause),
        ("resume", GoalArg::Resume),
        ("reanudar", GoalArg::Resume),
        ("continue", GoalArg::Resume),
        ("continuar", GoalArg::Resume),
        ("seguir", GoalArg::Resume),
        ("sigue", GoalArg::Resume),
    ];
    let word = word.to_lowercase();
    let word = word.trim_start_matches(['¡', '¿']).trim_end_matches(['.', '!', '?']);
    if let Some((_, a)) = WORDS.iter().find(|(w, _)| *w == word) {
        return Some(a.clone());
    }
    if word.chars().count() < 3 {
        return None;
    }
    let mut hits = WORDS.iter().filter(|(w, _)| w.starts_with(word)).map(|(_, a)| a);
    let first = hits.next()?;
    if hits.all(|a| a == first) {
        Some(first.clone())
    } else {
        None
    }
}

/// Parses a line that starts with `/`.
pub fn parse(line: &str) -> Option<Command> {
    let line = line.trim();
    let rest = line.strip_prefix('/')?;
    let mut it = rest.splitn(2, char::is_whitespace);
    let mut typed = it.next().unwrap_or("").trim().to_ascii_lowercase();
    let mut arg = it.next().map(|a| a.trim().to_string()).filter(|a| !a.is_empty());
    if let Some((_, parent, before)) = hidden(&typed) {
        // `/login` stays what it was: the model setup, with a pointer to the providers
        if typed == "login" || typed == "signin" {
            return Some(Command::Login);
        }
        // these took no argument before the fold and ignored any typed after them
        let ignores = matches!(
            typed.as_str(),
            "yolo" | "mouse" | "timestamps" | "vim" | "context-live" | "live" | "map" | "tree"
        );
        if ignores {
            arg = None;
        }
        if !before.is_empty() {
            arg = Some(match arg {
                Some(a) => format!("{before} {a}"),
                None => before.to_string(),
            });
        }
        typed = parent.to_string();
    }
    let Some(spec) = lookup(&typed) else { return Some(Command::Unknown(typed)) };
    Some(match spec.name {
        "help" => Command::Help,
        "loop" => Command::Loop(arg),
        "model" => Command::Model(arg),
        "logout" => Command::Logout(arg),
        "doctor" => Command::Doctor,
        "effort" => Command::Effort(arg),
        "fast" => Command::Fast(arg),
        "undo" => Command::Undo,
        "init" => Command::Init,
        "review" => Command::Review(arg),
        "commit" => Command::Commit(arg),
        "pr" => Command::Pr(arg),
        "status" => Command::Status,
        "tools" => Command::Tools,
        "sent" => Command::Sent,
        "hooks" => Command::Hooks,
        "mcp" => Command::Mcp,
        "chrome" => Command::Chrome,
        "team" => Command::Team,
        "skills" => Command::Skills,
        "plugins" => Command::Plugins,
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
        "sandbox" => match arg {
            None => Command::Sandbox(None),
            Some(a) => match nexo_core::sandbox::SandboxMode::parse(&a) {
                Some(m) => Command::Sandbox(Some(m)),
                None => Command::BadArgument("sandbox".into(), a),
            },
        },
        "plan" => Command::Plan(arg),
        "ultracode" => Command::Ultracode(arg),
        "prompt" => match arg.as_deref() {
            Some("edit") | Some("e") => Command::PromptEdit,
            Some(a) => Command::BadArgument("prompt".into(), a.to_string()),
            None => Command::Prompt,
        },
        "shared-memory" => Command::SharedMemory(arg),
        "memory" => match arg.as_deref() {
            None => Command::Memory { user: false },
            Some("user") => Command::Memory { user: true },
            Some("share") => Command::Mem(None),
            Some(a) if a.starts_with("share ") => Command::Mem(Some(a["share ".len()..].trim().to_string())),
            Some("add") => Command::Remember(None),
            Some(a) if a.starts_with("add ") => Command::Remember(Some(a["add ".len()..].trim().to_string())),
            Some(a) => Command::BadArgument("memory".into(), a.to_string()),
        },
        "pricing" => Command::Pricing(arg),
        "goal" => Command::Goal(match arg.as_deref() {
            None => GoalArg::Show,
            // only a bare word: `/goal pause the cron jobs` is a goal; `res` is `resume`
            Some(t) => goal_word(t).unwrap_or_else(|| GoalArg::Set(t.to_string())),
        }),
        "compact" => Command::Compact,
        "clear" => Command::Clear,
        "resume" => match arg {
            Some(s) if s == "tree" => Command::Tree,
            Some(s) if s == "undelete" => Command::Undelete(None),
            Some(s) if s.starts_with("undelete ") => Command::Undelete(Some(s["undelete ".len()..].trim().to_string())),
            Some(s) => Command::Resume(s),
            None => Command::Sessions,
        },
        "rewind" => Command::Rewind,
        "fork" => Command::Fork,
        "todo" => Command::Todo,
        "queue" => Command::Queue,
        "export" => Command::Export(arg),
        "rename" | "title" | "name" => Command::Rename(arg),
        "add-dir" | "adddir" => Command::AddDir(arg),
        "msg" | "mail" | "tell" => Command::Msg(arg),
        "diff" => Command::Diff,
        "cost" => Command::Cost,
        "context" => match arg.as_deref() {
            None => Command::Context,
            Some("live") => Command::ContextLive,
            Some(a) => Command::BadArgument("context".into(), a.to_string()),
        },
        "settings" => match arg.as_deref().map(|a| a.split_whitespace().collect::<Vec<_>>()).as_deref() {
            None => Command::Settings,
            // `/settings mouse off` reads as `/settings mouse`: the key is the switch
            Some(["mouse", ..]) => Command::Mouse,
            Some(["timestamps", ..]) => Command::Timestamps,
            Some(["vim", ..]) => Command::Vim,
            Some(_) => Command::BadArgument("settings".into(), arg.clone().unwrap_or_default()),
        },
        "theme" => Command::Theme(arg),
        "copy" => Command::Copy(arg),
        "quit" => Command::Quit,
        other => Command::Unknown(other.to_string()),
    })
}
