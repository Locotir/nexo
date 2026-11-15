use super::*;

// ---- fetching -----------------------------------------------------------------------------------

/// A folder under the plugins folder that is removed when it goes away.
pub(super) struct Staging(PathBuf);

impl Staging {
    pub(super) fn new(config_dir: &Path) -> anyhow::Result<Staging> {
        let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos());
        let p = plugins::plugins_dir(config_dir).join(format!(".staging-{}-{nanos}", std::process::id()));
        std::fs::create_dir_all(&p).with_context(|| format!("cannot create {}", p.display()))?;
        Ok(Staging(p))
    }
}

impl Drop for Staging {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

pub(super) fn git(args: &[&str], dir: Option<&Path>) -> anyhow::Result<String> {
    let mut c = Command::new("git");
    if let Some(d) = dir {
        c.current_dir(d);
    }
    // never ask for a password, and only the usual transports (no `ext::` command URLs)
    let out = c
        .args(args)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_ALLOW_PROTOCOL", "file:git:http:https:ssh")
        .stdin(Stdio::null())
        .output()
        .context("cannot run git (is it installed?)")?;
    if !out.status.success() {
        bail!("git {} failed: {}", args.first().unwrap_or(&""), String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

pub(super) fn head_of(dir: &Path) -> Option<String> {
    git(&["rev-parse", "HEAD"], Some(dir)).ok().filter(|h| !h.is_empty())
}

pub(super) fn is_repo(p: &Path) -> bool {
    p.join(".git").exists() || (p.join("HEAD").is_file() && p.join("objects").is_dir())
}

/// A shallow clone of `url` into `dest` (which must not exist), at `git_ref` when given.
pub(super) fn clone(url: &str, git_ref: Option<&str>, dest: &Path) -> anyhow::Result<String> {
    let d = dest.display().to_string();
    let shallow = ["clone", "--quiet", "--depth", "1"];
    match git_ref {
        None => {
            git(&[&shallow[..], &["--", url, &d]].concat(), None)?;
        }
        Some(r) => {
            // a branch or tag clones shallow; a commit needs the history to check it out
            if git(&[&shallow[..], &["--branch", r, "--", url, &d]].concat(), None).is_err() {
                let _ = std::fs::remove_dir_all(dest);
                git(&["clone", "--quiet", "--", url, &d], None)?;
                git(&["checkout", "--quiet", r], Some(dest))?;
            }
        }
    }
    head_of(dest).ok_or_else(|| anyhow!("the clone has no commit"))
}

/// Copies a folder without `.git` and without links.
pub(super) fn copy_tree(from: &Path, to: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(to)?;
    for e in std::fs::read_dir(from)? {
        let e = e?;
        let name = e.file_name();
        let t = e.file_type()?;
        if name == ".git" || t.is_symlink() {
            continue;
        }
        if t.is_dir() {
            copy_tree(&e.path(), &to.join(&name))?;
        } else {
            std::fs::copy(e.path(), to.join(&name))?;
        }
    }
    Ok(())
}

/// Gets `w` into the empty folder `dest`; returns the commit it is pinned to. A folder that is a
/// plugin or marketplace is copied as it is (with no `ref`); a git repository or URL is cloned.
pub(super) fn fetch(w: &Where, dest: &Path, is_market: bool) -> anyhow::Result<String> {
    if is_url(&w.loc) {
        return clone(&w.loc, w.git_ref.as_deref(), dest);
    }
    let p = Path::new(&w.loc);
    if !p.is_dir() {
        bail!("{} is not a folder", w.loc);
    }
    let has_manifest = if is_market { market_file(p).is_some() } else { plugins::manifest_file(p).is_some() };
    if has_manifest && w.git_ref.is_none() {
        copy_tree(p, dest)?;
        return Ok(head_of(p).unwrap_or_else(|| "local".into()));
    }
    if is_repo(p) {
        return clone(&w.loc, w.git_ref.as_deref(), dest);
    }
    bail!("{} is neither a plugin folder (no plugin.json) nor a git repository", w.loc)
}

pub(super) fn stage(config_dir: &Path, w: &Where, is_market: bool) -> anyhow::Result<(Staging, PathBuf, String)> {
    let st = Staging::new(config_dir)?;
    let dest = st.0.join("src");
    let commit = fetch(w, &dest, is_market)?;
    Ok((st, dest, commit))
}

/// Puts `from` at `to`; an old `to` is moved aside first and removed once the new one is in place.
pub(super) fn swap_in(from: &Path, to: &Path) -> anyhow::Result<()> {
    std::fs::create_dir_all(to.parent().unwrap_or(Path::new(".")))?;
    let old = to.with_extension("old-replaced");
    let had = to.exists();
    if had {
        let _ = std::fs::remove_dir_all(&old);
        std::fs::rename(to, &old)?;
    }
    if let Err(e) = std::fs::rename(from, to) {
        if had {
            let _ = std::fs::rename(&old, to);
        }
        return Err(e).context("cannot move the new files into place");
    }
    if had {
        let _ = std::fs::remove_dir_all(&old);
    }
    Ok(())
}
