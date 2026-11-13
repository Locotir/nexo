//! The page as a short list of actionable elements, each with a stable ref (`e1`, `e2`, ...).

use serde::Deserialize;

/// One actionable element: the ref to act on, its role (or tag) and the name a person would call it.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Element {
    #[serde(rename = "ref")]
    pub reference: String,
    pub role: String,
    pub name: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Snapshot {
    pub elements: Vec<Element>,
}

impl Snapshot {
    /// One line per element, e.g. `e3 button "Submit"`, for the model to read.
    pub fn render(&self) -> String {
        let mut out = String::new();
        for e in &self.elements {
            if !out.is_empty() {
                out.push('\n');
            }
            let name = e.name.replace('\\', "\\\\").replace('"', "\\\"");
            out.push_str(&format!("{} {} \"{}\"", e.reference, e.role, name));
        }
        out
    }

    /// The first element with this ref.
    pub fn find(&self, reference: &str) -> Option<&Element> {
        self.elements.iter().find(|e| e.reference == reference)
    }
}

/// Walks the visible actionable elements. An element keeps the ref it was given on an earlier
/// snapshot (the attribute stays on the node); new ones continue after the highest number in use.
/// Returns a JSON string so the reply crosses the protocol as one value.
pub(crate) const SNAPSHOT_JS: &str = r#"(() => {
  const sel = 'a, button, input, select, textarea, [role], [onclick], [contenteditable]';
  const clean = (s) => (s || '').replace(/\s+/g, ' ').trim().slice(0, 120);
  let max = 0;
  for (const el of document.querySelectorAll('[data-nexo-ref]')) {
    const n = parseInt(el.getAttribute('data-nexo-ref').slice(1), 10);
    if (n > max) max = n;
  }
  const out = [];
  for (const el of document.querySelectorAll(sel)) {
    if (el.type === 'hidden' || el.getClientRects().length === 0) continue;
    const style = getComputedStyle(el);
    if (style.visibility === 'hidden' || style.display === 'none') continue;
    const tag = el.tagName.toLowerCase();
    let role = el.getAttribute('role') || tag;
    if (!el.getAttribute('role') && tag === 'input') role = el.type || 'text';
    let name = clean(el.getAttribute('aria-label'));
    if (!name && el.getAttribute('aria-labelledby')) {
      name = clean(el.getAttribute('aria-labelledby').split(/\s+/)
        .map((id) => (document.getElementById(id) || {}).textContent).join(' '));
    }
    if (!name && el.labels && el.labels.length) name = clean(el.labels[0].textContent);
    if (!name && !['input', 'textarea', 'select'].includes(tag)) name = clean(el.innerText || el.textContent);
    if (!name) name = clean(el.placeholder);
    if (!name && tag === 'input' && ['button', 'submit', 'reset'].includes(el.type)) name = clean(el.value);
    if (!name) name = clean(el.title) || clean(el.alt) || clean(el.value);
    let ref = el.getAttribute('data-nexo-ref');
    if (!ref) {
      ref = 'e' + (++max);
      el.setAttribute('data-nexo-ref', ref);
    }
    out.push({ ref, role, name });
  }
  return JSON.stringify(out);
})()"#;
