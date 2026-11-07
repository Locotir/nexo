# Hooks

A hook is a shell command nexo runs at a point of the loop. It reads one JSON object on stdin and may
answer with an exit code and a JSON object on stdout. Run `/hooks` in the TUI to list what is loaded and
where each hook came from.

## Where hooks are set

- `[[hooks]]` in `config.toml` (user file, or the project's `.nexo/config.toml` when the project is trusted).
- The `hooks` block of the project's `.claude/settings.json` and `.claude/settings.local.json`, only when
  the project is trusted (`nexo config trust`). Without trust they are ignored and nexo says so once.
  The user-level `~/.claude/settings.json` is never read.

```toml
[[hooks]]
event = "pre_tool"          # an event below; CamelCase spellings such as PreToolUse work too
matcher = "edit|write"      # optional regex on the tool name; none or "*" means every call
command = "./check.sh"
timeout = 10                # optional, seconds; default 60. A hook that runs longer is killed.
```

The `.claude/settings.json` form is read as is (`{"hooks": {"PreToolUse": [{"matcher": "Bash",
"hooks": [{"type": "command", "command": "...", "timeout": 10}]}]}}`). Claude tool names in a matcher
are mapped to nexo's (`Bash` is `bash`, `Edit` is `edit`, `Task` is `task`, and so on). Hook types other
than `command` and events nexo has no point for are skipped.

## Events

| Event | When | Matcher tests | Can block |
| --- | --- | --- | --- |
| `session_start` | the agent starts or resumes | `startup` or `resume` | no |
| `session_end` | the agent shuts down | `exit` | no |
| `user_prompt_submit` | a request is sent, before the model sees it | not used | yes |
| `pre_tool` | before a tool call, before permission is asked | tool name | yes |
| `post_tool` | after a tool call | tool name | feedback only |
| `stop` | the model ended its reply with no tool call | not used | yes |
| `subagent_start` | a `task` sub-agent starts | agent name | no |
| `subagent_stop` | a sub-agent has reported | agent name | no |
| `pre_compact` | before the history is compacted (nexo's point where context is folded) | `compact` | no |
| `notification` | a permission prompt, or a request that ended in an error or the turn limit | `permission_prompt`, `turn_stopped` | no |
| `permission_request` | a tool call is about to ask the person | tool name | yes |
| `turn_end` | a request is over (kept from before) | not used | no |

`pre_tool`, `post_tool` and `turn_end` keep working under these names; `PreToolUse`, `PostToolUse`,
`UserPromptSubmit`, `SessionStart`, `SessionEnd`, `Stop`, `SubagentStart`, `SubagentStop`, `PreCompact`,
`Notification` and `PermissionRequest` are the same events.

## Input (stdin)

Always: `session_id`, `cwd`, `event` (nexo's name), `hook_event_name` (`PreToolUse` style). Plus:

| Event | Fields |
| --- | --- |
| `session_start` | `source` |
| `session_end` | `reason` |
| `user_prompt_submit` | `prompt` |
| `pre_tool`, `permission_request` | `tool_name`, `tool_input` (also `tool`, `args`) |
| `post_tool` | the above and `tool_response`, `output`, `is_error`, `elapsed_ms` |
| `stop` | `stop_hook_active`, `last_assistant_message` |
| `subagent_start` | `agent_type`, `prompt` |
| `subagent_stop` | `agent_type`, `report` |
| `pre_tool`, `post_tool` in a sub-agent | the same, and `agent` (the sub-agent that made the call) |
| `pre_compact` | `trigger`, `messages`, `tokens` |
| `notification` | `notification_type`, `message` |

The environment also has `NEXO_EVENT`, `NEXO_TOOL` and `NEXO_CWD`.

## Output

- Exit 0: go on. Stdout may hold JSON:
  `{"decision": "allow" | "deny" | "ask", "reason": "...", "additional_context": "...", "updated_input": {...}}`
  (`hookSpecificOutput` with `permissionDecision` is accepted too).
  - `deny` blocks where the event can be blocked; the reason is what the model reads.
  - `allow` skips the question to the person (deny rules still apply); `ask` forces it.
  - `updated_input` replaces the tool input before the call (later hooks see the new input).
  - `additional_context` is shown to the model: added to the request for `session_start` and
    `user_prompt_submit`, to the task for `subagent_start`, to the tool output for `post_tool`.
    Plain text on stdout does the same for `session_start` and `user_prompt_submit`.
- Exit 2: block, where the event can be blocked. Stderr (or stdout when stderr is empty) is the reason the
  model reads: `Blocked by a hook: <reason>` for a tool, `a hook stopped it` for a prompt. For
  `post_tool` it is appended to the output as `[hook] ...`.
- Any other exit code, a timeout or a start failure: not blocking. The person sees a notice with the
  command and what it printed.

## Stop hooks

A `stop` hook that blocks makes the agent keep working: its reason arrives as a new note
(`[Stop hook] <reason>`). The agent can be sent back at most 3 times in a row for one request. The
payload has `stop_hook_active` true after the first.

## Examples

A formatter after edits (`config.toml`):

```toml
[[hooks]]
event = "post_tool"
matcher = "edit|write"
command = "cargo fmt >/dev/null 2>&1 || true"
```

An audit log of every tool call, one JSON line each:

```toml
[[hooks]]
event = "pre_tool"
command = "cat | jq -c '{session_id, tool_name, tool_input}' >> .nexo-audit.jsonl"
```

## Sub-agents

The tool calls of a `task` sub-agent fire `pre_tool` and `post_tool` like the main agent's, with the same
decisions (`deny`, `allow`, `ask`, `updated_input`, `additional_context`, exit 2) and one more payload field,
`agent`, the sub-agent's name (it is absent for the main agent). A matcher still tests the tool name, so a
hook that should only judge sub-agents checks `agent` in the JSON. A `pre_tool` deny reaches the sub-agent as
`Blocked by a hook: <reason>`. `permission_request` hooks fire for a sub-agent's asks too, without `agent`.

## Not covered

The curator's own fold is not a hook point: `pre_compact` fires when nexo compacts the history.
