# Lanes (`nexo team`)

A lane is a named session that runs on its own in the background: its own folder, model and permission mode,
and a routine it runs on a clock. Between rounds it answers mail from your other sessions, and it keeps one
session for good, so every round sees what the previous ones did.

## `team.toml`

Lanes live in `team.toml`, next to `config.toml`:

```toml
[[lane]]
name = "WINDOWS"                  # letters, digits, - and _
dir = "~/work/StorePrep"          # the folder the lane works in
model = "deepseek/deepseek-chat"  # optional: provider/model; the folder's config decides otherwise
mode = "accept-edits"             # optional: ask, accept-edits, plan, yolo, read-only
every = "15m"                     # optional: a round every 30s, 15m, 2h (start to start); none: mail only
max_spend_day = 2.0               # optional: dollars a day, rounds and mail together
routine = "Check the orders at the top of docs/STATUS-WINDOWS.md, do the next one, report there."
# or: routine_file = "docs/LANES.md" and routine_section = "WINDOWS"
#     (the first code block under the line "WINDOWS:", read again before each round)
```

A key nexo does not read is reported, with the key it most looks like.

## Commands

| Command | What it does |
| --- | --- |
| `nexo team status` | one line per lane: running a round, idle until HH:MM, paused, held, stopped or failed (and why); the last round's result and age; spend today; its session |
| `nexo team start [NAME...]` | starts the lanes (all of them by default) in the background; held lanes and lanes already running are left alone |
| `nexo team stop [NAME...] [--now]` | the runner ends after the round in flight; `--now` ends it at once |
| `nexo team pause` / `resume` | no lane starts a round while paused; mail waits too |
| `nexo team hold NAME` / `unhold NAME` | `start` skips a held lane |
| `nexo team open NAME` | stops the lane at its next idle point and opens its session in the interface; `nexo team start NAME` puts it back |

## How a lane runs

`start` launches `nexo team run NAME` detached from the terminal: on Linux and macOS in a session of its own
(closing the terminal does not end it), on Windows detached from the console in a process group of its own,
with no window. What it prints goes to `<data dir>/team/NAME.log` (one older copy is kept as `NAME.log.1`
past 5 MB). Its state is in `<data dir>/team/NAME.json`.

The runner sets the session up as `nexo -p` does: a call the lane's mode would ask about is refused, since
nobody is there to answer; the round is marked `denied` and the log says to set `mode` or an allow rule. A
round is `ok`, `failed`, `denied` or `capped` (`max_spend_day` or the session limit reached). Three failed
rounds in a row end the runner, and `status` shows why.

The lane's session is named after the lane, and it is live as any open session is: `/panel` lists it, and
`send_message` or `/msg NAME` reach it. Its mail is answered between rounds.

`stop` and `--now` only ever end the process recorded in the lane's state, after checking that its command
line is `nexo team run NAME`.
