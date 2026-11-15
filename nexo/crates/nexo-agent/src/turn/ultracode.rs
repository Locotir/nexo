//! Local, non-secret planning facts for an ultracode request. Never resolve credentials here:
//! a key command may have side effects, and readiness is not proof that an endpoint works.

use nexo_core::config::Config;
use nexo_providers::usage::{self, PlanUsage};

/// A proposal is untrusted until validated; the executor must retain this exact
/// value across the approval and execution boundary, never reparse model text.
#[derive(Debug, Clone, serde::Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
#[allow(dead_code)]
pub(super) struct Plan {
    pub steps: Vec<Step>,
}

#[derive(Debug, Clone, serde::Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
#[allow(dead_code)]
pub(super) struct Step {
    pub agent: String,
    pub prompt: String,
    pub provider: String,
    pub model: String,
    pub start_clean: bool,
}

#[allow(dead_code)]
impl Plan {
    /// Reject implicit launch choices and bound approval UI/input size.
    pub fn validate(&self, config: &Config, agents: &[String]) -> Result<(), String> {
        if self.steps.is_empty() || self.steps.len() > 20 {
            return Err("a plan must contain between 1 and 20 sequential steps".into());
        }
        for (i, step) in self.steps.iter().enumerate() {
            if [&step.agent, &step.prompt, &step.provider, &step.model]
                .iter()
                .any(|s| s.trim().is_empty() || s.len() > 32_768)
            {
                return Err(format!("step {} has an empty or oversized field", i + 1));
            }
            if !agents.iter().any(|agent| agent == &step.agent) {
                return Err(format!("step {} names an unknown agent", i + 1));
            }
            // Exact spelling avoids ambiguity from case-insensitive provider lookup.
            if !config.providers.contains_key(&step.provider) {
                return Err(format!("step {} names an unconfigured provider", i + 1));
            }
        }
        Ok(())
    }

    /// JSON string encoding preserves the exact content and prevents a prompt
    /// containing Markdown from impersonating another approval field.
    pub fn approval_view(&self, config: &Config, history: &[nexo_core::subs::SubInfo]) -> String {
        let mut out = String::from(
            "Sequential launches. Ordinary permissions still apply. Connectivity, quota and actual charges are unknown. Hooks execute and may add context; non-clean launches may also receive a briefing.\nHistorical costs below are recorded session-local sub-agent totals, not a forecast or spending cap. They aggregate multiple calls but may be incomplete when usage or prices were unavailable; actual charges may differ. History matches the agent profile only: provider, model and task may differ.\n",
        );
        for (i, s) in self.steps.iter().enumerate() {
            out.push_str(&format!(
                "\n    Step {}\n    agent: {}\n    provider: {}\n    model: {}\n    start_clean: {}\n    prompt: {}\n",
                i + 1,
                serde_json::to_string(&s.agent).unwrap(),
                serde_json::to_string(&s.provider).unwrap(),
                serde_json::to_string(&s.model).unwrap(),
                s.start_clean,
                serde_json::to_string(&s.prompt).unwrap()
            ));
            // The registry does not retain provider/model or whether all calls had
            // pricing and usage. A zero cost therefore cannot establish a free run.
            let sample = history
                .iter()
                .filter(|run| {
                    run.profile == s.agent
                        && !run.state.running()
                        && run.ended_ms.is_some()
                        && run.cost_usd.is_finite()
                        && run.cost_usd > 0.0
                })
                .max_by_key(|run| run.ended_ms);
            if let Some(run) = sample {
                out.push_str(&format!(
                    "    latest recorded historical cost: ${:.6} USD (ended unix ms {}; profile match only; may be incomplete)\n",
                    run.cost_usd, run.ended_ms.unwrap()
                ));
            } else {
                out.push_str("    historical cost: unknown (no usable completed run for this profile)\n");
            }
            out.push_str("    planned step cost: unknown\n");
            let cfg = config.providers.get(&s.provider);
            out.push_str(if cfg.is_some_and(|c| c.login.is_some()) {
                "    billing: subscription login configured; actual charges unknown\n"
            } else {
                "    billing: API/key or local endpoint; billing type and actual charges unknown\n"
            });
        }
        out.push_str("\n    Planned total cost: unknown\n");
        out
    }
}

/// Executes only the owned proposal displayed in the confirmation. This tool must
/// not be exposed to a planning turn until direct task/workflow calls are gated.
#[allow(dead_code)]
pub(super) struct ExecutePlan {
    pub sub: std::sync::Arc<crate::subagent::SubAgent>,
    pub guard: std::sync::Arc<tokio::sync::OwnedRwLockWriteGuard<()>>,
    pub config: Config,
    pub asker: Option<std::sync::Arc<crate::ask::ChannelAsker>>,
    pub permissions: crate::ask::PlanCell,
    pub state: std::sync::Weak<tokio::sync::Mutex<super::state::State>>,
    pub used: std::sync::atomic::AtomicBool,
}

