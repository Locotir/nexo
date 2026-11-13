use super::*;

/// Characters of every built-in tool spec (name, description, parameter schema) plus the tool guidance.
fn tool_text_chars() -> usize {
    let set = nexo_tools::builtin();
    let specs: usize = set
        .specs()
        .iter()
        .map(|s| s.name.len() + s.description.len() + serde_json::to_string(&s.parameters).unwrap().len())
        .sum();
    specs + tool_guidance(&set).len()
}

/// 10135 characters (about 2500 tokens) when this budget was set, after the text said twice was taken out
/// (the budget before it was 12150); it may grow 5% and no more (10650). The `browser` tool then added its
/// 1989 (12124 in all; a request carries only its short text, as it is a rare tool): 12640.
/// The gif_creator actions (start_recording, stop_recording, export_gif, clear_recording: 4 enum values, the
/// `gif` options object and the export path in `value`) then added 209, after trimming them to the bare names: 12850.
/// `read_page` (action, filter, depth, ref_id, max_chars) then added 372: 13222; its `open` action with `profile` and `visible` took 292 more: 13514. Then 306 more for
/// `browsers`, `select_browser` and `switch_browser` (the person's own logged-in browser): 13820; the merged tool text then measured 22 more: 13842.
/// Trimming the browser text took 352 off (the rules for the person's own browser come in the result of
/// `select_browser`, not in the tool text): 13490.
/// A tool description says what is nexo's own (limits, errors, what saves steps), never what a
/// model already knows about shells and search tools, and a parameter gets a description only for what its
/// name and the tool description do not say.
#[test]
fn the_tool_text_stays_within_its_budget() {
    let n = tool_text_chars();
    // the `bash` description names the shell it found: on Windows that phrase is up to 43 characters longer than `sh -c`
    let budget = if cfg!(windows) { 13_540 } else { 13_490 };
    assert!(n <= budget, "tool descriptions and guidance take {n} characters; the budget is {budget}");
}

/// nexo writes parts of its own into the person's messages (ended jobs, the project note, hook output, files
/// it attached, facts and lines it brought back, the supervisor's mark). Bare, a model answers them as if the
/// person had just said them; the full prompt names each one as nexo's and the compact one names the kinds.
#[test]
fn both_prompts_name_every_part_nexo_adds_to_the_persons_messages() {
    let attached = crate::reads::ATTACHED.trim_end_matches(':');
    let marks = [
        "[background jobs ended]",
        "[project at the start of this request]",
        nexo_core::agent::SUPERVISOR_RESUME_MARK,
        nexo_core::agent::HOOK_CONTEXT_START,
        attached,
        "`Standing:`",
        "`Recall`",
    ];
    let line = |text: &'static str| {
        text.lines().find(|l| l.contains("the person did not type them")).unwrap_or_else(|| panic!("{text}"))
    };
    let full = line(BUILT_IN);
    for m in marks {
        assert!(full.contains(m), "{m} is not named in: {full}");
    }
    assert!(full.contains("do not reply to them as if the person had just said them"), "{full}");
    // the compact text has 2 600 characters in all: it names the kinds, not each label
    // and both say where the person's own words are
    assert!(
        full.contains(nexo_core::agent::PERSON_MARK) && line(BUILT_IN_COMPACT).contains(nexo_core::agent::PERSON_MARK)
    );
    let compact = line(BUILT_IN_COMPACT);
    for m in ["`[label]`", nexo_core::agent::HOOK_CONTEXT_START, "`@path (attached", "`Standing:`", "`Recall`"] {
        assert!(compact.contains(m), "{m} is not named in: {compact}");
    }
    assert!(compact.contains("do not reply to them"), "{compact}");
}

#[test]
fn both_prompts_say_to_answer_at_once_when_the_proving_check_passes() {
    for text in [BUILT_IN, BUILT_IN_COMPACT] {
        assert!(text.contains("run the one check that proves it; when it passes, answer at once"), "{text}");
    }
}

#[test]
fn the_guidance_names_what_saves_steps() {
    let g = tool_guidance(&nexo_tools::builtin());
    assert!(g.contains("run at the same time") && g.contains("many files"), "{g}");
}

#[test]
fn the_built_in_part_has_neither_the_machine_nor_the_files() {
    let tools = nexo_tools::builtin();
    let b = builtin_part(&tools, Some("Directive text."));
    assert!(b.starts_with(built_in()) && b.contains("You can do:") && b.ends_with("Directive text."), "{b}");
    for not in ["Working directory", "Date:", "Instructions from", "The person's preferences", "System:"] {
        assert!(!b.contains(not), "{not}: {b}");
    }
    assert_eq!(builtin_part(&ToolSet::new(), None), built_in());
}

