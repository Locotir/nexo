//! How the environment variables that switch something on or off are read, in one place, so that
//! `NEXO_NO_UPDATE_CHECK=0` means the same as `NEXO_NO_ALT_SCREEN=0` and `NO_COLOR=` the same
//! everywhere.

/// Whether the variable `name` switches something on: set, not blank, and not `0` or `false` in
/// any case. `NAME=0` is how a person writes "no", so presence alone must not count.
pub fn env_flag(name: &str) -> bool {
    flag_value(std::env::var(name).ok().as_deref())
}

/// The reading of one value of [`env_flag`], apart from the process environment so tests need not
/// change it.
pub fn flag_value(value: Option<&str>) -> bool {
    value.is_some_and(|v| {
        let v = v.trim();
        !v.is_empty() && v != "0" && !v.eq_ignore_ascii_case("false")
    })
}

/// Colour is off because `NO_COLOR` is set to anything but empty (the rule of no-color.org).
pub fn no_color() -> bool {
    no_color_value(std::env::var_os("NO_COLOR").as_deref())
}

/// The reading of one value of `NO_COLOR`; `None` is unset. The value is judged as bytes, so one
/// that is not UTF-8 still counts.
pub fn no_color_value(value: Option<&std::ffi::OsStr>) -> bool {
    value.is_some_and(|v| !v.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsStr;

    #[test]
    fn a_flag_is_off_when_unset_blank_zero_or_false() {
        for off in [None, Some(""), Some("  "), Some("0"), Some("false"), Some("FALSE"), Some(" False ")] {
            assert!(!flag_value(off), "{off:?}");
        }
    }

    #[test]
    fn a_flag_is_on_for_one_and_other_words() {
        for on in ["1", "true", "yes", "on"] {
            assert!(flag_value(Some(on)), "{on}");
        }
    }

    #[test]
    fn no_color_follows_no_color_org() {
        assert!(!no_color_value(None));
        assert!(!no_color_value(Some(OsStr::new(""))));
        assert!(no_color_value(Some(OsStr::new("1"))));
        assert!(no_color_value(Some(OsStr::new("0"))), "any non-empty value counts, even 0");
    }
}
