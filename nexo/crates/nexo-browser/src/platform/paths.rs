//! Paths and URLs as the browser wants them, on every OS: no `\\?\` prefixes, and local files written as
//! `file://` URLs the model does not have to build.

use std::borrow::Cow;
use std::io;
use std::path::{Path, PathBuf};

/// `p` without the `\\?\` (verbatim) prefix Windows adds when canonicalizing: Chrome takes `C:\a\b`, not
/// `\\?\C:\a\b`, and `\\?\UNC\host\share` is `\\host\share`. Anything else is returned as it is.
pub fn plain(p: &Path) -> PathBuf {
    let s = p.to_string_lossy();
    if let Some(rest) = s.strip_prefix(r"\\?\UNC\") {
        return PathBuf::from(format!(r"\\{rest}"));
    }
    if let Some(rest) = s.strip_prefix(r"\\?\") {
        let b = rest.as_bytes();
        if b.len() >= 2 && b[0].is_ascii_alphabetic() && b[1] == b':' {
            return PathBuf::from(rest);
        }
    }
    p.to_path_buf()
}

/// The absolute, plain path of an existing file; the error names the path.
pub fn existing_file(path: &Path) -> io::Result<PathBuf> {
    let full = std::fs::canonicalize(path).map_err(|e| io::Error::new(e.kind(), format!("{}: {e}", path.display())))?;
    if !full.is_file() {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, format!("{} is not a file", path.display())));
    }
    Ok(plain(&full))
}

fn is_drive_path(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() >= 3 && b[0].is_ascii_alphabetic() && b[1] == b':' && (b[2] == b'\\' || b[2] == b'/')
}

