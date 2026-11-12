//! Names and notes that stay with a file: `<data dir>/bin/<sha256 of the file>.json`.
//!
//! The data dir is given by the caller (the command line resolves it the way the rest of nexo does).
//! Writes take a lock on `<sha256>.lock`, read the file again under the lock, and replace the file
//! by renaming a temp file from the same folder, so two writers at once lose nothing.

use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use serde_json::{json, Map, Value};

use crate::{parse_addr, Error, Image, Result};

/// Entries (names and notes together) one file may hold.
pub const MAX_ENTRIES: usize = 20_000;
/// Longest name (characters).
pub const MAX_NAME: usize = 64;
/// Longest note (characters).
pub const MAX_NOTE: usize = 200;
/// A names file bigger than this is not read (bytes). 20 000 entries of the longest size need under 6 MB.
const MAX_FILE: u64 = 8 << 20;

/// What the person gave: names and notes by address.
#[derive(Debug, Clone, Default)]
pub struct UserNames {
    pub(crate) names: BTreeMap<u64, String>,
    pub(crate) notes: BTreeMap<u64, String>,
    /// C prototypes by function address.
    pub(crate) protos: BTreeMap<u64, String>,
    /// Struct layouts by name: `0:i32:id 8:ptr:next`.
    pub(crate) structs: BTreeMap<String, String>,
    /// Variable names by function address: `a1` -> `path`.
    pub(crate) vars: BTreeMap<u64, BTreeMap<String, String>>,
    /// Targets the debugger saw at an indirect call site: `0x401100 add1` (program code) or `libc.so.6!qsort`.
    pub(crate) targets: BTreeMap<u64, std::collections::BTreeSet<String>>,
}

impl UserNames {
    pub fn len(&self) -> usize {
        self.names.len() + self.notes.len()
    }
}

/// Where the names of the file with this SHA-256 live.
pub fn file_path(data_dir: &Path, sha256: &str) -> PathBuf {
    data_dir.join("bin").join(format!("{sha256}.json"))
}

fn bad(path: &Path, why: impl std::fmt::Display) -> Error {
    Error::Bad(format!("{}: {why}", path.display()))
}

/// A name the rules accept: 1 to 64 characters of `[A-Za-z0-9_.:$@<>~-]`, not `sub_` and hex digits.
pub fn check_name_syntax(name: &str) -> std::result::Result<(), String> {
    let n = name.chars().count();
    if n == 0 || n > MAX_NAME {
        return Err(format!("a name is 1 to {MAX_NAME} characters; this one has {n}"));
    }
    if let Some(c) = name.chars().find(|c| !(c.is_ascii_alphanumeric() || "_.:$@<>~-".contains(*c))) {
        return Err(format!("{c:?} is not allowed in a name; use letters, digits and _ . : $ @ < > ~ -"));
    }
    if name.strip_prefix("sub_").is_some_and(|rest| rest.chars().all(|c| c.is_ascii_hexdigit())) {
        return Err("names that look like sub_<hex> are the automatic ones; pick another".into());
    }
    Ok(())
}

/// One line, control characters removed, cut to 200 characters.
pub fn clean_note(text: &str) -> String {
    let mut s: String = text.chars().map(|c| if c == '\n' || c == '\r' || c == '\t' { ' ' } else { c }).collect();
    s.retain(|c| !c.is_control());
    s.trim().chars().take(MAX_NOTE).collect::<String>().trim_end().to_string()
}

/// Read the file for `sha256`. A missing file is empty names. A file that is cut, too big or not
/// what this module writes is an error that names the file.
pub fn load(data_dir: &Path, sha256: &str) -> Result<UserNames> {
    let path = file_path(data_dir, sha256);
    let meta = match std::fs::metadata(&path) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(UserNames::default()),
        Err(e) => return Err(bad(&path, format!("cannot be read ({e})"))),
    };
    if meta.len() > MAX_FILE {
        return Err(bad(&path, format!("is {} bytes; the limit is {MAX_FILE}", meta.len())));
    }
    let raw = std::fs::read(&path).map_err(|e| bad(&path, format!("cannot be read ({e})")))?;
    parse(&raw).map_err(|why| bad(&path, why))
}

