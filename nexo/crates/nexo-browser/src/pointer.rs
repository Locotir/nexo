//! Mouse input at viewport coordinates through `Input.dispatchMouseEvent`: clicks, double and right clicks,
//! drags, hovering and the wheel, optionally with Ctrl, Shift, Alt or Meta held.

use crate::input::MODIFIERS;
use crate::{Error, Result};
use chromiumoxide::cdp::browser_protocol::input::{
    DispatchDragEventParams, DispatchDragEventType, DispatchMouseEventParams, DispatchMouseEventType,
    EventDragIntercepted, MouseButton, SetInterceptDragsParams,
};
use chromiumoxide::page::Page;
use futures::StreamExt;
use serde_json::{json, Value};
use std::time::Duration;

/// Moves between the press and the release of a drag, so pages that follow the pointer see a path.
const DRAG_STEPS: u32 = 8;

/// How long after the moves of a drag the browser may take to say it started a drag and drop of its own.
const DRAG_START_WAIT: Duration = Duration::from_millis(100);

/// Modifier keys held during a mouse action, as the bit mask of the CDP `modifiers` field.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Modifiers(i64);

impl Modifiers {
    /// Names joined by `+`, `,` or spaces: `ctrl`, `shift`, `alt`, `meta` (also `control`, `option`, `cmd`,
    /// `command`), in any case. An empty text is no modifier.
    pub fn parse(text: &str) -> Result<Modifiers> {
        let mut mask = 0;
        for name in text.split(|c: char| c == '+' || c == ',' || c.is_whitespace()).filter(|n| !n.is_empty()) {
            let lower = name.to_lowercase();
            let (_, _, bit) =
                MODIFIERS.iter().find(|(aliases, ..)| aliases.contains(&lower.as_str())).ok_or_else(|| {
                    Error::Unusable {
                        target: name.to_string(),
                        reason: "unknown modifier (ctrl, shift, alt, meta)".into(),
                    }
                })?;
            mask |= bit;
        }
        Ok(Modifiers(mask))
    }

    pub fn is_none(self) -> bool {
        self.0 == 0
    }
}

/// `ctrl+shift`: the names of the held keys joined by `+`.
impl std::fmt::Display for Modifiers {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let names: Vec<String> = MODIFIERS
            .iter()
            .filter(|(_, _, bit)| self.0 & bit != 0)
            .map(|(aliases, ..)| aliases[0].to_string())
            .collect();
        f.write_str(&names.join("+"))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Button {
    Left,
    Right,
}

impl Button {
    fn name(self) -> &'static str {
        match self {
            Button::Left => "left",
            Button::Right => "right",
        }
    }

    fn cdp(self) -> MouseButton {
        match self {
            Button::Left => MouseButton::Left,
            Button::Right => MouseButton::Right,
        }
    }

    /// The bit of this button in the `buttons` field.
    fn bit(self) -> i64 {
        match self {
            Button::Left => 1,
            Button::Right => 2,
        }
    }
}

pub(crate) type Xy = (f64, f64);

