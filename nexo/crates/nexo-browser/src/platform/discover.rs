//! Finding an installed browser. The search is a pure function of an [`Env`] value, so the list for every OS can
//! be tested from any host; only [`Env::detect`] touches the real machine.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

/// Variable that names the browser executable to use, ahead of any search.
pub const OVERRIDE_VAR: &str = "NEXO_BROWSER";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Os {
    Linux,
    Windows,
    Mac,
}

impl Os {
    pub const fn current() -> Os {
        if cfg!(windows) {
            Os::Windows
        } else if cfg!(target_os = "macos") {
            Os::Mac
        } else {
            Os::Linux
        }
    }

    fn exe_suffix(self) -> &'static str {
        if self == Os::Windows {
            ".exe"
        } else {
            ""
        }
    }
}

/// What a search looks at.
#[derive(Debug, Clone)]
pub struct Env {
    pub os: Os,
    /// The directories of `PATH`.
    pub path_dirs: Vec<PathBuf>,
    /// The user's home directory.
    pub home: Option<PathBuf>,
    /// Windows: `%ProgramFiles%`, `%ProgramFiles(x86)%` and `%ProgramW6432%` as set.
    pub program_files: Vec<PathBuf>,
    /// Windows: `%LOCALAPPDATA%`, where per-user installs and Canary live.
    pub local_app_data: Option<PathBuf>,
    /// Windows: the default values of the registry `App Paths` keys, as (exe file name, full path).
    pub app_paths: Vec<(String, PathBuf)>,
}

impl Env {
    /// An environment with nothing in it, for the given OS.
    pub fn empty(os: Os) -> Env {
        Env {
            os,
            path_dirs: Vec::new(),
            home: None,
            program_files: Vec::new(),
            local_app_data: None,
            app_paths: Vec::new(),
        }
    }

    /// The real machine: `PATH`, the home directory and, on Windows, the install roots and the registry.
    pub fn detect() -> Env {
        let os = Os::current();
        let var = |k: &str| std::env::var_os(k).filter(|v| !v.is_empty()).map(PathBuf::from);
        let mut env = Env::empty(os);
        env.path_dirs = std::env::var_os("PATH").map(|p| std::env::split_paths(&p).collect()).unwrap_or_default();
        env.home = var(if os == Os::Windows { "USERPROFILE" } else { "HOME" });
        if os == Os::Windows {
            for key in ["ProgramFiles", "ProgramFiles(x86)", "ProgramW6432"] {
                if let Some(p) = var(key) {
                    if !env.program_files.contains(&p) {
                        env.program_files.push(p);
                    }
                }
            }
            if env.program_files.is_empty() {
                env.program_files = vec![PathBuf::from(r"C:\Program Files"), PathBuf::from(r"C:\Program Files (x86)")];
            }
            env.local_app_data =
                var("LOCALAPPDATA").or_else(|| env.home.as_ref().map(|h| h.join("AppData").join("Local")));
            env.app_paths = super::registry::app_paths();
        }
        env
    }
}

/// One browser that may be installed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    pub name: &'static str,
    pub path: PathBuf,
}

/// A browser found on disk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstalledBrowser {
    pub name: &'static str,
    pub path: PathBuf,
}

/// How one product is installed on each OS.
struct Product {
    name: &'static str,
    /// Names on `PATH`, without `.exe`.
    bins: &'static [&'static str],
    /// Linux: fixed locations.
    linux: &'static [&'static str],
    /// Windows: under `Program Files` and `%LOCALAPPDATA%`, with `\` separators.
    windows: &'static [&'static str],
    /// Windows: the exe whose `App Paths` registry key points at this product's stable install.
    app_path_exe: Option<&'static str>,
    /// macOS: under `/Applications` and `~/Applications`.
    mac: &'static [&'static str],
    /// Linux: Flatpak application id.
    flatpak: Option<&'static str>,
}

