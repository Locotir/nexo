//! `--module NAME` and `--live`: read a module of the program a `nexo dbg` session is running. The session says
//! where the module's file is and where it is loaded; the image is opened from that file (or from the session's
//! `dump module`, the bytes now in memory) and moved to the load base, so every address in and out is a live one.
//! The channel is the one `nexo dbg` itself uses: the session's server on localhost, one command per call.

use std::path::{Path, PathBuf};

use nexo_bin::{Error, Image};

use super::effect::Effect;
use super::{BinOp, Common};

/// The most memory `--live` reads from the program (bytes).
pub(crate) const LIVE_CAP: u64 = 64 << 20;

/// One module of the program, as the session lists it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Module {
    pub path: String,
    pub base: u64,
    pub end: u64,
}

impl Module {
    /// What to add to a file address to get the live one: where the module is loaded minus where its file puts its
    /// first byte (`Image::image_base`). Wrapping, so a module loaded below its file base is a negative amount.
    #[cfg(test)]
    pub fn delta(&self, image_base: u64) -> u64 {
        self.base.wrapping_sub(image_base)
    }
}

/// The modules in the rows of the session's `modules` answer: `0x555555554000-0x555555559000 /path/to/file`.
pub(crate) fn parse_modules(rows: &[String]) -> Vec<Module> {
    let hex = |s: &str| u64::from_str_radix(s.trim().trim_start_matches("0x"), 16).ok();
    rows.iter()
        .filter_map(|r| {
            let (range, path) = r.trim().split_once(' ')?;
            let (lo, hi) = range.split_once('-')?;
            Some(Module { path: path.trim().to_string(), base: hex(lo)?, end: hex(hi)? })
        })
        .collect()
}

fn short(m: &Module) -> &str {
    m.path.rsplit(['/', '\\']).next().unwrap_or(&m.path)
}

/// The module `name` means, as `dump module` takes it: the file name (any case), else part of a file name or path.
pub(crate) fn pick<'a>(list: &'a [Module], name: &str) -> Result<&'a Module, String> {
    let low = name.to_lowercase();
    let mut hits: Vec<&Module> = list.iter().filter(|m| short(m).eq_ignore_ascii_case(name)).collect();
    if hits.is_empty() {
        hits = list
            .iter()
            .filter(|m| short(m).to_lowercase().contains(&low) || m.path.to_lowercase().contains(&low))
            .collect();
    }
    match hits.as_slice() {
        [m] => Ok(m),
        [] => {
            let some = list.iter().take(12).map(short).collect::<Vec<_>>().join(", ");
            Err(format!("no loaded module is named {name:?}; loaded: {some}"))
        }
        many => {
            let paths = many.iter().map(|m| m.path.as_str()).collect::<Vec<_>>().join(", ");
            Err(format!("{name:?} names {} modules: {paths}", many.len()))
        }
    }
}

/// Why an op cannot read a module of a running program, or None when it can. Also the checks on the flags.
pub(crate) fn check(op: &BinOp) -> Result<(), Error> {
    let why = if let Some(why) = op.no_module_reason() {
        why
    } else {
        match op.effect() {
            Effect::WritesFile => "patch writes a file, and nothing here writes the running program's memory",
            Effect::SavesNotes if matches!(op, BinOp::Crypto { .. }) => {
                "--save names things by file address, and the module is at live addresses"
            }
            Effect::SavesNotes => {
                "it saves by file address, and a module of a running program is read at live addresses"
            }
            Effect::ReadOnly if op.common().raw => "--raw reads bare bytes from a file, not a module",
            Effect::ReadOnly => return Ok(()),
        }
    };
    Err(Error::Usage(format!("--module cannot be used here: {why}")))
}

fn dbg_err(e: nexo_dbg::Error) -> Error {
    match e {
        nexo_dbg::Error::Usage(m) | nexo_dbg::Error::Fail(m) => Error::Usage(format!(
            "--module needs a running `nexo dbg` session (start one with `nexo dbg run PROGRAM`): {m}"
        )),
    }
}

/// One command to the session the way `nexo dbg` finds it (the only live one, or the one whose program runs).
fn ask(words: &[&str]) -> Result<nexo_dbg::Reply, Error> {
    let dir = nexo_dbg::state_dir();
    let id = nexo_dbg::pick_session(&dir, &None).map_err(dbg_err)?;
    let words: Vec<String> = words.iter().map(|w| w.to_string()).collect();
    nexo_dbg::request(&dir, &id, &words).map_err(dbg_err)
}

/// A private folder for one `--live` call, removed with its content when it is dropped, on every exit path. Its
/// path cannot be guessed (pid, clock, counter), it is made with mode 0700 and by `create_dir`, which refuses a
/// path that exists, so a planted link cannot take the dump. The call also saves its names and decompiler answers
/// here, so nothing of a live image is filed under a key of the file on disk.
pub(crate) struct Scratch(PathBuf);

