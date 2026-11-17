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
        let dir = short_dir(&dir);
        println!("{:<22} {:<9} {:<28} {}", m.id, m.messages, dir, m.title);
    }
    0
}

/// Keeps the last 27 chars of a long directory behind an ellipsis. It cuts by chars, not bytes,
/// because a byte offset can fall inside a multibyte char (ñ, CJK) and panic.
fn short_dir(dir: &str) -> String {
    let n = dir.chars().count();
    if n > 28 {
        format!("…{}", dir.chars().skip(n - 27).collect::<String>())
    } else {
        dir.to_string()
    }
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

#[cfg(test)]
mod tests {
    use super::short_dir;

    #[test]
    fn a_long_directory_of_multibyte_chars_is_shortened_without_panicking() {
        let dir = "ñ".repeat(40);
        assert_eq!(short_dir(&dir), format!("…{}", "ñ".repeat(27)));
    }

    #[test]
    fn a_long_cjk_directory_keeps_its_last_27_chars() {
        let dir = format!("/home/用户/{}", "文".repeat(30));
        let out = short_dir(&dir);
        assert_eq!(out.chars().count(), 28);
        assert!(out.ends_with(&"文".repeat(27)));
    }

    #[test]
    fn a_short_ascii_directory_is_left_unchanged() {
        assert_eq!(short_dir("/home/user/project"), "/home/user/project");
    }

    #[test]
    fn a_long_cwd_of_multibyte_characters_is_shortened_without_a_panic() {
        let wide = "目".repeat(30);
        let short = short_dir(&wide);
        assert_eq!(short.chars().count(), 28);
        assert!(short.starts_with('…'));
        let accented = format!("/home/é/{}", "é".repeat(30));
        assert_eq!(short_dir(&accented).chars().count(), 28);
    }

    #[test]
    fn a_short_or_ascii_cwd_is_kept_as_it_is_or_cut_to_its_tail() {
        assert_eq!(short_dir("/home/me/project"), "/home/me/project");
        assert_eq!(short_dir(&"é".repeat(28)), "é".repeat(28));
        let long = format!("/{}", "a".repeat(40));
        assert_eq!(short_dir(&long), format!("…{}", "a".repeat(27)));
    }
}
