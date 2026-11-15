//! Excerpts checked against the disk, and the file and error lines the ledger shows.

use super::*;

pub(super) fn resolve(cwd: &Path, p: &str) -> PathBuf {
    let path = Path::new(p);
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        cwd.join(path)
    }
}

/// Whether every numbered line of `text` is still that line of the file
/// (None: no numbered lines, or no file).
pub(super) fn current(text: &str, file: &Path) -> Option<bool> {
    let lines: Vec<(usize, &str)> = text.lines().filter_map(numbered).collect();
    if lines.is_empty() {
        return None;
    }
    let disk = std::fs::read_to_string(file).ok()?;
    let disk: Vec<&str> = disk.lines().collect();
    Some(lines.iter().all(|&(n, shown)| {
        let Some(real) = n.checked_sub(1).and_then(|i| disk.get(i)) else { return false };
        let shown = shown.trim_end();
        let real = real.trim_end();
        real == shown || shown.split_once('…').is_some_and(|(a, _)| !a.is_empty() && real.starts_with(a))
    }))
}

pub(super) fn range(text: &str) -> Option<(usize, usize)> {
    let ns: Vec<usize> = text.lines().filter_map(numbered).map(|(n, _)| n).collect();
    Some((*ns.iter().min()?, *ns.iter().max()?))
}

/// Read excerpts say which lines they hold and whether those still match the disk.
pub(super) fn annotate(s: &mut Span, cwd: &Path, check: bool) {
    let Some(path) = s.label.strip_prefix("read ").map(str::to_string) else { return };
    if let Some((a, b)) = range(&s.text) {
        s.label = format!("read {} lines {a}-{b}", shown(cwd, &path));
        if check {
            match current(&s.text, &resolve(cwd, &path)) {
                Some(true) => s.label.push_str(" (current)"),
                Some(false) => s.label.push_str(" (changed on disk since)"),
                None => {}
            }
        }
    }
}

// ------------------------------------------------------------ the ledger

/// A path as the briefing shows it: relative to the project when inside it (shorter).
pub(super) fn shown(cwd: &Path, p: &str) -> String {
    match Path::new(p).strip_prefix(cwd) {
        Ok(r) if !r.as_os_str().is_empty() => r.display().to_string(),
        _ => p.to_string(),
    }
}

/// Up to six lines that say what went wrong.
pub(super) fn error_lines(text: &str) -> Vec<String> {
    let body = text.rsplit_once("[exit code").map(|(a, _)| a).unwrap_or(text);
    let lines: Vec<&str> = body.lines().filter(|l| !l.trim().is_empty()).collect();
    let keys = ["error", "fail", "panic", "assert", "expected", "cannot", "not found", "traceback"];
    let hits: Vec<&str> =
        lines.iter().copied().filter(|l| keys.iter().any(|k| l.to_lowercase().contains(k))).take(6).collect();
    let pick = if hits.is_empty() { lines[lines.len().saturating_sub(6)..].to_vec() } else { hits };
    pick.into_iter().map(|l| one_line(l, 200)).collect()
}
