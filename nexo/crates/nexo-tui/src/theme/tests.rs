#[test]
fn the_nexo_theme_leaves_the_terminal_ground_alone() {
    let t = super::Theme::nexo();
    assert_eq!((t.bg, t.bar_bg), (ratatui::style::Color::Reset, ratatui::style::Color::Reset));
}

#[test]
fn auto_reads_the_terminal_background() {
    assert!(light_background(Some("0;15")));
    assert!(light_background(Some("0;default;7")));
    assert!(!light_background(Some("15;0")));
    assert!(!light_background(None));
}

use super::*;

#[test]
fn every_name_resolves_and_the_default_is_dark() {
    for n in NAMES {
        let t = Theme::by_name(n).unwrap_or_else(|| panic!("{n}"));
        assert_eq!(&t.name, n);
    }
    assert_eq!(Theme::by_name("Catppuccin").unwrap().name, "catppuccin-mocha");
    assert_eq!(Theme::by_name("tokyonight").unwrap().name, "tokyo-night");
    assert!(Theme::by_name("neon").is_none());
    assert_eq!(Theme::by_name("").unwrap().name, "nexo", "nexo is the default theme");
    assert_eq!(Theme::by_name("default").unwrap().name, "nexo");
    assert_eq!(&NAMES[..2], &["nexo", "dark"], "nexo first, dark always second");
    assert_eq!(Theme::pick("no-such-theme").name, "nexo");
}

#[test]
fn every_colour_of_every_coloured_theme_is_an_explicit_rgb_value() {
    for n in NAMES.iter().filter(|n| **n != "mono") {
        let t = Theme::by_name(n).unwrap();
        for c in t.colours() {
            // nexo paints no ground of its own: the terminal's shows through
            assert!(matches!(c, Color::Rgb(..)) || (*n == "nexo" && c == Color::Reset), "{n}: {c:?} is not RGB");
        }
        assert_eq!(t.base().bg, Some(t.bg));
        assert_eq!(t.base().fg, Some(t.fg));
        assert_eq!(t.alt().bg, Some(t.bg_alt));
        assert_eq!(t.diff_add_line().bg, Some(t.diff_add_bg));
        assert_eq!(t.code_block().bg, Some(t.code_bg));
    }
}

#[test]
fn depth_follows_the_environment() {
    assert_eq!(depth_from("", "truecolor", "xterm", false), Depth::True);
    assert_eq!(depth_from("", "", "xterm-256color", false), Depth::Indexed256);
    assert_eq!(depth_from("", "", "linux", false), Depth::Ansi16);
    assert_eq!(depth_from("16", "truecolor", "xterm-256color", false), Depth::Ansi16);
    assert_eq!(depth_from("256", "", "linux", false), Depth::Indexed256);
    // a Windows console sets no TERM and takes 24-bit colour; Git Bash's mintty sets one
    assert_eq!(depth_from("", "", "", true), Depth::True);
    assert_eq!(depth_from("", "", "xterm", true), Depth::Ansi16);
    assert_eq!(depth_from("", "", "", false), Depth::Ansi16);
    assert_eq!(depth_from("16", "", "", true), Depth::Ansi16);
}

#[test]
fn pastels_keep_their_hue_on_sixteen_colours() {
    assert_eq!(nearest_16(0x7e, 0xf0, 0xdc), Color::LightCyan);
    assert_eq!(nearest_16(0xbf, 0xa8, 0xff), Color::LightBlue);
    assert_eq!(nearest_16(0xff, 0x9e, 0xcf), Color::LightMagenta);
    assert_eq!(nearest_16(0x95, 0xf0, 0xa0), Color::LightGreen);
    assert_eq!(nearest_16(0xff, 0x8a, 0xa0), Color::LightRed);
    assert_eq!(nearest_16(0xff, 0xe3, 0x8a), Color::LightYellow);
    assert_eq!(nearest_16(0xa4, 0xa9, 0xc4), Color::Gray);
    assert_eq!(nearest_16(0x14, 0x15, 0x1f), Color::Black);
}

