//! The command palette: refresh, arguments and accepting an entry.

use super::*;

impl App {
    // ------------------------------------------------------------ palette

    /// After the draft changed: open, update or close the palette.
    pub fn refresh_palette(&mut self) {
        // no commands from a sub-agent's conversation
        if self.in_sub() {
            self.palette = None;
            return;
        }
        // a draft that has text is no longer the empty box the suggestion belongs to
        if !self.input.is_empty() {
            self.suggestion = None;
        }
        let draft = self.input.text();
        let cursor = self.input.cursor_offset();
        match palette::wanted(&draft, cursor) {
            Some((kind, start, query)) => {
                let same = self.palette.as_ref().is_some_and(|p| p.kind == kind && p.start == start);
                if same {
                    if let Some(p) = &mut self.palette {
                        p.set_query(&query);
                    }
                } else {
                    self.palette = Some(match kind {
                        PaletteKind::Commands => Palette::commands_full(
                            &query,
                            start,
                            self.custom_commands
                                .iter()
                                .map(|c| {
                                    let d = if c.hint.is_empty() {
                                        c.description.clone()
                                    } else {
                                        format!("{} {}", c.hint, c.description)
                                    };
                                    (c.name.clone(), format!("{d}  · {}", custom_origin(&c.origin)))
                                })
                                .collect(),
                            palette::recent_commands(self.input.history()),
                        ),
                        PaletteKind::Files => {
                            let files = self.files().to_vec();
                            let mut files = files;
                            // `@server:uri` mentions of the MCP servers complete next to the files
                            files.extend(self.mcp_mentions());
                            Palette::files(&query, start, files)
                        }
                        PaletteKind::Args(cmd) => {
                            let mut values =
                                palette::arg_values(&cmd, &self.opts.models, &self.in_use(), &self.effort_levels);
                            values.extend(palette::live_values(
                                &cmd,
                                &self.jobs,
                                &self.trashed,
                                &self.opts.sessions,
                                &self.peer_values(),
                            ));
                            if cmd == "add-dir" {
                                values.extend(palette::dir_values(&self.opts.cwd, &query));
                            }
                            values.splice(0..0, self.current_arg(&cmd));
                            Palette::args(&cmd, &query, start, values)
                        }
                    });
                }
            }
            None => self.palette = None,
        }
        // an argument list with nothing to offer gets out of the way of free text
        if self.palette.as_ref().is_some_and(|p| p.command().is_some() && p.is_empty()) {
            self.palette = None;
        }
        self.dirty = true;
    }

    /// The sessions `/msg` offers: a one-word name when it is unique, else the start of the id.
    pub(super) fn peer_values(&self) -> Vec<(String, String)> {
        let ok = |p: &&nexo_core::mail::PeerInfo| p.sendable && !p.me && !p.external;
        self.peers
            .iter()
            .filter(ok)
            .map(|p| {
                let unique = self.peers.iter().filter(|q| q.name == p.name).count() == 1;
                let value = if !p.name.is_empty() && !p.name.contains(char::is_whitespace) && unique {
                    p.name.clone()
                } else {
                    p.id.chars().take(8).collect()
                };
                let tail = p.cwd.trim_end_matches(['/', '\\']).rsplit(['/', '\\']).next().unwrap_or("");
                let hint = if p.bypass { format!("{tail} · every tool allowed") } else { tail.to_string() };
                (value, hint)
            })
            .collect()
    }

    /// What `/goal` and `/rename` hold now, first in their list: Tab puts it on the line to edit.
    /// `/export` offers a file here named after the session.
    /// `provider/model` in use, as the model list names its entries.
    pub(super) fn in_use(&self) -> String {
        format!("{}/{}", self.provider, self.model)
    }

    pub(super) fn current_arg(&self, cmd: &str) -> Option<(String, String)> {
        if cmd == "export" {
            let slug = self
                .session_title
                .to_lowercase()
                .split(|c: char| !c.is_alphanumeric())
                .filter(|w| !w.is_empty())
                .collect::<Vec<_>>()
                .join("-");
            let slug: String = slug.chars().take(48).collect();
            let slug = slug.trim_end_matches('-');
            return (!slug.is_empty())
                .then(|| (format!("{slug}.md"), "a file here named after the session".to_string()));
        }
        let (now, what) = match cmd {
            "goal" => (self.goal.as_ref()?.headline().trim(), "the goal now · Tab to edit it"),
            "rename" => (self.session_title.trim(), "the name now · Tab to edit it"),
            _ => return None,
        };
        (!now.is_empty()).then(|| (now.to_string(), what.to_string()))
    }

    pub(super) fn files(&mut self) -> &[String] {
        if self.files.is_none() {
            self.files = Some(palette::project_files(&self.cwd, 20_000));
        }
        self.files.as_deref().unwrap_or(&[])
    }

    /// Tab or Enter on the palette: complete the selection into the draft.
    pub fn palette_accept(&mut self, run: bool) {
        let Some(p) = self.palette.take() else { return };
        // nothing matches: Tab takes the likely command, Enter sends the line as typed (and hears why not)
        let guess = if run { None } else { p.suggestion().map(String::from) };
        let Some(choice) = p.current().map(String::from).or(guess) else {
            if run {
                self.submit();
            }
            self.dirty = true;
            return;
        };
        match p.kind {
            PaletteKind::Commands => {
                self.input.replace_range(p.start, &choice);
                // a command that cannot run without its argument waits for it instead of failing
                let needs_arg = commands::lookup(&choice).is_some_and(|s| s.hint.starts_with('<'));
                if run && !needs_arg {
                    self.submit();
                } else {
                    self.input.insert_paste(" ");
                    // a command with values to choose from lists them right away
                    self.refresh_palette();
                }
            }
            PaletteKind::Files => {
                self.input.replace_range(p.start, &format!("{choice} "));
            }
            PaletteKind::Args(_) if run && !p.runs_selection() && !p.picks_model() => self.submit(),
            PaletteKind::Args(_) => {
                self.input.replace_range(p.start, &choice);
                if run {
                    self.submit();
                } else if choice.ends_with('/') {
                    // a folder picked with Tab lists its own folders next, so a path is built step by step
                    self.refresh_palette();
                }
            }
        }
        self.dirty = true;
    }
}
