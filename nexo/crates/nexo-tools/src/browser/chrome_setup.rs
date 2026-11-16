//! The way into the person's own Chrome, Chromium, Edge or Brave: the native-messaging manifest that lets the
//! extension start nexo as its host (`nexo chrome-extension://<id>/`), the extension's files where the browser
//! can load them, and the relay token. `nexo chrome-setup` and `/chrome` both call [`setup`]; `/chrome` also
//! asks [`inspect`] what is already in place. Everything the person's account is read from sits in [`Places`],
//! so a test can point it at a temporary folder.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use nexo_core::agent::{ChromeFiles, ChromeHostInfo, ChromeHostState, ChromeInfo, ChromeRunning};
use serde_json::json;

/// The name the extension passes to `connectNative` (see `background.js`).
pub const HOST_NAME: &str = "dev.nexo.browser";
/// The id Chrome derives from the `key` in the extension's manifest: it is the same on every machine.
pub const EXTENSION_ID: &str = "lollgkhihanglklkkahimoikiegabdon";

/// The extension's files: name and text, as this nexo carries them.
pub const EXTENSION_FILES: [(&str, &str); 4] = [
    ("manifest.json", include_str!("../../assets/chrome-extension/manifest.json")),
    ("background.js", include_str!("../../assets/chrome-extension/background.js")),
    ("consent.html", include_str!("../../assets/chrome-extension/consent.html")),
    ("consent.js", include_str!("../../assets/chrome-extension/consent.js")),
];

/// A browser that reads native-messaging manifests. An empty folder name means the browser has no build for that
/// system (Arc is macOS only, the Canary channels have no Linux build) or nexo does not set it up there.
#[allow(dead_code)] // each system reads the fields of its own
pub struct Browser {
    pub name: &'static str,
    /// Programs looked up along `PATH` to see if it is installed, and to open it.
    bins: &'static [&'static str],
    /// Its application bundle's name under `/Applications` (macOS).
    app: &'static str,
    /// Its folder under `~/.config` (Linux).
    linux: &'static str,
    /// Its folder under `~/Library/Application Support` (macOS).
    mac: &'static str,
    /// Its key under `HKCU` (Windows).
    win: &'static str,
}

impl Browser {
    /// Whether this browser exists on the system nexo runs on.
    fn exists_here(&self) -> bool {
        let folder = if cfg!(windows) {
            self.win
        } else if cfg!(target_os = "macos") {
            self.mac
        } else {
            self.linux
        };
        !folder.is_empty()
    }
}

/// In order of preference: the four stable browsers first, then Arc and the pre-release channels. The Windows
/// registry keys of the pre-release channels are not verified, so they are left out there.
pub const BROWSERS: [Browser; 11] = [
    Browser {
        name: "Google Chrome",
        bins: &["google-chrome-stable", "google-chrome"],
        app: "Google Chrome",
        linux: "google-chrome",
        mac: "Google/Chrome",
        win: r"Software\Google\Chrome",
    },
    Browser {
        name: "Chromium",
        bins: &["chromium", "chromium-browser"],
        app: "Chromium",
        linux: "chromium",
        mac: "Chromium",
        win: r"Software\Chromium",
    },
    Browser {
        name: "Microsoft Edge",
        bins: &["microsoft-edge-stable", "microsoft-edge"],
        app: "Microsoft Edge",
        linux: "microsoft-edge",
        mac: "Microsoft Edge",
        win: r"Software\Microsoft\Edge",
    },
    Browser {
        name: "Brave",
        bins: &["brave-browser", "brave"],
        app: "Brave Browser",
        linux: "BraveSoftware/Brave-Browser",
        mac: "BraveSoftware/Brave-Browser",
        win: r"Software\BraveSoftware\Brave-Browser",
    },
    Browser { name: "Arc", bins: &[], app: "Arc", linux: "", mac: "Arc/User Data", win: "" },
    Browser {
        name: "Google Chrome Beta",
        bins: &["google-chrome-beta"],
        app: "Google Chrome Beta",
        linux: "google-chrome-beta",
        mac: "Google/Chrome Beta",
        win: "",
    },
    Browser {
        name: "Google Chrome Dev",
        bins: &["google-chrome-unstable"],
        app: "Google Chrome Dev",
        linux: "google-chrome-unstable",
        mac: "Google/Chrome Dev",
        win: "",
    },
    Browser {
        name: "Google Chrome Canary",
        bins: &[],
        app: "Google Chrome Canary",
        linux: "",
        mac: "Google/Chrome Canary",
        win: "",
    },
    Browser {
        name: "Microsoft Edge Beta",
        bins: &["microsoft-edge-beta"],
        app: "Microsoft Edge Beta",
        linux: "microsoft-edge-beta",
        mac: "Microsoft Edge Beta",
        win: "",
    },
    Browser {
        name: "Microsoft Edge Dev",
        bins: &["microsoft-edge-dev"],
        app: "Microsoft Edge Dev",
        linux: "microsoft-edge-dev",
        mac: "Microsoft Edge Dev",
        win: "",
    },
    Browser {
        name: "Microsoft Edge Canary",
        bins: &[],
        app: "Microsoft Edge Canary",
        linux: "",
        mac: "Microsoft Edge Canary",
        win: "",
    },
];

