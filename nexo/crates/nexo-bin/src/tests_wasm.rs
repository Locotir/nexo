//! Tests of the WebAssembly ops, on the module built in `wasm_fixture`.

use crate::wasm_fixture::{self as fx, HELLO};
use crate::*;

fn ask(op: &str, target: Option<&str>) -> String {
    let a = WasmAsk { op, target, budget: 200, ..WasmAsk::default() };
    let o = wasm_answer_bytes(&fx::module(), "/fx/m.wasm", &a).unwrap_or_else(|e| panic!("{op}: {e}"));
    o.page(None, 1000).text()
}

fn err(op: &str, target: Option<&str>) -> String {
    let a = WasmAsk { op, target, budget: 200, ..WasmAsk::default() };
    wasm_answer_bytes(&fx::module(), "/fx/m.wasm", &a).unwrap_err().to_string()
}

#[test]
fn info_lists_sections_imports_memory_tables_and_names() {
    let t = ask("info", None);
    assert!(t.contains("version 1") && t.contains("2 imports (2 functions), 3 defined functions"), "{t}");
    assert!(t.contains("memory[0] 1..2 pages") && t.contains("table[0] funcref 1.. entries"), "{t}");
    assert!(t.contains("global[0] mut i32") && t.contains("2 of 5 functions named"), "{t}");
    assert!(t.contains("custom name"), "{t}");
}

#[test]
fn funcs_name_imports_exports_and_demangled_rust_names() {
    let t = ask("funcs", None);
    assert!(t.contains("5 functions"), "{t}");
    assert!(t.contains("func[0] import") && t.contains("wasi_snapshot_preview1.fd_write"), "{t}");
    assert!(t.contains("demo::add") && t.contains("func[3]") && t.contains("greet"), "{t}");
    // no name section entry: the export gives the name
    assert!(t.contains("_start"), "{t}");
    assert!(t.contains("(i32, i32) -> i32"), "{t}");
}

#[test]
fn fn_disassembles_with_offsets_callee_names_branch_targets_and_strings() {
    let t = ask("fn", Some("greet"));
    assert!(t.contains("func[3] greet") && t.contains("() -> ()"), "{t}");
    assert!(t.contains("i32.const 1024") && t.contains("string? \"hello wasm\""), "{t}");
    assert!(t.contains("call 1") && t.contains("env.log"), "{t}");
    assert!(t.contains("br_if 1") && t.contains("-> end of block at 0x"), "{t}");
    assert!(t.contains("br 0") && t.contains("-> loop at 0x"), "{t}");
    assert!(t.contains("call 2") && t.contains("demo::add"), "{t}");
    // every instruction line starts with a hex file offset
    assert!(t.lines().skip(1).all(|l| l.trim_start().starts_with("0x")), "{t}");
    assert!(t.contains("global.get 0  ; $counter"), "{t}");
    let add = ask("fn", Some("add"));
    assert!(add.contains("local.get 0  ; $a") && add.contains("local.get 1  ; $b"), "{add}");
    let t = ask("fn", Some("4"));
    assert!(t.contains("call_indirect type 0 table 0") && t.contains("not known statically"), "{t}");
}

#[test]
fn fn_resolves_an_index_a_hex_offset_and_a_part_of_a_name() {
    let by_index = ask("fn", Some("2"));
    assert!(by_index.contains("local.get 0") && by_index.contains("i32.add"), "{by_index}");
    let off = by_index.lines().nth(2).unwrap().split_whitespace().next().unwrap().to_string();
    assert_eq!(ask("fn", Some(&off)), by_index);
    assert_eq!(ask("fn", Some("add")), by_index);
    assert!(err("fn", Some("99")).contains("the module has 5"));
    assert!(err("fn", Some("0x1")).contains("not inside any function body"));
    assert!(err("fn", Some("nothere")).contains("no function named"));
    assert!(err("fn", Some("e")).contains("functions contain"), "ambiguous names are listed, not picked");
    assert!(err("fn", None).contains("give a function"));
}

#[test]
fn fn_of_an_import_says_there_is_no_body() {
    let t = ask("fn", Some("env.log"));
    assert!(t.contains("imported from module \"env\"") && t.contains("1 direct callers"), "{t}");
}

#[test]
fn calls_and_xref_follow_direct_indirect_and_table_edges() {
    let t = ask("calls", Some("greet"));
    assert!(t.contains("1 callers") && t.contains("func[4] _start"), "{t}");
    assert!(t.contains("func[1] env.log") && t.contains("func[2] demo::add"), "{t}");
    let t = ask("calls", Some("_start"));
    assert!(t.contains("call_indirect at 0x") && t.contains("(i32, i32) -> i32"), "{t}");
    let t = ask("calls", Some("add"));
    assert!(t.contains("sits in a function table"), "{t}");
    let t = ask("xref", Some("greet"));
    assert!(t.contains("1 direct call sites") && t.contains("in func[4] _start"), "{t}");
    let t = ask("xref", Some("_start"));
    assert!(t.contains("export \"_start\""), "{t}");
}

