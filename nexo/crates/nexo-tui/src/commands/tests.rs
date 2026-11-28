use super::rank::distance;
use super::table::HIDDEN;
use super::{
    closed_args, closest, fixed_args, grouped, lookup, names, parse, rank, takes_listed_args, Command, GoalArg,
    COMMANDS,
};
use nexo_core::permission::PermissionMode;

#[test]
fn every_hidden_alias_still_takes_the_arguments_it_took_before_the_fold() {
    // before the fold these ignored whatever followed them
    let ignoring = ["yolo", "mouse", "timestamps", "vim", "context-live", "live", "map", "tree", "login", "signin"];
    for (old, parent, _) in HIDDEN {
        let bare = parse(&format!("/{old}")).unwrap();
        assert!(!matches!(bare, Command::BadArgument(..) | Command::Unknown(_)), "/{old}: {bare:?}");
        assert!(lookup(old).is_some_and(|s| s.name == *parent), "/{old} does not resolve to /{parent}");
        let args: &[&str] = match (ignoring.contains(old), *parent) {
            (true, _) => &["on", "off", "toggle", "whatever x"],
            (_, "permissions") => &["auto", "read-only"],
            _ => &["s1", "x y"],
        };
        for a in args {
            let got = parse(&format!("/{old} {a}")).unwrap();
            assert!(!matches!(got, Command::BadArgument(..) | Command::Unknown(_)), "/{old} {a}: {got:?}");
            if ignoring.contains(old) {
                assert_eq!(got, bare, "/{old} {a}");
            }
        }
    }
    for key in ["mouse", "timestamps", "vim"] {
        let bare = parse(&format!("/settings {key}")).unwrap();
        for v in ["on", "off"] {
            assert_eq!(parse(&format!("/settings {key} {v}")), Some(bare.clone()), "/settings {key} {v}");
        }
    }
    assert_eq!(parse("/settings mouse off"), Some(Command::Mouse));
    assert_eq!(parse("/yolo off"), Some(Command::Permissions(Some(PermissionMode::Auto))));
    assert_eq!(parse("/settings nonsense on"), Some(Command::BadArgument("settings".into(), "nonsense on".into())));
}

#[test]
fn a_goal_word_of_three_letters_is_the_subcommand_it_starts() {
    let goal = |t: &str| parse(&format!("/goal {t}"));
    assert_eq!(goal("res"), Some(Command::Goal(GoalArg::Resume)));
    assert_eq!(goal("resum"), Some(Command::Goal(GoalArg::Resume)));
    assert_eq!(goal("pau"), Some(Command::Goal(GoalArg::Pause)));
    assert_eq!(goal("cle"), Some(Command::Goal(GoalArg::Clear)));
    assert_eq!(goal("don"), Some(Command::Goal(GoalArg::Clear)));
    assert_eq!(goal("non"), Some(Command::Goal(GoalArg::Clear)));
    // too short, not a prefix, or more than one word: a goal
    assert_eq!(goal("re"), Some(Command::Goal(GoalArg::Set("re".into()))));
    assert_eq!(goal("fix"), Some(Command::Goal(GoalArg::Set("fix".into()))));
    assert_eq!(goal("resumes"), Some(Command::Goal(GoalArg::Set("resumes".into()))));
    assert_eq!(goal("res the parser"), Some(Command::Goal(GoalArg::Set("res the parser".into()))));
    assert_eq!(goal("pau now"), Some(Command::Goal(GoalArg::Set("pau now".into()))));
}

#[test]
fn goal_words_in_spanish_and_english_pause_resume_or_clear() {
    let goal = |t: &str| parse(&format!("/goal {t}"));
    for w in ["stop", "Stop", "PAUSA", "para", "¡para!", "parar", "cancel", "cancelar", "detener", "fin"] {
        assert_eq!(goal(w), Some(Command::Goal(GoalArg::Pause)), "{w}");
    }
    for w in ["¿continuar?", "continuar", "sigue", "reanudar", "Resume"] {
        assert_eq!(goal(w), Some(Command::Goal(GoalArg::Resume)), "{w}");
    }
    for w in ["borrar", "quitar"] {
        assert_eq!(goal(w), Some(Command::Goal(GoalArg::Clear)), "{w}");
    }
}

