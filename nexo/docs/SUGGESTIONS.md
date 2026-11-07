# Prompt suggestions

When a turn ends normally, the empty input can show a dim guess at your next message. `Tab` or `→` takes it
whole into the draft; typing anything, a submit or a new turn hides it. It shows only in an empty draft, with
no window or command list open.

## What it costs

One short side request per finished turn. It repeats the last request of the conversation byte for byte (same
system prompt, tools and messages, so the provider's prompt cache is hit), then adds the reply that request
got and one message asking for the likely next message. The answer is capped at 200 tokens, asked with the
least thinking the model takes, and skipped for models that always reason first. Its usage is billed and shown
as `suggest`.

## What it never does

- It does not enter the session history, the saved session or the context curator: the next real request is
  built from the history alone.
- It runs no tools: a reply that calls one is dropped, nothing is executed or asked.
- It fires no hooks.
- It is dropped when the reply is empty, a request started in the meantime, or the conversation changed.
- It is never asked in `nexo -p` (headless) or `nexo acp`: only the interface sends the request.

## Turning it off

`[ui] suggest = false` in the user or project configuration. The agent checks the merged configuration (the
managed policy included) before it sends anything, so the setting holds whichever client asks.
