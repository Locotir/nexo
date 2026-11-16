//! Bare code with no file format around it: a region a debugger dumped, shellcode, JIT output, an unpacked stub.
//! Nothing in the bytes says where they were loaded or what machine they are for, so the person says both
//! (`--raw --base ADDR --arch x86-64|x86-32|arm64`). The image is one section at that address, readable, writable
//! and executable as such memory usually is, and the entry is its first byte. Functions come from the entry, the
//! call targets and the prologues found after the end of other code (see `funcs`); strings from the same bytes.

use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use super::load::blank;
use super::{Format, Image, Section};
use crate::{Error, Result};

/// The architectures raw bytes are read as, in the words `--arch` takes: (arm64, bits).
fn arch_of(word: &str) -> Result<(bool, u32)> {
    match word.trim().to_ascii_lowercase().as_str() {
        "x86-64" | "x86_64" | "x64" | "amd64" => Ok((false, 64)),
        "x86-32" | "x86" | "i386" | "i686" | "x86_32" => Ok((false, 32)),
        "arm64" | "aarch64" => Ok((true, 64)),
        _ => Err(Error::Usage(format!("--arch {word:?} with --raw: the code is read as x86-64, x86-32 or arm64"))),
    }
}

impl Image {
    /// Read a file of bare code loaded at `base`; `arch` is `x86-64` (the default), `x86-32` or `arm64`.
    pub fn open_raw(path: &Path, base: u64, arch: Option<&str>) -> Result<Image> {
        let data = super::open::read_file(path)?;
        Image::from_raw(data, path.to_path_buf(), base, arch)
    }

    /// Bare code already in memory, loaded at `base`.
    pub fn from_raw(data: Vec<u8>, path: PathBuf, base: u64, arch: Option<&str>) -> Result<Image> {
        let (arm64, bits) = arch_of(arch.unwrap_or("x86-64"))?;
        if data.is_empty() {
            return Err(Error::NotRead(format!("{} is empty: there are no bytes to read as code", path.display())));
        }
        let len = data.len() as u64;
        let top = if bits == 32 { 1u64 << 32 } else { u64::MAX };
        if base.checked_add(len).is_none_or(|end| end > top) {
            return Err(Error::Usage(format!(
                "--base {base:#x}: {len} bytes from there pass the top of the {bits}-bit address space"
            )));
        }
        let mut img = blank(Format::Raw);
        img.arm64 = arm64;
        img.bits = bits;
        img.image_base = base;
        img.entry = base;
        img.sections.push(Section {
            index: 0,
            name: "raw".into(),
            addr: base,
            size: len,
            file_off: 0,
            file_size: len,
            read: true,
            write: true,
            exec: true,
            alloc: true,
        });
        img.sha256 = Sha256::digest(&data).iter().map(|b| format!("{b:02x}")).collect();
        img.path = path;
        img.data = data;
        img.finish();
        Ok(img)
    }

    /// The `--arch` word that reads this raw image again.
    pub(crate) fn raw_arch(&self) -> &'static str {
        match (self.arm64, self.bits) {
            (true, _) => "arm64",
            (false, 32) => "x86-32",
            _ => "x86-64",
        }
    }
}

/// The flags that open this image the same way again, for the next commands: `--raw --base 0x.. --arch ..` for
/// bare code, ` --arch arm64` for a slice picked from a universal Mach-O file, nothing otherwise.
pub(crate) fn reopen_flags(img: &Image) -> String {
    if img.format == Format::Raw {
        return format!(" --raw --base {:#x} --arch {}", img.image_base, img.raw_arch());
    }
    crate::macho::arch_flag(img)
}
