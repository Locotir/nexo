# How the curator is scored

This is the scoring protocol for every result in the report. It is fixed
**before** the runs it judges. A change to it is recorded in the log at the end,
with its date, and is never made after looking at the runs it would change.

The question is the one in the title. Can a curated context cost less than
sending the whole history **without** the agent doing its work worse? So
quality is the primary endpoint and cost the secondary one. A cheaper arm that
solves fewer tasks has not won.

## 1. Units

| Term | Meaning |
|---|---|
| **task** | one benchmark task (a task of `nexo bench`): files, the user's turns, an automatic checker and a reference solution |
| **arm** | one way of building the context sent upstream. `full` (send everything, the baseline; `plain` in `nexo bench`) or a curator setting such as `D` |
| **run** | one (task, arm, model, repetition). The agent works the task from a fresh copy of its files |
| **model** | the upstream LLM. Results are reported **per model**, never pooled across models |

Every checker is tested against the reference solution: it must fail on the
untouched files and pass after the solution. The
checker is the only judge. No run is scored by hand or by another LLM.

## 2. Primary endpoint: does the agent still do the work?

**Score of a run** `s ∈ [0, 1]`:

- `1` if the checker passes;
- otherwise the task's partial credit, when the task defines one:
  - **realm**: the share of the 400 unseen cases the answer file (`coins.rs`) gets right (0 if it
    does not run or calls the game);
  - **three-sessions, selective-revert, stale-doc**: the share of their
    independent requirements that hold (for example, *shipping back to the
    original*, *tax kept*, *invoice right*);
- `0` for the tasks that have no partial credit (pass or fail).

**Score of an arm** = the mean over tasks of its per-task mean score. Every task
weighs the same, however many runs it had.

**Decision: non-inferiority.** Let `Δ = score(arm) − score(full)`, computed over
the tasks both arms ran. Its 90% interval comes from resampling the runs of
every (task, arm) with replacement, 2,000 times.

> The arm is **non-inferior** when the lower end of that interval is above
> **−0.10** (ten points). This is a one-sided test at 5%.

The pilot of §8 was summed up this way (margin 0.10) with the tooling of an
earlier, unpublished implementation; its records are not in this repository.

## 3. Secondary endpoint: what does it cost?

**Dollars per run**, from the token counts the **provider reports** (uncached
input, cache reads, cache writes, output) at the model's published prices. They
are not our own token estimates.

The runner prices runs at DeepSeek's prices. For any other model, the same
reported counts are re-priced at that provider's ratios, checked on its pricing page on the day of the run.

**Decision: superiority**, tested only if the arm is non-inferior on §2. The
relative difference of the summed per-task means against `full`, with the same
resampling. The arm is **cheaper** when the upper end of its 90% interval is
below 0%.

## 4. Reported, not decided on

Always reported for every arm, so none of them is picked after the fact:

- model calls, wall-clock seconds, input tokens sent, uncached input tokens and
  output tokens;
- loop signs: repeated calls, cycles, undone edits, runs that hit the step
  limit, and runs where the agent asked the user instead of acting.

## 5. Offline evaluation (no model calls)

The offline results replay recorded sessions and synthetic ones through the
simulated agent of an earlier, unpublished implementation.

- **Tuning and report data are disjoint.**
  - Settings are chosen on the *tune* data: the recorded segments the model was
    trained on, and synthetic sessions 0–5.
  - Results are given only on the *report* data: segments it never saw, and
    synthetic sessions 100–105.
- **Saving** is the share of the cost of sending everything that is saved, net
  of every recovery call a miss causes. It is given with:
  - the **oracle** saving: the same budget with a perfect predictor;
  - the proved **lower bound** on any policy;
  - the **share of the oracle saving captured**, which is objective O7 of the
    proposal.
- **Cold tests (no model)**: the curator's view is scored on generated sessions with questions that need
  facts from far back (`examples/scale.rs`, cold mode). *Recall* is the share of questions whose needed
  lines are in the view; *standing facts* the share of tasks whose supposed default has its fact in the
  view; *prefix* the median share of a request's prefix equal to the previous request's. These do not
  need the §2 interval: they are counts, reported with the commit.
