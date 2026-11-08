//! Copying text out of the terminal.
//!
//! On Linux a clipboard is *served* by the process that last wrote it: with
//! X11 the content disappears the moment the serving `arboard::Clipboard` is
//! dropped (which is what the first version of this module did, right after
//! reporting "copied"). So the system clipboard is held in one instance that
//! lives as long as nexo does, the content is also handed to a clipboard
//! tool that forks and outlives us (`wl-copy`, `xclip`, `xsel`, `pbcopy`),
//! and the terminal itself is asked through OSC 52, which works over SSH and
//! inside tmux where no display is reachable. The first method that worked
//! names the copy; when none did, the error says what to install.

use std::io::Write;
use std::process::{Command, Stdio};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use base64::Engine;

/// Copies `text` and says how (`clipboard`, `wl-copy`, `xclip`, `xsel`,
/// `pbcopy` or `terminal (OSC 52)`).
pub fn copy(text: &str) -> Result<&'static str, String> {
    // Tests never reach the person's clipboard or spawn a clipboard tool (`wl-copy` outlives the
    // test and keeps its inherited fds): the text goes to a recorder instead.
    #[cfg(test)]
    return fake::record(text);
    #[cfg(not(test))]
    copy_with(text, true, TOOLS)
}

/// The stand-in for the system clipboard while this crate's own tests run.
#[cfg(test)]
mod fake {
    use std::cell::RefCell;

    thread_local! {
        static COPIED: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
    }

    pub fn record(text: &str) -> Result<&'static str, String> {
        COPIED.with(|c| c.borrow_mut().push(text.to_string()));
        Ok("test clipboard")
    }

    /// What this test's thread copied so far.
    #[cfg(unix)]
    pub fn copied() -> Vec<String> {
        COPIED.with(|c| c.borrow().clone())
    }
}

/// A clipboard tool: its name, its arguments, and the environment variable
/// that must be set for it to reach a display (empty: none needed).
pub type Tool = (&'static str, &'static [&'static str], &'static str);

/// In the order tried. `xclip` and `xsel` also work under XWayland, so they
/// only need *some* display variable.
pub const TOOLS: &[Tool] = &[
    ("wl-copy", &[], "WAYLAND_DISPLAY"),
    ("xclip", &["-selection", "clipboard"], "DISPLAY"),
    ("xsel", &["--clipboard", "--input"], "DISPLAY"),
    #[cfg(target_os = "macos")]
    ("pbcopy", &[], ""),
];

/// The chain, with the system clipboard switchable off for tests.
pub fn copy_with(text: &str, system: bool, tools: &[Tool]) -> Result<&'static str, String> {
    let mut via: Option<&'static str> = None;
    let mut failures: Vec<String> = Vec::new();
    if system {
        match system_clipboard(text) {
            Ok(()) => via = Some("clipboard"),
            Err(e) => failures.push(format!("clipboard: {e}")),
        }
    }
    // A tool that forks keeps the content after nexo exits, so it is fed even
    // when the system clipboard took the text already.
    for (name, args, needs) in tools {
        if !needs.is_empty() && std::env::var_os(needs).is_none_or(|v| v.is_empty()) {
            continue;
        }
        match run_tool(name, args, text) {
            Ok(()) => {
                via.get_or_insert(name);
                break;
            }
            Err(e) => {
                if !e.is_empty() {
                    failures.push(format!("{name}: {e}"));
                }
            }
        }
    }
    // The terminal may honour it even where nothing else can (ssh, tmux).
    let osc = osc52(text);
    if via.is_none() && osc.is_ok() {
        via = Some("terminal (OSC 52)");
    }
    match via {
        Some(v) => Ok(v),
        None => {
            let why = if failures.is_empty() { "no clipboard reachable".to_string() } else { failures.join("; ") };
            Err(format!(
                "{why}. Install wl-clipboard or xclip, or use /settings mouse and select with the terminal (Shift+drag)"
            ))
        }
    }
}

/// One instance for the whole run (see the module doc).
/// The one system clipboard instance, opened on first use and kept (see the module's note).
fn clipboard_cell() -> &'static Mutex<Option<arboard::Clipboard>> {
    static CLIPBOARD: OnceLock<Mutex<Option<arboard::Clipboard>>> = OnceLock::new();
    CLIPBOARD.get_or_init(|| Mutex::new(None))
}

/// What Ctrl+V finds in the clipboard.
#[derive(Debug, Clone, PartialEq)]
pub enum Pasted {
    /// An image, as PNG bytes.
    Image(Vec<u8>),
    Text(String),
}

