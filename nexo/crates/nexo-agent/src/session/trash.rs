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
    Ok(to)
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
    Ok(to)
}

/// The sessions in the trash, newest first.
pub fn trashed() -> Vec<SessionMeta> {
    list_in(&trash_dir_in(&dir()))
}
