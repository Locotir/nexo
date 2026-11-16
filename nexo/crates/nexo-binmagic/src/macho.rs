//! The Mach-O header fields the first-bytes probes read.

pub const MH_MAGIC_64: u32 = 0xfeed_facf;
pub const FAT_MAGIC: u32 = 0xcafe_babe;
pub const FAT_MAGIC_64: u32 = 0xcafe_babf;
pub const CPU_X86_64: u32 = 0x0100_0007;
pub const CPU_ARM64: u32 = 0x0100_000c;
/// The most slices of a universal file that are read.
pub const MAX_SLICES: usize = 64;

pub fn u32_at(b: &[u8], o: u64) -> Option<u32> {
    let o = usize::try_from(o).ok()?;
    Some(u32::from_le_bytes(b.get(o..o.checked_add(4)?)?.try_into().ok()?))
}

pub fn be32(b: &[u8], o: u64) -> Option<u32> {
    let o = usize::try_from(o).ok()?;
    Some(u32::from_be_bytes(b.get(o..o.checked_add(4)?)?.try_into().ok()?))
}

/// Whether the first bytes of a file are a Mach-O file `Image::open` reads: a thin x86-64 or arm64
/// program or library, or a universal file with such a slice.
pub fn opens_head(head: &[u8]) -> bool {
    match be32(head, 0) {
        Some(FAT_MAGIC | FAT_MAGIC_64) => {
            let n = be32(head, 4).unwrap_or(0) as u64;
            let entry = if be32(head, 0) == Some(FAT_MAGIC_64) { 32 } else { 20 };
            (1..=MAX_SLICES as u64).contains(&n)
                && !crate::is_managed(&head[..head.len().min(crate::MANAGED_HEAD)])
                && (0..n).any(|i| matches!(be32(head, 8 + i * entry), Some(CPU_X86_64 | CPU_ARM64)))
        }
        _ => {
            u32_at(head, 0) == Some(MH_MAGIC_64)
                && matches!(u32_at(head, 4), Some(CPU_X86_64 | CPU_ARM64))
                && matches!(u32_at(head, 12), Some(2 | 3 | 5 | 6 | 7 | 8 | 9 | 0xb))
        }
    }
}
