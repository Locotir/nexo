use super::apply::Change;

use crate::util::display_path;

/// Files looked at beside the edited one, and the largest of them.
pub(super) const SIBLINGS: usize = 200;
pub(super) const SIBLING_BYTES: u64 = 1 << 20;

/// When the first change whose `old_string` is not in `text` is exactly in one other file of the
/// same folder, a sentence saying where (`edit` changes one file per call). `None` when it is in
/// none or in several, so the hint never guesses.
pub(super) async fn elsewhere(
    cwd: &std::path::Path,
    path: &std::path::Path,
    text: &str,
    changes: &[Change],
) -> Option<String> {
    let old = &changes.iter().find(|(old, _, _)| !text.contains(old.as_str()))?.0;
    let mut dir = tokio::fs::read_dir(path.parent()?).await.ok()?;
    let mut found: Vec<(std::path::PathBuf, usize)> = Vec::new();
    for _ in 0..SIBLINGS {
        let Some(e) = dir.next_entry().await.ok()? else { break };
        let other = e.path();
        if other == path || !e.metadata().await.is_ok_and(|m| m.is_file() && m.len() <= SIBLING_BYTES) {
            continue;
        }
        let Ok(bytes) = tokio::fs::read(&other).await else { continue };
        let Ok(body) = String::from_utf8(bytes) else { continue };
        if let Some(at) = body.find(old.as_str()) {
            found.push((other, body[..at].matches('\n').count() + 1));
        }
    }
    match found.as_slice() {
        [(other, line)] => Some(format!(
            "; it is in {} line {line}: `edit` changes one file per call, so send that path for that change",
            display_path(cwd, other)
        )),
        _ => None,
    }
}

/// A way to cut whitespace from the ends of a line.
pub(super) type Trim = fn(&str) -> &str;

/// Lines shown from the file when `old_string` matches it only up to whitespace.
pub(super) const NEAR_LINES: usize = 30;

/// Where `old` sits in `text` when it differs only in the spaces and tabs around its lines: its
/// first line (1-based), the file's own text there, and which ends differ. `None` when no single
/// place matches, so a hint never points at the wrong copy.
pub(super) fn near_miss<'a>(text: &'a str, old: &str) -> Option<(usize, Vec<&'a str>, &'static str)> {
    let want: Vec<&str> = old.lines().collect();
    if want.iter().all(|l| l.trim().is_empty()) {
        return None;
    }
    let have: Vec<&str> = text.lines().collect();
    let rules: [(Trim, &str); 2] =
        [(str::trim_end, "at the ends of lines"), (str::trim, "in the indentation or at the ends of lines")];
    for (norm, what) in rules {
        let mut hits = have
            .windows(want.len())
            .enumerate()
            .filter(|(_, w)| w.iter().zip(&want).all(|(a, b)| norm(a) == norm(b)))
            .map(|(i, _)| i);
        if let (Some(i), None) = (hits.next(), hits.next()) {
            return Some((i + 1, have[i..i + want.len()].to_vec(), what));
        }
    }
    None
}

/// The not-found error, with the file's own text when one place matches up to whitespace.
pub(super) fn not_found(text: &str, old: &str, shown: &str) -> String {
    let Some((first, lines, what)) = near_miss(text, old) else {
        return format!("edit: `old_string` not found in {shown}; read the file and copy the text exactly");
    };
    let last = first + lines.len() - 1;
    let at = if last == first { format!("line {first} matches") } else { format!("lines {first}-{last} match") };
    let mut body: String = lines.iter().take(NEAR_LINES).map(|l| format!("{l}\n")).collect();
    if lines.len() > NEAR_LINES {
        body.push_str(&format!("… {} more lines\n", lines.len() - NEAR_LINES));
    }
    format!(
        "edit: `old_string` not found in {shown} as given; {at} it except for spaces or tabs \
         {what}. The file has there:\n{body}Copy this text exactly (whitespace included) into `old_string`."
    )
}