/// In order of preference: the stable channel of each vendor first (Edge is on every Windows machine), then
/// the beta, dev and canary channels.
const PRODUCTS: &[Product] = &[
    Product {
        name: "Google Chrome",
        bins: &["google-chrome-stable", "google-chrome"],
        linux: &["/opt/google/chrome/chrome"],
        windows: &[r"Google\Chrome\Application\chrome.exe"],
        app_path_exe: Some("chrome.exe"),
        mac: &["Google Chrome.app/Contents/MacOS/Google Chrome"],
        flatpak: Some("com.google.Chrome"),
    },
    Product {
        name: "Chromium",
        bins: &["chromium", "chromium-browser"],
        linux: &[
            "/usr/lib/chromium/chromium",
            "/usr/lib/chromium-browser/chromium-browser",
            "/usr/lib64/chromium-browser/chromium-browser",
            "/snap/bin/chromium",
        ],
        windows: &[r"Chromium\Application\chrome.exe"],
        app_path_exe: None,
        mac: &["Chromium.app/Contents/MacOS/Chromium"],
        flatpak: Some("org.chromium.Chromium"),
    },
    Product {
        name: "Microsoft Edge",
        bins: &["microsoft-edge-stable", "microsoft-edge", "msedge"],
        linux: &["/opt/microsoft/msedge/msedge"],
        windows: &[r"Microsoft\Edge\Application\msedge.exe"],
        app_path_exe: Some("msedge.exe"),
        mac: &["Microsoft Edge.app/Contents/MacOS/Microsoft Edge"],
        flatpak: Some("com.microsoft.Edge"),
    },
    Product {
        name: "Brave",
        bins: &["brave-browser-stable", "brave-browser", "brave"],
        linux: &["/opt/brave.com/brave/brave", "/snap/bin/brave"],
        windows: &[r"BraveSoftware\Brave-Browser\Application\brave.exe"],
        app_path_exe: Some("brave.exe"),
        mac: &["Brave Browser.app/Contents/MacOS/Brave Browser"],
        flatpak: Some("com.brave.Browser"),
    },
    Product {
        name: "Google Chrome Beta",
        bins: &["google-chrome-beta"],
        linux: &["/opt/google/chrome-beta/chrome"],
        windows: &[r"Google\Chrome Beta\Application\chrome.exe"],
        app_path_exe: None,
        mac: &["Google Chrome Beta.app/Contents/MacOS/Google Chrome Beta"],
        flatpak: None,
    },
    Product {
        name: "Google Chrome Dev",
        bins: &["google-chrome-unstable"],
        linux: &["/opt/google/chrome-unstable/chrome"],
        windows: &[r"Google\Chrome Dev\Application\chrome.exe"],
        app_path_exe: None,
        mac: &["Google Chrome Dev.app/Contents/MacOS/Google Chrome Dev"],
        flatpak: None,
    },
    Product {
        name: "Google Chrome Canary",
        bins: &[],
        linux: &[],
        windows: &[r"Google\Chrome SxS\Application\chrome.exe"],
        app_path_exe: None,
        mac: &["Google Chrome Canary.app/Contents/MacOS/Google Chrome Canary"],
        flatpak: None,
    },
    Product {
        name: "Microsoft Edge Beta",
        bins: &["microsoft-edge-beta"],
        linux: &["/opt/microsoft/msedge-beta/msedge"],
        windows: &[r"Microsoft\Edge Beta\Application\msedge.exe"],
        app_path_exe: None,
        mac: &["Microsoft Edge Beta.app/Contents/MacOS/Microsoft Edge Beta"],
        flatpak: None,
    },
    Product {
        name: "Microsoft Edge Dev",
        bins: &["microsoft-edge-dev"],
        linux: &["/opt/microsoft/msedge-dev/msedge"],
        windows: &[r"Microsoft\Edge Dev\Application\msedge.exe"],
        app_path_exe: None,
        mac: &["Microsoft Edge Dev.app/Contents/MacOS/Microsoft Edge Dev"],
        flatpak: None,
    },
    Product {
        name: "Microsoft Edge Canary",
        bins: &[],
        linux: &[],
        windows: &[r"Microsoft\Edge SxS\Application\msedge.exe"],
        app_path_exe: None,
        mac: &["Microsoft Edge Canary.app/Contents/MacOS/Microsoft Edge Canary"],
        flatpak: None,
    },
    Product {
        name: "Brave Beta",
        bins: &["brave-browser-beta"],
        linux: &["/opt/brave.com/brave-beta/brave"],
        windows: &[r"BraveSoftware\Brave-Browser-Beta\Application\brave.exe"],
        app_path_exe: None,
        mac: &["Brave Browser Beta.app/Contents/MacOS/Brave Browser Beta"],
        flatpak: None,
    },
    Product {
        name: "Brave Nightly",
        bins: &["brave-browser-nightly"],
        linux: &["/opt/brave.com/brave-nightly/brave"],
        windows: &[r"BraveSoftware\Brave-Browser-Nightly\Application\brave.exe"],
        app_path_exe: None,
        mac: &["Brave Browser Nightly.app/Contents/MacOS/Brave Browser Nightly"],
        flatpak: None,
    },
];

/// `root` with `rel` appended one component at a time, whatever separator `rel` uses.
fn join_rel(root: &Path, rel: &str, sep: char) -> PathBuf {
    rel.split(sep).filter(|c| !c.is_empty()).fold(root.to_path_buf(), |p, c| p.join(c))
}

