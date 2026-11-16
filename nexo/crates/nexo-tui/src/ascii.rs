//! A plain-ASCII fallback for the chrome: terminals without a UTF-8 locale or with a
//! console font (TERM=linux) cannot draw the box, gauge and icon glyphs, so every one of
//! them is folded to a character that is always there. Text the person or the model wrote is
//! left alone: only the interface's own symbols are in the table.

use ratatui::buffer::Buffer;
use unicode_width::UnicodeWidthStr;

/// Whether the ASCII fallback is wanted: `NEXO_ASCII=1` forces it, `NEXO_ASCII=0` forbids it,
/// otherwise it is on for the Linux console, for a locale that is not UTF-8 and for a legacy Windows console
/// (`legacy_windows`: conhost, whose default font has no braille, so the effects would show as boxes).
pub fn wanted_from(forced: &str, term: &str, locale: &str, legacy_windows: bool) -> bool {
    match forced.trim() {
        "1" | "true" | "yes" | "on" => return true,
        "0" | "false" | "no" | "off" => return false,
        _ => {}
    }
    if term == "linux" || legacy_windows {
        return true;
    }
    let l = locale.to_ascii_lowercase();
    !l.is_empty() && !l.contains("utf-8") && !l.contains("utf8")
}

/// Whether the environment looks like the bare legacy Windows console (conhost). Every modern
/// terminal on Windows names itself through one of these variables (Windows Terminal, VS Code,
/// WezTerm, Alacritty, ConEmu, Tabby, mintty, a TERM from ssh or MSYS), and they all draw braille
/// and box glyphs; only a console that sets none of them is the one whose font has no braille.
pub fn legacy_console(get: &dyn Fn(&str) -> String) -> bool {
    const MODERN: [&str; 11] = [
        "WT_SESSION",
        "TERM",
        "TERM_PROGRAM",
        "WEZTERM_EXECUTABLE",
        "ALACRITTY_LOG",
        "ALACRITTY_SOCKET",
        "ConEmuPID",
        "ANSICON",
        "COLORTERM",
        "VSCODE_INJECTION",
        "TABBY_CONFIG_DIRECTORY",
    ];
    MODERN.iter().all(|k| get(k).is_empty())
}

/// The fallback, read once from the environment.
pub fn wanted() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| {
        let get = |k: &str| std::env::var(k).unwrap_or_default();
        let locale = ["LC_ALL", "LC_CTYPE", "LANG"].iter().map(|k| get(k)).find(|v| !v.is_empty()).unwrap_or_default();
        let legacy = cfg!(windows) && legacy_console(&get);
        wanted_from(&get("NEXO_ASCII"), &get("TERM"), &locale, legacy)
    })
}

/// The ASCII stand-in for one interface symbol (None: not one of ours).
pub fn fold_char(c: char) -> Option<char> {
    Some(match c {
        '█' | '▇' | '▉' => '#',
        '▏' | '▎' | '▍' | '▌' | '▋' | '▊' | '│' | '║' | '┃' | '▕' => '|',
        '░' | '▁' | '▂' => '.',
        '▒' | '▃' | '▄' => ':',
        '▓' | '▅' | '▆' => '|',
        '─' | '━' | '╌' => '-',
        '═' => '=',
        '┌' | '┐' | '└' | '┘' | '├' | '┤' | '┬' | '┴' | '┼' | '╭' | '╮' | '╰' | '╯' => '+',
        '·' | '•' | '∙' => '.',
        '…' => '.',
        '◎' | '○' | '◌' => 'o',
        '◉' => 'O',
        '╸' => '-',
        '●' | '◆' | '◇' | '⏱' | '★' => '*',
        '⎇' => '@',
        'Σ' => 'S',
        '▸' | '▶' | '›' | '❯' | '→' => '>',
        '◂' | '◀' | '‹' | '←' => '<',
        '▾' | '▼' | '↓' => 'v',
        '☰' | '≡' => '=',
        '⚙' => '*',
        '◐' => '>',
        '▐' => '|',
        '▴' | '▲' | '↑' => '^',
        '✓' | '✔' => '+',
        '✗' | '✘' | '×' => 'x',
        '⚠' => '!',
        '\u{2800}'..='\u{28ff}' => ['|', '/', '-', '\\'][(c as u32 % 4) as usize],
        _ => return None,
    })
}