#[test]
fn a_short_goal_word_that_names_one_action_is_that_action() {
    let goal = |t: &str| parse(&format!("/goal {t}"));
    assert_eq!(goal("con"), Some(Command::Goal(GoalArg::Resume)));
    assert_eq!(goal("par"), Some(Command::Goal(GoalArg::Pause)));
    assert_eq!(goal("can"), Some(Command::Goal(GoalArg::Pause)));
}

#[test]
fn a_sentence_starting_with_a_goal_word_is_a_goal() {
    let goal = |t: &str| parse(&format!("/goal {t}"));
    for t in ["para el servidor y escribe el informe", "stop the cron jobs"] {
        assert_eq!(goal(t), Some(Command::Goal(GoalArg::Set(t.into()))), "{t}");
    }
}

#[test]
fn parses_names_aliases_and_arguments() {
    assert_eq!(parse("/model"), Some(Command::Model(None)));
    assert_eq!(parse("/model deepseek/deepseek-chat "), Some(Command::Model(Some("deepseek/deepseek-chat".into()))));
    assert_eq!(parse("/m x"), Some(Command::Model(Some("x".into()))));
    assert_eq!(parse("/fast"), Some(Command::Fast(None)));
    assert_eq!(parse("/fast on"), Some(Command::Fast(Some("on".into()))));
    assert_eq!(parse("/fast"), Some(Command::Fast(None)));
    assert_eq!(parse("/fast on"), Some(Command::Fast(Some("on".into()))));
    assert_eq!(parse("/permissions auto"), Some(Command::Permissions(Some(PermissionMode::Auto))));
    assert_eq!(parse("/mode auto"), Some(Command::Permissions(Some(PermissionMode::Auto))));
    assert_eq!(parse("/permissions sideways"), Some(Command::BadArgument("permissions".into(), "sideways".into())));
    assert_eq!(parse("/yolo"), Some(Command::Permissions(Some(PermissionMode::Auto))));
    assert_eq!(parse("/sandbox"), Some(Command::Sandbox(None)));
    assert_eq!(parse("/sandbox read-only"), Some(Command::Sandbox(Some(nexo_core::sandbox::SandboxMode::ReadOnly))));
    assert_eq!(parse("/sandbox sideways"), Some(Command::BadArgument("sandbox".into(), "sideways".into())));
    assert_eq!(parse("/system"), Some(Command::Prompt));
    assert_eq!(parse("/explore status 4872"), Some(Command::Explore(Some("status 4872".into()))));
    assert_eq!(parse("/sent"), Some(Command::Sent));
    assert_eq!(parse("/request"), Some(Command::Sent));
    assert_eq!(parse("/prompt edit"), Some(Command::PromptEdit));
    assert_eq!(parse("/pricing in=1 out=2"), Some(Command::Pricing(Some("in=1 out=2".into()))));
    assert_eq!(parse("/prices"), Some(Command::Pricing(None)));
    assert_eq!(parse("/mouse"), Some(Command::Mouse));
    assert_eq!(parse("/timestamps"), Some(Command::Timestamps));
    assert_eq!(parse("/goal"), Some(Command::Goal(GoalArg::Show)));
    assert_eq!(parse("/goal make the tests pass"), Some(Command::Goal(GoalArg::Set("make the tests pass".into()))));
    assert_eq!(parse("/goal off"), Some(Command::Goal(GoalArg::Clear)));
    assert_eq!(parse("/goal pause"), Some(Command::Goal(GoalArg::Pause)));
    assert_eq!(parse("/goal resume"), Some(Command::Goal(GoalArg::Resume)));
    assert_eq!(parse("/goal pause the cron jobs"), Some(Command::Goal(GoalArg::Set("pause the cron jobs".into()))));
    assert_eq!(parse("/plan"), Some(Command::Plan(None)));
    assert_eq!(parse("/planning add a cache"), Some(Command::Plan(Some("add a cache".into()))));
    assert_eq!(parse("/g x"), Some(Command::Goal(GoalArg::Set("x".into()))));
    assert_eq!(parse("/q"), Some(Command::Quit));
    assert_eq!(parse("/exit"), Some(Command::Quit));
    assert_eq!(parse("/resume"), Some(Command::Sessions));
    assert_eq!(parse("/sessions"), Some(Command::Sessions));
    assert_eq!(parse("/resume parser fix"), Some(Command::Resume("parser fix".into())));
    assert_eq!(parse("/undelete"), Some(Command::Undelete(None)));
    assert_eq!(parse("/mem s1 the parser"), Some(Command::Mem(Some("s1 the parser".into()))));
    assert_eq!(parse("/restore 2026-1"), Some(Command::Undelete(Some("2026-1".into()))));
    assert_eq!(parse("/export notes.md"), Some(Command::Export(Some("notes.md".into()))));
    assert_eq!(parse("/save"), Some(Command::Export(None)));
    assert_eq!(parse("/rename parser de pesos"), Some(Command::Rename(Some("parser de pesos".into()))));
    assert_eq!(parse("/add-dir ../lib"), Some(Command::AddDir(Some("../lib".into()))));
    assert_eq!(parse("/add-dir"), Some(Command::AddDir(None)));
    assert_eq!(parse("/memory"), Some(Command::Memory { user: false }));
    assert_eq!(parse("/memory user"), Some(Command::Memory { user: true }));
    assert_eq!(parse("/remember use tabs"), Some(Command::Remember(Some("use tabs".into()))));
    assert_eq!(parse("/note run the tests"), Some(Command::Remember(Some("run the tests".into()))));
    assert_eq!(parse("/logout"), Some(Command::Logout(None)));
    assert_eq!(parse("/signout openai"), Some(Command::Logout(Some("openai".into()))));
    assert!(matches!(parse("/approvals auto"), Some(Command::Permissions(_))), "the Codex name");
    assert_eq!(parse("/memory all"), Some(Command::BadArgument("memory".into(), "all".into())));
    assert_eq!(parse("/title"), Some(Command::Rename(None)));
    assert_eq!(parse("/diff"), Some(Command::Diff));
    assert_eq!(parse("/back"), Some(Command::Rewind));
    assert_eq!(parse("/fork"), Some(Command::Fork));
    assert_eq!(parse("/tree"), Some(Command::Tree));
    assert_eq!(parse("/ctx"), Some(Command::Context));
    assert_eq!(parse("/context-live"), Some(Command::ContextLive));
    assert_eq!(parse("/live"), Some(Command::ContextLive));
    assert_eq!(parse("/usage"), Some(Command::Cost));
    assert_eq!(parse("/new"), Some(Command::Clear));
    assert_eq!(parse("/?"), Some(Command::Help));
    assert_eq!(parse("/nope"), Some(Command::Unknown("nope".into())));
    assert_eq!(parse("/login"), Some(Command::Login));
    assert_eq!(parse("/settings"), Some(Command::Settings));
    assert_eq!(parse("/doctor"), Some(Command::Doctor));
    assert_eq!(parse("/models"), Some(Command::Model(None)));
    assert_eq!(parse("hello"), None);
    assert!(names().contains(&"compact".to_string()));
    assert!(names().contains(&"resume".to_string()) && !names().contains(&"sessions".to_string()));
}

