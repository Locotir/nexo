//! `nexo chrome-setup`: connects nexo to the Chrome you already use and are logged in to. Chrome 136 and
//! later refuse outside debugging on the default profile, so nexo goes in through an extension instead:
//! this writes the native-messaging manifest that lets the extension start nexo itself as the host
//! (`nexo chrome-extension://<id>/`), puts the extension's files where the browser can load them, and opens
//! the browser on the page where that takes one click. One command does the lot, for Chrome, Chromium, Edge
//! and Brave, on Linux, macOS and Windows.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{bail, Context, Result};
use clap::Args;
use serde_json::json;

/// The name the extension passes to `connectNative` (see `background.js`).
const HOST_NAME: &str = "dev.nexo.browser";
/// The id Chrome derives from the `key` in the extension's manifest: it is the same on every machine.
const EXTENSION_ID: &str = "lollgkhihanglklkkahimoikiegabdon";

const EXTENSION_FILES: [(&str, &str); 4] = [
    ("manifest.json", include_str!("../assets/chrome-extension/manifest.json")),
    ("background.js", include_str!("../assets/chrome-extension/background.js")),
    ("consent.html", include_str!("../assets/chrome-extension/consent.html")),
    ("consent.js", include_str!("../assets/chrome-extension/consent.js")),
];

/// A browser that reads native-messaging manifests.
#[allow(dead_code)] // each system reads the fields of its own
struct Browser {
    name: &'static str,
    /// Programs looked up along `PATH` to see if it is installed, and to open it.
    bins: &'static [&'static str],
    /// Its folder under `~/.config` (Linux).
    linux: &'static str,
    /// Its folder under `~/Library/Application Support` (macOS).
    mac: &'static str,
    /// Its key under `HKCU` (Windows).
    win: &'static str,
}

const BROWSERS: [Browser; 4] = [
    Browser {
        name: "Google Chrome",
        bins: &["google-chrome-stable", "google-chrome"],
        linux: "google-chrome",
        mac: "Google/Chrome",
        win: r"Software\Google\Chrome",
    },
    Browser {
        name: "Chromium",
        bins: &["chromium", "chromium-browser"],
        linux: "chromium",
        mac: "Chromium",
        win: r"Software\Chromium",
    },
    Browser {
        name: "Microsoft Edge",
        bins: &["microsoft-edge-stable", "microsoft-edge"],
        linux: "microsoft-edge",
        mac: "Microsoft Edge",
        win: r"Software\Microsoft\Edge",
    },
    Browser {
        name: "Brave",
        bins: &["brave-browser", "brave"],
        linux: "BraveSoftware/Brave-Browser",
        mac: "BraveSoftware/Brave-Browser",
        win: r"Software\BraveSoftware\Brave-Browser",
    },
];

#[derive(Args, Debug)]
pub struct ChromeSetupArgs {
    /// Write the files and print the steps, but do not open the browser.
    #[arg(long)]
    pub no_open: bool,
}

/// The text of the native-messaging manifest for a host at `host`: the nexo program itself, which runs as the host
/// when the browser starts it with the extension's origin as its argument.
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

/// Where `b` reads native-messaging manifests from, given the folder its profile folder lives under.
#[cfg(not(windows))]
fn manifest_dir(b: &Browser, base: &Path) -> PathBuf {
    let sub = if cfg!(target_os = "macos") { b.mac } else { b.linux };
    base.join(sub).join("NativeMessagingHosts")
}

/// A program of `b` found along `PATH`.
fn find_bin(b: &Browser) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    let dirs: Vec<PathBuf> = std::env::split_paths(&path).collect();
    b.bins.iter().find_map(|name| {
        let file = format!("{name}{}", std::env::consts::EXE_SUFFIX);
        dirs.iter().map(|d| d.join(&file)).find(|p| p.is_file())
    })
}

/// The browsers to set up: those `present` says are on this machine, else Chrome alone.
fn choose(present: impl Fn(&Browser) -> bool) -> Vec<&'static Browser> {
    let found: Vec<&Browser> = BROWSERS.iter().filter(|b| present(b)).collect();
    if found.is_empty() {
        vec![&BROWSERS[0]]
    } else {
        found
    }
}

/// Whether `b` is installed: a program on `PATH` or a profile folder of its own.
#[cfg(not(windows))]
fn installed(b: &Browser) -> bool {
    let profile = dirs::config_dir().map(|c| manifest_dir(b, &c).parent().is_some_and(Path::is_dir));
    find_bin(b).is_some() || profile.unwrap_or(false)
}

/// Whether `b` has run on this account: its key under `HKCU` exists.
#[cfg(windows)]
fn installed(b: &Browser) -> bool {
    Command::new("reg")
        .args(["query", &format!(r"HKCU\{}", b.win)])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
        || find_bin(b).is_some()
}

/// Writes `host`'s manifest into each folder (see `manifest_dirs`). Returns the files written.
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

#[cfg(not(windows))]
fn manifest_dirs(browsers: &[&Browser]) -> Result<Vec<PathBuf>> {
    let Some(base) = dirs::config_dir() else { bail!("this account has no configuration folder") };
    Ok(browsers.iter().map(|b| manifest_dir(b, &base)).collect())
}

/// On Windows the manifest can live anywhere: the registry entries point at it.
#[cfg(windows)]
fn manifest_dirs(_browsers: &[&Browser]) -> Result<Vec<PathBuf>> {
    Ok(vec![nexo_core::paths::data_dir().join("chrome-host")])
}