/// Refuses coordinates that cannot be sent (NaN, infinity).
pub(crate) fn check(points: &[Xy]) -> Result<()> {
    match points.iter().find(|(x, y)| !x.is_finite() || !y.is_finite()) {
        None => Ok(()),
        Some((x, y)) => Err(Error::Unusable { target: format!("{x},{y}"), reason: "not a finite point".into() }),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Kind {
    Moved,
    Pressed,
    Released,
    Wheel,
}

/// One `Input.dispatchMouseEvent`. Both backends send the same list: the headless one typed, the person's own
/// browser as raw protocol parameters ([`Mouse::params`]).
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Mouse {
    kind: Kind,
    at: Xy,
    button: Option<Button>,
    /// Buttons held while the event happens.
    held: i64,
    count: i64,
    mods: Modifiers,
    wheel: Option<Xy>,
}

impl Mouse {
    fn new(kind: Kind, at: Xy, mods: Modifiers) -> Mouse {
        Mouse { kind, at, button: None, held: 0, count: 0, mods, wheel: None }
    }

    /// The raw protocol parameters of the event.
    pub(crate) fn params(&self) -> Value {
        let kind = match self.kind {
            Kind::Moved => "mouseMoved",
            Kind::Pressed => "mousePressed",
            Kind::Released => "mouseReleased",
            Kind::Wheel => "mouseWheel",
        };
        let mut p =
            json!({"type": kind, "x": self.at.0, "y": self.at.1, "modifiers": self.mods.0, "buttons": self.held});
        if let Some(b) = self.button {
            p["button"] = json!(b.name());
            p["clickCount"] = json!(self.count);
        }
        if let Some((dx, dy)) = self.wheel {
            p["deltaX"] = json!(dx);
            p["deltaY"] = json!(dy);
        }
        p
    }

    fn cdp(&self) -> Result<DispatchMouseEventParams> {
        let kind = match self.kind {
            Kind::Moved => DispatchMouseEventType::MouseMoved,
            Kind::Pressed => DispatchMouseEventType::MousePressed,
            Kind::Released => DispatchMouseEventType::MouseReleased,
            Kind::Wheel => DispatchMouseEventType::MouseWheel,
        };
        let mut cmd = DispatchMouseEventParams::builder()
            .r#type(kind)
            .x(self.at.0)
            .y(self.at.1)
            .modifiers(self.mods.0)
            .buttons(self.held);
        if let Some(b) = self.button {
            cmd = cmd.button(b.cdp()).click_count(self.count);
        }
        if let Some((dx, dy)) = self.wheel {
            cmd = cmd.delta_x(dx).delta_y(dy);
        }
        cmd.build().map_err(Error::Config)
    }
}

fn press(at: Xy, button: Button, count: i64, mods: Modifiers) -> Mouse {
    Mouse { button: Some(button), held: button.bit(), count, ..Mouse::new(Kind::Pressed, at, mods) }
}

fn release(at: Xy, button: Button, count: i64, mods: Modifiers) -> Mouse {
    Mouse { button: Some(button), held: 0, count, ..Mouse::new(Kind::Released, at, mods) }
}

/// The pointer moved there, no button down.
pub(crate) fn hover(at: Xy, mods: Modifiers) -> Result<Vec<Mouse>> {
    check(&[at])?;
    Ok(vec![Mouse::new(Kind::Moved, at, mods)])
}

/// `count` clicks in a row with this button: 1 is a click, 2 a double click (which fires `dblclick`).
pub(crate) fn click(at: Xy, button: Button, count: i64, mods: Modifiers) -> Result<Vec<Mouse>> {
    let mut out = hover(at, mods)?;
    for n in 1..=count {
        out.push(press(at, button, n, mods));
        out.push(release(at, button, n, mods));
    }
    Ok(out)
}

/// The wheel turned at `at` by `delta` CSS pixels (positive y scrolls down, positive x right).
pub(crate) fn wheel(at: Xy, delta: Xy, mods: Modifiers) -> Result<Vec<Mouse>> {
    check(&[at, delta])?;
    let mut out = hover(at, mods)?;
    out.push(Mouse { wheel: Some(delta), ..Mouse::new(Kind::Wheel, at, mods) });
    Ok(out)
}

/// A drag up to the release: move to `from`, press there and move in steps to `to` with the button held.
pub(crate) fn drag_moves(from: Xy, to: Xy, mods: Modifiers) -> Result<Vec<Mouse>> {
    check(&[from, to])?;
    let mut out = hover(from, mods)?;
    out.push(press(from, Button::Left, 1, mods));
    for step in 1..=DRAG_STEPS {
        let t = f64::from(step) / f64::from(DRAG_STEPS);
        let at = (from.0 + (to.0 - from.0) * t, from.1 + (to.1 - from.1) * t);
        out.push(Mouse { button: Some(Button::Left), held: Button::Left.bit(), ..Mouse::new(Kind::Moved, at, mods) });
    }
    Ok(out)
}

/// The release that ends a drag at `to`.
pub(crate) fn drag_release(to: Xy, mods: Modifiers) -> Mouse {
    release(to, Button::Left, 1, mods)
}

/// Sends the events in order on a headless page.
pub(crate) async fn send(page: &Page, events: &[Mouse]) -> Result<()> {
    for e in events {
        page.execute(e.cdp()?).await?;
    }
    Ok(())
}

/// Presses the left button at `from`, moves in steps to `to` and releases it there. When that starts an HTML5
/// drag and drop (the press was on a `draggable` element, a selection or a link) the drag is carried to `to` as
/// drag events and dropped there; the reply says whether that happened. Otherwise only the mouse events are sent,
/// which is what sliders and canvases listen to.
pub(crate) async fn drag(page: &Page, from: Xy, to: Xy, mods: Modifiers) -> Result<bool> {
    let moves = drag_moves(from, to, mods)?;
    let mut intercepted = page.event_listener::<EventDragIntercepted>().await?;
    page.execute(SetInterceptDragsParams::new(true)).await?;
    let done = drag_with(page, &mut intercepted, &moves, to, mods).await;
    // Interception must not stay on, or a later drag by hand would never reach the page.
    let off = page.execute(SetInterceptDragsParams::new(false)).await;
    let dropped = done?;
    off?;
    Ok(dropped)
}

async fn drag_with(
    page: &Page,
    intercepted: &mut (impl futures::Stream<Item = std::sync::Arc<EventDragIntercepted>> + Unpin),
    moves: &[Mouse],
    to: Xy,
    mods: Modifiers,
) -> Result<bool> {
    send(page, moves).await?;
    let started = tokio::time::timeout(DRAG_START_WAIT, intercepted.next()).await.ok().flatten();
    if let Some(event) = &started {
        for kind in [DispatchDragEventType::DragEnter, DispatchDragEventType::DragOver, DispatchDragEventType::Drop] {
            let mut cmd = DispatchDragEventParams::new(kind, to.0, to.1, event.data.clone());
            cmd.modifiers = Some(mods.0);
            page.execute(cmd).await?;
        }
    }
    send(page, &[drag_release(to, mods)]).await?;
    Ok(started.is_some())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn modifiers_parse_names_in_any_form() {
        assert_eq!(Modifiers::parse("").unwrap(), Modifiers::default());
        assert!(Modifiers::parse(" ").unwrap().is_none());
        assert_eq!(Modifiers::parse("ctrl").unwrap(), Modifiers(2));
        assert_eq!(Modifiers::parse("Ctrl+Shift").unwrap(), Modifiers(10));
        assert_eq!(Modifiers::parse("alt, meta").unwrap(), Modifiers(5));
        assert_eq!(Modifiers::parse("cmd option control shift").unwrap(), Modifiers(15));
        assert_eq!(Modifiers::parse("shift ctrl").unwrap().to_string(), "ctrl+shift");
        assert!(matches!(Modifiers::parse("ctrl+hyper"), Err(Error::Unusable { .. })));
    }

    #[test]
    fn the_raw_events_carry_button_count_modifiers_and_wheel() {
        let ctrl = Modifiers::parse("ctrl").unwrap();
        let dbl = click((5.0, 6.0), Button::Left, 2, ctrl).unwrap();
        assert_eq!(dbl.len(), 5);
        assert_eq!(dbl[0].params(), json!({"type": "mouseMoved", "x": 5.0, "y": 6.0, "modifiers": 2, "buttons": 0}));
        assert_eq!(dbl[3].params()["clickCount"], 2);
        assert_eq!((dbl[3].params()["button"].clone(), dbl[3].params()["buttons"].clone()), (json!("left"), json!(1)));
        let right = click((1.0, 1.0), Button::Right, 1, Modifiers::default()).unwrap();
        assert_eq!(right[1].params()["buttons"], 2);
        let w = wheel((1.0, 2.0), (0.0, 300.0), Modifiers::default()).unwrap();
        assert_eq!(
            (w[1].params()["type"].clone(), w[1].params()["deltaY"].clone()),
            (json!("mouseWheel"), json!(300.0))
        );
        let d = drag_moves((0.0, 0.0), (80.0, 0.0), Modifiers::default()).unwrap();
        assert_eq!(d.len(), 2 + DRAG_STEPS as usize);
        assert_eq!(d.last().unwrap().params()["x"], 80.0);
        assert_eq!(drag_release((80.0, 0.0), Modifiers::default()).params()["buttons"], 0);
        assert!(drag_moves((0.0, f64::NAN), (1.0, 1.0), Modifiers::default()).is_err());
    }

    #[test]
    fn points_must_be_finite() {
        assert!(check(&[(1.0, 2.0), (-3.5, 0.0)]).is_ok());
        assert!(check(&[(1.0, f64::NAN)]).is_err());
        assert!(check(&[(f64::INFINITY, 0.0)]).is_err());
    }
}
