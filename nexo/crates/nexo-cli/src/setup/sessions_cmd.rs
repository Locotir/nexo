//! `nexo sessions` list, rename, delete and restore.

use nexo_agent::session::SessionMeta;

/// `nexo sessions [--here] [-n N]`: newest first, each with its age. `--here` keeps the sessions of this folder,
/// `-n` the first N after that. Sessions a program ran (`nexo -p`, a team lane) carry a `program` chip, and the
/// ones nothing was said in show 0 messages.
pub fn print_sessions(here: bool, limit: Option<usize>) -> i32 {
    let cwd = std::env::current_dir().unwrap_or_default();
    let list = nexo_agent::session::list();
    let total = list.len();
    let lines = session_lines(list, here.then_some(cwd.as_path()), limit, nexo_core::mailbox::now_ms());
    if lines.is_empty() {
        println!(
            "{}",
            if total == 0 { "no sessions yet" } else { "no sessions in this folder; `nexo sessions` lists all" }
        );
        return 0;
    }
    println!("{:<22} {:<9} {:<8} {:<28} title", "id", "messages", "age", "directory");
    for l in lines {
        println!("{l}");
    }
    0
}

/// The table rows of `nexo sessions`, newest first (the order `list` returns).
fn session_lines(
    list: Vec<SessionMeta>,
    here: Option<&std::path::Path>,
    limit: Option<usize>,
    now_ms: i64,
) -> Vec<String> {
    list.into_iter()
        .filter(|m| here.is_none_or(|h| nexo_agent::session::same_folder(&m.cwd, h)))
        .take(limit.unwrap_or(usize::MAX))
        .map(|m| {
            let at = nexo_agent::session::updated_ms(&m);
            let age = if at > 0 { nexo_core::team::age(now_ms - at) } else { "-".to_string() };
            let chip = if m.headless { "[program] " } else { "" };
            format!(
                "{:<22} {:<9} {:<8} {:<28} {}{}",
                m.id,
                m.messages,
                age,
                tail(&m.cwd.to_string_lossy(), 28),
                chip,
                m.title
            )
        })
        .collect()
}

/// The last `width` characters of `s`, with a leading `…` when it was cut. It counts characters, not bytes: a
/// folder name with an accent or a CJK character must not be cut in the middle of one.
fn tail(s: &str, width: usize) -> String {
    let n = s.chars().count();
    if n <= width {
        return s.to_string();
    }
    let kept: String = s.chars().skip(n - (width - 1)).collect();
    format!("…{kept}")
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
    use super::*;

    fn meta(id: &str, cwd: &str, messages: usize, headless: bool) -> SessionMeta {
        SessionMeta {
            id: id.into(),
            cwd: cwd.into(),
            created: String::new(),
            updated: "2026-11-17T10:00:00Z".into(),
            provider: String::new(),
            model: String::new(),
            title: "t".into(),
            messages,
            parent: None,
            fork_at: None,
            headless,
        }
    }

    #[test]
    fn a_folder_with_multibyte_characters_is_cut_on_a_character() {
        let long = "/home/josé/proyectos/ñandú/日本語のフォルダ/nexo";
        let cut = tail(long, 28);
        assert_eq!(cut.chars().count(), 28);
        assert!(cut.starts_with('…') && cut.ends_with("nexo"));
        assert_eq!(tail("/short", 28), "/short");
    }

    #[test]
    fn the_list_shows_the_age_and_marks_the_sessions_programs_ran() {
        let now = nexo_agent::session::updated_ms(&meta("a", "/x", 1, false)) + 3 * 3_600_000;
        let lines = session_lines(vec![meta("a", "/x", 4, false), meta("b", "/x", 2, true)], None, None, now);
        assert!(lines[0].contains("3h") && !lines[0].contains("[program]"), "{}", lines[0]);
        assert!(lines[1].contains("[program]"), "{}", lines[1]);
    }

    #[test]
    fn here_and_the_limit_narrow_the_list() {
        let all = || vec![meta("a", "/x", 1, false), meta("b", "/y", 1, false), meta("c", "/x", 1, false)];
        let here = session_lines(all(), Some(std::path::Path::new("/x")), None, 0);
        assert_eq!(here.len(), 2);
        let two = session_lines(all(), None, Some(2), 0);
        assert_eq!(two.len(), 2);
        assert!(two[1].starts_with('b'));
    }

    #[test]
    fn a_long_directory_of_multibyte_chars_is_shortened_without_panicking() {
        let dir = "ñ".repeat(40);
        assert_eq!(tail(&dir, 28), format!("…{}", "ñ".repeat(27)));
    }

    #[test]
    fn a_long_cjk_directory_keeps_its_last_27_chars() {
        let dir = format!("/home/用户/{}", "文".repeat(30));
        let out = tail(&dir, 28);
        assert_eq!(out.chars().count(), 28);
        assert!(out.ends_with(&"文".repeat(27)));
    }

    #[test]
    fn a_short_ascii_directory_is_left_unchanged() {
        assert_eq!(tail("/home/user/project", 28), "/home/user/project");
    }

    #[test]
    fn a_long_cwd_of_multibyte_characters_is_shortened_without_a_panic() {
        let wide = "目".repeat(30);
        let short = tail(&wide, 28);
        assert_eq!(short.chars().count(), 28);
        assert!(short.starts_with('…'));
        let accented = format!("/home/é/{}", "é".repeat(30));
        assert_eq!(tail(&accented, 28).chars().count(), 28);
    }

    #[test]
    fn a_short_or_ascii_cwd_is_kept_as_it_is_or_cut_to_its_tail() {
        assert_eq!(tail("/home/me/project", 28), "/home/me/project");
        assert_eq!(tail(&"é".repeat(28), 28), "é".repeat(28));
        let long = format!("/{}", "a".repeat(40));
        assert_eq!(tail(&long, 28), format!("…{}", "a".repeat(27)));
    }
}
