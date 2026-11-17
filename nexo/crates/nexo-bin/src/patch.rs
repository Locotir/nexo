//! Byte patches: a new copy by default, or the input itself after a backup (`--in-place`). Everything is checked
//! before anything is written.
use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
};

use serde_json::json;

use crate::{asm::disassembly, ops::hex, parse_addr, Error, Image, Output, Result, Row, Section};

pub enum PatchTarget<'a> {
    Address(u64),
    Function(&'a str),
}

pub enum PatchData<'a> {
    Bytes(&'a str),
    Asm(&'a str),
    Nop(usize),
}

const MAX_PATCH: usize = 262144;

fn bytes(text: &str) -> Result<Vec<u8>> {
    // Accept both compact hex and whitespace-separated bytes, but never wildcards.
    let compact: String = text.chars().filter(|c| !c.is_ascii_whitespace()).collect();
    if compact.is_empty()
        || compact.len() > MAX_PATCH * 2
        || !compact.len().is_multiple_of(2)
        || !compact.bytes().all(|c| c.is_ascii_hexdigit())
    {
        return Err(Error::Bad("patch bytes must be nonempty pairs of hex digits (at most 262144 bytes)".into()));
    }
    compact
        .as_bytes()
        .as_chunks::<2>()
        .0
        .iter()
        .map(|b| {
            u8::from_str_radix(std::str::from_utf8(b).unwrap_or(""), 16)
                .map_err(|_| Error::Bad("invalid hex byte".into()))
        })
        .collect()
}

fn address(img: &Image, target: PatchTarget<'_>) -> Result<u64> {
    match target {
        PatchTarget::Address(a) => Ok(a),
        PatchTarget::Function(text) => {
            let (name, off) = match text.rsplit_once('+') {
                Some((name, off)) => (name, parse_addr(off)?),
                None => (text, 0),
            };
            let f = crate::ops::resolve(img, name)?;
            f.start.checked_add(off).ok_or_else(|| Error::Bad("function offset overflow".into()))
        }
    }
}

/// What else a patch can ask for.
#[derive(Default, Clone, Copy)]
pub struct PatchOpts<'a> {
    /// The bytes the place must hold now (hex); the patch is refused when they differ.
    pub old: Option<&'a str>,
    /// Demand the old bytes: a single patch needs `old`, a line of a patch file needs its OLD_HEX.
    pub verify: bool,
    /// Change the input itself, after copying it to `<file>.orig` (which must not exist yet).
    pub in_place: bool,
}

/// One change: where, the new bytes and, when known, the bytes that must be there now.
struct Edit {
    at: u64,
    new: Vec<u8>,
    old: Option<Vec<u8>>,
}

/// Where an [`Edit`] lands in the file.
struct Spot<'a> {
    sec: &'a Section,
    start: usize,
    end: usize,
    /// The end of the section's file-backed bytes, as a file offset.
    backed_end: usize,
}

/// Where the patched bytes go.
struct Dest {
    path: PathBuf,
    /// `<file>.orig` for an in-place patch.
    backup: Option<PathBuf>,
}

fn destination(img: &Image, out: Option<&Path>, in_place: bool) -> Result<Dest> {
    let input = fs::canonicalize(&img.path).map_err(|e| Error::Io(format!("cannot resolve input: {e}")))?;
    if in_place {
        if out.is_some() {
            return Err(Error::Bad("give --out or --in-place, not both".into()));
        }
        let mut name = input.clone().into_os_string();
        name.push(".orig");
        let backup = PathBuf::from(name);
        if fs::symlink_metadata(&backup).is_ok() {
            return Err(Error::Bad(format!(
                "backup {} already exists; move it away first (it is never overwritten)",
                backup.display()
            )));
        }
        return Ok(Dest { path: input, backup: Some(backup) });
    }
    let out = out.ok_or_else(|| Error::Bad("give --out PATH or --in-place".into()))?;
    if out == img.path || fs::canonicalize(out).is_ok_and(|p| p == input) {
        return Err(Error::Bad("patch refuses OUT == IN; choose a new output path".into()));
    }
    if fs::symlink_metadata(out).is_ok() {
        return Err(Error::Bad(format!("patch output {} already exists; choose a new path", out.display())));
    }
    Ok(Dest { path: out.to_path_buf(), backup: None })
}

