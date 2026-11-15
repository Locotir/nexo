//! The findings of `nexo review`: read from the model's reply and shown.

/// One finding of `nexo review`.
#[derive(Debug, Clone, PartialEq)]
pub struct Finding {
    pub severity: String,
    pub file: String,
    pub line: Option<u64>,
    pub title: String,
    pub detail: String,
}

/// Reads the model's JSON answer (it may be wrapped in a fence or prose).
pub fn parse_findings(text: &str) -> Result<Vec<Finding>, String> {
    let (a, b) = (text.find('{'), text.rfind('}'));
    let (Some(a), Some(b)) = (a, b) else { return Err("the reply holds no JSON object".into()) };
    let v: serde_json::Value =
        serde_json::from_str(&text[a..=b]).map_err(|e| format!("the reply is not valid JSON: {e}"))?;
    let list = v.get("findings").and_then(|f| f.as_array()).ok_or("the reply has no \"findings\" list")?;
    let s = |x: &serde_json::Value, k: &str| x.get(k).and_then(|v| v.as_str()).unwrap_or("").to_string();
    Ok(list
        .iter()
        .map(|f| Finding {
            severity: s(f, "severity").to_ascii_lowercase(),
            file: s(f, "file"),
            line: f.get("line").and_then(|l| l.as_u64()),
            title: s(f, "title"),
            detail: s(f, "detail"),
        })
        .collect())
}

/// Whether CI should fail on these findings.
pub fn has_high(findings: &[Finding]) -> bool {
    findings.iter().any(|f| matches!(f.severity.as_str(), "high" | "critical"))
}

/// The findings as plain text, one block each.
pub fn render_findings(findings: &[Finding]) -> String {
    if findings.is_empty() {
        return "No findings.\n".into();
    }
    let mut out = String::new();
    for (i, f) in findings.iter().enumerate() {
        let at = match f.line {
            Some(l) => format!("{}:{l}", f.file),
            None => f.file.clone(),
        };
        out.push_str(&format!("{}. [{}] {at} - {}\n", i + 1, f.severity.to_ascii_uppercase(), f.title));
        if !f.detail.is_empty() {
            out.push_str(&format!("   {}\n", f.detail));
        }
    }
    out
}

/// The findings as JSON, for scripts.
pub fn findings_json(findings: &[Finding]) -> String {
    let list: Vec<_> = findings
        .iter()
        .map(|f| serde_json::json!({"severity": f.severity, "file": f.file, "line": f.line, "title": f.title, "detail": f.detail}))
        .collect();
    serde_json::json!({"findings": list, "high": has_high(findings)}).to_string()
}
