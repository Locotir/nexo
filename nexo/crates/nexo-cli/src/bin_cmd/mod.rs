//! `nexo bin`: read an ELF, PE or Mach-O file as bytes and answer in bounded pieces.
//! The file is never run or loaded. See the `nexo-bin` crate.

use std::path::Path;

use nexo_bin::{Error, FnQuery, FuncSort, Image, Lines, Output, PageAsk, StrSort};

use sigs::make_sigs;
use wasm::wasm_in;

mod args;
pub(crate) mod bridge;
mod ops;
mod sigs;
#[cfg(test)]
mod tests;
mod wasm;

pub use args::{BinArgs, Common};
pub use ops::BinOp;

fn parse_addr(s: &str) -> Result<u64, String> {
    nexo_bin::parse_addr(s).map_err(|e| e.to_string())
}

fn parse_lines(s: &str) -> Result<Lines, String> {
    Lines::parse(s).map_err(|e| e.to_string())
}

impl BinOp {
    /// The page this call asks for, and the `--grep` the op applies itself: `funcs`, `strings` and the lists of
    /// `diff` and `managed` match it against names or text before they count; every other answer has its rows
    /// filtered by the page.
    pub(crate) fn page_ask(&self) -> (PageAsk, Option<String>) {
        let sized = match self {
            BinOp::Fn { to, target, .. } => to.is_some() || nexo_bin::is_span(target),
            BinOp::Xref { to, target, .. } => to.is_some() || target.as_deref().is_some_and(nexo_bin::is_span),
            BinOp::Find { within, .. } => within.is_some(),
            BinOp::Bytes { .. } => true,
            _ => false,
        };
        let mut ask = self.common().ask(sized);
        let own = matches!(
            self,
            BinOp::Funcs { .. }
                | BinOp::Strings { .. }
                | BinOp::Diff { target: None, .. }
                | BinOp::Managed { target: None, .. }
        );
        let op_grep = if own { ask.grep.take() } else { None };
        (ask, op_grep)
    }

    fn file(&self) -> &Path {
        match self {
            BinOp::Patch { file, .. }
            | BinOp::Info { file, .. }
            | BinOp::Syms { file, .. }
            | BinOp::Overview { file, .. }
            | BinOp::Path { file, .. }
            | BinOp::Slice { file, .. }
            | BinOp::Crypto { file, .. }
            | BinOp::Tree { file, .. }
            | BinOp::Fn { file, .. }
            | BinOp::Bytes { file, .. }
            | BinOp::Strings { file, .. }
            | BinOp::Xref { file, .. }
            | BinOp::Funcs { file, .. }
            | BinOp::Decomp { file, .. }
            | BinOp::Proto { file, .. }
            | BinOp::Struct { file, .. }
            | BinOp::Var { file, .. }
            | BinOp::Calls { file, .. }
            | BinOp::Cfg { file, .. }
            | BinOp::Name { file, .. }
            | BinOp::Note { file, .. }
            | BinOp::Names { file, .. }
            | BinOp::Find { file, .. }
            | BinOp::Diff { file, .. }
            | BinOp::Managed { file, .. }
            | BinOp::Emu { file, .. } => file,
            BinOp::Sigs { .. } | BinOp::Asm { .. } => Path::new(""),
        }
    }

    /// The files the op reads, to be made absolute against a session's folder: the second build of `diff`, the
    /// libraries of `sigs`.
    pub fn files_mut(&mut self) -> Vec<&mut std::path::PathBuf> {
        match self {
            BinOp::Diff { file, other, .. } => vec![file, other],
            BinOp::Patch { file, .. }
            | BinOp::Info { file, .. }
            | BinOp::Syms { file, .. }
            | BinOp::Overview { file, .. }
            | BinOp::Path { file, .. }
            | BinOp::Slice { file, .. }
            | BinOp::Crypto { file, .. }
            | BinOp::Tree { file, .. }
            | BinOp::Fn { file, .. }
            | BinOp::Bytes { file, .. }
            | BinOp::Strings { file, .. }
            | BinOp::Xref { file, .. }
            | BinOp::Funcs { file, .. }
            | BinOp::Decomp { file, .. }
            | BinOp::Proto { file, .. }
            | BinOp::Struct { file, .. }
            | BinOp::Var { file, .. }
            | BinOp::Calls { file, .. }
            | BinOp::Cfg { file, .. }
            | BinOp::Name { file, .. }
            | BinOp::Note { file, .. }
            | BinOp::Names { file, .. }
            | BinOp::Find { file, .. }
            | BinOp::Managed { file, .. }
            | BinOp::Emu { file, .. } => vec![file],
            BinOp::Sigs { libs, .. } => libs.iter_mut().collect(),
            BinOp::Asm { .. } => Vec::new(),
        }
    }

