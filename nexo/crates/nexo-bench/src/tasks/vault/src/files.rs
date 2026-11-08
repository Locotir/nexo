//! Attachments: files that live under one base directory and are served by name.

use std::path::{Path, PathBuf};

use crate::error::{Result, VaultError};

/// The largest attachment, in bytes.
pub const MAX_FILE: usize = 1 << 20;

/// The longest file name, in bytes.
pub const MAX_PATH: usize = 200;

/// The attachments under one base directory.
#[derive(Debug, Clone)]
pub struct FileRoot {
    base: PathBuf,
}

impl FileRoot {
    /// Attachments live under `base`.
    pub fn new(base: impl Into<PathBuf>) -> FileRoot {
        FileRoot { base: base.into() }
    }

    /// The base directory.
    pub fn base(&self) -> &Path {
        &self.base
    }

    /// Where the attachment called `name` lives. Names may hold sub-folders (`notes/a.txt`).
    pub fn resolve(&self, name: &str) -> Result<PathBuf> {
        let name = name.trim();
        if name.is_empty() {
            return Err(VaultError::Invalid("the file name is empty".into()));
        }
        if name.len() > MAX_PATH || name.contains('\0') {
            return Err(VaultError::Invalid("the file name is not acceptable".into()));
        }
        Ok(self.base.join(name))
    }

    /// The content of an attachment.
    pub fn read(&self, name: &str) -> Result<Vec<u8>> {
        let path = self.resolve(name)?;
        let bytes = std::fs::read(&path)?;
        if bytes.len() > MAX_FILE {
            return Err(VaultError::Invalid(format!("{name} is larger than {MAX_FILE} bytes")));
        }
        Ok(bytes)
    }

    /// Stores an attachment, creating its sub-folders, and replaces any earlier one.
    pub fn write(&self, name: &str, content: &[u8]) -> Result<()> {
        if content.len() > MAX_FILE {
            return Err(VaultError::Invalid(format!("an attachment may have at most {MAX_FILE} bytes")));
        }
        let path = self.resolve(name)?;
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(&path, content)?;
        Ok(())
    }

    /// Deletes an attachment.
    pub fn remove(&self, name: &str) -> Result<()> {
        std::fs::remove_file(self.resolve(name)?)?;
        Ok(())
    }

    /// Whether an attachment with this name exists.
    pub fn exists(&self, name: &str) -> bool {
        self.resolve(name).map(|p| p.is_file()).unwrap_or(false)
    }

    /// The names of all attachments, sorted, with `/` between folders.
    pub fn list(&self) -> Result<Vec<String>> {
        let mut out = Vec::new();
        if self.base.is_dir() {
            collect(&self.base, "", &mut out)?;
        }
        out.sort();
        Ok(out)
    }
}

fn collect(dir: &Path, prefix: &str, out: &mut Vec<String>) -> Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let name = format!("{prefix}{}", entry.file_name().to_string_lossy());
        if entry.file_type()?.is_dir() {
            collect(&entry.path(), &format!("{name}/"), out)?;
        } else {
            out.push(name);
        }
    }
    Ok(())
}
