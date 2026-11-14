use nexo_bin::Error;

use super::BinOp;

/// The ops that read a WebAssembly module (found by its signature, not its name).
pub(super) fn wasm_in(op: &BinOp) -> Result<String, Error> {
    let c = op.common();
    let (from, lines) = (c.from, c.budget());
    let mut ask = nexo_bin::WasmAsk { from, budget: lines, ..Default::default() };
    match op {
        BinOp::Info { .. } => ask.op = "info",
        BinOp::Overview { .. } => ask.op = "overview",
        BinOp::Funcs { sort, grep, min_size, .. } => {
            (ask.op, ask.sort, ask.grep, ask.min_size) = ("funcs", sort.as_str(), grep.as_deref(), *min_size)
        }
        BinOp::Fn { target, bytes, .. } => (ask.op, ask.target, ask.with_bytes) = ("fn", Some(target.as_str()), *bytes),
        BinOp::Calls { target, .. } => (ask.op, ask.target) = ("calls", Some(target.as_str())),
        BinOp::Xref { to: Some(_), .. } => {
            return Err(Error::Usage(
                "xref --to does not read WebAssembly modules yet; ask one address at a time".into(),
            ))
        }
        BinOp::Xref { target, str_text, insensitive, .. } => {
            (ask.op, ask.target, ask.str_text, ask.insensitive) =
                ("xref", target.as_deref(), str_text.as_deref(), *insensitive)
        }
        BinOp::Strings { grep, .. } => (ask.op, ask.grep) = ("strings", grep.as_deref()),
        _ => {
            return Err(Error::Usage(format!(
                "this op does not read WebAssembly modules yet; use one of: {}",
                nexo_bin::WASM_OPS.join(", ")
            )))
        }
    }
    let out = nexo_bin::wasm_answer(op.file(), &ask)?;
    let page = out.page_as(from, lines, c.json);
    if c.json {
        serde_json::to_string_pretty(&page.value).map_err(|e| Error::Bad(e.to_string()))
    } else {
        Ok(page.text())
    }
}
