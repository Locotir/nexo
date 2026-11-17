//! One run: every subcommand and the three modes picked from the arguments.

use super::*;

pub(super) async fn run_in(args: Args, cwd: PathBuf) -> anyhow::Result<i32> {
    // These commands never read the configuration, so a file that cannot be parsed must not stop them:
    // they are what a person runs to find the file (`config path`), start over (`config init`) or
    // read what is stored beside it.
    match &args.cmd {
        Some(Cmd::Config { what }) => return setup::config_cmd(&cwd, what.as_ref()),
        Some(Cmd::Guide) => {
            print!("{}", crate::account::GUIDE);
            return Ok(0);
        }
        Some(Cmd::Auth { what: AuthCmd::List }) => return Ok(crate::account::list_cmd()),
        Some(Cmd::Worktree { what }) => return crate::worktree::run(&cwd, what),
        Some(Cmd::Export(a)) => return crate::export_cmd::run(a, &cwd),
        Some(Cmd::Sessions { what: None | Some(SessionsCmd::List) }) => return Ok(setup::print_sessions()),
        Some(Cmd::Sessions { what: Some(SessionsCmd::Rename { id, title }) }) => {
            return setup::rename_session(id, &title.join(" "));
        }
        Some(Cmd::Sessions { what: Some(SessionsCmd::Delete { ids }) }) => {
            return Ok(setup::trash_sessions(ids, false))
        }
        Some(Cmd::Sessions { what: Some(SessionsCmd::Restore { ids }) }) => {
            return Ok(setup::trash_sessions(ids, true))
        }
        Some(Cmd::Sessions { what: Some(SessionsCmd::Live { json }) }) => {
            return Ok(crate::sessions_live::run(*json));
        }
        _ => {}
    }
    let mut config = match load_config(&args, &cwd) {
        Ok(c) => c,
        // `doctor` is the tool for this: it names the file and the fix instead of stopping
        Err(e) if matches!(args.cmd, Some(Cmd::Doctor { .. })) => {
            println!("FIX  config            {e:#}");
            println!("     fix the file named above, or move it aside and run `nexo config init`");
            return Ok(1);
        }
        Err(e) => return Err(point_to_the_files(e)),
    };
    if trust_asked(&args, &cwd, &config).await {
        config = load_config(&args, &cwd)?;
    }
    // what the policy forbids is refused when the person asks for it on the command line, not changed quietly
    if let Some(m) = &args.permission_mode {
        config.permissions.mode =
            PermissionMode::parse(m).ok_or_else(|| anyhow::anyhow!("unknown permission mode {m:?}"))?;
        config.check_mode(config.permissions.mode).map_err(|e| anyhow::anyhow!(e))?;
    }
    if args.yolo {
        config.check_mode(PermissionMode::Auto).map_err(|e| anyhow::anyhow!(e))?;
        config.permissions.mode = PermissionMode::Auto;
    }
    if let Some(m) = &args.sandbox {
        config.permissions.sandbox =
            nexo_core::sandbox::SandboxMode::parse(m).ok_or_else(|| anyhow::anyhow!("unknown sandbox mode {m:?}"))?;
        if let Some(p) = &config.managed {
            p.check_sandbox(config.permissions.sandbox).map_err(|e| anyhow::anyhow!(e))?;
        }
    }
    config.agent.session_title = args.name.clone();
    if let Some(c) = &args.context {
        config.context.policy = c.clone();
    }
    if let Some(s) = &args.curator_spec {
        config.context.curator_spec = s.clone();
    }
    if let Some(t) = &args.theme {
        config.ui.theme = t.clone();
    }
    if args.no_mouse {
        config.ui.mouse = false;
    }
    if args.no_alt_screen {
        std::env::set_var("NEXO_NO_ALT_SCREEN", "1");
    }
    if args.inline || config.ui.screen == "inline" {
        std::env::set_var("NEXO_INLINE", "1");
    }

    match &args.cmd {
        // the benchmark sets up its own provider and keeps away from the user configuration
        #[cfg(feature = "bench")]
        Some(Cmd::Bench(b)) => return crate::bench_cmd::run(b).await,
        Some(Cmd::Acp) => return crate::acp::run(&args).await,
        Some(Cmd::Providers { all, json }) => return Ok(setup::print_providers(&config, *all, *json)),
        Some(Cmd::Models { provider, refresh, json }) => {
            let p = provider.as_deref().or(args.provider.as_deref());
            if let Some(p) = p {
                crate::account::adopt(&mut config, p);
            }
            return setup::print_models(&config, p, *refresh, *json).await;
        }
        Some(Cmd::Auth { what: AuthCmd::Login { provider, key, flow } }) => {
            return crate::account::login_cmd(&mut config, provider.as_deref(), key.as_deref(), flow.as_deref()).await
        }
        Some(Cmd::Auth { what: AuthCmd::Logout { provider } }) => return crate::account::logout_cmd(&config, provider),
        Some(Cmd::Usage) => return crate::account::usage_cmd(&mut config).await,
        Some(Cmd::Use { provider, model, force }) => {
            return crate::account::use_cmd(&mut config, provider, model.as_deref(), *force)
        }
        Some(Cmd::Doctor { offline }) => {
            if let Some(p) = &args.provider {
                config.provider = Some(p.clone());
            }
            if let Some(m) = &args.model {
                config.model = Some(m.clone());
            }
            return crate::account::doctor(&cwd, &config, *offline).await;
        }
        Some(Cmd::Mcp { what }) => return crate::mcp_cmd::run(&cwd, &config, what).await,
        Some(Cmd::Audit { what }) => return crate::audit_cmd::run(&config, what),
        Some(Cmd::Plugin { what }) => return crate::plugin_cmd::run(&cwd, &config, what),
        Some(Cmd::Update { check }) => return crate::update::run(*check).await,
        Some(Cmd::Prompt { edit: false, tools: show_tools, builtin, window }) => {
            let tools = setup::prompt_tools(&config, &cwd);
            // what the session would get: `auto` is the built-in curator, told which tools there are
            let specs: Vec<_> = tools
                .specs_for(nexo_agent::prompt::is_compact(*window))
                .into_iter()
                .filter(|s| !nexo_agent::prompt::left_out(*window, &s.name))
                .collect();
            let names: Vec<&str> = specs.iter().map(|s| s.name.as_str()).collect();
            let policy = nexo_agent::context::from_config(&config.context);
            let directives = policy.directives(&names);
            if *show_tools {
                // as `turn::offered` hands them out: `exit_plan` only in plan mode, `goal_brief` only with a goal,
                // `recall` only when the policy offers it
                let specs: Vec<_> = specs
                    .into_iter()
                    .filter(|s| {
                        !matches!(s.name.as_str(), "exit_plan" | "goal_brief")
                            && (policy.recall() || s.name != "recall")
                    })
                    .collect();
                println!("{}", nexo_agent::prompt::tool_definitions_for(&specs, *window));
            } else if *builtin {
                let close = config.agent.close_on_verified;
                println!("{}", nexo_agent::prompt::builtin_part_for(&tools, directives, *window, close));
            } else {
                let base = nexo_agent::prompt::system_prompt_for(&cwd, &config.agent, &tools, *window);
                println!("{}", nexo_agent::prompt::with_directives(base, &config.agent, directives));
            }
            return Ok(0);
        }
        Some(Cmd::Prompt { edit: true, .. }) => return setup::edit_prompt(&config),
        Some(Cmd::Pricing { model, set }) => {
            let set = if set.is_empty() { None } else { Some(set.join(",")) };
            return setup::pricing_cmd(
                &config,
                model.as_deref(),
                args.provider.as_deref(),
                args.model.as_deref(),
                set.as_deref(),
            )
            .await;
        }
        Some(Cmd::Brief { conversation, task, tokens, engine, no_disk, json }) => {
            let mut cfg = nexo_agent::brief::BriefConfig::from_agent(&config.agent);
            if let Some(t) = tokens {
                cfg.tokens = *t;
            }
            if let Some(e) = engine {
                cfg.engine = nexo_agent::brief::Engine::parse(e);
            }
            cfg.check_disk = !no_disk;
            return setup::brief_cmd(&cwd, conversation, &task.join(" "), &cfg, *json).await;
        }
        Some(Cmd::Review { .. }) => {}
        None => {}
        // handled before the configuration is read
        Some(Cmd::Supervise { .. }) => return Ok(0),
        Some(Cmd::Bin(_)) => return Ok(0),
        Some(Cmd::Dbg(_)) => return Ok(0),
        Some(Cmd::ChromeSetup(_)) => return Ok(0),
        Some(Cmd::Re(_)) => return Ok(0),
        Some(Cmd::Api(_)) => return Ok(0),
        Some(Cmd::Team { .. }) => return Ok(0),
    }

    if args.prompt.is_none() && args.headless.any() {
        eprintln!("nexo: --output-format, --input-format, --max-turns, --allowed-tools, --disallowed-tools, --append-system-prompt and --json-schema need --print (-p)");
        return Ok(crate::headless::EXIT_USAGE);
    }
    if args.provider.is_none() {
        if let Some(why) = crate::first_use::unknown_provider(&cwd, &config) {
            anyhow::bail!(why);
        }
    }
    // plain `nexo` with nothing set up opens on the locked home; a run that cannot ask says what to do
    let unset = args.provider.is_none() && setup::nothing_chosen(&config);
    if unset && args.prompt.is_some() {
        anyhow::bail!(setup::nothing_chosen_message());
    }
    let picked: Picked = if unset {
        setup::placeholder(&config)
    } else {
        setup::pick(&config, args.provider.as_deref(), args.model.as_deref())?
    };
    let resume = if args.continue_ { nexo_agent::session::latest_for(&cwd).map(|m| m.id) } else { args.resume.clone() };
    if args.continue_ && resume.is_none() {
        eprintln!("nexo: no earlier session in {}; starting a new one", cwd.display());
    }

    if let Some(Cmd::Review { base, commit, staged, json }) = &args.cmd {
        use nexo_agent::gitcmds as g;
        let target = match (staged, commit, base) {
            (true, _, _) => g::Target::Staged,
            (_, Some(c), _) => g::Target::Commit(c.clone()),
            (_, _, Some(b)) => g::Target::Base(b.clone()),
            _ => g::Target::Default,
        };
        let prompt = match g::review_prompt_json(&cwd, &target) {
            Ok(p) => p,
            Err(e) => {
                eprintln!("nexo review: {e}");
                // a clean tree is a result, not a misuse
                return Ok(if e.starts_with("nothing to review") { 0 } else { 2 });
            }
        };
        // a review reads and never writes
        config.permissions.mode = PermissionMode::ReadOnly;
        let (text, code) = headless_collect(config, cwd, picked, prompt).await?;
        if code != 0 {
            eprintln!("nexo review: the model did not finish the review");
            return Ok(code);
        }
        return Ok(match g::parse_findings(&text) {
            Ok(f) => {
                if *json {
                    println!("{}", g::findings_json(&f));
                } else {
                    print!("{}", g::render_findings(&f));
                }
                i32::from(g::has_high(&f))
            }
            Err(e) => {
                eprintln!("nexo review: {e}; the reply was:\n{text}");
                3
            }
        });
    }

    if let Some(prompt) = args.prompt.clone() {
        // `nexo "question" -p`: the question is the request
        let prompt = if prompt.is_empty() { args.first.clone().unwrap_or_default() } else { prompt };
        return crate::headless::run(config, cwd, picked, resume, prompt, args.json, args.headless).await;
    }
    if !args.headless.add_dir.is_empty() {
        let roots = match crate::headless::add_dirs(&mut config, &cwd, &args.headless.add_dir) {
            Ok(r) => r,
            Err(m) => {
                eprintln!("nexo: {m}");
                return Ok(crate::headless::EXIT_USAGE);
            }
        };
        config.enforce_managed();
        let note = format!("You may also read and write in these folders: {}.", roots.join(", "));
        config.agent.system_prompt_extra = Some(match config.agent.system_prompt_extra.take() {
            Some(e) if !e.trim().is_empty() => format!("{}\n\n{note}", e.trim_end()),
            _ => note,
        });
    }
    let first = match crate::headless::images(&cwd, &args.headless.image) {
        Ok(i) => crate::headless::with_images(args.first.clone(), &i),
        Err(m) => {
            eprintln!("nexo: {m}");
            return Ok(crate::headless::EXIT_USAGE);
        }
    };
    {
        use std::io::IsTerminal;
        if !std::io::stdin().is_terminal() || !std::io::stdout().is_terminal() {
            eprintln!("nexo: the interactive interface needs a terminal; for scripts use `nexo -p \"<prompt>\"`");
            return Ok(crate::headless::EXIT_USAGE);
        }
    }
    interactive(config, cwd, picked, resume, unset, first).await
}