    /// The file the op writes, to be made absolute against a session's folder: a patched copy, or a signature file.
    pub fn out_mut(&mut self) -> Option<&mut std::path::PathBuf> {
        match self {
            BinOp::Patch { out, .. } => Some(out),
            BinOp::Sigs { out, .. } => out.as_mut(),
            _ => None,
        }
    }

    fn common(&self) -> &Common {
        match self {
            BinOp::Asm { common, .. }
            | BinOp::Patch { common, .. }
            | BinOp::Info { common, .. }
            | BinOp::Syms { common, .. }
            | BinOp::Overview { common, .. }
            | BinOp::Path { common, .. }
            | BinOp::Slice { common, .. }
            | BinOp::Crypto { common, .. }
            | BinOp::Tree { common, .. }
            | BinOp::Fn { common, .. }
            | BinOp::Bytes { common, .. }
            | BinOp::Strings { common, .. }
            | BinOp::Xref { common, .. }
            | BinOp::Funcs { common, .. }
            | BinOp::Decomp { common, .. }
            | BinOp::Proto { common, .. }
            | BinOp::Struct { common, .. }
            | BinOp::Var { common, .. }
            | BinOp::Calls { common, .. }
            | BinOp::Cfg { common, .. }
            | BinOp::Name { common, .. }
            | BinOp::Note { common, .. }
            | BinOp::Names { common, .. }
            | BinOp::Find { common, .. }
            | BinOp::Diff { common, .. }
            | BinOp::Sigs { common, .. }
            | BinOp::Managed { common, .. }
            | BinOp::Emu { common, .. } => common,
        }
    }
}

/// Answer one question: the text (or JSON) to print. Names and notes live in nexo's data dir.
pub fn execute(a: &BinArgs) -> Result<String, Error> {
    execute_in(a, &nexo_core::paths::data_dir())
}

