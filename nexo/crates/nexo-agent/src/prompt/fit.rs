//! The person's own text in the system prompt (instruction files, preferences, the extra prompt) against a
//! small window: together it takes at most a quarter of the window, and each cut says what was cut.

use std::path::PathBuf;

use super::Volatile;

/// The least characters the person's own text may be given, whatever the window.
const FLOOR_CHARS: usize = 1_000;
/// What the line that says a text was cut may take, set aside from the budget for each text.
const CUT_LINE_CHARS: usize = 260;

/// The characters the person's own text may take together in a window of `window` tokens: a quarter of
/// the window, at the four characters a token the token estimate uses. `None` (window unknown) is no limit.
pub(super) fn budget(window: Option<u64>) -> Option<usize> {
    window.map(|w| usize::try_from(w / 4).unwrap_or(usize::MAX).saturating_mul(4))
}

/// How many characters each text of lengths `lens` may keep when together they may take `budget`: a text
/// shorter than its share keeps all of it and leaves the rest to the longer ones.
pub(super) fn caps(lens: &[usize], budget: usize) -> Vec<usize> {
    let mut order: Vec<usize> = (0..lens.len()).collect();
    order.sort_by_key(|&i| lens[i]);
    let mut out = lens.to_vec();
    let mut left = budget;
    for (k, &i) in order.iter().enumerate() {
        let share = left / (order.len() - k);
        if lens[i] <= share {
            left -= lens[i];
        } else {
            for &j in &order[k..] {
                out[j] = share;
            }
            break;
        }
    }
    out
}

/// The first `cap` characters of `text`, ended at the last line end in their second half when there is one.
fn head(text: &str, cap: usize) -> &str {
    let end = text.char_indices().nth(cap).map_or(text.len(), |(i, _)| i);
    let cut = &text[..end];
    match cut.rfind('\n') {
        Some(i) if i >= cut.len() / 2 => &cut[..i],
        _ => cut,
    }
}

/// What is said where a text was cut: its name, what is left of it, and where the rest is.
fn said(name: &str, kept: usize, full: usize, window: u64, rest: &str) -> String {
    format!("{name} cut to {kept} of {full} characters for a {window}-token window; {rest}")
}

/// `text` kept to `cap` characters, with the line that says so after it when it was cut.
fn part(text: &str, cap: usize, name: &str, full: usize, window: u64, rest: &str) -> String {
    if text.chars().count() <= cap {
        return text.to_string();
    }
    let kept = head(text, cap);
    format!("{kept}\n{}", said(name, kept.chars().count(), full, window, rest))
}

/// The person's own text as the prompt carries it.
pub(super) struct Fitted {
    pub instructions: String,
    pub prefs: String,
    pub extra: String,
}

/// `vol`'s instruction files, the framed `prefs` (read from `prefs_path`) and the `extra` prompt, fitted to
/// `window`. With no window, or when they fit, they come out as they are.
pub(super) fn fit(
    window: Option<u64>,
    vol: &Volatile,
    prefs: &str,
    prefs_path: Option<PathBuf>,
    extra: &str,
) -> Fitted {
    let whole = || Fitted { instructions: vol.instructions.clone(), prefs: prefs.into(), extra: extra.into() };
    let (Some(w), Some(budget)) = (window, budget(window)) else { return whole() };
    // a session saved before the files were kept apart has them in one text: it counts, it is not cut
    let split = !vol.files.is_empty() || vol.instructions.is_empty();
    let fixed = if split {
        vol.files.iter().map(|f| f.head.chars().count()).sum::<usize>() + vol.nested.chars().count()
    } else {
        vol.instructions.chars().count()
    };
    let mut lens: Vec<usize> = vol.files.iter().map(|f| f.text.chars().count()).collect();
    lens.push(prefs.chars().count());
    lens.push(extra.chars().count());
    if lens.iter().sum::<usize>() + fixed <= budget {
        return whole();
    }
    let lines = CUT_LINE_CHARS * lens.iter().filter(|l| **l > 0).count();
    let caps = caps(&lens, budget.saturating_sub(fixed + lines).max(FLOOR_CHARS));
    let mut instructions = if split { String::new() } else { vol.instructions.clone() };
    for (f, cap) in vol.files.iter().zip(&caps) {
        let full = if f.full > 0 { f.full } else { f.text.chars().count() };
        let text = part(&f.text, *cap, &f.name, full, w, &format!("read {} for the rest", f.path));
        instructions.push_str(&f.head);
        instructions.push_str(&text);
    }
    if split {
        instructions.push_str(&vol.nested);
    }
    let n = vol.files.len();
    let prefs_name = prefs_path
        .as_ref()
        .and_then(|p| p.file_name())
        .map_or_else(|| "the preferences".to_string(), |f| f.to_string_lossy().into_owned());
    let prefs_rest = match &prefs_path {
        Some(p) => format!("read {} for the rest", p.display()),
        None => "they are in the person's prompt file".to_string(),
    };
    let prefs_full = prefs.chars().count();
    Fitted {
        instructions,
        prefs: part(prefs, caps[n], &prefs_name, prefs_full, w, &prefs_rest),
        extra: part(
            extra,
            caps[n + 1],
            "system_prompt_extra",
            extra.chars().count(),
            w,
            "it is the agent.system_prompt_extra setting in the configuration",
        ),
    }
}
