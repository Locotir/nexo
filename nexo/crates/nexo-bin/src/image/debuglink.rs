//! Separate debug info of an ELF file. A distribution strips its programs and ships the DWARF and the full symbol
//! table in a `.debug` file beside them, found the way gdb finds it: by the build id in
//! `/usr/lib/debug/.build-id/xx/rest.debug`, or by the name and CRC in `.gnu_debuglink` in the program's folder, in
//! its `.debug` subfolder, or under `/usr/lib/debug` followed by the program's own folder. The file that was used
//! gives the DWARF sections to everything that asks the image for them, and the function names the program lost.

use std::path::{Path, PathBuf};

use object::{Object, ObjectSection};

use super::load_elf::load_elf;
use super::{Format, Image, Symbol};

/// Biggest debug file read, as for any file.
const MAX_DEBUG_FILE: u64 = 1 << 30;

/// Where the system keeps debug files, as gdb's `debug-file-directory`.
pub(super) fn global_dirs() -> Vec<PathBuf> {
    if cfg!(unix) {
        vec![PathBuf::from("/usr/lib/debug")]
    } else {
        Vec::new()
    }
}

/// The debug file an image read, and what was looked at to find it.
#[derive(Debug, Clone, Default)]
pub struct DebugLookup {
    /// `.gnu_debuglink`: the file name and the CRC-32 of the debug file.
    pub link: Option<(String, u32)>,
    /// The build id as hex.
    pub build_id: Option<String>,
    /// The debug file taken and how it was found (`build-id`, or `.gnu_debuglink` with its CRC checked).
    pub found: Option<(PathBuf, String)>,
    /// Each place looked at that gave nothing, with why.
    pub tried: Vec<String>,
    /// The `.debug_*` sections of the file taken, by their `.debug_info` names.
    pub(crate) sections: Vec<(String, Vec<u8>)>,
}

