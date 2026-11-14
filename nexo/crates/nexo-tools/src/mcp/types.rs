use serde_json::Value;

#[derive(Debug, Clone)]
pub struct ToolDef {
    pub name: String,
    pub description: String,
    pub input_schema: Value,
    pub read_only: bool,
}
/// A resource the server lists.
#[derive(Debug, Clone, PartialEq)]
pub struct ResourceDef {
    pub uri: String,
    pub name: String,
    pub description: String,
    pub mime_type: String,
}

/// A resource template (`file:///{path}`).
#[derive(Debug, Clone, PartialEq)]
pub struct TemplateDef {
    pub uri_template: String,
    pub name: String,
    pub description: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PromptArg {
    pub name: String,
    pub description: String,
    pub required: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PromptDef {
    pub name: String,
    pub description: String,
    pub arguments: Vec<PromptArg>,
}

/// The answer of `prompts/get`: `(role, text)` per message.
#[derive(Debug, Clone, PartialEq)]
pub struct PromptText {
    pub description: String,
    pub messages: Vec<(String, String)>,
}

impl PromptText {
    /// The text that goes in as the user's request: the messages in order, those not from the user
    /// marked with their role.
    pub fn as_request(&self) -> String {
        let one = self.messages.len() == 1;
        self.messages
            .iter()
            .map(|(role, text)| if one || role == "user" { text.clone() } else { format!("[{role}] {text}") })
            .collect::<Vec<_>>()
            .join("\n\n")
    }
}
