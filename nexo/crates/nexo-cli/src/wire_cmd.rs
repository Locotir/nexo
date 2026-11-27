//! `nexo --wire` and `nexo wire`: turn the recording of model requests on, and find its report afterwards
//! (see docs/WIRE.md).

use std::path::{Path, PathBuf};

use clap::Args;
use nexo_providers::wire;

#[derive(Args, Debug)]
pub struct WireArgs {
    /// Open the newest report in the browser.
    #[arg(long)]
    pub open: bool,
    /// The folder the runs are in (default: ~/.nexo/wire, or NEXO_WIRE).
    #[arg(long, value_name = "DIR")]
    pub dir: Option<PathBuf>,
}

/// Turns the recording on in `dir` (empty: the default folder) and says where its report is.
pub fn start(dir: &Path, cwd: &Path) {
    let root = if dir.as_os_str().is_empty() {
        wire::default_dir()
    } else if dir.is_absolute() {
        dir.to_path_buf()
    } else {
        cwd.join(dir)
    };
    std::env::set_var(wire::ENV, &root);
    match wire::run_dir() {
        Some(run) => eprintln!(
            "nexo wire: recording every model request in {} (open report.html, or run `nexo wire --open`)",
            nexo_core::paths::shown(&run)
        ),
        None => eprintln!("nexo wire: cannot record in {}", nexo_core::paths::shown(&root)),
    }
}

fn root(a: &WireArgs) -> PathBuf {
    a.dir
        .clone()
        .or_else(|| std::env::var_os(wire::ENV).filter(|p| !p.is_empty()).map(PathBuf::from))
        .unwrap_or_else(wire::default_dir)
}

pub fn run(a: &WireArgs) -> anyhow::Result<i32> {
    let root = root(a);
    let Some(run) = wire::latest(&root) else {
        eprintln!("no recording in {} yet: start nexo with --wire", nexo_core::paths::shown(&root));
        return Ok(1);
    };
    let report = run.join("report.html");
    println!("{}", report.display());
    if a.open {
        let url = format!("file://{}", nexo_core::shell::slashed(&report));
        let url = if url.starts_with("file:///") { url } else { url.replacen("file://", "file:///", 1) };
        if !nexo_tui::links::open(&url) {
            eprintln!("could not start a browser: open the file above");
            return Ok(1);
        }
    }
    Ok(0)
}
