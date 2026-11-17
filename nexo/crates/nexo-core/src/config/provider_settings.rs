use super::*;

/// How long a reply may go quiet when the provider names no limit (see [`ProviderConfig::idle_timeout`]).
pub const IDLE_TIMEOUT_SECS: u64 = 300;

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ProviderConfig {
    /// `openai-chat`, `openai-responses`, `anthropic`, `gemini`, ...
    pub protocol: String,
    /// Up to (not including) the protocol's path, e.g. `https://api.deepseek.com`
    /// or `https://api.deepseek.com/v1` (the adapter adds `/chat/completions`).
    pub base_url: String,
    /// The environment variable that holds the key.
    pub api_key_env: Option<String>,
    /// A command that prints the key (`pass show deepseek`), run in the system's shell.
    pub api_key_cmd: Option<String>,
    /// A file that holds the key (what `/model` writes a typed key to, readable only by you).
    pub api_key_file: Option<PathBuf>,
    /// The entry of the credential store (`nexo auth login`) read when no other
    /// source gives a key; none: the provider's own name.
    pub auth: Option<String>,
    /// The subscription login this provider is signed in with (`copilot`,
    /// `chatgpt`, `codex`, `xai`, `qwen`): `nexo auth login <provider>` runs it,
    /// and its tokens are refreshed before a request.
    pub login: Option<String>,
    /// Extra headers on every request.
    pub headers: BTreeMap<String, String>,
    /// Models offered in the picker (the vendor's list is added when it has one).
    pub models: Vec<String>,
    pub default_model: Option<String>,
    /// Prices per model, per million tokens.
    pub pricing: BTreeMap<String, Pricing>,
    /// Context windows per model, when known.
    pub context_window: BTreeMap<String, u32>,
    /// The most the provider's backend takes in one request whatever the model's own window (a subscription
    /// that serves less than the API: `codex` 272000). Applies to every model it serves; none: no cap.
    pub window_cap: Option<u32>,
    /// Largest reply per model, when known (the agent caps `max_tokens` by it).
    pub max_output: BTreeMap<String, u32>,
    /// The reasoning levels each model takes, as `/model` found them (`/effort` offers these).
    pub efforts: BTreeMap<String, Vec<String>>,
    /// The reasoning level chosen per model (`/effort`); it wins over `agent.reasoning`.
    pub effort: BTreeMap<String, String>,
    /// Settings a protocol may read (`api_version`, `bearer`, `cache`, ...), the `[providers.name.extra]` table; the README lists them.
    pub extra: BTreeMap<String, toml::Value>,
}

/// How long `api_key_cmd` may run unless `extra.api_key_cmd_secs` says otherwise. The lookup happens on every
/// request, so a password manager waiting for an unlock or a tty must not hold it for good; the time is long
/// enough for a person to answer a biometric or master-password prompt.
const KEY_CMD_LIMIT: std::time::Duration = std::time::Duration::from_secs(20);

/// Ends `pid` and everything it started. The command runs as the leader of its own process group, so a
/// pipeline or a `;` list is stopped whole: killing the shell alone would leave its children running with
/// the stdout pipe open. Windows has no groups, so the tree under the shell goes (`taskkill /T`).
fn kill_tree(pid: u32) {
    #[cfg(unix)]
    // SAFETY: a plain signal to a process group this process created.
    unsafe {
        libc::kill(-(pid as i32), libc::SIGKILL);
    }
    #[cfg(windows)]
    {
        let _ = std::process::Command::new("taskkill")
            .args(["/T", "/F", "/PID", &pid.to_string()])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
    }
}

/// What `cmd` prints when it succeeds within `limit`; None when it fails, prints nothing, or is still running
/// at the limit (it and what it started are killed then). Stdin is null, so it cannot prompt on the tty. It is
/// not cached: a command that mints short-lived tokens, or that works once the person unlocks the manager,
/// must run again. It blocks the thread for up to `limit`: async code calls it through `spawn_blocking`.
pub(super) fn run_key_cmd(cmd: &str, limit: std::time::Duration) -> Option<String> {
    use std::io::Read;
    use std::process::Stdio;
    let mut command = crate::shell::shell().command(cmd);
    command.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    let mut child = command.spawn().ok()?;
    // read on a thread of its own, so a command that prints a lot cannot fill the pipe and stall
    let mut out = child.stdout.take()?;
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut bytes = Vec::new();
        let _ = out.read_to_end(&mut bytes);
        let _ = tx.send(bytes);
    });
    let end = std::time::Instant::now() + limit;
    let status = loop {
        match child.try_wait() {
            Ok(Some(s)) => break s,
            Ok(None) if std::time::Instant::now() < end => std::thread::sleep(std::time::Duration::from_millis(10)),
            Ok(None) => {
                // the group first, while its leader still holds the id; the pipe closes with the last of them
                kill_tree(child.id());
                let _ = child.kill();
                let _ = child.wait();
                // once per process, and only the command's first word: its output may be the key
                static SAID: std::sync::Once = std::sync::Once::new();
                SAID.call_once(|| {
                    let word = cmd.split_whitespace().next().unwrap_or_default();
                    tracing::warn!("api_key_cmd `{word}` did not finish in {}s and was stopped", limit.as_secs_f32());
                });
                return None;
            }
            Err(_) => return None,
        }
    };
    // a child the command left running can hold the pipe open: wait for the output only a moment
    let bytes = rx.recv_timeout(std::time::Duration::from_secs(1)).ok()?;
    let k = String::from_utf8_lossy(&bytes).trim().to_string();
    (status.success() && !k.is_empty()).then_some(k)
}

