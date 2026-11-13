//! The local endpoint a host listens on and an agent connects to: a Unix socket (mode 0600, in a folder only the
//! current user can enter) on Linux and macOS, a named pipe whose access list holds only the current user on
//! Windows. One endpoint per browser, named after the browser's id.

use std::io;
use std::path::{Path, PathBuf};
use tokio::io::{AsyncRead, AsyncWrite};

/// An open connection, whatever carries it.
pub trait Duplex: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> Duplex for T {}
pub type BoxStream = Box<dyn Duplex>;

/// Where nexo keeps its browser state, and where the endpoints live.
#[derive(Debug, Clone)]
pub struct Dirs {
    data: PathBuf,
    run: PathBuf,
}

impl Dirs {
    /// Explicit folders (tests, or a caller that already knows them).
    pub fn new(data: PathBuf, run: PathBuf) -> Dirs {
        Dirs { data, run }
    }

    /// The current user's: `NEXO_DATA_DIR` or `~/.nexo` for the data, and `$XDG_RUNTIME_DIR/nexo-browser` for the
    /// sockets (else `<data>/browser/run`).
    pub fn for_user() -> Dirs {
        let data = std::env::var_os("NEXO_DATA_DIR")
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
            .unwrap_or_else(|| dirs::home_dir().unwrap_or_else(|| PathBuf::from(".")).join(".nexo"));
        let run = std::env::var_os("XDG_RUNTIME_DIR")
            .map(PathBuf::from)
            .filter(|p| p.is_absolute() && p.is_dir())
            .map(|p| p.join("nexo-browser"))
            .unwrap_or_else(|| data.join("browser").join("run"));
        Dirs { data, run }
    }

    /// `<data>/browser`: the token lives here.
    pub fn state(&self) -> PathBuf {
        self.data.join("browser")
    }

    pub fn run(&self) -> &Path {
        &self.run
    }
}

/// A browser a host serves right now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Found {
    pub id: String,
    pub location: String,
}

/// The id inside an endpoint name, when the name is one of ours.
fn id_of(name: &str) -> Option<&str> {
    let id = name.strip_prefix("browser-")?;
    let id = id.strip_suffix(".sock").unwrap_or(id);
    crate::proto::valid_id(id).then_some(id)
}

fn check_id(id: &str) -> io::Result<()> {
    if crate::proto::valid_id(id) {
        Ok(())
    } else {
        Err(io::Error::new(io::ErrorKind::InvalidInput, "a browser id is 1 to 64 letters, digits, - or _"))
    }
}

#[cfg(unix)]
pub use unix::*;
#[cfg(windows)]
pub use windows::*;

