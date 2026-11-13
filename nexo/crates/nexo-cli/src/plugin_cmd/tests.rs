use super::*;

fn w(p: PathBuf, text: &str) {
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, text).unwrap();
}

/// A plugin folder with a bit of everything.
fn sample(root: &Path, manifest_dir: &str, version: &str) {
    w(
        root.join(manifest_dir).join("plugin.json"),
        &format!(r#"{{"name":"acme","version":"{version}","description":"Acme tools","author":"Acme"}}"#),
    );
    w(root.join("commands/ship.md"), "---\ndescription: Ship it\n---\nShip $ARGUMENTS");
    w(root.join("agents/reviewer.md"), "---\ndescription: reviews\n---\nReview.");
    w(root.join("skills/lint/SKILL.md"), "---\nname: lint\ndescription: Lint things\n---\nRun the linter.");
    w(
        root.join("hooks/hooks.json"),
        r#"{"hooks":{"PreToolUse":[{"matcher":"Bash","hooks":[{"type":"command","command":"${CLAUDE_PLUGIN_ROOT}/check.sh"}]}]}}"#,
    );
    w(
        root.join(".mcp.json"),
        r#"{"mcpServers":{"db":{"command":"${NEXO_PLUGIN_ROOT}/bin/db","args":["${CLAUDE_PLUGIN_ROOT}"]}}}"#,
    );
}

fn git_ok(dir: &Path, args: &[&str]) {
    let st = Command::new("git")
        .args([
            "-c",
            "user.email=t@t",
            "-c",
            "user.name=t",
            "-c",
            "commit.gpgsign=false",
            "-c",
            "core.hooksPath=/dev/null",
        ])
        .args(args)
        .current_dir(dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .stdout(Stdio::null())
        .status()
        .unwrap();
    assert!(st.success(), "git {args:?}");
}

/// A work repo with the sample plugin and a bare repo it was pushed to, the "remote".
fn remote(base: &Path, manifest_dir: &str) -> (PathBuf, PathBuf) {
    let work = base.join("work");
    let bare = base.join("remote.git");
    std::fs::create_dir_all(&work).unwrap();
    sample(&work, manifest_dir, "1.0.0");
    git_ok(&work, &["init", "-q", "-b", "main"]);
    git_ok(&work, &["add", "-A"]);
    git_ok(&work, &["commit", "-q", "-m", "first"]);
    git_ok(base, &["clone", "-q", "--bare", work.to_str().unwrap(), bare.to_str().unwrap()]);
    git_ok(&work, &["remote", "add", "origin", bare.to_str().unwrap()]);
    (work, bare)
}

fn yes(shown: &mut Vec<String>) -> impl FnMut(&str) -> anyhow::Result<bool> + '_ {
    move |s| {
        shown.push(s.to_string());
        Ok(true)
    }
}

#[test]
fn install_from_a_folder_shows_what_it_adds_and_loads_namespaced() {
    let (cfg, src, cwd) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    sample(src.path(), ".claude-plugin", "1.0.0");
    let mut shown = Vec::new();
    let done = install(cfg.path(), cwd.path(), src.path().to_str().unwrap(), &mut yes(&mut shown)).unwrap().unwrap();
    assert_eq!((done.name.as_str(), done.commit.as_str()), ("acme", "local"));
    let s = &shown[0];
    let root = plugins::plugins_dir(cfg.path()).join("acme").display().to_string();
    for want in
        ["/acme:ship", "acme:reviewer", "/acme:lint", "check.sh", "bin/db", "Hooks and MCP servers run programs"]
    {
        assert!(s.contains(want), "{want} missing in:\n{s}");
    }
    assert!(s.contains(&format!("{root}/check.sh")), "paths shown are the final ones:\n{s}");
    assert!(!s.contains(".staging"), "{s}");
    // loaded by the existing loaders, namespaced
    let none = |_: &str| false;
    let trust = cfg.path().join("trusted");
    let cmds = nexo_core::compat::commands(cwd.path(), cfg.path(), &trust, None, &none);
    assert_eq!(cmds.iter().map(|c| c.name.as_str()).collect::<Vec<_>>(), ["acme:ship"]);
    let skills = nexo_core::skills::discover_in(cwd.path(), cwd.path(), cfg.path(), true);
    assert_eq!(skills.iter().map(|s| s.name.as_str()).collect::<Vec<_>>(), ["acme:lint"]);
    let (list, _) = plugins::enabled(cfg.path());
    assert_eq!(list[0].agent_dirs().len(), 1);
    // a second install of the same name says so
    let again = install(cfg.path(), cwd.path(), src.path().to_str().unwrap(), &mut |_| Ok(true));
    assert!(again.unwrap_err().to_string().contains("already installed"));
}

#[test]
fn install_from_a_git_remote_pins_the_commit_and_update_follows_it() {
    let (cfg, base, cwd) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let (work, bare) = remote(base.path(), ".nexo-plugin");
    let url = format!("file://{}", bare.display());
    let first = head_of(&work).unwrap();
    let done = install(cfg.path(), cwd.path(), &url, &mut |_| Ok(true)).unwrap().unwrap();
    assert_eq!(done.commit, first);
    let reg = Registry::load(cfg.path());
    assert_eq!((reg.plugins["acme"].commit.as_str(), reg.plugins["acme"].version.as_str()), (first.as_str(), "1.0.0"));
    // nothing new: up to date, and nothing is asked
    let mut asked = 0;
    update_one(cfg.path(), cwd.path(), "acme", &mut |_| {
        asked += 1;
        Ok(true)
    })
    .unwrap();
    assert_eq!(asked, 0);
    // a new commit upstream: update asks again and moves the pin
    sample(&work, ".nexo-plugin", "1.1.0");
    git_ok(&work, &["commit", "-qam", "second"]);
    git_ok(&work, &["push", "-q", "origin", "main"]);
    let second = head_of(&work).unwrap();
    let said = update_one(cfg.path(), cwd.path(), "acme", &mut |s| {
        assert!(s.contains("changed"), "{s}");
        Ok(true)
    })
    .unwrap();
    assert!(said.unwrap().starts_with("updated"));
    let reg = Registry::load(cfg.path());
    assert_eq!((reg.plugins["acme"].commit.as_str(), reg.plugins["acme"].version.as_str()), (second.as_str(), "1.1.0"));
    // declining an update keeps the old files
    sample(&work, ".nexo-plugin", "1.2.0");
    git_ok(&work, &["commit", "-qam", "third"]);
    git_ok(&work, &["push", "-q", "origin", "main"]);
    assert!(update_one(cfg.path(), cwd.path(), "acme", &mut |_| Ok(false)).unwrap().is_none());
    assert_eq!(Registry::load(cfg.path()).plugins["acme"].commit, second);
    // a ref pins an older commit
    remove(cfg.path(), "acme").unwrap();
    let done = install(cfg.path(), cwd.path(), &format!("{url}@{first}"), &mut |_| Ok(true)).unwrap().unwrap();
    assert_eq!(done.commit, first);
    assert_eq!(Registry::load(cfg.path()).plugins["acme"].git_ref.as_deref(), Some(first.as_str()));
}

#[test]
fn a_bare_repository_path_is_cloned_and_claude_plugin_manifests_load() {
    let (cfg, base, cwd) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let (_, bare) = remote(base.path(), ".claude-plugin");
    let done = install(cfg.path(), cwd.path(), bare.to_str().unwrap(), &mut |_| Ok(true)).unwrap().unwrap();
    assert_eq!(done.name, "acme");
    assert_eq!(plugins::enabled(cfg.path()).0.len(), 1);
}

#[test]
fn declining_installs_nothing_and_remove_deletes() {
    let (cfg, src, cwd) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    sample(src.path(), ".nexo-plugin", "1.0.0");
    let s = src.path().to_str().unwrap();
    assert!(install(cfg.path(), cwd.path(), s, &mut |_| Ok(false)).unwrap().is_none());
    assert!(Registry::load(cfg.path()).plugins.is_empty());
    let left: Vec<_> = std::fs::read_dir(plugins::plugins_dir(cfg.path())).unwrap().flatten().collect();
    assert!(left.is_empty(), "no staging left behind: {left:?}");
    install(cfg.path(), cwd.path(), s, &mut |_| Ok(true)).unwrap();
    let dir = plugins::plugins_dir(cfg.path()).join("acme");
    assert!(dir.is_dir());
    remove(cfg.path(), "acme").unwrap();
    assert!(!dir.exists() && Registry::load(cfg.path()).plugins.is_empty());
    assert!(remove(cfg.path(), "acme").is_err());
}

#[test]
fn disable_hides_the_pieces_and_enable_brings_them_back() {
    let (cfg, src, cwd) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    sample(src.path(), ".nexo-plugin", "1.0.0");
    install(cfg.path(), cwd.path(), src.path().to_str().unwrap(), &mut |_| Ok(true)).unwrap();
    let count = || {
        let trust = cfg.path().join("trusted");
        nexo_core::compat::commands(cwd.path(), cfg.path(), &trust, None, &|_| false).len()
            + nexo_core::skills::discover_in(cwd.path(), cwd.path(), cfg.path(), true).len()
    };
    assert_eq!(count(), 2);
    set_enabled(cfg.path(), "acme", false).unwrap();
    assert_eq!(count(), 0);
    let mut c = Config::default();
    plugins::apply(&mut c, cfg.path());
    assert!(c.hooks.is_empty() && c.mcp.is_empty());
    set_enabled(cfg.path(), "acme", true).unwrap();
    assert_eq!(count(), 2);
    plugins::apply(&mut c, cfg.path());
    assert_eq!((c.hooks.len(), c.mcp.len()), (1, 1));
    assert!(set_enabled(cfg.path(), "nope", true).is_err());
}

#[test]
fn a_marketplace_resolves_names_to_folders_and_repos() {
    let (cfg, mk, cwd, base) = (
        tempfile::tempdir().unwrap(),
        tempfile::tempdir().unwrap(),
        tempfile::tempdir().unwrap(),
        tempfile::tempdir().unwrap(),
    );
    sample(&mk.path().join("plugins/acme"), ".claude-plugin", "1.0.0");
    let (_, bare) = remote(base.path(), ".nexo-plugin");
    w(
        mk.path().join(".claude-plugin/marketplace.json"),
        &format!(
            r#"{{"name":"team","owner":{{"name":"Team"}},"plugins":[
            {{"name":"acme","source":"./plugins/acme","description":"in the marketplace"}},
            {{"name":"remote-one","source":{{"source":"url","url":"file:///{}","ref":"main"}}}},
            {{"name":"evil","source":"./../x"}}]}}"#,
            bare.display().to_string().replace('\\', "/").trim_start_matches('/')
        ),
    );
    let name = market_add(cfg.path(), cwd.path(), mk.path().to_str().unwrap()).unwrap();
    assert_eq!(name, "team");
    assert!(market_list_text(cfg.path()).contains("acme@team: in the marketplace"));
    assert!(market_add(cfg.path(), cwd.path(), mk.path().to_str().unwrap())
        .unwrap_err()
        .to_string()
        .contains("already"));
    // by name@marketplace, and by bare name
    let a = resolve(cfg.path(), cwd.path(), "acme@team").unwrap();
    assert_eq!(a.marketplace.as_deref(), Some("team"));
    assert!(a.loc.replace('\\', "/").ends_with("plugins/acme"), "{a:?}");
    assert_eq!(resolve(cfg.path(), cwd.path(), "acme").unwrap().loc, a.loc);
    let r = resolve(cfg.path(), cwd.path(), "remote-one@team").unwrap();
    assert_eq!((r.loc.starts_with("file://"), r.git_ref.as_deref()), (true, Some("main")));
    assert!(resolve(cfg.path(), cwd.path(), "evil@team").is_err());
    assert!(resolve(cfg.path(), cwd.path(), "missing@team").unwrap_err().to_string().contains("no plugin missing"));
    assert!(resolve(cfg.path(), cwd.path(), "acme@other").unwrap_err().to_string().contains("no marketplace"));
    // installing through it records the marketplace
    let done = install(cfg.path(), cwd.path(), "acme@team", &mut |_| Ok(true)).unwrap().unwrap();
    assert_eq!(done.name, "acme");
    assert_eq!(Registry::load(cfg.path()).plugins["acme"].marketplace.as_deref(), Some("team"));
    assert!(list_text(cfg.path(), &BTreeMap::new()).contains("via team"));
    let r = install(cfg.path(), cwd.path(), "remote-one@team", &mut |_| Ok(true));
    assert!(r.is_err(), "same plugin name as the installed acme: {r:?}");
    market_update(cfg.path(), cwd.path(), None).unwrap();
    market_remove(cfg.path(), "team").unwrap();
    assert!(market_list_text(cfg.path()).contains("no marketplaces"));
    assert_eq!(Registry::load(cfg.path()).plugins.len(), 1, "installed plugins stay");
}