/// Where everything lives for one account.
#[derive(Debug, Clone)]
pub struct Places {
    /// The folder the browsers' profile folders are under (`~/.config`, `~/Library/Application Support`).
    #[cfg_attr(windows, allow(dead_code))]
    pub config_dir: Option<PathBuf>,
    /// Windows: the one folder the manifest is written to (the registry entries point at it).
    #[cfg_attr(not(windows), allow(dead_code))]
    pub manifest_dir: PathBuf,
    /// Where the extension's files go: the folder the person picks in "Load unpacked".
    pub extension_dir: PathBuf,
    /// The nexo program the browser will start as the host.
    pub host: PathBuf,
    /// The relay's folders, where the token is made.
    pub relay: nexo_relay::Dirs,
    /// Windows: write the registry entries too (off in tests).
    #[cfg_attr(not(windows), allow(dead_code))]
    pub registry: bool,
}

/// The folder the extension's files are written to.
pub fn extension_dir() -> PathBuf {
    nexo_core::paths::data_dir().join("chrome-extension")
}

impl Places {
    /// This account's own places, with the running nexo as the host.
    pub fn user() -> Result<Places, String> {
        let host = std::env::current_exe().map_err(|e| format!("could not find the nexo program: {e}"))?;
        Ok(Places {
            config_dir: dirs::config_dir(),
            manifest_dir: nexo_core::paths::data_dir().join("chrome-host"),
            extension_dir: extension_dir(),
            host,
            relay: nexo_relay::Dirs::for_user(),
            registry: true,
        })
    }
}

/// What [`setup`] did.
#[derive(Debug, Clone)]
pub struct Setup {
    /// The browsers set up.
    pub browsers: Vec<&'static str>,
    pub host: PathBuf,
    pub extension_dir: PathBuf,
    /// The first of those browsers that was found on `PATH` (on macOS its `.app` folder): it can be opened on
    /// `chrome://extensions`.
    pub opener: Option<PathBuf>,
    /// What the person should know that setup could not fix by itself.
    pub notes: Vec<String>,
}

