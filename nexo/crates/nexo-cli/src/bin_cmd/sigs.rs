use std::path::{Path, PathBuf};

use nexo_bin::Error;

/// `nexo bin sigs`: sign the functions of static libraries and write the file `nexo bin` reads.
pub(super) fn make_sigs(libs: &[PathBuf], all: bool, out: Option<&Path>, data_dir: &Path) -> Result<String, Error> {
    let libs = if libs.is_empty() { nexo_bin::sig_libraries() } else { libs.to_vec() };
    if libs.is_empty() {
        return Err(Error::Bad("no static library found here; give one: nexo bin sigs /path/libc.a".into()));
    }
    let keep = if all { nexo_bin::SigKeep::All } else { nexo_bin::SigKeep::Common };
    let mut set = nexo_bin::SigSet::default();
    let mut lines = Vec::new();
    for l in &libs {
        match nexo_bin::sig_generate(l, keep) {
            Ok(s) => {
                lines.push(format!("  {} signatures from {}", s.len(), l.display()));
                set = set.merged(s);
            }
            Err(e) => lines.push(format!("  skipped {}: {e}", l.display())),
        }
    }
    if set.is_empty() {
        return Err(Error::Bad(format!("no signatures made:\n{}", lines.join("\n"))));
    }
    let path = out.map(Path::to_path_buf).unwrap_or_else(|| data_dir.join("sigs").join("local.sig"));
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| Error::Io(format!("cannot create {}: {e}", dir.display())))?;
    }
    let names: Vec<String> =
        libs.iter().filter_map(|l| l.file_name()).map(|n| n.to_string_lossy().into_owned()).collect();
    std::fs::write(&path, set.to_text(&names.join(", ")))
        .map_err(|e| Error::Io(format!("cannot write {}: {e}", path.display())))?;
    Ok(format!(
        "wrote {} signatures to {}\n{}\nfuncs, fn, overview and decomp name a matching unnamed function and label it [signature ...]; a match is never proof.",
        set.len(),
        path.display(),
        lines.join("\n")
    ))
}
