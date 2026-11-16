use serde_json::Value;

use super::miss::not_found;
use crate::util::bool_arg;

/// `text` with `changes` made in order, each on the text the ones before it left, and the note
/// for the answer (" (3 changes)", " (4 occurrences)"); the error of the first that fails.
pub fn apply(text: &str, changes: &[Change], shown: &str) -> Result<(String, String), String> {
    let mut edited = text.to_string();
    let mut last = 0;
    for (i, (old, new, all)) in changes.iter().enumerate() {
        match replace(&edited, old, new, *all, shown) {
            Ok((t, n)) => (edited, last) = (t, n),
            Err(e) if changes.len() > 1 => {
                return Err(format!(
                    "edit: change {} of {}: {}; nothing was written",
                    i + 1,
                    changes.len(),
                    e.trim_start_matches("edit: ")
                ))
            }
            Err(e) => return Err(e),
        }
    }
    let note = match changes {
        [(_, _, true)] => format!(" ({last} occurrences)"),
        [_] => String::new(),
        _ => format!(" ({} changes)", changes.len()),
    };
    Ok((edited, note))
}

/// One change: the text to replace, what replaces it, and whether every occurrence goes.
pub type Change = (String, String, bool);

/// The changes asked for: the one `old_string`/`new_string` pair, or
/// each entry of `edits`.
pub fn changes(args: &Value) -> Result<Vec<Change>, String> {
    let one = |v: &Value, at: &str| -> Result<Change, String> {
        let (Some(old), Some(new)) =
            (v.get("old_string").and_then(Value::as_str), v.get("new_string").and_then(Value::as_str))
        else {
            return Err(format!("edit: {at}`old_string` and `new_string` (strings) are required"));
        };
        if old.is_empty() {
            return Err(format!("edit: {at}`old_string` is empty; use `write` to create a file"));
        }
        if old == new {
            return Err(format!("edit: {at}`old_string` and `new_string` are the same"));
        }
        Ok((old.to_string(), new.to_string(), bool_arg(v, "replace_all")))
    };
    match args.get("edits") {
        Some(Value::Array(list)) if !list.is_empty() => {
            if args.get("old_string").is_some() {
                return Err("edit: give either `old_string`/`new_string` or `edits`, not both".into());
            }
            list.iter().enumerate().map(|(i, v)| one(v, &format!("change {}: ", i + 1))).collect()
        }
        Some(Value::Array(_)) => {
            Err("edit: `edits` is empty, so nothing changes. To only run a command, use `bash`.".into())
        }
        Some(_) => Err("edit: `edits` must be a list of {old_string, new_string} objects".into()),
        None => one(args, "").map(|c| vec![c]),
    }
}

/// `text` with `old` replaced by `new` (every occurrence with `all`), and how many there were.
pub(super) fn replace(text: &str, old: &str, new: &str, all: bool, shown: &str) -> Result<(String, usize), String> {
    // the file's text is matched as it is; only the strings given adapt to its line ends
    let (old, new) = match_line_ends(text, old, new);
    let (old, new) = (old.as_str(), new.as_str());
    let Some(first) = text.find(old) else { return Err(not_found(text, old, shown)) };
    if all {
        return Ok((text.replace(old, new), text.matches(old).count()));
    }
    // a second match may overlap the first (`aa` in `aaa`), which a count of separate matches misses
    let step = text[first..].chars().next().map_or(1, char::len_utf8);
    if text[first + step..].contains(old) {
        let n = text.char_indices().filter(|(i, _)| text[*i..].starts_with(old)).count();
        return Err(format!(
            "edit: `old_string` occurs {n} times in {shown}; include more surrounding lines to make it unique, or set replace_all"
        ));
    }
    Ok((text.replacen(old, new, 1), 1))
}

/// `old` and `new` as they fit `text`'s line ends: as given when `old` is found so, else with
/// CRLF ends (a CRLF file edited with `\n` strings), else with plain ends. `new` follows the
/// ends of the `old` that matched, and a file that ends every line with CRLF gives them to it.
pub fn match_line_ends(text: &str, old: &str, new: &str) -> (String, String) {
    let lf = |s: &str| s.replace("\r\n", "\n");
    let crlf = |s: &str| lf(s).replace('\n', "\r\n");
    let file_crlf = text.contains("\r\n");
    if !text.contains(old) {
        if file_crlf && old.contains('\n') && text.contains(&crlf(old)) {
            return (crlf(old), crlf(new));
        }
        if old.contains("\r\n") && text.contains(&lf(old)) {
            return (lf(old), lf(new));
        }
    }
    let all_crlf = file_crlf && text.matches('\n').count() == text.matches("\r\n").count();
    if all_crlf && !old.contains('\n') {
        (old.to_string(), crlf(new))
    } else {
        (old.to_string(), new.to_string())
    }
}
