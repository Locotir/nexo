//! `nexo plugin install | list | remove | update | enable | disable | marketplace`: manage the plugins
//! in `<config dir>/plugins` (see `docs/PLUGINS.md` and `nexo_core::plugins`). Plugins come from a git
//! repository (a shallow clone through the `git` program) or from a folder, directly or by name from
//! a marketplace. Before anything is installed the person is shown what it adds, hooks and MCP
//! servers with the commands they would run, and agrees (`--yes` when there is no one to ask).

use std::collections::BTreeMap;
use std::io::{BufRead, IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{anyhow, bail, Context};
use clap::Subcommand;
use nexo_core::plugins::{self, Entry, MarketEntry, Plugin, Registry, MANIFEST_DIRS};
use nexo_core::Config;
use serde_json::Value;
use toml_edit::{value, DocumentMut, Item, Table};

mod ask;
mod fetch;
mod install;
mod manage;
mod market;
mod source;

use ask::*;
use fetch::*;
use install::*;
use manage::*;
use market::*;
use source::*;

#[derive(Subcommand, Debug)]
pub enum PluginCmd {
    /// Install a plugin: `<git-url | folder>[@ref]`, or `name@marketplace`. Shows what it adds and asks first.
    /// With no argument: install what this project asks for (`[plugins]` in .nexo/config.toml).
    Install {
        source: Option<String>,
        /// Do not ask (needed when nothing can be asked, as in a script).
        #[arg(long, short = 'y')]
        yes: bool,
        /// `user` (the default) or `project`: also record it in .nexo/config.toml so teammates are told to install it.
        #[arg(long, value_parser = ["user", "project"], default_value = "user")]
        scope: String,
    },
    /// The installed plugins and what each adds.
    List,
    /// Delete an installed plugin.
    Remove { name: String },
    /// Fetch the newest version of a plugin (all of them with no name); asks again when the code changed.
    Update {
        name: Option<String>,
        #[arg(long, short = 'y')]
        yes: bool,
    },
    /// Turn an installed plugin on.
    Enable { name: String },
    /// Turn an installed plugin off without deleting it.
    Disable { name: String },
    /// Lists of plugins a team keeps in a repository or folder (a `marketplace.json`).
    Marketplace {
        #[command(subcommand)]
        what: MarketCmd,
    },
}

#[derive(Subcommand, Debug)]
pub enum MarketCmd {
    /// Add a marketplace: `<git-url | folder>[@ref]`.
    Add { source: String },
    /// The marketplaces and the plugins each lists.
    List,
    /// Forget a marketplace (plugins installed from it stay).
    Remove { name: String },
    /// Fetch the newest version of a marketplace (all of them with no name).
    Update { name: Option<String> },
}

pub fn run(cwd: &Path, config: &Config, what: &PluginCmd) -> anyhow::Result<i32> {
    let dir = nexo_core::paths::config_dir();
    match what {
        PluginCmd::Install { source, yes, scope } => {
            // installing what the project asks for (no source) is project scope too
            let level = if source.is_none() { "project" } else { scope.as_str() };
            if let Some(p) = &config.managed {
                if !p.extension_allowed(level, "plugins") {
                    bail!("{}", p.blocked(&format!("installing {level} plugins")));
                }
            }
            let mut ask = |summary: &str| ask_person(summary, *yes);
            let sources: Vec<String> = match source {
                Some(s) => vec![s.clone()],
                None => config.plugins.values().cloned().collect(),
            };
            if sources.is_empty() {
                bail!("give a source: nexo plugin install <git-url | folder>[@ref] (this project asks for no plugins)");
            }
            let mut code = 0;
            for s in sources {
                if source.is_none()
                    && config.plugins.iter().any(|(n, v)| *v == s && Registry::load(&dir).plugins.contains_key(n))
                {
                    continue;
                }
                match install(&dir, cwd, &s, &mut ask)? {
                    Some(done) => {
                        println!("installed {} (pinned to {})", done.name, short(&done.commit));
                        if level == "project" {
                            set_scope(&dir, &done.name, "project")?;
                        }
                        if scope == "project" {
                            let file = project_file(cwd);
                            record_in_project(&file, &done.name, &done.portable_source)?;
                            println!("recorded in {}: teammates are told to install it", file.display());
                        }
                    }
                    None => code = 1,
                }
            }
            Ok(code)
        }
        PluginCmd::List => {
            print!("{}", list_text(&dir, &config.plugins));
            Ok(0)
        }
        PluginCmd::Remove { name } => {
            remove(&dir, name)?;
            println!("removed {name}");
            Ok(0)
        }
        PluginCmd::Update { name, yes } => {
            let mut ask = |summary: &str| ask_person(summary, *yes);
            update(&dir, cwd, name.as_deref(), &mut ask)
        }
        PluginCmd::Enable { name } => set_enabled(&dir, name, true).map(|_| 0),
        PluginCmd::Disable { name } => set_enabled(&dir, name, false).map(|_| 0),
        PluginCmd::Marketplace { what } => match what {
            MarketCmd::Add { source } => {
                let name = market_add(&dir, cwd, source)?;
                println!("added marketplace {name}; install from it with `nexo plugin install <plugin>@{name}`");
                Ok(0)
            }
            MarketCmd::List => {
                print!("{}", market_list_text(&dir));
                Ok(0)
            }
            MarketCmd::Remove { name } => {
                market_remove(&dir, name)?;
                println!("removed marketplace {name}");
                Ok(0)
            }
            MarketCmd::Update { name } => market_update(&dir, cwd, name.as_deref()).map(|_| 0),
        },
    }
}

#[cfg(test)]
mod tests;
