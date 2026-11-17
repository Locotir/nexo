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
    std::fs::rename(&from, &to)?;
    mark_trashed(&to);
    // the context state saved with it (`context::saved`) goes with it, or it is dropped
    move_saved_state(&from, &to);
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
    std::fs::rename(&from, &to)?;
    move_saved_state(&from, &to);
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

/// Moves the context state saved with a session (`context::saved`) along with it. It is only ever a shortcut:
/// one that cannot move is removed, and the session rebuilds it.
fn move_saved_state(from: &Path, to: &Path) {
    // a write cut by a crash leaves its file behind: both places lose theirs
    crate::context::saved::sweep(from);
    crate::context::saved::sweep(to);
    let (a, b) = (crate::context::saved::path_for(from), crate::context::saved::path_for(to));
    if a.is_file() && std::fs::rename(&a, &b).is_err() {
        crate::context::saved::forget(from);
    }
}

/// The sessions in the trash, newest first.
pub fn trashed() -> Vec<SessionMeta> {
    list_in(&trash_dir_in(&dir()))
}
