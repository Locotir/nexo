# What is left of a subscription plan

When nexo runs on a subscription login instead of a key, it shows how much of
the plan is left, like the Codex CLI's `/status`. Nothing is shown for a
provider that runs on a key: there is no plan to read.

## Where it shows

- **Status bar**: a compact item before the token count, `5h 37% ↻2h · wk 12% ↻4d`
  for ChatGPT, `premium 210/300 ↻12d` for Copilot. Warm from 80%, red when a
  window is spent. A bar under 100 columns drops the reset times, one under 60
  drops the item. A window whose reset time has passed leaves the bar until a
  new reading arrives. Click it to open `/usage`. `status_line` takes the piece
  name `plan` to move or remove it.
- **`/usage`** (alias of `/cost`): the Usage tab starts with the plan, one bar
  per window with the share used, the requests used and granted when the plan
  counts them, and when it starts over.
- **`nexo usage`**: the same, headless, for every subscription login in the
  credential store. Exit 1 when there is none.
- **`nexo -p ... --output-format json`**: the `result` object gets a
  `plan_usage` array when the backend reported the plan during the request
  (see [HEADLESS.md](HEADLESS.md)).

## Where the figures come from

Both ways use what the vendors' open-source clients read; nothing else is
guessed.

| Plan | Passive (every response) | Active (`nexo usage`, the TUI at start and every 10 minutes) |
|---|---|---|
| ChatGPT (`codex`) | headers `x-codex-primary-*` and `x-codex-secondary-*` (`used-percent`, `window-minutes`, `reset-at` or `reset-after-seconds`), `x-codex-plan-type`, `x-codex-credits-*`, and the `codex.rate_limits` stream event | `GET https://chatgpt.com/backend-api/wham/usage` (openai/codex, `codex-rs/backend-client`): `rate_limit.primary_window` and `secondary_window`, `credits`, `plan_type` |
| GitHub Copilot (`copilot`) | header `x-quota-snapshot-premium_interactions` (`ent`, `rem`, `rst`) | `GET https://api.github.com/copilot_internal/user`: `quota_snapshots.premium_interactions` (and `chat` when it is not unlimited), `quota_reset_date` (as the Copilot Chat extension reads it) |

A response of any kind counts, an error one too, so the figures are fresh when
a limit is reached. The window names come from its length: 300 minutes is `5h`,
10080 is `wk`. A Plus plan today reports one weekly window; the slot that is
not in use comes as zeros and is ignored. An unlimited Copilot quota has no
gauge.

## Not covered

| Subscription | Why |
|---|---|
| SuperGrok / X Premium (`xai`) | no documented endpoint and no open-source client reads one; `nexo usage` says so |
| Gemini, Qwen and other vendors | nexo has no subscription login for them (they run on keys) |
| ChatGPT limits beyond the default `codex` family (`additional_rate_limits`, per-model metered limits) | not read; only the plan's own windows are |
| Copilot Free's `limited_user_quotas` | only `quota_snapshots` is read |

## Privacy

The login is only used to ask the vendor for its own figures. No token is
stored with them, printed, logged or put in an error. The figures are kept in
memory for the session.
