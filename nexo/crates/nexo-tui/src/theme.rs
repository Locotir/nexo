//! Colours, forced: every theme is a complete set of explicit RGB values,
//! background included, and the interface paints its own background, so nexo
//! looks the same in any terminal whatever palette it has configured. Only
//! `mono` (and `NO_COLOR`) leaves the terminal's colours alone. On a terminal
//! without 24-bit colour every value is mapped to the nearest of the 256-colour
//! cube.

use ratatui::style::{Color, Modifier, Style};

#[derive(Debug, Clone, PartialEq)]
pub struct Theme {
    pub name: &'static str,
    pub description: &'static str,
    pub mono: bool,
    /// The whole frame's background.
    pub bg: Color,
    /// One step from `bg`: code blocks, cards, the palette.
    pub bg_alt: Color,
    /// Ordinary text.
    pub fg: Color,
    /// Chrome and hints.
    pub dim: Color,
    /// Fainter still: separators, folded counts.
    pub dim2: Color,
    /// The person's bar, the prompt caret, links, markers.
    pub accent: Color,
    /// Headings.
    pub accent2: Color,
    /// Inline code.
    pub code_fg: Color,
    pub user: Color,
    pub assistant: Color,
    pub success: Color,
    /// Money: costs and budgets.
    pub money: Color,
    pub error: Color,
    pub warn: Color,
    /// Between warn and error: the gauge at 75-90%.
    pub warn2: Color,
    pub info: Color,
    pub border: Color,
    /// Code blocks and inline code (normally `bg_alt`).
    pub code_bg: Color,
    pub selection_bg: Color,
    pub highlight_bg: Color,
    pub highlight_fg: Color,
    pub diff_add: Color,
    pub diff_del: Color,
    pub diff_add_bg: Color,
    pub diff_del_bg: Color,
    pub hunk: Color,
    pub bar_bg: Color,
    pub bar_fg: Color,
    pub modal_bg: Color,
    /// The syntect theme used for code (None: no highlighting).
    pub syntax: Option<&'static str>,
    /// The values were mapped to the 256-colour cube (no 24-bit colour here).
    pub degraded: bool,
}

pub const NAMES: &[&str] = &[
    "nexo",
    "dark",
    "light",
    "mono",
    "catppuccin-mocha",
    "gruvbox",
    "nord",
    "dracula",
    "solarized-dark",
    "tokyo-night",
];

/// `a` moved `t` (0..1) of the way to `b`: the steps of a fade or a shimmer.
/// Colours that are not RGB (a mapped theme) switch half way.
pub fn mix(a: Color, b: Color, t: f32) -> Color {
    let t = t.clamp(0.0, 1.0);
    match (a, b) {
        (Color::Rgb(r1, g1, b1), Color::Rgb(r2, g2, b2)) => {
            let m = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round() as u8;
            Color::Rgb(m(r1, r2), m(g1, g2), m(b1, b2))
        }
        _ => {
            if t < 0.5 {
                a
            } else {
                b
            }
        }
    }
}

fn rgb(hex: u32) -> Color {
    Color::Rgb(((hex >> 16) & 0xff) as u8, ((hex >> 8) & 0xff) as u8, (hex & 0xff) as u8)
}

impl Theme {
    pub fn by_name(name: &str) -> Option<Theme> {
        match name.trim().to_ascii_lowercase().replace('_', "-").as_str() {
            "nexo" | "default" | "" => Some(Theme::nexo()),
            "dark" | "ansi" => Some(Theme::dark()),
            "light" => Some(Theme::light()),
            "mono" | "none" | "plain" => Some(Theme::mono()),
            "catppuccin-mocha" | "catppuccin" | "mocha" => Some(Theme::catppuccin_mocha()),
            "gruvbox" | "gruvbox-dark" => Some(Theme::gruvbox()),
            "nord" => Some(Theme::nord()),
            "dracula" => Some(Theme::dracula()),
            "solarized-dark" | "solarized" => Some(Theme::solarized_dark()),
            "tokyo-night" | "tokyonight" => Some(Theme::tokyo_night()),
            _ => None,
        }
    }