- **Misses**: the share of names a step needed from older context that its
  context did not hold (coverage), and those names per 1,000 tokens (density).
- The **quality gate** is frozen on the report segments.
  A change that makes any of its numbers worse is not adopted.

## 6. How runs are made

- **Fixed before running**:
  - the task list, the arms, the model and its settings (temperature,
    max tokens, step limit);
  - the number of repetitions;
  - the budget cap (the runner starts no run once it is reached).
- **Order**: every repetition runs the tasks one after another, and within a
  task every arm back to back. The order of the arms flips from one repetition
  to the next, so neither arm always goes first.
- **Repetitions**: at least 3 per (task, arm, model).
- **Exclusions** (each excluded run is listed in the report with its reason):
  - a run whose provider failed (an HTTP 5xx or 429 that the retries did not
    clear) is run again once. If it fails again it counts as a 0;
  - a run the runner flags as `ctx_suspect` (the server truncated the prompt)
    measures the server, not the arm. It is excluded and the server is fixed
    first.
- **No leaks, no damage**: the agent's commands never see the runner's
  secrets, and every task runs *sealed* where Linux namespaces are available:
  its commands run in their own namespaces (`unshare`), with the system's
  directories read-only and a scratch tmpfs over the user's files and the
  benchmark's source (which holds the rules and the unseen cases), and with no
  network and no other process in sight. Only the working directory can be
  changed, so a command like `rm -rf ~` cannot reach anything real. Each result
  records `sealed`. Realm must be sealed: the runner refuses to start it where
  that cannot be set up.
- **Tasks**: `ALL` (6 short tasks), `LONG` (long-haul, marathon), `MORE`
  (three-sessions, selective-revert, stale-doc) and `HARD` (realm, needs a C
  compiler).

## 7. How many runs are enough

Pass or fail outcomes are noisy. Suppose both arms truly solve about 90% of
runs. Then the difference of one pair of runs has a variance near
`2 × 0.9 × 0.1 = 0.18`. Showing non-inferiority at a ten-point margin needs the
standard error of `Δ` below about `0.10 / 1.645 ≈ 0.06`. That is roughly
**50 pairs of runs per model**.

Partial credit lowers the variance, and so lowers that number. The runs so far
(about 10 per arm) are a pilot. They give the size of the effect, not the
verdict.

## 8. Where the evidence stands (pilot, DeepSeek)

This pilot is the last run scored under §2 and §3. It was measured with the earlier implementation, before
2026-10-30 (no commit), on a different curator tuning from today's. Later evidence is in
[`results.md`](results.md): a paired live run at `575380d` on 2026-11-09 that did not meet its target, and
a 16-task harness race that ranks nothing. Neither was scored by §2.

| Arm against `full` | Score Δ [90%] | Verdict on §2 | Dollars [90%] |
|---|---|---|---|
| `D` (5 tasks, 10 runs) | −10 pts [−20, 0] | **not yet shown** | −42% [−52, −27] |
| `D-no-ledger` (4 tasks, 8 runs) | 0 pts [0, 0] | non-inferior (small sample) | −35% [−47, −20] |

In that pilot `D` was cheaper. Whether it was as good was not shown: more runs were needed, which §7
says. Today's default is not `D` of the pilot, so the pilot says nothing about it.

## 9. Change log

- **2026-11-09.** Adds the cold tests of §5, and the harness race, scored apart. In the race every arm gets
  one valid attempt per task, pass rates carry Wilson 95 % intervals, and no ranking is claimed when
  intervals overlap. It was not pre-registered under §6, so it is reported as a comparison, not a decision.
  The pilot of §8 is marked as the earlier engine's.
- **2026-10-28.** First version.
  - Adds partial credit to realm and to the three new tasks.
  - Adds the non-inferiority margin of ten points.
  - Puts the earlier live runs under this protocol as a pilot (§8).
  - Seals realm before its first live run (§6, *No leaks*).
- **2026-10-30.** The benchmark runs in nexo (`nexo bench`, arm `plain` for
  `full`); the realm answer is `coins.rs` and the small tasks are Rust
  mini-projects. The pilot of §8 was run with the earlier reference
  implementation.