#[test]
fn strings_come_from_data_segments_with_memory_addresses_and_users() {
    let t = ask("strings", None);
    assert!(t.contains(&format!("mem {:#x}", fx::HELLO_ADDR)) && t.contains(HELLO), "{t}");
    assert!(t.contains("<- func[3]@0x"), "greet loads the first string: {t}");
    assert!(t.contains(fx::SECOND) && t.contains("<- func[4]@0x"), "{t}");
    let a = WasmAsk { op: "strings", grep: Some("^second"), budget: 50, ..WasmAsk::default() };
    let o = wasm_answer_bytes(&fx::module(), "/fx/m.wasm", &a).unwrap();
    let p = o.page(None, 100);
    assert_eq!(p.value["items"].as_array().unwrap().len(), 1);
    let a = WasmAsk { op: "xref", str_text: Some("HELLO"), insensitive: true, budget: 50, ..WasmAsk::default() };
    let t = wasm_answer_bytes(&fx::module(), "/fx/m.wasm", &a).unwrap().page(None, 100).text();
    assert!(t.contains("1 strings contain") && t.contains("i32.const in func[3] greet"), "{t}");
}

#[test]
fn overview_maps_entry_imports_by_capability_and_hot_functions() {
    let t = ask("overview", None);
    assert!(t.contains("3 functions + 2 imported") && t.contains("export _start = func[4]"), "{t}");
    assert!(t.contains("files: wasi_snapshot_preview1.fd_write"), "{t}");
    assert!(t.contains("host (JavaScript glue): env.log"), "{t}");
    assert!(t.contains("most called") && t.contains("strings used by code"), "{t}");
}

#[test]
fn a_cut_answer_names_the_command_to_resume_it() {
    let a = WasmAsk { op: "funcs", sort: "addr", budget: 10, ..WasmAsk::default() };
    let o = wasm_answer_bytes(&fx::module(), "/fx/m.wasm", &a).unwrap();
    assert!(o.command.starts_with("nexo bin funcs /fx/m.wasm"), "{}", o.command);
    // five functions plus the heading fit in ten lines
    assert!(o.page(None, 10).next.is_none());
    let t = o.page(None, 10).text();
    assert!(t.contains("5 functions") && t.contains("(sorted by addr)"), "{t}");
}

#[test]
fn bad_input_names_what_is_wrong() {
    let e = |d: &[u8]| {
        wasm_answer_bytes(d, "x.wasm", &WasmAsk { op: "info", budget: 20, ..WasmAsk::default() })
            .unwrap_err()
            .to_string()
    };
    assert!(e(b"hello world").contains("no \\0asm signature"));
    let m = fx::module();
    let cut = e(&m[..m.len() - 30]);
    assert!(cut.contains("not a readable WebAssembly module") && cut.contains("file offset"), "{cut}");
    assert!(err("decomp", None).contains("use one of: info, overview, funcs, fn, calls, xref, strings"));
}

#[test]
fn a_body_that_does_not_decode_is_reported_with_its_offset() {
    let mut m = fx::module();
    // turn the `i32.add` of func 2 into an opcode that does not exist
    let at = m.windows(5).position(|w| w == [0x20, 0, 0x20, 1, 0x6a]).unwrap() + 4;
    m[at] = 0xff;
    let a = WasmAsk { op: "info", budget: 50, ..WasmAsk::default() };
    let t = wasm_answer_bytes(&m, "x.wasm", &a).unwrap().page(None, 100).text();
    assert!(t.contains("problem") && t.contains("1 function bodies do not decode (first: func[2]"), "{t}");
    let a = WasmAsk { op: "fn", target: Some("2"), budget: 50, ..WasmAsk::default() };
    let t = wasm_answer_bytes(&m, "x.wasm", &a).unwrap().page(None, 100).text();
    assert!(t.contains("local.get 1") && t.contains("decoding stops at 0x"), "{t}");
}

#[test]
fn mnemonics_follow_the_text_format() {
    use crate::wasm_dis::mnemonic;
    for (dbg, want) in [
        ("I32Const { value: 1 }", "i32.const"),
        ("F64ConvertI32S", "f64.convert_i32_s"),
        ("I32TruncSatF32S", "i32.trunc_sat_f32_s"),
        ("I64Load32S { memarg: x }", "i64.load32_s"),
        ("BrIf { relative_depth: 1 }", "br_if"),
        ("LocalGet { local_index: 0 }", "local.get"),
        ("MemoryGrow { mem: 0 }", "memory.grow"),
        ("CallIndirect { .. }", "call_indirect"),
        ("Drop", "drop"),
        ("I8x16Splat", "i8x16.splat"),
    ] {
        assert_eq!(mnemonic(dbg), want, "{dbg}");
    }
}
