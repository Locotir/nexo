//! Telling what a file is before it is opened.

use std::path::Path;

use object::read::pe::ImageNtHeaders;
use object::{pe, Endianness, FileKind, LittleEndian as LE};

/// The bytes of a file's start that [`opens`] looks at.
const HEAD: u64 = 4096;

/// Whether `Image::open` would take the file at `path`: an ELF64, PE32+ or Mach-O 64-bit file (not a
/// relocatable object), or an ELF64 arm64 one. 32-bit x86 files open too but are left out here: the
/// hints this answer feeds lead to the debugger, which runs 64-bit programs only. Only the headers in the first 4096 bytes are read, as bytes: the file is not
/// run, not loaded and not read whole, so a large file costs the same as a small one. A path that
/// leaves through a symlink is read the same way. `false` for anything else, including a missing file.
/// The kind of file is asked before it is opened: opening a named pipe that nobody writes to never returns.
pub fn opens(path: &Path) -> bool {
    use object::read::elf::FileHeader;
    use std::io::Read;
    if !std::fs::metadata(path).is_ok_and(|m| m.is_file()) {
        return false;
    }
    let Ok(file) = std::fs::File::open(path) else { return false };
    if !file.metadata().is_ok_and(|m| m.is_file()) {
        return false;
    }
    let mut head = Vec::new();
    if file.take(HEAD).read_to_end(&mut head).is_err() {
        return false;
    }
    match FileKind::parse(&head[..]) {
        Ok(FileKind::Elf64) => object::elf::FileHeader64::<Endianness>::parse(&head[..]).is_ok_and(|h| {
            h.endian().is_ok_and(|e| {
                matches!(h.e_machine(e), object::elf::EM_X86_64 | object::elf::EM_AARCH64)
                    && h.e_type(e) != object::elf::ET_REL
            })
        }),
        Ok(FileKind::Pe64) => {
            let Ok(dos) = pe::ImageDosHeader::parse(&head[..]) else { return false };
            let mut at = u64::from(dos.nt_headers_offset());
            pe::ImageNtHeaders64::parse(&head[..], &mut at)
                .is_ok_and(|(nt, _)| nt.file_header().machine.get(LE) == pe::IMAGE_FILE_MACHINE_AMD64)
        }
        Ok(FileKind::MachO64 | FileKind::MachOFat32 | FileKind::MachOFat64) => crate::macho::opens_head(&head),
        _ => false,
    }
}
