use nexo_core::{ToolDisplay, ToolOutput};

use super::{FILE_CAP, LINE_CHARS};
use crate::util::cut_line;
/// Parse Rust rather than guessing brace boundaries in comments, strings or macros.
pub(super) fn read_symbol(shown: &str, text: &str, name: &str) -> ToolOutput {
    use syn::spanned::Spanned;
    use syn::visit::{self, Visit};
    struct Find<'a> {
        name: &'a str,
        spans: Vec<(usize, usize)>,
    }
    impl<'ast> Visit<'ast> for Find<'_> {
        fn visit_item(&mut self, item: &'ast syn::Item) {
            let ident = match item {
                syn::Item::Fn(i) => Some(&i.sig.ident),
                syn::Item::Struct(i) => Some(&i.ident),
                syn::Item::Enum(i) => Some(&i.ident),
                syn::Item::Trait(i) => Some(&i.ident),
                syn::Item::Mod(i) => Some(&i.ident),
                syn::Item::Type(i) => Some(&i.ident),
                syn::Item::Const(i) => Some(&i.ident),
                syn::Item::Static(i) => Some(&i.ident),
                syn::Item::Union(i) => Some(&i.ident),
                syn::Item::Macro(i) => i.ident.as_ref(),
                _ => None,
            };
            if ident.is_some_and(|i| i == self.name) {
                self.add(item.span());
            }
            visit::visit_item(self, item);
        }
        fn visit_impl_item_fn(&mut self, item: &'ast syn::ImplItemFn) {
            if item.sig.ident == self.name {
                self.add(item.span());
            }
            visit::visit_impl_item_fn(self, item);
        }
        fn visit_trait_item_fn(&mut self, item: &'ast syn::TraitItemFn) {
            if item.sig.ident == self.name {
                self.add(item.span());
            }
            visit::visit_trait_item_fn(self, item);
        }
    }
    impl Find<'_> {
        fn add(&mut self, span: proc_macro2::Span) {
            self.spans.push((span.start().line, span.end().line));
        }
    }
    let file = match syn::parse_file(text) {
        Ok(file) => file,
        Err(e) => return ToolOutput::error(format!("read: cannot parse {shown} as Rust: {e}; use line ranges")),
    };
    let mut find = Find { name, spans: Vec::new() };
    find.visit_file(&file);
    match find.spans.as_slice() {
        [] => ToolOutput::error(format!("read: symbol {name:?} not found in {shown}")),
        [(start, end)] => {
            let (start, end) = (*start, *end);
            let mut out = String::new();
            let mut raw = String::new();
            let mut last = start - 1;
            let mut chars = 0;
            for (i, line) in text.lines().enumerate().skip(start - 1).take(end - start + 1) {
                let line = cut_line(line, LINE_CHARS);
                let numbered = format!("{:>6}│ {line}\n", i + 1);
                let len = numbered.chars().count();
                if last >= start && chars + len > FILE_CAP {
                    break;
                }
                out.push_str(&numbered);
                raw.push_str(&line);
                raw.push('\n');
                chars += len;
                last = i + 1;
            }
            if last < end {
                out.push_str(&format!(
                    "… symbol truncated at line {last} of {end}; read with offset {} and limit {} to continue\n",
                    last + 1,
                    end - last
                ));
            }
            ToolOutput::ok(out).with_display(ToolDisplay::File { path: shown.into(), text: raw, start_line: start })
        }
        spans => {
            let ranges = spans.iter().take(100).map(|(a, b)| format!("{a}-{b}")).collect::<Vec<_>>().join(", ");
            ToolOutput::error(format!(
                "read: symbol {name:?} is ambiguous in {shown} ({} matches): lines {ranges}{}; use offset and limit",
                spans.len(),
                if spans.len() > 100 { ", …" } else { "" }
            ))
        }
    }
}
