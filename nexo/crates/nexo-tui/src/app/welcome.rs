//! The banner of an empty transcript and the branch it shows.

use super::*;

impl App {
    fn welcome_block(&self) -> Block {
        Block::Welcome {
            provider: self.provider.clone(),
            model: self.model.clone(),
            cwd: crate::ui::short_path(&self.cwd.to_string_lossy()),
            branch: self.branch.clone(),
            permission_mode: self.permission_mode.as_str().to_string(),
        }
    }

    /// The banner of an empty transcript.
    pub(super) fn push_welcome(&mut self) {
        if self.blocks.is_empty() {
            let b = self.welcome_block();
            self.blocks.push(b);
            self.cache.push(None);
            self.looks.push(Look::default());
            self.dirty = true;
        }
    }

    /// Keeps the banner's facts current while it is shown.
    pub(super) fn refresh_welcome(&mut self) {
        if matches!(self.blocks.first(), Some(Block::Welcome { .. })) {
            self.blocks[0] = self.welcome_block();
            self.invalidate(0);
        }
    }

    pub fn has_welcome(&self) -> bool {
        matches!(self.blocks.first(), Some(Block::Welcome { .. }))
    }

    /// Takes the banner off the top (tests that measure a transcript from its first message).
    #[doc(hidden)]
    pub fn drop_banner(&mut self) {
        if self.has_welcome() {
            self.blocks.remove(0);
            self.cache.remove(0);
            self.looks.remove(0);
            self.selected = self.selected.and_then(|i| i.checked_sub(1));
        }
    }

    /// Nothing said yet: only the banner, and maybe notices under it. The banner itself
    /// stays at the top of the transcript and scrolls away with it.
    pub fn fresh(&self) -> bool {
        self.blocks.iter().all(|b| matches!(b, Block::Welcome { .. } | Block::Notice { .. } | Block::Problem { .. }))
    }

    /// The banner is still coming in (its intro is drawn on the frame clock).
    pub fn intro_running(&self) -> bool {
        self.opts.animations
            && self.has_welcome()
            && self.now().saturating_duration_since(self.opened).as_millis() as u64
                <= crate::render::blocks::INTRO_MS + 100
    }

    /// `.git/HEAD` of the nearest repository, re-read every five seconds.
    pub(super) fn read_branch(&mut self) {
        self.branch_read = Some(self.now());
        let b = git_branch(&self.cwd);
        if b != self.branch {
            self.branch = b;
            self.refresh_welcome();
            self.dirty = true;
        }
    }
}
