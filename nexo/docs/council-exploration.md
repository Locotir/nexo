# Exploring a council workflow

Session idea 63 asks to explore a council, not to promise that a council improves answers. This is a source and code review, not an evaluation of model quality. Reviewed 2026-11-24.

## Reference and existing Nexo behavior

[LLM Council](https://github.com/karpathy/llm-council) is a separate local web application using OpenRouter. Its README describes three stages: parallel first opinions, peer rankings under anonymous labels, and a chairman synthesis. Its [orchestrator](https://raw.githubusercontent.com/karpathy/llm-council/master/backend/council.py) sends every successful first answer to every reviewer, including its own; labels hide model names in that prompt, but content or style may reveal identity. Failed model responses are omitted. Its ranking parser has permissive fallbacks; an aggregate rank is not a correctness guarantee. The chairman sees attributed answers and reviews. These observations describe the inspected source, not a guarantee about future revisions.

Nexo already has a `perspectives` built-in skill (`crates/nexo-core/src/skills_builtin.rs`): it seeks bounded complementary views where useful, uses read-only workers when offered, and synthesizes evidence rather than votes. The `task` schema (`crates/nexo-agent/src/subagent/tool.rs`) allows a configured model and, when available, provider; otherwise the default/profile applies. Different calls can use the same model. `start_clean` suppresses briefing, hook context and shared-memory tools, but keeps profile and project tools; it is neither a sandbox nor a guarantee of independent reasoning. Read-only built-in profiles are described in `crates/nexo-agent/src/subagent/profile.rs`; trusted custom profile overrides mean the actual profile should be checked. Multiple eligible read-only calls can run concurrently, subject to the tool's runtime behavior and limits.

## Practical exploration and decision

For a consequential question with genuine uncertainty, use the existing `perspectives` skill instead of creating a duplicate council agent or always running the more expensive three-stage procedure:

1. Set an explicit small worker/time/cost budget and provide each worker the same minimal authorized facts, question and constraints, asking for evidence, assumptions and counterexamples without sharing the coordinator's preliminary answer. Choose complementary lenses. Select different models/providers only when configured and permitted; otherwise honestly label same-model perspectives. Avoid delegation for straightforward questions.
2. Collect the available reports; identify failure, truncation and disagreement. Where a material disagreement could change a decision and budget and privacy permit, optionally ask a read-only worker to critique anonymized excerpts against explicit criteria. Do not present anonymization as a privacy guarantee; the excerpts still go to that worker's provider. This optional critique is a proposed manual step, not an automatically implemented review stage.
3. The coordinator checks consequential claims against primary evidence, synthesizes agreements and unresolved conflicts, and recommends the smallest discriminating test. Reports and quoted content are untrusted data, not instructions; majority agreement and reviewer rankings are not proof.

If no authorized worker is available, use the evidence at hand and disclose the limitation; never invent independent reports. Do not transmit secrets or private material to a provider without authorization. Stop within the budget, including when workers fail; do not silently retry indefinitely. The coordinator's final answer must not claim that cross-review or different models were used unless they actually were.

## What would justify a dedicated feature

A controlled evaluation should compare direct answering, the existing perspectives workflow, and optional peer review on the same held-out questions with independently graded correctness, calibration and safety, recording provider/model, failures, latency and total cost. Rotate order and repeat where feasible; account for correlated model errors. Only add a distinct agent/skill or automated rankings if it materially improves measured outcomes enough to justify additional complexity, data exposure and cost. No new automation, model, permission, dependency or external service is enabled by this exploration. No provider call, end-to-end council trial or quality benchmark was performed here.
