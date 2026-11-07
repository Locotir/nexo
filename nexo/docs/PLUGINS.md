# Plugins

A plugin is a folder (usually a git repository) that bundles commands, agents, skills, hooks and MCP servers, so a team ships one install to everyone. The format is the Claude Code plugin format: plugins written for Claude Code load in nexo, and the other way round.

## Layout

```
my-plugin/
  .nexo-plugin/plugin.json      (or .claude-plugin/plugin.json)
  commands/ship.md              slash commands
  agents/reviewer.md            subagents
  skills/lint/SKILL.md          Agent Skills
  hooks/hooks.json              hooks, same shape as Claude Code's
  .mcp.json                     MCP servers
```

`plugin.json`:

```json
{ "name": "acme", "version": "1.0.0", "description": "Acme tools", "author": "Acme" }
```

`author` may also be `{ "name": "Acme" }`. Optional path overrides (`commands`, `agents`, `skills`, `hooks`, `mcpServers`) take a folder or file inside the plugin; paths that leave the plugin folder are ignored. `hooks` and `mcpServers` may also be written inline in the manifest. Names hold letters, digits, `.`, `_` and `-`.

In hook commands and MCP `command`, `args`, `env` and `url`, `${CLAUDE_PLUGIN_ROOT}` and `${NEXO_PLUGIN_ROOT}` become the plugin's folder.

## Names

Everything a plugin adds is namespaced with the plugin name: `/acme:ship`, the skill `acme:lint`, the agent `acme:reviewer`, the MCP server `plugin_acme_db` (its tools are `mcp__plugin_acme_db__...`). If you have your own item with the same name, yours wins.

## Install

```
nexo plugin install <git-url | folder>[@ref]
nexo plugin install name@marketplace
nexo plugin list
nexo plugin update [name]
nexo plugin disable <name>      # hidden, files stay
nexo plugin enable <name>
nexo plugin remove <name>
```

Plugins live in `~/.nexo/plugins/<name>`. A git source is a shallow clone through the `git` program; the commit is pinned in `~/.nexo/plugins/.registry.json` (`@ref` is a branch, tag or commit). A folder is copied, without `.git` and without symlinks.

Before installing, nexo prints what the plugin adds: commands, agents, skills, every hook and every MCP server with the command it would run. You answer `y` or `n`. When nothing can be asked (a script, a pipe), the install stops unless you pass `--yes`.

`update` fetches again. If the commit is the same nothing happens; if it changed, the new contents are shown and you are asked again.

### Per project

`nexo plugin install <source> --scope project` also writes the plugin into `.nexo/config.toml`:

```toml
[plugins]
acme = "https://github.com/acme/nexo-acme.git@v1"
```

Teammates who open the project are told which of these plugins they lack and the command to install them. Nothing is installed by the project file alone. `nexo plugin install` with no source installs everything the project lists.

## Marketplace

A marketplace is a repository or folder with `.nexo-plugin/marketplace.json` or `.claude-plugin/marketplace.json`:

```json
{ "name": "team", "plugins": [
  { "name": "acme", "source": "./plugins/acme", "description": "Acme tools" },
  { "name": "other", "source": { "source": "github", "repo": "owner/repo", "ref": "v2" } },
  { "name": "third", "source": { "source": "url", "url": "https://host/x.git" } }
] }
```

```
nexo plugin marketplace add <git-url | folder>[@ref]
nexo plugin marketplace list | update [name] | remove <name>
nexo plugin install acme@team      # or just `acme` when only one marketplace lists it
```

A `./path` source must stay inside the marketplace folder.

## `/plugins`

In the TUI, `/plugins` lists the installed plugins and what each adds.

## Security

- Hooks and MCP servers run programs on your machine with your permissions. Read the summary, and install only plugins you trust.
- The commit is pinned; an update shows the new contents and asks again.
- Only the `file`, `git`, `http`, `https` and `ssh` git transports are allowed (no `ext::` command URLs), and git never prompts for a password.
- A project's `[plugins]` list only prompts teammates; it installs nothing and runs nothing.
- Plugin folders and marketplace entries cannot point outside themselves.