#[cfg(unix)]
mod unix {
    use super::*;
    use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};
    use tokio::net::{UnixListener, UnixStream};

    /// The longest path a Unix socket takes (`sun_path` is 108 bytes on Linux, 104 on macOS).
    const MAX_PATH: usize = 100;

    fn euid() -> u32 {
        // SAFETY: geteuid has no preconditions and cannot fail.
        unsafe { libc::geteuid() }
    }

    /// Makes `dir` (and its parents) and leaves it closed to everybody but the current user. A folder that exists and
    /// belongs to somebody else is refused.
    pub fn ensure_private_dir(dir: &Path) -> io::Result<()> {
        std::fs::DirBuilder::new().recursive(true).mode(0o700).create(dir)?;
        let meta = std::fs::metadata(dir)?;
        if meta.uid() != euid() {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!("{} belongs to another user", dir.display()),
            ));
        }
        if meta.permissions().mode() & 0o077 != 0 {
            std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
        }
        Ok(())
    }

    /// Fails unless `path` belongs to the current user and no one else can touch it. `NotFound` stays `NotFound`.
    /// `is_socket` says a socket is expected (it must then be closed to others, any other file too).
    pub fn check_private(path: &Path, is_socket: bool) -> io::Result<()> {
        let meta = std::fs::metadata(path)?;
        if meta.uid() != euid() {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!("{} belongs to another user", path.display()),
            ));
        }
        if meta.permissions().mode() & 0o077 != 0 {
            let want = if is_socket { "600" } else { "600 (chmod 600)" };
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!(
                    "{} is open to other users (mode {:o}); it must be {want}",
                    path.display(),
                    meta.permissions().mode() & 0o777
                ),
            ));
        }
        Ok(())
    }

    fn socket_path(dirs: &Dirs, id: &str) -> io::Result<PathBuf> {
        check_id(id)?;
        let p = dirs.run().join(format!("browser-{id}.sock"));
        if p.as_os_str().len() > MAX_PATH {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("{} is too long for a Unix socket", p.display()),
            ));
        }
        Ok(p)
    }

    pub struct Listener {
        inner: UnixListener,
        path: PathBuf,
    }

    impl Listener {
        /// Starts listening for browser `id`. A socket left by a dead host is replaced; a live one is refused.
        pub fn bind(dirs: &Dirs, id: &str) -> io::Result<Listener> {
            let path = socket_path(dirs, id)?;
            ensure_private_dir(dirs.run())?;
            if path.exists() {
                if std::os::unix::net::UnixStream::connect(&path).is_ok() {
                    return Err(io::Error::new(
                        io::ErrorKind::AddrInUse,
                        format!("another nexo host already serves browser {id}"),
                    ));
                }
                std::fs::remove_file(&path)?;
            }
            let inner = UnixListener::bind(&path)?;
            let listener = Listener { inner, path };
            std::fs::set_permissions(&listener.path, std::fs::Permissions::from_mode(0o600))?;
            check_private(&listener.path, true)?;
            Ok(listener)
        }

        /// The next connection from the current user; another user's is dropped.
        pub async fn accept(&mut self) -> io::Result<BoxStream> {
            loop {
                let (stream, _) = self.inner.accept().await?;
                match stream.peer_cred() {
                    Ok(c) if c.uid() == euid() => return Ok(Box::new(stream)),
                    _ => continue,
                }
            }
        }

        pub fn location(&self) -> String {
            self.path.display().to_string()
        }
    }

    impl Drop for Listener {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.path);
        }
    }

    /// The browsers whose hosts are listening, by id.
    pub fn list(dirs: &Dirs) -> Vec<Found> {
        let Ok(rd) = std::fs::read_dir(dirs.run()) else { return Vec::new() };
        let mut found: Vec<Found> = rd
            .flatten()
            .filter_map(|e| {
                let name = e.file_name().to_string_lossy().into_owned();
                id_of(&name)
                    .filter(|_| name.ends_with(".sock"))
                    .map(|id| Found { id: id.to_string(), location: e.path().display().to_string() })
            })
            .collect();
        found.sort_by(|a, b| a.id.cmp(&b.id));
        found
    }

    /// Connects to the host of browser `id`, after checking that the socket and its server belong to the current
    /// user: the token is never offered to anyone else.
    pub async fn connect(dirs: &Dirs, id: &str) -> io::Result<BoxStream> {
        let path = socket_path(dirs, id)?;
        check_private(&path, true)?;
        let stream = UnixStream::connect(&path).await?;
        match stream.peer_cred() {
            Ok(c) if c.uid() == euid() => Ok(Box::new(stream)),
            _ => {
                Err(io::Error::new(io::ErrorKind::PermissionDenied, "the host on that socket is not run by this user"))
            }
        }
    }
}

#[cfg(windows)]
mod windows {
    use super::*;
    use std::ffi::c_void;
    use tokio::net::windows::named_pipe::{ClientOptions, NamedPipeServer, ServerOptions};
    use windows_sys::Win32::Foundation::{CloseHandle, LocalFree, HANDLE};
    use windows_sys::Win32::Security::Authorization::{
        ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
    };
    use windows_sys::Win32::Security::{GetTokenInformation, TokenUser, SECURITY_ATTRIBUTES, TOKEN_QUERY, TOKEN_USER};
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

    const ERROR_PIPE_BUSY: i32 = 231;
    const PREFIX: &str = r"\\.\pipe\nexo-browser-";

    /// On Windows the token file sits in the user's own profile folder, which the default access list already limits
    /// to that user, SYSTEM and the administrators: nothing more to do than make the folder.
    pub fn ensure_private_dir(dir: &Path) -> io::Result<()> {
        std::fs::create_dir_all(dir)
    }

    pub fn check_private(path: &Path, _is_socket: bool) -> io::Result<()> {
        std::fs::metadata(path).map(|_| ())
    }

