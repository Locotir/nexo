# Benchmark results: nexo against Codex CLI, same model

Run on 2026-11-10. This is a smaller test than the one against Claude Code ([`RESULTS.md`](RESULTS.md)): one run per tool on the long tasks, and no plan published before the run. Read it as a demonstration, not as a statistic. See [Limits](#limits).

**In short:** on a long session with the same model, Codex CLI compacted its context twice, lost five values the user had given in the first two turns, and did not solve the task. nexo did not compact, still had them, and solved it.

## Setup

| | nexo | Codex CLI |
| --- | --- | --- |
| Model | `gpt-5.6-luna`, reasoning effort medium | `gpt-5.6-luna`, reasoning effort medium |
| Version | commit `7cd8f27` (long session A), `bf57ed4` (B and C) | 0.167.0 |
| Account | the same ChatGPT subscription for both | the same ChatGPT subscription for both |
| Sandbox | its own, with an empty home | its own, with an empty home |

The subscription does not bill per token. The dollar columns use third-party list prices per million tokens: $0.20 input, $0.02 cached input, $1.20 output. Some third-party reports give a higher rate for requests above 272,000 tokens ($0.40 input, $1.80 output); with that rate the long session comes to $0.233 for nexo and $0.292 for Codex CLI.

## A. Long session (the headline)

Task `marathon-pasted` from nexo's own bench: 10 user turns on a small Rust crate, about 865,000 characters of pasted material across the requests. It is graded by hidden Rust tests and by a final `RELEASE.json` that must repeat values given in the first message and in a CI log pasted in the second turn. One run per tool.

| | nexo | Codex CLI |
| --- | --- | --- |
| Solved | yes | no |
| Model calls | 27 | 42 |
| Input tokens (cached) | 3,859,462 (3,207,552) | 6,533,932 (5,818,752) |
| Output tokens | 7,828 | 10,770 |
| List price | $0.204 | $0.272 |
| Wall time | 215 s | 350 s |
| Compactions | 0 | 2 |

Why Codex CLI failed: after its two compactions it no longer had five values it needed at the end: the codename, build number and maintainer from the first message, and the failed CI run and its job id from a log pasted in the second turn. In the last turn it answered "the conversation contains no values for `codename`, `build`, `maintainer`, `ci_run`, or `ci_job`" and asked for them instead of writing the file.

## B. An earlier long run, same day

Task `marathon-noisy`, 10 turns, nexo commit `bf57ed4`, one run per tool.

| | nexo | Codex CLI |
| --- | --- | --- |
| Solved | yes | no (two wrong values in `RELEASE.json`) |
| Model calls | 33 | 38 |
| Input tokens (cached) | 2,823,025 (2,513,920) | 3,672,795 (3,494,400) |
| Output tokens | 8,714 | 10,676 |
| List price | $0.123 | $0.118 |

Codex CLI did not compact in this run. Here nexo's list price was slightly higher than Codex CLI's. Two context rewrites re-sent old history. Fixed size limits on file reads landed after this run.

## C. Three short visual tasks

V1, V2 and V3, the same one-shot tasks as in the Claude Code test, 3 pairs each, nexo `bf57ed4`. The two longer tasks of that set were not run (stopped early). Totals over the 9 pairs:

| | nexo | Codex CLI |
| --- | --- | --- |
| Model calls | 25 | 39 |
| Tokens | 221,801 | 698,213 |
| List price | $0.0515 | $0.0769 |
| Solved | 8 of 9 | 9 of 9 |

nexo's one failure was a page with a JavaScript error, delivered without checking it.

Index by the rule of the Claude Code page (1.00 is best): nexo 1.01, Codex CLI 1.80. But nexo was cheaper in only 6 of 9 pairs. That page's rule asks for an index gap of 1.20 and cheaper in 80 % of pairs, so on the short tasks the verdict is a **tie**.

## Reading it

- On the long session, nexo solved the task and Codex CLI did not, with fewer calls, fewer input tokens and a lower list price.
- The failure has a cause in the log: compaction dropped facts given in the first turn and needed in the last one.
- On the earlier long run, nexo solved the task with fewer calls and Codex CLI did not, but nexo's list price was slightly higher.
- On short tasks, the result is a tie by the stated rule.

## Limits

- One run per tool on the long tasks. This is a demonstration, not a statistic.
- The author of nexo wrote the tasks and the checks. `marathon-pasted` is built to push history past the window, which is where a tool that compacts loses facts. A task that fits in the window would not show this.
- List prices are third-party. Neither tool was billed per token.
- No pre-registered plan for this test, unlike the Claude Code one.
- Codex CLI's call count comes from its event log.
- The hidden checks do not cover everything the prompts allow. A solved run is not a perfect run.

nexo is an independent project. It is not affiliated with or endorsed by OpenAI.
