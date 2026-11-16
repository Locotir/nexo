//! The DWARF of a macOS program. The linker leaves it out of the program: it stays in the object files the
//! program's debug map names (`N_OSO` stabs), and `dsymutil` gathers it into a dSYM bundle beside the program
//! (`prog.dSYM/Contents/Resources/DWARF/prog`; `cc -g` makes one by itself when it compiles and links in one step).
//! A dSYM whose UUID is the program's holds the program's own addresses, so its `__DWARF` sections serve as the
//! program's.

use std::path::{Path, PathBuf};

use crate::image::Image;

/// Biggest dSYM file read, as for any file.
const MAX_DSYM: u64 = 1 << 30;

/// The Mach-O name of a DWARF section named the ELF way: `.debug_info` is `__debug_info`, and a section name stops
/// at 16 bytes, so `.debug_str_offsets` is `__debug_str_offs`.
fn macho_name(name: &str) -> Option<String> {
    let rest = name.strip_prefix('.')?;
    Some(format!("__{rest}").chars().take(16).collect())
}

impl Image {
    /// The bytes of a DWARF section named the ELF way (`.debug_line`): the file's own section, its Mach-O form
    /// (`__DWARF,__debug_line`), or the one of the dSYM read with the program. Empty when there is none.
    pub fn debug_section(&self, name: &str) -> &[u8] {
        if let Some(s) = self.sections.iter().find(|s| s.name == name) {
            return self.section_bytes(s);
        }
        let (Some(m), Some(short)) = (self.macho.as_deref(), macho_name(name)) else {
            // an ELF program stripped of its DWARF: the separate debug file found beside it or by build id
            return self.debug_file_section(name);
        };
        if let Some(s) = self.sections.iter().find(|s| s.name.strip_prefix("__DWARF,") == Some(short.as_str())) {
            return self.section_bytes(s);
        }
        m.dwarf.iter().find(|(n, _)| *n == short).map_or(&[], |(_, b)| b.as_slice())
    }

    /// Where `dsymutil` and `cc -g` put the DWARF of the program at `prog`.
    pub fn dsym_path(prog: &Path) -> PathBuf {
        let mut bundle = prog.as_os_str().to_owned();
        bundle.push(".dSYM");
        PathBuf::from(bundle).join("Contents/Resources/DWARF").join(prog.file_name().unwrap_or_default())
    }

    /// Read the DWARF of the dSYM file at `path` when its UUID is this program's (a dSYM of another build would
    /// put lines at the wrong addresses). True when it was taken.
    pub fn attach_dsym(&mut self, path: &Path) -> bool {
        let Some(uuid) = self.macho.as_ref().and_then(|m| m.uuid.clone()) else { return false };
        if !std::fs::metadata(path).is_ok_and(|m| m.is_file() && m.len() <= MAX_DSYM) {
            return false;
        }
        let Ok(data) = std::fs::read(path) else { return false };
        let arch = if self.arm64 { "arm64" } else { "x86-64" };
        let Ok(d) = super::load_dsym(&data, Some(arch)) else { return false };
        if d.macho.as_ref().and_then(|m| m.uuid.as_ref()) != Some(&uuid) {
            return false;
        }
        let dwarf: Vec<(String, Vec<u8>)> = d
            .sections
            .iter()
            .filter_map(|s| {
                let name = s.name.strip_prefix("__DWARF,")?;
                let start = usize::try_from(s.file_off).ok()?;
                let bytes = data.get(start..start.checked_add(usize::try_from(s.file_size).ok()?)?)?;
                Some((name.to_string(), bytes.to_vec()))
            })
            .collect();
        match self.macho.as_mut() {
            Some(m) if !dwarf.is_empty() => {
                m.dwarf = dwarf;
                true
            }
            _ => false,
        }
    }

    /// The object files the debug map names, as the linker saw them (an archive member is `lib.a(member.o)`):
    /// where the DWARF of a program built with `-g` is when no dSYM holds it. Empty for a file that is not Mach-O,
    /// and for one built without `-g` or stripped.
    pub fn debug_map_objects(&self) -> &[String] {
        self.macho.as_ref().map_or(&[], |m| m.objects.as_slice())
    }