    /// The theme asked for (`auto`: `light` on a light terminal background, else `nexo`;
    /// `nexo` when unknown), unless `NO_COLOR` is set;
    /// mapped to the 256-colour cube when the terminal has no 24-bit colour.
    pub fn pick(name: &str) -> Theme {
        if std::env::var_os("NO_COLOR").is_some_and(|v| !v.is_empty()) {
            return Theme::mono();
        }
        let t = if name.trim().eq_ignore_ascii_case("auto") {
            let v = std::env::var("COLORFGBG").ok();
            if light_background(v.as_deref()) {
                Theme::light()
            } else {
                Theme::nexo()
            }
        } else {
            Theme::by_name(name).unwrap_or_else(Theme::nexo)
        };
        if truecolor_terminal() {
            t
        } else {
            t.downsampled()
        }
    }
    /// The colours of the four kinds of message (your messages, replies, tool calls, tool output) in the
    /// context map and chart. Yours and the replies keep their chat colours; the calls take a warm colour
    /// of the theme and the output a cool one, the pair that keeps the four farthest apart to the eye
    /// (OKLab, on the full colours even when the terminal shows fewer), so no two kinds read alike:
    /// several themes give the replies the colour of `warn`, or yours the colour of `info`. Four colours
    /// the terminal shows apart come first; the error, accent and dim colours only stand in when no warm
    /// and cool pair gives four (on 16 colours the warm ones of a theme can all fall on one yellow).
    pub fn kind_colours(&self) -> [Color; 4] {
        let full = if self.degraded { Theme::by_name(self.name) } else { None };
        let src = full.as_ref().unwrap_or(self);
        let sets = |t: &Theme| {
            let warm = [t.warn, t.warn2, t.code_fg, t.error];
            let cool = [t.info, t.accent2, t.success, t.accent, t.dim];
            let (user, assistant) = (t.user, t.assistant);
            (0..20).map(move |k| [user, assistant, warm[k / 5], cool[k % 5]])
        };
        let mut best: Option<(usize, (bool, bool), f64)> = None;
        for (k, (seen, shown)) in sets(src).zip(sets(self)).enumerate() {
            let rank = ((0..4).all(|i| (i + 1..4).all(|j| shown[i] != shown[j])), k / 5 < 3 && k % 5 < 3);
            let gap = min_gap(&seen).unwrap_or(0.0);
            // a pair only takes the place of an earlier one of the same rank when clearly farther apart
            let better = best.is_none_or(|(_, r, g)| rank > r || (rank == r && gap > g + 0.02));
            if better {
                best = Some((k, rank, gap));
            }
        }
        sets(self).nth(best.map_or(0, |b| b.0)).unwrap_or([self.user, self.assistant, self.warn, self.info])
    }
    /// Every colour mapped to the nearest of the 256-colour cube (for a terminal without 24-bit colour).
    pub fn downsampled(mut self) -> Theme {
        if self.mono {
            return self;
        }
        let depth = colour_depth();
        for c in self.colours_mut() {
            *c = if depth == Depth::Ansi16 { to_ansi16(*c) } else { to_indexed(*c) };
        }
        self.degraded = true;
        self
    }

    fn colours_mut(&mut self) -> [&mut Color; 29] {
        [
            &mut self.bg,
            &mut self.bg_alt,
            &mut self.fg,
            &mut self.dim,
            &mut self.dim2,
            &mut self.accent,
            &mut self.accent2,
            &mut self.code_fg,
            &mut self.user,
            &mut self.assistant,
            &mut self.success,
            &mut self.money,
            &mut self.error,
            &mut self.warn,
            &mut self.warn2,
            &mut self.info,
            &mut self.border,
            &mut self.code_bg,
            &mut self.selection_bg,
            &mut self.highlight_bg,
            &mut self.highlight_fg,
            &mut self.diff_add,
            &mut self.diff_del,
            &mut self.diff_add_bg,
            &mut self.diff_del_bg,
            &mut self.hunk,
            &mut self.bar_bg,
            &mut self.bar_fg,
            &mut self.modal_bg,
        ]
    }

    /// Every colour of the theme (tests: all of them must be explicit).
    pub fn colours(&self) -> Vec<Color> {
        let mut t = self.clone();
        t.colours_mut().into_iter().map(|c| *c).collect()
    }

