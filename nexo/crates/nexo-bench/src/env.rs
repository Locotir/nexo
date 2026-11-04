//! The environment of every program the bench starts for a run (the agent's commands, the
//! diagnostics tool, the checkers): cleared, then a short allow-list put back. Nothing that holds
//! a key, a token or a socket of the person's session reaches them.

use std::path::{Path, PathBuf};

/// The variables kept as they are, when set.
const KEEP: &[&str] = &["PATH", "LANG", "LANGUAGE", "TERM", "RUSTUP_TOOLCHAIN"];

/// What Windows needs to start any program (the profile folders are set to the run's own home).
const KEEP_WINDOWS: &[&str] = &[
    "SystemRoot",
    "windir",
    "PATHEXT",
    "ComSpec",
    "SystemDrive",
    "OS",
    "NUMBER_OF_PROCESSORS",
    "PROCESSOR_ARCHITECTURE",
    "ProgramData",
    "ProgramFiles",
    "ProgramFiles(x86)",
    "ProgramW6432",
    "CommonProgramFiles",
    "CommonProgramFiles(x86)",
    "CommonProgramW6432",
];

/// Whether the variable `name` is kept. On Windows names match without regard to case (`Path`).
fn kept(name: &str, windows: bool) -> bool {
    if !windows {
        return KEEP.contains(&name) || name.starts_with("LC_");
    }
    KEEP.iter().chain(KEEP_WINDOWS).any(|w| w.eq_ignore_ascii_case(name))
        || name.get(..3).is_some_and(|p| p.eq_ignore_ascii_case("LC_"))
}

/// The Windows profile folders, under `AppData` in a run's home.
const PROFILE: [(&str, &str); 2] = [("APPDATA", "Roaming"), ("LOCALAPPDATA", "Local")];

/// The environment of a run's program: `PATH`, `LANG`, `LC_*`, `TERM`, the Rust toolchain's
/// homes, `HOME` set to `home` and `TMPDIR` to `tmp`.
pub fn clean(home: &Path, tmp: &Path) -> Vec<(String, String)> {
    let key = std::env::var(crate::run::KEY_ENV).ok().filter(|k| !k.is_empty());
    let mut env: Vec<(String, String)> = std::env::vars_os()
        .filter_map(|(k, v)| Some((k.into_string().ok()?, v.into_string().ok()?)))
        .filter(|(k, _)| kept(k, cfg!(windows)))
        .collect();
    for (var, dir) in [("CARGO_HOME", ".cargo"), ("RUSTUP_HOME", ".rustup")] {
        if let Some(p) = toolchain_home(var, dir) {
            env.push((var.into(), p.to_string_lossy().into_owned()));
        }
    }
    env.push(("HOME".into(), home.to_string_lossy().into_owned()));
    env.push(("TMPDIR".into(), tmp.to_string_lossy().into_owned()));
    if cfg!(windows) {
        for var in ["USERPROFILE", "TEMP", "TMP"] {
            let to = if var == "USERPROFILE" { home } else { tmp };
            env.push((var.into(), to.to_string_lossy().into_owned()));
        }
        for (var, dir) in PROFILE {
            env.push((var.into(), home.join("AppData").join(dir).to_string_lossy().into_owned()));
        }
    }
    // a safeguard: whatever its name, a value that is the key stays out
    env.retain(|(_, v)| key.as_deref().is_none_or(|k| !v.contains(k)));
    env
}

/// Where the Rust toolchain keeps `var` (`CARGO_HOME`, `RUSTUP_HOME`): the variable, else `dir`
/// under the person's home when it exists. Passed on so `cargo` keeps working with another `HOME`.
fn toolchain_home(var: &str, dir: &str) -> Option<PathBuf> {
    if let Some(p) = std::env::var_os(var).filter(|p| !p.is_empty()) {
        return Some(PathBuf::from(p));
    }
    let home = std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" })?;
    Some(PathBuf::from(home).join(dir)).filter(|p| p.is_dir())
}

