# Offline memory/checker regression gate

Run from the workspace, without provider credentials or an API call:

```sh
cargo test --offline -p nexo-bench -j 2 tasks::session
cargo test --offline -p nexo-curator -j 2 --test it fragment_matrix -- --nocapture
cargo test --offline -p nexo-curator -j 2
```

## session-ops

Requires Python 3. The checker copies the submitted tree, clears the command environment,
uses a 10-second subprocess timeout, and runs the Python client/report/check script with
mocked transport. It checks required keyword-only timeout and forwarding with two values,
both report call paths, endpoint/port, a five-second script timeout, a runtime read of
`secrets/stage.env`, and OK/FAIL for up=1, up=0, missing up, and up=2. Python source keywords
and formatting are no longer the oracle for these behaviors. Config/contact/immutable-file
requirements remain artifact checks. Tests include aliases, pathlib, computed ports,
nonfunctional keyword decoys, incorrect timeouts, constant OK, and original-tree preservation.

The task does not specify an authentication transport contract. Accordingly the checker
observes the staging-file read, not whether the selected token authenticates a server.
It does not certify token parsing in every possible implementation. Mocked sockets prevent
accidental exporter traffic, but are **not a hostile-code sandbox**. As with existing bench
checkers, execute untrusted submissions in an OS-level sandbox when necessary. Missing
Python, timeout, or missing probe output is reported as checker execution failure, not
silently accepted. Partial scores changed; do not compare old and new session-ops scores
without rechecking both saved candidate trees with the same checker.

## Fragment matrix

`fragment_matrix.rs` compares default curator against curator `ratio=1`, on the same
4,000 estimated-token window. No plain arm. Distances are 0/8/32 tool pairs, noise is
1/48 lines per tool result. It measures exact labelled current clause, stale clause,
unrelated surviving fragment, other service's scope, estimated final-view tokens, and
removed/cut messages. Separate tool-document revision fixtures distinguish observations
from user constraints. Each replay repeats with identical logical timestamps to detect
nondeterminism; repeated runs are not independent statistical samples.

Observed on this fixture: both variants produce identical measurements. Current and
unrelated clauses survive, but **stale clauses also remain**. At distance 32/noise 48,
31 removals/cuts occur; final views estimate 3,200 tokens (user facts), 3,275 (tool facts).
This demonstrates exercised compaction and clause availability, not resolved contradictions,
model recall/use, semantic correctness, API dollar savings, or superiority of a candidate.
The tool-revision test now gates current/shell/other-scope retention and exercised
compaction on this fixture, in addition to reproducibility; it is not a universal recall
guarantee. The exact-clause oracle deliberately
does not certify paraphrases; its negative controls reject reversed relations and wrong scopes.

No 30k runtime enforcement, production tokenizer guarantee or statistical quality estimate
is introduced by this gate.

## Follow-up mutation audit

The direct `from urllib.request import urlopen` implementation is now accepted: transport
patching happens before importing the client. A negative mutation that reads the staging
file but then substitutes `wrong-token` still passes, deliberately documenting the task's
read-only contract and the checker's inability to certify authentication. Do not report
this score as token authentication correctness. Existing runtime timeout/output/endpoint
mutations are rejected. Independent deletion of each fragment label is detected; the tool
revision replay now fails on loss of any required current/shell/other-scope clause.

`index.rs` now removes request-only keys before spending its 24-key retrieval budget.
A regression with 23 novel query words plus atlas/boreal requests previously retrieved only
atlas; the candidate retrieves both topics and still suppresses old observations when a
revision is visible. This is index-level evidence, not model-use or task-quality evidence.
The full run exposed changed golden views in `bench-marathon-D` and `bench-marathon4-full`
(+63 and +118 estimated tokens at the first divergent step). Snapshots were not reblessed:
review this retrieval tradeoff before treating the candidate as release-ready.

### Paid comparison preparation (not executed)

Preserve two executables built with the **same checker**, differing only in curator changes,
as `$BASE` and `$CAND`. Use separate output directories and alternate order (AB, BA, AB):

```sh
for order in 'base cand' 'cand base' 'base cand'; do
  round=$(( ${round:-0} + 1 ))
  for variant in $order; do
    if [ "$variant" = base ]; then bin=$BASE; else bin=$CAND; fi
    printf %s "$DEEPSEEK_API_KEY" | "$bin" bench \
      --upstream https://api.deepseek.com/anthropic --model deepseek-chat --key - \
      --arms "${variant}@curator" --tasks session-ops,marathon-noisy \
      --repeats 1 --window 30000 --max-tokens 4096 --max-steps 100 \
      --temperature 0 --budget 1 --run-timeout 1200 \
      --out "$OUT/${round}-${variant}"
  done
done
```

Budget is per invocation (six invocations), not a strict aggregate spend ceiling. The window
is an estimated curated-input limit, not a certified provider input-plus-output cap. Record
provider tokens, dollars, steps, end reason, checks and per-fragment availability separately.
Never count a max-steps or compilation failure as proof that memory was lost.