/// Folds every interface symbol of the frame to ASCII. A wide symbol (width 2) leaves a hole
/// after it, which becomes a space so the rest of the row keeps its columns.
pub fn fold(buf: &mut Buffer) {
    let n = buf.content.len();
    for i in 0..n {
        let sym = buf.content[i].symbol().to_string();
        let mut chars = sym.chars();
        let (Some(c), None) = (chars.next(), chars.next()) else { continue };
        let Some(a) = fold_char(c) else { continue };
        let wide = sym.width() == 2;
        buf.content[i].set_symbol(a.encode_utf8(&mut [0; 4]));
        if wide && i + 1 < n && buf.content[i + 1].symbol().is_empty() {
            buf.content[i + 1].set_symbol(" ");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::layout::Rect;

    #[test]
    fn the_fallback_follows_the_environment() {
        assert!(wanted_from("1", "xterm-256color", "en_US.UTF-8", false));
        assert!(!wanted_from("0", "linux", "C", false));
        assert!(wanted_from("", "linux", "en_US.UTF-8", false));
        assert!(wanted_from("", "xterm", "C", false));
        assert!(wanted_from("", "xterm", "POSIX", false));
        assert!(!wanted_from("", "xterm-256color", "es_ES.utf8", false));
        assert!(!wanted_from("", "xterm-256color", "", false));
    }

    #[test]
    fn a_legacy_windows_console_gets_ascii_unless_the_person_says_otherwise() {
        // conhost: no WT_SESSION, no TERM, no locale
        assert!(wanted_from("", "", "", true));
        // Windows Terminal sets WT_SESSION, so it is not legacy and keeps its braille
        assert!(!wanted_from("", "", "", false));
        assert!(!wanted_from("0", "", "", true));
        assert!(wanted_from("1", "", "", false));
    }

    #[test]
    fn only_a_console_that_names_no_modern_terminal_is_legacy() {
        let with = |name: &'static str| move |k: &str| if k == name { "x".to_string() } else { String::new() };
        assert!(legacy_console(&|_: &str| String::new()));
        for k in ["WT_SESSION", "TERM", "TERM_PROGRAM", "WEZTERM_EXECUTABLE", "ALACRITTY_LOG", "ConEmuPID", "COLORTERM"]
        {
            assert!(!legacy_console(&with(k)), "{k} should mark a modern terminal");
        }
    }

    #[test]
    fn every_symbol_of_the_table_becomes_one_plain_ascii_character() {
        for c in "█▏▎▍▌▋▊▉│─═┌┐└┘├┤╭╮╰╯·…◎◉╸⎇Σ◆⏱●○▸▾✓✗⠋⠙⠹".chars()
        {
            let a = fold_char(c).unwrap_or_else(|| panic!("{c} has no stand-in"));
            assert!(a.is_ascii() && !a.is_control(), "{c} -> {a:?}");
        }
        assert_eq!(fold_char('a'), None);
        assert_eq!(fold_char('é'), None);
    }

    #[test]
    fn a_frame_and_its_symbols_are_folded() {
        let mut buf = Buffer::empty(Rect::new(0, 0, 10, 1));
        buf.set_string(0, 0, "│⏱ 5% é█", ratatui::style::Style::default());
        fold(&mut buf);
        let row: String = (0..10).map(|x| buf[(x, 0)].symbol().to_string()).collect();
        assert!(row.starts_with("|* 5%"), "{row}");
        assert!(row.contains('#'));
        assert!(row.contains('é'));
        assert!(!row.contains('⏱') && !row.contains('│') && !row.contains('█'));
    }
}