    /// Take the dSYM beside the program when the program carries no DWARF of its own.
    pub(crate) fn find_dsym(&mut self) {
        if !self.debug_section(".debug_info").is_empty() {
            return;
        }
        let at = Image::dsym_path(&self.path);
        if at.is_file() {
            self.attach_dsym(&at);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dwarf_names_are_cut_at_sixteen_bytes_as_mach_o_cuts_them() {
        assert_eq!(macho_name(".debug_info").as_deref(), Some("__debug_info"));
        assert_eq!(macho_name(".debug_str_offsets").as_deref(), Some("__debug_str_offs"));
        assert_eq!(macho_name(".debug_line_str").as_deref(), Some("__debug_line_str"));
        assert_eq!(macho_name("debug_info"), None);
    }

    #[test]
    fn a_dsym_beside_the_program_gives_its_dwarf_only_when_it_is_of_the_same_build() {
        use crate::tests_macho::{dsym, thin, DSYM_INFO, DSYM_OFFSETS};
        let dir = tempfile::tempdir().unwrap();
        let prog = dir.path().join("prog");
        std::fs::write(&prog, thin(false, false).bytes).unwrap();
        let bare = Image::open(&prog).unwrap();
        assert!(bare.debug_section(".debug_info").is_empty(), "no dSYM yet");
        let at = Image::dsym_path(&prog);
        std::fs::create_dir_all(at.parent().unwrap()).unwrap();
        std::fs::write(&at, dsym(false)).unwrap();
        let img = Image::open(&prog).unwrap();
        assert_eq!(img.debug_section(".debug_info"), DSYM_INFO);
        assert_eq!(img.debug_section(".debug_str_offsets"), DSYM_OFFSETS);
        assert!(img.debug_section(".debug_line").is_empty());
        std::fs::write(&at, dsym(true)).unwrap();
        let other = Image::open(&prog).unwrap();
        assert!(other.debug_section(".debug_info").is_empty(), "a dSYM of another build would put lines elsewhere");
    }

    #[test]
    fn a_dsym_read_as_a_program_is_still_refused_with_what_to_read() {
        let Err(e) = crate::macho::load(&crate::tests_macho::dsym(false), None) else {
            panic!("a dSYM was read as a program")
        };
        let e = e.to_string();
        assert!(e.contains("dSYM"), "{e}");
    }

    #[test]
    fn the_debug_map_lists_the_object_files_the_dwarf_is_in() {
        let mut b = crate::tests_macho::thin(false, false).bytes;
        let img = Image::from_bytes(b.clone(), PathBuf::from("/x")).unwrap();
        assert!(img.debug_map_objects().is_empty(), "the fixture has no debug map");
        // turn the first symbol (`_helper`) into an N_OSO stab: the debug map then names one object file
        let first = find_symtab(&b);
        b[first + 4] = 0x66;
        let img = Image::from_bytes(b, PathBuf::from("/x")).unwrap();
        assert_eq!(img.debug_map_objects(), ["_helper".to_string()]);
    }

    /// File offset of the first `nlist_64` of a thin file's LC_SYMTAB.
    fn find_symtab(b: &[u8]) -> usize {
        let word = |o: usize| u32::from_le_bytes(b[o..o + 4].try_into().unwrap()) as usize;
        let (mut at, n) = (32, word(16));
        for _ in 0..n {
            if word(at) == 0x2 {
                return word(at + 8);
            }
            at += word(at + 4);
        }
        panic!("no LC_SYMTAB")
    }

    #[test]
    fn the_dsym_of_a_program_is_in_the_bundle_beside_it() {
        assert_eq!(
            Image::dsym_path(Path::new("/w/build/prog")),
            PathBuf::from("/w/build/prog.dSYM/Contents/Resources/DWARF/prog")
        );
    }
}
