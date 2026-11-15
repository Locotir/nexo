use super::*;

/// The SHA-256 a `sha256sum` list gives for `name` (`<hex>  <name>`, or `<hex> *<name>`).
pub(super) fn expected_sum<'a>(sums: &'a str, name: &str) -> Option<&'a str> {
    sums.lines().find_map(|l| {
        let (hex, file) = l.split_once(char::is_whitespace)?;
        (file.trim_start().trim_start_matches('*') == name).then_some(hex)
    })
}

pub(super) fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::Digest;
    sha2::Sha256::digest(bytes).iter().map(|b| format!("{b:02x}")).collect()
}

/// An install from before nexo had its own folder (cargo's `bin`), or a build in a checkout's
/// `target`: the installer puts nexo in its own place instead of writing over these.
pub(super) fn needs_installer(exe: &Path) -> bool {
    let Some(dir) = exe.parent() else { return true };
    let name = |p: &Path| p.file_name().and_then(|n| n.to_str()).map(str::to_owned).unwrap_or_default();
    let cargo_home = std::env::var_os("CARGO_HOME").map(PathBuf::from);
    let in_cargo =
        name(dir) == "bin" && dir.parent().is_some_and(|p| name(p) == ".cargo" || Some(p) == cargo_home.as_deref());
    let in_target =
        matches!(name(dir).as_str(), "debug" | "release") && dir.parent().is_some_and(|p| name(p) == "target");
    in_cargo || in_target
}

/// Puts the new build at `exe`: a file renamed into place, so a running nexo keeps working; Windows,
/// which cannot replace a running program, renames the old one to `.old` first.
pub(super) fn place(new: &Path, exe: &Path) -> std::io::Result<()> {
    place_with(new, exe, cfg!(windows), |from, to| std::fs::rename(from, to))
}

/// [`place`] with the operating system and the renaming made explicit. When the new file cannot take
/// the place (a virus scanner holding it, on Windows), the old program is put back: nexo is never left missing.
pub(super) fn place_with(
    new: &Path,
    exe: &Path,
    windows: bool,
    rename: impl Fn(&Path, &Path) -> std::io::Result<()>,
) -> std::io::Result<()> {
    let next = exe.with_extension("new");
    std::fs::copy(new, &next)?;
    let old = exe.with_extension("old");
    let moved = windows && exe.exists();
    if moved {
        let _ = std::fs::remove_file(&old);
        if let Err(e) = rename(exe, &old) {
            let _ = std::fs::remove_file(&next);
            return Err(e);
        }
    }
    if let Err(e) = rename(&next, exe) {
        if moved {
            let _ = rename(&old, exe);
        }
        let _ = std::fs::remove_file(&next);
        return Err(e);
    }
    Ok(())
}

/// Runs the installer script (into `dir` when given); it installs what is missing and builds.
pub(super) fn installer(dir: Option<&Path>) -> anyhow::Result<i32> {
    let (shell, flag, run) = if cfg!(windows) {
        ("powershell", "-Command", format!("irm {INSTALLER_PS} | iex"))
    } else {
        ("sh", "-c", format!("curl -fsSL {INSTALLER_SH} | sh"))
    };
    println!("nexo: running the installer: {run}");
    let mut cmd = std::process::Command::new(shell);
    if cfg!(windows) {
        cmd.args(["-NoProfile", "-ExecutionPolicy", "Bypass"]);
    }
    if let Some(d) = dir {
        cmd.env("NEXO_INSTALL_DIR", d);
    }
    let status =
        cmd.args([flag, &run]).status().map_err(|e| anyhow::anyhow!("cannot run {shell} ({e}); run: {run}"))?;
    Ok(status.code().unwrap_or(1))
}
