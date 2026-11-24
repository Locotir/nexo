use super::*;

/// Moves session `id` to the trash and says where it went. Nothing is erased: `restore` brings it back.
pub fn trash(id: &str) -> anyhow::Result<PathBuf> {
    trash_in(&dir(), id)
}

pub fn trash_in(dir: &Path, id: &str) -> anyhow::Result<PathBuf> {
    let id = checked_id(id)?;
    let from = dir.join(format!("{id}.jsonl"));
    if !from.is_file() {
        anyhow::bail!("no session {id}");
    }
    // another nexo still writing to it would bring it back at its next rewrite
    let lock = lock_path(&from);
    if let Some(pid) = holder(&lock).filter(|p| *p != Some(std::process::id())) {
        return Err(in_use(&lock, pid));
    }
    let bin = trash_dir_in(dir);
    std::fs::create_dir_all(&bin)?;
    let to = bin.join(format!("{id}.jsonl"));
    if to.exists() {
        anyhow::bail!("a session {id} is already in the trash");
    }
    move_session_with_state(&from, &to)?;
    mark_trashed(&to);
    move_todo(dir, &trash_dir_in(dir), id);
    Ok(to)
}

/// A file moved by `rename` keeps the time of its last write, so a session left alone for months would look as
/// old in the trash as it did outside and `nexo sessions prune --trash` would delete it the day it was trashed.
/// Its time becomes the day it was trashed (what `trashed` lists newest first by), which also gives a session
/// that is restored a day before the sweep looks at it again. Best effort: where the time cannot be set, the
/// session stays what it was.
fn mark_trashed(path: &Path) {
    let _ = std::fs::File::options().write(true).open(path).and_then(|f| f.set_modified(std::time::SystemTime::now()));
}

/// Brings session `id` back from the trash.
pub fn restore(id: &str) -> anyhow::Result<PathBuf> {
    restore_in(&dir(), id)
}

pub fn restore_in(dir: &Path, id: &str) -> anyhow::Result<PathBuf> {
    let id = checked_id(id)?;
    let from = trash_dir_in(dir).join(format!("{id}.jsonl"));
    let to = dir.join(format!("{id}.jsonl"));
    if !from.is_file() {
        anyhow::bail!("no session {id} in the trash");
    }
    if to.exists() {
        anyhow::bail!("a session {id} is already there");
    }
    move_session_with_state(&from, &to)?;
    move_todo(&trash_dir_in(dir), dir, id);
    Ok(to)
}

/// Moves the task list saved with session `id` (`nexo_tools::todo`) from one folder to another, so the trash does
/// not leave it behind in the live folder. Like the context state it is only ever a shortcut: one that cannot move
/// is removed.
fn move_todo(from_dir: &Path, to_dir: &Path, id: &str) {
    let (a, b) = (nexo_tools::todo::file_in(from_dir, id), nexo_tools::todo::file_in(to_dir, id));
    if a.is_file() && std::fs::rename(&a, &b).is_err() {
        let _ = std::fs::remove_file(&a);
    }
}

/// Link every recovery state before moving the session. A crash on either side of the session rename
/// leaves a complete copy beside whichever session is authoritative. Never overwrite an existing state.
fn move_session_with_state(from: &Path, to: &Path) -> anyhow::Result<()> {
    let name = from.file_name().and_then(|n| n.to_str()).ok_or_else(|| anyhow::anyhow!("invalid session path"))?;
    let prefix = format!("{name}.curator");
    let mut linked = Vec::new();
    let result = (|| -> anyhow::Result<()> {
        for entry in std::fs::read_dir(from.parent().unwrap_or(Path::new(".")))? {
            let entry = entry?;
            let file = entry.file_name();
            let Some(file) = file.to_str() else { continue };
            let Some(suffix) = file.strip_prefix(&prefix) else { continue };
            // Only committed snapshots, never a temporary write or another session's files.
            let compact =
                suffix.strip_prefix(".compact.").and_then(|s| s.split_once('.')).is_some_and(|(count, hash)| {
                    count.parse::<usize>().is_ok() && hash.len() == 16 && hash.bytes().all(|b| b.is_ascii_hexdigit())
                });
            if !suffix.is_empty() && !compact {
                continue;
            }
            let dest = to.parent().unwrap_or(Path::new(".")).join(file);
            match std::fs::hard_link(entry.path(), &dest) {
                Ok(()) => linked.push((entry.path(), dest, true)),
                Err(e)
                    if e.kind() == std::io::ErrorKind::AlreadyExists
                        && std::fs::read(entry.path())? == std::fs::read(&dest)? =>
                {
                    linked.push((entry.path(), dest, false));
                }
                Err(e) => return Err(e.into()),
            }
        }
        std::fs::rename(from, to)?;
        Ok(())
    })();
    if result.is_ok() {
        for (source, _, _) in &linked {
            let _ = std::fs::remove_file(source);
        }
    } else {
        for (_, dest, created) in &linked {
            if *created {
                let _ = std::fs::remove_file(dest);
            }
        }
    }
    result
}

/// The sessions in the trash, newest first.
pub fn trashed() -> Vec<SessionMeta> {
    list_in(&trash_dir_in(&dir()))
}
