//! Opening a file: size checks, the kind of file, the hash, and the lookup tables built once it is read.

use std::collections::btree_map::Entry;
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

use object::{pe, Endianness, FileKind};
use sha2::{Digest, Sha256};

use super::load_elf::load_elf;
use super::load_pe::load_pe;
use super::names::{demangle, raw_of};
use super::probe::{describe_kind, explain, not_a_binary};
use super::{Format, Image, Symbol, MAX_FILE};
use crate::{Error, Result};

impl Image {
    /// Read the file and parse its headers.
    pub fn open(path: &Path) -> Result<Image> {
        Image::open_arch(path, None)
    }

    /// Read the file; `arch` picks the slice of a universal (fat) Mach-O file (`x86-64`, `arm64`).
    pub fn open_arch(path: &Path, arch: Option<&str>) -> Result<Image> {
        let meta = std::fs::metadata(path).map_err(|e| Error::Io(format!("cannot read {}: {e}", path.display())))?;
        if !meta.is_file() {
            return Err(Error::NotRead(format!("{} is not a file", path.display())));
        }
        if meta.len() > MAX_FILE {
            return Err(Error::NotRead(format!(
                "{} is {} bytes; files over 1 GiB are not read",
                path.display(),
                meta.len()
            )));
        }
        let data = std::fs::read(path).map_err(|e| Error::Io(format!("cannot read {}: {e}", path.display())))?;
        Image::from_bytes_arch(data, path.to_path_buf(), arch)
    }

    /// Parse bytes already in memory.
    pub fn from_bytes(data: Vec<u8>, path: PathBuf) -> Result<Image> {
        Image::from_bytes_arch(data, path, None)
    }

    /// Parse bytes already in memory; `arch` as in [`Image::open_arch`].
    pub fn from_bytes_arch(data: Vec<u8>, path: PathBuf, arch: Option<&str>) -> Result<Image> {
        if crate::is_managed(&data[..data.len().min(crate::MANAGED_HEAD)]) {
            return Err(Error::NotRead(
                "this is JVM bytecode, a jar or a .NET assembly: read it with `nexo bin managed FILE`".into(),
            ));
        }
        let kind = FileKind::parse(&data[..]).map_err(|_| not_a_binary(&data))?;
        let mut img = match kind {
            FileKind::Elf64 => load_elf::<object::elf::FileHeader64<Endianness>>(&data).map_err(|e| explain(e, &data))?,
            FileKind::Elf32 => load_elf::<object::elf::FileHeader32<Endianness>>(&data).map_err(|e| explain(e, &data))?,
            FileKind::Pe64 => load_pe::<pe::ImageNtHeaders64>(&data).map_err(|e| explain(e, &data))?,
            FileKind::MachO64 | FileKind::MachO32 | FileKind::MachOFat32 | FileKind::MachOFat64 => {
                crate::macho::load(&data, arch).map_err(|e| crate::macho::with_extent(e, &data))?
            }
            FileKind::Pe32 if crate::is_mixed_mode(&data[..data.len().min(crate::MANAGED_HEAD)]) => {
                return Err(Error::NotRead(format!(
                    "{} is a mixed-mode .NET assembly (C++/CLI), not read natively: `nexo bin managed FILE` reads its .NET metadata and IL",
                    describe_kind(kind, &data)
                )))
            }
            FileKind::Pe32 => load_pe::<pe::ImageNtHeaders32>(&data).map_err(|e| explain(e, &data))?,
            other => return Err(Error::NotRead(format!("{} is not read yet", describe_kind(other, &data)))),
        };
        if let Some(w) = crate::damage::extent_problem(&data) {
            img.warnings.push(w);
        }
        if crate::is_mixed_mode(&data[..data.len().min(crate::MANAGED_HEAD)]) {
            img.warnings.push(
                "mixed-mode .NET assembly (C++/CLI): these native ops cover its native code; `nexo bin managed FILE` also reads its .NET metadata and IL"
                    .into(),
            );
        }
        if let (Some(a), None) = (arch, &img.macho) {
            crate::macho::check_arch(&img, a)?;
        }
        img.path = path;
        // a universal file: the hash is of the slice read, the one other tools name
        let (from, to) = img.macho.as_ref().map_or((0, data.len()), |m| (m.hashed.0, m.hashed.0 + m.hashed.1));
        let hashed = data.get(from..to).unwrap_or(&data);
        img.sha256 = Sha256::digest(hashed).iter().map(|b| format!("{b:02x}")).collect();
        if img.format == Format::Pe {
            img.attach_pdb(&data);
        }
        img.data = data;
        img.finish();
        Ok(img)
    }

    /// Read the .pdb next to the file: its function names come before the exported and public ones.
    fn attach_pdb(&mut self, data: &[u8]) {
        let Some(path) = crate::pdbread::find(&self.path, data) else { return };
        let p = crate::pdbread::read(&path, self.image_base);
        let mut named: Vec<Symbol> = Vec::new();
        for (addr, size, raw) in &p.funcs {
            let name = demangle(raw);
            named.push(Symbol { raw: raw_of(raw, &name), name, addr: *addr, size: *size, func: true });
        }
        self.symbols.splice(0..0, named);
        self.pdb = Some(p);
    }

    fn finish(&mut self) {
        // function names win over exports, exports over data names
        let mut names: BTreeMap<u64, String> = BTreeMap::new();
        // the size of the symbol that gave each name, so `symbol_covering` is a lookup
        let mut sizes: HashMap<u64, u64> = HashMap::new();
        for s in self.symbols.iter().filter(|s| s.func) {
            if let Entry::Vacant(v) = names.entry(s.addr) {
                v.insert(s.name.clone());
                sizes.insert(s.addr, s.size);
            }
        }
        for e in &self.exports {
            names.entry(e.addr).or_insert_with(|| e.name.clone());
        }
        // several data symbols can share an address (mingw puts `__data_start__` where `g_limit` is): the
        // sized one without a linker-style leading `__` is the name the program's author wrote
        let mut data: Vec<_> = self.symbols.iter().filter(|s| !s.func).collect();
        data.sort_by_key(|s| (s.size == 0, s.name.starts_with("__")));
        for s in data {
            if let Entry::Vacant(v) = names.entry(s.addr) {
                v.insert(s.name.clone());
                sizes.insert(s.addr, s.size);
            }
        }
        let mut raws: HashMap<u64, String> = HashMap::new();
        for s in self.symbols.iter().filter(|s| !s.raw.is_empty()) {
            raws.entry(s.addr).or_insert_with(|| s.raw.clone());
        }
        for e in self.exports.iter().filter(|e| !e.raw.is_empty()) {
            raws.entry(e.addr).or_insert_with(|| e.raw.clone());
        }
        for i in self.imports.iter().filter(|i| !i.raw.is_empty()) {
            if let Some(slot) = i.slot {
                raws.entry(slot).or_insert_with(|| i.raw.clone());
            }
        }
        self.raws = raws;
        self.names = names;
        self.sizes = sizes;
        self.slots = self.imports.iter().enumerate().filter_map(|(i, im)| im.slot.map(|s| (s, i))).collect();
        let mut sorted: Vec<usize> =
            (0..self.sections.len()).filter(|&i| self.sections[i].alloc && self.sections[i].size > 0).collect();
        sorted.sort_by_key(|&i| (self.sections[i].addr, i));
        self.sorted = sorted;
    }
}
