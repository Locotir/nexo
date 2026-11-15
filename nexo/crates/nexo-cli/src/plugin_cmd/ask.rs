use super::*;

// ---- asking -------------------------------------------------------------------------------------

/// Shows `summary` and asks. With `yes` it only shows it.
pub(super) fn ask_person(summary: &str, yes: bool) -> anyhow::Result<bool> {
    println!("{summary}");
    let stdin = std::io::stdin();
    confirm(yes, stdin.is_terminal(), &mut stdin.lock())
}

/// The answer: yes when `yes`; an error when nobody can be asked; else a line from `input`.
pub(super) fn confirm(yes: bool, interactive: bool, input: &mut dyn BufRead) -> anyhow::Result<bool> {
    if yes {
        return Ok(true);
    }
    if !interactive {
        bail!("nothing can be asked here, so pass --yes to go ahead (read what it adds above first)");
    }
    print!("Go ahead? [y/N] ");
    std::io::stdout().flush().ok();
    let mut line = String::new();
    input.read_line(&mut line)?;
    Ok(matches!(line.trim().to_ascii_lowercase().as_str(), "y" | "yes"))
}

pub(super) type Ask<'a> = &'a mut dyn FnMut(&str) -> anyhow::Result<bool>;

pub(super) fn short(commit: &str) -> &str {
    &commit[..commit.len().min(12)]
}