/// The text of the native-messaging manifest for a host at `host`: the nexo program itself, which runs as the host
/// when the browser starts it with the extension's origin as its argument.
pub fn manifest_json(host: &Path) -> String {
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

/// The application bundle of `b` in the first of `roots` that has it (macOS: `/Applications`, `~/Applications`).
fn find_app(b: &Browser, roots: &[PathBuf]) -> Option<PathBuf> {
    if b.app.is_empty() {
        return None;
    }
    roots.iter().map(|r| r.join(format!("{}.app", b.app))).find(|p| p.is_dir())
}

/// Where macOS keeps applications for this account.
fn app_roots() -> Vec<PathBuf> {
    std::iter::once(PathBuf::from("/Applications")).chain(dirs::home_dir().map(|h| h.join("Applications"))).collect()
}

/// A program of `b` found along `PATH`, or on macOS its application bundle (the browsers there are not on `PATH`).
fn find_bin(b: &Browser) -> Option<PathBuf> {
    if cfg!(target_os = "macos") {
        if let Some(app) = find_app(b, &app_roots()) {
            return Some(app);
        }
    }
    let path = std::env::var_os("PATH")?;
    let dirs: Vec<PathBuf> = std::env::split_paths(&path).collect();
    b.bins.iter().find_map(|name| {
        let file = format!("{name}{}", std::env::consts::EXE_SUFFIX);
        dirs.iter().map(|d| d.join(&file)).find(|p| p.is_file())
    })
}

/// The browsers to set up: those `present` says are on this machine, else Chrome alone.
pub fn choose(present: impl Fn(&Browser) -> bool) -> Vec<&'static Browser> {
    let found: Vec<&Browser> = BROWSERS.iter().filter(|b| b.exists_here() && present(b)).collect();
    if found.is_empty() {
        vec![&BROWSERS[0]]
    } else {
        found
    }
}

/// Whether `b` is installed: a program on `PATH` or a profile folder of its own.
#[cfg(not(windows))]
fn installed(b: &Browser, p: &Places) -> bool {
    let profile = p.config_dir.as_deref().map(|c| manifest_dir(b, c).parent().is_some_and(Path::is_dir));
    find_bin(b).is_some() || profile.unwrap_or(false)
}

/// Whether `b` has run on this account: its key under `HKCU` exists.
#[cfg(windows)]
fn installed(b: &Browser, _p: &Places) -> bool {
    Command::new("reg")
        .args(["query", &format!(r"HKCU\{}", b.win)])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
        || find_bin(b).is_some()
}

/// The browsers on this machine, or Chrome alone when there is none.
pub fn present(p: &Places) -> Vec<&'static Browser> {
    choose(|b| installed(b, p))
}

/// The file `b`'s manifest is (or will be) in.
#[cfg(not(windows))]
fn manifest_file(b: &Browser, p: &Places) -> Option<PathBuf> {
    Some(manifest_dir(b, p.config_dir.as_deref()?).join(format!("{HOST_NAME}.json")))
}

/// On Windows the manifest can live anywhere: the registry entry of the browser names it.
#[cfg(windows)]
fn manifest_file(b: &Browser, _p: &Places) -> Option<PathBuf> {
    let key = format!(r"HKCU\{}\NativeMessagingHosts\{HOST_NAME}", b.win);
    let out = Command::new("reg").args(["query", &key, "/ve"]).stderr(Stdio::null()).output().ok()?;
    if !out.status.success() {
        return None;
    }
    registry_value(&String::from_utf8_lossy(&out.stdout)).map(PathBuf::from)
}

/// The data of the `(Default)` value in the output of `reg query ... /ve`.
#[cfg_attr(not(windows), allow(dead_code))]
fn registry_value(out: &str) -> Option<String> {
    out.lines().find_map(|l| {
        let (_, data) = l.split_once("REG_SZ")?;
        let data = data.trim();
        (!data.is_empty()).then(|| data.to_string())
    })
}

/// Writes `host`'s manifest into each folder. Returns the first file written.
#[cfg(not(windows))]
fn write_manifests(browsers: &[&Browser], p: &Places) -> Result<PathBuf, String> {
    let Some(base) = p.config_dir.as_deref() else { return Err("this account has no configuration folder".into()) };
    let dirs: Vec<PathBuf> = browsers.iter().map(|b| manifest_dir(b, base)).collect();
    write_manifest_files(&dirs, &p.host).map(|mut w| w.remove(0))
}

#[cfg(windows)]
fn write_manifests(_browsers: &[&Browser], p: &Places) -> Result<PathBuf, String> {
    write_manifest_files(std::slice::from_ref(&p.manifest_dir), &p.host).map(|mut w| w.remove(0))
}

