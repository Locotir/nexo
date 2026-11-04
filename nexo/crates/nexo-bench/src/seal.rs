//! Sealed commands: on Linux with unprivileged user namespaces, a command runs with no network,
//! the system read-only and every home and temporary folder replaced by an empty one, except the
//! task's working directory and the Rust toolchain. The agent cannot read the benchmark's own
//! source (where the answers are) nor reach anything over the network.

use std::path::{Path, PathBuf};

/// Runs as `sh -c SCRIPT seal WD KEEP MASK TMP CMD HIDE...`: binds the system read-only, hides the homes,
/// temporary folders, `/run` (but for the PATH folders under it, put back read-only: NixOS keeps its programs
/// there), `/dev/shm` and HIDE under empty filesystems, then puts back WD
/// (writable) and each colon-separated KEEP folder (read-only) at their own paths. Each
/// colon-separated program name in MASK is shadowed by a stub that says the program is not found,
/// first on PATH and bound over the real file (and its versioned names, `python3.12`) in every PATH
/// folder and the system's `bin` folders, so a full path finds the stub too.
/// A non-empty TMP is a folder of the run put at /tmp, so what a command leaves there is still there for the next.
pub const SCRIPT: &str = r#"set -e
wd="$1"; keep="$2"; mask="$3"; tmp="$4"; cmd="$5"; shift 5
for d in /usr /etc /bin /sbin /lib /lib32 /lib64 /boot /var /nix; do
  case "$wd/" in "$d"/*) continue;; esac
  if [ -d "$d" ] && [ ! -L "$d" ]; then mount --rbind "$d" "$d"; mount -o remount,bind,ro "$d"; fi
done
mount -t tmpfs none /mnt
mkdir /mnt/wd
mount --bind "$wd" /mnt/wd
if [ -n "$tmp" ]; then mkdir /mnt/tmp; mount --bind "$tmp" /mnt/tmp; fi
i=0; IFS=:
for k in $keep; do
  if [ -d "$k" ]; then mkdir "/mnt/k$i"; mount --bind "$k" "/mnt/k$i"; mount -o remount,bind,ro "/mnt/k$i"; fi
  i=$((i+1))
done
unset IFS
path0="$PATH"; real=""; runs=""; j=0; IFS=:
for p in $PATH; do
  r=$(readlink -f "$p" 2>/dev/null) || r="$p"; real="$real:$r"
  case "$p" in /run/user|/run/user/*) continue;; /run/?*) ;; *) continue;; esac
  t="${p#/run/}"; t="/run/${t%%/*}"
  case "$runs:" in *":$t:"*) continue;; esac
  if [ -d "$t" ]; then mkdir "/mnt/r$j"; mount --bind "$t" "/mnt/r$j"; mount -o remount,bind,ro "/mnt/r$j"; runs="$runs:$t"; j=$((j+1)); fi
done
unset IFS; PATH="${real#:}"
for d in /tmp /root /home /var/tmp /opt /srv /media /run /dev/shm "$@"; do
  if [ -d "$d" ]; then mount -t tmpfs none "$d"; fi
done
if [ -d /var/run ] && [ ! -L /var/run ]; then mount -t tmpfs none /var/run; fi
if [ -n "$tmp" ]; then mount --bind /mnt/tmp /tmp; fi
j=0; IFS=:
for t in ${runs#:}; do
  mkdir -p "$t"; mount --bind "/mnt/r$j" "$t"; mount -o remount,bind,ro "$t"; j=$((j+1))
done
unset IFS; PATH="$path0"
mkdir -p "$wd"
mount --bind /mnt/wd "$wd"
i=0; IFS=:
for k in $keep; do
  if [ -d "/mnt/k$i" ]; then mkdir -p "$k"; mount --bind "/mnt/k$i" "$k"; mount -o remount,bind,ro "$k"; fi
  i=$((i+1))
done
unset IFS
if [ -n "$mask" ]; then
  mkdir -p /tmp/.mask; IFS=:
  dirs="$PATH:/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin"
  for m in $mask; do
    printf '#!/bin/sh\necho "sh: 1: %s: not found" >&2\nexit 127\n' "$m" > "/tmp/.mask/$m"; chmod +x "/tmp/.mask/$m"
    for d in $dirs; do
      for f in "$d/$m" "$d/$m"[0-9]*; do
        if [ -f "$f" ]; then mount --bind "/tmp/.mask/$m" "$f"; fi
      done
    done
  done
  unset IFS; export PATH="/tmp/.mask:$PATH"
fi
cd "$wd"
exec sh -c "$cmd"
"#;

/// The namespaces a sealed command gets: its own user (mapped to root inside), mounts, network
/// and process ids.
const UNSHARE: [&str; 7] = ["-r", "-m", "-n", "-p", "--mount-proc", "--fork", "--kill-child"];

/// How commands are sealed for one task.
#[derive(Debug, Clone)]
pub struct Seal {
    /// The working directory, the only writable folder.
    pub wd: PathBuf,
    /// Folders hidden on top of the usual ones (the bench's output, the project it runs from).
    pub hide: Vec<PathBuf>,
    /// The Rust toolchain, kept visible read-only so `cargo` and `rustc` work inside.
    pub sysroot: Option<PathBuf>,
    /// Programs the box lacks: each name answers "not found" however the system has it.
    pub mask: Vec<String>,
    /// A folder of the run that commands see as /tmp, kept between commands; none: an empty /tmp each time.
    pub tmp: Option<PathBuf>,
}

impl Seal {
    /// The seal for `wd`, or why commands cannot be sealed on this machine.
    pub fn new(wd: &Path, hide: Vec<PathBuf>, mask: Vec<String>) -> Result<Seal, String> {
        available()?;
        Ok(Seal { wd: wd.to_path_buf(), hide, sysroot: sysroot(), mask, tmp: None })
    }

    /// The same seal with `tmp` as every command's /tmp.
    pub fn with_tmp(mut self, tmp: &Path) -> Seal {
        self.tmp = Some(tmp.to_path_buf());
        self
    }

    /// `cmd` as a shell command line that runs it sealed.
    pub fn wrap(&self, cmd: &str) -> String {
        // a cleared environment: inside, the home and the temporary folder are the empty /tmp
        let mut parts = vec!["env".to_string(), "-i".to_string()];
        let tmp = Path::new("/tmp");
        for (k, v) in crate::env::clean(tmp, tmp) {
            let v = match k.as_str() {
                "PATH" => match &self.sysroot {
                    Some(s) => format!("{}:{v}", s.join("bin").to_string_lossy()),
                    None => v,
                },
                "CARGO_HOME" => continue,
                _ => v,
            };
            parts.push(quote(&format!("{k}={v}")));
        }
        parts.push("CARGO_HOME=/tmp/.cargo-home".into());
        parts.push("unshare".into());
        parts.extend(UNSHARE.iter().map(|s| s.to_string()));
        parts.extend(["sh".into(), "-c".into(), quote(SCRIPT), "seal".into()]);
        parts.push(quote(&self.wd.to_string_lossy()));
        parts.push(quote(&self.sysroot.as_deref().map(|p| p.to_string_lossy().into_owned()).unwrap_or_default()));
        parts.push(quote(&self.mask.join(":")));
        parts.push(quote(&self.tmp.as_deref().map(|p| p.to_string_lossy().into_owned()).unwrap_or_default()));
        parts.push(quote(cmd));
        parts.extend(self.hide.iter().map(|h| quote(&h.to_string_lossy())));
        parts.join(" ")
    }
}

/// `s` as one single-quoted shell word.
pub fn quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

/// The Rust toolchain's root (`rustc --print sysroot`).
pub fn sysroot() -> Option<PathBuf> {
    let out = std::process::Command::new("rustc").args(["--print", "sysroot"]).output().ok()?;
    let p = PathBuf::from(String::from_utf8_lossy(&out.stdout).trim());
    (out.status.success() && p.is_dir()).then_some(p)
}

/// Ok when commands can be sealed here, else why not. Probed once.
#[cfg(target_os = "linux")]
pub fn available() -> Result<(), String> {
    static PROBE: std::sync::OnceLock<Result<(), String>> = std::sync::OnceLock::new();
    PROBE
        .get_or_init(|| {
            let ok = std::process::Command::new("unshare")
                .args(["-r", "-m", "-n", "-p", "--mount-proc", "--fork", "true"])
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status()
                .is_ok_and(|s| s.success());
            if ok {
                Ok(())
            } else {
                Err("sealing needs unshare (util-linux) and unprivileged user namespaces".into())
            }
        })
        .clone()
}

/// Ok when commands can be sealed here, else why not.
#[cfg(not(target_os = "linux"))]
pub fn available() -> Result<(), String> {
    Err("sealing needs Linux (unshare and user namespaces)".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quoting_survives_single_quotes() {
        assert_eq!(quote("it's"), r"'it'\''s'");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn a_sealed_command_sees_its_folder_but_not_the_hidden_one_nor_a_masked_program() {
        if available().is_err() {
            eprintln!("skipped: {}", available().unwrap_err());
            return;
        }
        let wd = tempfile::tempdir().unwrap();
        let secret = tempfile::tempdir().unwrap();
        std::fs::write(wd.path().join("mine.txt"), "visible").unwrap();
        std::fs::write(secret.path().join("answer.txt"), "42").unwrap();
        let seal = Seal::new(wd.path(), vec![secret.path().to_path_buf()], vec!["awk".into()]).unwrap();
        let cmd = format!(
            "cat mine.txt; cat {}/answer.txt || echo hidden; awk 2>/dev/null; echo \"awk $?\"",
            secret.path().display()
        );
        let out = std::process::Command::new("sh").arg("-c").arg(seal.wrap(&cmd)).output().unwrap();
        let text = String::from_utf8_lossy(&out.stdout);
        assert!(text.contains("visible") && text.contains("hidden") && !text.contains("42"), "{text}");
        assert!(text.contains("awk 127"), "{text}");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn a_runs_tmp_is_kept_between_commands_and_the_machines_is_not_seen() {
        if available().is_err() {
            eprintln!("skipped: {}", available().unwrap_err());
            return;
        }
        let wd = tempfile::tempdir().unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let other = tempfile::tempdir().unwrap();
        std::fs::write(other.path().join("theirs.txt"), "secret").unwrap();
        let seal = Seal::new(wd.path(), vec![], vec!["awk".into()]).unwrap().with_tmp(tmp.path());
        let sh = |c: &str| {
            let out = std::process::Command::new("sh").arg("-c").arg(seal.wrap(c)).output().unwrap();
            String::from_utf8_lossy(&out.stdout).into_owned()
        };
        sh("echo kept > /tmp/note.txt; echo mine > \"$TMPDIR/b.txt\"; echo here > in-wd.txt");
        let seen = sh(&format!(
            "cat /tmp/note.txt /tmp/b.txt in-wd.txt; cat {}/theirs.txt || echo hidden",
            other.path().display()
        ));
        assert!(
            seen.contains("kept") && seen.contains("mine") && seen.contains("here") && seen.contains("hidden"),
            "{seen}"
        );
        assert!(!seen.contains("secret"), "{seen}");
        assert_eq!(std::fs::read_to_string(tmp.path().join("note.txt")).unwrap(), "kept\n");
        assert_eq!(std::fs::read_to_string(wd.path().join("in-wd.txt")).unwrap(), "here\n");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn cargo_tests_a_crate_without_dependencies_inside_the_seal() {
        if available().is_err() || sysroot().is_none() {
            eprintln!("skipped: no seal or no Rust toolchain");
            return;
        }
        let wd = tempfile::tempdir().unwrap();
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir(wd.path().join("src")).unwrap();
        std::fs::write(wd.path().join("Cargo.toml"), crate::tasks::manifest("boxed", "")).unwrap();
        std::fs::write(wd.path().join("src/lib.rs"), "#[test]\nfn adds() { assert_eq!(1 + 1, 2); }\n").unwrap();
        let seal = Seal::new(wd.path(), vec![], vec![]).unwrap().with_tmp(tmp.path());
        let out =
            std::process::Command::new("sh").arg("-c").arg(seal.wrap("cargo test --quiet 2>&1")).output().unwrap();
        let text = String::from_utf8_lossy(&out.stdout);
        assert!(out.status.success() && text.contains("1 passed"), "{text}");
    }

    /// Systems whose programs live under /run (NixOS: /run/current-system is a link into the store,
    /// /run/wrappers a folder of its own), reproduced in a namespace where /run is a fresh tmpfs.
    #[cfg(target_os = "linux")]
    #[test]
    fn programs_on_a_path_under_run_keep_working_and_the_rest_of_run_stays_hidden() {
        if available().is_err() {
            eprintln!("skipped: {}", available().unwrap_err());
            return;
        }
        let Some(sh) = nexo_core::shell::find_program("sh") else { return };
        let bin = sh.canonicalize().unwrap().parent().unwrap().to_path_buf();
        let wd = tempfile::tempdir().unwrap();
        let seal = Seal::new(wd.path(), vec![], vec![]).unwrap();
        let wrapped = seal.wrap("nexo-run-probe; echo \"run=[$(ls -A /run | grep -vx mount | tr '\\n' ' ')]\"");
        // only the folders under /run are on PATH, as on NixOS
        let start = wrapped.find("'PATH=").unwrap();
        let end = start + 1 + wrapped[start + 1..].find('\'').unwrap();
        let wrapped =
            format!("{}'PATH=/run/wrappers/bin:/run/current-system/bin'{}", &wrapped[..start], &wrapped[end + 1..]);
        let outer = format!(
            "set -e; mount -t tmpfs none /run; mkdir -p /run/wrappers/bin /run/user/1000; touch /run/user/1000/bus /run/secret.sock; \
             ln -s {} /run/current-system; printf '#!/bin/sh\\necho probe-ran\\n' > /run/wrappers/bin/nexo-run-probe; \
             chmod +x /run/wrappers/bin/nexo-run-probe; {wrapped}",
            quote(&bin.parent().unwrap().to_string_lossy())
        );
        let out = std::process::Command::new("unshare").args(["-r", "-m", "sh", "-c", &outer]).output().unwrap();
        let text = String::from_utf8_lossy(&out.stdout);
        assert!(text.contains("probe-ran"), "{text}{}", String::from_utf8_lossy(&out.stderr));
        assert!(text.contains("run=[current-system wrappers ]"), "{text}");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn a_sealed_command_sees_no_session_sockets_no_secrets_and_no_masked_program_by_full_path() {
        if available().is_err() {
            eprintln!("skipped: {}", available().unwrap_err());
            return;
        }
        let Some(awk) = nexo_core::shell::find_program("awk") else { return };
        let wd = tempfile::tempdir().unwrap();
        std::env::set_var("NEXO_BENCH_SEAL_TOKEN", "seal-token-marker");
        let seal = Seal::new(wd.path(), vec![], vec!["awk".into()]).unwrap();
        let cmd = format!(
            "echo \"run=[$(ls -A /run | grep -vx mount)] shm=[$(ls -A /dev/shm)] varrun=[$(ls -A /var/run/ | grep -vx mount)]\"; env; \
             {} 'BEGIN{{print \"ran\"}}' 2>/dev/null; echo \"full $?\"; touch /dev/shm/x && echo shm-writable",
            awk.display()
        );
        let out = std::process::Command::new("sh").arg("-c").arg(seal.wrap(&cmd)).output().unwrap();
        let text = String::from_utf8_lossy(&out.stdout);
        // `mount` itself keeps its table in /run/mount, inside the empty /run
        assert!(text.contains("run=[] shm=[] varrun=[]"), "{text}");
        assert!(!text.contains("seal-token-marker") && text.contains("HOME=/tmp"), "{text}");
        assert!(text.contains("full 127") && !text.contains("ran\n"), "{text}");
    }
}
