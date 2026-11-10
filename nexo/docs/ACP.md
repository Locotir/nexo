# nexo in your editor (Agent Client Protocol)

`nexo acp` runs nexo as an [Agent Client Protocol](https://agentclientprotocol.com) server on
stdin and stdout. An editor starts it, and nexo works there with the same loop, curator, tools and
sessions as in the terminal. The editor draws the conversation, the tool calls and the diffs, and
asks you before nexo edits or runs something.

Set nexo up once in a terminal first (`nexo doctor` checks it): a provider and a key, or
`nexo auth login`. The editor cannot sign in for nexo.

Options before the word `acp` apply to every session: `nexo --provider openai --model gpt-5 acp`,
`nexo --permission-mode accept-edits acp`, `nexo --log /tmp/nexo-acp.log acp`.
Nothing but protocol JSON goes to stdout; logs go to stderr and to `--log`.

## Zed

Open the settings (`zed: open settings`) and add nexo under `agent_servers`:

```json
{
  "agent_servers": {
    "nexo": {
      "command": "nexo",
      "args": ["acp"],
      "env": {}
    }
  }
}
```

Then open the agent panel, press `+` and pick **nexo**. If `nexo` is not on the PATH Zed sees, give
the full path in `command`. The permission modes show in the panel's mode picker.

## JetBrains (IntelliJ IDEA, PyCharm, WebStorm, ...)

In the AI Assistant chat, open the agent menu, choose **Add custom agent** and edit
`~/.jetbrains/acp.json`:

```json
{
  "agent_servers": {
    "nexo": {
      "command": "nexo",
      "args": ["acp"]
    }
  }
}
```

Reopen the chat and pick nexo from the agent list.

## Neovim

### CodeCompanion

```lua
require("codecompanion").setup({
  adapters = {
    acp = {
      nexo = function()
        return require("codecompanion.adapters").extend("gemini_cli", {
          name = "nexo",
          formatted_name = "nexo",
          commands = { default = { "nexo", "acp" } },
        })
      end,
    },
  },
  strategies = { chat = { adapter = "nexo" } },
})
```

The adapter name and the way it is extended differ between CodeCompanion versions; what matters is
the command, `nexo acp`, and that the adapter speaks ACP over stdio.

### avante.nvim

```lua
require("avante").setup({
  provider = "nexo",
  acp_providers = {
    nexo = {
      command = "nexo",
      args = { "acp" },
      env = {},
    },
  },
})
```

## What works

| ACP | In nexo |
| --- | --- |
| `initialize` | protocol version 1; no sign-in methods (use `nexo auth login`) |
| `session/new` | a new nexo session in `cwd`; stdio, `http` and `sse` MCP servers (with headers) from `mcpServers` are added to nexo's MCP client for that session; `initialize` advertises `mcpCapabilities` http and sse |
| `session/load` | resumes a recorded nexo session by its id and replays it (messages, tool calls) |
| `session/prompt` | one turn of the normal agent loop; text, resource links and embedded files are read, images and audio are not |
| `session/update` | `agent_message_chunk`, `agent_thought_chunk`, `tool_call`, `tool_call_update` (kind, status, file locations, diffs for edits and writes), `plan` (from the model's task list), `current_mode_update` |
| `session/cancel` | interrupts the turn; the prompt ends with `cancelled` |
| `session/set_mode` | `ask`, `accept-edits`, `yolo`, `read-only`, `plan` (nexo's permission modes) |
| `session/request_permission` | sent whenever nexo would ask you: Allow, Always allow (when nexo has a rule for it), Reject; the model's questions and plans to approve come the same way |
| `fs/read_text_file`, `fs/write_text_file` | when the editor offers them, nexo's `read`, `write` and `edit` go through them, so unsaved buffers are seen and edits land in the buffer; otherwise nexo uses the disk |
| `terminal/create`, `terminal/wait_for_exit`, `terminal/output`, `terminal/kill`, `terminal/release` | when the editor offers `terminal`, a foreground `bash` call runs in its terminal, shown live in the tool call (`content: [{type: "terminal", terminalId}]`) and kept there when it ends. A timeout or a cancel kills the command, and the terminal is always released. `bash` with `background: true` stays a nexo process (the `jobs` tool needs its output), and so does every call when the editor has no terminal; with the OS sandbox on (`permissions.sandbox`) every `bash` call runs in nexo, sandboxed, because the editor's terminal is not sandboxed |

## Left out

- Images and audio in prompts.
- Slash commands and session lists.

A session is a nexo session: it shows in `nexo sessions`, and `nexo --resume <id>` opens it in the
terminal once the editor has closed it.
