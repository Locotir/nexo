//! Documents rendered to text, Markdown or JSON, and the files they are exported to.

use std::path::{Path, PathBuf};

use crate::error::{Result, VaultError};
use crate::store::{Document, Store};
use crate::text::sanitize_name;
use crate::users::UserId;

/// The formats a document can be exported in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    Text,
    Markdown,
    Json,
}

impl Format {
    /// The format called `s` (`txt`, `md` or `json`).
    pub fn parse(s: &str) -> Result<Format> {
        match s.trim().to_ascii_lowercase().as_str() {
            "txt" | "text" => Ok(Format::Text),
            "md" | "markdown" => Ok(Format::Markdown),
            "json" => Ok(Format::Json),
            other => Err(VaultError::Invalid(format!("unknown export format {other:?}"))),
        }
    }

    /// The file extension of the format.
    pub fn extension(self) -> &'static str {
        match self {
            Format::Text => "txt",
            Format::Markdown => "md",
            Format::Json => "json",
        }
    }
}

/// `s` as a JSON string, quotes included.
pub fn json_string(s: &str) -> String {
    let mut out = String::from("\"");
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c.is_control() => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// The document in the given format.
pub fn render(doc: &Document, format: Format) -> String {
    match format {
        Format::Text => format!("{}\n\n{}\n", doc.title, doc.body),
        Format::Markdown => {
            let tags: Vec<String> = doc.tags.iter().map(|t| format!("`{t}`")).collect();
            format!("# {}\n\n{}\n\n{}\n", doc.title, doc.body, tags.join(" "))
        }
        Format::Json => {
            let tags: Vec<String> = doc.tags.iter().map(|t| json_string(t)).collect();
            format!(
                "{{\"id\":{},\"owner\":{},\"title\":{},\"body\":{},\"tags\":[{}]}}",
                doc.id,
                doc.owner,
                json_string(&doc.title),
                json_string(&doc.body),
                tags.join(",")
            )
        }
    }
}

/// The file a document titled `title` is exported to under `base`.
pub fn export_path(base: &Path, title: &str, format: Format) -> PathBuf {
    base.join(format!("{}.{}", sanitize_name(title), format.extension()))
}

/// Every document of `owner` as `(file, content)`. Two titles that give the same file name get
/// the document id added, so no export overwrites another.
pub fn export_all(store: &Store, owner: UserId, base: &Path, format: Format) -> Vec<(PathBuf, String)> {
    let mut out: Vec<(PathBuf, String)> = Vec::new();
    for doc in store.by_owner(owner) {
        let mut path = export_path(base, &doc.title, format);
        if out.iter().any(|(p, _)| *p == path) {
            path = export_path(base, &format!("{} {}", doc.title, doc.id), format);
        }
        out.push((path, render(doc, format)));
    }
    out
}