fn write_manifest_files(dirs: &[PathBuf], host: &Path) -> Result<Vec<PathBuf>, String> {
    let text = manifest_json(host);
    let mut written = Vec::new();
    for dir in dirs {
        std::fs::create_dir_all(dir).map_err(|e| format!("could not create {}: {e}", dir.display()))?;
        let file = dir.join(format!("{HOST_NAME}.json"));
        std::fs::write(&file, &text).map_err(|e| format!("could not write {}: {e}", file.display()))?;
        written.push(file);
    }
    Ok(written)
}

/// Windows only: `HKCU\<browser>\NativeMessagingHosts\<name>` holds the manifest's path.
#[cfg(windows)]
fn register(browsers: &[&Browser], manifest: &Path) -> Result<(), String> {
    for b in browsers {
        let key = format!(r"HKCU\{}\NativeMessagingHosts\{HOST_NAME}", b.win);
        let status = Command::new("reg")
            .args(["add", &key, "/ve", "/t", "REG_SZ", "/d"])
            .arg(manifest)
            .arg("/f")
            .stdout(Stdio::null())
            .status()
            .map_err(|e| format!("could not run `reg`: {e}"))?;
        if !status.success() {
            return Err(format!("`reg add {key}` failed"));
        }
    }
    Ok(())
}

#[cfg(not(windows))]
fn register(_browsers: &[&Browser], _manifest: &Path) -> Result<(), String> {
    Ok(())
}

/// Puts the extension's files in `dir`, replacing the ones nexo wrote before.
pub fn write_extension(dir: &Path) -> Result<(), String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("could not create {}: {e}", dir.display()))?;
    for (name, text) in EXTENSION_FILES {
        std::fs::write(dir.join(name), text)
            .map_err(|e| format!("could not write {name} in {}: {e}", dir.display()))?;
    }
    Ok(())
}

/// Writes the manifest for each browser found (and, on Windows, registers it), makes the relay token and puts the
/// extension's files in place. It does not open the browser (see [`open_extensions`]).
pub fn setup(p: &Places) -> Result<Setup, String> {
    let browsers = present(p);
    let first = write_manifests(&browsers, p)?;
    if p.registry {
        register(&browsers, &first)?;
    }
    // The agents and the host share a random token in a file only this user can read: make it now so the first
    // connection finds it.
    nexo_relay::token::load_or_create(&p.relay).map_err(|e| format!("could not create the nexo browser token: {e}"))?;
    write_extension(&p.extension_dir)?;
    Ok(Setup {
        browsers: browsers.iter().map(|b| b.name).collect(),
        host: p.host.clone(),
        extension_dir: p.extension_dir.clone(),
        opener: browsers.iter().find_map(|b| find_bin(b)),
        notes: quarantine_note(&p.host).into_iter().collect(),
    })
}

/// The command that opens `bin` on the extensions page: an application bundle (macOS) goes through `open -a`,
/// since the program inside it is not meant to be started by hand.
fn opener_command(bin: &Path) -> Command {
    if bin.extension().is_some_and(|e| e == "app") {
        let mut c = Command::new("open");
        c.arg("-a").arg(bin).arg("chrome://extensions");
        c
    } else {
        let mut c = Command::new(bin);
        c.arg("chrome://extensions");
        c
    }
}

/// macOS: a program downloaded with a browser carries the quarantine mark, and the browser may then refuse to
/// start it as the native host. The note names the exact command that lifts it; nexo does not run it silently.
fn quarantine_note(host: &Path) -> Option<String> {
    if !cfg!(target_os = "macos") {
        return None;
    }
    let marked = Command::new("xattr")
        .args(["-p", "com.apple.quarantine"])
        .arg(host)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success());
    marked.then(|| quarantine_text(host))
}

#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn quarantine_text(host: &Path) -> String {
    format!(
        "macOS marked {} as downloaded, so the browser may refuse to start it. If the extension says the host \
         exited, run: xattr -d com.apple.quarantine \"{}\"",
        host.display(),
        host.display()
    )
}