#[async_trait::async_trait]
impl nexo_core::Tool for ExecutePlan {
    fn name(&self) -> &str {
        "ultracode_execute"
    }
    fn kind(&self) -> nexo_core::ToolKind {
        nexo_core::ToolKind::Other
    }
    fn spec(&self) -> nexo_core::ToolSpec {
        nexo_core::ToolSpec {
            name: self.name().into(),
            description: "Present a structured sequential agent plan for approval, then execute exactly its steps."
                .into(),
            parameters: serde_json::json!({"type":"object", "additionalProperties":false,
                "properties":{"steps":{"type":"array", "minItems":1, "maxItems":20,
                  "items":{"type":"object", "additionalProperties":false,
                    "properties":{"agent":{"type":"string"},"prompt":{"type":"string"},
                      "provider":{"type":"string"},"model":{"type":"string"},"start_clean":{"type":"boolean"}},
                    "required":["agent","prompt","provider","model","start_clean"]}}},"required":["steps"]}),
        }
    }
    fn handles_cancel(&self) -> bool {
        true
    }
    fn summary(&self, _: &serde_json::Value) -> String {
        "Approve and execute sequential agent plan".into()
    }
    async fn run(&self, args: serde_json::Value, ctx: &nexo_core::ToolContext) -> nexo_core::ToolOutput {
        use nexo_core::{PermissionMode, Question, QuestionOption, ToolOutput};
        use std::sync::atomic::Ordering;
        if self.used.swap(true, Ordering::SeqCst) {
            return ToolOutput::error("plan already submitted");
        }
        let plan: Plan = match serde_json::from_value(args) {
            Ok(p) => p,
            Err(e) => return ToolOutput::error(format!("invalid plan: {e}")),
        };
        let agents: Vec<String> = self.sub.profiles().iter().map(|p| p.name.clone()).collect();
        if let Err(e) = plan.validate(&self.config, &agents) {
            return ToolOutput::error(e);
        }
        let Some(state) = self.state.upgrade() else {
            return ToolOutput::error("session ended");
        };
        let (mode, snapshot) = {
            let st = state.lock().await;
            (st.perm.mode, super::approval::TaskPolicySnapshot::capture(&st))
        };
        if ctx.cancel.is_cancelled() || mode == PermissionMode::ReadOnly {
            return ToolOutput::error("cancelled or read-only mode");
        }
        if mode != PermissionMode::Auto {
            let Some(asker) = &self.asker else {
                return ToolOutput::error("cannot request plan approval without an interface");
            };
            let q = Question {
                id: ctx.call_id.clone(),
                header: "Agent plan".into(),
                question: "Execute this agent plan?".into(),
                options: vec![
                    QuestionOption {
                        label: "Approve agent plan".into(),
                        description: "Execute the displayed steps".into(),
                    },
                    QuestionOption { label: "Reject".into(), description: "Do not execute".into() },
                ],
                multi: false,
                plan: Some(plan.approval_view(&self.config, &ctx.jobs.subs().list())),
            };
            let answer = tokio::select! {
                a = asker.ask(q) => a,
                _ = ctx.cancel.cancelled() => { asker.resolve(&ctx.call_id, None); return ToolOutput::error("cancelled"); }
            };
            if answer.as_deref() != Some("Approve agent plan") {
                return ToolOutput::error("plan not approved");
            }
        }
        let (mode, approved_snapshot) = {
            let mut st = state.lock().await;
            if ctx.cancel.is_cancelled() || snapshot != super::approval::TaskPolicySnapshot::capture(&st) {
                return ToolOutput::error("plan authorization changed while awaiting approval");
            }
            let mode = if mode == PermissionMode::Plan {
                if st.config.check_mode(PermissionMode::Ask).is_err() {
                    return ToolOutput::error("cannot leave plan mode under current configuration");
                }
                super::host::set_mode(&mut st, PermissionMode::Ask);
                PermissionMode::Ask
            } else {
                mode
            };
            (mode, super::approval::TaskPolicySnapshot::capture(&st))
        };
        let mut reports = Vec::new();
        for (i, step) in plan.steps.iter().enumerate() {
            if ctx.cancel.is_cancelled() {
                return ToolOutput::error("cancelled");
            }
            let task_args = serde_json::json!({"agent":step.agent,"prompt":step.prompt,"provider":step.provider,"model":step.model,"start_clean":step.start_clean});
            let permit = match super::approval::permit_internal_task(
                &state,
                &format!("{}-step-{}", ctx.call_id, i + 1),
                &task_args,
                &ctx.cancel,
                &approved_snapshot,
            )
            .await
            {
                Ok(p) => p,
                Err(e) => return ToolOutput::error(format!("step {}: {e}", i + 1)),
            };
            let launch = crate::subagent::Launch {
                agent: &step.agent,
                prompt: &step.prompt,
                provider: Some(&step.provider),
                model: Some(&step.model),
                description: None,
                title: None,
                background: false,
                detachable: false,
                start_clean: step.start_clean,
            };
            let started = match self
                .sub
                .launch_approved(self.guard.clone(), launch, ctx, &self.permissions, mode, permit)
                .await
            {
                Ok(s) => s,
                Err(e) => return ToolOutput::error(format!("step {}: {e}", i + 1)),
            };
            let id = started.id;
            let mut rx = started.rx;
            let ended = tokio::select! {
                biased;
                _ = ctx.cancel.cancelled() => {
                    ctx.jobs.kill(id);
                    if tokio::time::timeout(std::time::Duration::from_secs(5), &mut rx).await.is_err() {
                        return ToolOutput::error(format!("cancelled; job {id} is still stopping"));
                    }
                    return ToolOutput::error("cancelled");
                }
                result = &mut rx => result,
            };
            ctx.jobs.tell(id);
            match ended {
                Ok(crate::subagent::Ended::Report(r)) => reports.push(format!("Step {}: {r}", i + 1)),
                Ok(crate::subagent::Ended::Failed(e, tail)) | Ok(crate::subagent::Ended::Stopped(e, tail)) => {
                    return ToolOutput::error(format!("step {}: {e}{tail}", i + 1));
                }
                Err(_) => return ToolOutput::error(format!("step {} ended without a report", i + 1)),
            }
        }
        ToolOutput::ok(reports.join("\n\n"))
    }
}

