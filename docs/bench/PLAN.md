# Benchmark plan: nexo against Claude Code, same model

Status: published BEFORE any counted run. Nothing in this file may change after the first counted run. If a rule has to change, a new plan is published and the old one stays up.

Items marked `TO FILL` are values that do not exist yet. They are filled in a commit made before the first counted run, and that commit's hash is published first.

## 1. What is compared, and why

Two coding-agent harnesses, each driving the same model:

| | nexo | Claude Code |
| --- | --- | --- |
| Model | `claude-sonnet-5-5` | `claude-sonnet-5-5` |
| Effort | medium | medium (`--effort medium`) |
| Version | commit `NEXO_COMMIT` (musl release build, TO FILL when frozen) | 2.1.322, pinned in the image, auto-update off |

Same model on both sides, so the question is about the harness: how many model calls, how many tokens and how many dollars it takes to get the same job done, not which model is smarter.

The test is a race on cost and effort for work that both sides finish. It is not a test of which tool is nicer to use.

## 2. How each side is run

Both sides run in the same container image (Debian, Python 3, Node, git). nexo is bind-mounted into it as a read-only binary. Claude Code is installed from npm at the pinned version.

| Point | nexo | Claude Code |
| --- | --- | --- |
| Command, one turn | `nexo --provider anthropic --model claude-sonnet-5-5 --yolo --json --max-turns M -p PROMPT` | `claude -p PROMPT --model claude-sonnet-5-5 --effort medium --permission-mode bypassPermissions --disallowedTools=WebSearch,WebFetch --safe-mode --output-format stream-json --verbose --max-turns M` |
| Next turn of a long task | same line plus `-c` (latest session of the folder) | same line plus `--resume <session id>` |
| Config | `hermetic = true`, `reasoning = "medium"`, `[web] enabled = false`, `max_cost` set, `max_tokens` left at default | `--safe-mode` (no CLAUDE.md, skills, plugins, hooks, MCP, memory) and a new empty `CLAUDE_CONFIG_DIR` |
| Web tools | off | off (`WebSearch`, `WebFetch` disallowed) |
| Context management | nexo's own defaults, including its context curator | Claude Code's own defaults, including auto-compact at its default threshold. Nothing is tuned |
| Access | API key | subscription token from `claude setup-token` |

`M` is 25 for visual tasks and 40 for long tasks (per turn).

Rules for every run:

- One fresh container per run, same image, empty home folder (in memory), a work folder with a unique name.
- One run at a time on one machine.
- Order alternates by pair: nexo first, then Claude Code, then the other way round.
- 3 repetitions per task and arm: 30 counted runs, 15 pairs.
- The two runs of a pair share the same nonce and the same seeded inputs.
- Network: containers sit on an internal network with no route out. The only way out is a logging proxy with an exact-host allow list, port 443 only, no wildcards:

```
api.anthropic.com
```

If the pilot proves another host is needed for Claude Code to start, it is added to this list before the plan is frozen, and the final list is the one in the frozen commit.

- Caps per run (both arms): visual 25 model calls per turn, $0.60, 15 minutes. Long: 40 calls per turn, $3.00, 60 minutes. A run that hits a cap is "not solved".
- The launcher refuses to start if any `ANTHROPIC_*` variable exists, and records the variable names (never values) that each container saw.

## 3. Access, and how cost is computed

nexo uses an API key, passed in a variable named `NEXO_BENCH_KEY` and only to the nexo container. Claude Code uses the subscription token, passed as `CLAUDE_CODE_OAUTH_TOKEN` and only to its own container. Neither secret is ever on a command line. The token and key are redacted from every record before anything is stored.

Claude Code on a subscription does not bill per token, and the cost figure each tool prints is its own estimate. So cost is never taken from either tool. For both arms it is computed from raw token counts with one public list-price table (USD per million tokens, `score/prices.json`):

| Model | Input | Output | Cache read | Cache write (5 min) | Cache write (1 h) |
| --- | --- | --- | --- | --- | --- |
| `claude-sonnet-5-5` | 2.00 | 10.00 | 0.10 | 2.50 | 4.00 |

