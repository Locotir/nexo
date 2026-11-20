# Security

nexo runs commands and edits files for an agent, so security reports matter.

## Reporting a problem

Report it privately: open the repository's **Security** tab and choose **Report a vulnerability**.
Please do not open a public issue for it. Include the nexo version (`nexo --version`), the platform,
and the steps that show the problem.

You will get an answer within a week. Fixes go to `main`, and the advisory says which commit fixes it.

## In scope

- A command, write or network call that runs without the permission the settings ask for.
- A path rule (`read(...)`, `edit(...)`) that lets the agent reach files outside what it names.
- API keys or session contents leaking into logs, the terminal title, crash output or a request to
  a host other than the chosen provider.
- Hooks or MCP servers started without the user's configuration.

## Out of scope

- What a model does inside the permissions you granted (`--yolo` and `auto` mode allow everything).
- Problems in a provider's own API or terms.

## Keeping your setup safe

- Keys belong in environment variables, not in `config.toml`.
- Sessions in `~/.nexo/sessions/` keep whatever passed through the conversation: treat them like
  credentials.
- Run `nexo bench` in a container or a VM: its tasks execute commands the model writes.