impl Scratch {
    fn new(data_dir: &Path) -> Result<Scratch, Error> {
        use std::sync::atomic::{AtomicU64, Ordering};
        static N: AtomicU64 = AtomicU64::new(0);
        let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos());
        let parent = data_dir.join("live");
        let dir = parent.join(format!("{}-{nanos:x}-{}", std::process::id(), N.fetch_add(1, Ordering::Relaxed)));
        // `recursive` for the parent only: the data folder may not exist yet (a first run), the call's own must be new
        let make = |p: &Path, recursive: bool| -> std::io::Result<()> {
            let mut b = std::fs::DirBuilder::new();
            b.recursive(recursive);
            #[cfg(unix)]
            {
                use std::os::unix::fs::DirBuilderExt;
                b.mode(0o700);
            }
            b.create(p)
        };
        // the parent may exist already; the folder of this call must not
        if !parent.is_dir() {
            if let Err(e) = make(&parent, true) {
                if !parent.is_dir() {
                    return Err(Error::Io(format!("cannot make {}: {e}", parent.display())));
                }
            }
        }
        make(&dir, false).map_err(|e| Error::Io(format!("cannot make {}: {e}", dir.display())))?;
        Ok(Scratch(dir))
    }

    pub fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// The image of module `name`, not yet moved, the address its module is loaded at, and with `--live` the private
/// folder the call works in (keep it until the answer is made).
pub(crate) fn open(c: &Common, name: &str, data_dir: &Path) -> Result<(Image, u64, Option<Scratch>), Error> {
    let reply = ask(&["modules"])?;
    let rows: Vec<String> = reply.out.rows.iter().map(|r| r.text.clone()).collect();
    let list = parse_modules(&rows);
    let m = pick(&list, name).map_err(|e| Error::Usage(format!("--module: {e}")))?;
    if !c.live {
        let img = Image::open_arch(Path::new(&m.path), c.arch.as_deref())?;
        return Ok((img, m.base, None));
    }
    let span = m.end.saturating_sub(m.base);
    if span > LIVE_CAP {
        return Err(Error::Usage(format!(
            "--live: {} spans {span} bytes; at most {} MiB are read from the program",
            m.path,
            LIVE_CAP >> 20
        )));
    }
    let scratch = Scratch::new(data_dir)?;
    let out = scratch.path().join("module.bin");
    let wrote = ask(&["dump", "module", short(m), &out.display().to_string()]);
    let unread = wrote.as_ref().ok().and_then(|r| r.out.meta["unread"].as_u64()).unwrap_or(0);
    let mut img = wrote.and_then(|_| Image::open_arch(&out, c.arch.as_deref()))?;
    let _ = std::fs::remove_file(&out);
    img.path = PathBuf::from(&m.path);
    if unread > 0 {
        img.warnings.push(format!("{unread} bytes of the module could not be read from memory and show as zeros"));
    }
    Ok((img, m.base, Some(scratch)))
}

#[cfg(test)]
mod tests {
    use super::*;

    const BASE: u64 = 0x5555_5555_4000;

    fn rows() -> Vec<String> {
        [
            "0x555555554000-0x555555559000 /usr/bin/prog",
            "0x7ffff7c00000-0x7ffff7e28000 /usr/lib/libc.so.6",
            "0x7ffff7fc3000-0x7ffff7fc5000 /usr/lib/ld-linux-x86-64.so.2",
            "not a module row",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect()
    }

    #[test]
    fn the_rows_of_the_session_become_modules() {
        let l = parse_modules(&rows());
        assert_eq!(l.len(), 3);
        assert_eq!(l[0], Module { path: "/usr/bin/prog".into(), base: BASE, end: 0x5555_5555_9000 });
        assert_eq!(l[1].base, 0x7fff_f7c0_0000);
    }

    #[test]
    fn a_file_address_and_a_live_address_convert_both_ways() {
        let m = Module { path: "/usr/bin/prog".into(), base: BASE, end: BASE + 0x5000 };
        // a position independent file: its first byte is address 0
        let delta = m.delta(0);
        assert_eq!(delta, BASE);
        let (file, live) = (0x1139u64, BASE + 0x1139);
        assert_eq!(file.wrapping_add(delta), live);
        assert_eq!(live.wrapping_sub(delta), file);
        // a file linked at 0x400000 and loaded at BASE
        let delta = m.delta(0x40_0000);
        assert_eq!(0x40_1139u64.wrapping_add(delta), BASE + 0x1139);
        assert_eq!((BASE + 0x1139).wrapping_sub(delta), 0x40_1139);
        // a module loaded below its file base is a negative amount that still round trips
        let low = Module { path: "x".into(), base: 0x1000, end: 0x2000 };
        let delta = low.delta(0x40_0000);
        assert_eq!(0x40_1139u64.wrapping_add(delta), 0x2139);
        assert_eq!(0x2139u64.wrapping_sub(delta), 0x40_1139);
    }

    #[test]
    fn a_name_picks_one_module() {
        let l = parse_modules(&rows());
        assert_eq!(pick(&l, "LIBC.so.6").unwrap().base, 0x7fff_f7c0_0000);
        assert_eq!(pick(&l, "prog").unwrap().path, "/usr/bin/prog");
        assert_eq!(pick(&l, "ld-linux").unwrap().end, 0x7fff_f7fc_5000);
        let none = pick(&l, "libm").unwrap_err();
        assert!(none.contains("no loaded module is named \"libm\"") && none.contains("libc.so.6"), "{none}");
        let many = pick(&l, "/usr/").unwrap_err();
        assert!(many.contains("names 3 modules"), "{many}");
    }
}
