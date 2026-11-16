//! The sandbox settings of a session: what the `bash` tool may touch.
//!
//! This file holds only the settings and the list of places a sandboxed command may write. The OS
//! code that enforces them lives in `nexo-tools` (`sandbox.rs`), behind `cfg` gates per platform.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SandboxMode {
    /// Commands run with the rights of the person (the permission rules are the only guard).
    #[default]
    Off,
    /// Commands read the whole disk but write only the project, the added directories, the temp
    /// directory and the caches of the language tools.
    WorkspaceWrite,
    /// Commands read the whole disk and write nothing but the temp directory and `/dev`.
    ReadOnly,
}

impl SandboxMode {
    pub fn as_str(&self) -> &'static str {
        match self {
            SandboxMode::Off => "off",
            SandboxMode::WorkspaceWrite => "workspace-write",
            SandboxMode::ReadOnly => "read-only",
        }
    }

    pub fn parse(s: &str) -> Option<SandboxMode> {
        match s.trim().to_ascii_lowercase().replace('_', "-").as_str() {
            "off" | "none" | "false" | "no" => Some(SandboxMode::Off),
            "workspace-write" | "workspace" | "on" | "true" | "yes" => Some(SandboxMode::WorkspaceWrite),
            "read-only" | "readonly" => Some(SandboxMode::ReadOnly),
            _ => None,
        }
    }

    pub fn is_on(&self) -> bool {
        *self != SandboxMode::Off
    }
}

/// The sandbox of a session as the tools see it, copied into each call.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct SandboxSettings {
    pub mode: SandboxMode,
    /// Whether a sandboxed command may use the network.
    pub network: bool,
    /// Directories besides the project that may be written (`--add-dir` and `sandbox_write_dirs`).
    pub extra_dirs: Vec<PathBuf>,
}

/// Caches of the language tools, relative to the home directory. A command that builds or installs
/// writes there, so they stay writable in `workspace-write`. Missing ones are skipped.
pub const CACHE_DIRS: &[&str] = &[
    ".cargo/registry",
    ".cargo/git",
    ".npm",
    ".cache/pip",
    ".cache/go-build",
    ".cache/yarn",
    ".cache/pnpm",
    ".local/share/pnpm",
    ".gradle",
    ".m2/repository",
    ".nuget/packages",
    "go/pkg",
];

/// The same caches where macOS keeps them (`~/Library/Caches` and Xcode's build folder), added to
/// `CACHE_DIRS` there. SwiftPM's `~/Library/org.swift.swiftpm` is left out: it holds its settings
/// and the package fingerprints it trusts, not only a cache.
pub const MACOS_CACHE_DIRS: &[&str] = &[
    "Library/Caches/pip",
    "Library/Caches/go-build",
    "Library/Caches/Yarn",
    "Library/Caches/pnpm",
    "Library/pnpm",
    "Library/Caches/Homebrew",
    "Library/Caches/org.swift.swiftpm",
    "Library/Caches/CocoaPods",
    "Library/Developer/Xcode/DerivedData",
];

/// Places outside the home that are always writable (device nodes and shared memory).
pub const SYSTEM_WRITE_DIRS: &[&str] = &["/dev"];

impl SandboxSettings {
    pub fn new(mode: SandboxMode, network: bool, extra_dirs: Vec<PathBuf>) -> SandboxSettings {
        SandboxSettings { mode, network, extra_dirs }
    }

    /// The directories a sandboxed command may write, as paths that exist (a root that does not
    /// exist cannot be named to the OS, and is skipped).
    pub fn write_roots(&self, cwd: &Path) -> Vec<PathBuf> {
        self.write_roots_in(cwd, dirs::home_dir().as_deref())
    }