/// How to reach the extension's folder in a macOS file dialog, which hides folders that start with a dot.
pub const MAC_FOLDER_HINT: &str =
    "In the file dialog press Cmd+Shift+G and paste the folder's path (the dialog hides folders that start with a dot).";

/// macOS: puts `dir` on the clipboard, best effort. Returns whether it worked; false on every other system.
pub fn copy_folder_on_mac(dir: &Path) -> bool {
    if !cfg!(target_os = "macos") {
        return false;
    }
    use std::io::Write;
    let spawned = Command::new("pbcopy").stdin(Stdio::piped()).stdout(Stdio::null()).stderr(Stdio::null()).spawn();
    let Ok(mut child) = spawned else { return false };
    let wrote = child.stdin.take().is_some_and(|mut i| i.write_all(dir.to_string_lossy().as_bytes()).is_ok());
    wrote && child.wait().is_ok_and(|s| s.success())
}

/// Opens the browser on `chrome://extensions`. `Ok(false)`: no browser program was found to open.
pub fn open_extensions(s: &Setup) -> Result<bool, String> {
    let Some(bin) = &s.opener else { return Ok(false) };
    opener_command(bin)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("could not start {}: {e}", bin.display()))?;
    Ok(true)
}

/// Whether the extension's files in `dir` are the ones this nexo carries.
pub fn extension_files(dir: &Path) -> ChromeFiles {
    let found: Vec<Option<String>> =
        EXTENSION_FILES.iter().map(|(name, _)| std::fs::read_to_string(dir.join(name)).ok()).collect();
    if found.iter().all(Option::is_none) {
        ChromeFiles::Missing
    } else if EXTENSION_FILES.iter().zip(&found).all(|((_, text), f)| f.as_deref() == Some(*text)) {
        ChromeFiles::Current
    } else {
        ChromeFiles::Stale
    }
}

/// Whether the manifest `file` points at `host`.
fn host_state(file: Option<PathBuf>, host: &Path) -> ChromeHostState {
    let Some(text) = file.and_then(|f| std::fs::read_to_string(f).ok()) else { return ChromeHostState::Missing };
    let Some(path) = serde_json::from_str::<serde_json::Value>(&text)
        .ok()
        .and_then(|v| v.get("path").and_then(|p| p.as_str()).map(str::to_string))
    else {
        return ChromeHostState::Missing;
    };
    // a link or a relative spelling of the same file counts; two paths that do not exist are not "the same"
    let same = path == host.to_string_lossy()
        || matches!((Path::new(&path).canonicalize(), host.canonicalize()), (Ok(a), Ok(b)) if a == b);
    if same {
        ChromeHostState::Ready
    } else {
        ChromeHostState::OldPath(path)
    }
}

/// What is in place for each browser on this machine, and the extension's files. Nothing about running browsers
/// (`running`, `not_answering` and `in_use` are for the caller to fill in).
pub fn inspect(p: &Places) -> ChromeInfo {
    ChromeInfo {
        hosts: present(p)
            .into_iter()
            .map(|b| ChromeHostInfo { browser: b.name.into(), state: host_state(manifest_file(b, p), &p.host) })
            .collect(),
        extension_dir: p.extension_dir.clone(),
        files: extension_files(&p.extension_dir),
        running: Vec::new(),
        not_answering: Vec::new(),
        in_use: None,
        just_set_up: false,
    }
}