/// Every place a browser may be, in the order to try them. Nothing here checks that a file exists.
pub fn candidates(env: &Env) -> Vec<Candidate> {
    let mut out: Vec<Candidate> = Vec::new();
    let mut push = |name: &'static str, path: PathBuf| {
        if !out.iter().any(|c| c.path == path) {
            out.push(Candidate { name, path });
        }
    };
    let suffix = env.os.exe_suffix();
    for p in PRODUCTS {
        match env.os {
            Os::Windows => {
                let roots = env.program_files.iter().chain(env.local_app_data.iter());
                for root in roots {
                    for rel in p.windows {
                        push(p.name, join_rel(root, rel, '\\'));
                    }
                }
                if let Some(exe) = p.app_path_exe {
                    for (_, path) in env.app_paths.iter().filter(|(name, _)| name.eq_ignore_ascii_case(exe)) {
                        push(p.name, path.clone());
                    }
                }
            }
            Os::Mac => {
                let roots = std::iter::once(PathBuf::from("/Applications"))
                    .chain(env.home.iter().map(|h| h.join("Applications")));
                for root in roots {
                    for rel in p.mac {
                        push(p.name, join_rel(&root, rel, '/'));
                    }
                }
            }
            Os::Linux => {
                for abs in p.linux {
                    push(p.name, PathBuf::from(abs));
                }
                if let Some(id) = p.flatpak {
                    push(p.name, PathBuf::from("/var/lib/flatpak/exports/bin").join(id));
                    if let Some(h) = &env.home {
                        push(p.name, h.join(".local/share/flatpak/exports/bin").join(id));
                    }
                }
            }
        }
        // On PATH: the Linux launcher names, or on Windows the exe names of the install (`chrome.exe`).
        let bins: Vec<String> = if env.os == Os::Windows {
            p.windows.iter().filter_map(|w| w.rsplit('\\').next()).map(str::to_string).collect()
        } else {
            p.bins.iter().map(|b| format!("{b}{suffix}")).collect()
        };
        for bin in &bins {
            for dir in &env.path_dirs {
                push(p.name, dir.join(bin));
            }
        }
    }
    if env.os == Os::Linux {
        // Snap and Flatpak browsers are sandboxed: they cannot always write a profile under /tmp. They come
        // last so a regular install wins, and still work with a persistent profile under the home directory.
        let sandboxed = |p: &Path| p.starts_with("/snap/") || p.components().any(|c| c.as_os_str() == "flatpak");
        let (sandboxed_list, regular): (Vec<_>, Vec<_>) = out.into_iter().partition(|c| sandboxed(&c.path));
        out = regular;
        out.extend(sandboxed_list);
    }
    out
}

/// The names of the browsers a search tries, for the "none found" message.
pub fn tried_names() -> String {
    PRODUCTS.iter().take(4).map(|p| p.name).collect::<Vec<_>>().join(", ")
}

/// Why `NEXO_BROWSER` is ignored, when it is set but does not point at a file.
pub fn override_problem() -> Option<String> {
    problem_in(std::env::var_os(OVERRIDE_VAR))
}

fn problem_in(value: Option<OsString>) -> Option<String> {
    let v = value?;
    if v.is_empty() || PathBuf::from(&v).is_file() {
        return None;
    }
    Some(format!("{OVERRIDE_VAR}={} is not a file", v.to_string_lossy()))
}

/// The browser named by `NEXO_BROWSER`, if it points at a file.
fn from_override(value: Option<OsString>) -> Option<PathBuf> {
    value.map(PathBuf::from).filter(|p| p.is_file())
}

/// Every browser installed, best first: `NEXO_BROWSER` if it names a file, then the stable channels, then the
/// pre-release ones.
pub fn installed_browsers() -> Vec<InstalledBrowser> {
    installed_in(&Env::detect(), std::env::var_os(OVERRIDE_VAR))
}

fn installed_in(env: &Env, override_value: Option<OsString>) -> Vec<InstalledBrowser> {
    let mut out: Vec<InstalledBrowser> =
        from_override(override_value).map(|path| InstalledBrowser { name: "NEXO_BROWSER", path }).into_iter().collect();
    for c in candidates(env) {
        if c.path.is_file() && !out.iter().any(|b| b.path == c.path) {
            out.push(InstalledBrowser { name: c.name, path: c.path });
        }
    }
    out
}