/// Tools that print the clipboard's image as PNG, for when the system clipboard cannot be
/// read directly (Wayland, a terminal without a display connection): `(name, args, needs)`.
pub const IMAGE_TOOLS: &[Tool] = &[
    ("wl-paste", &["--no-newline", "--type", "image/png"], "WAYLAND_DISPLAY"),
    ("xclip", &["-selection", "clipboard", "-t", "image/png", "-o"], "DISPLAY"),
    #[cfg(target_os = "macos")]
    ("pngpaste", &["-"], ""),
];

/// The clipboard's content: an image first (screenshots), else text.
pub fn paste() -> Result<Pasted, String> {
    let mut why = Vec::new();
    match clipboard_cell().lock() {
        Ok(mut guard) => {
            if guard.is_none() {
                match arboard::Clipboard::new() {
                    Ok(c) => *guard = Some(c),
                    Err(e) => why.push(format!("clipboard: {e}")),
                }
            }
            if let Some(cb) = guard.as_mut() {
                match cb.get_image() {
                    Ok(img) => return encode_png(img.width as u32, img.height as u32, &img.bytes).map(Pasted::Image),
                    Err(e) => why.push(format!("no image in the clipboard ({e})")),
                }
                if let Ok(t) = cb.get_text() {
                    if !t.is_empty() {
                        return Ok(Pasted::Text(t));
                    }
                }
            }
        }
        Err(_) => why.push("clipboard lock poisoned".into()),
    }
    for (name, args, needs) in IMAGE_TOOLS {
        if !needs.is_empty() && std::env::var_os(needs).is_none_or(|v| v.is_empty()) {
            continue;
        }
        if let Ok(out) = Command::new(name).args(*args).stderr(Stdio::null()).output() {
            if out.status.success() && out.stdout.starts_with(b"\x89PNG") {
                return Ok(Pasted::Image(out.stdout));
            }
        }
    }
    Err(why.join("; "))
}

/// RGBA pixels as a PNG file.
pub fn encode_png(width: u32, height: u32, rgba: &[u8]) -> Result<Vec<u8>, String> {
    let img = image::RgbaImage::from_raw(width, height, rgba.to_vec())
        .ok_or_else(|| format!("the clipboard's image is not {width}×{height} RGBA"))?;
    let mut out = std::io::Cursor::new(Vec::new());
    img.write_to(&mut out, image::ImageFormat::Png).map_err(|e| e.to_string())?;
    Ok(out.into_inner())
}

/// Keeps a pasted image in the cache directory; returns its path.
pub fn save_pasted(png: &[u8]) -> Result<std::path::PathBuf, String> {
    let dir = nexo_core::paths::cache_dir().join("pastes");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let now = time::OffsetDateTime::now_utc();
    let name = format!(
        "paste-{:04}{:02}{:02}-{:02}{:02}{:02}-{:03}.png",
        now.year(),
        u8::from(now.month()),
        now.day(),
        now.hour(),
        now.minute(),
        now.second(),
        now.millisecond()
    );
    let f = dir.join(name);
    std::fs::write(&f, png).map_err(|e| e.to_string())?;
    Ok(f)
}

fn system_clipboard(text: &str) -> Result<(), String> {
    let cell = clipboard_cell();
    let mut guard = cell.lock().map_err(|_| "clipboard lock poisoned".to_string())?;
    if guard.is_none() {
        *guard = Some(arboard::Clipboard::new().map_err(|e| e.to_string())?);
    }
    let cb = guard.as_mut().ok_or_else(|| "no clipboard".to_string())?;
    cb.set_text(text.to_string()).map_err(|e| {
        // a broken instance is dropped so the next copy opens a fresh one
        *guard = None;
        e.to_string()
    })
}

/// Pipes `text` into the tool. An absent tool is `Err("")` (nothing to
/// report); a tool that fails says so. The tool's own output is discarded so
/// it never writes over the interface.
fn run_tool(name: &str, args: &[&str], text: &str) -> Result<(), String> {
    let mut child =
        match Command::new(name).args(args).stdin(Stdio::piped()).stdout(Stdio::null()).stderr(Stdio::null()).spawn() {
            Ok(c) => c,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Err(String::new()),
            Err(e) => return Err(e.to_string()),
        };
    if let Some(mut stdin) = child.stdin.take() {
        // a closed pipe (the tool exited early) is reported by the status below
        let _ = stdin.write_all(text.as_bytes());
    }
    let start = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                return if status.success() { Ok(()) } else { Err(format!("exit {}", status.code().unwrap_or(-1))) };
            }
            Ok(None) if start.elapsed() > Duration::from_secs(3) => {
                let _ = child.kill();
                // reaped, so a tool that hung leaves no zombie behind
                let _ = child.wait();
                return Err("did not finish".into());
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(10)),
            Err(e) => return Err(e.to_string()),
        }
    }
}

