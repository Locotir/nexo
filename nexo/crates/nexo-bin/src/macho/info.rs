//! What the Mach-O header says, as text and JSON.

use super::*;

/// Whether the first bytes of a file are a Mach-O file `Image::open` reads: a thin x86-64 or arm64
/// program or library, or a universal file with such a slice.
pub(crate) fn opens_head(head: &[u8]) -> bool {
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

/// ` --arch arm64` when the person picked a slice, for the next commands; empty otherwise.
pub(crate) fn arch_flag(img: &Image) -> String {
    img.macho.as_ref().and_then(|m| m.arch_arg.as_deref()).map_or(String::new(), |a| format!(" --arch {a}"))
}

/// The lines `info` adds for a Mach-O file.
pub(crate) fn info_lines(img: &Image) -> Vec<String> {
    let Some(m) = img.macho.as_deref() else { return Vec::new() };
    let mut l = Vec::new();
    let mut kind = m.kind.to_string();
    if let Some(p) = &m.platform {
        kind.push_str(&format!(" for {p}"));
    }
    if let Some(id) = &m.id {
        kind.push_str(&format!(", install name {id}"));
    }
    let main = m.main.map_or("no LC_MAIN".into(), |a| format!("LC_MAIN main at {a:#x}"));
    l.push(format!("macho       {kind}; {main}; fixups {}; {} function starts listed", m.fixups, m.starts.len()));
    if !m.libs.is_empty() {
        let shown: Vec<String> = m.libs.iter().take(12).map(|p| short_lib(p)).collect();
        let more = if m.libs.len() > 12 { format!(" ... {} more", m.libs.len() - 12) } else { String::new() };
        l.push(format!("libraries   {}{more}", shown.join(", ")));
    }
    if !m.slices.is_empty() {
        let list: Vec<String> =
            m.slices
                .iter()
                .enumerate()
                .map(|(i, s)| {
                    if i == m.picked {
                        format!("{} (read; the sha256 is of this slice)", s.arch)
                    } else {
                        s.arch.clone()
                    }
                })
                .collect();
        let other = m.slices.iter().enumerate().find(|&(i, s)| i != m.picked && arch_key(&s.arch).is_some());
        let hint = other.map_or(String::new(), |(_, s)| {
            format!("; read another: {}", crate::ops::command_arch(img, "info", &s.arch))
        });
        l.push(format!("slices      universal file: {}{hint}", list.join(", ")));
    }
    if let Some(u) = &m.uuid {
        l.push(format!("uuid        {u}"));
    }
    l
}

/// The same facts for `info --json`.
pub(crate) fn info_json(img: &Image) -> Option<Value> {
    let m = img.macho.as_deref()?;
    Some(json!({
        "kind": m.kind,
        "platform": m.platform,
        "install_name": m.id,
        "main": m.main,
        "fixups": m.fixups,
        "function_starts": m.starts.len(),
        "libraries": m.libs,
        "slices": m.slices.iter().map(|s| json!({"arch": s.arch, "offset": s.off, "size": s.size})).collect::<Vec<_>>(),
        "slice_read": (!m.slices.is_empty()).then_some(m.picked),
        "uuid": m.uuid,
    }))
}