/// The first installed browser, if any.
pub fn find_browser() -> Option<PathBuf> {
    installed_browsers().into_iter().next().map(|b| b.path)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A path with every separator as `/`, so the same expectation holds on any host.
    fn flat(p: &Path) -> String {
        p.to_string_lossy().replace('\\', "/")
    }

    fn listed(env: &Env) -> Vec<(String, &'static str)> {
        candidates(env).into_iter().map(|c| (flat(&c.path), c.name)).collect()
    }

    fn windows_env() -> Env {
        let mut e = Env::empty(Os::Windows);
        e.program_files = vec![PathBuf::from(r"C:\Program Files"), PathBuf::from(r"C:\Program Files (x86)")];
        e.local_app_data = Some(PathBuf::from(r"C:\Users\Ana\AppData\Local"));
        e.home = Some(PathBuf::from(r"C:\Users\Ana"));
        e
    }

    #[test]
    fn windows_lists_every_channel_under_every_root() {
        let got = listed(&windows_env());
        let has = |s: &str| got.iter().any(|(p, _)| p == s);
        assert!(has("C:/Program Files/Google/Chrome/Application/chrome.exe"));
        assert!(has("C:/Program Files (x86)/Google/Chrome/Application/chrome.exe"));
        assert!(has("C:/Users/Ana/AppData/Local/Google/Chrome/Application/chrome.exe"));
        assert!(has("C:/Users/Ana/AppData/Local/Google/Chrome SxS/Application/chrome.exe"));
        assert!(has("C:/Program Files/Google/Chrome Beta/Application/chrome.exe"));
        assert!(has("C:/Program Files/Google/Chrome Dev/Application/chrome.exe"));
        assert!(has("C:/Program Files (x86)/Microsoft/Edge/Application/msedge.exe"));
        assert!(has("C:/Users/Ana/AppData/Local/Microsoft/Edge SxS/Application/msedge.exe"));
        assert!(has("C:/Program Files/BraveSoftware/Brave-Browser/Application/brave.exe"));
        assert!(has("C:/Users/Ana/AppData/Local/Chromium/Application/chrome.exe"));
        // Chrome stable is tried before Edge, Edge before any pre-release channel.
        let pos = |s: &str| got.iter().position(|(p, _)| p == s).unwrap();
        assert!(
            pos("C:/Program Files/Google/Chrome/Application/chrome.exe")
                < pos("C:/Program Files/Microsoft/Edge/Application/msedge.exe")
        );
        assert!(
            pos("C:/Program Files/Microsoft/Edge/Application/msedge.exe")
                < pos("C:/Program Files/Google/Chrome Beta/Application/chrome.exe")
        );
    }

    #[test]
    fn windows_registry_paths_slot_in_with_their_product() {
        let mut env = windows_env();
        env.app_paths = vec![
            ("chrome.exe".into(), PathBuf::from(r"D:\Apps\Chrome\chrome.exe")),
            ("MSEDGE.EXE".into(), PathBuf::from(r"D:\Apps\Edge\msedge.exe")),
            ("other.exe".into(), PathBuf::from(r"D:\Apps\other.exe")),
        ];
        let got = listed(&env);
        let pos = |s: &str| got.iter().position(|(p, _)| p == s).unwrap();
        let chrome = pos("D:/Apps/Chrome/chrome.exe");
        assert_eq!(got[chrome].1, "Google Chrome");
        assert!(chrome < pos("C:/Program Files/Microsoft/Edge/Application/msedge.exe"));
        assert!(pos("D:/Apps/Edge/msedge.exe") < pos("C:/Program Files/Google/Chrome Beta/Application/chrome.exe"));
        assert!(!got.iter().any(|(p, _)| p.contains("other.exe")));
    }

    #[test]
    fn windows_path_search_adds_the_exe_suffix() {
        let mut env = windows_env();
        env.path_dirs = vec![PathBuf::from(r"C:\Tools")];
        let got = listed(&env);
        assert!(got.iter().any(|(p, n)| p == "C:/Tools/chrome.exe" && *n == "Google Chrome"));
        assert!(got.iter().any(|(p, _)| p == "C:/Tools/msedge.exe"));
        assert!(got.iter().any(|(p, _)| p == "C:/Tools/brave.exe"));
        assert!(!got.iter().any(|(p, _)| p.ends_with("/google-chrome")));
    }

    #[test]
    fn linux_prefers_regular_installs_and_puts_sandboxed_ones_last() {
        let mut env = Env::empty(Os::Linux);
        env.path_dirs = vec![PathBuf::from("/usr/local/bin"), PathBuf::from("/snap/bin"), PathBuf::from("/usr/bin")];
        env.home = Some(PathBuf::from("/home/ana"));
        let got = listed(&env);
        let pos = |s: &str| got.iter().position(|(p, _)| p == s).unwrap();
        assert!(pos("/usr/bin/google-chrome-stable") < pos("/usr/bin/chromium"));
        assert!(pos("/usr/bin/chromium") < pos("/usr/bin/microsoft-edge"));
        assert!(pos("/opt/google/chrome/chrome") < pos("/opt/microsoft/msedge/msedge"));
        assert!(pos("/usr/bin/brave-browser") < pos("/usr/bin/google-chrome-beta"));
        // The snap launcher is on PATH here but still goes after every regular candidate.
        assert!(pos("/usr/bin/microsoft-edge-dev") < pos("/snap/bin/chromium"));
        assert!(pos("/usr/bin/microsoft-edge-dev") < pos("/var/lib/flatpak/exports/bin/com.google.Chrome"));
        assert!(got.iter().any(|(p, _)| p == "/home/ana/.local/share/flatpak/exports/bin/org.chromium.Chromium"));
        assert!(!got.iter().any(|(p, _)| p.contains(".exe")));
        assert!(!got.iter().any(|(p, _)| p.contains("Program Files")));
    }

    #[test]
    fn macos_looks_in_both_applications_folders() {
        let mut env = Env::empty(Os::Mac);
        env.home = Some(PathBuf::from("/Users/ana"));
        let got = listed(&env);
        assert_eq!(got[0].0, "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome");
        assert_eq!(got[1].0, "/Users/ana/Applications/Google Chrome.app/Contents/MacOS/Google Chrome");
        assert!(got.iter().any(|(p, _)| p == "/Applications/Microsoft Edge.app/Contents/MacOS/Microsoft Edge"));
    }

    #[test]
    fn a_candidate_appears_once() {
        let mut env = Env::empty(Os::Linux);
        env.path_dirs = vec![PathBuf::from("/usr/bin"), PathBuf::from("/usr/bin")];
        let got = listed(&env);
        let n = got.iter().filter(|(p, _)| p == "/usr/bin/chromium").count();
        assert_eq!(n, 1);
    }

    #[test]
    fn installed_browsers_keeps_only_files_and_honours_the_override() {
        let dir = tempfile::tempdir().unwrap();
        let chrome = dir.path().join("google-chrome");
        let edge = dir.path().join("microsoft-edge");
        std::fs::write(&chrome, "").unwrap();
        std::fs::write(&edge, "").unwrap();
        let mut env = Env::empty(Os::Linux);
        env.path_dirs = vec![dir.path().to_path_buf()];
        // Fixed locations such as /opt/google/chrome may exist on the machine running the test; only the
        // directory under test counts.
        let mine = |list: Vec<InstalledBrowser>| {
            list.into_iter().filter(|b| b.path.starts_with(dir.path())).collect::<Vec<_>>()
        };
        let found = mine(installed_in(&env, None));
        assert_eq!(
            found.iter().map(|b| (b.name, b.path.clone())).collect::<Vec<_>>(),
            vec![("Google Chrome", chrome.clone()), ("Microsoft Edge", edge.clone())]
        );

        let custom = dir.path().join("my-browser");
        std::fs::write(&custom, "").unwrap();
        let all = installed_in(&env, Some(custom.clone().into_os_string()));
        assert_eq!(all[0], InstalledBrowser { name: "NEXO_BROWSER", path: custom });
        assert_eq!(mine(all).len(), 3);
        // An override that is not a file is ignored.
        let all = installed_in(&env, Some(dir.path().join("missing").into_os_string()));
        assert_eq!(mine(all).len(), 2);
    }

    #[test]
    fn an_unusable_override_is_explained() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("b");
        std::fs::write(&file, "").unwrap();
        assert_eq!(problem_in(None), None);
        assert_eq!(problem_in(Some(OsString::new())), None);
        assert_eq!(problem_in(Some(file.into_os_string())), None);
        let missing = dir.path().join("missing");
        let got = problem_in(Some(missing.clone().into_os_string())).unwrap();
        assert!(got.starts_with("NEXO_BROWSER=") && got.ends_with("missing is not a file"), "{got}");
        // A directory is not a browser either.
        assert!(problem_in(Some(dir.path().as_os_str().to_owned())).is_some());
    }

    #[test]
    fn the_failure_message_names_the_main_browsers() {
        assert_eq!(tried_names(), "Google Chrome, Chromium, Microsoft Edge, Brave");
    }

    #[cfg(windows)]
    #[test]
    fn the_real_windows_environment_has_install_roots() {
        let env = Env::detect();
        assert_eq!(env.os, Os::Windows);
        assert!(!env.program_files.is_empty());
        assert!(env.local_app_data.is_some());
    }
}
