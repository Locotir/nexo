# Results

nexo began as a university thesis on context curation. Unless a table says otherwise, the simulated
figures and the live runs below were measured with **an earlier implementation** of the curator, written
for that thesis and not published, from which nexo's `nexo-curator` crate was ported. The port is checked
against fixtures recorded from it (`nexo/crates/nexo-curator/tests/`). Its settings have been retuned
since, so those figures describe the earlier engine, not nexo as shipped.

The raw records named below (`experiments/...`) are **not published**; the tables here carry every
figure they give, and they cannot be reproduced from this repository. The figures you can reproduce
are in "Measured in nexo" and "One long run, step by step".

## The hypothesis

A curator that knows how prompt caching is billed can send each model call a much smaller context than
the whole history, at no loss in task success, and so cut the cost of a long session by more than half;
together with a few efficiency directives to the agent, it can also cut the number of calls and the
wall time. What it cannot do, with the agent making the same calls, is go past a proved ceiling.

Where each claim stands:

| claim | status | evidence |
|---|---|---|
| token savings are not cost savings under a prefix cache | shown | the eager hindsight oracle removes 56% of the tokens but saves 15% of the cost |
| a cache-aware curator saves most of the bill in simulation | shown | D saves 61.5% (5-minute cache) and 56.0% (no expiry) on unseen recorded segments |
| it holds live, at comparable success | shown on one model | DeepSeek: −42% cost for D, −59% with the directives; 10/10 against 9/10 solved |
| fewer calls and less time need the agent to change, not only its context | shown | D alone: −1% calls; D + directives: −23% calls, −41% time |
| no curator sending literal text with the same calls saves more than 89.7% | proved | the ceiling below; a constructed curator reaches 89.3% |


## What a context is used for

