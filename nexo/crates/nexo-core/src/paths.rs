//! Where nexo keeps things: one folder, `~/.nexo`, for the settings, the
//! sessions and the cache alike (`%USERPROFILE%\.nexo` on Windows).

use std::path::{Path, PathBuf};

pub const APP: &str = "nexo";

/// `~/.nexo`: everything of the person's own lives here.
pub fn home_dir() -> PathBuf {
    dirs::home_dir().unwrap_or_else(|| PathBuf::from(".")).join(".nexo")
}

/// The person's own home folder as the environment gives it: `HOME`, or `USERPROFILE` where there is none (Windows).
pub fn user_home() -> Option<PathBuf> {
    std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")).map(PathBuf::from)
}

/// Settings: `config.toml`, `prompt.md`, `commands/`, `agents/` (`~/.nexo`).
pub fn config_dir() -> PathBuf {
    env_dir("NEXO_CONFIG_DIR").unwrap_or_else(home_dir)
}

/// What nexo writes as it works: `sessions/`, `keys/`, `spend.json`, the request history (`~/.nexo`).
pub fn data_dir() -> PathBuf {
    env_dir("NEXO_DATA_DIR").unwrap_or_else(home_dir)
}

/// What can be thrown away: `~/.nexo/cache`.
pub fn cache_dir() -> PathBuf {
    env_dir("NEXO_CACHE_DIR").unwrap_or_else(|| home_dir().join("cache"))
}

/// A path as nexo shows it: `/` between the parts on Windows too, and the person's home folder as
/// `~` (the folder of `HOME`, or of the profile on Windows).
pub fn shown(p: &Path) -> String {
    shown_from(p, dirs::home_dir().as_deref())
}

/// The same with the home folder given (tests): only a whole folder becomes `~`.
pub fn shown_from(p: &Path, home: Option<&Path>) -> String {
    let s = crate::shell::slashed(p);
    let Some(home) = home.map(crate::shell::slashed) else { return s };
    let home = home.trim_end_matches('/');
    match s.strip_prefix(home) {
        Some(rest) if !home.is_empty() && (rest.is_empty() || rest.starts_with('/')) => format!("~{rest}"),
        _ => s,
    }
}

/// Before `~/.nexo`, nexo used the XDG folders (`~/.config/nexo`, `~/.local/share/nexo`,
/// `~/.cache/nexo`; the platform's own on Windows). What is in them moves into the new
/// place, never over a file already there, and an old folder left empty is removed.
/// A folder set by `NEXO_*_DIR` is left alone. Returns the folders it moved from.
pub fn move_old_dirs() -> Vec<PathBuf> {
    // nexo never used the XDG folders on macOS, and there `dirs` points config and data into the same
    // ~/Library folder: nothing to move, and nothing to merge twice
    if cfg!(target_os = "macos") {
        return Vec::new();
    }
    let pairs = [
        ("NEXO_CONFIG_DIR", dirs::config_dir(), config_dir()),
        ("NEXO_DATA_DIR", dirs::data_dir(), data_dir()),
        ("NEXO_CACHE_DIR", dirs::cache_dir(), cache_dir()),
    ];
    let mut moved = Vec::new();
    for (var, base, new) in pairs {
        let Some(old) = base.map(|b| b.join(APP)) else { continue };
        if env_dir(var).is_some() || old == new || !old.is_dir() {
            continue;
        }
        if merge_into(&old, &new) {
            moved.push(old.clone());
        }
        let _ = std::fs::remove_dir(&old);
    }
    moved
}

/// Moves each entry of `from` into `to` unless `to` has one by that name already
/// (folders are merged the same way). True when something moved.
fn merge_into(from: &Path, to: &Path) -> bool {
    let Ok(entries) = std::fs::read_dir(from) else { return false };
    if std::fs::create_dir_all(to).is_err() {
        return false;
    }
    let mut any = false;
    for e in entries.flatten() {
        let (src, dst) = (e.path(), to.join(e.file_name()));
        if !dst.exists() {
            any |= std::fs::rename(&src, &dst).is_ok();
        } else if src.is_dir() && dst.is_dir() {
            any |= merge_into(&src, &dst);
            let _ = std::fs::remove_dir(&src);
        }
    }
    any
}

