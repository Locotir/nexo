//! `nexo chrome-setup`: connects nexo to the Chrome you already use and are logged in to. Chrome 136 and
//! later refuse outside debugging on the default profile, so nexo goes in through an extension instead:
//! this writes the native-messaging manifest that lets the extension start `nexo-browser-host`, puts the
//! extension's files where Chrome can load them, and opens Chrome on the page where that takes one click.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{bail, Context, Result};
use clap::Args;
use serde_json::json;

/// The name the extension passes to `connectNative` (see `background.js`).
const HOST_NAME: &str = "dev.nexo.browser";
/// The id Chrome derives from the `key` in the extension's manifest: it is the same on every machine.
const EXTENSION_ID: &str = "lollgkhihanglklkkahimoikiegabdon";
const HOST_BIN: &str = "nexo-browser-host";
/// Browsers tried, in this order, along `PATH`.
const BROWSERS: [&str; 5] = ["google-chrome-stable", "google-chrome", "chromium", "chromium-browser", "microsoft-edge"];

const EXTENSION_FILES: [(&str, &str); 3] = [
    ("manifest.json", include_str!("../assets/chrome-extension/manifest.json")),
    ("background.js", include_str!("../assets/chrome-extension/background.js")),
    ("content.js", include_str!("../assets/chrome-extension/content.js")),
];

#[derive(Args, Debug)]
pub struct ChromeSetupArgs {
    /// Write the files and print the steps, but do not open Chrome.
    #[arg(long)]
    pub no_open: bool,
}

/// The text of the native-messaging manifest for a host at `host`.
fn manifest_json(host: &Path) -> String {
    let m = json!({
        "name": HOST_NAME,
        "description": "nexo browser host",
        "path": host.to_string_lossy(),
        "type": "stdio",
        "allowed_origins": [format!("chrome-extension://{EXTENSION_ID}/")],
    });
    serde_json::to_string_pretty(&m).unwrap_or_default() + "\n"
}

/// Folders Chrome and Chromium read native-messaging manifests from, for the current system.
#[cfg(target_os = "linux")]
fn manifest_dirs() -> Vec<PathBuf> {
    let Some(cfg) = dirs::config_dir() else { return Vec::new() };
    ["google-chrome", "chromium"].iter().map(|b| cfg.join(b).join("NativeMessagingHosts")).collect()
}

#[cfg(target_os = "macos")]
fn manifest_dirs() -> Vec<PathBuf> {
    let Some(home) = dirs::home_dir() else { return Vec::new() };
    let base = home.join("Library/Application Support");
    ["Google/Chrome", "Chromium"].iter().map(|b| base.join(b).join("NativeMessagingHosts")).collect()
}

/// On Windows the manifest can live anywhere: the registry entry points at it.
#[cfg(windows)]
fn manifest_dirs() -> Vec<PathBuf> {
    vec![nexo_core::paths::data_dir().join("chrome-host")]
}

#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
fn manifest_dirs() -> Vec<PathBuf> {
    Vec::new()
}

/// Windows only: `HKCU\Software\Google\Chrome\NativeMessagingHosts\<name>` holds the manifest's path.
#[cfg(windows)]
fn register(manifest: &Path) -> Result<()> {
    let key = format!(r"HKCU\Software\Google\Chrome\NativeMessagingHosts\{HOST_NAME}");
    let status = Command::new("reg")
        .args(["add", &key, "/ve", "/t", "REG_SZ", "/d"])
        .arg(manifest)
        .arg("/f")
        .stdout(Stdio::null())
        .status()
        .context("could not run `reg`")?;
    if !status.success() {
        bail!("`reg add {key}` failed");
    }
    Ok(())
}

#[cfg(not(windows))]
fn register(_manifest: &Path) -> Result<()> {
    Ok(())
}

/// The host program: next to `nexo` first, then along `PATH`.
fn find_host() -> Option<PathBuf> {
    let file = format!("{HOST_BIN}{}", std::env::consts::EXE_SUFFIX);
    let beside = std::env::current_exe().ok().and_then(|e| e.parent().map(|d| d.join(&file)));
    if let Some(p) = beside.filter(|p| p.is_file()) {
        return Some(p);
    }
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path).map(|d| d.join(&file)).find(|p| p.is_file())
}

fn find_browser() -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    let dirs: Vec<PathBuf> = std::env::split_paths(&path).collect();
    BROWSERS.iter().find_map(|name| {
        let file = format!("{name}{}", std::env::consts::EXE_SUFFIX);
        dirs.iter().map(|d| d.join(&file)).find(|p| p.is_file())
    })
}

/// Writes the manifest into each folder; returns the files written.
fn write_manifests(dirs: &[PathBuf], host: &Path) -> Result<Vec<PathBuf>> {
    let text = manifest_json(host);
    let mut written = Vec::new();
    for dir in dirs {
        std::fs::create_dir_all(dir).with_context(|| format!("could not create {}", dir.display()))?;
        let file = dir.join(format!("{HOST_NAME}.json"));
        std::fs::write(&file, &text).with_context(|| format!("could not write {}", file.display()))?;
        written.push(file);
    }
    Ok(written)
}