fn encode(path: &str) -> String {
    let mut out = String::with_capacity(path.len());
    for byte in path.bytes() {
        match byte {
            b'A'..=b'Z'
            | b'a'..=b'z'
            | b'0'..=b'9'
            | b'-'
            | b'.'
            | b'_'
            | b'~'
            | b'/'
            | b':'
            | b'@'
            | b'!'
            | b'$'
            | b'&'
            | b'\''
            | b'('
            | b')'
            | b'*'
            | b'+'
            | b','
            | b';'
            | b'=' => out.push(byte as char),
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

/// A `file://` URL for a path, from any host: `C:\Users\me\a b.html` is `file:///C:/Users/me/a%20b.html`,
/// `\\srv\share\a.html` is `file://srv/share/a.html`, `/tmp/a.html` is `file:///tmp/a.html`.
pub fn file_url(path: &str) -> String {
    let flat = path.replace('\\', "/");
    if is_drive_path(path) {
        format!("file:///{}", encode(&flat))
    } else if let Some(unc) = flat.strip_prefix("//") {
        format!("file://{}", encode(unc))
    } else if flat.starts_with('/') {
        format!("file://{}", encode(&flat))
    } else {
        format!("file:///{}", encode(&flat))
    }
}

/// The text before the first `/`, `?` or `#`: `host`, `host:port` or `[::1]:port`.
fn authority(t: &str) -> &str {
    t.split(['/', '?', '#']).next().unwrap_or("")
}

/// True for `scheme:` followed by something that is not a port number: `about:blank`, `file:///a`, `data:...`.
/// `localhost:3000` and `example.com:8080/x` have no scheme.
fn has_scheme(t: &str) -> bool {
    let Some((scheme, rest)) = t.split_once(':') else { return false };
    let mut chars = scheme.chars();
    let valid = chars.next().is_some_and(|c| c.is_ascii_alphabetic())
        && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'));
    let port = rest.split(['/', '?', '#']).next().unwrap_or("");
    valid && !(!port.is_empty() && port.bytes().all(|b| b.is_ascii_digit()))
}

/// A host the browser can reach on this machine, which dev servers serve over plain http.
fn is_local_host(authority: &str) -> bool {
    let host = if let Some(v6) = authority.strip_prefix('[') {
        v6.split(']').next().unwrap_or("")
    } else {
        authority.split(':').next().unwrap_or("")
    };
    let host = host.to_ascii_lowercase();
    host == "localhost"
        || host.ends_with(".localhost")
        || host == "::1"
        || host == "0.0.0.0"
        || host.starts_with("127.")
}

/// True when `authority` is a host a person would type without a scheme: it has a dot, a port or brackets, or is
/// `localhost`, and no spaces.
fn looks_like_host(authority: &str) -> bool {
    !authority.is_empty()
        && !authority.contains(char::is_whitespace)
        && (authority.contains('.')
            || authority.contains(':')
            || authority.starts_with('[')
            || is_local_host(authority))
}

/// What to navigate to for what the model typed. A Windows path (`C:\x\a.html`, `C:/x/a.html`, `\\srv\a.html`)
/// or the path of an existing file becomes a `file://` URL; an address with no scheme gets one (`example.com` is
/// `https://example.com`, `localhost:3000` is `http://localhost:3000`); anything else, such as `about:blank` or a
/// full URL, is left alone.
pub fn navigable(target: &str) -> Cow<'_, str> {
    let t = target.trim();
    if t.is_empty() || t.starts_with("//") {
        return Cow::Borrowed(target);
    }
    let unc = t.starts_with(r"\\") && t.len() > 2;
    if is_drive_path(t) || unc {
        return Cow::Owned(file_url(t));
    }
    if has_scheme(t) {
        return Cow::Borrowed(target);
    }
    // A path to a file that exists: absolute as typed, a relative one made absolute first.
    let local = Path::new(t);
    if local.is_file() {
        let abs = if local.is_absolute() {
            local.to_path_buf()
        } else {
            existing_file(local).unwrap_or_else(|_| local.to_path_buf())
        };
        return Cow::Owned(file_url(&abs.to_string_lossy()));
    }
    let host = authority(t);
    if looks_like_host(host) && !t.starts_with(['.', '~', '/', '\\']) {
        let scheme = if is_local_host(host) { "http" } else { "https" };
        return Cow::Owned(format!("{scheme}://{t}"));
    }
    Cow::Borrowed(target)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verbatim_prefixes_are_stripped() {
        assert_eq!(plain(Path::new(r"\\?\C:\Users\ana\cv.pdf")), PathBuf::from(r"C:\Users\ana\cv.pdf"));
        assert_eq!(plain(Path::new(r"\\?\UNC\srv\share\cv.pdf")), PathBuf::from(r"\\srv\share\cv.pdf"));
        assert_eq!(plain(Path::new(r"C:\Users\ana\cv.pdf")), PathBuf::from(r"C:\Users\ana\cv.pdf"));
        assert_eq!(plain(Path::new("/home/ana/cv.pdf")), PathBuf::from("/home/ana/cv.pdf"));
        // A verbatim path that is not a drive or UNC path has no plain form.
        assert_eq!(plain(Path::new(r"\\?\Volume{1234}\a")), PathBuf::from(r"\\?\Volume{1234}\a"));
    }

    #[test]
    fn file_urls_cover_drives_unc_and_unix() {
        assert_eq!(file_url(r"C:\Users\me\a b.html"), "file:///C:/Users/me/a%20b.html");
        assert_eq!(file_url("d:/x/y.html"), "file:///d:/x/y.html");
        assert_eq!(file_url(r"\\srv\share\a.html"), "file://srv/share/a.html");
        assert_eq!(file_url("/tmp/a#1?.html"), "file:///tmp/a%231%3F.html");
        assert_eq!(file_url(r"C:\Users\ñu\é.html"), "file:///C:/Users/%C3%B1u/%C3%A9.html");
    }

    #[test]
    fn navigable_only_rewrites_local_paths() {
        assert_eq!(navigable(r"C:\Users\me\page.html"), "file:///C:/Users/me/page.html");
        assert_eq!(navigable("C:/Users/me/page.html"), "file:///C:/Users/me/page.html");
        assert_eq!(navigable(r"\\srv\share\page.html"), "file://srv/share/page.html");
        assert_eq!(navigable("https://example.com/a"), "https://example.com/a");
        assert_eq!(navigable("http://localhost:3000"), "http://localhost:3000");
        assert_eq!(navigable("file:///tmp/a.html"), "file:///tmp/a.html");
        assert_eq!(navigable("about:blank"), "about:blank");
        assert_eq!(navigable("//cdn.example.com/x.js"), "//cdn.example.com/x.js");
        assert_eq!(navigable("chrome://settings"), "chrome://settings");
        assert_eq!(navigable("data:text/html,<p>x</p>"), "data:text/html,<p>x</p>");
        assert_eq!(navigable("view-source:https://example.com"), "view-source:https://example.com");
        // A Unix path that is not a file stays as typed.
        assert_eq!(navigable("/no/such/file.html"), "/no/such/file.html");
        assert_eq!(navigable(""), "");
        assert_eq!(navigable("two words"), "two words");
        assert_eq!(navigable("localhostage"), "localhostage");
    }

    #[test]
    fn an_address_with_no_scheme_gets_one() {
        assert_eq!(navigable("example.com"), "https://example.com");
        assert_eq!(navigable("  example.com/a?b=1#c "), "https://example.com/a?b=1#c");
        assert_eq!(navigable("www.example.com:8443/x"), "https://www.example.com:8443/x");
        assert_eq!(navigable("sub.example.co.uk"), "https://sub.example.co.uk");
        // Servers on this machine are plain http.
        assert_eq!(navigable("localhost"), "http://localhost");
        assert_eq!(navigable("localhost:3000"), "http://localhost:3000");
        assert_eq!(navigable("localhost:3000/app"), "http://localhost:3000/app");
        assert_eq!(navigable("127.0.0.1:8080"), "http://127.0.0.1:8080");
        assert_eq!(navigable("[::1]:5173"), "http://[::1]:5173");
        assert_eq!(navigable("app.localhost:3000"), "http://app.localhost:3000");
        // A LAN address is not this machine.
        assert_eq!(navigable("192.168.1.5:8080"), "https://192.168.1.5:8080");
    }

    #[test]
    fn a_relative_path_to_an_existing_file_becomes_a_url() {
        // The tests run from the crate directory, where Cargo.toml is.
        let got = navigable("Cargo.toml");
        assert!(got.starts_with("file://") && got.ends_with("/Cargo.toml"), "{got}");
        // The same name that is not a file is an address.
        assert_eq!(navigable("no-such-file.html"), "https://no-such-file.html");
        assert_eq!(navigable("./no-such-file.html"), "./no-such-file.html");
    }

    #[test]
    fn a_unix_file_path_becomes_a_url() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("p q.html");
        std::fs::write(&f, "").unwrap();
        let s = f.to_string_lossy().to_string();
        if s.starts_with('/') {
            assert_eq!(navigable(&s), format!("file://{}", s.replace(' ', "%20")));
        }
    }

    #[test]
    fn existing_file_is_absolute_plain_and_names_the_path_on_error() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("cv.pdf");
        std::fs::write(&f, "x").unwrap();
        let got = existing_file(&f).unwrap();
        assert!(got.is_absolute() && got.is_file());
        assert!(!got.to_string_lossy().starts_with(r"\\?\"));

        let missing = dir.path().join("nope.pdf");
        let err = existing_file(&missing).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::NotFound);
        assert!(err.to_string().contains("nope.pdf"), "{err}");
        let err = existing_file(dir.path()).unwrap_err();
        assert!(err.to_string().contains("is not a file"), "{err}");
    }
}
