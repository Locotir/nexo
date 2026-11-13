use super::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TurnEnd {
    Done,
    Interrupted,
    Error,
    MaxTurns,
}

/// What a message of the history is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CellKind {
    /// Something the person wrote.
    Request,
    /// The model's text.
    Reply,
    /// The model asking for tools.
    Call,
    /// What the tools answered.
    Output,
}

/// How much of a message the next model call carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CellState {
    Whole,
    /// Shortened or stubbed.
    Shrunk,
    Dropped,
    /// Left out, with parts of it brought back.
    Recalled,
}

/// Where the output of one tool call stands for the model call just made.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CallMark {
    /// Sent whole, in the part of the request equal to the one before (the provider serves it from its
    /// cache while that lasts).
    Cached,
    /// Sent whole, after the point where the request changed: paid in full.
    Sent,
    /// Sent shortened.
    Cut,
    /// Left out of the request; kept in the history, so it can come back.
    Out,
    /// Left out, with parts of it brought back.
    Back,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextCell {
    pub kind: CellKind,
    pub tokens: u64,
    pub state: CellState,
    /// Index into the map's `zones` (0 when there are none).
    #[serde(default)]
    pub zone: u32,
}

/// A part of the curator's memory: the anchor or one topic of the conversation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextZone {
    /// The request that opened the topic (`anchor` for the first message).
    pub label: String,
    /// The topic in progress, whose messages the call carries.
    pub active: bool,
    /// Described by a card in the call.
    #[serde(default)]
    pub card: bool,
}

/// What kind of thing the curator put in a call (or held back).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CuratedKind {
    /// A tool output kept in the history: one a step brought back, or one the view left out or cut.
    Recalled,
    /// The start of the request: the map of what moved out, a file it pins or one the agent wrote.
    Start,
    /// A standing instruction the user gave.
    Rule,
    /// A topic of the conversation, as a card or left out.
    Thread,
    /// The plan the agent wrote down.
    Goal,
    /// A fact the agent or the curator's ledger kept.
    Memory,
}

/// Where an item of the curator stands in the call.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CuratedState {
    In,
    /// In the call, shortened.
    Trimmed,
    /// Held back (its tokens are what was held back).
    Out,
}

/// One item the curator added to a call, or held back: its name, kind and size, never its text.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CuratedItem {
    pub kind: CuratedKind,
    pub label: String,
    pub tokens: u64,
    pub state: CuratedState,
}

/// What nexo put in a message of the person beyond the words they typed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AddedKind {
    /// An MCP resource named with `@server:uri`.
    Mention,
    /// A file attached, named by the person or found by nexo.
    File,
    /// The note of background jobs that ended.
    Jobs,
    /// What a hook added for the model.
    Hook,
    /// Memory shared from another session.
    Memory,
    /// The `[project at the start of this request]` lines.
    StartNote,
    /// A note of the supervisor that rides on one request and is never saved.
    Supervisor,
}

/// One piece of text nexo added to a message of the person, kept apart from their own words where the
/// two are put together (a live session tells them as they are built; a resumed one finds them by markers).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AddedPart {
    pub kind: AddedKind,
    /// What it is, short: a path, a title, `hook context`.
    pub label: String,
    /// The exact text that went to the model.
    pub text: String,
    /// It rides on one request only and is not in the saved conversation.
    #[serde(default)]
    pub request_only: bool,
}

impl AddedPart {
    pub fn new(kind: AddedKind, label: impl Into<String>, text: impl Into<String>) -> AddedPart {
        AddedPart { kind, label: label.into(), text: text.into(), request_only: false }
    }

    /// About how many tokens it takes (4 characters a token).
    pub fn tokens(&self) -> u64 {
        (self.text.len() as u64).div_ceil(4)
    }
}

/// One section of the last request as it was sent (`/sent`): a heading, its text and its size.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SentSection {
    pub heading: String,
    /// 0: a main section (system prompt, a message); 1: a part of the one above.
    pub depth: u8,
    /// `curator` or `supervisor` when the context policy or the supervisor put this text in.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tag: Option<String>,
    pub tokens: u64,
    pub text: String,
    /// Shown as its first lines until opened (a tool result).
    #[serde(default)]
    pub collapse: bool,
}
