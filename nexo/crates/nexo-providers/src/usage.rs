//! How much of a subscription plan is left: the windows of a ChatGPT plan (5 hours, week) and the
//! premium requests of a GitHub Copilot plan.
//!
//! Two ways in, both from what the vendors' own open-source clients read:
//!
//! - **Passive**: every response of the ChatGPT backend carries `x-codex-primary-*` and
//!   `x-codex-secondary-*` headers (and a `codex.rate_limits` stream event), Copilot's carries
//!   `x-quota-snapshot-premium_interactions`. [`crate::http`] hands the headers of every response to
//!   [`record_headers`], and the figures land in a store shared by the process.
//! - **Active**: [`fetch`] asks for them with the stored login: `GET {backend}/wham/usage` for ChatGPT
//!   (openai/codex, `codex-rs/backend-client`) and `GET api.github.com/copilot_internal/user` for Copilot
//!   (`quota_snapshots`, as the Copilot Chat extension reads it).
//!
//! SuperGrok is read from the endpoint the Grok CLI's own `/usage` reads: `GET {chat proxy}/billing?format=credits`
//! answers with the share of the credits used in the current period and when the period ends.
//!
//! Besides the plans, any provider's rate-limit headers are kept (`x-ratelimit-*` of the OpenAI-style
//! APIs, `anthropic-ratelimit-*`), per host, under the kind `limits:{host}`: what is left of the requests
//! and tokens of the current minute.
//!
//! Claude subscriptions are read from `GET {api}/api/oauth/usage` (`five_hour`, `seven_day`, each with
//! `utilization` in percent and `resets_at`), as Claude Code's own usage view does.
//!
//! No token is ever stored in, or printed from, anything here.

mod claude;
mod codex;
mod copilot;
mod fetch;
mod limits;
mod model;
mod store;
#[cfg(test)]
mod tests;
mod xai;

pub use claude::claude_from_usage;
pub use codex::{codex_from_event, codex_from_headers, codex_from_usage};
pub use copilot::{copilot_from_header, copilot_from_user};
pub use fetch::{fetch, fetch_in};
pub use limits::limits_from_headers;
pub use model::{long_span, short_span, window_label, Item, Meter, PlanUsage, CLAUDE, CODEX, COPILOT, LIMITS, XAI};
pub use store::{all, changed, get, host_of, kind_of, reading_for, record, record_event, record_headers};
pub use xai::xai_from_billing;