/// The section and file bytes an edit covers; refuses a place outside the file-backed bytes of a section, and old
/// bytes that are not there.
fn locate<'a>(img: &'a Image, e: &Edit) -> Result<Spot<'a>> {
    let at = e.at;
    if e.new.len() > MAX_PATCH {
        return Err(Error::Bad("patch exceeds 262144 bytes".into()));
    }
    let sec = img
        .section_at(at)
        .ok_or_else(|| Error::Bad(format!("{at:#x} is outside all loaded sections; no file-backed bytes to patch")))?;
    let backed = img.section_bytes(sec);
    let delta = at - sec.addr;
    let end = delta.checked_add(e.new.len() as u64);
    if end.is_none_or(|end| end > backed.len() as u64) {
        return Err(Error::Bad(format!(
            "patch at {at:#x} of {} bytes crosses section {} limits: memory [{:#x}, {:#x}), file-backed [{:#x}, {:#x}), file offsets [{:#x}, {:#x})",
            e.new.len(), sec.name, sec.addr, sec.addr.saturating_add(sec.size), sec.addr, sec.addr.saturating_add(backed.len() as u64), sec.file_off, sec.file_off.saturating_add(backed.len() as u64)
        )));
    }
    let start = (sec.file_off + delta) as usize;
    let end = start + e.new.len();
    if let Some(old) = &e.old {
        if old.len() != e.new.len() {
            return Err(Error::Bad(format!(
                "patch at {at:#x}: old has {} bytes and new has {}; they must match",
                old.len(),
                e.new.len()
            )));
        }
        if img.data[start..end] != old[..] {
            return Err(Error::Bad(format!(
                "refused: the bytes at {at:#x} are {} but the old bytes say {}; nothing was written",
                hex(&img.data[start..end]),
                hex(old)
            )));
        }
    }
    Ok(Spot { sec, start, end, backed_end: (sec.file_off + backed.len() as u64) as usize })
}

/// Write a modified copy to a previously nonexistent path. The input is never opened for writing.
pub fn patch(img: &Image, out: &Path, target: PatchTarget<'_>, data: PatchData<'_>) -> Result<Output> {
    patch_with(img, Some(out), target, data, PatchOpts::default())
}

/// [`patch`] with the extras of [`PatchOpts`]: with `in_place` there is no `out`; the input is changed after a backup.
pub fn patch_with(
    img: &Image,
    out: Option<&Path>,
    target: PatchTarget<'_>,
    data: PatchData<'_>,
    opts: PatchOpts<'_>,
) -> Result<Output> {
    let dest = destination(img, out, opts.in_place)?;
    if opts.verify && opts.old.is_none() {
        return Err(Error::Bad("--verify needs --old HEX, the bytes that must be there now".into()));
    }
    let at = address(img, target)?;
    let new = match data {
        PatchData::Bytes(s) => bytes(s)?,
        PatchData::Asm(s) => {
            img.require_x86("asm patch")?;
            crate::assemble(s, at, img.bits)?
        }
        PatchData::Nop(n) => {
            img.require_x86("nop patch")?;
            if n == 0 || n > MAX_PATCH {
                return Err(Error::Bad("nop length must be 1 to 262144".into()));
            }
            vec![0x90; n]
        }
    };
    let old = opts.old.map(bytes).transpose()?;
    let edit = Edit { at, new, old };
    let spot = locate(img, &edit)?;
    let (sec, start, end) = (spot.sec, spot.start, spot.end);
    let new = &edit.new;
    let offset = start as u64;
    let old = &img.data[start..end];
    let mut lines = vec![
        format!(
            "patch {} -> {}: section {}, address {at:#x}, file offset {offset:#x}",
            img.path.display(),
            dest.path.display(),
            sec.name
        ),
        format!("old: {}", hex(old)),
        format!("new: {}", hex(new)),
    ];
    lines.push("before:".into());
    lines.extend(patch_disassembly(img, &img.data[start..end.saturating_add(32).min(spot.backed_end)], at));
    lines.push("after:".into());
    let mut copy = img.data.clone();
    copy[start..end].copy_from_slice(new);
    lines.extend(patch_disassembly(img, &copy[start..end.saturating_add(32).min(spot.backed_end)], at));
    let meta = json!({"at": at, "file_offset": offset, "section": sec.name, "old": hex(old), "new": hex(new),
        "out": dest.path, "in_place": opts.in_place, "backup": dest.backup});
    finish(img, &dest, copy, lines, meta)
}

/// The most patches one patch file may hold.
const MAX_EDITS: usize = 100_000;

