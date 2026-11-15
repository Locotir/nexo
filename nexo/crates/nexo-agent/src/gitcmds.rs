//! The git commands `/review`, `/commit` and `/pr` (and `nexo review`): the git facts are
//! gathered here, up front, and put into one well-made prompt, so the model does not spend
//! steps asking for the status, the diff and the log. A diff too big for the prompt is cut
//! by file and by hunk (the biggest hunks stay), never summarised by a model.

mod diff;
mod findings;
mod prompts;
mod run;

pub use diff::{cap_diff, Target, DIFF_CAP};
pub use findings::{findings_json, has_high, parse_findings, render_findings, Finding};
pub use prompts::{commit_prompt, gh_ready, pr_prompt, review_prompt, review_prompt_json};

#[cfg(test)]
mod tests;
