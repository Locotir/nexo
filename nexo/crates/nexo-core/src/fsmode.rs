//! File rights for what only the user should read: sessions, keys, the audit log and the lane logs.
//! The rights are Unix modes; on Windows every helper here does the plain thing and the user's profile
//! folder rights apply.

use std::fs::OpenOptions;
use std::path::Path;

/// Makes `dir` and its missing parents. On Unix the ones made are `0700`, so the names of what is inside
/// are not listed for other users; a folder that already exists keeps the rights it has (see [`tighten_dir`]).
pub fn create_private_dir(dir: &Path) -> std::io::Result<()> {
    let mut b = std::fs::DirBuilder::new();
    b.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        b.mode(0o700);
    }
    b.create(dir)
}

/// Options for a file only the user can read: on Unix a file this opens for the first time is `0600`
/// (a file that exists keeps its rights, see [`tighten_file`]).
pub fn private_file() -> OpenOptions {
    #[cfg_attr(not(unix), allow(unused_mut))]
    let mut o = OpenOptions::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        o.mode(0o600);
    }
    o
}

/// Makes a folder that already exists `0700`. Folders made before nexo set a mode got the umask (usually
/// `0755`), so the one that holds a private log has to be closed once; a missing folder is left alone.
pub fn tighten_dir(dir: &Path) -> std::io::Result<()> {
    set_mode(dir, 0o700)
}

/// Makes a file that already exists `0600`, for the same reason as [`tighten_dir`].
pub fn tighten_file(file: &Path) -> std::io::Result<()> {
    set_mode(file, 0o600)
}

#[cfg_attr(not(unix), allow(unused_variables))]
fn set_mode(path: &Path, mode: u32) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        match std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            r => r,
        }
    }
    #[cfg(not(unix))]
    Ok(())
}

#[cfg(all(test, unix))]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    use super::*;

    fn mode(p: &Path) -> u32 {
        std::fs::metadata(p).unwrap().permissions().mode() & 0o777
    }

    #[test]
    fn a_made_folder_and_file_are_the_users_alone_and_an_old_one_can_be_closed() {
        let d = tempfile::tempdir().unwrap();
        let dir = d.path().join("a/b");
        create_private_dir(&dir).unwrap();
        assert_eq!(mode(&dir), 0o700);
        let f = dir.join("x.log");
        private_file().create(true).append(true).open(&f).unwrap();
        assert_eq!(mode(&f), 0o600);
        // a folder and a file made the old way keep their rights until they are tightened
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
        std::fs::set_permissions(&f, std::fs::Permissions::from_mode(0o644)).unwrap();
        tighten_dir(&dir).unwrap();
        tighten_file(&f).unwrap();
        assert_eq!((mode(&dir), mode(&f)), (0o700, 0o600));
        // nothing to close is not an error
        tighten_file(&dir.join("missing")).unwrap();
    }
}
