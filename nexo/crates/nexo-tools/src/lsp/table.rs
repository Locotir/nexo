use std::path::{Path, PathBuf};
use std::process::Stdio;

/// The servers for a file, by extension: `(language id, candidate commands)`.
pub fn servers_for(path: &Path) -> Option<(&'static str, &'static [&'static [&'static str]])> {
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    Some(match ext.as_str() {
        "rs" => ("rust", &[&["rust-analyzer"]]),
        "py" | "pyi" => {
            ("python", &[&["pyright-langserver", "--stdio"], &["basedpyright-langserver", "--stdio"], &["pylsp"]])
        }
        "ts" | "tsx" => ("typescript", &[&["typescript-language-server", "--stdio"]]),
        "js" | "jsx" | "mjs" | "cjs" => ("javascript", &[&["typescript-language-server", "--stdio"]]),
        "go" => ("go", &[&["gopls"]]),
        "c" | "h" | "cc" | "cpp" | "cxx" | "hpp" => ("cpp", &[&["clangd"]]),
        _ => return None,
    })
}

/// Files that mark the root of a project.
pub(super) const MARKERS: [&str; 8] = [
    "Cargo.toml",
    "pyproject.toml",
    "setup.py",
    "package.json",
    "tsconfig.json",
    "go.mod",
    "compile_commands.json",
    ".git",
];

/// The nearest ancestor of `file` with a project marker.
pub(super) fn marker_root(file: &Path) -> Option<PathBuf> {
    file.ancestors().skip(1).find(|d| MARKERS.iter().any(|m| d.join(m).exists())).map(Path::to_path_buf)
}

/// The project root a server is started in: the nearest ancestor with a marker of the
/// language's projects, else the working directory.
pub(super) fn root_for(file: &Path, cwd: &Path) -> PathBuf {
    let mut d = file.parent();
    while let Some(dir) = d {
        if MARKERS.iter().any(|m| dir.join(m).exists()) {
            return dir.to_path_buf();
        }
        if dir == cwd {
            break;
        }
        d = dir.parent();
    }
    cwd.to_path_buf()
}

pub(super) fn which(cmd: &str) -> bool {
    nexo_core::shell::find_program(cmd).is_some()
}

/// `file:///…` for a path, Windows drive letters and spaces included.
pub(super) fn uri(p: &Path) -> String {
    reqwest::Url::from_file_path(p).map(|u| u.to_string()).unwrap_or_else(|_| format!("file://{}", p.display()))
}

/// Whether a candidate can run at all: installed, and (for `rust-analyzer`, which a rustup
/// shim stands in for when the component is missing) answering `--version`.
pub(super) fn usable(cmd: &[&str]) -> bool {
    if !which(cmd[0]) {
        return false;
    }
    if cmd[0] != "rust-analyzer" {
        return true;
    }
    let mut probe = std::process::Command::new(cmd[0]);
    if let Some(env) = crate::child_env() {
        probe.env_clear().envs(env.iter().map(|(k, v)| (k, v)));
    }
    probe
        .arg("--version")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// One source file of each language the project has under `cwd` (shallow and bounded: three levels,
/// 5000 entries, no hidden, vendored or build folders).
pub(super) fn sample_files(cwd: &Path) -> Vec<PathBuf> {
    const SKIP: [&str; 4] = ["node_modules", "target", "vendor", "dist"];
    let mut found: Vec<PathBuf> = Vec::new();
    let mut seen = 0usize;
    let mut dirs = vec![(cwd.to_path_buf(), 0usize)];
    while let Some((dir, depth)) = dirs.pop() {
        let Ok(rd) = std::fs::read_dir(&dir) else { continue };
        for e in rd.flatten() {
            seen += 1;
            if seen > 5000 {
                break;
            }
            let path = e.path();
            let name = e.file_name();
            let name = name.to_string_lossy();
            if e.file_type().is_ok_and(|t| t.is_dir()) {
                if depth < 3 && !name.starts_with('.') && !SKIP.contains(&&*name) {
                    dirs.push((path, depth + 1));
                }
            } else if let Some((language, _)) = servers_for(&path) {
                if !found.iter().any(|f| servers_for(f).is_some_and(|(l, _)| l == language)) {
                    found.push(path);
                }
            }
        }
    }
    found
}

/// Whether `diagnostics` can answer anywhere under `cwd`: a language server (or the compiler check
/// that stands in for it) exists for a language the project has. A project with no source files yet
/// counts every language. It runs once, when the tools are chosen.
pub fn available(cwd: &Path) -> bool {
    let mut found = sample_files(cwd);
    if found.is_empty() {
        found = ["a.rs", "a.py", "a.ts", "a.js", "a.go", "a.c"].iter().map(PathBuf::from).collect();
    }
    found.iter().filter_map(|f| servers_for(f)).any(|(language, candidates)| {
        // The free PATH checks come first: a rustup install ships the rust-analyzer shim with cargo, so
        // where cargo is found the `--version` spawn is skipped. `usable` still runs lazily before use.
        (language == "rust" && which("cargo"))
            || (language == "javascript" && which("node"))
            || candidates.iter().any(|c| usable(c))
    })
}
