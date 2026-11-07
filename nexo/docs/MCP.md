# MCP servers

An MCP server gives the agent extra tools, resources and prompts. nexo connects to local servers (a command, over stdio) and remote ones (streamable HTTP, with the old SSE transport as a fallback). Servers come from `[mcp.<name>]` in `~/.nexo/config.toml` or the project's `.nexo/config.toml`, from a project `.mcp.json`, from [plugins](PLUGINS.md), and from an ACP client's `session/new`. A [managed policy](MANAGED-POLICY.md) can leave some of them out; those are never started.

## Add, list, remove, sign in

```
nexo mcp add files npx -y @modelcontextprotocol/server-filesystem .
nexo mcp add --transport http docs https://example.com/mcp --header "Authorization: Bearer ${DOCS_TOKEN}"
nexo mcp list
nexo mcp login docs
nexo mcp remove docs
```

A remote server that answers 401 needs a login (OAuth in the browser): `nexo mcp login <name>`, or `l` in `/mcp`.

## What the model gets

- The server's tools, named `mcp__<server>__<tool>`. When a server says its tools changed, the new list takes effect at the start of the next turn, never in the middle of one.
- `list_mcp_resources` and `read_mcp_resource`, when a server offers resources.
- `@server:uri` in your message attaches that resource to the request.
- The server's prompts as slash commands: `/mcp__<server>__<prompt> [arguments]`.

## `/mcp`

In the TUI, `/mcp` opens a window with every server: how it is reached (stdio, http, sse), where it stands (connected, needs login, failed with the reason, off) and how many tools, resources and prompts it offers.

| Key | Does |
| --- | --- |
| Up, Down | pick a server |
| Space, Enter | turn the server off or on for this session |
| `p` | the same, and write it to the configuration (`enabled = false` in the file that defines the server) |
| `l` | sign in |
| `r` | connect again |
| Esc, `q` | close |

A server that comes from a plugin, a project `.mcp.json` or an ACP client has no nexo file to write to: `p` says so and the change lasts for the session.