/// Like [`execute`], with the data dir given (tests use a temporary one).
pub fn execute_in(a: &BinArgs, data_dir: &Path) -> Result<String, Error> {
    let op = a.op.as_ref().ok_or_else(|| Error::Usage("choose a bin operation or use --mcp".into()))?;
    let t0 = std::time::Instant::now();
    if let BinOp::Asm { source, at, bits, .. } = op {
        let output = nexo_bin::assembly(source, *at, if bits == "32" { 32 } else { 64 })?;
        return render_copy_op(&output, op.common().json);
    }
    if let BinOp::Sigs { libs, all, out, .. } = op {
        return make_sigs(libs, *all, out.as_deref(), data_dir);
    }
    if let Some(r) = crate::bin_managed::try_managed(op) {
        return r;
    }
    if nexo_bin::is_wasm(op.file()) {
        return wasm_in(op);
    }
    let mut img = Image::open_arch(op.file(), op.common().arch.as_deref())?;
    let mut warning = nexo_bin::attach_names(&mut img, data_dir);
    let mut other = None;
    if let BinOp::Diff { other: path, .. } = op {
        let mut b = Image::open(path)?;
        if let Some(w) = nexo_bin::attach_names(&mut b, data_dir).filter(|w| warning.as_ref() != Some(w)) {
            warning = Some(warning.map_or(w.clone(), |v| format!("{v}\n{w}")));
        }
        other = Some(b);
    }
    let c = op.common();
    let (ask, op_grep) = op.page_ask();
    let (from, lines) = ask.window();
    let out: Output = match op {
        BinOp::Asm { .. } => unreachable!("assembled before opening an image"),
        BinOp::Patch { out, at, function, bytes, asm, nop, .. } => {
            let target = match (at, function) {
                (Some(at), None) => nexo_bin::PatchTarget::Address(nexo_bin::any_addr_in(&img, at)?),
                (None, Some(name)) => nexo_bin::PatchTarget::Function(name),
                _ => return Err(Error::Usage("give exactly one of --at or --fn".into())),
            };
            let data = match (bytes, asm, nop) {
                (Some(bytes), None, None) => nexo_bin::PatchData::Bytes(bytes),
                (None, Some(asm), None) => nexo_bin::PatchData::Asm(asm),
                (None, None, Some(n)) => nexo_bin::PatchData::Nop(*n),
                _ => return Err(Error::Usage("give exactly one of --bytes, --asm or --nop".into())),
            };
            return render_copy_op(&nexo_bin::patch(&img, out, target, data)?, c.json);
        }
        BinOp::Info { .. } => nexo_bin::info(&img),
        BinOp::Syms { kind, .. } => nexo_bin::syms_window(&img, kind, from, lines)?,
        BinOp::Overview { .. } => nexo_bin::overview_window(&img, from, lines)?,
        BinOp::Crypto { save, .. } => nexo_bin::crypto_window(&img, save.then_some(data_dir), from, lines)?,
        BinOp::Slice { at, reg, mem, forward: true, .. } => {
            nexo_bin::slice_forward(&img, at, reg.as_deref(), mem.as_deref(), from, lines)?
        }
        BinOp::Slice { at, reg, arg, mem, .. } => {
            nexo_bin::slice_window(&img, at, reg.as_deref(), *arg, mem.as_deref(), from, lines)?
        }
        BinOp::Path { from_fn, to_fn, ptrs, routes, .. } => {
            nexo_bin::path_window(&img, from_fn, to_fn, *ptrs, *routes, from, lines)?
        }
        BinOp::Tree { target, up, depth, ptrs, .. } => {
            nexo_bin::tree_window(&img, target, *up, *depth, *ptrs, from, lines)?
        }
        BinOp::Fn { target, bytes, to, code, .. } => nexo_bin::func_view(
            &img,
            &FnQuery {
                to: to.as_deref(),
                as_code: *code,
                with_bytes: *bytes,
                from,
                budget: lines,
                json: c.json,
                ..FnQuery::new(target)
            },
        )?,
        BinOp::Xref { target, to, str_text, insensitive, .. } => match (str_text, target) {
            (Some(t), _) => nexo_bin::xref_str_window(&img, t, *insensitive, from, lines)?,
            (None, Some(t)) => match (to, nexo_bin::is_span(t)) {
                (Some(to), _) => nexo_bin::xref_range_window(&img, t, to, from, lines)?,
                // `A..B`: B is the address the range stops before, whatever it names
                (None, true) => {
                    let (lo, hi) = nexo_bin::range_in(&img, t)?;
                    nexo_bin::xref_range_window(&img, &format!("{lo:#x}"), &format!("{hi:#x}"), from, lines)?
                }
                (None, false) => nexo_bin::xref_window(&img, t, from, lines)?,
            },
            (None, None) => return Err(Error::Usage("give an address or a name, or --str TEXT".into())),
        },
        BinOp::Funcs { sort, min_size, .. } => {
            nexo_bin::func_list_window(&img, FuncSort::parse(sort)?, op_grep.as_deref(), *min_size, from, lines)?
        }
        BinOp::Decomp { targets, addr, .. } => nexo_bin::decomp_window(&img, data_dir, targets, *addr, from, lines)?,
        BinOp::Proto { target, text, clear, .. } => {
            nexo_bin::proto_set(&img, data_dir, target, if *clear { None } else { text.as_deref() })?
        }
        BinOp::Struct { name, fields, clear, .. } => {
            nexo_bin::struct_set(&img, data_dir, name, if *clear { None } else { fields.as_deref() })?
        }
        BinOp::Var { target, key, name, clear, .. } => {
            nexo_bin::var_set(&img, data_dir, target, key, if *clear { None } else { name.as_deref() })?
        }
        BinOp::Calls { target, .. } => nexo_bin::calls_window(&img, target, from, lines)?,
        BinOp::Cfg { target, .. } => nexo_bin::cfg_op(&img, target)?,
        BinOp::Name { target, name, clear, .. } => match (clear, name) {
            (false, Some(n)) => nexo_bin::name_set(&img, data_dir, target, n)?,
            _ => nexo_bin::name_clear(&img, data_dir, target)?,
        },
        BinOp::Note { target, text, clear, .. } => {
            if *clear {
                nexo_bin::note_clear(&img, data_dir, target)?
            } else {
                nexo_bin::note_set(&img, data_dir, target, &text.join(" "))?
            }
        }
        BinOp::Sigs { .. } => unreachable!("handled before the file is opened"),
        BinOp::Names { .. } => nexo_bin::names_list_window(&img, from, lines),
        BinOp::Bytes { addr, len, ty, .. } => {
            let view = nexo_bin::resolve_type(&img, ty)?;
            let (at, n) = match (len, view.fixed_size()) {
                (Some(n), _) => (nexo_bin::any_addr_in(&img, addr)?, *n),
                // a struct or `T[N]` is as long as its type: one place is enough
                (None, Some(size)) if !nexo_bin::is_span(addr) => (nexo_bin::any_addr_in(&img, addr)?, size),
                (None, _) => nexo_bin::range_in(&img, addr).map(|(lo, hi)| (lo, hi - lo))?,
            };
            nexo_bin::bytes(&img, at, n, ty)?
        }
        BinOp::Strings { sort, .. } => {
            nexo_bin::strings_window(&img, op_grep.as_deref(), StrSort::parse(sort)?, from, lines)?
        }
        BinOp::Emu { target, args, to, regs, mems, stubs, abi, steps, secs, trace, dump, stack, show, .. } => {
            nexo_bin::emulate(
                &img,
                &nexo_bin::EmuSpec {
                    args: args.clone(),
                    to: to.as_deref().map(|t| nexo_bin::any_addr_in(&img, t)).transpose()?,
                    regs: regs.clone(),
                    mem: mems.clone(),
                    stubs: stubs.clone(),
                    abi: abi.clone(),
                    steps: *steps,
                    secs: *secs,
                    trace: *trace,
                    dump: *dump,
                    stack: *stack,
                    show: show.clone(),
                    ..nexo_bin::EmuSpec::new(target)
                },
            )?
        }
        BinOp::Managed { .. } => unreachable!("answered by bin_managed before the image is opened"),
        BinOp::Find { pattern, imm, within, .. } => match imm {
            Some(n) => nexo_bin::find_imm_in(&img, nexo_bin::parse_number(n)?, within.as_deref())?,
            None => nexo_bin::find_bytes_in(&img, &pattern.join(" "), within.as_deref())?,
        },
        BinOp::Diff { target, all, .. } => match &other {
            Some(b) => nexo_bin::diff_window(&img, b, target.as_deref(), op_grep.as_deref(), *all, from, lines)?,
            None => unreachable!("the second build is opened with the first"),
        },
    };
    let page = ask.page(&out)?;
    if c.timing {
        let built = img.refs().build.as_secs_f64() * 1000.0;
        eprintln!(
            "timing: references built in {built:.1} ms; whole answer {:.1} ms",
            t0.elapsed().as_secs_f64() * 1000.0
        );
    }
    if c.json {
        let mut value = page.value;
        if let (Some(w), Some(o)) = (&warning, value.as_object_mut()) {
            o.insert("warning".into(), serde_json::Value::String(w.clone()));
        }
        serde_json::to_string_pretty(&value).map_err(|e| Error::Bad(e.to_string()))
    } else {
        let mut text = page.text();
        if let Some(w) = warning {
            text.push('\n');
            text.push_str(&w);
        }
        Ok(text)
    }
}

// Copy operations print their complete receipt; pagination must never suggest repeating a write.
fn render_copy_op(out: &Output, json: bool) -> Result<String, Error> {
    if json {
        serde_json::to_string_pretty(&out.page(None, usize::MAX).value).map_err(|e| Error::Bad(e.to_string()))
    } else {
        Ok(out.text_all())
    }
}

/// Print the answer; the exit code is 0, 1 for a question that cannot be answered, 2 for a file that is not read.
pub fn run(a: &BinArgs) -> i32 {
    if a.mcp {
        return crate::re_mcp::run_stdio(a.allow_changes);
    }
    match execute(a) {
        Ok(text) => {
            println!("{text}");
            0
        }
        Err(e) => {
            eprintln!("nexo bin: {e}");
            e.exit_code()
        }
    }
}
