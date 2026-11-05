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
    "dark",
    "nexo",
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
    /// nexo's own colours (Monokai's) on Monokai's warm charcoal ground.
    pub fn nexo() -> Theme {
        Theme {
            name: "nexo",
            description: "the default colours on the Monokai charcoal ground (#272822)",
            bg: rgb(0x272822),
            bg_alt: rgb(0x2f302a),
            code_bg: rgb(0x2f302a),
            highlight_fg: rgb(0x272822),
            diff_add_bg: rgb(0x2f3b1c),
            diff_del_bg: rgb(0x40202b),
            bar_bg: rgb(0x1e1f1a),
            modal_bg: rgb(0x2b2c26),
            ..Theme::dark()
        }
    }

    /// The default: Monokai colours on a pure black ground. White carries the text, purple bold the
    /// headings and strong words, cyan the person, magenta the assistant, green code; orange only warns.
    /// Every colour that carries text reads at 4.5:1 or more on every ground (WCAG AA).
    pub fn dark() -> Theme {
        Theme {
            name: "dark",
            description: "the default: the Monokai colours of an alacritty setup on a pure black ground, AA contrast",
            mono: false,
            bg: rgb(0x000000),
            bg_alt: rgb(0x141414),
            fg: rgb(0xf8f8f2),
            dim: rgb(0xcfcfc7),
            dim2: rgb(0x9e9552),
            accent: rgb(0x66d9ef),
            accent2: rgb(0xae81ff),
            code_fg: rgb(0xa6e22e),
            user: rgb(0x66d9ef),
            assistant: rgb(0xf92690),
            success: rgb(0xa6e22e),
            error: rgb(0xff0000),
            warn: rgb(0xe6db74),
            warn2: rgb(0xfd971f),
            info: rgb(0x66d9ef),
            border: rgb(0x414868),
            code_bg: rgb(0x141414),
            selection_bg: rgb(0x4b6b3e),
            highlight_bg: rgb(0xe6db74),
            highlight_fg: rgb(0x000000),
            diff_add: rgb(0xa6e22e),
            diff_del: rgb(0xf92690),
            diff_add_bg: rgb(0x16240a),
            diff_del_bg: rgb(0x2e0a16),
            hunk: rgb(0xae81ff),
            bar_bg: rgb(0x0c0c0c),
            bar_fg: rgb(0xf8f8f2),
            modal_bg: rgb(0x0e0e0e),
            syntax: Some("Monokai Extended"),
            degraded: false,
        }
    }

    pub fn light() -> Theme {
        Theme {
            name: "light",
            description: "a light ground with dark text",
            mono: false,
            bg: rgb(0xfafafa),
            bg_alt: rgb(0xf1f5f9),
            fg: rgb(0x1f2937),
            dim: rgb(0x4b5563),
            dim2: rgb(0x6b7280),
            accent: rgb(0x0f766e),
            accent2: rgb(0x1d4ed8),
            code_fg: rgb(0x9a3412),
            user: rgb(0x0f766e),
            assistant: rgb(0xb45309),
            success: rgb(0x15803d),
            error: rgb(0xb91c1c),
            warn: rgb(0xa16207),
            warn2: rgb(0xc2410c),
            info: rgb(0x0369a1),
            border: rgb(0x94a3b8),
            code_bg: rgb(0xf1f5f9),
            selection_bg: rgb(0xbae0dc),
            highlight_bg: rgb(0xfde68a),
            highlight_fg: rgb(0x111827),
            diff_add: rgb(0x15803d),
            diff_del: rgb(0xb91c1c),
            diff_add_bg: rgb(0xe6f4ea),
            diff_del_bg: rgb(0xfbe9e9),
            hunk: rgb(0x0369a1),
            bar_bg: rgb(0xe2e8f0),
            bar_fg: rgb(0x1e293b),
            modal_bg: rgb(0xf8fafc),
            syntax: Some("InspiredGitHub"),
            degraded: false,
        }
    }

    /// No colour at all: the terminal's own foreground and background, with
    /// bold, dim and reverse for structure.
    pub fn mono() -> Theme {
        Theme {
            name: "mono",
            description: "no colour (also when NO_COLOR is set)",
            mono: true,
            bg: Color::Reset,
            bg_alt: Color::Reset,
            fg: Color::Reset,
            dim: Color::Reset,
            dim2: Color::Reset,
            accent: Color::Reset,
            accent2: Color::Reset,
            code_fg: Color::Reset,
            user: Color::Reset,
            assistant: Color::Reset,
            success: Color::Reset,
            error: Color::Reset,
            warn: Color::Reset,
            warn2: Color::Reset,
            info: Color::Reset,
            border: Color::Reset,
            code_bg: Color::Reset,
            selection_bg: Color::Reset,
            highlight_bg: Color::Reset,
            highlight_fg: Color::Reset,
            diff_add: Color::Reset,
            diff_del: Color::Reset,
            diff_add_bg: Color::Reset,
            diff_del_bg: Color::Reset,
            hunk: Color::Reset,
            bar_bg: Color::Reset,
            bar_fg: Color::Reset,
            modal_bg: Color::Reset,
            syntax: None,
            degraded: false,
        }
    }

    pub fn catppuccin_mocha() -> Theme {
        Theme {
            name: "catppuccin-mocha",
            description: "Catppuccin Mocha",
            mono: false,
            bg: rgb(0x1e1e2e),
            bg_alt: rgb(0x181825),
            fg: rgb(0xcdd6f4),
            dim: rgb(0x7f849c),
            dim2: rgb(0x585b70),
            accent: rgb(0x94e2d5),
            accent2: rgb(0x89b4fa),
            code_fg: rgb(0xfab387),
            user: rgb(0x94e2d5),
            assistant: rgb(0xf5c2e7),
            success: rgb(0xa6e3a1),
            error: rgb(0xf38ba8),
            warn: rgb(0xf9e2af),
            warn2: rgb(0xfab387),
            info: rgb(0x89dceb),
            border: rgb(0x45475a),
            code_bg: rgb(0x181825),
            selection_bg: rgb(0x45475a),
            highlight_bg: rgb(0xf9e2af),
            highlight_fg: rgb(0x1e1e2e),
            diff_add: rgb(0xa6e3a1),
            diff_del: rgb(0xf38ba8),
            diff_add_bg: rgb(0x26332e),
            diff_del_bg: rgb(0x3a2532),
            hunk: rgb(0x89dceb),
            bar_bg: rgb(0x11111b),
            bar_fg: rgb(0xcdd6f4),
            modal_bg: rgb(0x181825),
            syntax: Some("base16-mocha.dark"),
            degraded: false,
        }
    }

    pub fn gruvbox() -> Theme {
        Theme {
            name: "gruvbox",
            description: "Gruvbox dark",
            mono: false,
            bg: rgb(0x282828),
            bg_alt: rgb(0x1d2021),
            fg: rgb(0xebdbb2),
            dim: rgb(0x928374),
            dim2: rgb(0x665c54),
            accent: rgb(0x8ec07c),
            accent2: rgb(0x83a598),
            code_fg: rgb(0xfe8019),
            user: rgb(0x8ec07c),
            assistant: rgb(0xfabd2f),
            success: rgb(0xb8bb26),
            error: rgb(0xfb4934),
            warn: rgb(0xfabd2f),
            warn2: rgb(0xfe8019),
            info: rgb(0x83a598),
            border: rgb(0x504945),
            code_bg: rgb(0x1d2021),
            selection_bg: rgb(0x504945),
            highlight_bg: rgb(0xfabd2f),
            highlight_fg: rgb(0x282828),
            diff_add: rgb(0xb8bb26),
            diff_del: rgb(0xfb4934),
            diff_add_bg: rgb(0x2e3320),
            diff_del_bg: rgb(0x3c2422),
            hunk: rgb(0x83a598),
            bar_bg: rgb(0x1d2021),
            bar_fg: rgb(0xebdbb2),
            modal_bg: rgb(0x1d2021),
            syntax: Some("base16-eighties.dark"),
            degraded: false,
        }
    }

    pub fn nord() -> Theme {
        Theme {
            name: "nord",
            description: "Nord",
            mono: false,
            bg: rgb(0x2e3440),
            bg_alt: rgb(0x3b4252),
            fg: rgb(0xd8dee9),
            dim: rgb(0x7b88a1),
            dim2: rgb(0x4c566a),
            accent: rgb(0x88c0d0),
            accent2: rgb(0x81a1c1),
            code_fg: rgb(0xd08770),
            user: rgb(0x88c0d0),
            assistant: rgb(0xebcb8b),
            success: rgb(0xa3be8c),
            error: rgb(0xbf616a),
            warn: rgb(0xebcb8b),
            warn2: rgb(0xd08770),
            info: rgb(0x8fbcbb),
            border: rgb(0x434c5e),
            code_bg: rgb(0x3b4252),
            selection_bg: rgb(0x434c5e),
            highlight_bg: rgb(0xebcb8b),
            highlight_fg: rgb(0x2e3440),
            diff_add: rgb(0xa3be8c),
            diff_del: rgb(0xbf616a),
            diff_add_bg: rgb(0x343f3a),
            diff_del_bg: rgb(0x3f3339),
            hunk: rgb(0x8fbcbb),
            bar_bg: rgb(0x272c36),
            bar_fg: rgb(0xd8dee9),
            modal_bg: rgb(0x3b4252),
            syntax: Some("base16-ocean.dark"),
            degraded: false,
        }
    }

    pub fn dracula() -> Theme {
        Theme {
            name: "dracula",
            description: "Dracula",
            mono: false,
            bg: rgb(0x282a36),
            bg_alt: rgb(0x21222c),
            fg: rgb(0xf8f8f2),
            dim: rgb(0x6272a4),
            dim2: rgb(0x44475a),
            accent: rgb(0x8be9fd),
            accent2: rgb(0xbd93f9),
            code_fg: rgb(0xffb86c),
            user: rgb(0x8be9fd),
            assistant: rgb(0xff79c6),
            success: rgb(0x50fa7b),
            error: rgb(0xff5555),
            warn: rgb(0xf1fa8c),
            warn2: rgb(0xffb86c),
            info: rgb(0x8be9fd),
            border: rgb(0x44475a),
            code_bg: rgb(0x21222c),
            selection_bg: rgb(0x44475a),
            highlight_bg: rgb(0xf1fa8c),
            highlight_fg: rgb(0x282a36),
            diff_add: rgb(0x50fa7b),
            diff_del: rgb(0xff5555),
            diff_add_bg: rgb(0x243b2f),
            diff_del_bg: rgb(0x40292e),
            hunk: rgb(0xbd93f9),
            bar_bg: rgb(0x191a21),
            bar_fg: rgb(0xf8f8f2),
            modal_bg: rgb(0x21222c),
            syntax: Some("base16-mocha.dark"),
            degraded: false,
        }
    }

    pub fn solarized_dark() -> Theme {
        Theme {
            name: "solarized-dark",
            description: "Solarized dark",
            mono: false,
            bg: rgb(0x002b36),
            bg_alt: rgb(0x073642),
            fg: rgb(0x93a1a1),
            dim: rgb(0x839496),
            dim2: rgb(0x586e75),
            accent: rgb(0x2aa198),
            accent2: rgb(0x268bd2),
            code_fg: rgb(0xcb4b16),
            user: rgb(0x2aa198),
            assistant: rgb(0xb58900),
            success: rgb(0x859900),
            error: rgb(0xdc322f),
            warn: rgb(0xb58900),
            warn2: rgb(0xcb4b16),
            info: rgb(0x268bd2),
            border: rgb(0x0e4452),
            code_bg: rgb(0x073642),
            selection_bg: rgb(0x0e4452),
            highlight_bg: rgb(0xb58900),
            highlight_fg: rgb(0x002b36),
            diff_add: rgb(0x859900),
            diff_del: rgb(0xdc322f),
            diff_add_bg: rgb(0x0e3a30),
            diff_del_bg: rgb(0x3a2a30),
            hunk: rgb(0x268bd2),
            bar_bg: rgb(0x00212b),
            bar_fg: rgb(0x93a1a1),
            modal_bg: rgb(0x073642),
            syntax: Some("Solarized (dark)"),
            degraded: false,
        }
    }

    pub fn tokyo_night() -> Theme {
        Theme {
            name: "tokyo-night",
            description: "Tokyo Night",
            mono: false,
            bg: rgb(0x1a1b26),
            bg_alt: rgb(0x16161e),
            fg: rgb(0xc0caf5),
            dim: rgb(0x737aa2),
            dim2: rgb(0x565f89),
            accent: rgb(0x7dcfff),
            accent2: rgb(0x7aa2f7),
            code_fg: rgb(0xff9e64),
            user: rgb(0x7dcfff),
            assistant: rgb(0xbb9af7),
            success: rgb(0x9ece6a),
            error: rgb(0xf7768e),
            warn: rgb(0xe0af68),
            warn2: rgb(0xff9e64),
            info: rgb(0x2ac3de),
            border: rgb(0x3b4261),
            code_bg: rgb(0x16161e),
            selection_bg: rgb(0x33467c),
            highlight_bg: rgb(0xe0af68),
            highlight_fg: rgb(0x1a1b26),
            diff_add: rgb(0x9ece6a),
            diff_del: rgb(0xf7768e),
            diff_add_bg: rgb(0x20303b),
            diff_del_bg: rgb(0x37222c),
            hunk: rgb(0x2ac3de),
            bar_bg: rgb(0x101018),
            bar_fg: rgb(0xc0caf5),
            modal_bg: rgb(0x16161e),
            syntax: Some("base16-ocean.dark"),
            degraded: false,
        }
    }

    pub fn by_name(name: &str) -> Option<Theme> {
        match name.trim().to_ascii_lowercase().replace('_', "-").as_str() {
            "dark" | "default" | "" | "ansi" => Some(Theme::dark()),
            "nexo" => Some(Theme::nexo()),
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

    /// The theme asked for (`auto`: `light` on a light terminal background, else `dark`;
    /// `dark` when unknown), unless `NO_COLOR` is set;
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
                Theme::dark()
            }
        } else {
            Theme::by_name(name).unwrap_or_else(Theme::dark)
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

    fn colours_mut(&mut self) -> [&mut Color; 28] {
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

#[cfg(test)]
mod tests {
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
        assert_eq!(Theme::by_name("").unwrap().name, "dark");
        assert_eq!(Theme::pick("no-such-theme").name, "dark");
    }

    #[test]
    fn every_colour_of_every_coloured_theme_is_an_explicit_rgb_value() {
        for n in NAMES.iter().filter(|n| **n != "mono") {
            let t = Theme::by_name(n).unwrap();
            for c in t.colours() {
                assert!(matches!(c, Color::Rgb(..)), "{n}: {c:?} is not RGB");
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
        let names = [
            "dark",
            "nexo",
            "light",
            "catppuccin-mocha",
            "gruvbox",
            "nord",
            "dracula",
            "solarized-dark",
            "tokyo-night",
        ];
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
        for c in t.colours() {
            assert!(matches!(c, Color::Indexed(_)), "{c:?}");
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
}
