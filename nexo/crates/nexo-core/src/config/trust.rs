use super::*;

/// `<root>` of `<root>/.nexo/config.toml`, resolved.
pub fn project_root(project_file: &Path) -> PathBuf {
    let root = project_file.parent().and_then(Path::parent).unwrap_or(project_file);
    root.canonicalize().unwrap_or_else(|_| root.to_path_buf())
}

/// Whether `root` (resolved) is a line of `trust_file`.
pub fn is_trusted(root: &Path, trust_file: &Path) -> bool {
    let root = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    // a worktree nexo made (`<repo>/.nexo/worktrees/<name>`) is as trusted as its repository
    if let Some(main) = root.parent().filter(|p| p.ends_with(".nexo/worktrees")).and_then(|p| p.parent()?.parent()) {
        if is_trusted(main, trust_file) {
            return true;
        }
    }
    std::fs::read_to_string(trust_file)
        .map(|t| t.lines().map(str::trim).any(|l| !l.is_empty() && !l.starts_with('#') && Path::new(l) == root))
        .unwrap_or(false)
}

/// Adds `root` (resolved) to `trust_file`, one path per line; true when it was not there yet.
/// The line is appended in one write, so sessions adding at the same moment lose nothing.
pub fn trust_project(root: &Path, trust_file: &Path) -> std::io::Result<bool> {
    let root = root.canonicalize()?;
    if is_trusted(&root, trust_file) {
        return Ok(false);
    }
    if let Some(d) = trust_file.parent() {
        std::fs::create_dir_all(d)?;
    }
    // One append, never a rewrite: two sessions trusting at once both keep their line. A blank
    // line or the same path twice is harmless to `is_trusted`.
    let open_line = std::fs::read(trust_file).is_ok_and(|b| b.last().is_some_and(|c| *c != b'\n'));
    let line = format!("{}{}\n", if open_line { "\n" } else { "" }, root.display());
    let mut f = std::fs::OpenOptions::new().create(true).append(true).open(trust_file)?;
    std::io::Write::write_all(&mut f, line.as_bytes())?;
    Ok(true)
}
