//! The diff key (F4 by default): the key bar offers it when the selected block is an edit.
//! The key itself opens the diff viewer (`App::open_diffview`); the old side panel is gone.

use crate::app::App;
use crate::render::blocks::Block;
use nexo_core::tool::ToolDisplay;

/// Path, old text, new text and first line of the change a block made, if it is an edit.
pub type DiffParts = (String, String, String, usize);

pub fn diff_of(b: &Block) -> Option<DiffParts> {
    match b {
        Block::Tool { output: Some(o), .. } => match &o.display {
            ToolDisplay::Diff { path, old, new, start_line } => {
                Some((path.clone(), old.clone(), new.clone(), *start_line))
            }
            _ => None,
        },
        _ => None,
    }
}

impl App {
    /// Whether the selected block is an edit (the key bar then offers the diff key).
    pub fn selected_is_edit(&self) -> bool {
        self.selected.and_then(|i| self.blocks.get(i)).and_then(diff_of).is_some()
    }
}
