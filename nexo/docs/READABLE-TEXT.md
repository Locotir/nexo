# Readable text

Every word nexo puts on screen follows these rules.
They help people with dyslexia or ADHD. They help everyone else too.

## Rules

1. **Short sentences.** 12 words or fewer when possible.
2. **One idea per line.** A second idea gets its own line or bullet.
3. **Verb first.** "Press Enter to send", not "Sending happens with Enter".
4. **Outcome first.** Say what happened, then why, then what to do.
5. **Same word for the same thing.** One name per thing, everywhere (see the list below).
6. **Keys stand out.** Keys are drawn in the accent colour, nothing else on the line is.
7. **No italics.** Slanted letters are hard to read. Use colour or bold.
8. **No ALL CAPS** for words. Short key names (`Esc`, `Tab`) are fine.
9. **No filler.** Drop "please", "simply", "just", "note that", "in order to".
10. **Few highlights.** One or two bold words per block, or none.
11. **Numbers as digits.** "3 files", not "three files".
12. **Lists over paragraphs.** Three or more items become a list.

## Errors

An error says three things, each on its own line when there is room:

- **What failed.** "Could not read config.toml"
- **Why**, when known. "Line 4: unknown key `them`"
- **What to do.** "Fix the line, or run `nexo config init`"

## One word per thing

| Use | Not |
|---|---|
| session | conversation, chat, thread |
| model | engine, LLM, AI |
| provider | vendor, backend, service |
| key | token, secret, API key (in the interface) |
| block | message card, entry |
| fork | branch, copy (for `/fork`) |
| send | submit |

## Check before you commit

- Read the text out loud. Cut every word you can.
- Does each line start with the point?
- Is any word in italics or ALL CAPS?