/// Puts the extension's files in `dir`, replacing the ones nexo wrote before.
fn write_extension(dir: &Path) -> Result<()> {
    std::fs::create_dir_all(dir).with_context(|| format!("could not create {}", dir.display()))?;
    for (name, text) in EXTENSION_FILES {
        std::fs::write(dir.join(name), text).with_context(|| format!("could not write {name} in {}", dir.display()))?;
    }
    Ok(())
}

pub fn run(a: &ChromeSetupArgs) -> i32 {
    match execute(a) {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("nexo chrome-setup: {e:#}");
            1
        }
    }
}

fn execute(a: &ChromeSetupArgs) -> Result<()> {
    let Some(host) = find_host() else {
        bail!("{HOST_BIN} is not installed next to nexo or on PATH (build it with `cargo build -p nexo-browser-host`)");
    };
    let dirs = manifest_dirs();
    if dirs.is_empty() {
        bail!("this system has no Chrome native-messaging folder nexo knows");
    }
    let manifests = write_manifests(&dirs, &host)?;
    for m in &manifests {
        register(m)?;
    }
    let ext = nexo_core::paths::data_dir().join("chrome-extension");
    write_extension(&ext)?;
    println!("Wrote {} manifest(s) for {HOST_NAME}, host {}", manifests.len(), host.display());
    println!("In Chrome, on chrome://extensions: 1) turn on Developer mode (top right)");
    println!("2) click Load unpacked and pick {}", ext.display());
    println!("3) click the nexo browser button in the toolbar: it connects to nexo and you can allow it");
    if a.no_open {
        return Ok(());
    }
    match find_browser() {
        Some(chrome) => {
            Command::new(&chrome)
                .arg("chrome://extensions")
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .with_context(|| format!("could not start {}", chrome.display()))?;
        }
        None => println!("No Chrome, Chromium or Edge found on PATH: open chrome://extensions yourself."),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine;
    use sha2::{Digest, Sha256};

    #[test]
    fn extension_id_is_the_one_its_key_gives() {
        let manifest: serde_json::Value = serde_json::from_str(EXTENSION_FILES[0].1).unwrap();
        let der = base64::engine::general_purpose::STANDARD.decode(manifest["key"].as_str().unwrap()).unwrap();
        let id: String =
            Sha256::digest(&der)[..16].iter().flat_map(|b| [b >> 4, b & 15]).map(|n| (b'a' + n) as char).collect();
        assert_eq!(id, EXTENSION_ID);
    }

    #[test]
    fn extension_asks_for_what_the_design_needs() {
        let m: serde_json::Value = serde_json::from_str(EXTENSION_FILES[0].1).unwrap();
        assert_eq!(m["manifest_version"], 3);
        assert_eq!(m["background"]["service_worker"], "background.js");
        assert!(m["action"].is_object(), "background.js listens to chrome.action");
        let perms: Vec<&str> = m["permissions"].as_array().unwrap().iter().filter_map(|p| p.as_str()).collect();
        for p in ["nativeMessaging", "scripting", "tabs", "activeTab"] {
            assert!(perms.contains(&p), "{p}");
        }
        assert_eq!(m["host_permissions"][0], "<all_urls>");
        assert!(EXTENSION_FILES[1].1.contains(&format!("\"{HOST_NAME}\"")));
    }

    #[test]
    fn manifest_names_the_host_and_only_our_extension() {
        let m: serde_json::Value =
            serde_json::from_str(&manifest_json(Path::new("/opt/nexo/nexo-browser-host"))).unwrap();
        assert_eq!(m["name"], HOST_NAME);
        assert_eq!(m["type"], "stdio");
        assert_eq!(m["path"], "/opt/nexo/nexo-browser-host");
        assert_eq!(m["allowed_origins"], json!([format!("chrome-extension://{EXTENSION_ID}/")]));
    }

    #[test]
    fn manifests_and_extension_are_written_to_disk() {
        let tmp = tempfile::tempdir().unwrap();
        let dirs = [tmp.path().join("a/NativeMessagingHosts"), tmp.path().join("b/NativeMessagingHosts")];
        let written = write_manifests(&dirs, Path::new("/x/host")).unwrap();
        assert_eq!(written.len(), 2);
        for f in &written {
            assert!(f.ends_with(format!("{HOST_NAME}.json")));
            assert!(std::fs::read_to_string(f).unwrap().contains("/x/host"));
        }
        let ext = tmp.path().join("ext");
        write_extension(&ext).unwrap();
        for (name, text) in EXTENSION_FILES {
            assert_eq!(std::fs::read_to_string(ext.join(name)).unwrap(), text);
        }
    }
}