#[test]
fn the_old_names_of_repeated_commands_still_work_without_a_line_of_their_own() {
    assert_eq!(parse("/provider"), Some(Command::Model(None)));
    assert_eq!(parse("/providers"), Some(Command::Model(None)));
    assert_eq!(parse("/keys"), Some(Command::Help));
    assert_eq!(parse("/keybindings"), Some(Command::Help));
    assert_eq!(parse("/config"), Some(Command::Status));
    assert_eq!(parse("/yolo"), Some(Command::Permissions(Some(PermissionMode::Auto))));
    for (old, new) in [
        ("/login", Command::Login),
        ("/signin", Command::Login),
        ("/mouse", Command::Mouse),
        ("/timestamps", Command::Timestamps),
        ("/vim", Command::Vim),
        ("/settings", Command::Settings),
        ("/settings mouse", Command::Mouse),
        ("/context live", Command::ContextLive),
        ("/context-live", Command::ContextLive),
        ("/live", Command::ContextLive),
        ("/map", Command::ContextLive),
        ("/context", Command::Context),
        ("/tree", Command::Tree),
        ("/resume tree", Command::Tree),
        ("/undelete", Command::Undelete(None)),
        ("/undelete s1", Command::Undelete(Some("s1".into()))),
        ("/restore s1", Command::Undelete(Some("s1".into()))),
        ("/resume undelete s1", Command::Undelete(Some("s1".into()))),
        ("/shared-memory", Command::SharedMemory(None)),
        ("/shared-memory read 1", Command::SharedMemory(Some("read 1".into()))),
        ("/shared-memory s1 the tabs", Command::SharedMemory(Some("s1 the tabs".into()))),
        ("/shared-memory list", Command::SharedMemory(Some("list".into()))),
        ("/share s1 the tabs", Command::Mem(Some("s1 the tabs".into()))),
        ("/memory-shared off", Command::Mem(Some("off".into()))),
        ("/memory share off", Command::Mem(Some("off".into()))),
        ("/note", Command::Remember(None)),
        ("/note use tabs", Command::Remember(Some("use tabs".into()))),
        ("/remember use tabs", Command::Remember(Some("use tabs".into()))),
        ("/memory add use tabs", Command::Remember(Some("use tabs".into()))),
        ("/resume s1", Command::Resume("s1".into())),
    ] {
        assert_eq!(parse(old), Some(new), "{old}");
    }
    assert_eq!(parse("/settings nonsense"), Some(Command::BadArgument("settings".into(), "nonsense".into())));
    let shown = names();
    for old in [
        "login",
        "signin",
        "mouse",
        "timestamps",
        "vim",
        "live",
        "map",
        "tree",
        "undelete",
        "restore",
        "mem",
        "memory-shared",
        "share",
        "note",
        "remember",
    ] {
        assert!(!shown.contains(&old.to_string()), "/{old} still has its own line");
        assert!(COMMANDS.iter().all(|s| !s.aliases.contains(&old)), "/{old} is still a listed alias");
        assert!(lookup(old).is_some(), "/{old} no longer resolves");
        assert!(COMMANDS.iter().all(|s| s.name != old), "/{old} is a menu command");
    }
    assert_eq!(COMMANDS.len(), 53, "the menu has grown or shrunk: update this on purpose");
    for old in ["provider", "keys", "config", "yolo"] {
        assert!(!shown.contains(&old.to_string()), "/{old} still has its own line");
    }
}