/// `cmd` with only `env` in its environment, the variables it set itself kept.
pub fn apply(cmd: &mut std::process::Command, env: &[(String, String)]) {
    let own: Vec<_> = cmd.get_envs().filter_map(|(k, v)| v.map(|v| (k.to_os_string(), v.to_os_string()))).collect();
    cmd.env_clear();
    cmd.envs(env.iter().map(|(k, v)| (k, v)));
    cmd.envs(own);
}

/// A home and a temporary folder of a run's own, removed when dropped.
pub struct Home {
    dir: tempfile::TempDir,
}

impl Home {
    pub fn new() -> std::io::Result<Home> {
        let dir = tempfile::Builder::new().prefix("nexo-bench-home-").tempdir()?;
        std::fs::create_dir(dir.path().join("home"))?;
        std::fs::create_dir(dir.path().join("tmp"))?;
        if cfg!(windows) {
            for (_, sub) in PROFILE {
                std::fs::create_dir_all(dir.path().join("home").join("AppData").join(sub))?;
            }
        }
        Ok(Home { dir })
    }

    /// The clean environment with this home and temporary folder.
    pub fn env(&self) -> Vec<(String, String)> {
        clean(&self.dir.path().join("home"), &self.dir.path().join("tmp"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_match_without_case_on_windows_only() {
        assert!(kept("PATH", false) && kept("LC_ALL", false));
        assert!(!kept("Path", false) && !kept("SystemRoot", false) && !kept("lc_all", false));
        for name in ["Path", "PATH", "SystemRoot", "SYSTEMROOT", "ComSpec", "ProgramFiles(x86)", "lc_all", "os"] {
            assert!(kept(name, true), "{name}");
        }
        for name in [crate::run::KEY_ENV, "HOME", "USERPROFILE", "APPDATA", "LOCALAPPDATA", "SSH_AUTH_SOCK"] {
            assert!(!kept(name, true) && !kept(name, false), "{name}");
        }
    }

    #[cfg(windows)]
    #[test]
    fn windows_keeps_what_programs_need_and_redirects_the_profile() {
        let home = Home::new().unwrap();
        let env = home.env();
        let get = |k: &str| env.iter().find(|(n, _)| n.eq_ignore_ascii_case(k)).map(|(_, v)| v.clone());
        for var in ["PATH", "SystemRoot", "ComSpec", "PATHEXT"] {
            assert!(get(var).is_some(), "{var}: {env:?}");
        }
        let h = home.dir.path().join("home");
        assert_eq!(get("USERPROFILE").unwrap(), h.to_string_lossy());
        assert!(get("APPDATA").unwrap().starts_with(&*h.to_string_lossy()));
        assert!(std::path::Path::new(&get("LOCALAPPDATA").unwrap()).is_dir());
    }

    #[test]
    fn only_the_allow_list_is_kept_and_home_is_the_runs_own() {
        std::env::set_var("NEXO_BENCH_TEST_TOKEN", "token-marker");
        let home = Home::new().unwrap();
        let env = home.env();
        assert!(env.iter().all(|(k, v)| k != "NEXO_BENCH_TEST_TOKEN" && !v.contains("token-marker")), "{env:?}");
        // Windows stores `Path`, and its names match without regard to case
        let get = |k: &str| {
            env.iter()
                .find(|(n, _)| if cfg!(windows) { n.eq_ignore_ascii_case(k) } else { n == k })
                .map(|(_, v)| v.clone())
        };
        assert_eq!(get("HOME").unwrap(), home.dir.path().join("home").to_string_lossy());
        assert_eq!(get("PATH"), std::env::var("PATH").ok());
        let mut cmd = std::process::Command::new("x");
        cmd.env("OWN", "1");
        apply(&mut cmd, &env);
        let vars: Vec<_> = cmd.get_envs().map(|(k, _)| k.to_string_lossy().into_owned()).collect();
        assert!(vars.contains(&"OWN".to_string()) && vars.contains(&"HOME".to_string()));
    }
}