pub fn sessions_dir() -> PathBuf {
    data_dir().join("sessions")
}

pub fn user_config_file() -> PathBuf {
    config_dir().join("config.toml")
}

/// `~/.nexo/prompt.md`: the person's preferences, added after the built-in system prompt
/// (a file that starts like the built-in text replaces it instead).
pub fn prompt_file() -> PathBuf {
    config_dir().join("prompt.md")
}

/// What a new `~/.nexo/prompt.md` holds: the file adds the person's own preferences after the built-in text.
pub const PREFS_TEMPLATE: &str = "<!-- Your preferences: added after nexo's built-in prompt, which stays first (tools, subagents, how to work).
     Write who you are, your machine and stack, how you like answers. Comments like this one are not sent.
     A file that starts with \"You are nexo,\" replaces the built-in prompt instead (README: Your own system prompt). -->
";

/// The part of a prompt file nexo does not add itself: what comes before a
/// `Working directory:` line (files saved from the whole prompt carried the directory,
/// date and tool list, which go stale; nexo appends the current ones).
pub fn prompt_base(text: &str) -> &str {
    let cut = text
        .find("\nWorking directory: ")
        .map(|i| i + 1)
        .or_else(|| text.starts_with("Working directory: ").then_some(0));
    text[..cut.unwrap_or(text.len())].trim()
}

/// The project's own settings: `<root>/.nexo/config.toml`, where `<root>` is
/// the nearest ancestor of `cwd` with a `.nexo` or `.git` directory. The
/// person's own `~/.nexo` is not a project's, so the search passes over it.
pub fn project_dir(cwd: &Path) -> Option<PathBuf> {
    project_dir_in(cwd, dirs::home_dir().as_deref(), &[home_dir(), config_dir()])
}

/// The person's home folder, as every check of it reads it.
pub fn own_home() -> Option<PathBuf> {
    dirs::home_dir()
}

/// Where a path the model wrote in a tool call points: `p` against `cwd`. This one function turns every such path
/// into a file (`read`, `write`, `edit`, `ls`, `glob`, `grep` and the checks of the agent), so they all agree:
/// the text is trimmed; `~`, `$HOME` and `${HOME}`, alone or before a `/`, mean the person's home folder (and
/// before a `\` on Windows only, where a backslash separates folders; on Linux it is a file-name character);
/// on Windows Git Bash forms (`/c/x`, `/tmp/x`) are Windows paths; an absolute path is kept and every other is
/// joined to `cwd`; then `.` and `..` parts are removed without touching the file system (no link is followed,
/// nothing is refused). Other variables (`$USERPROFILE`, `%USERPROFILE%`, `$PWD`) and `~user` are not expanded.
pub fn tool_path(cwd: &Path, p: &str) -> PathBuf {
    tool_path_with(cwd, p, own_home().as_deref(), cfg!(windows), &std::env::temp_dir())
}

/// [`tool_path`] with the home folder (None when unknown: the text is then a relative path), the system and the
/// temp folder given (tests).
pub fn tool_path_with(cwd: &Path, p: &str, home: Option<&Path>, windows: bool, temp: &Path) -> PathBuf {
    let p = p.trim();
    if let (Some(home), Some(rest)) = (home, home_prefix_rest(p, windows)) {
        return normalize(&home.join(rest));
    }
    if windows {
        if let Some(native) = from_git_bash(p, temp) {
            return normalize(&native);
        }
    }
    let path = Path::new(p);
    if path.is_absolute() {
        normalize(path)
    } else {
        normalize(&cwd.join(path))
    }
}

