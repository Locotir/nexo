//! The memory map a reply carries: one cell for each message of the history, in zones (the anchor and the
//! conversation's topics). It is kept typed and shared, so that a reply costs what changed since the last one
//! and not one JSON object for each message of the session; `to_value` gives the JSON the protocol sends.

use std::fmt;
use std::sync::Arc;

use serde_json::{json, Value};

/// Whether the view takes a message: left out, sent with parts cut, or sent whole.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum State {
    #[default]
    Out,
    Cut,
    Whole,
}

impl State {
    /// The name the protocol gives it (`out`, `cut`, `whole`).
    pub fn as_str(self) -> &'static str {
        match self {
            State::Out => "out",
            State::Cut => "cut",
            State::Whole => "whole",
        }
    }
}

/// One message of the history: its zone (an index into [`Memory::zones`]), its tokens as it arrived (at least
/// 1) and whether the view takes it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Cell {
    pub zone: usize,
    pub tokens: i64,
    pub state: State,
}

/// The anchor or one topic: the request that opened it, whether the view carries its messages, and whether a
/// card describes it instead.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Zone {
    pub label: Arc<str>,
    pub active: bool,
    pub card: bool,
}

/// Every message, by zone, and whether the view takes it. The engine keeps the same lists from one reply to
/// the next and changes only the cells that moved: a host that drops a reply before it asks for the next one
/// never makes it copy them.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Memory {
    pub zones: Arc<Vec<Zone>>,
    pub cells: Arc<Vec<Cell>>,
}

impl Memory {
    /// The map as the JSON-lines protocol sends it: `{"zones": [{label, active, card}], "cells": [{zone,
    /// tokens, state}]}`.
    pub fn to_value(&self) -> Value {
        let zones: Vec<Value> =
            self.zones.iter().map(|z| json!({"label": &*z.label, "active": z.active, "card": z.card})).collect();
        let cells: Vec<Value> =
            self.cells.iter().map(|c| json!({"zone": c.zone, "tokens": c.tokens, "state": c.state.as_str()})).collect();
        let mut out = serde_json::Map::new();
        out.insert("zones".into(), Value::Array(zones));
        out.insert("cells".into(), Value::Array(cells));
        Value::Object(out)
    }
}

/// The same text as `to_value().to_string()`, written without building a JSON value for each message.
impl fmt::Display for Memory {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("{\"zones\":[")?;
        for (k, z) in self.zones.iter().enumerate() {
            let label = serde_json::to_string(&*z.label).map_err(|_| fmt::Error)?;
            let sep = if k > 0 { "," } else { "" };
            write!(f, "{sep}{{\"label\":{label},\"active\":{},\"card\":{}}}", z.active, z.card)?;
        }
        f.write_str("],\"cells\":[")?;
        for (k, c) in self.cells.iter().enumerate() {
            let sep = if k > 0 { "," } else { "" };
            write!(f, "{sep}{{\"zone\":{},\"tokens\":{},\"state\":\"{}\"}}", c.zone, c.tokens, c.state.as_str())?;
        }
        f.write_str("]}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_text_of_a_map_is_the_text_of_its_json() {
        let zone = |label: &str, active, card| Zone { label: label.into(), active, card };
        let cell = |zone, tokens, state| Cell { zone, tokens, state };
        let m = Memory {
            zones: Arc::new(vec![zone("anchor", true, false), zone("fix \"the\" tab\tand ñ\n\\ \u{1}", false, true)]),
            cells: Arc::new(vec![cell(0, 1, State::Whole), cell(1, 90210, State::Cut), cell(1, 7, State::Out)]),
        };
        assert_eq!(m.to_string(), m.to_value().to_string());
        assert_eq!(Memory::default().to_string(), Memory::default().to_value().to_string());
        assert_eq!(Memory::default().to_string(), r#"{"zones":[],"cells":[]}"#);
    }
}
