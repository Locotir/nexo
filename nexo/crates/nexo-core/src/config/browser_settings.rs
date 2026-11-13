use super::*;

/// Which browser the `browser` tool drives.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum BrowserSource {
    /// The Chrome, Chromium or Edge installed on this machine (never downloads anything).
    #[default]
    Installed,
    /// A Chrome nexo keeps for itself.
    OwnChrome,
    /// The installed one, else nexo's own.
    Auto,
}

/// Where the browser keeps cookies, logins and local storage.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum BrowserProfile {
    /// A throwaway profile, deleted when the browser closes.
    #[default]
    Ephemeral,
    /// One profile kept under nexo's data folder: sign in to a site once and it stays signed in.
    Persistent,
}

/// A window size, `1280x800` in the file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WindowSize {
    pub width: u32,
    pub height: u32,
}

impl WindowSize {
    /// `1280x800` (also `1280,800` or `1280 800`); both sides between 200 and 16384 pixels.
    pub fn parse(text: &str) -> Option<WindowSize> {
        let mut parts =
            text.split(|c: char| c == 'x' || c == 'X' || c == ',' || c.is_whitespace()).filter(|p| !p.is_empty());
        let (width, height): (u32, u32) = (parts.next()?.parse().ok()?, parts.next()?.parse().ok()?);
        let ok = |n: u32| (200..=16384).contains(&n);
        (parts.next().is_none() && ok(width) && ok(height)).then_some(WindowSize { width, height })
    }
}

impl Default for WindowSize {
    fn default() -> WindowSize {
        WindowSize { width: 1280, height: 800 }
    }
}

impl std::fmt::Display for WindowSize {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}x{}", self.width, self.height)
    }
}

impl Serialize for WindowSize {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for WindowSize {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<WindowSize, D::Error> {
        let text = String::deserialize(d)?;
        WindowSize::parse(&text).ok_or_else(|| {
            serde::de::Error::custom(format!(
                "window must be WIDTHxHEIGHT between 200 and 16384, like 1280x800, not {text:?}"
            ))
        })
    }
}

/// `[browser]`: how the `browser` tool starts its browser.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct BrowserConfig {
    /// `installed` (default), `own-chrome` or `auto`.
    pub source: BrowserSource,
    /// `ephemeral` (default: a clean profile, deleted at close) or `persistent` (kept, so logins last).
    pub profile: BrowserProfile,
    /// Hide the window. Unset: hidden for an ephemeral profile, shown for a persistent one (so the person can
    /// sign in), and hidden anyway where there is no screen.
    pub headless: Option<bool>,
    /// The browser program, instead of searching for an installed one.
    pub path: Option<String>,
    /// The window size, `1280x800` by default.
    pub window: WindowSize,
}

impl Default for BrowserConfig {
    fn default() -> BrowserConfig {
        BrowserConfig {
            source: BrowserSource::Installed,
            profile: BrowserProfile::Ephemeral,
            headless: None,
            path: None,
            window: WindowSize::default(),
        }
    }
}
