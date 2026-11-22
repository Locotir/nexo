//! Open the *named* regular skill file, never a target substituted by a link between
//! permission and use. Read from this handle; do not canonicalize and reopen it.
use std::{fs::File, io, path::Path};

pub(super) fn open(path: &Path) -> io::Result<File> {
    if !path.is_absolute() || path.components().any(|c| c == std::path::Component::ParentDir) {
        return Err(io::Error::other("expected an absolute skill path without traversal"));
    }
    let f = open_checked(path)?;
    if !f.metadata()?.is_file() {
        return Err(io::Error::other("not a regular skill file"));
    }
    Ok(f)
}

#[cfg(unix)]
fn open_checked(path: &Path) -> io::Result<File> {
    use std::{
        ffi::CString,
        os::fd::{AsRawFd, FromRawFd},
        os::unix::ffi::OsStrExt,
        path::Component,
    };
    let mut dir = File::open("/")?;
    let mut parts =
        path.components().filter_map(|c| if let Component::Normal(n) = c { Some(n) } else { None }).peekable();
    while let Some(part) = parts.next() {
        let name = CString::new(part.as_bytes())?;
        let flags = libc::O_RDONLY
            | libc::O_NOFOLLOW
            | libc::O_CLOEXEC
            | if parts.peek().is_some() { libc::O_DIRECTORY } else { libc::O_NONBLOCK };
        // SAFETY: dir is live, name is NUL terminated, no mode argument is needed.
        let fd = unsafe { libc::openat(dir.as_raw_fd(), name.as_ptr(), flags) };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: openat just returned a new, owned descriptor.
        dir = unsafe { File::from_raw_fd(fd) };
    }
    Ok(dir)
}

#[cfg(windows)]
fn open_checked(path: &Path) -> io::Result<File> {
    use std::{
        ffi::OsString,
        os::windows::{
            ffi::OsStringExt,
            fs::{MetadataExt, OpenOptionsExt},
            io::AsRawHandle,
        },
        path::PathBuf,
    };
    use windows_sys::Win32::Storage::FileSystem::{
        GetFinalPathNameByHandleW, FILE_ATTRIBUTE_REPARSE_POINT, FILE_FLAG_OPEN_REPARSE_POINT,
    };
    let f = File::options().read(true).custom_flags(FILE_FLAG_OPEN_REPARSE_POINT).open(path)?;
    if f.metadata()?.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        return Err(io::Error::other("linked skill file"));
    }
    let mut buf = vec![0u16; 32768];
    // SAFETY: f is live; buf has the stated writable capacity in UTF-16 units.
    let n = unsafe { GetFinalPathNameByHandleW(f.as_raw_handle(), buf.as_mut_ptr(), buf.len() as u32, 0) } as usize;
    if n == 0 || n >= buf.len() {
        return Err(io::Error::other("cannot verify skill file handle"));
    }
    let actual = PathBuf::from(OsString::from_wide(&buf[..n]));
    // Windows returns verbatim paths, including `\\?\UNC\server\share` for UNC.
    if !windows_same_path(path, &actual) {
        return Err(io::Error::other("skill file resolves through an alias"));
    }
    Ok(f)
}

#[cfg(windows)]
fn windows_same_path(requested: &Path, actual: &Path) -> bool {
    requested == actual
        || actual.to_str().and_then(windows_plain_path).is_some_and(|plain| requested == Path::new(&plain))
}

#[cfg(any(windows, test))]
fn windows_plain_path(path: &str) -> Option<String> {
    let rest = path.strip_prefix(r"\\?\")?;
    Some(if let Some(unc) = rest.strip_prefix(r"UNC\") { format!(r"\\{unc}") } else { rest.to_owned() })
}

#[cfg(test)]
mod tests {
    #[cfg(windows)]
    #[test]
    fn windows_final_path_accepts_mixed_separators_but_not_redirects() {
        use std::path::Path;
        let same = super::windows_same_path;
        assert!(same(
            Path::new(r"C:\project\.nexo/skills/debug/SKILL.md"),
            Path::new(r"\\?\C:\project\.nexo\skills\debug\SKILL.md")
        ));
        assert!(same(
            Path::new(r"\\server\share\skills/debug/SKILL.md"),
            Path::new(r"\\?\UNC\server\share\skills\debug\SKILL.md")
        ));
        assert!(!same(Path::new(r"C:\project\skills\debug\SKILL.md"), Path::new(r"\\?\C:\outside\debug\SKILL.md")));
    }

    #[cfg(windows)]
    #[test]
    fn opens_skill_with_mixed_path_separators() {
        use std::io::Read;
        let temp = tempfile::tempdir().unwrap();
        let root = crate::paths::canonical(temp.path()).unwrap();
        let dir = root.join(".nexo/skills/example");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("SKILL.md");
        std::fs::write(&path, "instructions").unwrap();
        let mut text = String::new();
        super::open(&path).unwrap().read_to_string(&mut text).unwrap();
        assert_eq!(text, "instructions");
    }

    #[test]
    fn windows_verbatim_drive_and_unc_paths() {
        assert_eq!(super::windows_plain_path(r"\\?\C:\skill\SKILL.md").as_deref(), Some(r"C:\skill\SKILL.md"));
        assert_eq!(
            super::windows_plain_path(r"\\?\UNC\server\share\SKILL.md").as_deref(),
            Some(r"\\server\share\SKILL.md")
        );
        assert_eq!(super::windows_plain_path(r"C:\skill"), None);
    }
}

#[cfg(not(any(unix, windows)))]
fn open_checked(_: &Path) -> io::Result<File> {
    Err(io::Error::other("safe skill reads unsupported"))
}
