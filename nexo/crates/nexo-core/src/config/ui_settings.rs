use super::*;

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct McpServerConfig {
    /// A remote server: its address (`https://...`). Then `command` is empty. `${VAR}` in it is read from the environment.
    pub url: Option<String>,
    /// Extra headers sent to a remote server (`Authorization = "Bearer ${TOKEN}"`).
    pub headers: BTreeMap<String, String>,
    /// For a remote server: `http` (streamable HTTP, with the old SSE transport as a fallback) or
    /// `sse` (the old transport only). Unset: `http`.
    pub transport: Option<String>,
    pub command: String,
    pub args: Vec<String>,
    pub env: BTreeMap<String, String>,
    pub cwd: Option<PathBuf>,
    pub enabled: Option<bool>,
    /// Believe the server when it marks a tool read-only (`readOnlyHint`): such a tool then runs without
    /// asking and is offered to read-only sub-agents. Off: every tool of the server is asked about.
    pub trust_read_only: bool,
}

/// How much the interface says by itself (`[ui] detail`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Detail {
    /// The least that works: no key hints, no doubled figures; the rest sits behind a key or a click.
    #[default]
    Quiet,
    /// Every hint and figure on screen.
    Full,
}

impl<'de> Deserialize<'de> for Detail {
    /// An unknown word reads as the default.
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Detail, D::Error> {
        let s = String::deserialize(d)?;
        Ok(if s.trim().eq_ignore_ascii_case("full") { Detail::Full } else { Detail::Quiet })
    }
}

/// The figure of dots on the welcome screen (`[ui] figure`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Figure {
    /// A turning spiral galaxy.
    #[default]
    Galaxy,
    /// A turning constellation on a sphere.
    Constellation,
    /// No figure, and no frames asked for it.
    None,
}

impl<'de> Deserialize<'de> for Figure {
    /// An unknown word reads as the default.
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Figure, D::Error> {
        let s = String::deserialize(d)?;
        Ok(match s.trim().to_ascii_lowercase().as_str() {
            "constellation" => Figure::Constellation,
            "none" => Figure::None,
            _ => Figure::Galaxy,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct UiConfig {
    pub theme: String,
    pub mouse: bool,
    pub show_reasoning: bool,
    /// Lines of a tool result shown before it is folded.
    pub fold_after: usize,
    /// A run of tool calls shows as one summary line that unfolds on demand.
    pub group_tools: bool,
    /// Grey ghost text finishes the word being typed from the words of the session; → takes it.
    pub complete_words: bool,
    /// The vim keys in the editor (`/settings vim` toggles them).
    pub vim: bool,
    /// Ring the terminal's bell (and send a desktop notification where the terminal
    /// supports it) when a turn that took this many seconds ends; 0 turns it off.
    pub notify_after: u64,
    /// Smooth scrolling, fades and the other motion of the interface (off: every
    /// change shows at once).
    pub animations: bool,
    /// The figure of dots on the welcome screen: `galaxy` (default), `constellation` or `none`.
    pub figure: Figure,
    /// Faint stars drift across an empty session until its first message.
    pub home_stars: bool,
    /// A burst of dots when a goal is met (green) or a turn or goal stops on a failure (red).
    pub bursts: bool,
    /// `quiet` (default): the screen drops the key hints, the doubled figures and the sentences nobody needs to act on;
    /// detail sits behind a key or a click. `full`: all of it on screen, as before.
    pub detail: Detail,
    /// The open to-do list as rows above the draft (the old strip). Off: a done/total count in the status bar instead
    /// (a click opens `/todo`).
    pub todo_strip: bool,
    /// Remapped keys, action name -> key (`search = "ctrl+s"`); see `nexo-tui`'s keymap.
    pub keys: BTreeMap<String, String>,
    /// The pieces of the status bar and their order, from `model`, `effort`, `dir`, `branch`,
    /// `mode` (permissions), `context` (gauge and %), `curator`, `mem`, `cache`, `plan` (what is left of a
    /// subscription plan), `tokens` and
    /// `cost`. Empty: all of them in that order. Unknown names are ignored.
    pub status_line: Vec<String>,
    /// `full` (the alternate screen) or `inline` (finished turns go to the terminal's own
    /// scrollback and only the draft and the turn in progress stay live at the bottom).
    pub screen: String,
    /// A shell command (`sh -c`, on Windows `cmd /C`) that draws the status bar: it gets a JSON of
    /// the session on stdin (model, cwd, branch, tokens, context %, cost, session id) and its first
    /// line of stdout, ANSI colours included, becomes the bar. Empty: the pieces of `status_line`.
    pub status_command: String,
    /// A dim `HH:MM` at the right of each message and reply, and a reply's duration (`/settings timestamps` toggles it).
    pub timestamps: bool,
    /// At each start of the interface, ask GitHub whether `main` has commits past the one nexo was built from, and say so
    /// (`NEXO_NO_UPDATE_CHECK` also turns it off).
    pub update_check: bool,
    /// When a turn ends, a dim suggestion of your next message shows in the empty input; Tab or → takes it and typing
    /// hides it. It costs one short model call per turn, which reuses the conversation's cached prompt.
    pub suggest: bool,
    /// The empty input shows one short hint that fits the moment (queued messages, a key worth knowing); false: it
    /// only says "ask anything".
    pub hints: bool,
}

impl Default for UiConfig {
    fn default() -> UiConfig {
        UiConfig {
            theme: "nexo".into(),
            mouse: true,
            show_reasoning: true,
            fold_after: 12,
            group_tools: true,
            complete_words: true,
            vim: false,
            notify_after: 15,
            animations: true,
            figure: Figure::Galaxy,
            home_stars: true,
            bursts: true,
            detail: Detail::Quiet,
            todo_strip: false,
            keys: BTreeMap::new(),
            status_line: Vec::new(),
            screen: "full".into(),
            status_command: String::new(),
            timestamps: true,
            update_check: true,
            suggest: true,
            hints: true,
        }
    }
}
