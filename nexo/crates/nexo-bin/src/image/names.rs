//! Names as the file writes them and as they are shown.

use super::{MAX_NAME, MAX_NAME_IN};

/// Demangle Rust (legacy and v0), Itanium C++ and MSVC C++ names; anything else stays as written. The
/// whole name is demangled (up to `MAX_NAME_IN` characters) and then the result is cut to `MAX_NAME`.
pub(crate) fn demangle(raw: &str) -> String {
    let cut = |t: &str| t.chars().take(MAX_NAME).collect::<String>();
    let raw: String = raw.chars().take(MAX_NAME_IN).collect();
    // Mach-O writes every C symbol with one more underscore in front
    let body = raw.strip_prefix("__Z").map(|_| &raw[1..]).unwrap_or(&raw);
    if body.starts_with("_Z") || body.starts_with("_R") {
        if let Ok(d) = rustc_demangle::try_demangle(body) {
            return cut(&format!("{d:#}"));
        }
        if body.starts_with("_Z") {
            if let Ok(s) = cpp_demangle::Symbol::new(body.as_bytes()) {
                if let Ok(t) = s.demangle() {
                    return cut(&t);
                }
            }
        }
    }
    if raw.starts_with('?') {
        if let Ok(t) = msvc_demangler::demangle(&raw, msvc_demangler::DemangleFlags::llvm()) {
            return cut(&t);
        }
    }
    cut(&raw)
}

/// The C name in a 32-bit Windows symbol: `_main` and `_WinMain@16` (cdecl, stdcall) and `@f@8` (fastcall)
/// name `main`, `WinMain` and `f`. None when the name carries no such decoration.
pub(super) fn undecorate_x86(raw: &str) -> Option<String> {
    let at_n = |s: &str| -> Option<usize> {
        let i = s.rfind('@')?;
        (i > 0 && s.len() > i + 1 && s[i + 1..].bytes().all(|b| b.is_ascii_digit())).then_some(i)
    };
    let body = if let Some(r) = raw.strip_prefix('@') {
        &r[..at_n(r)?]
    } else {
        let r = raw.strip_prefix('_')?;
        at_n(r).map_or(r, |i| &r[..i])
    };
    (!body.is_empty() && !body.contains('@')).then(|| body.to_string())
}

/// The mangled name to keep next to a demangled one: empty when demangling changed nothing.
pub(crate) fn raw_of(raw: &str, shown: &str) -> String {
    if raw == shown {
        String::new()
    } else {
        raw.chars().take(MAX_NAME_IN).collect()
    }
}