/// [`inspect`], plus the browsers that answer right now (see `nexo_browser::AnyBrowser::available`; nothing is asked
/// of them or controlled) and what this session is using.
pub async fn check(p: &Places, in_use: Option<String>) -> ChromeInfo {
    let places = p.clone();
    let mut info = tokio::task::spawn_blocking(move || inspect(&places)).await.unwrap_or_else(|_| inspect(p));
    let (found, bad) = nexo_browser::AnyBrowser::available().await;
    info.running = found.into_iter().map(|b| ChromeRunning { id: b.id, name: b.name }).collect();
    info.not_answering = bad;
    info.in_use = in_use;
    info
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine;
    use sha2::{Digest, Sha256};

    /// Places inside `root`, with Chromium's profile folder there so it counts as installed.
    #[cfg(not(windows))]
    fn places(root: &Path) -> Places {
        let config = root.join("config");
        #[cfg(not(windows))]
        std::fs::create_dir_all(manifest_dir(&BROWSERS[1], &config).parent().unwrap()).unwrap();
        Places {
            config_dir: Some(config),
            manifest_dir: root.join("host"),
            extension_dir: root.join("data/chrome-extension"),
            host: root.join("bin/nexo"),
            relay: nexo_relay::Dirs::new(root.join("relay-data"), root.join("relay-run")),
            registry: false,
        }
    }

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
        let here: Vec<&Browser> = BROWSERS.iter().filter(|b| b.exists_here()).collect();
        let all: Vec<PathBuf> = here.iter().map(|b| manifest_dir(b, base)).collect();
        for d in &all {
            assert!(d.starts_with("/cfg") && d.ends_with("NativeMessagingHosts"), "{}", d.display());
        }
        let mut sorted = all.clone();
        sorted.sort();
        sorted.dedup();
        assert_eq!(sorted.len(), here.len(), "no two browsers share a folder");
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
    fn registry_output_gives_the_manifest_path() {
        let out = "\r\nHKEY_CURRENT_USER\\Software\\Google\\Chrome\\NativeMessagingHosts\\dev.nexo.browser\r\n    \
                   (Default)    REG_SZ    C:\\Users\\a b\\nexo\\chrome-host\\dev.nexo.browser.json\r\n\r\n";
        assert_eq!(registry_value(out).as_deref(), Some(r"C:\Users\a b\nexo\chrome-host\dev.nexo.browser.json"));
        assert_eq!(registry_value("ERROR: not found"), None);
    }

    #[cfg(not(windows))]
    #[test]
    fn set_up_writes_the_manifest_and_the_extension_into_the_places_it_is_given() {
        let tmp = tempfile::tempdir().unwrap();
        let p = places(tmp.path());
        let done = setup(&p).unwrap();
        assert!(done.browsers.contains(&"Chromium"), "{:?}", done.browsers);
        let file = manifest_file(&BROWSERS[1], &p).unwrap();
        let m: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
        assert_eq!(m["path"], p.host.to_string_lossy().as_ref());
        for (name, text) in EXTENSION_FILES {
            assert_eq!(std::fs::read_to_string(p.extension_dir.join(name)).unwrap(), text);
        }
        assert!(nexo_relay::token::path(&p.relay).is_file(), "the relay token is made");
        assert_eq!(done.extension_dir, p.extension_dir);
    }

    #[cfg(not(windows))]
    #[test]
    fn inspect_tells_nothing_set_up_from_set_up_and_from_a_moved_nexo() {
        let tmp = tempfile::tempdir().unwrap();
        let p = places(tmp.path());
        let before = inspect(&p);
        let chromium = |i: &ChromeInfo| i.hosts.iter().find(|h| h.browser == "Chromium").map(|h| h.state.clone());
        assert_eq!(chromium(&before), Some(ChromeHostState::Missing));
        assert_eq!(before.files, ChromeFiles::Missing);
        setup(&p).unwrap();
        let after = inspect(&p);
        assert_eq!(chromium(&after), Some(ChromeHostState::Ready));
        assert_eq!(after.files, ChromeFiles::Current);
        // nexo moved: the manifest names the old path
        let moved = Places { host: tmp.path().join("elsewhere/nexo"), ..p.clone() };
        assert_eq!(chromium(&inspect(&moved)), Some(ChromeHostState::OldPath(p.host.to_string_lossy().into_owned())));
        // an extension from another version
        std::fs::write(p.extension_dir.join("background.js"), "old").unwrap();
        assert_eq!(inspect(&p).files, ChromeFiles::Stale);
    }

    #[test]
    fn a_browser_that_only_exists_in_the_applications_folder_is_found_there() {
        let tmp = tempfile::tempdir().unwrap();
        let apps = tmp.path().join("Applications");
        std::fs::create_dir_all(apps.join("Microsoft Edge.app")).unwrap();
        let roots = vec![tmp.path().join("none"), apps.clone()];
        let edge = BROWSERS.iter().find(|b| b.name == "Microsoft Edge").unwrap();
        let chrome = &BROWSERS[0];
        assert_eq!(find_app(edge, &roots), Some(apps.join("Microsoft Edge.app")));
        assert_eq!(find_app(chrome, &roots), None);
        // Edge is chosen and Chrome is not, though no profile folder exists yet
        let chosen = choose(|b| find_app(b, &roots).is_some());
        assert_eq!(chosen.iter().map(|b| b.name).collect::<Vec<_>>(), ["Microsoft Edge"]);
    }

    #[test]
    fn an_application_bundle_is_opened_with_open_and_a_program_directly() {
        let c = opener_command(Path::new("/Applications/Google Chrome.app"));
        assert_eq!(c.get_program(), "open");
        let args: Vec<_> = c.get_args().collect();
        assert_eq!(args, ["-a", "/Applications/Google Chrome.app", "chrome://extensions"]);
        let c = opener_command(Path::new("/usr/bin/chromium"));
        assert_eq!(c.get_program(), "/usr/bin/chromium");
        assert_eq!(c.get_args().collect::<Vec<_>>(), ["chrome://extensions"]);
    }

    #[test]
    fn browsers_that_only_some_systems_have_are_skipped_elsewhere() {
        let by = |n: &str| BROWSERS.iter().find(|b| b.name == n).unwrap();
        assert_eq!(by("Arc").exists_here(), cfg!(target_os = "macos"));
        assert_eq!(by("Google Chrome Canary").exists_here(), cfg!(target_os = "macos"));
        assert!(by("Google Chrome").exists_here() && by("Brave").exists_here());
        // the four stable browsers stay first, so Chrome is still the fallback
        assert_eq!(
            BROWSERS.iter().take(4).map(|b| b.name).collect::<Vec<_>>(),
            ["Google Chrome", "Chromium", "Microsoft Edge", "Brave"]
        );
    }

    #[cfg(not(windows))]
    #[test]
    fn manifest_folders_of_the_macos_browsers_are_the_ones_each_reads() {
        let base = Path::new("/Users/a/Library/Application Support");
        let mac = |n: &str| {
            let b = BROWSERS.iter().find(|b| b.name == n).unwrap();
            let sub = b.mac;
            base.join(sub).join("NativeMessagingHosts")
        };
        assert!(mac("Google Chrome").ends_with("Google/Chrome/NativeMessagingHosts"));
        assert!(mac("Arc").ends_with("Arc/User Data/NativeMessagingHosts"));
        assert!(mac("Google Chrome Canary").ends_with("Google/Chrome Canary/NativeMessagingHosts"));
        assert!(mac("Google Chrome Beta").ends_with("Google/Chrome Beta/NativeMessagingHosts"));
        assert!(mac("Microsoft Edge Dev").ends_with("Microsoft Edge Dev/NativeMessagingHosts"));
        #[cfg(target_os = "macos")]
        {
            let tmp = tempfile::tempdir().unwrap();
            let p = Places { config_dir: dirs::config_dir(), ..places(tmp.path()) };
            let f = manifest_file(&BROWSERS[0], &p).unwrap();
            assert!(f.ends_with("Library/Application Support/Google/Chrome/NativeMessagingHosts/dev.nexo.browser.json"));
        }
    }

    #[test]
    fn the_quarantine_note_gives_the_exact_command() {
        let t = quarantine_text(Path::new("/Users/a/bin/nexo"));
        assert!(t.ends_with(r#"xattr -d com.apple.quarantine "/Users/a/bin/nexo""#), "{t}");
        assert!(quarantine_note(Path::new("/nonexistent/nexo")).is_none());
    }
}
