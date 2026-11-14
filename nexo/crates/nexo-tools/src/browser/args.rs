//! Reading one call: the `Action` it asks for, its arguments and why a malformed one is refused.

use super::*;

/// One call, read from the arguments. Borrows from them.
#[derive(Debug, PartialEq)]
pub(super) enum Action<'a> {
    /// Starts the browser (or restarts it) the way asked, then goes to the address.
    Open(Option<&'a str>, Want),
    Navigate(&'a str),
    Snapshot,
    ReadPage(ReadPageArgs<'a>),
    ReadText(Option<&'a str>),
    Click(&'a str),
    Fill(&'a str, &'a str),
    FillBatch(Vec<(&'a str, &'a str)>),
    Select(&'a str, &'a str),
    SetCheckbox(&'a str, bool),
    Press(&'a str),
    Hover(&'a str),
    Scroll(&'a str),
    Wait(Option<&'a str>, Option<u64>),
    Screenshot(bool),
    Back,
    Forward,
    Reload,
    Eval(&'a str),
    Console(Option<&'a str>),
    /// A regex filter, or the number of one request to show in full.
    Network(Option<&'a str>, Option<u64>),
    /// Elements by what a person would call them.
    Find(&'a str),
    Tabs,
    TabNew(&'a str),
    TabSwitch(usize),
    TabClose(usize),
    Upload(&'a str, &'a str),
    Resize(u32, u32),
    ClickXy(f64, f64, Modifiers),
    DoubleClick(f64, f64, Modifiers),
    RightClick(f64, f64, Modifiers),
    /// From x1,y1 to x2,y2.
    Drag([f64; 4], Modifiers),
    HoverXy(f64, f64),
    /// At x,y by dx,dy.
    ScrollXy([f64; 4]),
    TypeText(&'a str),
    /// start_recording, stop_recording, export_gif, clear_recording.
    Gif(gif::GifCall<'a>),
    /// Steps in order, each with the name of its action.
    Batch(Vec<(&'a str, Action<'a>)>),
    Close,
    /// The browsers there are to drive, and which is in use.
    Browsers,
    /// Use the browser with this id (the person's own, or `headless`).
    SelectBrowser(&'a str),
    /// Ask every running browser of the person's at once.
    SwitchBrowser,
}

/// The arguments of `read_page`; what they leave out the browser's defaults fill in.
#[derive(Debug, PartialEq, Default)]
pub(super) struct ReadPageArgs<'a> {
    pub(super) interactive: bool,
    pub(super) depth: Option<usize>,
    pub(super) ref_id: Option<&'a str>,
    pub(super) max_chars: Option<usize>,
}

impl ReadPageArgs<'_> {
    pub(super) fn options(&self) -> ReadPageOptions {
        let mut o = ReadPageOptions::default();
        if self.interactive {
            o.filter = Filter::Interactive;
        }
        o.depth = self.depth.unwrap_or(o.depth);
        o.max_chars = self.max_chars.unwrap_or(o.max_chars);
        o.ref_id = self.ref_id.map(str::to_string);
        o
    }
}

pub(super) const ACTIONS: &str = "open, navigate, snapshot, read_page, read_text, click, fill, fill_batch, select, set_checkbox, press, hover, scroll, \
                       wait, screenshot, back, forward, reload, eval, console, network, tabs, tab_new, tab_switch, tab_close, \
                       upload, resize, find, click_xy, double_click, right_click, drag, hover_xy, scroll_xy, type_text, start_recording, stop_recording, export_gif, clear_recording, batch, close, browsers, select_browser, switch_browser";

/// A required text argument; the error says which action needs it.
pub(super) fn need<'a>(args: &'a Value, key: &str, action: &str) -> Result<&'a str, String> {
    str_arg(args, key).ok_or_else(|| format!("browser {action}: `{key}` is required"))
}

/// A `value` may be empty (clearing a field), so it is not trimmed or refused when empty.
pub(super) fn value_arg<'a>(v: &'a Value, action: &str) -> Result<&'a str, String> {
    match v.get("value") {
        Some(Value::String(s)) => Ok(s.as_str()),
        Some(Value::Number(n)) => Err(format!("browser {action}: `value` must be a string, not {n}")),
        _ => Err(format!("browser {action}: `value` is required (use \"\" to clear a field)")),
    }
}

/// Exactly `N` finite numbers in one text: `1280x720`, `120,300` or `10,20 30,40`.
pub(super) fn numbers<const N: usize>(text: &str) -> Option<[f64; N]> {
    let parts: Vec<&str> =
        text.split(|c: char| c == ',' || c == 'x' || c == 'X' || c.is_whitespace()).filter(|p| !p.is_empty()).collect();
    if parts.len() != N {
        return None;
    }
    let mut out = [0.0; N];
    for (slot, part) in out.iter_mut().zip(parts) {
        *slot = part.parse().ok().filter(|n: &f64| n.is_finite())?;
    }
    Some(out)
}

/// Two numbers in one text: `1280x720` or `120,300`.
pub(super) fn pair(text: &str) -> Option<(f64, f64)> {
    numbers::<2>(text).map(|[a, b]| (a, b))
}

/// The point (`to`) of a mouse action, and the modifier keys held (`modifiers`).
pub(super) fn point_args(args: &Value, action: &str) -> Result<(f64, f64, Modifiers), String> {
    let (x, y) = pair(need(args, "to", action)?)
        .ok_or_else(|| format!("browser {action}: `to` must be X,Y in pixels, e.g. 120,300"))?;
    Ok((x, y, modifiers_arg(args, action)?))
}

pub(super) fn modifiers_arg(args: &Value, action: &str) -> Result<Modifiers, String> {
    Modifiers::parse(str_arg(args, "modifiers").unwrap_or("")).map_err(|e| format!("browser {action}: {e}"))
}

/// What `open` asks for: `profile` (`persistent` or `ephemeral`) and `visible` (show the window).
pub(super) fn open_want(args: &Value) -> Result<Want, String> {
    let profile = match str_arg(args, "profile") {
        None => None,
        Some(p) if p.eq_ignore_ascii_case("persistent") => Some(BrowserProfile::Persistent),
        Some(p) if p.eq_ignore_ascii_case("ephemeral") => Some(BrowserProfile::Ephemeral),
        Some(p) => return Err(format!("browser open: `profile` must be persistent or ephemeral, not {p:?}")),
    };
    let visible = match args.get("visible") {
        None | Some(Value::Null) => None,
        Some(Value::Bool(b)) => Some(*b),
        // models send booleans as text now and then; `bool_arg` takes the same words
        Some(Value::String(s)) => match s.trim().to_ascii_lowercase().as_str() {
            "true" | "yes" | "1" => Some(true),
            "false" | "no" | "0" => Some(false),
            _ => return Err("browser open: `visible` must be true or false".into()),
        },
        Some(_) => return Err("browser open: `visible` must be true or false".into()),
    };
    Ok(Want { profile, visible })
}

/// The tab number (in `to`), as listed by `tabs`.
pub(super) fn index_arg(args: &Value, action: &str) -> Result<usize, String> {
    int_arg(args, "to")
        .map(|i| i as usize)
        .ok_or_else(|| format!("browser {action}: `to` (the tab number, from `tabs`) is required"))
}

/// A positive whole number that may be left out; present but unusable is an error that says what it must be.
pub(super) fn count_arg(args: &Value, key: &str, action: &str, max: u64) -> Result<Option<usize>, String> {
    if args.get(key).is_none_or(Value::is_null) {
        return Ok(None);
    }
    match int_arg(args, key) {
        Some(n) if (1..=max).contains(&n) => Ok(Some(n as usize)),
        _ => Err(format!("browser {action}: `{key}` must be a whole number from 1 to {max}")),
    }
}

pub(super) fn read_page_args<'a>(args: &'a Value, action: &str) -> Result<ReadPageArgs<'a>, String> {
    let interactive = match str_arg(args, "filter") {
        None | Some("all") => false,
        Some("interactive") => true,
        Some(other) => {
            return Err(format!("browser {action}: `filter` must be `all` or `interactive`, not `{other}`"));
        }
    };
    Ok(ReadPageArgs {
        interactive,
        depth: count_arg(args, "depth", action, 1_000)?,
        ref_id: str_arg(args, "ref_id").or_else(|| str_arg(args, "target")),
        max_chars: count_arg(args, "max_chars", action, MAX_PAGE_CHARS)?,
    })
}

pub(super) fn parse(args: &Value) -> Result<Action<'_>, String> {
    let action = str_arg(args, "action").ok_or_else(|| format!("browser: `action` is required (one of: {ACTIONS})"))?;
    Ok(match action {
        "open" => Action::Open(str_arg(args, "url"), open_want(args)?),
        "navigate" => Action::Navigate(need(args, "url", action)?),
        "snapshot" => Action::Snapshot,
        "read_page" => Action::ReadPage(read_page_args(args, action)?),
        // `get_page_text` is the name in Claude's Chrome extension: accepted, not advertised (the schema is budgeted).
        "read_text" | "get_page_text" => Action::ReadText(str_arg(args, "selector")),
        "click" => Action::Click(need(args, "target", action)?),
        "fill" => Action::Fill(need(args, "target", action)?, value_arg(args, action)?),
        "fill_batch" => {
            let list = args
                .get("fields")
                .and_then(Value::as_array)
                .filter(|a| !a.is_empty())
                .ok_or("browser fill_batch: `fields` must be a non-empty list of {target, value}")?;
            let mut pairs = Vec::with_capacity(list.len());
            for (i, f) in list.iter().enumerate() {
                let target = str_arg(f, "target")
                    .ok_or_else(|| format!("browser fill_batch: field {} has no `target`", i + 1))?;
                let value = value_arg(f, action).map_err(|e| format!("{e} (field {})", i + 1))?;
                pairs.push((target, value));
            }
            Action::FillBatch(pairs)
        }
        "select" => Action::Select(need(args, "target", action)?, need(args, "option", action)?),
        "set_checkbox" => match args.get("checked") {
            Some(Value::Bool(b)) => Action::SetCheckbox(need(args, "target", action)?, *b),
            _ => return Err("browser set_checkbox: `checked` (true or false) is required".into()),
        },
        "press" => Action::Press(need(args, "keys", action)?),
        "hover" => Action::Hover(need(args, "target", action)?),
        "scroll" => Action::Scroll(need(args, "to", action)?),
        "wait" => {
            let (selector, ms) = (str_arg(args, "selector"), int_arg(args, "ms"));
            if selector.is_none() && ms.is_none() {
                return Err("browser wait: give `selector` (wait for it to appear) or `ms` (pause)".into());
            }
            Action::Wait(selector, ms)
        }
        "screenshot" => Action::Screenshot(bool_arg(args, "full")),
        "back" => Action::Back,
        "forward" => Action::Forward,
        "reload" => Action::Reload,
        "eval" => match value_arg(args, action)? {
            js if js.trim().is_empty() => return Err("browser eval: `value` (the JavaScript) is required".into()),
            js => Action::Eval(js),
        },
        "console" => Action::Console(str_arg(args, "selector")),
        "network" => Action::Network(str_arg(args, "selector"), int_arg(args, "to")),
        "find" => Action::Find(need(args, "target", action)?),
        "tabs" => Action::Tabs,
        "tab_new" => Action::TabNew(need(args, "url", action)?),
        "tab_switch" => Action::TabSwitch(index_arg(args, action)?),
        "tab_close" => Action::TabClose(index_arg(args, action)?),
        "upload" => Action::Upload(need(args, "target", action)?, need(args, "value", action)?),
        "resize" => match pair(need(args, "to", action)?) {
            Some((w, h)) if (1.0..=65535.0).contains(&w) && (1.0..=65535.0).contains(&h) => {
                Action::Resize(w as u32, h as u32)
            }
            _ => return Err("browser resize: `to` must be WIDTHxHEIGHT in pixels, e.g. 1280x720".into()),
        },
        "click_xy" => point_args(args, action).map(|(x, y, m)| Action::ClickXy(x, y, m))?,
        "double_click" => point_args(args, action).map(|(x, y, m)| Action::DoubleClick(x, y, m))?,
        "right_click" => point_args(args, action).map(|(x, y, m)| Action::RightClick(x, y, m))?,
        "hover_xy" => point_args(args, action).map(|(x, y, _)| Action::HoverXy(x, y))?,
        "drag" => match numbers::<4>(need(args, "to", action)?) {
            Some(p) => Action::Drag(p, modifiers_arg(args, action)?),
            None => return Err("browser drag: `to` must be X1,Y1,X2,Y2 (from, to), e.g. 100,200,300,200".into()),
        },
        "scroll_xy" => match numbers::<4>(need(args, "to", action)?) {
            Some(p) => Action::ScrollXy(p),
            None => {
                return Err("browser scroll_xy: `to` must be X,Y,DX,DY (where, and how far), e.g. 400,300,0,500".into())
            }
        },
        "type_text" => Action::TypeText(value_arg(args, action)?),
        name if gif::NAMES.contains(&name) => match gif::parse(name, args) {
            Some(call) => Action::Gif(call?),
            None => return Err(format!("browser: unknown action `{name}`")),
        },
        "batch" => {
            let list = args
                .get("steps")
                .and_then(Value::as_array)
                .filter(|a| !a.is_empty())
                .ok_or("browser batch: `steps` must be a non-empty list of {action, ...}")?;
            if list.len() > MAX_STEPS {
                return Err(format!("browser batch: at most {MAX_STEPS} steps per call"));
            }
            let mut steps = Vec::with_capacity(list.len());
            for (i, s) in list.iter().enumerate() {
                let name = str_arg(s, "action").unwrap_or("?");
                let step = parse(s).map_err(|e| format!("{e} (batch step {})", i + 1))?;
                if matches!(
                    step,
                    Action::Batch(_)
                        | Action::Close
                        | Action::Open(..)
                        | Action::Screenshot(_)
                        | Action::Browsers
                        | Action::SelectBrowser(_)
                        | Action::SwitchBrowser
                ) {
                    return Err(format!("browser batch: step {} `{name}` cannot be part of a batch", i + 1));
                }
                // A batch shows each step's answer in a few lines, which would cut a page read to pieces.
                if matches!(step, Action::ReadPage(_) | Action::ReadText(_)) {
                    return Err(format!(
                        "browser batch: step {} `{name}` cannot be part of a batch (its answer is too long to keep whole): call it on its own",
                        i + 1
                    ));
                }
                steps.push((name, step));
            }
            Action::Batch(steps)
        }
        "close" => Action::Close,
        "browsers" => Action::Browsers,
        "select_browser" => Action::SelectBrowser(need(args, "id", action)?),
        "switch_browser" => Action::SwitchBrowser,
        other => return Err(format!("browser: unknown action `{other}` (one of: {ACTIONS})")),
    })
}
