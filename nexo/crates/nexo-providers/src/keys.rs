//! What the first-run setup needs to know about a key a person pastes: how to clean it, which service
//! it belongs to when its start says so, how to show it without showing it, and the page where such a
//! key is made.

/// A pasted key as the services want it: no escape codes or line breaks, no spaces around it, no quotes.
/// A key wrapped over several lines is joined back.
pub fn clean(text: &str) -> String {
    let joined: String = text.chars().filter(|c| !c.is_control()).collect();
    let mut s = joined.trim();
    loop {
        let before = s;
        for q in ['"', '\'', '`', '\u{201c}', '\u{201d}', '\u{2018}', '\u{2019}'] {
            s = s.trim_matches(q).trim();
        }
        if s == before {
            break;
        }
    }
    s.to_string()
}

/// The starts of the keys of one service only; each is the service's own, published in its documentation.
const STARTS: &[(&str, &str)] = &[
    ("sk-ant-", "anthropic"),
    ("sk-or-v1-", "openrouter"),
    ("sk-proj-", "openai"),
    ("gsk_", "groq"),
    ("AIza", "gemini"),
    ("csk-", "cerebras"),
];

/// The start several services share (OpenAI's older keys, DeepSeek and others): it marks a key, but names
/// no service.
const SHARED_START: &str = "sk-";

/// Whether a text, as it is typed, starts like a key (a known service's start, or the one several share)
/// and has `more` characters after that start. Nothing else is taken for a key: a model name or a search
/// never is.
pub fn starts_like_key(text: &str, more: usize) -> bool {
    let t = text.trim();
    STARTS
        .iter()
        .map(|(p, _)| *p)
        .chain([SHARED_START])
        .any(|p| t.starts_with(p) && t.chars().count() >= p.chars().count() + more)
}

/// The preset a key belongs to, from a start that only that service uses. A start that two services
/// share (a bare `sk-`) names none: the person is asked.
pub fn recognise(key: &str) -> Option<&'static str> {
    let key = key.trim();
    STARTS
        .iter()
        .filter(|(p, _)| key.starts_with(p) && key.len() > p.len() + 8)
        .max_by_key(|(p, _)| p.len())
        .map(|(_, n)| *n)
}

/// Whether a text looks like a key rather than a name or a sentence: one word, long enough, no spaces.
pub fn looks_like_key(text: &str) -> bool {
    let t = clean(text);
    t.chars().count() >= 16 && !t.contains(char::is_whitespace) && !t.contains("://")
}

/// A key for the screen: dots, then its last four characters (dots only when it is too short to show any).
pub fn mask(key: &str) -> String {
    let n = key.chars().count();
    if n <= 8 {
        return "\u{2022}".repeat(n);
    }
    let tail: String = key.chars().skip(n - 4).collect();
    format!("{}{tail}", "\u{2022}".repeat(n - 4))
}

/// The page where the key of this service is made, for the services whose page is known.
pub fn page(name: &str) -> Option<&'static str> {
    Some(match name {
        "openai" => "https://platform.openai.com/api-keys",
        "anthropic" => "https://platform.claude.com/settings/keys",
        "gemini" => "https://aistudio.google.com/apikey",
        "deepseek" => "https://platform.deepseek.com/api_keys",
        "openrouter" => "https://openrouter.ai/keys",
        "groq" => "https://console.groq.com/keys",
        "mistral" => "https://console.mistral.ai/api-keys",
        "together" => "https://api.together.ai/settings/api-keys",
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_paste_loses_its_quotes_spaces_breaks_and_escapes() {
        assert_eq!(clean("  \"abc-123\"  "), "abc-123");
        assert_eq!(clean("'abc-123'\n"), "abc-123");
        assert_eq!(clean("\u{201c}abc-123\u{201d}"), "abc-123");
        assert_eq!(clean("abc-\r\n123"), "abc-123");
        assert_eq!(clean("\u{1b}abc-123\u{7}"), "abc-123");
        assert_eq!(clean(" ` \"abc\" ` "), "abc");
    }

    #[test]
    fn only_a_start_one_service_uses_names_the_service() {
        assert_eq!(recognise("sk-ant-api03-madeupmadeupmade"), Some("anthropic"));
        assert_eq!(recognise("sk-or-v1-madeupmadeupmadeup"), Some("openrouter"));
        assert_eq!(recognise("sk-proj-madeupmadeupmade"), Some("openai"));
        assert_eq!(recognise("gsk_madeupmadeupmadeup"), Some("groq"));
        assert_eq!(recognise("AIzaMadeUpMadeUpMadeUp"), Some("gemini"));
        // a bare `sk-` is shared (OpenAI, DeepSeek and others): ask
        assert_eq!(recognise("sk-made-up-0123-made-up-4567-abcd"), None);
        assert_eq!(recognise("sk-ant-"), None, "a start with nothing after it names nothing");
        assert_eq!(recognise("hello"), None);
    }

    #[test]
    fn a_key_shows_dots_and_its_last_four() {
        assert_eq!(mask("abcdefghijklmnop"), "\u{2022}".repeat(12) + "mnop");
        assert_eq!(mask("abc"), "\u{2022}\u{2022}\u{2022}");
        assert_eq!(mask(""), "");
        assert!(!mask("sk-secret-secret-secret-1234").contains("secret"));
    }

    #[test]
    fn a_text_is_taken_for_a_key_only_when_it_looks_like_one() {
        assert!(looks_like_key("sk-0123-4567-89ab-cdef-0123"));
        assert!(!looks_like_key("hello there"));
        assert!(!looks_like_key("localhost:11434"));
        assert!(!looks_like_key("https://api.example.com/v1"));
    }

    #[test]
    fn the_pages_are_https_and_belong_to_presets() {
        for n in crate::presets::names() {
            if let Some(p) = page(n) {
                assert!(p.starts_with("https://"), "{n}");
            }
        }
        assert!(page("deepseek").is_some() && page("ollama").is_none());
    }
}
