# Benchmark results: nexo against Claude Code, same model

Run on 2026-11-10, 13:53 to 14:49 CET. Plan: [`PLAN.md`](PLAN.md), published before the run and unchanged since commit `79ad041`.

## Verdict

By the rule of section 7 of the plan: **nexo is clearly better** on this test. The Claude Code index is 1.47 against 1.00 for nexo (the rule asks for 1.20 or more), and nexo was cheaper in dollars in 15 of 15 pairs (the rule asks for 12). The gap also shows on the visual tasks alone, so the plain claim applies, not the "on long sessions" wording.

This holds for these 5 tasks, one model and 3 repetitions each. See [Limits](#limits).

| | nexo | Claude Code |
| --- | --- | --- |
| Model | `claude-sonnet-5-5`, effort medium | `claude-sonnet-5-5`, effort medium |
| Version | commit `bf57ed4`, API key | 2.1.322 (from the `claude_code_version` field of its init event), subscription token |
| Solved | 15 of 15 | 14 of 15 |

The one failed run is `full-p08-cc-V3-r2`: it failed the check `nonce_in_file`. It counts as not solved, stays in the totals and in the pairs, and is left out of the medians.

## Index

1.00 is best. Per task, the median over solved repetitions; each metric is the arm's value over the best arm's, averaged over the tasks; the index is the mean of the three. All 5 tasks counted (both arms solved at least 2 of 3 repetitions of each). Scorer: `score/index.py`, 0 runs excluded.

| Group | Tasks | nexo | Claude Code | calls | tokens | usd |
| --- | --- | --- | --- | --- | --- | --- |
| Total | 5 | 1.00 | 1.4656 | 1.2425 | 1.7378 | 1.4165 |
| Visual (V1, V2, V3) | 3 | 1.00 | 1.4243 | 1.1667 | 1.7928 | 1.3134 |
| Long (L1, L2) | 2 | 1.00 | 1.5276 | 1.3562 | 1.6554 | 1.5712 |

The calls, tokens and usd columns are the Claude Code values. For nexo they are 1.00 in every row.

## Per task

Median per arm over solved repetitions. Dollars are list price (see [Limits](#limits)).

| Task | Arm | Solved | Calls | Tokens | $ | Seconds |
| --- | --- | --- | --- | --- | --- | --- |
| V1 solar system | nexo | 3/3 | 2 | 24,394 | 0.0720 | 30.9 |
| V1 solar system | Claude Code | 3/3 | 2 | 41,334 | 0.0983 | 35.8 |
| V2 Breakout | nexo | 3/3 | 2 | 21,320 | 0.0609 | 21.1 |
| V2 Breakout | Claude Code | 3/3 | 2 | 39,584 | 0.0878 | 30.8 |
| V3 bar chart race | nexo | 3/3 | 2 | 28,927 | 0.0601 | 17.9 |
| V3 bar chart race | Claude Code | 2/3 | 3 | 52,863 | 0.0681 | 25.1 |
| L1 inventory tool | nexo | 3/3 | 23 | 808,079 | 0.4679 | 174.0 |
| L1 inventory tool | Claude Code | 3/3 | 27 | 1,356,973 | 0.8717 | 434.3 |
| L2 bug hunt | nexo | 3/3 | 13 | 330,953 | 0.1802 | 60.1 |
| L2 bug hunt | Claude Code | 3/3 | 20 | 539,932 | 0.2305 | 144.0 |

## Totals

All 30 runs, including the failed one.

| | nexo | Claude Code |
| --- | --- | --- |
| Model calls | 126 | 164 |
| Tokens (input, output, cache read, cache write) | 3,645,331 | 6,042,276 |
| Output tokens | 126,658 | 213,414 |
| Dollars at list price (headline, all cache writes at the 5-minute price) | 2.4824 | 3.9745 |
| Dollars with Claude Code's 1-hour cache writes at the 1-hour price ("declared") | 2.4824 | 4.4552 |
| Dollars by Claude Code's own method (`usd_warm_declared`: no cold-first-call move, 1-hour writes at the 1-hour price) | 2.2980 | 3.9673 |
| Wall time, seconds (not in the index) | 912.5 | 2014.5 |

The headline is the figure the decision uses. The two other dollar rows are shown because the plan said they would be. They do not change the order. For nexo, the "declared" row equals the headline.

## The 15 pairs

Same task and repetition, both arms. Dollars at list price. nexo is cheaper in all 15.

| Task | Rep | nexo $ | Claude Code $ |
| --- | --- | --- | --- |
| V1 | 1 | 0.0690 | 0.0983 |
| V1 | 2 | 0.0720 | 0.0978 |
| V1 | 3 | 0.0803 | 0.0995 |
| V2 | 1 | 0.0577 | 0.0871 |
| V2 | 2 | 0.0612 | 0.0878 |
| V2 | 3 | 0.0609 | 0.0889 |
| V3 | 1 | 0.0601 | 0.0676 |
| V3 | 2 | 0.0601 | 0.0671 (not solved) |
| V3 | 3 | 0.0549 | 0.0686 |
| L1 | 1 | 0.4679 | 0.7173 |
| L1 | 2 | 0.3958 | 0.8717 |
| L1 | 3 | 0.5077 | 0.9326 |
| L2 | 1 | 0.1802 | 0.2603 |
| L2 | 2 | 0.1656 | 0.1996 |
| L2 | 3 | 0.1893 | 0.2305 |

## Checks done

All from the run folder `rig/out/20261110-135329-full` (30 runs). Exclusions file: `_post/exclusions-final.json`.

- **Network.** The only denied hosts were the two start-up hosts named in section 9 of the plan: `models.dev` (nexo, 1 request in each of its 15 runs) and `http-intake.logs.us5.datadoghq.com` (Claude Code, 1 request in each of its 15 runs). All 30 runs are listed with their host. No model tool call named either host.
- **Keys.** 0 key strings found in the records. The launcher's leak check reads `leak=False` for 30 of 30 runs, and no run saw the other arm's secret.
- **Variable names.** 0 `ANTHROPIC_*` variable names in the environment of the 30 containers (recorded as names only, never values).
- **One flagged run, read by hand.** `full-p10-cc-L1-r1` was flagged `parent-escape`. It copied its own files to `/tmp` inside its container (`/tmp/inv.json`, `/tmp/inv_old.py`, `/tmp/out_*.txt`) as scratch. It read no benchmark file and used no network. Not excluded, because section 9 of the plan lists no such case.
- **Exclusions.** 0 runs excluded, so no run was repeated.

## Blind vote

Voting has not opened. The vote covers the 9 visual pairs (section 8 of the plan). The hash of the file that says which side is which arm is published here, before voting opens:

`VOTE_MAP_SHA256 = 5863fc579cf6672be8f4e67117af7a919d4fe3bc1fccc47261684dccec9fde4b`

The map and the raw votes will be published when voting closes. The result of the vote will be added to this page. It measures the quality of the pages, not cost, and it is not part of the verdict above.

## Deviations from the plan

- The plan said every `TO FILL` value would be filled before the first counted run. The vote map hash cannot exist before the clips, and the clips come from the counted runs. So `VOTE_MAP_SHA256` is published here, before voting opens, and not in `PLAN.md`. `PLAN.md` is unchanged since commit `79ad041`.
- The extra arm of the plan (Claude Code with auto-compact forced to 100k tokens) is not in these results. Nothing in this page depends on it.

## Limits

- 5 tasks, 3 repetitions each, one model, one day. Only differences of about 20% or more can be seen.
- The author of nexo wrote the tasks and the checks. Publishing the plan first, running the rival on its defaults and releasing the records are the answer to that, not a proof it is fair.
- These sessions are short. Averaged over a run, the model was sent 10 to 52 thousand tokens per call (tokens divided by calls; for the long tasks 25 to 52 thousand), well below the context window. They do not measure the context curator's eviction against compaction. The scorer recorded 0 compactions for Claude Code. We read the gap here as coming from fewer calls, less output and a smaller fixed prompt, not from the curator's eviction. The three were not measured apart.
- Claude Code's dollars are its token counts at list price, because it ran on a subscription and was not billed per token. Cache writes are priced at the 5-minute rate for both arms in the headline.
- Wall time is reported but is not in the index. The two arms use different access paths (API key against subscription token), so seconds are not a fair comparison.
- nexo's cost has not yet been cross-checked against the provider's billing page. That check is pending.
- The hidden checks do not cover everything the prompts allow. A solved run is not a perfect run.
- The raw records (transcripts, token logs, proxy logs, produced files, hidden checks, rig and scoring tools, with the home path of the test machine replaced) are packed and will be attached to a GitHub release when the blind vote closes. They cannot go out earlier: they hold the pages the voters judge blind and say which arm wrote each. The archive's SHA-256 is fixed now: `ccdf33fef4cd7677cc37020f7dac1be9c2ea72add547b61e236412ed8e7ded6f`. Until the release, the figures here cannot be recomputed by others.

nexo is an independent project. It is not affiliated with or endorsed by Anthropic. Claude and Claude Code are Anthropic products.