fn parse(raw: &[u8]) -> std::result::Result<UserNames, String> {
    let v: Value = serde_json::from_slice(raw).map_err(|e| format!("is not valid JSON ({e})"))?;
    let obj = v.as_object().ok_or("is not a JSON object")?;
    let mut out = UserNames::default();
    for (field, notes) in [("names", false), ("notes", true)] {
        let Some(m) = obj.get(field) else { continue };
        let m = m.as_object().ok_or_else(|| format!("\"{field}\" is not an object"))?;
        for (k, val) in m {
            let addr = parse_addr(k).map_err(|_| format!("\"{k}\" in \"{field}\" is not an address"))?;
            let s = val.as_str().ok_or_else(|| format!("the value of \"{k}\" in \"{field}\" is not text"))?;
            if notes {
                out.notes.insert(addr, clean_note(s));
            } else {
                check_name_syntax(s).map_err(|why| format!("holds a bad name at {k}: {why}"))?;
                out.names.insert(addr, s.to_string());
            }
            if out.len() > MAX_ENTRIES {
                return Err(format!("has more than {MAX_ENTRIES} entries"));
            }
        }
    }
    if let Some(m) = obj.get("protos").and_then(Value::as_object) {
        for (k, val) in m {
            let addr = parse_addr(k).map_err(|_| format!("\"{k}\" in \"protos\" is not an address"))?;
            out.protos.insert(addr, val.as_str().unwrap_or_default().to_string());
        }
    }
    if let Some(m) = obj.get("structs").and_then(Value::as_object) {
        for (k, val) in m {
            out.structs.insert(k.clone(), val.as_str().unwrap_or_default().to_string());
        }
    }
    if let Some(m) = obj.get("vars").and_then(Value::as_object) {
        for (k, val) in m {
            let addr = parse_addr(k).map_err(|_| format!("\"{k}\" in \"vars\" is not an address"))?;
            let inner = val
                .as_object()
                .map(|o| o.iter().filter_map(|(a, b)| b.as_str().map(|s| (a.clone(), s.to_string()))).collect())
                .unwrap_or_default();
            out.vars.insert(addr, inner);
        }
    }
    if let Some(m) = obj.get("targets").and_then(Value::as_object) {
        for (k, val) in m {
            let addr = parse_addr(k).map_err(|_| format!("\"{k}\" in \"targets\" is not an address"))?;
            let set = val
                .as_array()
                .map(|a| a.iter().filter_map(|x| x.as_str().map(str::to_string)).collect())
                .unwrap_or_default();
            out.targets.insert(addr, set);
        }
    }
    Ok(out)
}

fn render(n: &UserNames, sha256: &str) -> String {
    let side = |m: &BTreeMap<u64, String>| -> Value {
        Value::Object(m.iter().map(|(a, s)| (format!("{a:#x}"), json!(s))).collect::<Map<String, Value>>())
    };
    serde_json::to_string_pretty(
        &json!({"version": 1, "sha256": sha256, "names": side(&n.names), "notes": side(&n.notes),
               "protos": side(&n.protos),
               "structs": n.structs,
               "targets": n.targets.iter().map(|(a, s)| (format!("{a:#x}"), json!(s))).collect::<Map<String, Value>>(),
               "vars": n.vars.iter().map(|(a, m)| (format!("{a:#x}"), json!(m))).collect::<Map<String, Value>>()}),
    )
    .unwrap_or_default()
}

static TEMP: AtomicU64 = AtomicU64::new(0);

/// Read, change under the lock, write. An unreadable file is never overwritten: the error comes back.
pub(crate) fn edit<R>(data_dir: &Path, sha256: &str, change: impl FnOnce(&mut UserNames) -> Result<R>) -> Result<R> {
    let path = file_path(data_dir, sha256);
    let dir = path.parent().unwrap_or(data_dir).to_path_buf();
    std::fs::create_dir_all(&dir).map_err(|e| bad(&dir, format!("cannot be created ({e})")))?;
    let lock_path = path.with_extension("lock");
    let lock = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&lock_path)
        .map_err(|e| bad(&lock_path, format!("cannot be opened ({e})")))?;
    lock.lock().map_err(|e| bad(&lock_path, format!("cannot be locked ({e})")))?;
    let mut cur = load(data_dir, sha256)?;
    let out = change(&mut cur)?;
    let tmp = dir.join(format!("{sha256}.json.{}-{}.tmp", std::process::id(), TEMP.fetch_add(1, Ordering::Relaxed)));
    let written = (|| -> std::io::Result<()> {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(render(&cur, sha256).as_bytes())?;
        f.sync_all()?;
        std::fs::rename(&tmp, &path)
    })();
    if let Err(e) = written {
        let _ = std::fs::remove_file(&tmp);
        return Err(bad(&path, format!("cannot be written ({e})")));
    }
    let _ = lock.unlock();
    Ok(out)
}

/// Load the names of this image from `data_dir` into it. On failure the image keeps no names and
/// the answer is a one-line reason to print on the last line.
pub fn attach(img: &mut Image, data_dir: &Path) -> Option<String> {
    img.add_sigs(crate::SigSet::load_dir(data_dir));
    match load(data_dir, &img.sha256) {
        Ok(n) => {
            img.user = n;
            None
        }
        Err(e) => Some(format!("names file not read: {e}")),
    }
}