fn first(q: &str) -> (&'static str, Option<&'static str>) {
    let r = rank(q);
    (COMMANDS[r[0].index].name, r[0].via_alias)
}

#[test]
fn ranks_primary_names_before_aliases_before_fuzzy_matches() {
    assert_eq!(first("ses"), ("resume", Some("sessions")), "an alias prefix finds its command");
    assert_eq!(first("re"), ("resume", None), "resume before rewind, by table order");
    assert_eq!(rank("re").iter().map(|r| COMMANDS[r.index].name).take(2).collect::<Vec<_>>(), ["resume", "rewind"]);
    assert_eq!(first("mo"), ("model", None));
    assert_eq!(first("mode"), ("model", None), "a primary-name prefix wins even over an exact alias");
    assert_eq!(first("sys"), ("prompt", Some("system")), "an alias prefix beats a fuzzy name");
    assert_eq!(first("exi"), ("quit", Some("exit")));
    assert_eq!(first("q"), ("quit", None), "the name's own prefix, not the alias");
    assert_eq!(first("cmpct"), ("compact", None), "a fuzzy name still matches");
    assert!(rank("zzz").is_empty());
    let by_desc: Vec<_> = rank("search session").iter().map(|r| COMMANDS[r.index].name).collect();
    assert!(by_desc.contains(&"explore"), "every word found in the description: {by_desc:?}");
    let tail = rank("curator");
    assert!(
        tail.iter().any(|r| COMMANDS[r.index].name == "explore" && r.hits.is_empty()),
        "a description match lights no letters"
    );
    assert_eq!(rank("").len(), COMMANDS.len());
    assert_eq!(rank("cmpct")[0].hits, vec![0, 2, 3, 5, 6], "the letters it matched");
    assert_eq!(rank("ses")[0].hits, vec![0, 1, 2], "in the alias it matched through");
    let names: Vec<_> = rank("c").iter().map(|r| COMMANDS[r.index].name).collect();
    assert!(
        names.starts_with(&["compact", "clear"]) && ["cost", "context", "copy"].iter().all(|n| names.contains(n)),
        "{names:?}"
    );
}

#[test]
fn context_live_is_discoverable_without_knowing_its_name() {
    assert!(names().iter().any(|n| n == "context-live"));
    let s = COMMANDS.iter().find(|s| s.name == "context-live").unwrap();
    assert!(s.description.contains("not payload"));
    assert!(s.description.contains("/context"));
    assert_eq!(first("context-l"), ("context-live", None));
    assert_eq!(parse("/context-live"), Some(Command::ContextLive));
    assert_eq!(parse("/context live"), Some(Command::ContextLive));
}