Rules for the dollar figure:

- Headline: every cache write is priced at the 5-minute price in both arms. This favors Claude Code, whose subscription writes with a 1-hour life.
- A second column, "declared", prices Claude Code's 1-hour writes at 4.00. It is shown, but it is not used for the decision.
- The first model call of each run: its cache-read tokens are priced as cache writes, in both arms, so nobody wins by arriving with a warm cache.
- Claude Code's extra calls (sub-agents, summaries, smaller models) count, each model at its own price, taken from `modelUsage` of the final result.
- nexo: the last `usage` event of its JSON log. Claude Code: the final `result` event (in a resumed session it already holds the session total).

What cannot be equalized, and is said here: the subscription is not pay per token, so dollars are the value of the work at list price, not an invoice. Rate limits and server priority differ between the two access routes. The system prompt and tool set are the harness itself.

## 4. The five tasks

Each task has seeded inputs and hidden checks. The nonce and the seeds are drawn at run time from a master seed. The SHA-256 of the master seed is `MASTER_SEED_SHA256` (TO FILL; the launcher prints it and it is published before the first counted run). The seed itself is published after the last run. Hidden tests and truth files are never mounted into the agent's container: they live on the host and run after the last turn.

Placeholders in the prompts: `{NONCE}` is a random code, one per pair, written by the agent into the page. `{N}`, `{R}`, `{W}` are values from the seed. All prompts are in English and are sent verbatim.

### V1: solar system (1 turn)

```
Create a single file index.html in the current directory: a 3D solar system on a full-window canvas, drawn with your own WebGL or 2D-canvas code. No libraries, no CDN, no network requests, no other files. Show a glowing sun, exactly 6 planets with different sizes, colors and orbit speeds, one planet with a ring, one planet with a moon, and a starfield behind. Everything is animated from the moment the page loads, with no click needed, and the camera slowly circles the scene. Draw the text {NONCE} in small letters in the bottom-left corner. Do not ask questions; stop when the file is written.
```

### V2: Breakout (1 turn)

```
Create a single file index.html in the current directory: a complete Breakout game on a canvas, written with your own code. No libraries, no network requests, no other files. It needs a paddle moved with the arrow keys and the mouse, a ball, 6 rows of colored bricks, a score, 3 lives, a win screen, a game-over screen, and a burst of particles when a brick breaks. When the page URL ends in ?demo=1 the game starts by itself and plays itself (the paddle follows the ball), so it can be recorded with no input. Draw the text {NONCE} in small letters in the bottom-left corner. Do not ask questions; stop when the file is written.
```

### V3: bar chart race (1 turn)

`data.csv` is generated from the seed: 12 teams, 30 years.

```
The file data.csv in the current directory has the columns year,team,points (12 teams, 30 years). Create a single file index.html, with the data embedded in it so the page works without data.csv, that plays an animated bar chart race: one step per year at about one second per year, bars sorted by cumulative points and sliding smoothly when ranks change, the top 8 teams visible, each bar labeled with its team name and running total, the year shown large. It starts by itself on load and stops at the last year. No libraries, no network requests. Also write answer.txt with one line, TEAM,TOTAL: the team with the highest cumulative points after the last year and that total. Draw the text {NONCE} in small letters in the bottom-left corner. Do not ask questions; stop when both files are written.
```

### L1: inventory tool (8 turns, one session)

`stock.csv` is generated from the seed. Each block below is one turn, sent in this order to the same session.

```
In this directory build inv.py, a Python 3 command-line stock tool (standard library only): "python3 inv.py add <sku> <qty> <unit_price>", "remove <sku> <qty>" and "list", with data kept in inv.json. Four rules hold for the whole project. R1: a SKU is 3 capital letters, a dash and 4 digits. R2: money is stored as integer cents and printed with 2 decimals. R3: every error prints one line starting with "E:" to stderr and exits with code 2. R4: every listing is sorted by SKU. Write unit tests in test_inv.py and run them.
```