pub(super) fn facts(config: &Config, current_provider: &str, current_model: &str, now: i64) -> String {
    render(config, current_provider, current_model, now, usage::get)
}

fn render(
    config: &Config,
    current_provider: &str,
    current_model: &str,
    now: i64,
    cached: impl Fn(&str) -> Option<PlanUsage>,
) -> String {
    let mut out =
        vec!["[Ultracode planning facts — local snapshot, not a connectivity or billing guarantee]".to_string()];
    if config.providers.is_empty() {
        out.push("Configured providers: unknown (none in configuration).".into());
    }
    for (name, cfg) in &config.providers {
        let auth = if cfg.login.is_some() {
            "subscription login configured"
        } else {
            "API/key or local endpoint (billing type unknown)"
        };
        let readiness = if name == current_provider {
            "current session provider; connectivity not verified"
        } else {
            "readiness unknown"
        };
        out.push(format!("Provider {name:?}: {auth}; {readiness}."));
        let mut models = cfg.models.clone();
        if let Some(m) = &cfg.default_model {
            models.push(m.clone());
        }
        if name == current_provider {
            models.push(current_model.to_string());
        }
        models.sort();
        models.dedup();
        if models.is_empty() {
            out.push("  Models: unknown.".into());
        }
        for model in models {
            let l = crate::limits::resolve(name, cfg, &model, None);
            match l.pricing {
                Some(p) => out.push(format!("  Model {model:?}: reference USD/million tokens input {}, output {}, cache read {}, cache write {} (source: {}; cache write 0 uses input rate). Actual subscription charges unknown.", p.input, p.output, p.cache_read, p.cache_write, l.source)),
                None => out.push(format!("  Model {model:?}: price unknown.")),
            }
        }
        match usage::kind_of(cfg).and_then(&cached) {
            Some(plan) => {
                out.push(format!("  Cached subscription quota: observed at unix {}; cache is shared by login kind, account attribution unverified.", plan.seen));
                if plan.meters.is_empty() {
                    out.push("  Quota remaining: unknown (no meters).".into());
                }
                for meter in &plan.meters {
                    if meter.is_stale(now) {
                        out.push(format!("  {:?}: unknown (cached reading has passed reset).", meter.label));
                    } else {
                        let counts = meter
                            .counts
                            .map(|(used, total)| format!(", {} of {} remaining", total.saturating_sub(used), total))
                            .unwrap_or_default();
                        out.push(format!(
                            "  {:?}: {:.1}% used{counts}; reset unix {}.",
                            meter.label,
                            meter.used_pct,
                            meter.resets_at.map(|v| v.to_string()).unwrap_or_else(|| "unknown".into())
                        ));
                    }
                }
            }
            None => out.push("  Subscription quota: unknown (no applicable cached reading).".into()),
        }
    }
    out.push("Estimates require token assumptions; unknown prices, availability, billing and quota must not be invented. Cached readings may be outdated.".into());
    out.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use nexo_core::config::ProviderConfig;
    use nexo_core::pricing::Pricing;
    use nexo_providers::usage::Meter;

    #[test]
    fn approval_fields_are_indented_code_not_markdown() {
        let plan = Plan {
            steps: vec![Step {
                agent: "explore".into(),
                prompt: "`**[forged](https://example.com)**`\n# heading".into(),
                provider: "p".into(),
                model: "m".into(),
                start_clean: true,
            }],
        };
        let view = plan.approval_view(&Config::default(), &[]);
        for line in view.lines().skip(2) {
            assert!(line.is_empty() || line.starts_with("    "), "{line}");
        }
        assert!(view.contains(r#"\n# heading"#));
    }

    #[test]
    fn approval_uses_only_completed_usable_profile_history() {
        use nexo_core::subs::{SubInfo, SubState};
        let plan = Plan {
            steps: vec![Step {
                agent: "explore".into(),
                prompt: "hi".into(),
                provider: "p".into(),
                model: "m".into(),
                start_clean: true,
            }],
        };
        let run = |profile: &str, state, cost_usd, ended_ms| SubInfo {
            id: "id".into(),
            profile: profile.into(),
            provider: None,
            model: None,
            title: "task".into(),
            state,
            started_ms: 1,
            ended_ms,
            tokens_in: 100,
            tokens_out: 20,
            cost_usd,
        };
        let history = vec![
            run("explore", SubState::Ok, 1.25, Some(2)),
            run("explore", SubState::Attached, 9.0, None),
            run("other", SubState::Ok, 8.0, Some(4)),
            run("explore", SubState::Ok, 0.0, Some(5)),
            run("explore", SubState::Failed, f64::NAN, Some(6)),
        ];
        let view = plan.approval_view(&Config::default(), &history);
        assert!(view.contains("historical cost: $1.250000 USD"), "{view}");
        assert!(view.contains("profile match only"));
        assert!(view.contains("multiple calls"));
        assert!(view.contains("may be incomplete"));
        assert!(view.contains("Planned total cost: unknown"));
        assert!(!view.contains("$9.000000"));
        let view = plan.approval_view(&Config::default(), &history[1..]);
        assert!(view.contains("historical cost: unknown"));
        assert!(!view.contains('$'));
        assert!(!view.contains("10,000"));
    }

    #[test]
    fn structured_plan_rejects_unknown_fields_and_invalid_launches() {
        let json = r#"{"steps":[{"agent":"explore","prompt":"hello\nmodel: forged","provider":"p","model":"m","start_clean":false}]}"#;
        let mut plan: Plan = serde_json::from_str(json).unwrap();
        let mut config = Config::default();
        config.providers.clear();
        config.providers.insert("p".into(), ProviderConfig::default());
        let agents = vec!["explore".to_string()];
        assert!(plan.validate(&config, &agents).is_ok());
        assert!(plan.approval_view(&config, &[]).contains(r#"prompt: "hello\nmodel: forged""#));
        plan.steps[0].provider = "other".into();
        assert!(plan.validate(&config, &agents).is_err());
        plan.steps.clear();
        assert!(plan.validate(&config, &agents).is_err());
        assert!(serde_json::from_str::<Plan>(r#"{"steps":[],"ignored":true}"#).is_err());
    }

    #[test]
    fn facts_are_allowlisted_and_mark_stale_and_missing_data() {
        let mut cfg = Config::default();
        cfg.providers.clear();
        let mut p =
            ProviderConfig { login: Some("codex".into()), models: vec!["model-x".into()], ..Default::default() };
        p.headers.insert("Authorization".into(), "secret-header".into());
        p.api_key_cmd = Some("secret-command".into());
        p.pricing.insert("model-x".into(), Pricing { input: 2.0, output: 4.0, ..Default::default() });
        cfg.providers.insert("example".into(), p);
        cfg.providers.insert("other".into(), ProviderConfig::default());
        let plan = PlanUsage {
            seen: 10,
            meters: vec![
                Meter {
                    slot: "a".into(),
                    label: "fresh".into(),
                    used_pct: 25.0,
                    counts: Some((25, 100)),
                    window_minutes: None,
                    resets_at: Some(200),
                },
                Meter {
                    slot: "b".into(),
                    label: "old".into(),
                    used_pct: 90.0,
                    counts: None,
                    window_minutes: None,
                    resets_at: Some(50),
                },
            ],
            ..Default::default()
        };
        let s = render(&cfg, "example", "model-x", 100, |_| Some(plan.clone()));
        assert!(s.contains("75 of 100 remaining"));
        assert!(s.contains("cached reading has passed reset"));
        assert!(s.contains("input 2, output 4"));
        assert!(s.contains("readiness unknown"));
        assert!(s.contains("Models: unknown"));
        assert!(!s.contains("secret-header") && !s.contains("secret-command"));
    }
}
