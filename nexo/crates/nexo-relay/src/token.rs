//! The per-install secret that lets an agent talk to the host: 64 random hex characters in a file only the current
//! user can read (`<data>/browser/token`, mode 0600 in a 0700 folder on Unix; inside the user's own profile folder on
//! Windows, which only that user and the administrators can open).

use crate::endpoint::{ensure_private_dir, Dirs};
use std::io::{self, Write};
use std::path::PathBuf;

/// A new random token (two random UUIDs, hex).
pub fn generate() -> String {
    format!("{}{}", uuid::Uuid::new_v4().simple(), uuid::Uuid::new_v4().simple())
}

/// Equal strings, compared in time that does not depend on where they differ.
pub fn matches(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    let mut diff = a.len() ^ b.len();
    for i in 0..a.len().max(b.len()) {
        diff |= (a.get(i).copied().unwrap_or(0) ^ b.get(i).copied().unwrap_or(0)) as usize;
    }
    diff == 0
}

pub fn path(dirs: &Dirs) -> PathBuf {
    dirs.state().join("token")
}

/// Reads the token. On Unix the file must belong to the current user and be closed to everybody else.
pub fn load(dirs: &Dirs) -> io::Result<String> {
    let p = path(dirs);
    crate::endpoint::check_private(&p, false)?;
    let text = std::fs::read_to_string(&p)?;
    let token = text.trim();
    if token.len() < 32 {
        return Err(io::Error::new(io::ErrorKind::InvalidData, format!("{} is too short to be a token", p.display())));
    }
    Ok(token.to_string())
}

/// Reads the token, creating it first when this is the first run. Safe to call from two processes at once.
pub fn load_or_create(dirs: &Dirs) -> io::Result<String> {
    match load_settled(dirs) {
        Ok(t) => return Ok(t),
        Err(e) if e.kind() != io::ErrorKind::NotFound => return Err(e),
        Err(_) => {}
    }
    ensure_private_dir(&dirs.state())?;
    let p = path(dirs);
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut opts, 0o600);
    match opts.open(&p) {
        Ok(mut f) => {
            let token = generate();
            f.write_all(token.as_bytes())?;
            f.write_all(b"\n")?;
            Ok(token)
        }
        // The other process won the race: take its token.
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => load_settled(dirs),
        Err(e) => Err(e),
    }
}

/// `load`, waiting a moment when the file exists but its writer has not finished.
fn load_settled(dirs: &Dirs) -> io::Result<String> {
    let mut last = io::Error::from(io::ErrorKind::NotFound);
    for _ in 0..50 {
        match load(dirs) {
            Err(e) if e.kind() == io::ErrorKind::InvalidData => last = e,
            other => return other,
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    Err(last)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dirs(tmp: &tempfile::TempDir) -> Dirs {
        Dirs::new(tmp.path().join("data"), tmp.path().join("run"))
    }

    #[test]
    fn tokens_are_long_random_and_compare_exactly() {
        let (a, b) = (generate(), generate());
        assert_eq!(a.len(), 64);
        assert!(a.bytes().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(a, b);
        assert!(matches(&a, &a.clone()));
        assert!(!matches(&a, &b));
        assert!(!matches(&a, &a[..63]));
        assert!(!matches("", &a));
        assert!(matches("", ""));
    }

    #[test]
    fn the_token_is_created_once_and_read_back() {
        let tmp = tempfile::tempdir().unwrap();
        let d = dirs(&tmp);
        assert_eq!(load(&d).unwrap_err().kind(), io::ErrorKind::NotFound);
        let t = load_or_create(&d).unwrap();
        assert_eq!(load_or_create(&d).unwrap(), t);
        assert_eq!(load(&d).unwrap(), t);
    }

    #[test]
    fn two_processes_creating_it_at_once_end_up_with_the_same_token() {
        let tmp = tempfile::tempdir().unwrap();
        let d = dirs(&tmp);
        let handles: Vec<_> = (0..8)
            .map(|_| {
                let d = d.clone();
                std::thread::spawn(move || load_or_create(&d).unwrap())
            })
            .collect();
        let tokens: Vec<String> = handles.into_iter().map(|h| h.join().unwrap()).collect();
        assert!(tokens.iter().all(|t| *t == tokens[0]), "{tokens:?}");
    }

    #[cfg(unix)]
    #[test]
    fn the_token_file_is_0600_in_a_0700_folder_and_an_open_one_is_refused() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let d = dirs(&tmp);
        load_or_create(&d).unwrap();
        let mode = |p: &std::path::Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&path(&d)), 0o600);
        assert_eq!(mode(&d.state()), 0o700);
        std::fs::set_permissions(path(&d), std::fs::Permissions::from_mode(0o644)).unwrap();
        let err = load(&d).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::PermissionDenied);
        assert!(err.to_string().contains("chmod 600"), "{err}");
    }
}
