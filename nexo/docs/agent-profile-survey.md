# Public agent/skill survey and Nexo's minimal delta

Scope: session idea 60. This is a source/documentation comparison, not a claim that
more personas improve model quality. Upstream pages are moving targets; their
names, limits and model defaults must not be treated as Nexo configuration.

## Primary sources inspected

| Project | Observed agents/skills | Relevant distinction |
| --- | --- | --- |
| [Claude Code](https://code.claude.com/docs/en/sub-agents) | Explore, Plan, general-purpose; specialist helpers such as statusline-setup and claude-code-guide; custom Markdown agents | Plan gathers codebase evidence read-only. Descriptions add recurring context; keep them short. |
| [Grok Build subagents](https://docs.x.ai/build/features/subagents) | general-purpose, explore, plan; custom types and persona overlays | Explore/plan have neither shell nor edits. A persona is behavior, not a permission grant. |
| [Grok skills/plugins](https://docs.x.ai/build/features/skills-plugins-marketplaces) | SKILL.md folders, plugins, hooks and agents; Claude compatibility | Skill allowed-tools metadata does not itself grant/restrict tools. Do not equate a skill with a sandbox. |
| [Codex](https://learn.chatgpt.com/docs/agent-configuration/subagents.md) | default, worker, explorer; custom TOML agents | Workers inherit parent permissions; a custom prompt asking for read-only behavior is not a separate read-only sandbox. Parallel writes need coordination and consume extra tokens. |
| [OpenCode](https://opencode.ai/docs/agents/) | Build/Plan primary agents; General/Explore subagents; custom reviewer example | Plan is a primary mode, not evidence of an identical subagent contract. Current docs describe explicit edit/shell permission rules and bounded steps. |
| [Pi](https://raw.githubusercontent.com/badlogic/pi-mono/main/packages/coding-agent/README.md) | Minimal core deliberately omits subagents/plan mode; skills/extensions provide workflows | Do not list extension examples as built-ins. |
| [Pi subagent example](https://raw.githubusercontent.com/badlogic/pi-mono/main/packages/coding-agent/examples/extensions/subagent/README.md) | scout, planner, reviewer, worker; scout→planner→worker and worker→reviewer→worker chains | Planner uses read/search tools. Example parallelism is bounded, not unlimited; project agents require trust. |
| [Hermes bundled catalog](https://hermes-agent.nousresearch.com/docs/reference/skills-catalog) | systematic-debugging, TDD, spike, dogfood, grounded-citations, simplify-code; domain skills; CLI delegation to Claude/Codex | Mostly reusable playbooks/integration skills, not a reason to add one built-in agent per skill. Many require external tools, accounts or runtime setup. |

No upstream skill bodies or model defaults were copied. No external software,
plugin, provider, credential or network permission was installed/enabled.

## What Nexo already had

`crates/nexo-agent/src/subagent/profile.rs` already provided **explore, general,
research, review**, not merely general/explore. Research explicitly checks sources
and cites URLs; review has a severity/evidence contract. Read-only profiles use
runtime tool filtering, not only a sentence in a prompt.

`crates/nexo-core/src/skills_builtin.rs` already provided debug, review, verify,
perspectives, research, performance, long-task, pr-ci, execute-plan, doctor,
simplify, skill-creator, documents and visual-qa. Skills are on-demand procedures,
not additional tool permissions. `task` already supports a selected skill.
Custom `.nexo/agents/*.md` profiles and imported Claude profiles already cover
project-specific specialists, with project trust and name overrides.

## Decision and minimal implementation

Add **plan**, a read-only subagent that inspects relevant code and returns a
small implementation plan with locations, dependent steps, tests/acceptance
criteria, risks and unresolved assumptions. This fills the recurring planning
contract visible in Claude/Grok/Pi without duplicating an editing worker. It
inherits the model/provider, has 24 tool rounds, and uses the same filtered tools
as explore/review. It cannot edit, execute a shell or operate a browser. It does
not introduce a global plan mode or prohibit the main agent from editing.

Example task arguments:

```json
{"agent":"plan","prompt":"Plan the smallest fix for the wrapped-link regression. Cite affected paths, dependent changes and acceptance tests; do not implement."}
```

The short task catalog and agent enum expose plan. Trusted custom profiles can
replace it, like existing built-ins. Existing discovery and tool-schema budget
tests remain applicable; no budget ceiling should be raised merely to add a name.
The full plan instructions load only for that worker.

### Other useful roles: compose before adding built-ins

| Candidate | Use now | Why not another built-in |
| --- | --- | --- |
| debugger / test implementer | general + debug or verify | Needs authorized commands/edits; already supported. |
| performance engineer | general + performance | Measurement methodology is a playbook, not a new permission class. |
| security reviewer | review with an explicit security scope, or trusted custom profile | No separate tool contract needed; do not imply that a persona is a security guarantee. |
| documenter / UI QA | general + documents / visual-qa | Actual rendering/browser availability must be verified. |
| scout / worker | explore / general | Existing equivalents; aliases add recurring prompt noise. |
| TDD / experimental spike | focused general task; optionally add a project skill | Useful future playbooks, but not needed to fill the planning gap. Avoid mandatory test-first policy or throwaway edits for every task. |
| external CLI orchestrator | opt-in custom skill after checking installation and authorization | Credentials, costs and permission boundaries differ; do not silently route tasks outside Nexo. |

A council/perspectives workflow is a separate session idea; this change neither
adds unbounded workers nor claims independent reasoning from repeated calls.
No profile changes the existing cancellation, step, context or cost limits.

## Verification

Verified commands (all prefixed with `nice -n 10 env CARGO_NET_OFFLINE=true
CARGO_TARGET_DIR=/home/lctr/build-cache/nexo-ci`, using `--offline --locked -j2`):

- `cargo test --offline --locked -j2 -p nexo-agent --lib subagent::`: **61 passed**.
  Evidence: `/home/lctr/build-cache/agent-profile-survey-verified.log`.
- `cargo test --offline --locked -j2 -p nexo-agent --test subagent_events plan_runtime`:
  **1 passed**. Evidence: `/home/lctr/build-cache/agent-plan-runtime-verified.log`.
- Broader `--test subagent_events`: **21 passed, 1 failed**. The existing request
  golden differs in `main_requests.json`, request 0 `system`, following separate
  main-prompt edits; fixtures were not regenerated by this work. Evidence:
  `/home/lctr/build-cache/agent-profile-events-verified.log`.

Checks cover plan's contract, default model inheritance, project trust and name
override, actual filtered tool parity with explore, discovery enum/short text,
and recurring tool-schema budgets. This is deterministic/mock verification,
not a live-provider quality benchmark; no claim of better generated plans is made.

The earlier run had 59 passes and 2 schema-budget failures; shortening the task
synopsis and plan entry by 37 characters fixed them without raising ceilings.
The previously interrupted final run is now superseded by the 61-pass result above.

The added `plan_runtime_refuses_writes_and_project_escape_but_can_read_and_report`
in `crates/nexo-agent/tests/subagent_events/golden_requests.rs` closes a concrete
coverage gap: a scripted planner emits an unoffered write anyway, then attempts
an outside-project read, reads a project path, and reports. The test checks actual
error tool results, successful read output, unchanged inherited model, plan
instructions on every request, and final registry state. A bounded timeout and
waiting write sentinel catch accidental execution rather than just schema drift.

The project-escape assertion covers standalone workers without a parent host.
Hosted workers consult the parent's permission policy (`subagent/run.rs:953–957`)
instead; this test does not claim hosted reads are always project-confined. Reads
and writes use deterministic test tools: no live model, network, or quality claim.
No new profile, permission, model default, or skill was introduced in this follow-up.