#[test]
fn nothing_is_installed_without_yes_when_nobody_can_be_asked() {
    let mut empty: &[u8] = b"";
    assert!(confirm(false, false, &mut empty).unwrap_err().to_string().contains("--yes"));
    assert!(confirm(true, false, &mut empty).unwrap());
    assert!(confirm(false, true, &mut "y\n".as_bytes()).unwrap());
    assert!(confirm(false, true, &mut "YES\n".as_bytes()).unwrap());
    assert!(!confirm(false, true, &mut "\n".as_bytes()).unwrap());
    assert!(!confirm(false, true, &mut "n\n".as_bytes()).unwrap());
}

#[test]
fn refs_urls_and_marketplace_names_are_told_apart() {
    assert_eq!(split_ref("https://h/o/r.git@v1"), ("https://h/o/r.git", Some("v1")));
    assert_eq!(split_ref("git@github.com:o/r.git"), ("git@github.com:o/r.git", None));
    assert_eq!(split_ref("git@github.com:o/r.git@v2"), ("git@github.com:o/r.git", Some("v2")));
    assert_eq!(split_ref("https://u@h/o/r"), ("https://u@h/o/r", None));
    assert_eq!(split_ref("./folder"), ("./folder", None));
    assert!(is_url("git@github.com:o/r.git") && is_url("https://h/x") && is_url("file:///x"));
    assert!(!is_url("./x@y") && !is_url("name@market"));
}

#[test]
fn the_project_records_the_plugin_for_teammates() {
    let d = tempfile::tempdir().unwrap();
    let f = d.path().join(".nexo/config.toml");
    w(f.clone(), "[agent]\nmax_turns = 3\n");
    record_in_project(&f, "acme", "https://h/o/acme.git@v1").unwrap();
    let c = Config::parse(&std::fs::read_to_string(&f).unwrap()).unwrap();
    assert_eq!(c.plugins["acme"], "https://h/o/acme.git@v1");
    assert_eq!(c.agent.max_turns, 3);
}
