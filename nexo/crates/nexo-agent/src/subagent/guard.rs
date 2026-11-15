use super::*;

/// A path argument that leaves the project: resolved as the tools resolve it (`~` expanded), with
/// links followed, so a link inside the project that points out of it counts as outside.
pub(super) fn outside(args: &Value, cwd: &Path) -> Option<String> {
    nexo_core::tool::call_paths(args).iter().find_map(|p| outside_path(p, cwd))
}

pub(super) fn outside_path(p: &str, cwd: &Path) -> Option<String> {
    // `~` is the home directory for the file tools, so it is here
    let home = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")).map(PathBuf::from);
    let p = p.trim();
    let path = match (p, home) {
        ("~", Some(h)) => h,
        (_, Some(h)) if p.starts_with("~/") => h.join(&p[2..]),
        _ => cwd.join(p),
    };
    let mut norm = PathBuf::new();
    for c in path.components() {
        match c {
            std::path::Component::ParentDir => {
                norm.pop();
            }
            std::path::Component::CurDir => {}
            other => norm.push(other),
        }
    }
    let path = norm;
    let (real, root) = (real_path(&path), real_path(cwd));
    (!real.starts_with(root)).then(|| p.to_string())
}

/// The path with links resolved as far as it exists, the rest as written.
pub(super) fn real_path(p: &Path) -> PathBuf {
    let mut rest = Vec::new();
    let mut base = p.to_path_buf();
    loop {
        if let Ok(c) = base.canonicalize() {
            return rest.iter().rev().fold(c, |acc, x| acc.join(x));
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