    /// The ground of the whole frame: the theme's text on the theme's background.
    pub fn base(&self) -> Style {
        if self.mono {
            Style::default()
        } else {
            Style::default().fg(self.fg).bg(self.bg)
        }
    }

    /// The ground of a card or a code block.
    pub fn alt(&self) -> Style {
        if self.mono {
            Style::default()
        } else {
            Style::default().fg(self.fg).bg(self.bg_alt)
        }
    }

    pub fn style(&self, c: Color) -> Style {
        Style::default().fg(c)
    }

    pub fn dim(&self) -> Style {
        if self.mono {
            Style::default().add_modifier(Modifier::DIM)
        } else {
            Style::default().fg(self.dim)
        }
    }

    pub fn dim2(&self) -> Style {
        if self.mono {
            Style::default().add_modifier(Modifier::DIM)
        } else {
            Style::default().fg(self.dim2)
        }
    }

    /// The colour of a share of a limit: calm below a half, then yellow,
    /// orange from three quarters, red from nine tenths.
    pub fn level_color(&self, pct: u64) -> Color {
        match pct {
            0..=49 => self.success,
            50..=74 => self.warn,
            75..=89 => self.warn2,
            _ => self.error,
        }
    }

    pub fn bold(&self, c: Color) -> Style {
        Style::default().fg(c).add_modifier(Modifier::BOLD)
    }

    pub fn bar(&self) -> Style {
        if self.mono {
            Style::default().add_modifier(Modifier::REVERSED)
        } else {
            Style::default().fg(self.bar_fg).bg(self.bar_bg)
        }
    }

    /// The background of the person's own messages: a faint wash of their colour, so they are
    /// found at a glance in a long conversation. None without colour.
    pub fn user_band(&self) -> Style {
        if self.mono {
            return Style::default();
        }
        let tint = match (self.bg, self.user) {
            (Color::Rgb(..), Color::Rgb(..)) => mix(self.bg, self.user, 0.16),
            _ => self.bg_alt,
        };
        Style::default().bg(tint)
    }

    /// The row the cursor is on in a list of choices: a band across the row, so it is seen at a glance.
    pub fn chosen(&self) -> Style {
        if self.mono {
            Style::default().add_modifier(Modifier::REVERSED | Modifier::BOLD)
        } else {
            Style::default().fg(self.accent).bg(self.selection_bg).add_modifier(Modifier::BOLD)
        }
    }

    pub fn highlight(&self) -> Style {
        if self.mono {
            Style::default().add_modifier(Modifier::REVERSED)
        } else {
            Style::default().fg(self.highlight_fg).bg(self.highlight_bg)
        }
    }

    pub fn selection(&self) -> Style {
        if self.mono {
            Style::default().add_modifier(Modifier::REVERSED)
        } else {
            Style::default().bg(self.selection_bg)
        }
    }

    /// What is under the pointer and answers a click: lighter than the selection.
    pub fn hover(&self) -> Style {
        if self.mono {
            Style::default().add_modifier(Modifier::UNDERLINED)
        } else {
            Style::default().bg(mix(self.bg_alt, self.selection_bg, 0.5))
        }
    }

    pub fn code_block(&self) -> Style {
        if self.mono {
            Style::default()
        } else {
            Style::default().bg(self.code_bg)
        }
    }

    /// Inline code: tinted, on the code background.
    pub fn code_inline(&self) -> Style {
        if self.mono {
            Style::default().add_modifier(Modifier::BOLD)
        } else {
            self.code_block().fg(self.code_fg)
        }
    }

    pub fn heading(&self) -> Style {
        if self.mono {
            Style::default().add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(self.accent2).add_modifier(Modifier::BOLD)
        }
    }

    /// An added line of a diff: the colour, on a faint tint.
    pub fn diff_add_line(&self) -> Style {
        if self.mono {
            Style::default().add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(self.diff_add).bg(self.diff_add_bg)
        }
    }

    pub fn diff_del_line(&self) -> Style {
        if self.mono {
            Style::default().add_modifier(Modifier::DIM)
        } else {
            Style::default().fg(self.diff_del).bg(self.diff_del_bg)
        }
    }