/// Apply every line of a patch file, or none. A line is `ADDR OLD_HEX NEW_HEX`, `ADDR NEW_HEX` or `ADDR nop N`;
/// `#` starts a comment. All lines are checked first (inside a section, old bytes there when given, no two lines
/// touching the same byte); only then is anything written. With `opts.verify` every line must give its OLD_HEX.
pub fn patch_file(img: &Image, out: Option<&Path>, text: &str, opts: PatchOpts<'_>) -> Result<Output> {
    let dest = destination(img, out, opts.in_place)?;
    if opts.old.is_some() {
        return Err(Error::Bad("--old is for one patch; a patch file gives OLD_HEX on each line".into()));
    }
    let edits = parse_patches(img, text, opts.verify)?;
    let spots = edits.iter().map(|e| locate(img, e)).collect::<Result<Vec<_>>>()?;
    let mut order: Vec<usize> = (0..edits.len()).collect();
    order.sort_by_key(|&i| spots[i].start);
    for pair in order.windows(2) {
        let (a, b) = (pair[0], pair[1]);
        if spots[b].start < spots[a].end {
            return Err(Error::Bad(format!(
                "patches overlap: {:#x} ({} bytes) and {:#x} ({} bytes) touch the same bytes; nothing was written",
                edits[a].at,
                edits[a].new.len(),
                edits[b].at,
                edits[b].new.len()
            )));
        }
    }
    let mut copy = img.data.clone();
    let mut lines = vec![format!(
        "patch {} -> {}: {} patches, all checked first",
        img.path.display(),
        dest.path.display(),
        edits.len()
    )];
    let mut items = Vec::new();
    for &i in &order {
        let (e, s) = (&edits[i], &spots[i]);
        let old = hex(&img.data[s.start..s.end]);
        lines.push(format!("{:#x} {} +{:#x}: {old} -> {}", e.at, s.sec.name, s.start, hex(&e.new)));
        items.push(json!({"at": e.at, "file_offset": s.start, "section": s.sec.name, "old": old, "new": hex(&e.new)}));
        copy[s.start..s.end].copy_from_slice(&e.new);
    }
    let meta = json!({"patches": items, "out": dest.path, "in_place": opts.in_place, "backup": dest.backup});
    finish(img, &dest, copy, lines, meta)
}

fn parse_patches(img: &Image, text: &str, verify: bool) -> Result<Vec<Edit>> {
    let mut edits = Vec::new();
    for (n, raw) in text.lines().enumerate() {
        let n = n + 1;
        let line = raw.split('#').next().unwrap_or("").trim();
        if line.is_empty() {
            continue;
        }
        let bad = |why: &str| Error::Bad(format!("patches line {n}: {why}"));
        if edits.len() >= MAX_EDITS {
            return Err(bad("too many patches (at most 100000)"));
        }
        let words: Vec<&str> = line.split_whitespace().collect();
        let at = crate::any_addr_in(img, words[0]).map_err(|e| bad(&e.to_string()))?;
        let edit = match words[1..] {
            [new] => Edit { at, new: bytes(new).map_err(|e| bad(&e.to_string()))?, old: None },
            [word, count] if word.eq_ignore_ascii_case("nop") => {
                img.require_x86("nop patch").map_err(|e| bad(&e.to_string()))?;
                let k = crate::parse_number(count)
                    .ok()
                    .and_then(|k| usize::try_from(k).ok())
                    .filter(|k| (1..=MAX_PATCH).contains(k))
                    .ok_or_else(|| bad("nop length must be 1 to 262144"))?;
                Edit { at, new: vec![0x90; k], old: None }
            }
            [old, new] => {
                let old = bytes(old).map_err(|e| bad(&e.to_string()))?;
                let new = bytes(new).map_err(|e| bad(&e.to_string()))?;
                if old.len() != new.len() {
                    return Err(bad(&format!("OLD_HEX has {} bytes and NEW_HEX has {}", old.len(), new.len())));
                }
                Edit { at, new, old: Some(old) }
            }
            _ => return Err(bad("expected `ADDR OLD_HEX NEW_HEX`, `ADDR NEW_HEX` or `ADDR nop N`")),
        };
        if verify && edit.old.is_none() {
            return Err(bad("--verify needs OLD_HEX on every line"));
        }
        edits.push(edit);
    }
    if edits.is_empty() {
        return Err(Error::Bad("the patch file holds no patches".into()));
    }
    Ok(edits)
}

