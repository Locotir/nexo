//! `nexo bin managed`: JVM `.class` and `.jar` files (and `info` on them), answered by the
//! `nexo-bin` crate without opening the file as a native image.

use crate::bin_cmd::BinOp;
use nexo_bin::{Error, ManagedQuery, MIN_LINES};

/// `Some(answer)` when this command is about managed bytecode, `None` for the native ops.
pub fn try_managed(op: &BinOp) -> Option<Result<String, Error>> {
    let (query, common, file) = match op {
        BinOp::Managed { file, target, strings, callers, grep, common } => (
            ManagedQuery {
                target: target.as_deref(),
                strings: *strings,
                callers: callers.as_deref(),
                grep: grep.as_deref(),
            },
            common,
            file,
        ),
        // `info` on a class or jar gives the map of types instead of an error
        BinOp::Info { file, common } => {
            let mut head = vec![0u8; nexo_bin::MANAGED_HEAD];
            let n = std::fs::File::open(file).and_then(|mut f| std::io::Read::read(&mut f, &mut head)).ok()?;
            if !nexo_bin::is_managed(&head[..n]) {
                return None;
            }
            (ManagedQuery::default(), common, file)
        }
        _ => return None,
    };
    Some(answer(file, &query, common.from, common.lines.max(MIN_LINES), common.json))
}

fn answer(
    file: &std::path::Path,
    q: &ManagedQuery<'_>,
    from: Option<u64>,
    lines: usize,
    json: bool,
) -> Result<String, Error> {
    let data = std::fs::read(file).map_err(|e| Error::Io(format!("cannot read {}: {e}", file.display())))?;
    let out = nexo_bin::managed_window(file, &data, q, from, lines)?;
    let page = out.page_as(from, lines, json);
    if json {
        serde_json::to_string_pretty(&page.value).map_err(|e| Error::Bad(e.to_string()))
    } else {
        Ok(page.text())
    }
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    fn ask(args: &[&str]) -> Result<String, nexo_bin::Error> {
        let mut argv = vec!["nexo", "bin"];
        argv.extend_from_slice(args);
        let parsed = crate::cli::Args::try_parse_from(argv).expect("the arguments parse");
        match parsed.cmd {
            Some(crate::cli::Cmd::Bin(b)) => {
                crate::bin_cmd::execute_in(&b, std::path::Path::new("/nonexistent-nexo-data"))
            }
            other => panic!("not a bin command: {other:?}"),
        }
    }

    #[test]
    fn managed_and_info_read_a_class_and_a_jar_through_the_parser() {
        let dir = tempfile::tempdir().unwrap();
        let class = dir.path().join("Calc.class");
        std::fs::write(&class, nexo_bin::managed_fixtures::demo_class()).unwrap();
        let jar = dir.path().join("app.jar");
        std::fs::write(
            &jar,
            nexo_bin::managed_fixtures::zip(&[("demo/Calc.class", nexo_bin::managed_fixtures::demo_class(), true)]),
        )
        .unwrap();
        let (c, j) = (class.display().to_string(), jar.display().to_string());
        let t = ask(&["managed", &c, "Calc.add"]).unwrap();
        assert!(t.contains("0x0002  iadd"), "{t}");
        let t = ask(&["managed", &j, "--strings", "--grep", "jvm"]).unwrap();
        assert!(t.contains("demo.Calc.greet +0x0003  \"hello jvm\""), "{t}");
        let t = ask(&["info", &j]).unwrap();
        assert!(t.contains("JAR app.jar: 1 types"), "{t}");
        let t = ask(&["managed", &c, "--json", "Calc.greet"]).unwrap();
        assert!(t.contains("\"op\": \"managed-method\"") && t.contains("\"left\": 0"), "{t}");
        let e = ask(&["fn", &c, "main"]).unwrap_err();
        assert!(e.to_string().contains("nexo bin managed"), "{e}");
    }
}
