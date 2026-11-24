# `/budget 5%`: safety assessment and implementation contract

Status: explored, **not enabled**. `/budget 5%` and `$5%` are explicitly
rejected; no percentage protection is installed, no request is sent and an
existing dollar budget is not changed. Dollar budgets remain available.
The current subscription readings are display telemetry, not an admission
ledger. A green quota indicator is not a spending guarantee.

## Why the existing readings cannot enforce it

- `crates/nexo-providers/src/usage/store.rs`: STORE is keyed only by plan kind
  (`codex`, `copilot`, `xai`), not authenticated account or credential generation.
  Passive headers carry no request/account identity into that store. Two logins
  can overwrite or merge one another. `reading_for` can fall back to host rate
  limits, which are not subscription quota.
- `usage/model.rs`: merge replaces individual slots and retains missing ones;
  `seen` is updated for the envelope, not proof every slot was observed then.
  `is_stale` tests the reset deadline only. A missing reset or an old observation
  is not sufficient evidence for budgeting. Some parsers do not populate a
  per-slot observation timestamp. Out-of-order requests have no sequence guard.
- `usage/codex.rs`: only default primary/secondary windows are read. Additional
  model-family limits are not covered. Copilot and SuperGrok have different units
  and scopes; an unlimited or absent meter is not zero cost.
- Provider percentages cover the entire account, including other clients and
  sessions. Their difference is not attributable cost of this session, tool
  block or subagent. Rounded/delayed readings also hide small requests.
- `crates/nexo-agent/src/turn/round.rs` admits once before calling the model
  client; `crates/nexo-providers/src/client.rs` can issue multiple HTTP attempts
  and reopen a failed pre-content stream internally. An admission check outside
  those retry paths cannot protect each provider request.
- The existing dollar reservation has a different unit. There is no documented
  upper bound converting tokens or API-equivalent dollars to subscription
  percentage. Checking the latest percentage before a request cannot prevent
  that request (or concurrent in-flight calls) from crossing the threshold.

Consequently, converting 5% to dollars, subtracting global readings as session
consumption, or silently choosing the weekly window would give false protection.

## Contract required before enabling a percentage command

The following is a proposed design, not functionality currently implemented.

1. **Define the unit explicitly.** Use five *percentage points of total quota*,
   not 5% of quota remaining. Bind to a documented provider/model-family quota
   scope and explicit window(s). Show the baseline, target, scope and reset time
   before confirming. For a multi-window plan, do not choose one implicitly.
   If only global telemetry is available, offer an explicitly named account-wide
   observed-delta stop, not an attributable session cost or a hard 5% cap.
2. **Bind identity end to end.** Carry an opaque account identifier, credential
   generation, effective endpoint/provider/model-family and observation sequence
   through request, response and quota store. Never use access tokens as keys or
   print them. Reject old-generation/old-sequence responses after account or
   target changes; do not merge retained slots across identities or windows.
3. **Complete, fresh baseline.** Activation needs a validated, complete reading
   of every selected slot from that identity. Require finite percentages, known
   reset/window identity, per-slot observation time and bounded age. A new
   envelope timestamp cannot refresh omitted slots. Unknown, partial, future,
   stale or contradictory readings block activation/admission; never treat them
   as zero. Choose and document a provider-specific freshness limit.
4. **One admission authority.** All main calls, subagents, scheduled/background
   requests and both retry routes must acquire authorization immediately before
   sending, under one account/scope ledger. Activation during active streams
   must wait for settlement or refuse: their unobserved consumption cannot be
   excluded from a new baseline. HTTP and pre-content SSE retries need the same
   gate, not merely a notification after the attempt has been made.
5. **State the actual guarantee.** A hard cap requires a trusted per-request
   upper bound and atomic reservations for in-flight work, including other
   processes sharing the account. Otherwise only best-effort stopping before
   the *next* request is possible. Say explicitly that the last request, other
   clients, delayed telemetry and cancellation can overshoot. Do not advertise
   that behavior as a strict spending limit.
6. **Never silently replenish.** A reset deadline, changing reset identifier,
   decreasing counter, account switch, logout/login, model-family switch or
   plan/entitlement change invalidates the baseline and blocks further protected
   calls. Request explicit reactivation with a new complete reading; a reset is
   not evidence for a free fresh 5%. Preserve already-accounted consumption and
   uncertainty when a request is cancelled; cancellation is not a refund.
7. **Lifecycle and restart.** Session clear/load/rewind/fork must have explicit
   policy ownership and generation changes; a rewind must not undo real spend.
   Persist a versioned baseline/identity/window ledger atomically if budgets are
   resumable. On restart, incomplete or incompatible persisted data stays blocked
   until reconciliation and explicit reactivation. Reject delayed callbacks from
   prior session generations. `/budget off` is an explicit opt-out, not a reset.
8. **Keep USD independent.** A rejected percentage command cannot remove/raise
   session or daily dollar limits. If both modes become supported, document
   composition and require both guards to admit, rather than implicitly replacing
   one with the other.

## Verification and next acceptance gate

The existing rejection path is exercised by TUI tests in
`crates/nexo-tui/src/app/tests/custom_commands_history_and_cost.rs`:

- Valid-looking, reset, missing-date, invalid and empty readings, plus no reading.
- Idle and active-turn command handling; no command/request emitted, no turn
  cancellation, and unchanged USD budget and plan reading.
- Session history clear, session identity change and provider/model change.

These tests prove **safe rejection only**, not a percentage quota implementation.
Before enabling one, offline fake-provider tests must additionally cover: two
accounts of the same vendor; partial and out-of-order observations; per-model
quota selection; concurrent admission/reservations and activation during streams;
subagents; retry before HTTP stream and retry after pre-content SSE failure;
cancellation with uncertain final billing; reset/counter regression; target and
credential changes; clear/load/rewind/fork; and restart with corrupted, obsolete or
unconfirmed ledger data. A strict cap additionally needs evidence for a provider
upper-bound/reservation protocol; without it, retain the explicit best-effort label.