```
Add "import <file.csv>" and load stock.csv from this directory with it. Rows that break a rule are skipped and counted. Print "imported N, skipped M".
```

```
Add "report": the total stock value, then the 5 SKUs with the highest value, one per line as "SKU VALUE".
```

```
Add an append-only audit.log with one line per change, "<UTC ISO time> <command> <sku> <qty> <unit_price>", and a command "history <sku>" that prints the lines of that SKU.
```

```
Add "price <sku> <new_unit_price>", and "report --at <UTC ISO time>" that rebuilds the report as it was at that time from audit.log.
```

```
Refactor into a package inv/ with cli.py, store.py, rules.py and report.py. "python3 inv.py ..." must keep working and every test must still pass.
```

```
Add "export <file.csv>" that writes a file "import" reads back with no change. Test the round trip.
```

```
Check every command built in this session against the four rules from my first message, fix whatever breaks them, run all tests, and write SUMMARY.md with one example per command.
```

### L2: bug hunt in a seeded repo (6 turns, one session)

The repo (`shopflow/`, `docs/RULES.md`, about 40 files, with four seeded bugs) is generated from the seed. `{N}`, `{R}`, `{W}` come from the same seed.

```
This repository is a small order system. Read docs/RULES.md and the code under shopflow/, run "python3 -m unittest", and tell me in at most 10 lines how an order goes from cart to invoice. Change nothing yet.
```

```
Customers report invoice totals that are off by a few cents. Find the cause, fix it, and add a test that fails without your fix.
```

```
Orders with more than {N} units of one item should get the bulk discount of rule {R}, and some do not. Find the cause, fix it, add a test.
```

```
Stock sometimes goes below zero when two orders reserve the same item. Find the cause, fix it, add a test.
```

```
Shipping cost is wrong for orders heavier than {W} kg. Find the cause, fix it, add a test.
```

```
Go back over every change you made in this session and check each one against all the rules in docs/RULES.md. Fix anything that breaks a rule, run the whole suite, and list the files you changed with one line each.
```

## 5. What "solved" means

A run is solved only if it passes every check of its task. Touching a cap (calls, dollars, minutes) means not solved.

| Task | Solved when |
| --- | --- |
| V1, V2, V3 | All of: `index.html` exists and is the only web file (no other `.html .js .css .svg .wasm .json`); the nonce is in the file; headless Chrome (1280x720, software rendering, DNS disabled) shows 0 console errors or exceptions; 0 requests outside `file:`, `data:`, `blob:`; the screenshot at 2 s is not flat (no single color covers 99.5% or more of sampled pixels); the screenshot at 6 s differs from the one at 2 s (more than 0.05% of sampled pixels change by a color distance above 24, so something is animated). V2 is opened with `?demo=1`. V3 also needs `answer.txt` to equal the generator's truth line exactly (after trimming). |
| L1 | Hidden tests, run after turn 8 on a copy, through the command line: at least 90% of all tests pass; every turn group T1 to T8 passes at least 75%; the rule tests R1 to R4 pass at least 90%. Layout details the prompts leave open (report wording, column spacing, log field details) are not tested. |
| L2 | Hidden tests on a copy: every test of the four bug groups B1 to B4 passes (each fails on the seeded bug), every test of the RULES group passes (rules a careless fix would break), and the repo's own `python3 -m unittest` passes. No partial credit. (Whether at least 4 tests were added is recorded but does not decide.) |

## 6. Metrics and scoring

Per run:

| Metric | Source |
| --- | --- |
| Model calls | calls of the main loop (Claude Code: distinct assistant messages; nexo: model responses) |
| Total tokens | input + output + cache read + cache write, all models |
| Dollars at list price | section 3 |
| Wall time | measured by the launcher; shown, not in the index |

Index:

1. Per task and arm, take the median over the repetitions that were solved.
2. A task counts only if both arms solved it in at least 2 of 3 repetitions.
3. For each of calls, tokens and dollars: arm value divided by the best arm's value, averaged over the counted tasks. 1.00 is best.
4. The index of an arm is the mean of those three numbers. It is reported for all counted tasks and, apart, for visual tasks and long tasks.
5. Shown beside the index and not in it: wall time, solve rate, total dollars including failed runs, tool calls.