    /// A pending, running and done task of the todo list.
    pub fn todo_pending(&self) -> Style {
        Style::default()
    }

    pub fn todo_running(&self) -> Style {
        if self.mono {
            Style::default().add_modifier(Modifier::BOLD)
        } else {
            self.bold(self.accent)
        }
    }

    pub fn todo_done(&self) -> Style {
        self.dim().add_modifier(Modifier::CROSSED_OUT)
    }

    /// The ground of the row being worked on, `t` (0..1) of the way from the dialog to the accent.
    pub fn todo_now_bg(&self, t: f32) -> Style {
        if self.mono {
            return Style::default();
        }
        Style::default().bg(crate::motion::fade(self, self.modal_bg, self.accent, 0.10 + 0.08 * t))
    }

    /// The small header over a group of tasks.
    pub fn todo_head(&self) -> Style {
        self.dim().add_modifier(Modifier::BOLD)
    }
}

/// Whether the terminal takes 24-bit colour: `NEXO_COLORS` wins; then `COLORTERM` says so, or `TERM`
/// names a 256-colour terminal (those take it too in practice), or it is a Windows console (no `TERM`:
/// Windows Terminal and the console of Windows 10 and later all take 24-bit colour and set neither).
pub fn truecolor_terminal() -> bool {
    match std::env::var("NEXO_COLORS").unwrap_or_default().trim().to_ascii_lowercase().as_str() {
        "16" | "ansi" | "256" => return false,
        "truecolor" | "24bit" => return true,
        _ => {}
    }
    let colorterm = std::env::var("COLORTERM").unwrap_or_default().to_ascii_lowercase();
    if colorterm.contains("truecolor") || colorterm.contains("24bit") {
        return true;
    }
    let term = std::env::var("TERM").unwrap_or_default();
    term.contains("256color") || (cfg!(windows) && term.is_empty())
}

/// How many colours the terminal takes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Depth {
    True,
    Indexed256,
    Ansi16,
}

/// The depth from the environment: `NEXO_COLORS` (`16`, `256`, `truecolor`) wins; then
/// `COLORTERM`; then `TERM` (a name with `256` is 256 colours, anything else is 16), where none on
/// `windows` is a Windows console: those take 24-bit colour and set neither.
pub fn depth_from(forced: &str, colorterm: &str, term: &str, windows: bool) -> Depth {
    match forced.trim().to_ascii_lowercase().as_str() {
        "16" | "ansi" => return Depth::Ansi16,
        "256" => return Depth::Indexed256,
        "truecolor" | "24bit" => return Depth::True,
        _ => {}
    }
    let ct = colorterm.to_ascii_lowercase();
    if ct.contains("truecolor") || ct.contains("24bit") || (windows && term.is_empty()) {
        Depth::True
    } else if term.contains("256") {
        Depth::Indexed256
    } else {
        Depth::Ansi16
    }
}

pub fn colour_depth() -> Depth {
    let get = |k: &str| std::env::var(k).unwrap_or_default();
    depth_from(&get("NEXO_COLORS"), &get("COLORTERM"), &get("TERM"), cfg!(windows))
}

/// A colour in OKLab (lightness and two opposed hue axes), where distance follows what the eye sees;
/// `None` for a colour that is not 24-bit.
fn oklab(c: Color) -> Option<[f64; 3]> {
    let Color::Rgb(r, g, b) = c else { return None };
    let lin = |v: u8| {
        let v = v as f64 / 255.0;
        if v <= 0.04045 {
            v / 12.92
        } else {
            ((v + 0.055) / 1.055).powf(2.4)
        }
    };
    let (r, g, b) = (lin(r), lin(g), lin(b));
    let l = (0.4122214708 * r + 0.5363325363 * g + 0.0514459929 * b).cbrt();
    let m = (0.2119034982 * r + 0.6806995451 * g + 0.1073969566 * b).cbrt();
    let s = (0.0883024619 * r + 0.2817188376 * g + 0.6299787005 * b).cbrt();
    Some([
        0.2104542553 * l + 0.7936177850 * m - 0.0040720468 * s,
        1.9779984951 * l - 2.4285922050 * m + 0.4505937099 * s,
        0.0259040371 * l + 0.7827717662 * m - 0.8086757660 * s,
    ])
}