#[test]
fn the_four_kinds_of_message_have_four_colours_far_apart_in_every_theme() {
    let names =
        ["dark", "nexo", "light", "catppuccin-mocha", "gruvbox", "nord", "dracula", "solarized-dark", "tokyo-night"];
    for name in names {
        let t = Theme::by_name(name).unwrap();
        let k = t.kind_colours();
        let gap = min_gap(&k).unwrap();
        assert!(gap >= 0.065, "{name}: {gap:.3}");
        // tool calls and tool output share a glyph in the chart: their colours are clearly apart
        let calls = min_gap(&k[2..]).unwrap();
        assert!(calls >= 0.14, "{name}: {calls:.3}");
        // on 16 and 256 colours the four stay four
        for map in [to_ansi16 as fn(Color) -> Color, to_indexed] {
            let mut d = t.clone();
            for c in d.colours_mut() {
                *c = map(*c);
            }
            d.degraded = true;
            let k = d.kind_colours();
            assert!((0..4).all(|i| (i + 1..4).all(|j| k[i] != k[j])), "{name}: {k:?}");
        }
    }
    // the default theme: calls in orange, output in purple
    let t = Theme::dark();
    assert_eq!(t.kind_colours(), [t.user, t.assistant, t.warn2, t.accent2]);
    assert_eq!(Theme::mono().kind_colours(), [Color::Reset; 4]);
}

#[test]
fn the_default_text_colours_pass_aa_on_every_ground() {
    fn lum(c: Color) -> f64 {
        let Color::Rgb(r, g, b) = c else { panic!("{c:?}") };
        let f = |v: u8| {
            let v = v as f64 / 255.0;
            if v <= 0.03928 {
                v / 12.92
            } else {
                ((v + 0.055) / 1.055).powf(2.4)
            }
        };
        0.2126 * f(r) + 0.7152 * f(g) + 0.0722 * f(b)
    }
    let ratio = |a: Color, b: Color| {
        let (x, y) = (lum(a), lum(b));
        (x.max(y) + 0.05) / (x.min(y) + 0.05)
    };
    let t = Theme::dark();
    let text = [
        t.fg,
        t.dim,
        t.dim2,
        t.accent,
        t.accent2,
        t.code_fg,
        t.user,
        t.assistant,
        t.success,
        t.error,
        t.warn,
        t.warn2,
        t.info,
        t.diff_add,
        t.diff_del,
        t.hunk,
        t.bar_fg,
    ];
    for ground in [t.bg, t.bg_alt, t.code_bg, t.bar_bg, t.modal_bg] {
        for c in text {
            assert!(ratio(c, ground) >= 4.5, "{c:?} on {ground:?}: {:.2}", ratio(c, ground));
        }
    }
    assert!(ratio(t.highlight_fg, t.highlight_bg) >= 4.5);
    assert!(ratio(t.fg, t.selection_bg) >= 4.5);
}

#[test]
fn sixteen_colours_pick_the_nearest_standard_one() {
    assert_eq!(nearest_16(0, 0, 0), Color::Black);
    assert_eq!(nearest_16(250, 250, 250), Color::White);
    assert_eq!(nearest_16(210, 10, 10), Color::Red);
    assert_eq!(nearest_16(120, 120, 120), Color::DarkGray);
    assert_eq!(nearest_16(20, 30, 200), Color::Blue);
}

#[test]
fn mono_uses_modifiers_not_colours() {
    let m = Theme::mono();
    assert_eq!(m.diff_add_line().fg, None);
    assert_eq!(m.code_block(), Style::default());
    assert_eq!(m.base(), Style::default());
    assert_eq!(m.clone().downsampled(), m, "nothing to map");
}

#[test]
fn downsampling_maps_to_the_nearest_of_the_cube_and_the_grey_ramp() {
    assert_eq!(nearest_256(0, 0, 0), 16);
    assert_eq!(nearest_256(255, 255, 255), 231);
    assert_eq!(nearest_256(95, 135, 175), 16 + 36 + 12 + 3);
    assert_eq!(nearest_256(128, 128, 128), 244, "a grey lands on the ramp");
    let t = Theme::nexo().downsampled();
    assert!(t.degraded);
    // what it maps to follows the terminal the tests run in (`TERM=linux` has 16 colours)
    let ansi16 = colour_depth() == Depth::Ansi16;
    for c in t.colours() {
        assert!(
            c == Color::Reset || if ansi16 { !matches!(c, Color::Rgb(..)) } else { matches!(c, Color::Indexed(_)) },
            "{c:?}"
        );
    }
    assert_ne!(t, Theme::nexo());
}

#[test]
fn colours_mix_for_fades() {
    let (a, b) = (Color::Rgb(0, 0, 0), Color::Rgb(200, 100, 50));
    assert_eq!(mix(a, b, 0.0), a);
    assert_eq!(mix(a, b, 1.0), b);
    assert_eq!(mix(a, b, 0.5), Color::Rgb(100, 50, 25));
    assert_eq!(mix(a, b, 7.0), b, "clamped");
    assert_eq!(mix(Color::Indexed(1), Color::Indexed(2), 0.4), Color::Indexed(1), "mapped colours switch half way");
}
