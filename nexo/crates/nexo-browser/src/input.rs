//! Low-level keyboard input: key combos such as `Ctrl+A` or `Shift+Tab` through the Input domain.

use crate::{Error, Result};
use chromiumoxide::cdp::browser_protocol::input::{DispatchKeyEventParams, DispatchKeyEventType};
use chromiumoxide::keys::{get_key_definition, KeyDefinition};
use chromiumoxide::page::Page;

/// Modifier names (lowercase aliases), the key they press and their bit in the CDP `modifiers` mask.
const MODIFIERS: [(&[&str], &str, i64); 4] = [
    (&["alt", "option"], "Alt", 1),
    (&["ctrl", "control"], "Control", 2),
    (&["meta", "cmd", "command"], "Meta", 4),
    (&["shift"], "Shift", 8),
];

const ALT: i64 = 1;
const SHORTCUT: i64 = 2 | 4;
const SHIFT: i64 = 8;

/// A key plus the modifiers held while it is pressed.
struct Combo {
    modifiers: Vec<(&'static KeyDefinition, i64)>,
    key: String,
    def: &'static KeyDefinition,
}

impl Combo {
    /// What the key types: its own text, or the key itself when it is one character.
    fn text(&self) -> Option<String> {
        let single = self.key.chars().count() == 1;
        self.def.text.map(str::to_string).or_else(|| single.then(|| self.key.clone()))
    }
}

fn unknown(key: &str) -> Error {
    Error::Unusable { target: key.to_string(), reason: "unknown key".to_string() }
}

/// `None` when the token is a single key, not a combo.
fn parse(token: &str) -> Option<Result<Combo>> {
    let mut parts: Vec<&str> = token.split('+').filter(|p| !p.is_empty()).collect();
    if token.ends_with("++") {
        parts.push("+");
    }
    if parts.len() < 2 {
        return None;
    }
    Some(build(token, &parts))
}

fn build(token: &str, parts: &[&str]) -> Result<Combo> {
    let (key, held) = parts.split_last().ok_or_else(|| unknown(token))?;
    let mut modifiers = Vec::new();
    for name in held {
        let lower = name.to_lowercase();
        let (_, key_name, bit) =
            MODIFIERS.iter().find(|(aliases, ..)| aliases.contains(&lower.as_str())).ok_or_else(|| unknown(name))?;
        modifiers.push((get_key_definition(key_name).ok_or_else(|| unknown(name))?, *bit));
    }
    // Shortcut keys are looked up as lowercase letters; shift alone turns them into capitals.
    let shifted = modifiers.iter().any(|(_, bit)| *bit == SHIFT);
    let def = get_key_definition(key)
        .or_else(|| get_key_definition(key.to_lowercase()))
        .or_else(|| get_key_definition(capitalize(key)))
        .ok_or_else(|| unknown(key))?;
    let key = if key.chars().count() == 1 && shifted { key.to_uppercase() } else { def.key.to_string() };
    Ok(Combo { modifiers, key, def })
}

fn capitalize(s: &str) -> String {
    let mut chars = s.chars();
    chars.next().map(|c| c.to_uppercase().chain(chars).collect()).unwrap_or_default()
}

/// The editing command a shortcut stands for; headless Chrome does not map shortcuts to commands itself.
fn command_for(mask: i64, key: &str) -> Option<&'static str> {
    let shifted = mask & SHIFT != 0;
    if mask & SHORTCUT == 0 || mask & ALT != 0 {
        return None;
    }
    match (shifted, key.to_lowercase().as_str()) {
        (false, "a") => Some("selectAll"),
        (false, "c") => Some("copy"),
        (false, "x") => Some("cut"),
        (false, "v") => Some("paste"),
        (false, "z") => Some("undo"),
        (false, "y") | (true, "z") => Some("redo"),
        _ => None,
    }
}

async fn send(page: &Page, kind: DispatchKeyEventType, def: &KeyDefinition, key: &str, mask: i64) -> Result<()> {
    send_with(page, kind, def, key, mask, None, None).await
}

async fn send_with(
    page: &Page,
    kind: DispatchKeyEventType,
    def: &KeyDefinition,
    key: &str,
    mask: i64,
    text: Option<String>,
    command: Option<&str>,
) -> Result<()> {
    let mut cmd = DispatchKeyEventParams::builder()
        .r#type(kind)
        .modifiers(mask)
        .key(key)
        .code(def.code)
        .windows_virtual_key_code(def.key_code)
        .native_virtual_key_code(def.key_code);
    if let Some(text) = text {
        cmd = cmd.text(text);
    }
    if let Some(command) = command {
        cmd = cmd.command(command);
    }
    page.execute(cmd.build().map_err(Error::Config)?).await?;
    Ok(())
}

/// Presses the combo if the token is one (`Ctrl+A`); returns `false` for a plain key so the caller handles it.
pub(crate) async fn press_combo(page: &Page, token: &str) -> Result<bool> {
    let combo = match parse(token) {
        None => return Ok(false),
        Some(combo) => combo?,
    };
    let mut mask = 0;
    for (def, bit) in &combo.modifiers {
        mask |= bit;
        send(page, DispatchKeyEventType::RawKeyDown, def, def.key, mask).await?;
    }
    // Text is only typed when no shortcut modifier is held.
    let text = if mask & !SHIFT == 0 { combo.text() } else { None };
    let kind = if text.is_some() { DispatchKeyEventType::KeyDown } else { DispatchKeyEventType::RawKeyDown };
    send_with(page, kind, combo.def, &combo.key, mask, text, command_for(mask, &combo.key)).await?;
    send(page, DispatchKeyEventType::KeyUp, combo.def, &combo.key, mask).await?;
    for (def, bit) in combo.modifiers.iter().rev() {
        mask &= !bit;
        send(page, DispatchKeyEventType::KeyUp, def, def.key, mask).await?;
    }
    Ok(true)
}