/// What follows the home prefix of `p`, empty for the home folder itself: `~`, `$HOME` and `${HOME}`, alone or
/// before `/`, and before `\` on Windows only. Separators right after the prefix are dropped, so the rest is
/// never absolute. `~user`, `$HOMEDIR` and the like are not a home prefix.
pub fn home_prefix_rest(p: &str, windows: bool) -> Option<&str> {
    ["~", "$HOME", "${HOME}"].iter().find_map(|prefix| {
        let rest = p.strip_prefix(prefix)?;
        let sep = |c: char| c == '/' || (windows && c == '\\');
        if !rest.is_empty() && !rest.starts_with(sep) {
            return None;
        }
        Some(rest.trim_start_matches(sep))
    })
}

/// A path as Git Bash prints it, in Windows form: `/c/Users/x` is `C:/Users/x`, `/tmp/x` is
/// under `temp`. The model copies these from `pwd` and `ls` into the file tools.
pub fn from_git_bash(p: &str, temp: &Path) -> Option<PathBuf> {
    let rest = p.strip_prefix('/')?;
    let (head, tail) = rest.split_once('/').unwrap_or((rest, ""));
    if head == "tmp" {
        return Some(temp.join(tail));
    }
    match head.as_bytes() {
        [d] if d.is_ascii_alphabetic() => Some(PathBuf::from(format!("{}:/{tail}", d.to_ascii_uppercase() as char))),
        _ => None,
    }
}

/// Removes `.` and `..` components without touching the file system. A `..` with nothing before it to remove
/// (at the root) is kept: the system reads `/..` as `/`.
pub fn normalize(p: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                if !out.pop() {
                    out.push(c);
                }
            }
            _ => out.push(c),
        }
    }
    out
}

/// Whether `dir` is the person's `home` (compared resolved). A home kept in git is still the
/// person's own folder, never a project's.
pub fn is_home(dir: &Path, home: Option<&Path>) -> bool {
    let real = |p: &Path| p.canonicalize().unwrap_or_else(|_| p.to_path_buf());
    home.is_some_and(|h| real(h) == real(dir))
}

/// [`project_dir`] with the person's home and their own nexo folders given (tests). The search
/// never returns one of `own` and, from a folder below `home`, stops at `home` instead of
/// climbing out of it.
fn project_dir_in(cwd: &Path, home: Option<&Path>, own: &[PathBuf]) -> Option<PathBuf> {
    // Compared resolved: Windows may spell the same folder long or short (`RUNNER~1`).
    let real = |p: &Path| p.canonicalize().unwrap_or_else(|_| p.to_path_buf());
    let own: Vec<PathBuf> = own.iter().map(|p| real(p)).collect();
    let home = home.map(real).filter(|h| real(cwd).starts_with(h));
    let mut p = Some(cwd);
    while let Some(d) = p {
        if d.join(".nexo").is_dir() && !own.contains(&real(&d.join(".nexo"))) {
            return Some(d.join(".nexo"));
        }
        // a home kept in git (a dotfiles repo) is still not a project of its own
        if d.join(".git").exists() {
            let n = d.join(".nexo");
            return (!own.contains(&real(&n))).then_some(n);
        }
        if home.as_deref() == Some(real(d).as_path()) {
            break;
        }
        p = d.parent();
    }
    None
}

/// `~/.nexo/trusted-projects`: the projects whose own config is applied whole, one resolved
/// directory per line (`nexo config trust` adds one).
pub fn trusted_projects_file() -> PathBuf {
    config_dir().join("trusted-projects")
}

pub fn project_config_file(cwd: &Path) -> Option<PathBuf> {
    project_dir(cwd).map(|d| d.join("config.toml"))
}

/// `canonicalize`, without the `\\?\` prefix it adds on Windows (other programs, git among
/// them, cannot take it); a UNC path keeps it.
pub fn canonical(p: &Path) -> std::io::Result<PathBuf> {
    let c = p.canonicalize()?;
    if cfg!(windows) {
        let s = c.to_string_lossy();
        if let Some(rest) = s.strip_prefix(r"\\?\").filter(|r| !r.starts_with(r"UNC\")) {
            return Ok(PathBuf::from(rest));
        }
    }
    Ok(c)
}