/// Sign when needed, write the new file (or back up and replace the input) and build the answer.
fn finish(
    img: &Image,
    dest: &Dest,
    mut copy: Vec<u8>,
    mut lines: Vec<String>,
    meta: serde_json::Value,
) -> Result<Output> {
    // a patched Mach-O no longer matches the page hashes of its signature, and the kernel kills a program on arm64
    // whose pages do not match: sign the copy ad hoc again, as the linker did the first time
    if let Some(note) = resign_macho(img, &mut copy, &dest.path) {
        lines.push(note);
    }
    write_dest(img, dest, &copy)?;
    if let Some(b) = &dest.backup {
        lines.push(format!("backup: the original is kept as {}", b.display()));
    }
    let rows = lines
        .into_iter()
        .enumerate()
        .map(|(i, text)| Row { key: i as u64, json: Some(json!({"text": text})), text })
        .collect();
    Ok(Output { op: "patch", meta, rows, tail: 0, command: String::new() })
}

fn write_dest(img: &Image, dest: &Dest, copy: &[u8]) -> Result<()> {
    let permissions =
        fs::metadata(&img.path).map_err(|e| Error::Io(format!("cannot read input permissions: {e}")))?.permissions();
    if let Some(backup) = &dest.backup {
        // the backup first, from the bytes that were read and patched; create_new never replaces one
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(backup)
            .map_err(|e| Error::Io(format!("cannot create backup {}: {e}", backup.display())))?;
        if let Err(e) = file.write_all(&img.data).and_then(|()| file.set_permissions(permissions.clone())) {
            drop(file);
            let _ = fs::remove_file(backup);
            return Err(Error::Io(format!("cannot write backup {}: {e}", backup.display())));
        }
        drop(file);
        // then a temporary file beside the input, renamed over it: the input is whole or untouched
        let mut name = dest.path.clone().into_os_string();
        name.push(".nexo-tmp");
        let tmp = PathBuf::from(name);
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&tmp)
            .map_err(|e| Error::Io(format!("cannot create {}: {e}", tmp.display())))?;
        let done = file.write_all(copy).and_then(|()| file.set_permissions(permissions)).and_then(|()| {
            drop(file);
            fs::rename(&tmp, &dest.path)
        });
        if let Err(e) = done {
            let _ = fs::remove_file(&tmp);
            return Err(Error::Io(format!(
                "cannot write {} (backup kept as {}): {e}",
                dest.path.display(),
                backup.display()
            )));
        }
        return Ok(());
    }
    // create_new is atomic and refuses symlinks, hard links and destinations created since the check.
    let out = &dest.path;
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(out)
        .map_err(|e| Error::Io(format!("cannot create new output {}: {e}", out.display())))?;
    if let Err(e) = file.write_all(copy).and_then(|()| file.set_permissions(permissions)) {
        drop(file);
        let _ = fs::remove_file(out);
        return Err(Error::Io(format!("cannot write {}: {e}", out.display())));
    }
    Ok(())
}

fn patch_disassembly(img: &Image, bytes: &[u8], at: u64) -> Vec<String> {
    if !img.arm64 {
        return disassembly(img.bits, bytes, at, 12);
    }
    let mut lines = Vec::new();
    crate::a64::view::walk(bytes, at, |ins, _, raw| {
        if lines.len() < 12 {
            lines.push(format!("{:#x}  {:<32} {} {}", ins.pc, hex(raw), ins.mnemonic(), ins.operands()));
        }
    });
    lines
}

/// Sign the patched copy of a Mach-O program ad hoc, when the original was signed or runs on arm64 (where the kernel
/// refuses an unsigned program). Returns the line the answer carries, or `None` for a file that is not Mach-O or
/// needs no signature. The entitlements of the old signature are not carried over.
fn resign_macho(img: &Image, copy: &mut Vec<u8>, out: &Path) -> Option<String> {
    img.macho.as_ref()?;
    let before = crate::read_signature(&img.data);
    if !img.arm64 && before == crate::Signature::Unsigned {
        return None;
    }
    let ident = out.file_name().map_or_else(|| "program".to_string(), |n| n.to_string_lossy().into_owned());
    Some(match crate::adhoc_sign(copy, &ident, &[]) {
        Ok(signed) => {
            *copy = signed;
            format!(
                "signature: re-signed ad hoc as {ident} ({}; the old one was: {}; entitlements of the old signature are not carried over)",
                crate::read_signature(copy).describe(),
                before.describe()
            )
        }
        Err(e) => format!("signature: the copy is not signed ({e}); its old signature no longer matches the bytes"),
    })
}