/// The OSC 52 sequence for `text`, wrapped for tmux when running inside it.
/// Terminals cap the sequence (about 100 KB in most), so longer texts are
/// not sent this way.
pub fn osc52_sequence(text: &str, in_tmux: bool) -> Option<Vec<u8>> {
    const LIMIT: usize = 100_000;
    let enc = base64::engine::general_purpose::STANDARD.encode(text.as_bytes());
    if enc.len() > LIMIT {
        return None;
    }
    let seq = format!("\x1b]52;c;{enc}\x07");
    Some(if in_tmux {
        // DCS passthrough: ESC doubled inside, closed with ST
        format!("\x1bPtmux;{}\x1b\\", seq.replace('\x1b', "\x1b\x1b")).into_bytes()
    } else {
        seq.into_bytes()
    })
}

fn osc52(text: &str) -> Result<(), String> {
    use std::io::IsTerminal;
    let seq = osc52_sequence(text, std::env::var_os("TMUX").is_some()).ok_or("too long for OSC 52")?;
    // the real terminal, not stdout: it may be redirected, and the interface's
    // own buffered frame must not be interleaved with the sequence
    let mut tty: Box<dyn Write> = match std::fs::OpenOptions::new().write(true).open("/dev/tty") {
        Ok(f) => Box::new(f),
        Err(_) if std::io::stdout().is_terminal() => Box::new(std::io::stdout()),
        Err(e) => return Err(format!("no terminal for OSC 52: {e}")),
    };
    tty.write_all(&seq).map_err(|e| e.to_string())?;
    tty.flush().map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    use crate::ENV_LOCK as ENV;

    #[cfg(unix)]
    fn fake_tools(dir: &std::path::Path) -> (std::path::PathBuf, std::path::PathBuf) {
        use std::os::unix::fs::PermissionsExt;
        let out = dir.join("xclip.out");
        let wl = dir.join("wl-copy");
        std::fs::write(&wl, "#!/bin/sh\nexit 1\n").unwrap();
        let xclip = dir.join("xclip");
        // the test narrows PATH to the fake tools, so the script names its own
        std::fs::write(&xclip, format!("#!/bin/sh\nPATH=/usr/bin:/bin cat > '{}'\n", out.display())).unwrap();
        for f in [&wl, &xclip] {
            std::fs::set_permissions(f, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        (dir.to_path_buf(), out)
    }

    /// Puts PATH and the display variables back when a test ends, so the next one sees the real ones.
    #[cfg(unix)]
    struct EnvBack(Vec<(&'static str, Option<std::ffi::OsString>)>);

    #[cfg(unix)]
    impl EnvBack {
        fn save() -> Self {
            EnvBack(["PATH", "DISPLAY", "WAYLAND_DISPLAY"].map(|k| (k, std::env::var_os(k))).into())
        }
    }

    #[cfg(unix)]
    impl Drop for EnvBack {
        fn drop(&mut self) {
            for (k, v) in &self.0 {
                match v {
                    Some(v) => std::env::set_var(k, v),
                    None => std::env::remove_var(k),
                }
            }
        }
    }

    /// `copy` in a test run records the text and starts no process: a tool on PATH that would
    /// leave a marker file must not run, and the system clipboard is not opened.
    #[cfg(unix)]
    #[test]
    fn copy_in_tests_goes_to_the_recorder_and_spawns_nothing() {
        use std::os::unix::fs::PermissionsExt;
        let _g = ENV.lock().unwrap_or_else(|e| e.into_inner());
        let dir = tempfile::tempdir().unwrap();
        let marker = dir.path().join("spawned");
        for tool in ["wl-copy", "xclip", "xsel"] {
            let f = dir.path().join(tool);
            std::fs::write(&f, format!("#!/bin/sh\ntouch '{}'\ncat >/dev/null\n", marker.display())).unwrap();
            std::fs::set_permissions(&f, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let back = EnvBack::save();
        std::env::set_var("PATH", format!("{}:/usr/bin:/bin", dir.path().display()));
        std::env::set_var("WAYLAND_DISPLAY", "wayland-0");
        std::env::set_var("DISPLAY", ":0");
        let got = copy("hello clipboard");
        drop(back);
        assert_eq!(got, Ok("test clipboard"));
        assert_eq!(fake::copied(), ["hello clipboard"]);
        assert!(!marker.exists(), "a clipboard tool was started by a test");
    }

    #[cfg(unix)]
    #[test]
    fn tries_the_tools_in_order_and_reports_the_one_that_took_the_text() {
        let _g = ENV.lock().unwrap_or_else(|e| e.into_inner());
        let dir = tempfile::tempdir().unwrap();
        let (bin, out) = fake_tools(dir.path());
        let back = EnvBack::save();
        std::env::set_var("PATH", &bin);
        std::env::set_var("WAYLAND_DISPLAY", "wayland-0");
        std::env::set_var("DISPLAY", ":0");
        let got = copy_with("hello\nworld", false, TOOLS);
        drop(back);
        // wl-copy fails, xclip takes it
        assert_eq!(got, Ok("xclip"));
        assert_eq!(std::fs::read_to_string(out).unwrap(), "hello\nworld");
    }

    #[cfg(unix)]
    #[test]
    fn a_tool_without_its_display_is_skipped_and_the_terminal_is_the_last_resort() {
        let _g = ENV.lock().unwrap_or_else(|e| e.into_inner());
        let dir = tempfile::tempdir().unwrap();
        let (bin, out) = fake_tools(dir.path());
        let back = EnvBack::save();
        std::env::set_var("PATH", &bin);
        std::env::remove_var("WAYLAND_DISPLAY");
        std::env::remove_var("DISPLAY");
        let got = copy_with("x", false, TOOLS);
        drop(back);
        assert!(!out.exists(), "xclip must not run without a display");
        // with a terminal the sequence counts; without one the error says what to install
        match got {
            Ok(how) => assert_eq!(how, "terminal (OSC 52)"),
            Err(e) => assert!(e.contains("Install wl-clipboard or xclip"), "{e}"),
        }
    }

    #[test]
    fn a_text_too_long_for_osc52_is_an_error_never_a_false_copy() {
        let got = copy_with(&"a".repeat(200_000), false, &[]);
        let e = got.expect_err("nothing took the text");
        assert!(e.contains("Install wl-clipboard or xclip"), "{e}");
    }

    #[test]
    fn osc52_is_base64_and_wrapped_for_tmux() {
        let plain = osc52_sequence("hi", false).unwrap();
        assert_eq!(plain, b"\x1b]52;c;aGk=\x07");
        let tmux = osc52_sequence("hi", true).unwrap();
        assert_eq!(tmux, b"\x1bPtmux;\x1b\x1b]52;c;aGk=\x07\x1b\\");
        assert!(osc52_sequence(&"a".repeat(200_000), false).is_none());
    }

    /// With a bare X display (xvfb-run) and xclip, a screenshot in the clipboard comes back as the
    /// same picture; elsewhere (no display, or a person's desktop) the test says why it skipped.
    #[cfg(unix)]
    #[test]
    fn ctrl_v_reads_an_image_from_a_real_x_clipboard() {
        // the copy tests narrow PATH to fake tools; this one needs the real xclip
        let _g = ENV.lock().unwrap_or_else(|e| e.into_inner());
        if std::env::var_os("DISPLAY").is_none() || Command::new("xclip").arg("-version").output().is_err() {
            eprintln!("no X display with xclip: skipped");
            return;
        }
        // a display with a window manager is someone's desktop: its clipboard is the person's (and
        // a clipboard manager may take it over), so only a bare display (xvfb-run, CI) is written to
        let wm = Command::new("xprop").args(["-root", "_NET_SUPPORTING_WM_CHECK"]).output();
        if !wm.is_ok_and(|o| o.status.success() && !String::from_utf8_lossy(&o.stdout).contains("window id")) {
            eprintln!("a desktop's display (or no xprop to tell): skipped, the person's clipboard is left alone");
            return;
        }
        let d = tempfile::tempdir().unwrap();
        let png = encode_png(3, 2, &[[10u8, 200, 30, 255]; 6].concat()).unwrap();
        let f = d.path().join("shot.png");
        std::fs::write(&f, &png).unwrap();
        // xclip serves the clipboard from the background until someone else owns it
        // the display may still be starting (xvfb-run): a few tries before it counts as absent
        let mut served = false;
        for _ in 0..20 {
            let st = Command::new("xclip").args(["-selection", "clipboard", "-t", "image/png", "-i"]).arg(&f).status();
            if st.is_ok_and(|s| s.success()) {
                served = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(250));
        }
        assert!(served, "xclip never reached the display");
        std::thread::sleep(Duration::from_millis(300));
        match paste().expect("the clipboard has the image") {
            Pasted::Image(bytes) => {
                let img = image::load_from_memory(&bytes).unwrap().to_rgba8();
                assert_eq!((img.width(), img.height(), img.get_pixel(2, 1).0), (3, 2, [10, 200, 30, 255]));
            }
            other => panic!("{other:?}"),
        }
    }
}