impl ProviderConfig {
    /// How long `api_key_cmd` may run: `extra.api_key_cmd_secs` (1 to 600) when set, else 20 s.
    pub fn key_cmd_limit(&self) -> std::time::Duration {
        let secs = self.extra.get("api_key_cmd_secs").and_then(toml::Value::as_integer);
        secs.filter(|s| (1..=600).contains(s)).map_or(KEY_CMD_LIMIT, |s| std::time::Duration::from_secs(s as u64))
    }

    pub fn new(protocol: &str, base_url: &str, api_key_env: Option<&str>) -> ProviderConfig {
        ProviderConfig {
            protocol: protocol.into(),
            base_url: base_url.trim_end_matches('/').into(),
            api_key_env: api_key_env.map(String::from),
            ..Default::default()
        }
    }

    /// How long a reply may go without a byte before the request counts as failed: `extra.idle_timeout`, in
    /// seconds, 0 for never. A server that loads a large model before it answers needs more than a hosted
    /// one, so the limit is the provider's to set; unset (or not a whole number of seconds) it is 300 s.
    pub fn idle_timeout(&self) -> Option<std::time::Duration> {
        let secs = self.extra.get("idle_timeout").and_then(|v| v.as_integer()).and_then(|n| u64::try_from(n).ok());
        match secs.unwrap_or(IDLE_TIMEOUT_SECS) {
            0 => None,
            n => Some(std::time::Duration::from_secs(n)),
        }
    }

    /// True when requests need a key: a source is named or a login is expected.
    pub fn needs_key(&self) -> bool {
        self.api_key_env.is_some() || self.api_key_cmd.is_some() || self.api_key_file.is_some() || self.login.is_some()
    }

    /// True when the provider can be used: it needs no key, or one is found.
    pub fn ready(&self, name: &str) -> bool {
        !self.needs_key() || self.resolve_key_for(name).is_some()
    }

    /// The key: the variable, the file, the command, then the credential store
    /// under `auth` (or `name`); None when none gives one.
    pub fn resolve_key_for(&self, name: &str) -> Option<String> {
        self.resolve_key()
            .or_else(|| crate::auth::get(self.auth.as_deref().unwrap_or(name)).map(|c| c.secret().to_string()))
    }

    /// The key from the variable, the file, the command, then (when `auth`
    /// names an entry) the credential store; None when none gives one.
    pub fn resolve_key(&self) -> Option<String> {
        if let Some(v) = &self.api_key_env {
            if let Ok(k) = std::env::var(v) {
                let k = k.trim().to_string();
                if !k.is_empty() {
                    return Some(k);
                }
            }
        }
        if let Some(f) = &self.api_key_file {
            if let Ok(k) = std::fs::read_to_string(f) {
                let k = k.trim().to_string();
                if !k.is_empty() {
                    return Some(k);
                }
            }
        }
        if let Some(cmd) = &self.api_key_cmd {
            if let Some(k) = run_key_cmd(cmd, self.key_cmd_limit()) {
                return Some(k);
            }
        }
        let entry = self.auth.as_deref()?;
        crate::auth::get(entry).map(|c| c.secret().to_string())
    }

    pub fn pricing_for(&self, model: &str) -> Option<Pricing> {
        self.pricing.get(model).copied().or_else(|| {
            // `deepseek-chat*` matches `deepseek-chat`; the longest matching prefix wins
            self.pricing
                .iter()
                .filter(|(k, _)| k.ends_with('*') && model.starts_with(&k[..k.len() - 1]))
                .max_by_key(|(k, _)| k.len())
                .map(|(_, p)| *p)
        })
    }
}

/// What a request does at `agent.max_turns` tool rounds.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum OnLimit {
    /// The model gets a progress report and decides to stop or to go on.
    #[default]
    Checkpoint,
    /// The request stops and the person is asked to continue.
    Ask,
}

impl<'de> Deserialize<'de> for OnLimit {
    /// An unknown word reads as the default.
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<OnLimit, D::Error> {
        let s = String::deserialize(d)?;
        Ok(if s.trim().eq_ignore_ascii_case("ask") { OnLimit::Ask } else { OnLimit::Checkpoint })
    }
}
