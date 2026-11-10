//! Part of the agent loop; see the parent module.
//!
//! The first context of a request: the small files of the project whole, within a token budget, and the
//! project's own check already run, so the first model call can already change something instead of
//! spending steps on reading files and running the tests. Nothing here knows a language, a task or a
//! benchmark: only the shape of a project folder and the manifests' check command.

use super::*;

/// Folders that hold build output, dependencies or tool state, never attached from.
const SKIP_DIRS: &[&str] =
    &["target", "node_modules", ".git", "dist", "build", "out", "__pycache__", "venv", "vendor", "coverage"];
/// A file longer than this (in lines) is listed in the tree, never attached.
const MAX_LINES: usize = 2000;
/// The most files attached by the budget in one request.
const MAX_FILES: usize = 24;
/// The most entries looked at while choosing.
const WALK_CAP: usize = 4000;
/// The most characters of a check result put in the request.
const CHECK_CHARS: usize = 1500;
/// The size, in estimated tokens, the first model call of a request stays under (a margin below the 20k target, for the estimate) with what is attached: the
/// system prompt, the tool list, the conversation so far, the request and the files together.
const FIRST_CALL: u64 = 19_000;

/// The tokens a file of `len` bytes takes attached whole (four bytes a token, plus its header and fence).
pub(super) fn attached_tokens(len: usize) -> u64 {
    len as u64 / 4 + 12
}

/// Files that carry no meaning for a reader of the code: lock files, licences, minified and generated maps.
fn dull(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    n.ends_with(".lock")
        || n.ends_with("-lock.json")
        || n.ends_with(".min.js")
        || n.ends_with(".min.css")
        || n.ends_with(".map")
        || n.starts_with("license")
        || n.starts_with("copying")
}

/// Whether `root` is no project folder: the person's home folder or a filesystem root.
fn outside_project(root: &Path, home: Option<&Path>) -> bool {
    let real = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    real.parent().is_none() || home.is_some_and(|h| h.canonicalize().is_ok_and(|h| h == real))
}

/// The text files under `root` to attach: smallest first, until `budget` tokens are spent (at most
/// [`MAX_FILES`]). `skip` holds the real paths already attached; `allowed` is asked about each candidate.
/// Returns each file's path relative to `root` (forward slashes) with its tokens.
pub(super) fn pick(
    root: &Path,
    budget: u64,
    skip: &[PathBuf],
    allowed: &dyn Fn(&str, &Path) -> bool,
) -> Vec<(String, u64)> {
    let home = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")).map(PathBuf::from);
    pick_in(root, budget, skip, allowed, home.as_deref())
}

/// [`pick`] with the home folder given. Nothing is picked when `root` is that home folder or a filesystem root,
/// or when the walk reaches [`WALK_CAP`] entries: neither is one small project, and the smallest files of a
/// capped, arbitrary walk are noise.
fn pick_in(
    root: &Path,
    budget: u64,
    skip: &[PathBuf],
    allowed: &dyn Fn(&str, &Path) -> bool,
    home: Option<&Path>,
) -> Vec<(String, u64)> {
    if budget == 0 || outside_project(root, home) {
        return Vec::new();
    }
    let real_root = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    let walk = ignore::WalkBuilder::new(root)
        .require_git(false)
        .filter_entry(|e| {
            !(e.file_type().is_some_and(|t| t.is_dir())
                && SKIP_DIRS.contains(&e.file_name().to_string_lossy().as_ref()))
        })
        .build();
    let mut found: Vec<(usize, String)> = Vec::new();
    let mut seen = 0usize;
    for e in walk.flatten().take(WALK_CAP) {
        seen += 1;
        if !e.file_type().is_some_and(|t| t.is_file()) || dull(&e.file_name().to_string_lossy()) {
            continue;
        }
        let Ok(rel) = e.path().strip_prefix(root) else { continue };
        let Ok(meta) = e.metadata() else { continue };
        let len = meta.len() as usize;
        // an empty file says nothing; a file over the budget could never be attached
        if len == 0 || attached_tokens(len) > budget {
            continue;
        }
        found.push((len, rel.to_string_lossy().replace('\\', "/")));
    }
    if seen >= WALK_CAP {
        return Vec::new();
    }
    found.sort();
    let mut out = Vec::new();
    let mut spent = 0u64;
    for (len, rel) in found {
        if out.len() >= MAX_FILES || spent + attached_tokens(len) > budget {
            break;
        }
        let path = root.join(&rel);
        let Ok(real) = path.canonicalize() else { continue };
        if !real.starts_with(&real_root) || skip.contains(&real) {
            continue;
        }
        let Ok(bytes) = std::fs::read(&path) else { continue };
        let Ok(text) = String::from_utf8(bytes) else { continue };
        if text.contains('\0') || text.lines().count() > MAX_LINES || !allowed(&rel, &real) {
            continue;
        }
        spent += attached_tokens(len);
        out.push((rel, attached_tokens(len)));
    }
    out
}

