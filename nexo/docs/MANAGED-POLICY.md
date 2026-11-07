# Managed policy

An administrator can pin settings that users and projects cannot change. The policy is one TOML file,
read at every start.

| System | File |
| --- | --- |
| Linux | `/etc/nexo/managed.toml` |
| macOS | `/Library/Application Support/nexo/managed.toml` |
| Windows | `%ProgramData%\nexo\managed.toml` |

`NEXO_MANAGED_FILE` points somewhere else, but only in tests and debug builds. Release builds ignore it.
No file means no policy. A file that cannot be read or parsed, or that has an unknown key or a bad value,
stops nexo at startup with the file name and the error. It never fails open.

## How it merges

The user file, the project file (when trusted) and the environment merge as usual. The managed file is
applied last and wins over all of them:

- Deny rules are added to the user's rules; a deny always beats an allow.
- The sandbox can be made stricter by the user, never looser. `sandbox_forced = true` fixes the mode exactly.
- Provider, model, MCP and extension lists remove what they forbid from the merged config.
- Forced hooks are appended with source `managed`; hooks from the user or project levels are dropped when
  their level is switched off.
- Every forced value shows in `/status` and `nexo doctor` as `(source: managed)`.
- A refused action says: `blocked by your organization's policy (<file>)`.

## Example

```toml
disable_web = true
disable_bypass_mode = true
allow_user_hooks = false
allow_project_hooks = false

[permissions]
deny = ["bash(rm -rf *)", "write(.env)"]
sandbox = "workspace-write"      # minimum; add sandbox_forced = true to fix it
sandbox_network = false
# sandbox_write_dirs = ["/srv/work"]

[providers]
allow = ["anthropic", "deepseek"]
deny = []

[models]
deny = ["*preview*", "deepseek/old-model"]

[mcp]
allow = ["files", "git"]                 # by server name
deny_commands = ["sh *", "curl *"]       # by "command args" or url

[extensions]
user_skills = true
project_skills = false
user_commands = false
project_commands = false
user_plugins = false
project_plugins = false

[audit]
enabled = true                           # forced on; user and project config cannot turn it off
path = "/var/log/nexo/audit.jsonl"       # pinned; their own path is replaced

[[forced_hooks]]
event = "pre_tool"
matcher = "bash"
command = "/usr/local/bin/audit-command"
```

## Keys

- `permissions.deny`, `permissions.allow`, `permissions.sandbox`, `permissions.sandbox_forced`,
  `permissions.sandbox_network`, `permissions.sandbox_write_dirs`
- `providers.allow|deny`, `models.allow|deny`: globs (`*`, `?`, case-insensitive). Models match `model` and
  `provider/model`. Checked at `/model`, `--model`, and for sub-agents.
- `mcp.allow|deny` (server names), `mcp.allow_commands|deny_commands` (command line or url)
- `allow_user_hooks`, `allow_project_hooks`, `forced_hooks`
- `extensions.{user,project}_{skills,commands,plugins}`: a plugin has the scope it was installed with (`nexo plugin install --scope project`, or a bare `nexo plugin install` for what the project asks for; otherwise `user`). A forbidden scope cannot install (`nexo plugin install` is refused) and its plugins load nothing: no commands, agents, skills, hooks or MCP servers.
- `audit.enabled`, `audit.path`: forces the audit log (docs/AUDIT.md) on and pins its file
- `disable_web`, `disable_bypass_mode`

## Notes

- There is no allowlist form for plugins yet, only per-scope on or off.
- A plugin hook counts as a user hook for `allow_user_hooks`.