/// Whether the disks of this platform compare file names without case: APFS by default on macOS, NTFS on Windows.
/// Every path rule and guarded-folder check folds case by this one flag, so `.GIT` is `.git` for all of them.
pub const FOLD_CASE: bool = cfg!(any(target_os = "macos", windows));

/// A file that holds keys or credentials, by where it is and what it is called: anything under
/// `.ssh`, `.gnupg`, `.aws`, `.kube`, `.docker` or `.password-store`, the macOS keychains
/// (`Library/Keychains`, `*.keychain`, `*.keychain-db`), the GitHub and Google Cloud command line
/// logins (`.config/gh`, `.config/gcloud`), `.env` files, private keys and certificates, `.netrc`
/// and its kind, and nexo's own credential store, browser token and keys. Names compare without case, as macOS
/// and Windows open them. The path as given (callers that care about links check the resolved one
/// too).
pub fn holds_keys(p: &Path) -> bool {
    const DIRS: &[&str] = &[".ssh", ".gnupg", ".aws", ".kube", ".docker", ".password-store"];
    // folders that hold keys only below this parent: a `gh` folder elsewhere is someone's project
    const PAIRS: &[[&str; 2]] = &[["library", "keychains"], [".config", "gh"], [".config", "gcloud"]];
    const FILES: &[&str] =
        &[".netrc", ".git-credentials", ".pgpass", ".npmrc", ".pypirc", "credentials", "credentials.json"];
    const KEYS: &[&str] = &["id_rsa", "id_dsa", "id_ecdsa", "id_ed25519"];
    const EXTS: &[&str] = &["pem", "key", "p12", "pfx", "jks", "keystore", "keychain", "keychain-db"];
    if p.starts_with(crate::auth::file())
        || p.starts_with(data_dir().join("keys"))
        || p.starts_with(data_dir().join("browser").join("token"))
    {
        return true;
    }
    let lower = |s: &std::ffi::OsStr| s.to_string_lossy().to_ascii_lowercase();
    let parts: Vec<String> = p.components().map(|c| lower(c.as_os_str())).collect();
    if parts.iter().any(|c| DIRS.contains(&c.as_str()))
        || parts.windows(2).any(|w| PAIRS.iter().any(|[a, b]| w[0] == *a && w[1] == *b))
    {
        return true;
    }
    let Some(name) = p.file_name().map(lower) else { return false };
    name == ".env"
        || name.starts_with(".env.")
        || FILES.contains(&name.as_str())
        || KEYS.iter().any(|k| name.starts_with(k))
        || p.extension().is_some_and(|e| EXTS.contains(&lower(e).as_str()))
}

/// A path nothing is to list, read or follow on the person's own account, beyond the files [`holds_keys`] names:
/// the folders of other programs' agents and accounts (`.pki`, `.nexo`, `.codex`, `.grok`, `.claude`), the desktop key
/// rings (`.local/share/keyrings`) and the profiles of web browsers (Chrome, Chromium, Brave, Firefox, Vivaldi, Opera,
/// Edge), which hold logins and history. Names compare without case. The path as given.
pub fn is_private(p: &Path) -> bool {
    const DIRS: &[&str] =
        &[".pki", ".nexo", ".codex", ".grok", ".claude", ".mozilla", "chromium", "bravesoftware", "brave-browser"];
    // browsers that keep their profile below a vendor folder (macOS and Windows)
    const PAIRS: &[[&str; 2]] = &[["google", "chrome"], ["microsoft", "edge"]];
    // a folder that is the stem, or the stem and a version or a channel (`google-chrome-beta`, `opera_next`)
    const FAMILIES: &[&str] =
        &["google-chrome", "chromium", "vivaldi", "opera", "microsoft-edge", "mozilla", "firefox"];
    let lower = |s: &std::ffi::OsStr| s.to_string_lossy().to_ascii_lowercase();
    if holds_keys(p) {
        return true;
    }
    let parts: Vec<String> = p.components().map(|c| lower(c.as_os_str())).collect();
    parts
        .windows(2)
        .any(|w| (w[0] == "share" && w[1] == "keyrings") || PAIRS.iter().any(|[a, b]| w[0] == *a && w[1] == *b))
        || parts.iter().any(|c| {
            DIRS.contains(&c.as_str())
                || FAMILIES
                    .iter()
                    .any(|f| c.strip_prefix(f).is_some_and(|rest| rest.is_empty() || rest.starts_with(['-', '_', ' '])))
        })
}