/// The text files of the project to attach to a request, after the ones it names: `agent.attach_budget`
/// tokens in all, the named files counted. The files already in the conversation stay out.
pub(super) fn budget_files(st: &State, text: &str, attachments: &[Attachment]) -> Vec<Attachment> {
    let budget = u64::from(st.config.agent.attach_budget);
    if budget == 0 || !st.config.agent.attach_named_files {
        return Vec::new();
    }
    // a long request leaves less room: the first call stays under FIRST_CALL tokens
    let tools = serde_json::to_string(&offered(st)).map_or(0, |t| t.len() as u64 / 4);
    let fixed = st.system.len() as u64 / 4 + tools + text.len() as u64 / 4 + CHECK_CHARS as u64 / 4;
    let room = FIRST_CALL.saturating_sub(fixed + nexo_core::message::estimate_tokens(&st.history));
    let budget = budget.min(room);
    let mut skip = Vec::new();
    let mut used = 0u64;
    for a in attachments {
        let p = if a.path.is_absolute() { a.path.clone() } else { st.cwd.join(&a.path) };
        if let Ok(m) = std::fs::metadata(&p) {
            used += attached_tokens(m.len() as usize);
        }
        skip.push(p.canonicalize().unwrap_or(p));
    }
    let said: String = st.history.iter().filter(|m| m.role == Role::User).map(|m| m.text()).collect();
    let allowed = |rel: &str, _real: &Path| {
        !said.contains(&attached_header(rel))
            && st.perm.decide("read", ToolKind::Read, &serde_json::json!({ "path": rel }), &st.cwd) == Decision::Allow
    };
    pick(&st.cwd, budget.saturating_sub(used), &skip, &allowed)
        .into_iter()
        .map(|(rel, _)| Attachment { path: PathBuf::from(rel) })
        .collect()
}

/// The lines put at the head of a request: how many files came with it and what the project's check said.
pub(super) fn start_note(attachments: &[Attachment], cwd: &Path, check: Option<&str>, tree: Option<&str>) -> String {
    let mut files = 0;
    let mut tokens = 0u64;
    for a in attachments {
        let p = if a.path.is_absolute() { a.path.clone() } else { cwd.join(&a.path) };
        if image_type(&p).is_some() {
            continue;
        }
        if let Ok(m) = std::fs::metadata(&p) {
            files += 1;
            tokens += attached_tokens(m.len() as usize);
        }
    }
    let mut lines = Vec::new();
    if files > 0 {
        lines.push(format!("attached: {files} files, {tokens} tokens"));
    }
    if let Some(t) = tree {
        lines.push(t.trim().to_string());
    }
    if let Some(c) = check {
        lines.push(c.trim().to_string());
    }
    if lines.is_empty() {
        return String::new();
    }
    format!("[project at the start of this request]\n{}", lines.join("\n"))
}