/// Windows only: `HKCU\<browser>\NativeMessagingHosts\<name>` holds the manifest's path.
#[cfg(windows)]
fn register(browsers: &[&Browser], manifest: &Path) -> Result<()> {
    for b in browsers {
        let key = format!(r"HKCU\{}\NativeMessagingHosts\{HOST_NAME}", b.win);
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
    }
    Ok(())
}

#[cfg(not(windows))]
fn register(_browsers: &[&Browser], _manifest: &Path) -> Result<()> {
    Ok(())
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
    let host = std::env::current_exe().context("could not find the nexo program")?;
    let browsers = choose(installed);
    let manifests = write_manifests(&manifest_dirs(&browsers)?, &host)?;
    register(&browsers, &manifests[0])?;
    // The agents and the host share a random token in a file only this user can read: make it now so the first
    // connection finds it.
    nexo_relay::token::load_or_create(&nexo_relay::Dirs::for_user())
        .context("could not create the nexo browser token")?;
    let ext = nexo_core::paths::data_dir().join("chrome-extension");
    write_extension(&ext)?;
    let names: Vec<&str> = browsers.iter().map(|b| b.name).collect();
    println!("Set up {} for {HOST_NAME}, host {}", names.join(", "), host.display());
    println!("In the browser, on chrome://extensions: 1) turn on Developer mode (top right)");
    println!("2) click Load unpacked and pick {}", ext.display());
    println!(
        "3) click the nexo button in the toolbar. Nothing is controlled until a nexo session asks and you click Allow"
    );
    println!("Run `nexo chrome-setup` again after moving or updating nexo, and to refresh the extension's files.");
    if a.no_open {
        return Ok(());
    }
    match browsers.iter().find_map(|b| find_bin(b)) {
        Some(bin) => {
            Command::new(&bin)
                .arg("chrome://extensions")
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .with_context(|| format!("could not start {}", bin.display()))?;
        }
        None => println!("No browser program found on PATH: open chrome://extensions yourself."),
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
    fn extension_asks_for_what_the_design_needs_and_no_more() {
        let m: serde_json::Value = serde_json::from_str(EXTENSION_FILES[0].1).unwrap();
        assert_eq!(m["manifest_version"], 3);
        assert_eq!(m["background"]["service_worker"], "background.js");
        assert!(m["action"].is_object(), "background.js listens to chrome.action");
        let perms: Vec<&str> = m["permissions"].as_array().unwrap().iter().filter_map(|p| p.as_str()).collect();
        let mut want = vec!["alarms", "debugger", "nativeMessaging", "storage", "tabGroups", "tabs"];
        let mut got = perms.clone();
        want.sort_unstable();
        got.sort_unstable();
        assert_eq!(got, want);
        // no access to the content of sites except through the debugger the person sees, and no injected scripts
        assert!(m.get("host_permissions").is_none());
        assert!(m.get("content_scripts").is_none());
        assert!(EXTENSION_FILES[1].1.contains(&format!("\"{HOST_NAME}\"")));
    }

    #[test]
    fn every_page_the_extension_opens_is_shipped() {
        let background = EXTENSION_FILES[1].1;
        assert!(background.contains("consent.html"));
        let html = EXTENSION_FILES[2].1;
        assert!(html.contains("consent.js"));
        assert!(html.contains("Allow nexo to control this browser?"));
    }

    #[test]
    fn manifest_names_the_host_and_only_our_extension() {
        let m: serde_json::Value = serde_json::from_str(&manifest_json(Path::new("/opt/nexo/nexo"))).unwrap();
        assert_eq!(m["name"], HOST_NAME);
        assert_eq!(m["type"], "stdio");
        assert_eq!(m["path"], "/opt/nexo/nexo");
        assert_eq!(m["allowed_origins"], json!([format!("chrome-extension://{EXTENSION_ID}/")]));
    }

    #[cfg(not(windows))]
    #[test]
    fn each_browser_gets_its_own_folder() {
        let base = Path::new("/cfg");
        let all: Vec<PathBuf> = BROWSERS.iter().map(|b| manifest_dir(b, base)).collect();
        for d in &all {
            assert!(d.starts_with("/cfg") && d.ends_with("NativeMessagingHosts"), "{}", d.display());
        }
        let mut sorted = all.clone();
        sorted.sort();
        sorted.dedup();
        assert_eq!(sorted.len(), BROWSERS.len(), "no two browsers share a folder");
        #[cfg(target_os = "linux")]
        assert_eq!(all[3], Path::new("/cfg/BraveSoftware/Brave-Browser/NativeMessagingHosts"));
    }

    #[test]
    fn only_installed_browsers_are_set_up_and_chrome_is_the_fallback() {
        let two = choose(|b| b.name == "Chromium" || b.name == "Brave");
        assert_eq!(two.iter().map(|b| b.name).collect::<Vec<_>>(), ["Chromium", "Brave"]);
        let none = choose(|_| false);
        assert_eq!(none.len(), 1);
        assert_eq!(none[0].name, "Google Chrome");
    }

    #[test]
    fn manifests_and_extension_are_written_to_disk() {
        let tmp = tempfile::tempdir().unwrap();
        let dirs = [tmp.path().join("a/NativeMessagingHosts"), tmp.path().join("b/NativeMessagingHosts")];
        let written = write_manifests(&dirs, Path::new("/x/nexo")).unwrap();
        assert_eq!(written.len(), 2);
        for f in &written {
            assert!(f.ends_with(format!("{HOST_NAME}.json")));
            assert!(std::fs::read_to_string(f).unwrap().contains("/x/nexo"));
        }
        let ext = tmp.path().join("ext");
        write_extension(&ext).unwrap();
        for (name, text) in EXTENSION_FILES {
            assert_eq!(std::fs::read_to_string(ext.join(name)).unwrap(), text);
        }
    }
}
