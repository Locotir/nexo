use super::*;

/// A hook: `command` runs on `event` (see [`HookConfig::event_name`]), for the tools `matcher` names
/// (`bash`, `edit|write`, `web_*`, or any regex; none: every tool).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct HookConfig {
    pub event: String,
    pub matcher: Option<String>,
    pub command: String,
    /// Seconds the command may run before it is stopped (none: 60).
    pub timeout: Option<u64>,
    /// Where the hook came from, for `/hooks` (empty: nexo's own configuration). Never read from a file.
    #[serde(skip)]
    pub source: String,
}

impl HookConfig {
    /// The event under its canonical name, whatever way it is spelled (`pre_tool`, `PreToolUse`,
    /// `pre-tool-use`); none for a name no event has.
    pub fn event_name(&self) -> Option<&'static str> {
        canonical_event(&self.event)
    }
}

/// Every event a hook can listen to, by its canonical name.
pub const HOOK_EVENTS: [&str; 12] = [
    "session_start",
    "session_end",
    "user_prompt_submit",
    "pre_tool",
    "post_tool",
    "stop",
    "subagent_start",
    "subagent_stop",
    "pre_compact",
    "notification",
    "permission_request",
    "turn_end",
];

/// The canonical event name for a spelling: case, `_` and `-` are ignored, and the long forms
/// `PreToolUse` and `PostToolUse` stand for `pre_tool` and `post_tool`.
pub fn canonical_event(name: &str) -> Option<&'static str> {
    let key: String = name.chars().filter(|c| c.is_ascii_alphanumeric()).collect::<String>().to_ascii_lowercase();
    let key = key.strip_suffix("use").filter(|k| k.ends_with("tool")).unwrap_or(&key);
    HOOK_EVENTS.iter().copied().find(|e| e.replace('_', "") == key)
}
