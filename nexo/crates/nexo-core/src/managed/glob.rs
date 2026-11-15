//! Case-insensitive globbing and the ranking of sandbox modes the policy compares with.

use crate::sandbox::SandboxMode;

/// `*` and `?` globbing, case-insensitive.
pub fn glob_match(pattern: &str, text: &str) -> bool {
    let p: Vec<char> = pattern.to_lowercase().chars().collect();
    let t: Vec<char> = text.to_lowercase().chars().collect();
    let (mut pi, mut ti, mut star, mut mark) = (0usize, 0usize, None::<usize>, 0usize);
    while ti < t.len() {
        if pi < p.len() && p[pi] != '*' && (p[pi] == '?' || p[pi] == t[ti]) {
            pi += 1;
            ti += 1;
        } else if pi < p.len() && p[pi] == '*' {
            star = Some(pi);
            mark = ti;
            pi += 1;
        } else if let Some(s) = star {
            pi = s + 1;
            mark += 1;
            ti = mark;
        } else {
            return false;
        }
    }
    while pi < p.len() && p[pi] == '*' {
        pi += 1;
    }
    pi == p.len()
}

pub(super) fn any_match(globs: &[String], text: &str) -> bool {
    globs.iter().any(|g| glob_match(g, text))
}

pub(super) fn rank(m: SandboxMode) -> u8 {
    match m {
        SandboxMode::Off => 0,
        SandboxMode::WorkspaceWrite => 1,
        SandboxMode::ReadOnly => 2,
    }
}