## 7. Decision rule, fixed in advance

- "Clearly better": the other arm's index is 1.20 or more, AND the winner is cheaper in dollars in at least 12 of the 15 pairs (a pair is the same task and repetition, both arms, solved or not). By chance this passes less than 4% of the time.
- If the gap shows only on the long tasks (the winner cheaper in all 6 long pairs and the long index gap 1.20 or more), the claim is worded "on long sessions".
- Anything else is a tie, and it is reported as a tie.
- If Claude Code wins by this rule, it is reported as a loss for nexo, with the same numbers and no softer wording.
- Tokens and dollars are always published together, never tokens alone.

## 8. Blind human vote (visual tasks only)

It measures quality of the pages, not cost.

- 9 pairs: 3 visual tasks times 3 repetitions. Each pair is a side-by-side clip of 8 seconds (headless Chrome screenshots at about 25 fps; V2 recorded with `?demo=1`).
- Left and right are shuffled at random. Voters answer "left", "right" or "same".
- 15 voters, none of whom has worked on nexo.
- The file that says which side is which arm is hashed, and the hash is published before voting opens: `VOTE_MAP_SHA256` (TO FILL).
- A difference exists only if one arm wins at least 6 of the 9 pairs and at least 60% of the votes that state a preference. Otherwise: no difference.

## 9. Exclusions

A run is dropped from scoring, and listed with the reason, only if:

- the proxy log shows a connection to a host outside the allow list (denied attempts are logged);
- the transcript shows the agent reading benchmark files (rig, checks, other runs, the hidden tests or truth);
- the run failed from infrastructure: server error 5xx or a usage limit after retries, or the container died.

An excluded run is repeated once, and both runs are published. No run is removed because its result is bad. A run that fails by its own fault (wrong output, cap reached, crash of the tool) counts as not solved.

## 10. Budget

- Expected API spend, nexo arm only: about $7.50 (range $4.50 to $11.50) at list price. The launcher adds up nexo's dollars after each run and stops at $12.00 to ask.
- A hard spending limit on the API workspace is set in the console by the owner.
- Claude Code uses subscription quota, not API money.
- A short pilot (under $1) is run first: one V1 and the first turns of L1. It is not counted and its records are published too. It decides only whether the rig works (no request errors, session resume works in both tools, cache reads appear from the second call).
- Extra arm, labeled and outside the headline: Claude Code with auto-compact forced to 100k tokens (`CLAUDE_CODE_AUTO_COMPACT_WINDOW=100000`), on L1 and L2, 3 repetitions each, 6 runs, no API cost. It exists to answer "what if the rival were tuned"; it never enters the index or the decision.

## 11. What will be published

After a secrets scan: all transcripts, token logs, proxy logs, the produced files, screenshots, clips, hidden tests, the generators and the seed, the vote map and raw votes, the price table, the rig, and the scoring script with the command to repeat the test. Anyone can recompute the table from the raw token counts with their own prices.

## 12. Limits of this test

- One model. Results say nothing about other models.
- Five tasks and 3 repetitions each. With this size only differences of about 20% or more can be seen.
- The author of nexo wrote the tasks and the checks. Publishing the plan first, running the rival on its defaults and releasing everything are the answer to that, not a proof it is fair.
- The visual tasks end in 2 to 5 model calls. The context does not grow there, so nexo's context management has little to do. A tie on them is expected. Only the long tasks can show a harness effect.
- Claude Code compacts only near the model's limit by default, which these tasks probably never reach. A low-threshold compaction setting is not tested, except as the labeled extra arm above.
- The model is not deterministic and the sampling settings cannot be fixed. Alternating order, medians and the 12-of-15 rule are the guard.
- The two access routes differ (subscription against API key): dollars are list-price value, not what anyone pays.
- Hidden tests do not cover everything the prompts allow. A solved run is not a perfect run.
