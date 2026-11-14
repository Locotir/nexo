use super::*;

/// Whether the browser's native-messaging manifest is in place, as `/chrome` shows it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChromeHostState {
    /// No manifest (or, on Windows, no registry entry): the browser cannot start nexo.
    Missing,
    /// The manifest points at the nexo program that is running.
    Ready,
    /// The manifest points at another path (nexo was moved or updated); the path it names.
    OldPath(String),
}

/// The extension's files in nexo's data folder, against the ones this nexo carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChromeFiles {
    Missing,
    /// There, but from another nexo version (or not whole).
    Stale,
    Current,
}

/// One browser of the person's whose manifest was looked at.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChromeHostInfo {
    /// `Google Chrome`, `Brave`...
    pub browser: String,
    pub state: ChromeHostState,
}

/// A browser with the nexo extension that answered.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChromeRunning {
    /// The id `select_browser` takes.
    pub id: String,
    /// `Google Chrome` ...
    pub name: String,
}

/// What `/chrome` shows: the live state of the way into the person's own browser.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChromeInfo {
    /// The browsers found on this machine (Chrome alone when none was found) and their manifests.
    pub hosts: Vec<ChromeHostInfo>,
    /// Where the extension's files are, or will be.
    pub extension_dir: PathBuf,
    pub files: ChromeFiles,
    /// Browsers with the extension that answered.
    pub running: Vec<ChromeRunning>,
    /// Browsers that are there but did not answer: `(id, why)`.
    #[serde(default)]
    pub not_answering: Vec<(String, String)>,
    /// What this session's browser tool is using, when it is the person's own browser.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub in_use: Option<String>,
    /// This listing follows a Set up, so the three steps to take in the browser are shown.
    #[serde(default)]
    pub just_set_up: bool,
}