/// The smallest OKLab distance between two of `cs`; `None` when one is not 24-bit.
fn min_gap(cs: &[Color]) -> Option<f64> {
    let labs = cs.iter().map(|&c| oklab(c)).collect::<Option<Vec<_>>>()?;
    let mut gap = f64::INFINITY;
    for (i, a) in labs.iter().enumerate() {
        for b in &labs[i + 1..] {
            gap = gap.min(a.iter().zip(b).map(|(x, y)| (x - y).powi(2)).sum::<f64>().sqrt());
        }
    }
    Some(gap)
}
/// The standard colour of the same hue as an RGB value, so a pastel keeps its colour on a
/// 16-colour terminal (by distance alone light pastels all fall on grey): little colour goes
/// to the grey ramp by lightness; the rest to the hue's colour, its light variant when light.
pub fn nearest_16(r: u8, g: u8, b: u8) -> Color {
    let (r, g, b) = (r as f32 / 255.0, g as f32 / 255.0, b as f32 / 255.0);
    let (max, min) = (r.max(g).max(b), r.min(g).min(b));
    let (chroma, light) = (max - min, (max + min) / 2.0);
    if chroma < 40.0 / 255.0 || chroma < 0.3 * max {
        return match light {
            l if l < 0.25 => Color::Black,
            l if l < 0.62 => Color::DarkGray,
            l if l < 0.85 => Color::Gray,
            _ => Color::White,
        };
    }
    let hue = if max == r {
        60.0 * ((g - b) / chroma).rem_euclid(6.0)
    } else if max == g {
        60.0 * ((b - r) / chroma + 2.0)
    } else {
        60.0 * ((r - g) / chroma + 4.0)
    };
    let bright = light > 0.6;
    let pick = |dark: Color, light: Color| if bright { light } else { dark };
    match hue {
        h if !(25.0..345.0).contains(&h) => pick(Color::Red, Color::LightRed),
        h if h < 90.0 => pick(Color::Yellow, Color::LightYellow),
        h if h < 150.0 => pick(Color::Green, Color::LightGreen),
        h if h < 210.0 => pick(Color::Cyan, Color::LightCyan),
        h if h < 270.0 => pick(Color::Blue, Color::LightBlue),
        _ => pick(Color::Magenta, Color::LightMagenta),
    }
}

fn to_ansi16(c: Color) -> Color {
    match c {
        Color::Rgb(r, g, b) => nearest_16(r, g, b),
        other => other,
    }
}

/// The nearest of the 256-colour cube (the 6×6×6 cube and the grey ramp) to an RGB value.
pub fn nearest_256(r: u8, g: u8, b: u8) -> u8 {
    const LEVELS: [u8; 6] = [0, 95, 135, 175, 215, 255];
    let dist = |x: (u8, u8, u8)| {
        let d = |a: u8, b: u8| (a as i32 - b as i32).pow(2);
        d(x.0, r) + d(x.1, g) + d(x.2, b)
    };
    let mut best = (i32::MAX, 16u8);
    for (ri, &rv) in LEVELS.iter().enumerate() {
        for (gi, &gv) in LEVELS.iter().enumerate() {
            for (bi, &bv) in LEVELS.iter().enumerate() {
                let d = dist((rv, gv, bv));
                if d < best.0 {
                    best = (d, 16 + 36 * ri as u8 + 6 * gi as u8 + bi as u8);
                }
            }
        }
    }
    for i in 0..24u8 {
        let v = 8 + 10 * i;
        let d = dist((v, v, v));
        if d < best.0 {
            best = (d, 232 + i);
        }
    }
    best.1
}

fn to_indexed(c: Color) -> Color {
    match c {
        Color::Rgb(r, g, b) => Color::Indexed(nearest_256(r, g, b)),
        other => other,
    }
}

/// True when `COLORFGBG` ("fg;bg", set by many terminals) names a light background (7 or 15).
pub fn light_background(colorfgbg: Option<&str>) -> bool {
    colorfgbg.and_then(|v| v.rsplit(';').next()).is_some_and(|bg| matches!(bg.trim(), "7" | "15"))
}

mod palettes;

#[cfg(test)]
mod tests;