/// True when nexo runs as root (effective uid 0).
pub fn is_root() -> bool {
    #[cfg(unix)]
    {
        // SAFETY: geteuid has no preconditions and cannot fail.
        unsafe { libc::geteuid() == 0 }
    }
    #[cfg(not(unix))]
    {
        false
    }
}

/// Root's own home directory from the user database (`/root` on Linux).
#[cfg(unix)]
fn root_home() -> Option<PathBuf> {
    // SAFETY: getpwuid returns null or a pointer to a static entry; we copy the path out at once.
    unsafe {
        let pw = libc::getpwuid(0);
        if pw.is_null() || (*pw).pw_dir.is_null() {
            return None;
        }
        let dir = std::ffi::CStr::from_ptr((*pw).pw_dir).to_string_lossy().into_owned();
        (!dir.is_empty()).then(|| PathBuf::from(dir))
    }
}

/// Run as root, nexo lives in root's home. `sudo` often keeps the caller's `$HOME` and
/// `$XDG_*_HOME`; left alone, root would read that user's settings and leave files owned by
/// root in their home. This points `$HOME` at root's home and drops the XDG variables that
/// lie outside it, so config, sessions and `~` all resolve under `/root`.
/// Call it at the very start of `main`, before any thread exists. Returns root's home when it
/// changed something.
pub fn adopt_root_home() -> Option<PathBuf> {
    #[cfg(unix)]
    {
        if !is_root() {
            return None;
        }
        let home = root_home()?;
        let mut changed = false;
        if std::env::var_os("HOME").map(PathBuf::from).as_deref() != Some(home.as_path()) {
            // SAFETY: called before the runtime starts, while the process has one thread.
            unsafe { std::env::set_var("HOME", &home) };
            changed = true;
        }
        for var in ["XDG_CONFIG_HOME", "XDG_DATA_HOME", "XDG_CACHE_HOME", "XDG_STATE_HOME"] {
            if env_dir(var).is_some_and(|d| !d.starts_with(&home)) {
                // SAFETY: as above.
                unsafe { std::env::remove_var(var) };
                changed = true;
            }
        }
        changed.then_some(home)
    }
    #[cfg(not(unix))]
    {
        None
    }
}

fn env_dir(var: &str) -> Option<PathBuf> {
    std::env::var_os(var).filter(|v| !v.is_empty()).map(PathBuf::from)
}

