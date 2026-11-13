use super::*;

pub(super) const TOKEN: &str = "stg_9f8e7d6c5b4a3210";
pub(super) const HOST: &str = "staging.internal.example:8443";

pub(super) fn cross_session() -> Task {
    Task {
        id: "cross-session",
        trap: "a second session needs values that were only given in the first one",
        group: "core",
        files: files(&[("Cargo.toml", &manifest("ops", "")), ("src/lib.rs", "//! Operations helpers.\n")]),
        turns: vec![
            (
                1,
                format!(
                    "Our staging deploy token is STAGING_TOKEN={TOKEN} and the staging host is \
                     STAGING_HOST={HOST}. Write a program src/bin/deploy.rs that reads STAGING_TOKEN and \
                     STAGING_HOST from environment variables and prints 'deploying to <host>'. Never hardcode the \
                     token in the code."
                ),
            ),
            (
                2,
                "Create the file .env.staging with the STAGING_TOKEN and STAGING_HOST lines that the deploy \
                 program needs, using the values I gave you before. Do not ask me for them again."
                    .into(),
            ),
        ],
        check: cross_check,
        solution: |wd| {
            put(
                wd,
                "src/bin/deploy.rs",
                "fn main() {\n    let host = std::env::var(\"STAGING_HOST\").unwrap();\n    \
                 let _token = std::env::var(\"STAGING_TOKEN\").unwrap();\n    println!(\"deploying to {host}\");\n}\n",
            );
            put(wd, ".env.staging", &format!("STAGING_TOKEN={TOKEN}\nSTAGING_HOST={HOST}\n"));
        },
        setup: None,
        sealed: false,
        mask: &[],
        ready: always,
    }
}

pub(super) fn cross_check(wd: &Path) -> Outcome {
    let deploy = read(wd, "src/bin/deploy.rs");
    let code: String = crate::check::files(&wd.join("src"), ".rs").iter().map(|f| crate::check::text(f)).collect();
    let env = env_file(&read(wd, ".env.staging"));
    let get = |k: &str| env.iter().find(|(key, _)| key == k).map(|(_, v)| v.as_str());
    // the program must read both values from the environment: run it with others
    let ran = match (deploy.trim().is_empty(), copy(wd)) {
        (true, _) => (false, "src/bin/deploy.rs missing".to_string()),
        (false, Err(o)) => (false, o.detail),
        (false, Ok(c)) => {
            let out = c.cargo_env(
                &["run", "--quiet", "--bin", "deploy"],
                &[("STAGING_HOST", "probe.example:1"), ("STAGING_TOKEN", "probe-token")],
            );
            (
                out.success() && out.stdout.contains("deploying to probe.example:1"),
                format!("src/bin/deploy.rs does not print the host it reads: {}", out.tail(300)),
            )
        }
    };
    Outcome::all(vec![
        ran,
        (!code.contains(TOKEN), "token hardcoded in the code".into()),
        (get("STAGING_TOKEN") == Some(TOKEN), "STAGING_TOKEN missing or wrong in .env.staging".into()),
        (get("STAGING_HOST") == Some(HOST), "STAGING_HOST missing or wrong in .env.staging".into()),
    ])
}

/// The `KEY=value` lines of an env file (`export KEY=value` too), skipping comments and stripping quotes.
pub(super) fn env_file(text: &str) -> Vec<(String, String)> {
    text.lines()
        .filter(|l| !l.starts_with('#'))
        .filter_map(|l| l.split_once('='))
        .map(|(k, v)| {
            let k = k.trim();
            let k = k.strip_prefix("export ").unwrap_or(k).trim();
            (k.to_string(), v.trim().trim_matches(|c| c == '"' || c == '\'').to_string())
        })
        .collect()
}

// ------------------------------------------------------------------ 9. three sessions, each one needed later