Once a session is over its future is known, so the ideal context at each step holds exactly what is
used then or later (the counterpart of Belady's optimal cache policy). Use is traced lexically: an
action uses a piece of context when it repeats one of its distinctive tokens (a path, a host, a version,
a long number, an identifier); names any model knows without being shown them are ignored. Replaying
recorded sessions under hindsight policies showed two things.

- **Tokens are not dollars.** With prompt caching, removing content rewrites the prefix and the rest is
  paid again. The eager oracle removes 56% of the tokens but saves only 15% of the cost, and
  provider-style tool-result clearing costs more than sending everything. *When* to cut matters as much
  as *what* to cut.
- **Choosing afresh at every request fails.** On the recorded sessions, with hindsight:

| clairvoyant policy | tokens sent (of everything) | saving |
|---|---|---|
| exactly what each request uses, chosen afresh, no care for the cache | 48% | **0.7%** |
| exactly what each model call uses, chosen afresh | 34% | 20.3% |
| dead content removed only when the cache allows it (dynamic programming) | 64% | 32.9% |
| floor: what each call uses, all of it already cached | – | 81.6% |

Three facts follow, and the design rests on them: removing from the end is free and from the middle is
not; every goal of a conversation can have its own cached prefix; and within a goal the question is
"needed by this request", since what a later request needs can be appended then, where it costs least.

## D against the earlier engine

B is the earlier engine (paragraphs scored by a learned model under a fixed cap); D organises the
conversation by goal and by request (threads of turns, described below). Replay on recorded and on
synthetic sessions of five unrelated goals; "missed" is the share of details the agent took from older
context that were not in what it was sent:

| sessions | B | D |
|---|---|---|
| recorded (one project, 9 segments, 745 calls) | 32.3% saved, 4.98% missed | **34.3% saved, 4.78% missed** |
| synthetic (8 sessions × 5 goals, 1,043 calls) | 26.0% saved, 14.76% missed | **29.6% saved, 8.32% missed** |

The learned link head that decides whether a request continues an earlier thread ranks held-out pairs
better than a fixed prior (AUC 0.856 against 0.712 on real pairs, 0.937 against 0.905 on synthetic
ones). A fixed 60k cap saved more than no cap (21% against 16% of effective cost) but lost details at 9%
of the steps against 1%: the cap must follow the session, which is what the economic order quantity does.

## A simulated agent

Replaying says what the agent would have been sent, not what it would have done without a detail it
needed. The simulator answers that: the agent does the recorded work step by step, and when its context
lacks something its next action uses, it spends a call getting it back (reading the file again, running
the command again). Every request is priced with the prefix cache. Settings were chosen on training data;
everything below is on segments the model was not trained on (1,419 calls) and synthetic sessions
100–105 (793 calls). Saving net of every recovery call; ×: how much sooner the work ends.

| simulated (saving; time) | recorded, 5-min cache | recorded, no expiry | 5 goals, 5-min cache | 5 goals, no expiry |
|---|---|---|---|---|
| B: the earlier engine | 37.4%, ×1.02 | 28.9%, ×0.99 | 14.6%, ×0.83 | 14.6%, ×0.83 |
| D before the ledger (a 40k view) | 47.6%, ×1.03 | 33.0%, ×1.01 | 42.2%, ×1.12 | 47.3%, ×1.15 |
| the same with a 10k view, no ledger | 53.6%, ×0.96 | 46.1%, ×0.93 | 43.5%, ×1.09 | 46.6%, ×1.10 |
| **D** (a 10k view, 7k of it the ledger) | **61.5%**, ×1.09 | **56.0%**, ×1.06 | 40.2%, ×1.06 | 42.6%, ×1.07 |
| D with an oracle ledger (same budget, perfect prediction) | 71.8%, ×1.17 | 67.4%, ×1.14 | 48.5%, ×1.14 | 50.8%, ×1.15 |

(`experiments/final.json`; segment by segment in `experiments/segments.json`.) The saving grows with the
session: D saves 42–68% on the five segments of over 40 calls and between −34% and +29% on the nine
review-agent sessions of 11–52 calls, where a rewritten view costs more than the few re-reads it spares.
On short synthetic sessions that switch between five unrelated goals the ledger costs 2–5 points. These
figures are for the previous D. The default was retuned since (`eoq=16000`, `protect=12`, and the rules,
notes and pin blocks) because a long agentic run lost its working set at every rewrite with a 10k view.

The ledger holds more of what the work needs in fewer tokens:

| context of each step | tokens (mean) | names it needed that it held | needed names per 1k tokens |
|---|---|---|---|
| everything (what the harness sends) | 133,898 | 100% | 0.033 |
| D before the ledger (40k view) | 45,249 | 86% | 0.083 |
| the same with a 10k view, no ledger | 23,997 | 65% | 0.119 |
| **D** | **25,166** | **93%** | **0.162** |
| D with an oracle ledger | 23,732 | 99% | 0.182 |

That is **5.0 times as many needed names per token** as the full history, and 362 calls spent reading
back instead of 499 with the 40k view (944 with a 10k view and no ledger; the oracle: 56).

## Where the optimisation stops

On the unseen recorded segments, 5-minute cache (no expiry in brackets):

| step | saving | what closes the gap | who can close it |
|---|---|---|---|
| sending everything | 0% | | |
| B, the earlier engine | 37.4% (28.9%) | goals, EOQ, the ledger | done: D |
| **D** | **61.5% (56.0%)** | | |
| D with a perfect predictor of what comes back | 71.8% (67.4%) | foresight of *which* names: the ranking saturates at AUC 0.75 | nobody with the conversation alone |
| clairvoyant that quotes lines, sends what arrives whole | 81.6% (79.3%) | foresight of *when*: each line at the request that uses it | nobody without the next request |
| floor for that family | 84.0% (81.8%) | the prefix cache's last two points | the provider |
| clairvoyant that also cuts each block at arrival and pings (reachable) | 89.3% (88.1%) | foresight of what an arriving block will be needed for | nobody without the whole future |
| **ceiling, proved for every literal curator with the same calls** | **89.7% (88.6%)** | the relaxation's slack; NP-hard to pin exactly | nobody: the optimum lies in here |
| ceiling for any curator at all, summarising included | 91.0% (90.1%) | what a summary could save over a line, under the lexical measure | a different measure of need |
| 95% | | ×2.1 fewer calls even at the ceiling | the agent's work, not its context |

**The ceiling.** Under a prefix cache with read price `ρ`, any curator that sends literal text (whole
blocks or lines of them) and makes the same calls pays at least: the head at every request, the
structure of every message once, the output, and, for every block `b` used at requests `k₁ < k₂ < …`,
the least of `b` the first use needs plus, for every later use, the least it needs times `min(1, Σ w_r)`
over the requests in between (`w_r` is `ρ` while the cache is warm, and after a pause the cheaper of the
full price and the pings that would have kept it warm). Whatever is held of `b` at a use was either held
at every request since the previous use or re-sent in between, and the terms count different tokens, so
they add. The bound was checked against every clairvoyant curator on random sessions and by exhaustive
search on tiny ones. With the least text taken as the names themselves, the same proof bounds
summarising curators too: 91.0%.

**What is left is foresight.** Deciding what to hold in a bounded view is online paging, and every
deterministic online paging algorithm is at least `k`-competitive (Sleator and Tarjan, 1985); the rewrite
decision is rent or buy, where no deterministic rule beats a factor 2 (Karlin, Manasse, Rudolph and
Sleator, 1988). On these recordings the value of foresight is all-or-nothing per request: an oracle that
knows the future of 25%, 50%, 75% or 100% of the names gives 61.8%, 63.1%, 65.7% and 71.4% against D's
60.8% in the same run (`experiments/foresight.json`), and no model of any shape ranks the names better
than the fixed rule (k-nearest neighbours 0.749, logistic regression 0.736, the rule 0.775). Recalling,
just in time, lines that share a name with what has just arrived lost at every budget (unseen data,
61.8% without, 58.4% and 51.2% with 300 and 1,200 tokens per request).

**Time.** Fitted on 688 live DeepSeek requests (R² 0.94): 1.17 s + 0.023 s per 1k uncached + 0.008 s per
1k cached + 2.98 s per 1k output tokens. Input is 8–9% of the model's time, so with the same calls no
curator makes the agent more than ×1.09 faster (Amdahl); finishing sooner needs fewer calls and shorter
answers.

## Live on DeepSeek

`deepseek-chat` through its Messages endpoint, the benchmark's reference agent, the whole history
(`full`) against D; sums over tasks of per-task means (`experiments/live/summary.json`):

| tasks | runs per arm | solved: full / D | calls | time | tokens sent | cost |
|---|---|---|---|---|---|---|
| 3 short (`early-detail`, `noisy-log`, `plan-change`) | 6 | 6/6 · 6/6 | −5% | −5% | −56% | −46% |
| `long-haul` | 2 | 2/2 · 2/2 | −7% | −10% | −35% | −28% |
| `marathon` | 2 | 2/2 · 1/2 | +6% | −4% | −57% | −38% |
| **all** | 10 | **10/10 · 9/10** | **+1%** | **−5%** | **−56%** | **−38%** |

With the **efficiency directives** (one fixed paragraph at the end of the system prompt: batch
independent calls, read only what is needed, check once, close a request in three lines saying what
changed and why), four interleaved arms (`experiments/live/directives.jsonl`):

| arm | solved | calls | time | tokens sent | cost |
|---|---|---|---|---|---|
| full | 10/11 | 112 | 278 s | 3.65M | $0.138 |
| D | 9/10 | −1% | −11% | −60% | −42% |
| **D + directives** | 8/11 | **−23%** | **−41%** | **−72%** | **−59%** |
| full + directives (no marathon) | 8/8 | −5% | −12% | −15% | −9% |

Exact permutation tests: D + directives cut time on every task (−8% to −47%; p < 0.001 relabelling the
runs, p = 0.031 flipping the sign of the per-task differences) and cost by 59% (p < 0.001 / 0.031); D
alone cuts cost by 42% (p = 0.005 / 0.094) but does not separate from zero in time. Every failure, in
any arm, is the same last question of the long tasks ("which modules did you change to make the tests
pass"), which turned out to be ambiguous: four of the six recorded failures listed, truthfully, every
module changed, with the three expected among them.

**At other prices.** The saving in dollars depends most on the price of a cached token
(the price ratios as published in October 2026):

| price of a cached token, a cache write, an output token | D | D + directives |
|---|---|---|
| DeepSeek V3.2 list (the test): 0.1, 1, 1.5 | −42% | −59% |
| cached at 1/2, output ×4 | −55% | −69% |
| cached at 1/4, output ×4 | −49% | −65% |
| cached at 1/10, output ×8 | −35% | −57% |
| cached at 1/10, writes ×1.25, output ×5 | −35% | −56% |
| cached at 1/20, writes ×1.25, output ×5 | −25% | −49% |
| cached at 1/40, writes ×1.25, output ×5 | −16% | −43% |

The cheaper the cache, the less a curator saves in money; with the directives D saves 40–70% at every
price, because fewer calls and less output are savings at any price.

**New tasks, and the marathon again.** Six tasks run once per arm (`experiments/live/new-tasks.jsonl`):
D + directives passed 5/6, the full history 4/6. On the marathon's closing question, after the map of
moved-out requests learnt to name the files changed and the tests that failed and then passed, and the
topic cards to carry the same account (`experiments/live/marathon-map.jsonl`):

| arm | passed | requests | dollars per run |
|---|---|---|---|
| full history (earlier runs) | 3/3 | | 0.08–0.14 |
| D + directives, first map | 2/7 | 57.7 | 0.049 |
| D + directives, `eoq=20000` | 0/2 | 67.0 | 0.058 |
| D + directives, + inline scripts and counts | 2/6 | 53.5 | 0.047 |
| D + directives, + topic cards | **10/15** | 51.3 | 0.045 |

Of the five failures with topic cards, three come from the agent cutting the first test run with
`tail -40` (no history holds a name never shown), one from a write through a helper function (handled
since) and one from a dropped connection (retried since). 10/15 against 3/3 gives p = 0.35: no loss is
shown, and none is ruled out.

## Measured in nexo

**Replay of the public fixtures.** The 7 whole-history recordings in
`nexo/crates/nexo-curator/tests/fixtures/` (`bench-*-full*.jsonl`), replayed offline through the curator
with its default settings (`examples/replay.rs`: what the curator would have sent at each recorded
request; tokens estimated at four characters per token):

| fixture | requests | whole history | curator | change |
|---|---|---|---|---|
| `bench-realm100-full` | 100 | 3.53M | 3.59M | +1.5% |
| `bench-marathon4-full` | 81 | 2.97M | 1.51M | −49.0% |
| `bench-marathon-full` | 55 | 1.63M | 0.86M | −47.6% |
| `bench-dir-full-4` | 22 | 204k | 187k | −8.3% |
| `bench-long-full-1` | 21 | 152k | 127k | −16.5% |
| `bench-results-full-4` | 5 | 21.7k | 21.7k | 0.0% |
| `bench-results-full-0` | 8 | 6.0k | 6.4k | +6.8% |
| **all** | | **8.52M** | **6.30M** | **−26%** |

Reproduce one with `cargo run --release -p nexo-curator --example replay -- crates/nexo-curator/tests/fixtures/<file>.jsonl --quiet`
(from `nexo/`; the `total over all requests` line). The replay cannot show how the model would have
reacted to the smaller view; only a live run does.

Not reproducible from this repository (the recordings are not published): on a scripted 37-call session
through an OpenAI-compatible endpoint, the whole history sends 2.78M tokens and the curator 0.90M (−68%);
with cached input at a tenth of the price, −42% of the input cost. Replaying 28 recorded nexo sessions of
8 to 52 calls: −42% of the tokens sent. The engine scales with
the history: on conversations built from real sessions (measured with the earlier reference
implementation), one call's view takes 5 ms median / 19 ms p95 at 1M tokens of history (0.1 GB of
memory), 6 / 23 ms at 10M (0.24 GB), 9 / 30 ms at 25M (0.5 GB) and 11 / 35 ms at 50M (0.9 GB).

## Threats to validity

- **One project, one person.** The recorded data is this project's own development plus synthetic
  sessions written by its author. The protocol tunes on part of it and reports on the rest, but the rest
  is the same project.
- **One model, few runs.** The live test is `deepseek-chat` on tasks written by the author, two or three
  runs per cell; the exact tests are the honest statement of what separates from zero.
- **A lexical measure.** A need is a name that appears in the next action; influence without copying is
  not counted. Token figures are estimated at four characters per token, low by a median factor of 1.22
  against DeepSeek's counts, so a simulated 62% is 58–66% in the provider's tokens; the dollar figures
  of the live runs use the server's own counts.
- **Comparisons within one run.** The recording grew while it was measured, so only candidates run
  together, on the same data, are compared.

## One long run, step by step (nexo, 100 steps)

The `realm` task (a stripped game binary whose coin rule the agent must rebuild), `deepseek-chat`,
one run per arm, `--max-steps 100`, the nexo curator as shipped:

| arm | score | cost | wall time | tokens sent (uncached + cached) | output | tool errors |
|---|---|---|---|---|---|---|
| plain (whole history) | 1.00% | $0.2659 | 521 s | 157k + 7.63M | 19.7k | 7 |
| **curator** | 1.00% | **$0.1703 (−36%)** | **213 s (−59%)** | 202k + 3.84M | 15.0k | 2 |

Neither arm solves the puzzle in 100 steps (the earlier 1000-step runs put the compact arm at 20.75%);
at the same point of the work the curator spends a third less and runs in less than half the time.

Replaying today's curator offline on a recorded whole-history session that is in the repository
(`nexo/crates/nexo-curator/tests/fixtures/bench-marathon4-full.jsonl`, 81 requests; `examples/replay.rs`, see
[`usage.md`](usage.md)) shows what it would have sent at each request:

![tokens per request](img/tokens-per-step.svg)

Whole session: 2.97M tokens plain, 1.51M curated (**51% kept**); at the last request 60k against 30k.
The dips are rewrites (requests 18, 40, 47 and 73). In 5 of the 80 replies the model used a word that
was no longer in the view (it stays on disk and can be read back). On the 100-step
`bench-realm100-full` recording the curator finds no cut worth breaking the prompt cache for and sends
it whole (+1.5%, table above). The replay cannot show how the model reacts to the notes and nudges;
only a live run does.