/// The project's check, run once for the request when the policy lets commands run and the tree changed since
/// the last result: its report, short. None where no check exists, the policy asks or denies, or the
/// check ran past its cap before.
pub(super) async fn start_check(state: &Shared, cancel: &CancellationToken) -> Option<String> {
    let (guard, perm, sandbox, cwd, secs, on, has_bash) = {
        let st = state.lock().await;
        (
            st.post.clone(),
            st.perm.clone(),
            st.sandbox.clone(),
            st.cwd.clone(),
            st.config.tools.post_write_check_secs,
            st.config.agent.check_at_start,
            st.tools.get("bash").is_some(),
        )
    };
    // a new request starts a new account of what was said about the check output
    guard.new_turn();
    if !on || !has_bash {
        return None;
    }
    let check = nexo_tools::project::check_command_within(&cwd, Some(&cwd))?;
    if perm.decide("bash", ToolKind::Exec, &serde_json::json!({"command": check.command}), &cwd) != Decision::Allow {
        return None;
    }
    let cap = Duration::from_secs(secs.max(1));
    if guard.skip_note(&check.command, &check.dir, cap).is_some() {
        return None;
    }
    let root = cwd.clone();
    let files = tokio::task::spawn_blocking(move || nexo_tools::postcheck::snapshot(&root)).await.ok()?;
    let print = nexo_tools::postcheck::fingerprint(&files);
    let text = match guard.recorded(&check.command, print) {
        Some(t) => t,
        None => {
            let t = guard.run(&check.command, &check.dir, cap, &sandbox, &cancel.child_token()).await;
            guard.remember(&check.command, print, &t);
            t
        }
    };
    Some(nexo_tools::postcheck::brief(&text, CHECK_CHARS))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tree(files: &[(&str, String)]) -> tempfile::TempDir {
        let d = tempfile::tempdir().unwrap();
        for (n, t) in files {
            let p = d.path().join(n);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, t).unwrap();
        }
        d
    }

    #[test]
    fn first_context_attaches_small_files_within_the_budget() {
        let big = "x\n".repeat(3000);
        let d = tree(&[
            ("src/a.rs", "fn a() {}\n".into()),
            ("src/b.rs", "fn b() {}\nfn bb() {}\n".into()),
            ("notes.txt", "some notes\n".into()),
            ("src/big.rs", big),
            ("Cargo.lock", "x".repeat(40)),
            ("target/debug/out.txt", "build output\n".into()),
            ("node_modules/m/index.js", "module\n".into()),
        ]);
        let got = pick(d.path(), 6000, &[], &|_, _| true);
        let names: Vec<&str> = got.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(names, ["src/a.rs", "notes.txt", "src/b.rs"], "{got:?}");
        assert!(got.iter().map(|(_, t)| t).sum::<u64>() <= 6000);
        // a file already attached stays out, and the budget bounds what comes
        let skip = [d.path().join("src/a.rs").canonicalize().unwrap()];
        let got = pick(d.path(), 6000, &skip, &|_, _| true);
        assert_eq!(got.len(), 2, "{got:?}");
        assert_eq!(pick(d.path(), 13, &[], &|_, _| true).len(), 0, "a file costs its body and header");
        assert_eq!(pick(d.path(), 0, &[], &|_, _| true).len(), 0);
        // the policy is asked about each file
        let got = pick(d.path(), 6000, &[], &|rel, _| rel != "notes.txt");
        assert_eq!(got.len(), 2);
    }

    #[test]
    fn nothing_is_attached_outside_a_project() {
        let files: Vec<(String, String)> = (0..10).map(|i| (format!("f{i}.txt"), "x\n".to_string())).collect();
        let refs: Vec<(&str, String)> = files.iter().map(|(n, t)| (n.as_str(), t.clone())).collect();
        let d = tree(&refs);
        // the same folder is a project until it is the home folder
        assert_eq!(pick_in(d.path(), 6000, &[], &|_, _| true, None).len(), 10);
        let other = tempfile::tempdir().unwrap();
        assert_eq!(pick_in(d.path(), 6000, &[], &|_, _| true, Some(other.path())).len(), 10);
        assert!(pick_in(d.path(), 6000, &[], &|_, _| true, Some(d.path())).is_empty());
        // a filesystem root is no project either
        let root = std::path::Path::new(std::path::MAIN_SEPARATOR_STR);
        assert!(pick_in(root, 6000, &[], &|_, _| true, None).is_empty());
    }

    #[test]
    fn a_tree_past_the_walk_cap_attaches_nothing() {
        let d = tempfile::tempdir().unwrap();
        for i in 0..WALK_CAP + 100 {
            std::fs::write(d.path().join(format!("f{i}.txt")), "x\n").unwrap();
        }
        assert!(pick_in(d.path(), 6000, &[], &|_, _| true, None).is_empty());
    }
}