/// The CRC-32 `.gnu_debuglink` holds (the zlib one: reflected, polynomial 0xedb88320).
pub fn crc32(data: &[u8]) -> u32 {
    let mut table = [0u32; 256];
    for (i, slot) in table.iter_mut().enumerate() {
        let mut c = i as u32;
        for _ in 0..8 {
            c = if c & 1 != 0 { 0xedb8_8320 ^ (c >> 1) } else { c >> 1 };
        }
        *slot = c;
    }
    !data.iter().fold(!0u32, |c, &b| table[((c ^ u32::from(b)) & 0xff) as usize] ^ (c >> 8))
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

impl Image {
    /// What the separate debug file lookup found or tried; `None` when the file carries its own DWARF, or is not ELF,
    /// or names no debug file.
    pub fn debug_lookup(&self) -> Option<&DebugLookup> {
        self.debug_lookup.as_deref()
    }

    /// One fact for `info` and for the debugger's first answer: which debug file gave the names, lines and types, or
    /// where one was looked for and why none was taken.
    pub fn debug_info_line(&self) -> Option<String> {
        let l = self.debug_lookup()?;
        if let Some((path, how)) = &l.found {
            let names: Vec<&str> = l.sections.iter().map(|(n, _)| n.as_str()).collect();
            let shown: Vec<&str> = names.iter().copied().take(6).collect();
            let more = if names.len() > shown.len() { ", ..." } else { "" };
            return Some(format!(
                "debug info  read from {} (found by {how}; {} sections: {}{more})",
                path.display(),
                names.len(),
                shown.join(" ")
            ));
        }
        let mut what = Vec::new();
        if let Some((name, crc)) = &l.link {
            what.push(format!(".gnu_debuglink names {name} (crc {crc:#010x})"));
        }
        if let Some(id) = &l.build_id {
            what.push(format!("build id {id}"));
        }
        Some(format!(
            "debug info  none in the file and no separate file taken: {}; looked at: {}",
            what.join(", "),
            l.tried.join("; ")
        ))
    }

    /// Look for the separate debug file of an ELF program that carries no DWARF of its own, in the places gdb
    /// looks, with the system's debug folders given.
    pub(super) fn find_debug_file_in(&mut self, dirs: &[PathBuf]) {
        if self.format != Format::Elf || !self.debug_section(".debug_info").is_empty() {
            return;
        }
        let Ok(file) = object::File::parse(&self.data[..]) else { return };
        let link = file.gnu_debuglink().ok().flatten().map(|(n, c)| (String::from_utf8_lossy(n).into_owned(), c));
        let build_id = file.build_id().ok().flatten().map(hex);
        let want_id = file.build_id().ok().flatten().map(<[u8]>::to_vec);
        if link.is_none() && build_id.is_none() {
            return;
        }
        let mut look = DebugLookup { link: link.clone(), build_id: build_id.clone(), ..DebugLookup::default() };
        // the folder of the program as it really is, as gdb takes it: a link to it leaves its debug file in the target's folder
        let own = std::fs::canonicalize(&self.path).unwrap_or_else(|_| self.path.clone());
        let folder = own.parent().map(Path::to_path_buf).unwrap_or_default();

        let mut candidates: Vec<(PathBuf, bool)> = Vec::new();
        if let Some(id) = &build_id {
            if id.len() > 2 {
                for d in dirs {
                    candidates.push((d.join(".build-id").join(&id[..2]).join(format!("{}.debug", &id[2..])), true));
                }
            }
        }
        if let Some((name, _)) = &link {
            candidates.push((folder.join(name), false));
            candidates.push((folder.join(".debug").join(name), false));
            for d in dirs {
                let rel: PathBuf =
                    folder.components().filter(|c| matches!(c, std::path::Component::Normal(_))).collect();
                candidates.push((d.join(rel).join(name), false));
            }
        }
        for (path, by_id) in candidates {
            if path == own {
                continue;
            }
            match std::fs::metadata(&path) {
                Ok(m) if m.is_file() && m.len() <= MAX_DEBUG_FILE => {}
                Ok(_) => {
                    look.tried.push(format!("{} is not a file of a readable size", path.display()));
                    continue;
                }
                Err(_) => {
                    look.tried.push(format!("{} does not exist", path.display()));
                    continue;
                }
            }
            let Ok(bytes) = std::fs::read(&path) else {
                look.tried.push(format!("{} could not be read", path.display()));
                continue;
            };
            let how = if by_id {
                let theirs =
                    object::File::parse(&bytes[..]).ok().and_then(|f| f.build_id().ok().flatten().map(<[u8]>::to_vec));
                if theirs != want_id {
                    look.tried.push(format!("{} holds another build id", path.display()));
                    continue;
                }
                "build id".to_string()
            } else {
                let got = crc32(&bytes);
                let (_, crc) = link.as_ref().expect("a link candidate comes from a link");
                if got != *crc {
                    look.tried.push(format!("{} has crc {got:#010x}, not {crc:#010x}", path.display()));
                    continue;
                }
                format!(".gnu_debuglink, crc {got:#010x} matches")
            };
            if self.take_debug_file(&mut look, &bytes) {
                look.found = Some((path, how));
                break;
            }
            look.tried.push(format!("{} is not an ELF file with DWARF", path.display()));
        }
        self.debug_lookup = Some(Box::new(look));
    }

    /// Take the DWARF sections and the function names of the debug file `bytes`. False when it has no DWARF.
    fn take_debug_file(&mut self, look: &mut DebugLookup, bytes: &[u8]) -> bool {
        let Ok(file) = object::File::parse(bytes) else { return false };
        let mut sections: Vec<(String, Vec<u8>)> = Vec::new();
        for s in file.sections() {
            let Ok(name) = s.name() else { continue };
            // a compressed section (`.zdebug_info` or SHF_COMPRESSED) is stored expanded, under its plain name
            let plain = name.strip_prefix(".zdebug_").map_or_else(|| name.to_string(), |r| format!(".debug_{r}"));
            if !plain.starts_with(".debug_") {
                continue;
            }
            if let Ok(d) = s.uncompressed_data() {
                if !d.is_empty() {
                    sections.push((plain, d.into_owned()));
                }
            }
        }
        if !sections.iter().any(|(n, _)| n == ".debug_info") {
            return false;
        }
        look.sections = sections;
        // the names the strip took: the debug file keeps the whole symbol table
        let loaded = match file.is_64() {
            true => load_elf::<object::elf::FileHeader64<object::Endianness>>(bytes),
            false => load_elf::<object::elf::FileHeader32<object::Endianness>>(bytes),
        };
        if let Ok(theirs) = loaded {
            let have: std::collections::HashSet<u64> = self.symbols.iter().filter(|s| s.func).map(|s| s.addr).collect();
            let add: Vec<Symbol> =
                theirs.symbols.into_iter().filter(|s| s.func && s.addr != 0 && !have.contains(&s.addr)).collect();
            self.symbols.extend(add);
        }
        true
    }

    /// The bytes of a `.debug_*` section of the separate debug file taken, when there is one.
    pub(crate) fn debug_file_section(&self, name: &str) -> &[u8] {
        self.debug_lookup
            .as_deref()
            .and_then(|l| l.sections.iter().find(|(n, _)| n == name))
            .map_or(&[], |(_, b)| b.as_slice())
    }
}