    /// The SID of the current user as text (`S-1-5-21-...`).
    fn user_sid() -> io::Result<String> {
        // SAFETY: plain Win32 calls with buffers sized by the API's own length answer; every handle and allocation is
        // released before returning.
        unsafe {
            let mut token: HANDLE = std::ptr::null_mut();
            if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) == 0 {
                return Err(io::Error::last_os_error());
            }
            let result = (|| {
                let mut len = 0u32;
                GetTokenInformation(token, TokenUser, std::ptr::null_mut(), 0, &mut len);
                if len == 0 {
                    return Err(io::Error::last_os_error());
                }
                // u64 words so the buffer is aligned for TOKEN_USER.
                let mut buf = vec![0u64; (len as usize).div_ceil(8)];
                if GetTokenInformation(token, TokenUser, buf.as_mut_ptr().cast(), len, &mut len) == 0 {
                    return Err(io::Error::last_os_error());
                }
                let user = &*(buf.as_ptr() as *const TOKEN_USER);
                let mut wide: *mut u16 = std::ptr::null_mut();
                if ConvertSidToStringSidW(user.User.Sid, &mut wide) == 0 {
                    return Err(io::Error::last_os_error());
                }
                let mut n = 0;
                while *wide.add(n) != 0 {
                    n += 1;
                }
                let text = String::from_utf16_lossy(std::slice::from_raw_parts(wide, n));
                LocalFree(wide.cast());
                Ok(text)
            })();
            CloseHandle(token);
            result
        }
    }

    /// One more instance of the pipe, readable and writable by the current user alone and closed to the network.
    fn instance(name: &str, first: bool, sid: &str) -> io::Result<NamedPipeServer> {
        // Protected DACL with one entry: full access for the current user. SYSTEM and the administrators are left out.
        let sddl: Vec<u16> = format!("D:P(A;;GA;;;{sid})").encode_utf16().chain(std::iter::once(0)).collect();
        let mut descriptor: *mut c_void = std::ptr::null_mut();
        // SAFETY: `sddl` is NUL-terminated; on success `descriptor` is a LocalAlloc block freed below, and the pipe
        // copies the descriptor when it is created.
        unsafe {
            if ConvertStringSecurityDescriptorToSecurityDescriptorW(
                sddl.as_ptr(),
                SDDL_REVISION_1,
                &mut descriptor,
                std::ptr::null_mut(),
            ) == 0
            {
                return Err(io::Error::last_os_error());
            }
            let mut attrs = SECURITY_ATTRIBUTES {
                nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
                lpSecurityDescriptor: descriptor,
                bInheritHandle: 0,
            };
            let made = ServerOptions::new()
                .first_pipe_instance(first)
                .reject_remote_clients(true)
                .create_with_security_attributes_raw(name, (&mut attrs as *mut SECURITY_ATTRIBUTES).cast());
            LocalFree(descriptor);
            made
        }
    }

    pub struct Listener {
        name: String,
        sid: String,
        next: NamedPipeServer,
    }

    impl Listener {
        /// Starts listening for browser `id`. The first instance is created with `first_pipe_instance`, so a pipe
        /// that somebody else already made under this name is an error and not a trap.
        pub fn bind(_dirs: &Dirs, id: &str) -> io::Result<Listener> {
            check_id(id)?;
            let name = format!("{PREFIX}{id}");
            let sid = user_sid()?;
            let next = instance(&name, true, &sid).map_err(|e| {
                if e.kind() == io::ErrorKind::PermissionDenied {
                    io::Error::new(io::ErrorKind::AddrInUse, format!("another nexo host already serves browser {id}"))
                } else {
                    e
                }
            })?;
            Ok(Listener { name, sid, next })
        }

        pub async fn accept(&mut self) -> io::Result<BoxStream> {
            self.next.connect().await?;
            let fresh = instance(&self.name, false, &self.sid)?;
            Ok(Box::new(std::mem::replace(&mut self.next, fresh)))
        }

        pub fn location(&self) -> String {
            self.name.clone()
        }
    }

    pub fn list(_dirs: &Dirs) -> Vec<Found> {
        let Ok(rd) = std::fs::read_dir(r"\\.\pipe\") else { return Vec::new() };
        let mut found: Vec<Found> = rd
            .flatten()
            .filter_map(|e| {
                let name = e.file_name().to_string_lossy().into_owned();
                id_of(&name).map(|id| Found { id: id.to_string(), location: format!("{PREFIX}{id}") })
            })
            .collect();
        found.sort_by(|a, b| a.id.cmp(&b.id));
        found
    }

    pub async fn connect(_dirs: &Dirs, id: &str) -> io::Result<BoxStream> {
        check_id(id)?;
        let name = format!("{PREFIX}{id}");
        for _ in 0..40 {
            match ClientOptions::new().open(&name) {
                Ok(c) => return Ok(Box::new(c)),
                Err(e) if e.raw_os_error() == Some(ERROR_PIPE_BUSY) => {
                    tokio::time::sleep(std::time::Duration::from_millis(50)).await
                }
                Err(e) => return Err(e),
            }
        }
        Err(io::Error::new(io::ErrorKind::TimedOut, "the pipe stayed busy"))
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    fn dirs(tmp: &tempfile::TempDir) -> Dirs {
        Dirs::new(tmp.path().join("data"), tmp.path().join("run"))
    }

    fn mode(p: &Path) -> u32 {
        std::fs::metadata(p).unwrap().permissions().mode() & 0o777
    }

    #[tokio::test]
    async fn the_socket_is_0600_in_a_0700_folder_and_carries_bytes() {
        let tmp = tempfile::tempdir().unwrap();
        let d = dirs(&tmp);
        let mut l = Listener::bind(&d, "b-1").unwrap();
        assert_eq!(mode(d.run()), 0o700);
        assert_eq!(mode(&d.run().join("browser-b-1.sock")), 0o600);
        assert_eq!(list(&d), vec![Found { id: "b-1".into(), location: l.location() }]);
        let server = tokio::spawn(async move {
            let mut s = l.accept().await.unwrap();
            let mut b = [0u8; 4];
            s.read_exact(&mut b).await.unwrap();
            s.write_all(&b).await.unwrap();
            l
        });
        let mut c = connect(&d, "b-1").await.unwrap();
        c.write_all(b"ping").await.unwrap();
        let mut back = [0u8; 4];
        c.read_exact(&mut back).await.unwrap();
        assert_eq!(&back, b"ping");
        drop(server.await.unwrap());
        assert!(list(&d).is_empty(), "the socket file goes away with the listener");
    }

    #[tokio::test]
    async fn an_open_folder_is_closed_and_an_open_socket_is_not_used() {
        let tmp = tempfile::tempdir().unwrap();
        let d = dirs(&tmp);
        std::fs::create_dir_all(d.run()).unwrap();
        std::fs::set_permissions(d.run(), std::fs::Permissions::from_mode(0o755)).unwrap();
        let _l = Listener::bind(&d, "b-2").unwrap();
        assert_eq!(mode(d.run()), 0o700);
        let sock = d.run().join("browser-b-2.sock");
        std::fs::set_permissions(&sock, std::fs::Permissions::from_mode(0o666)).unwrap();
        let err = connect(&d, "b-2").await.err().unwrap();
        assert_eq!(err.kind(), io::ErrorKind::PermissionDenied, "{err}");
    }

    #[tokio::test]
    async fn a_stale_socket_is_replaced_and_a_live_one_is_refused() {
        let tmp = tempfile::tempdir().unwrap();
        let d = dirs(&tmp);
        // A host that died without cleaning up leaves its socket file behind, with nobody listening.
        std::fs::create_dir_all(d.run()).unwrap();
        drop(std::os::unix::net::UnixListener::bind(d.run().join("browser-b-3.sock")).unwrap());
        assert!(d.run().join("browser-b-3.sock").exists());
        let second = Listener::bind(&d, "b-3").unwrap();
        let err = Listener::bind(&d, "b-3").err().unwrap();
        assert_eq!(err.kind(), io::ErrorKind::AddrInUse);
        drop(second);
    }

    #[test]
    fn ids_that_could_escape_the_folder_are_refused() {
        let tmp = tempfile::tempdir().unwrap();
        let d = dirs(&tmp);
        for bad in ["", "../x", "a/b", "a b", "a.b", &"x".repeat(65)] {
            assert_eq!(Listener::bind(&d, bad).err().unwrap().kind(), io::ErrorKind::InvalidInput, "{bad:?}");
        }
    }

    #[test]
    fn a_socket_path_too_long_for_the_system_is_an_error_not_a_crash() {
        let tmp = tempfile::tempdir().unwrap();
        let d = Dirs::new(tmp.path().join("d"), tmp.path().join("r".repeat(120)));
        assert_eq!(Listener::bind(&d, "b-4").err().unwrap().kind(), io::ErrorKind::InvalidInput);
    }
}
