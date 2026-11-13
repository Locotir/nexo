//! Universal (fat) files and architecture choice: which slice to read and how it is named.

use super::*;

/// The architecture of a CPU type, as people write it.
pub(super) fn cpu_word(cpu: u32, sub: u32) -> String {
    match (cpu, sub & CPU_SUBTYPE_MASK) {
        (CPU_X86_64, CPU_SUBTYPE_X86_64_H) => "x86-64h",
        (CPU_X86_64, _) => "x86-64",
        (CPU_ARM64, CPU_SUBTYPE_ARM64E) => "arm64e",
        (CPU_ARM64, _) => "arm64",
        (0x0200_000c, _) => "arm64_32",
        (7, _) => "x86",
        (12, _) => "arm",
        (18, _) => "ppc",
        (0x0100_0012, _) => "ppc64",
        _ => return format!("cpu {cpu:#x}"),
    }
    .into()
}

/// `--arch` in the words `cpu_word` uses; `None` for a word that names no architecture read.
pub(super) fn arch_key(a: &str) -> Option<&'static str> {
    match a.trim().to_ascii_lowercase().replace('_', "-").as_str() {
        "x86-64" | "amd64" | "x64" => Some("x86-64"),
        "x86-64h" => Some("x86-64h"),
        "arm64" | "aarch64" => Some("arm64"),
        "arm64e" => Some("arm64e"),
        _ => None,
    }
}

fn family(word: &str) -> &str {
    match word {
        "x86-64h" => "x86-64",
        "arm64e" => "arm64",
        w => w,
    }
}

fn bad_arch(a: &str) -> Error {
    let a: String = a.chars().take(40).collect();
    Error::Usage(format!("--arch {a:?}: write x86-64 or arm64 (arm64e and x86-64h pick those slices exactly)"))
}

/// Read a thin Mach-O file or one slice of a universal one.
pub(crate) fn load(data: &[u8], arch: Option<&str>) -> Result<Image> {
    match be32(data, 0) {
        Some(FAT_MAGIC | FAT_MAGIC_64) => load_fat(data, arch),
        _ => {
            let img = load_thin(data, 0)?;
            if let Some(a) = arch {
                check_arch(&img, a)?;
            }
            Ok(img)
        }
    }
}

/// `--arch` on a file that is not universal: it must name the file's own architecture.
pub(crate) fn check_arch(img: &Image, a: &str) -> Result<()> {
    let key = arch_key(a).ok_or_else(|| bad_arch(a))?;
    if family(key) != img.arch_name() {
        return Err(Error::Usage(format!(
            "--arch {a}: this file holds {} code only; --arch picks a slice of a universal (fat) Mach-O file",
            img.arch_name()
        )));
    }
    Ok(())
}

fn load_fat(data: &[u8], arch: Option<&str>) -> Result<Image> {
    let wide = be32(data, 0) == Some(FAT_MAGIC_64);
    let cut = || Error::NotRead("not a readable Mach-O universal file: its slice table is cut short".into());
    let n = be32(data, 4).ok_or_else(cut)? as usize;
    if n == 0 {
        return Err(Error::NotRead("a Mach-O universal file with no slices: nothing to read".into()));
    }
    if n > MAX_SLICES {
        return Err(Error::NotRead(format!(
            "not a readable Mach-O universal file: it claims {n} slices (real ones have a handful)"
        )));
    }
    let entry = if wide { 32 } else { 20 };
    let mut slices = Vec::with_capacity(n);
    let mut cpus = Vec::with_capacity(n);
    for i in 0..n as u64 {
        let h = 8 + i * entry;
        let (cpu, sub) = (be32(data, h).ok_or_else(cut)?, be32(data, h + 4).ok_or_else(cut)?);
        let (off, size) = if wide {
            (be64(data, h + 8).ok_or_else(cut)?, be64(data, h + 16).ok_or_else(cut)?)
        } else {
            (u64::from(be32(data, h + 8).ok_or_else(cut)?), u64::from(be32(data, h + 12).ok_or_else(cut)?))
        };
        slices.push(Slice { arch: cpu_word(cpu, sub), off, size });
        cpus.push(cpu);
    }
    let list = slice_list(&slices);
    let pick = match arch {
        Some(a) => {
            let key = arch_key(a).ok_or_else(|| bad_arch(a))?;
            slices
                .iter()
                .position(|s| s.arch == key)
                .or_else(|| slices.iter().position(|s| family(&s.arch) == key))
                .ok_or_else(|| {
                    Error::Usage(format!("--arch {a}: this universal file holds {list}; pick one of those"))
                })?
        }
        None => cpus.iter().position(|&c| c == CPU_X86_64 || c == CPU_ARM64).ok_or_else(|| {
            Error::NotRead(format!(
                "Mach-O universal binary with {list}: none of them is x86-64 or arm64, the ones read"
            ))
        })?,
    };
    let s = &slices[pick];
    let start = usize::try_from(s.off).ok().filter(|&o| o < data.len()).ok_or_else(|| {
        Error::NotRead(format!(
            "not a readable Mach-O universal file: its {} slice starts at {:#x}, past the end of the file",
            s.arch, s.off
        ))
    })?;
    let end = start.saturating_add(usize::try_from(s.size).unwrap_or(usize::MAX)).min(data.len());
    let mut img = load_thin(&data[start..end], start as u64)
        .map_err(|e| prefix(e, &format!("the {} slice at {:#x}: ", s.arch, s.off)))?;
    if let Some(m) = img.macho.as_mut() {
        m.slices = slices;
        m.picked = pick;
        m.arch_arg = arch.map(|_| m.slices[pick].arch.clone());
    }
    Ok(img)
}

/// A load error with where the file is cut, when its headers promise bytes it does not have.
pub(crate) fn with_extent(e: Error, data: &[u8]) -> Error {
    match (e, crate::damage::extent_problem(data)) {
        (Error::NotRead(m), Some(p)) => Error::NotRead(format!("{m}; {p}")),
        (e, _) => e,
    }
}

/// The architectures of a universal file, the first 8 and a count of the rest.
fn slice_list(slices: &[Slice]) -> String {
    let mut s = slices.iter().take(8).map(|s| s.arch.as_str()).collect::<Vec<_>>().join(", ");
    if slices.len() > 8 {
        s.push_str(&format!(" and {} more", slices.len() - 8));
    }
    s
}

fn prefix(e: Error, p: &str) -> Error {
    match e {
        Error::NotRead(m) => Error::NotRead(format!("{p}{m}")),
        other => other,
    }
}