    /// `write_roots` with the home folder given.
    fn write_roots_in(&self, cwd: &Path, home: Option<&Path>) -> Vec<PathBuf> {
        let mut out: Vec<PathBuf> = Vec::new();
        let mut add = |p: PathBuf| {
            let p = p.canonicalize().unwrap_or(p);
            if p.is_dir() && !out.contains(&p) {
                out.push(p);
            }
        };
        for d in SYSTEM_WRITE_DIRS {
            add(PathBuf::from(d));
        }
        // the system temp directory, and the usual places a tool looks for one
        add(std::env::temp_dir());
        // macOS's per-user temp folder, which `temp_dir` names only while `$TMPDIR` is set: Apple's
        // tools find it through the C library either way
        #[cfg(target_os = "macos")]
        if let Some(t) = confstr_path(libc::_CS_DARWIN_USER_TEMP_DIR) {
            add(t);
        }
        if cfg!(unix) {
            add(PathBuf::from("/tmp"));
            add(PathBuf::from("/var/tmp"));
        }
        if self.mode == SandboxMode::WorkspaceWrite {
            add(cwd.to_path_buf());
            for d in &self.extra_dirs {
                add(if d.is_absolute() { d.clone() } else { cwd.join(d) });
            }
            if let Some(home) = home {
                let mac: &[&str] = if cfg!(target_os = "macos") { MACOS_CACHE_DIRS } else { &[] };
                for c in CACHE_DIRS.iter().chain(mac) {
                    add(home.join(c));
                }
            }
            // macOS's per-user cache folder, the sibling of its temp folder: clang, swift and xcrun
            // keep their module caches there
            #[cfg(target_os = "macos")]
            if let Some(c) = confstr_path(libc::_CS_DARWIN_USER_CACHE_DIR) {
                add(c);
            }
            // where the build puts its output when the person moved it out of the project
            for var in ["CARGO_TARGET_DIR", "CARGO_HOME"] {
                if let Some(v) = std::env::var_os(var).filter(|v| !v.is_empty()) {
                    let p = PathBuf::from(v);
                    // CARGO_HOME holds the toolchain bin too: only its caches are opened
                    if var == "CARGO_HOME" {
                        add(p.join("registry"));
                        add(p.join("git"));
                    } else {
                        add(p);
                    }
                }
            }
        }
        out
    }
}