/// The path with links resolved as far as it exists, the rest as written.
pub fn real_path(p: &Path) -> PathBuf {
    let mut rest = Vec::new();
    let mut base = p.to_path_buf();
    loop {
        if let Ok(c) = base.canonicalize() {
            return rest.iter().rev().fold(c, |acc, x: &std::ffi::OsString| acc.join(x));
        }
        match (base.file_name().map(|n| n.to_os_string()), base.parent().map(Path::to_path_buf)) {
            (Some(n), Some(parent)) => {
                rest.push(n);
                base = parent;
            }
            _ => return p.to_path_buf(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn other_agents_keyrings_and_browser_profiles_are_private() {
        for p in [
            "/home/a/.pki/nssdb",
            "/home/a/.nexo/sessions",
            "/home/a/.CODEX/auth.json",
            "/home/a/.grok",
            "/home/a/.claude/projects",
            "/home/a/.local/share/keyrings/login.keyring",
            "/home/a/.config/google-chrome/Default",
            "/home/a/.config/google-chrome-beta",
            "/home/a/.config/chromium",
            "/home/a/.config/BraveSoftware",
            "/home/a/.mozilla/firefox",
            "/home/a/.config/vivaldi-snapshot",
            "/home/a/.config/opera",
            "/home/a/.config/microsoft-edge",
            "/home/a/.ssh/config",
        ] {
            assert!(is_private(Path::new(p)), "{p}");
        }
        for p in [
            "/home/a/.config/quickshell/shell.qml",
            "/home/a/.local/share/applications",
            "/home/a/.config/operator",
            "/home/a/.zshrc",
        ] {
            assert!(!is_private(Path::new(p)), "{p}");
        }
    }

    #[test]
    fn a_shown_path_has_the_home_folder_as_a_tilde_and_only_a_whole_one() {
        let home = Some(Path::new("/home/ana"));
        assert_eq!(shown_from(Path::new("/home/ana/work/app"), home), "~/work/app");
        assert_eq!(shown_from(Path::new("/home/ana"), home), "~");
        assert_eq!(shown_from(Path::new("/home/anabel/x"), home), "/home/anabel/x");
        assert_eq!(shown_from(Path::new("/srv/x"), home), "/srv/x");
        assert_eq!(shown_from(Path::new("/srv/x"), None), "/srv/x");
        assert_eq!(shown_from(Path::new("/x"), Some(Path::new("/"))), "/x");
    }

    #[test]
    fn a_prompt_file_keeps_only_what_nexo_does_not_add() {
        assert_eq!(prompt_base("Mine.\n\nWorking directory: /a\nDate: 2020-01-01\n\nTools:\n- x"), "Mine.");
        assert_eq!(
            prompt_base("Mine.\nSee the working directory: it is yours."),
            "Mine.\nSee the working directory: it is yours."
        );
        assert_eq!(prompt_base("Working directory: /a"), "");
    }

    #[test]
    fn old_folders_merge_in_without_overwriting() {
        let d = tempfile::tempdir().unwrap();
        let (old, new) = (d.path().join("old"), d.path().join("new"));
        std::fs::create_dir_all(old.join("sessions")).unwrap();
        std::fs::write(old.join("config.toml"), "old").unwrap();
        std::fs::write(old.join("sessions/a.jsonl"), "a").unwrap();
        std::fs::create_dir_all(new.join("sessions")).unwrap();
        std::fs::write(new.join("prompt.md"), "kept").unwrap();
        std::fs::write(new.join("sessions/a.jsonl"), "new").unwrap();
        std::fs::write(old.join("prompt.md"), "old").unwrap();
        assert!(merge_into(&old, &new));
        assert_eq!(std::fs::read_to_string(new.join("config.toml")).unwrap(), "old");
        assert_eq!(std::fs::read_to_string(new.join("prompt.md")).unwrap(), "kept");
        assert_eq!(std::fs::read_to_string(new.join("sessions/a.jsonl")).unwrap(), "new");
    }

    #[test]
    fn the_persons_own_folder_is_not_a_project() {
        let d = tempfile::tempdir().unwrap();
        let work = d.path().join("work");
        std::fs::create_dir_all(d.path().join(".nexo")).unwrap();
        std::fs::create_dir_all(&work).unwrap();
        std::env::set_var("NEXO_CONFIG_DIR", d.path().join(".nexo"));
        assert_eq!(project_dir(&work), None);
        std::fs::create_dir_all(d.path().join(".git")).unwrap();
        assert_eq!(project_dir(&work), None, "not even when the home is kept in git");
        std::fs::remove_dir(d.path().join(".git")).unwrap();
        std::env::remove_var("NEXO_CONFIG_DIR");
        assert_eq!(project_dir(&work), Some(d.path().join(".nexo")));
    }

    fn dirs3() -> (tempfile::TempDir, PathBuf, PathBuf) {
        let d = tempfile::tempdir().unwrap();
        let home = d.path().join("home");
        let work = home.join("build-cache/x/y");
        std::fs::create_dir_all(&work).unwrap();
        std::fs::create_dir_all(home.join(".nexo")).unwrap();
        (d, home, work)
    }

    #[test]
    fn a_folder_below_the_home_does_not_find_the_homes_nexo_folder() {
        let (_d, home, work) = dirs3();
        let own = [home.join(".nexo")];
        assert_eq!(project_dir_in(&work, Some(&home), &own), None);
        assert_eq!(project_dir_in(&home, Some(&home), &own), None, "started in the home itself");
    }

    /// A link to the home does not hide it.
    #[cfg(unix)]
    #[test]
    fn a_link_to_the_home_is_the_home() {
        let (d, home, work) = dirs3();
        let own = [home.join(".nexo")];
        let link = d.path().join("link");
        std::os::unix::fs::symlink(&home, &link).unwrap();
        assert_eq!(project_dir_in(&link.join("build-cache/x/y"), Some(&home), &own), None);
        assert_eq!(project_dir_in(&work, Some(&link), &own), None);
    }

    #[test]
    fn a_project_between_the_start_and_the_home_still_wins() {
        let (_d, home, work) = dirs3();
        let own = [home.join(".nexo")];
        std::fs::create_dir_all(home.join("build-cache/x/.nexo")).unwrap();
        assert_eq!(project_dir_in(&work, Some(&home), &own), Some(home.join("build-cache/x/.nexo")));
    }

    #[test]
    fn the_search_does_not_climb_out_of_the_home() {
        let (d, home, work) = dirs3();
        std::fs::create_dir_all(d.path().join(".nexo")).unwrap();
        let own = [home.join(".nexo")];
        assert_eq!(project_dir_in(&work, Some(&home), &own), None, "a .nexo above the home is not the project's");
        // outside the home the search goes all the way up, as before
        let outside = d.path().join("srv/app");
        std::fs::create_dir_all(&outside).unwrap();
        assert_eq!(project_dir_in(&outside, Some(&home), &own), Some(d.path().join(".nexo")));
    }

    #[test]
    fn a_global_folder_set_elsewhere_is_skipped_and_the_homes_is_still_skipped() {
        let (d, home, work) = dirs3();
        let elsewhere = d.path().join("cfg/.nexo");
        std::fs::create_dir_all(&elsewhere).unwrap();
        let own = [home.join(".nexo"), elsewhere.clone()];
        assert_eq!(project_dir_in(&work, Some(&home), &own), None);
        // Outside home the search continues upward. Keep its next project inside the fixture,
        // independent of whether the runner's real home has a .nexo directory.
        std::fs::create_dir(d.path().join(".git")).unwrap();
        assert_eq!(project_dir_in(&d.path().join("cfg"), Some(&home), &own), Some(d.path().join(".nexo")));
        // with nothing declared own, the home's folder is just a folder with a name
        assert_eq!(project_dir_in(&work, None, &[]), Some(home.join(".nexo")));
    }

    #[test]
    fn keychains_and_command_line_logins_hold_keys_on_every_system() {
        for p in [
            "/Users/a/Library/Keychains/login.keychain-db",
            "/Users/a/library/keychains/x",
            "/Users/a/backup/old.keychain",
            "/srv/copy/Login.Keychain-DB",
            "/home/a/.config/gh/hosts.yml",
            "/Users/a/.config/gcloud/application_default_credentials.json",
            "/home/a/.ssh/config",
        ] {
            assert!(holds_keys(Path::new(p)), "{p}");
        }
        assert!(holds_keys(&data_dir().join("browser").join("token")), "the browser relay token");
        for p in [
            "/Users/a/Library/Fonts/x.ttf",
            "/Users/a/Library/Caches/pip/x",
            "/Users/a/code/gh/main.go",
            "/home/a/.config/ghostty/config",
            "/home/a/keychains/notes.md",
        ] {
            assert!(!holds_keys(Path::new(p)), "{p}");
        }
    }

    // `--cwd`, team folders and `/add-dir` all go through `canonical`, so the prefix must never reach them.
    #[cfg(windows)]
    #[test]
    fn canonical_drops_the_verbatim_prefix_windows_adds() {
        let d = std::env::temp_dir();
        let c = canonical(&d).unwrap();
        assert!(!c.to_string_lossy().starts_with(r"\\?\"), "{c:?}");
        assert_eq!(c.canonicalize().unwrap(), d.canonicalize().unwrap());
    }

    #[test]
    fn tool_paths_follow_one_set_of_rules() {
        let (cwd, home, temp) = (Path::new("/w/p"), Path::new("/h"), Path::new("/t"));
        let at = |p: &str, windows: bool| tool_path_with(cwd, p, Some(home), windows, temp);
        for (given, want) in [
            ("~", "/h"),
            ("~/a", "/h/a"),
            ("$HOME", "/h"),
            ("$HOME/a", "/h/a"),
            ("${HOME}", "/h"),
            ("${HOME}/a", "/h/a"),
            ("  ~/a/b.md\n", "/h/a/b.md"),
            ("~//a", "/h/a"),
            ("$HOME/a/../b", "/h/b"),
            ("~user/a", "/w/p/~user/a"),
            ("$HOMEDIR/a", "/w/p/$HOMEDIR/a"),
            ("$USERPROFILE/a", "/w/p/$USERPROFILE/a"),
            ("%USERPROFILE%\\a", "/w/p/%USERPROFILE%\\a"),
            ("~\\a", "/w/p/~\\a"),
            ("/c/x", "/c/x"),
            ("a/../b", "/w/p/b"),
            ("./a/./b.rs", "/w/p/a/b.rs"),
            ("/etc/hosts", "/etc/hosts"),
            ("/../x", "/../x"),
        ] {
            assert_eq!(at(given, false), PathBuf::from(want), "{given:?} on Linux");
        }
        // Windows: a backslash after the home prefix, and the Git Bash forms
        assert_eq!(at("~\\a", true), PathBuf::from("/h/a"));
        assert_eq!(at("$HOME\\a", true), PathBuf::from("/h/a"));
        assert_eq!(at("${HOME}\\", true), PathBuf::from("/h"));
        assert_eq!(at("~user\\a", true), PathBuf::from("/w/p/~user\\a"));
        assert_eq!(at("/c/x", true), PathBuf::from("C:/x"));
        assert_eq!(at("/tmp/a", true), PathBuf::from("/t/a"));
        assert_eq!(at("/etc/hosts", true), PathBuf::from("/etc/hosts"));
        // no home known: the text is a relative path
        assert_eq!(tool_path_with(cwd, "~/a", None, false, temp), PathBuf::from("/w/p/~/a"));
        assert_eq!(tool_path_with(cwd, "$HOME/a", None, false, temp), PathBuf::from("/w/p/$HOME/a"));
        assert_eq!(home_prefix_rest("~\\notes.txt", false), None, "a backslash is a file-name character on Linux");
        assert_eq!(home_prefix_rest("~\\notes.txt", true), Some("notes.txt"));
        assert_eq!(home_prefix_rest("~", true), Some(""));
        assert_eq!(home_prefix_rest("~x", true), None);
    }

    #[cfg(windows)]
    #[test]
    fn windows_tool_paths_take_the_drive_of_cwd_and_the_temp_folder() {
        let (cwd, temp) = (Path::new(r"C:\p"), Path::new(r"T:\temp"));
        let at = |p: &str| tool_path_with(cwd, p, Some(Path::new(r"C:\h")), true, temp);
        assert_eq!(at("/tmp"), PathBuf::from(r"T:\temp"));
        assert_eq!(at("/tmp/a"), PathBuf::from(r"T:\temp\a"));
        assert_eq!(at(r"\xy"), PathBuf::from(r"C:\xy"));
        assert_eq!(at("/xy"), PathBuf::from(r"C:\xy"));
        assert_eq!(at(r"~\a"), PathBuf::from(r"C:\h\a"));
    }
}
