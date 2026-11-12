//! A small WebAssembly module built byte by byte, for tests (no binary file is committed).
//!
//! Function index space: 0 `wasi_snapshot_preview1.fd_write` and 1 `env.log` (imports), then
//! 2 `demo::add` (mangled in the name section), 3 `greet` (calls log with the string at [`HELLO_ADDR`]
//! and add, loops and branches), 4 `_start` (exported; calls greet and add, and does a `call_indirect`).
//! Memory holds [`HELLO`] at [`HELLO_ADDR`] and a second string at [`SECOND_ADDR`].

/// First string and where an active data segment puts it in linear memory.
pub const HELLO: &str = "hello wasm";
pub const HELLO_ADDR: u32 = 1024;
pub const SECOND: &str = "second string";
pub const SECOND_ADDR: u32 = 2048;

fn leb(mut v: u32, out: &mut Vec<u8>) {
    loop {
        let b = (v & 0x7f) as u8;
        v >>= 7;
        if v == 0 {
            out.push(b);
            return;
        }
        out.push(b | 0x80);
    }
}

fn sleb(mut v: i32, out: &mut Vec<u8>) {
    loop {
        let b = (v & 0x7f) as u8;
        v >>= 7;
        if (v == 0 && b & 0x40 == 0) || (v == -1 && b & 0x40 != 0) {
            out.push(b);
            return;
        }
        out.push(b | 0x80);
    }
}

fn name(s: &str, out: &mut Vec<u8>) {
    leb(s.len() as u32, out);
    out.extend_from_slice(s.as_bytes());
}

fn section(id: u8, body: Vec<u8>, out: &mut Vec<u8>) {
    out.push(id);
    leb(body.len() as u32, out);
    out.extend(body);
}

fn vec_of(items: Vec<Vec<u8>>) -> Vec<u8> {
    let mut b = Vec::new();
    leb(items.len() as u32, &mut b);
    for i in items {
        b.extend(i);
    }
    b
}

fn body(code: Vec<u8>) -> Vec<u8> {
    let mut b = vec![0u8]; // no locals
    b.extend(code);
    b.push(0x0b);
    let mut out = Vec::new();
    leb(b.len() as u32, &mut out);
    out.extend(b);
    out
}

/// The module described above.
pub fn module() -> Vec<u8> {
    let mut m = b"\0asm\x01\0\0\0".to_vec();
    // types: 0 (i32 i32) -> i32, 1 (i32) -> (), 2 () -> ()
    section(1, vec_of(vec![vec![0x60, 2, 0x7f, 0x7f, 1, 0x7f], vec![0x60, 1, 0x7f, 0], vec![0x60, 0, 0]]), &mut m);
    let imp = |module: &str, field: &str, ty: u32| {
        let mut b = Vec::new();
        name(module, &mut b);
        name(field, &mut b);
        b.push(0); // function
        leb(ty, &mut b);
        b
    };
    section(2, vec_of(vec![imp("wasi_snapshot_preview1", "fd_write", 0), imp("env", "log", 1)]), &mut m);
    section(3, vec_of(vec![vec![0], vec![2], vec![2]]), &mut m); // add: t0, greet: t2, _start: t2
    section(4, vec_of(vec![vec![0x70, 0, 1]]), &mut m); // table funcref min 1
    section(5, vec_of(vec![vec![1, 1, 2]]), &mut m); // memory min 1 max 2
    section(6, vec_of(vec![vec![0x7f, 1, 0x41, 0, 0x0b]]), &mut m); // mutable i32 global = 0
    let export = |n: &str, kind: u8, idx: u32| {
        let mut b = Vec::new();
        name(n, &mut b);
        b.push(kind);
        leb(idx, &mut b);
        b
    };
    section(7, vec_of(vec![export("_start", 0, 4), export("memory", 2, 0)]), &mut m);
    // element: active, table 0, offset 0, function 2
    section(9, vec_of(vec![vec![0, 0x41, 0, 0x0b, 1, 2]]), &mut m);
    // code
    let add = body(vec![0x20, 0, 0x20, 1, 0x6a]); // local.get 0, local.get 1, i32.add
    let mut greet = vec![0x41];
    sleb(HELLO_ADDR as i32, &mut greet);
    greet.extend([0x10, 1]); // call env.log
    greet.extend([0x02, 0x40]); // block
    greet.extend([0x03, 0x40]); // loop
    greet.extend([0x23, 0, 0x45, 0x0d, 1]); // global.get 0, i32.eqz, br_if 1 (leaves the block)
    greet.extend([0x0c, 0]); // br 0 (back to the loop)
    greet.extend([0x0b, 0x0b]); // end loop, end block
    greet.extend([0x41, 1, 0x41, 2, 0x10, 2, 0x1a]); // add(1, 2), drop
    let greet = body(greet);
    let mut start = vec![0x10, 3]; // call greet
    start.extend([0x41]);
    sleb(SECOND_ADDR as i32, &mut start);
    start.extend([0x1a]); // drop
    start.extend([0x41, 3, 0x41, 4, 0x41, 0, 0x11, 0, 0]); // 3, 4, 0 -> call_indirect type 0 table 0
    start.extend([0x1a]);
    let start = body(start);
    section(10, vec_of(vec![add, greet, start]), &mut m);
    // data: two active segments in memory 0
    let seg = |addr: u32, text: &str| {
        let mut b = vec![0, 0x41];
        sleb(addr as i32, &mut b);
        b.push(0x0b);
        leb(text.len() as u32 + 1, &mut b);
        b.extend(text.as_bytes());
        b.push(0);
        b
    };
    section(11, vec_of(vec![seg(HELLO_ADDR, HELLO), seg(SECOND_ADDR, SECOND)]), &mut m);
    // name section: function names (subsection 1): 2 = a Rust-mangled name, 3 = greet
    let mut names = Vec::new();
    name("name", &mut names);
    let mut sub = Vec::new();
    let mut entries = Vec::new();
    for (i, n) in [(2u32, "_ZN4demo3add17h0123456789abcdefE"), (3, "greet")] {
        leb(i, &mut entries);
        name(n, &mut entries);
    }
    leb(2, &mut sub);
    sub.extend(entries);
    names.push(1);
    leb(sub.len() as u32, &mut names);
    names.extend(sub);
    section(0, names, &mut m);
    m
}
