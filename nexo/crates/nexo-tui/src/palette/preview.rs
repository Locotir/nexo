//! The head of the selected file, shown beside the `@` picker.

use super::*;

/// How much of a file the preview reads.
const PREVIEW_BYTES: usize = 16 * 1024;

impl Palette {
    /// The head of the selected file (read once per file), for the `@` picker.
    pub fn preview(&mut self, root: &Path, lines: usize) -> Option<&Preview> {
        if self.kind != PaletteKind::Files {
            return None;
        }
        let path = self.current()?.to_string();
        if !matches!(&self.preview, Some((p, _)) if *p == path) {
            let p = read_preview(&root.join(&path), lines.max(1));
            self.preview = Some((path, p));
        }
        self.preview.as_ref().map(|(_, p)| p)
    }
}

pub(super) fn read_preview(path: &Path, max_lines: usize) -> Preview {
    use std::io::Read;
    let size = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    let mut buf = Vec::new();
    let read = std::fs::File::open(path).and_then(|f| f.take(PREVIEW_BYTES as u64).read_to_end(&mut buf));
    if let Err(e) = read {
        return Preview { lines: Vec::new(), note: format!("can't read: {}", e.kind()) };
    }
    let size_s = human_size(size);
    if buf.contains(&0) {
        return Preview { lines: Vec::new(), note: format!("binary · {size_s}") };
    }
    let text = String::from_utf8_lossy(&buf);
    let all = text.lines().count();
    let lines = text.lines().take(max_lines).map(|l| crate::render::wrap::clean(l).into_owned()).collect();
    let count = if (size as usize) > PREVIEW_BYTES { format!("{all}+ lines") } else { format!("{all} lines") };
    Preview { lines, note: format!("{count} · {size_s}") }
}

pub(super) fn human_size(n: u64) -> String {
    match n {
        n if n < 1024 => format!("{n} B"),
        n if n < 1024 * 1024 => format!("{:.1} KB", n as f64 / 1024.0),
        n => format!("{:.1} MB", n as f64 / (1024.0 * 1024.0)),
    }
}
