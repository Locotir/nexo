//! Key presses as raw `Input.dispatchKeyEvent` parameters, for the relay backend (the headless one goes through
//! `chromiumoxide` types in `input.rs`; the key table and the combo grammar are the same).

use crate::input::{command_for, parse, unknown, SHIFT};
use crate::Result;
use chromiumoxide::keys::{get_key_definition, KeyDefinition};
use serde_json::{json, Value};

fn event(kind: &str, def: &KeyDefinition, key: &str, mask: i64, text: Option<String>, command: Option<&str>) -> Value {
    let mut e = json!({
        "type": kind, "modifiers": mask, "key": key, "code": def.code,
        "windowsVirtualKeyCode": def.key_code, "nativeVirtualKeyCode": def.key_code,
    });
    if let Some(text) = text {
        e["text"] = json!(text);
    }
    if let Some(command) = command {
        e["commands"] = json!([command]);
    }
    e
}

/// The events that press one token: a plain key (`Enter`, `a`) or a combo (`Ctrl+A`, `Shift+Tab`).
pub(super) fn events_for(token: &str) -> Result<Vec<Value>> {
    if let Some(combo) = parse(token) {
        let combo = combo?;
        let mut out = Vec::new();
        let mut mask = 0;
        for (def, bit) in &combo.modifiers {
            mask |= bit;
            out.push(event("rawKeyDown", def, def.key, mask, None, None));
        }
        // Text is only typed when no shortcut modifier is held; with Shift held a letter is typed in capitals.
        let shifted = mask & SHIFT != 0 && combo.key.chars().count() == 1;
        let key = if shifted { combo.key.to_uppercase() } else { combo.key.clone() };
        let text =
            if mask & !SHIFT == 0 { combo.text().map(|t| if shifted { t.to_uppercase() } else { t }) } else { None };
        let kind = if text.is_some() { "keyDown" } else { "rawKeyDown" };
        out.push(event(kind, combo.def, &key, mask, text, command_for(mask, &combo.key)));
        out.push(event("keyUp", combo.def, &key, mask, None, None));
        for (def, bit) in combo.modifiers.iter().rev() {
            mask &= !bit;
            out.push(event("keyUp", def, def.key, mask, None, None));
        }
        return Ok(out);
    }
    let def = get_key_definition(token).ok_or_else(|| unknown(token))?;
    let text = def.text.map(str::to_string).or_else(|| (def.key.len() == 1).then(|| def.key.to_string()));
    let kind = if text.is_some() { "keyDown" } else { "rawKeyDown" };
    Ok(vec![event(kind, def, def.key, 0, text, None), event("keyUp", def, def.key, 0, None, None)])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(token: &str) -> Vec<String> {
        events_for(token)
            .unwrap()
            .iter()
            .map(|e| format!("{}:{}", e["type"].as_str().unwrap(), e["key"].as_str().unwrap()))
            .collect()
    }

    #[test]
    fn a_plain_key_is_down_and_up() {
        assert_eq!(kinds("Enter"), ["keyDown:Enter", "keyUp:Enter"]);
        let e = events_for("Enter").unwrap();
        assert_eq!(e[0]["text"], "\r");
        assert_eq!(e[0]["windowsVirtualKeyCode"], 13);
        assert_eq!(kinds("Tab"), ["rawKeyDown:Tab", "keyUp:Tab"]);
    }

    #[test]
    fn a_combo_holds_its_modifiers_around_the_key() {
        assert_eq!(kinds("Ctrl+A"), ["rawKeyDown:Control", "rawKeyDown:A", "keyUp:A", "keyUp:Control"]);
        let e = events_for("Ctrl+A").unwrap();
        assert_eq!(e[1]["modifiers"], 2);
        assert_eq!(e[1]["commands"], json!(["selectAll"]));
        assert_eq!(e[3]["modifiers"], 0);
        let shift = events_for("Shift+a").unwrap();
        assert_eq!(shift[1]["key"], "A");
        assert_eq!(shift[1]["text"], "A");
    }

    #[test]
    fn an_unknown_key_is_refused() {
        assert!(events_for("Ctrl+Nope").is_err());
        assert!(events_for("Nope").is_err());
    }
}