#[test]
fn chrome_opens_the_browser_window_and_is_listed() {
    assert_eq!(parse("/chrome"), Some(Command::Chrome));
    assert_eq!(parse("/browser"), Some(Command::Chrome));
    let s = lookup("chrome").expect("a listed command");
    assert!(!s.description.is_empty(), "the menu and /help show its line");
}

#[test]
fn team_opens_the_lanes_window_and_is_listed() {
    assert_eq!(parse("/team"), Some(Command::Team));
    assert_eq!(parse("/lanes"), Some(Command::Team));
    let s = lookup("team").expect("a listed command");
    assert!(!s.description.is_empty(), "the menu and /help show its line");
}

#[test]
fn every_command_sits_in_one_group() {
    let g = grouped();
    assert_eq!(g.len(), COMMANDS.len());
    assert!(g.iter().all(|(name, _)| *name != "more"), "a command no group lists: {g:?}");
    let mut slots: Vec<u8> = COMMANDS.iter().map(|s| s.slot).collect();
    slots.sort_unstable();
    assert_eq!(slots, (0..COMMANDS.len() as u8).collect::<Vec<_>>(), "every command has its own palette slot");
    assert!(COMMANDS.iter().all(|s| !s.group.is_empty()), "a command has no group");
}

#[test]
fn a_typo_finds_the_command_it_meant() {
    assert_eq!(closest("modle"), Some("model"));
    assert_eq!(closest("exti"), Some("quit"));
    assert_eq!(closest("comapct"), Some("compact"));
    assert_eq!(closest("zzzzzzzz"), None);
    assert_eq!(distance("kitten", "sitting"), 3);
    assert_eq!(distance("modle", "model"), 1);
}

#[test]
fn knows_which_arguments_it_can_list() {
    for name in ["permissions", "prompt", "copy", "model", "effort", "theme", "goal"] {
        assert!(takes_listed_args(name), "{name}");
    }
    assert!(takes_listed_args("rename") && takes_listed_args("export") && !takes_listed_args("compact"));
    for (v, _) in fixed_args("permissions") {
        assert!(PermissionMode::parse(v).is_some(), "{v}");
    }
    assert!(closed_args("permissions") && !closed_args("goal") && !closed_args("model"));
}

#[test]
fn the_palette_lists_every_command_in_this_exact_order_under_these_headings() {
    let rows: Vec<String> = grouped().iter().map(|(g, i)| format!("{g}/{}", COMMANDS[*i].name)).collect();
    let want = "\
common/model common/permissions common/resume common/plan common/ultracode common/compact common/clear common/help common/effort \
session/fork session/rewind session/undo session/rename session/msg session/export session/goal session/queue session/loop session/explore \
model/logout model/fast model/pricing model/prompt model/memory model/budget \
context and tools/context context and tools/context-live context and tools/sent context and tools/cost context and tools/tools context and tools/hooks \
context and tools/mcp context and tools/chrome context and tools/skills context and tools/plugins context and tools/jobs \
context and tools/panel context and tools/team context and tools/todo context and tools/diff context and tools/shared-memory context and tools/init \
context and tools/review context and tools/commit context and tools/pr context and tools/status \
settings/settings settings/sandbox settings/add-dir settings/theme \
nexo/doctor nexo/copy nexo/quit";
    // Headings hold spaces, so compare the joined text rather than splitting it.
    assert_eq!(rows.join(" "), want);
}

#[test]
fn every_description_fits_a_narrow_palette_row_whole() {
    // at 80 columns a row keeps about 42 cells for what a command does; a longer text is cut mid-word
    let long: Vec<String> = COMMANDS
        .iter()
        .filter(|s| s.description.chars().count() > 42)
        .map(|s| format!("/{}: {}", s.name, s.description))
        .collect();
    assert!(long.is_empty(), "too long for a narrow row:\n{}", long.join("\n"));
}

#[test]
fn loop_commands_are_recognized() {
    assert_eq!(parse("/loop 5m check status"), Some(Command::Loop(Some("5m check status".into()))));
    assert_eq!(parse("/loop list"), Some(Command::Loop(Some("list".into()))));
    assert_eq!(parse("/loop cancel 2"), Some(Command::Loop(Some("cancel 2".into()))));
}
