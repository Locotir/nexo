//! Copy-only byte patches. Validate the entire range before creating the destination.
use std::{fs, io::Write, path::Path};

use serde_json::json;

use crate::{asm::disassembly, ops::hex, parse_addr, Error, Image, Output, Result, Row};

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

/// Write a modified copy to a previously nonexistent path. The input is never opened for writing.
pub fn patch(img: &Image, out: &Path, target: PatchTarget<'_>, data: PatchData<'_>) -> Result<Output> {
    let input = fs::canonicalize(&img.path).map_err(|e| Error::Io(format!("cannot resolve input: {e}")))?;
    if out == img.path || fs::canonicalize(out).is_ok_and(|p| p == input) {
        return Err(Error::Bad("patch refuses OUT == IN; choose a new output path".into()));
    }
    if fs::symlink_metadata(out).is_ok() {
        return Err(Error::Bad(format!("patch output {} already exists; choose a new path", out.display())));
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
    if new.len() > MAX_PATCH {
        return Err(Error::Bad("patch exceeds 262144 bytes".into()));
    }
    let sec = img
        .section_at(at)
        .ok_or_else(|| Error::Bad(format!("{at:#x} is outside all loaded sections; no file-backed bytes to patch")))?;
    let backed = img.section_bytes(sec);
    let delta = at - sec.addr;
    let end = delta.checked_add(new.len() as u64);
    if end.is_none_or(|e| e > backed.len() as u64) {
        return Err(Error::Bad(format!(
            "patch at {at:#x} of {} bytes crosses section {} limits: memory [{:#x}, {:#x}), file-backed [{:#x}, {:#x}), file offsets [{:#x}, {:#x})",
            new.len(), sec.name, sec.addr, sec.addr.saturating_add(sec.size), sec.addr, sec.addr.saturating_add(backed.len() as u64), sec.file_off, sec.file_off.saturating_add(backed.len() as u64)
        )));
    }
    let offset = sec.file_off + delta;
    let start = offset as usize;
    let end = start + new.len();
    let old = &img.data[start..end];
    let mut lines = vec![
        format!(
            "patch {} -> {}: section {}, address {at:#x}, file offset {offset:#x}",
            img.path.display(),
            out.display(),
            sec.name
        ),
        format!("old: {}", hex(old)),
        format!("new: {}", hex(&new)),
    ];
    {
        lines.push("before:".into());
        lines.extend(patch_disassembly(
            img,
            &img.data[start..end.saturating_add(32).min((sec.file_off + backed.len() as u64) as usize)],
            at,
        ));
        lines.push("after:".into());
    }
    let mut copy = img.data.clone();
    copy[start..end].copy_from_slice(&new);
    {
        lines.extend(patch_disassembly(
            img,
            &copy[start..end.saturating_add(32).min((sec.file_off + backed.len() as u64) as usize)],
            at,
        ));
    }
    let permissions =
        fs::metadata(&img.path).map_err(|e| Error::Io(format!("cannot read input permissions: {e}")))?.permissions();
    // create_new is atomic and refuses symlinks, hard links and destinations created since the check.
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(out)
        .map_err(|e| Error::Io(format!("cannot create new output {}: {e}", out.display())))?;
    if let Err(e) = file.write_all(&copy).and_then(|()| file.set_permissions(permissions)) {
        drop(file);
        let _ = fs::remove_file(out);
        return Err(Error::Io(format!("cannot write {}: {e}", out.display())));
    }
    let rows = lines
        .into_iter()
        .enumerate()
        .map(|(i, text)| Row { key: i as u64, json: Some(json!({"text": text})), text })
        .collect();
    Ok(Output {
        op: "patch",
        meta: json!({"at": at, "file_offset": offset, "section": sec.name, "old": hex(old), "new": hex(&new), "out": out}),
        rows,
        tail: 0,
        command: String::new(),
    })
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