/// A path the C library reports through `confstr`, or `None` when it has none for `name`.
#[cfg(unix)]
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn confstr_path(name: libc::c_int) -> Option<PathBuf> {
    let mut buf = vec![0u8; 1024];
    // SAFETY: the buffer is writable for its whole length, and confstr writes at most that many
    // bytes, the terminating zero included; it returns the length the value needs (0: none).
    let need = unsafe { libc::confstr(name, buf.as_mut_ptr().cast(), buf.len()) };
    if need == 0 || need > buf.len() {
        return None;
    }
    buf.truncate(need - 1);
    let s = String::from_utf8(buf).ok()?;
    let s = s.trim_end_matches('/');
    (!s.is_empty()).then(|| PathBuf::from(s))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn modes_parse_and_print() {
        for m in [SandboxMode::Off, SandboxMode::WorkspaceWrite, SandboxMode::ReadOnly] {
            assert_eq!(SandboxMode::parse(m.as_str()), Some(m));
        }
        assert_eq!(SandboxMode::parse("on"), Some(SandboxMode::WorkspaceWrite));
        assert_eq!(SandboxMode::parse("Workspace_Write"), Some(SandboxMode::WorkspaceWrite));
        assert_eq!(SandboxMode::parse("sideways"), None);
    }

    #[test]
    fn roots_hold_the_project_only_in_workspace_write() {
        let d = tempfile::tempdir().unwrap();
        let cwd = d.path().canonicalize().unwrap();
        let ws = SandboxSettings::new(SandboxMode::WorkspaceWrite, false, vec![]);
        assert!(ws.write_roots(&cwd).contains(&cwd));
        let ro = SandboxSettings::new(SandboxMode::ReadOnly, false, vec![]);
        assert!(!ro.write_roots(&cwd).contains(&cwd));
    }

    #[test]
    fn added_dirs_count_only_when_they_exist() {
        let d = tempfile::tempdir().unwrap();
        let extra = tempfile::tempdir().unwrap();
        let s = SandboxSettings::new(
            SandboxMode::WorkspaceWrite,
            false,
            vec![extra.path().to_path_buf(), d.path().join("missing")],
        );
        let roots = s.write_roots(d.path());
        assert!(roots.contains(&extra.path().canonicalize().unwrap()));
        assert!(!roots.iter().any(|r| r.ends_with("missing")));
    }

    /// The macOS sandbox profile matches the path the kernel resolved (`/private/tmp`, never `/tmp`),
    /// so a root named any other way would never match.
    #[test]
    fn every_write_root_is_named_by_its_real_path() {
        let d = tempfile::tempdir().unwrap();
        let ws = SandboxSettings::new(SandboxMode::WorkspaceWrite, false, vec![d.path().to_path_buf()]);
        let roots = ws.write_roots(d.path());
        for r in &roots {
            assert_eq!(Some(r), r.canonicalize().ok().as_ref(), "{roots:?}");
        }
        if cfg!(unix) {
            assert!(roots.contains(&Path::new("/tmp").canonicalize().unwrap()), "{roots:?}");
        }
    }

    #[test]
    fn tool_caches_open_where_the_platform_keeps_them() {
        let home = tempfile::tempdir().unwrap();
        let cwd = tempfile::tempdir().unwrap();
        for c in [".cache/pip", "Library/Caches/pip", "Library/Developer/Xcode/DerivedData", "Library/Keychains"] {
            std::fs::create_dir_all(home.path().join(c)).unwrap();
        }
        let real = |c: &str| home.path().join(c).canonicalize().unwrap();
        let ws = SandboxSettings::new(SandboxMode::WorkspaceWrite, false, vec![]);
        let roots = ws.write_roots_in(cwd.path(), Some(home.path()));
        assert!(roots.contains(&real(".cache/pip")), "{roots:?}");
        let mac = cfg!(target_os = "macos");
        assert_eq!(roots.contains(&real("Library/Caches/pip")), mac, "{roots:?}");
        assert_eq!(roots.contains(&real("Library/Developer/Xcode/DerivedData")), mac, "{roots:?}");
        // only the caches: the rest of `~/Library` stays shut
        assert!(!roots.contains(&real("Library/Keychains")), "{roots:?}");
        let ro = SandboxSettings::new(SandboxMode::ReadOnly, false, vec![]);
        assert!(!ro.write_roots_in(cwd.path(), Some(home.path())).contains(&real(".cache/pip")));
    }

    #[cfg(unix)]
    #[test]
    fn confstr_paths_come_back_whole_or_not_at_all() {
        assert!(confstr_path(libc::_CS_PATH).is_some_and(|p| !p.as_os_str().is_empty()));
        assert_eq!(confstr_path(-1), None);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn the_darwin_user_folders_open_cache_in_workspace_write_and_temp_always() {
        let c = confstr_path(libc::_CS_DARWIN_USER_CACHE_DIR).expect("macOS names a user cache folder");
        let c = c.canonicalize().unwrap();
        let cwd = tempfile::tempdir().unwrap();
        let ws = SandboxSettings::new(SandboxMode::WorkspaceWrite, false, vec![]);
        assert!(ws.write_roots(cwd.path()).contains(&c));
        let ro = SandboxSettings::new(SandboxMode::ReadOnly, false, vec![]);
        let ro_roots = ro.write_roots(cwd.path());
        assert!(!ro_roots.contains(&c));
        // the temp folder is open in every mode, found with or without `$TMPDIR`
        let t = confstr_path(libc::_CS_DARWIN_USER_TEMP_DIR).expect("macOS names a user temp folder");
        assert!(ro_roots.contains(&t.canonicalize().unwrap()), "{ro_roots:?}");
    }
}