#[test]
fn tool_definitions_show_name_description_and_parameters_of_each_tool() {
    let tools = nexo_tools::builtin();
    let specs = tools.specs();
    let text = tool_definitions(&specs);
    assert_eq!(text.matches("\n=== ").count() + 1, specs.len());
    for s in &specs {
        assert!(
            text.contains(&format!("=== {} ===\ndescription:\n{}\nparameters:\n{{", s.name, s.description)),
            "{}",
            s.name
        );
    }
    assert!(text.contains("\"type\": \"object\""), "{text}");
    assert_eq!(tool_definitions(&[]), "");
}

/// The compact specs of the built-in tools, as `nexo prompt --tools --window N` prints them.
fn compact_tools_text() -> String {
    tool_definitions_for(&nexo_tools::builtin().compact_specs(), Some(8_000))
}

#[test]
fn a_large_window_gets_the_full_text_unchanged() {
    let tools = nexo_tools::builtin();
    for w in [None, Some(200_000), Some(COMPACT_WINDOW + 1)] {
        assert_eq!(builtin_part_for(&tools, Some("D."), w, false), builtin_part(&tools, Some("D.")), "{w:?}");
        assert_eq!(tool_definitions_for(&tools.specs(), w), tool_definitions(&tools.specs()), "{w:?}");
    }
    assert!(!is_compact(None) && !is_compact(Some(COMPACT_WINDOW + 1)) && is_compact(Some(COMPACT_WINDOW)));
    let d = tempfile::tempdir().unwrap();
    let cfg = AgentConfig { hermetic: true, ..Default::default() };
    let full = system_prompt(d.path(), &cfg, &tools);
    assert_eq!(system_prompt_for(d.path(), &cfg, &tools, Some(200_000)), full);
    assert!(full.contains("How to write code:"));
}

#[test]
fn a_small_window_gets_the_compact_text_within_its_caps() {
    let tools = nexo_tools::builtin();
    for w in [8_000, COMPACT_WINDOW] {
        let b = builtin_part_for(&tools, None, Some(w), false);
        assert!(b.len() <= 2_600, "{w}: the compact built-in part takes {} characters", b.len());
        assert!(!b.contains("How to write code:") && !b.contains("You can do:"), "{b}");
        // the tools this window is not offered (`browser` among them) are not in its request
        let offered: Vec<_> = tools.compact_specs().into_iter().filter(|s| !left_out(Some(w), &s.name)).collect();
        let t = tool_definitions_for(&offered, Some(w));
        assert!(t.len() <= 6_000, "{w}: the compact tools take {} characters", t.len());
        let json = serde_json::to_string(&offered).unwrap();
        assert!(json.len() <= 6_000, "{w}: the compact tool JSON takes {} characters", json.len());
    }
    assert!(BUILT_IN_COMPACT.len() <= 2_400, "{}", BUILT_IN_COMPACT.len());
    assert!(compact_guidance(&tools).lines().filter(|l| l.starts_with("- ")).count() <= 2);
}

#[test]
fn the_compact_prompt_keeps_the_safety_rules() {
    let b = builtin_part_for(&nexo_tools::builtin(), None, Some(8_000), false).to_lowercase();
    for want in ["secrets", "destructive", "only the person", "explicit request", "information"] {
        assert!(b.contains(want), "missing {want}: {b}");
    }
    assert!(b.starts_with("you are nexo,"), "{b}");
}

#[test]
fn a_compact_tool_keeps_its_parameter_names_types_and_required_list() {
    let tools = nexo_tools::builtin();
    let (full, compact) = (tools.specs(), tools.compact_specs());
    assert_eq!(full.len(), compact.len());
    fn shape(v: &serde_json::Value, out: &mut Vec<String>, at: &str) {
        let Some(o) = v.as_object() else { return };
        if let Some(t) = o.get("type").and_then(|t| t.as_str()) {
            out.push(format!("{at}:{t}"));
        }
        out.push(format!("{at}:required={}", o.get("required").map_or("-".into(), |r| r.to_string())));
        if let Some(p) = o.get("properties").and_then(|p| p.as_object()) {
            for (k, v) in p {
                shape(v, out, &format!("{at}.{k}"));
            }
        }
        if let Some(i) = o.get("items") {
            shape(i, out, &format!("{at}[]"));
        }
    }
    for (f, c) in full.iter().zip(&compact) {
        assert_eq!(f.name, c.name);
        let (mut a, mut b) = (Vec::new(), Vec::new());
        shape(&f.parameters, &mut a, "");
        shape(&c.parameters, &mut b, "");
        assert_eq!(a, b, "{}", f.name);
        assert!(c.description.len() < f.description.len() && c.description.len() <= 200, "{}", f.name);
    }
    assert!(compact_tools_text().contains("=== bash ==="));
}
