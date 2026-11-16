//! `nexo sessions` list, rename, delete and restore.

pub fn print_sessions() -> i32 {
    let list = nexo_agent::session::list();
    if list.is_empty() {
        println!("no sessions yet");
        return 0;
    }
    println!("{:<22} {:<9} {:<28} title", "id", "messages", "directory");
    for m in list {
        let dir = m.cwd.to_string_lossy();
        let dir = if dir.len() > 28 { format!("…{}", &dir[dir.len() - 27..]) } else { dir.to_string() };
        println!("{:<22} {:<9} {:<28} {}", m.id, m.messages, dir, m.title);
    }
    0
}

/// `nexo sessions rename <id> <title>`: the title line is appended to the session file.
pub fn rename_session(id: &str, title: &str) -> anyhow::Result<i32> {
    let title = title.trim();
    if title.is_empty() {
        anyhow::bail!("give the session a name: nexo sessions rename <id> <name>");
    }
    let mut s = nexo_agent::session::Session::open(id)
        .map_err(|_| anyhow::anyhow!("no session {id:?}; `nexo sessions` lists them"))?;
    s.set_title(title)?;
    println!("{id} is now \"{title}\"");
    Ok(0)
}

/// `nexo sessions delete|restore <id>...`: each one moved to or from the trash; 1 when any failed.
pub fn trash_sessions(ids: &[String], restore: bool) -> i32 {
    if ids.is_empty() {
        eprintln!("which session? `nexo sessions` lists them");
        return 1;
    }
    let mut code = 0;
    for id in ids {
        let r = if restore { nexo_agent::session::restore(id) } else { nexo_agent::session::trash(id) };
        match r {
            Ok(p) => println!("{id} {} ({})", if restore { "restored" } else { "moved to the trash" }, p.display()),
            Err(e) => {
                eprintln!("{id}: {e}");
                code = 1;
            }
        }
    }
    code
}