fn check_clash(img: &Image, name: &str) -> std::result::Result<(), String> {
    let clash = img.symbols.iter().any(|s| s.name == name)
        || img.imports.iter().any(|i| i.name == name || name.strip_suffix("@plt") == Some(i.name.as_str()))
        || img.exports.iter().any(|e| e.name == name);
    if clash {
        Err(format!("{name:?} is already the name of a symbol or an import of this file; pick another"))
    } else {
        Ok(())
    }
}

fn check_addr(img: &Image, addr: u64) -> Result<()> {
    if img.section_at(addr).is_none() {
        return Err(Error::Bad(format!("{addr:#x} is not inside any section")));
    }
    Ok(())
}

/// Give `addr` a name (or replace it). Returns the previous name.
pub fn set_name(img: &Image, data_dir: &Path, addr: u64, name: &str) -> Result<Option<String>> {
    check_name_syntax(name).map_err(Error::Bad)?;
    if crate::ops::numeric(name) {
        return Err(Error::Bad(format!(
            "{name:?} would be read as an address, not a name; use a name with a letter in front, like n_{name}"
        )));
    }
    check_clash(img, name).map_err(Error::Bad)?;
    check_addr(img, addr)?;
    edit(data_dir, &img.sha256, |n| {
        if let Some((other, _)) = n.names.iter().find(|(a, s)| **a != addr && s.as_str() == name) {
            return Err(Error::Bad(format!("{name:?} is already the name of {other:#x}; pick another")));
        }
        if !n.names.contains_key(&addr) && n.len() >= MAX_ENTRIES {
            return Err(Error::Bad(format!("this file already has {MAX_ENTRIES} names and notes; clear some first")));
        }
        Ok(n.names.insert(addr, name.to_string()))
    })
}

/// Leave a note at `addr` (or replace it). Returns the text kept.
pub fn set_note(img: &Image, data_dir: &Path, addr: u64, text: &str) -> Result<String> {
    let note = clean_note(text);
    if note.is_empty() {
        return Err(Error::Bad("the note is empty; write some text, or use --clear".into()));
    }
    check_addr(img, addr)?;
    edit(data_dir, &img.sha256, |n| {
        if !n.notes.contains_key(&addr) && n.len() >= MAX_ENTRIES {
            return Err(Error::Bad(format!("this file already has {MAX_ENTRIES} names and notes; clear some first")));
        }
        n.notes.insert(addr, note.clone());
        Ok(note)
    })
}

/// What this file already holds, for a first look: notes that start with `crash` first, then the other notes,
/// then the names, each list cut to `max` entries with a count of the rest. Empty when there is nothing.
pub fn brief(img: &Image, max: usize) -> Vec<String> {
    let mut notes: Vec<(&u64, &String)> = img.user.notes.iter().collect();
    notes.sort_by_key(|(a, t)| (!t.starts_with("crash"), **a));
    let mut out = Vec::new();
    let mut cut = |label: &str, items: Vec<String>| {
        let more = items.len().saturating_sub(max);
        let mut line = items.into_iter().take(max).collect::<Vec<_>>().join("; ");
        if line.is_empty() {
            return;
        }
        if more > 0 {
            line.push_str(&format!(" (+{more} more: `nexo bin names`)"));
        }
        out.push(format!("{label}: {line}"));
    };
    cut("notes", notes.iter().map(|(a, t)| format!("{a:#x} {t}")).collect());
    cut("names", img.user.names.iter().map(|(a, n)| format!("{a:#x} {n}")).collect());
    out
}

/// Remove the name at `addr`. True when there was one.
pub fn clear_name(img: &Image, data_dir: &Path, addr: u64) -> Result<bool> {
    edit(data_dir, &img.sha256, |n| Ok(n.names.remove(&addr).is_some()))
}

/// Remove the note at `addr`. True when there was one.
pub fn clear_note(img: &Image, data_dir: &Path, addr: u64) -> Result<bool> {
    edit(data_dir, &img.sha256, |n| Ok(n.notes.remove(&addr).is_some()))
}

/// Most targets kept for one call site.
pub const MAX_TARGETS: usize = 16;

/// Remember that the indirect call at `site` was seen going to `target` (`0x401100 add1` or `lib!name`).
/// Returns false when the site already holds it or is full.
pub fn add_call_target(img: &Image, data_dir: &Path, site: u64, target: &str) -> Result<bool> {
    check_addr(img, site)?;
    let target = clean_note(target);
    edit(data_dir, &img.sha256, |n| {
        if !n.targets.contains_key(&site) && n.targets.len() >= MAX_ENTRIES {
            return Ok(false);
        }
        let set = n.targets.entry(site).or_default();
        if set.len() >= MAX_TARGETS && !set.contains(&target) {
            return Ok(false);
        }
        Ok(set.insert(target))
    })
}

/// The targets seen at an indirect call site, sorted.
pub fn call_targets(img: &Image, site: u64) -> Vec<String> {
    img.user.targets.get(&site).map(|s| s.iter().cloned().collect()).unwrap_or_default()
}
