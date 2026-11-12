//! A tiny class-file and jar writer for tests: the layout is the one in the JVM specification
//! (chapter 4), so the tests need no compiler.

use std::collections::HashMap;

#[derive(Default)]
pub struct ClassBuilder {
    pool: Vec<u8>,
    count: u16,
    seen: HashMap<String, u16>,
    methods: Vec<u8>,
    n_methods: u16,
    fields: Vec<u8>,
    n_fields: u16,
    this: u16,
    sup: u16,
    pub major: u16,
}

fn u2(v: &mut Vec<u8>, x: u16) {
    v.extend_from_slice(&x.to_be_bytes());
}
fn u4(v: &mut Vec<u8>, x: u32) {
    v.extend_from_slice(&x.to_be_bytes());
}

impl ClassBuilder {
    pub fn new(name: &str, sup: &str) -> Self {
        let mut b = ClassBuilder { count: 1, major: 52, ..Default::default() };
        b.this = b.class(name);
        b.sup = b.class(sup);
        b
    }
    fn add(&mut self, key: String, bytes: &[u8]) -> u16 {
        if let Some(&i) = self.seen.get(&key) {
            return i;
        }
        self.pool.extend_from_slice(bytes);
        let i = self.count;
        self.count += 1;
        self.seen.insert(key, i);
        i
    }
    pub fn utf8(&mut self, s: &str) -> u16 {
        let mut b = vec![1];
        u2(&mut b, s.len() as u16);
        b.extend_from_slice(s.as_bytes());
        self.add(format!("u{s}"), &b)
    }
    pub fn class(&mut self, n: &str) -> u16 {
        let u = self.utf8(n);
        let mut b = vec![7];
        u2(&mut b, u);
        self.add(format!("c{n}"), &b)
    }
    pub fn string(&mut self, s: &str) -> u16 {
        let u = self.utf8(s);
        let mut b = vec![8];
        u2(&mut b, u);
        self.add(format!("s{s}"), &b)
    }
    pub fn name_type(&mut self, n: &str, t: &str) -> u16 {
        let (n, t) = (self.utf8(n), self.utf8(t));
        let mut b = vec![12];
        u2(&mut b, n);
        u2(&mut b, t);
        self.add(format!("n{n}:{t}"), &b)
    }
    /// tag 9 field, 10 method, 11 interface method.
    pub fn member(&mut self, tag: u8, owner: &str, name: &str, desc: &str) -> u16 {
        let (c, nt) = (self.class(owner), self.name_type(name, desc));
        let mut b = vec![tag];
        u2(&mut b, c);
        u2(&mut b, nt);
        self.add(format!("m{tag}{owner}.{name}{desc}"), &b)
    }
    pub fn field(&mut self, acc: u16, name: &str, desc: &str) {
        let (n, d) = (self.utf8(name), self.utf8(desc));
        u2(&mut self.fields, acc);
        u2(&mut self.fields, n);
        u2(&mut self.fields, d);
        u2(&mut self.fields, 0);
        self.n_fields += 1;
    }
    /// A method with code; `lines` are `(start_pc, line)` pairs for a LineNumberTable.
    pub fn method(&mut self, acc: u16, name: &str, desc: &str, code: &[u8], lines: &[(u16, u16)]) {
        let (n, d, ca) = (self.utf8(name), self.utf8(desc), self.utf8("Code"));
        u2(&mut self.methods, acc);
        u2(&mut self.methods, n);
        u2(&mut self.methods, d);
        u2(&mut self.methods, 1);
        let mut body = Vec::new();
        u2(&mut body, 4);
        u2(&mut body, 4);
        u4(&mut body, code.len() as u32);
        body.extend_from_slice(code);
        u2(&mut body, 0);
        if lines.is_empty() {
            u2(&mut body, 0);
        } else {
            u2(&mut body, 1);
            let lt = self.utf8("LineNumberTable");
            u2(&mut body, lt);
            u4(&mut body, 2 + 4 * lines.len() as u32);
            u2(&mut body, lines.len() as u16);
            for (pc, l) in lines {
                u2(&mut body, *pc);
                u2(&mut body, *l);
            }
        }
        u2(&mut self.methods, ca);
        u4(&mut self.methods, body.len() as u32);
        self.methods.extend_from_slice(&body);
        self.n_methods += 1;
    }
    pub fn build(&self) -> Vec<u8> {
        let mut o = vec![0xCA, 0xFE, 0xBA, 0xBE];
        u2(&mut o, 0);
        u2(&mut o, self.major);
        u2(&mut o, self.count);
        o.extend_from_slice(&self.pool);
        u2(&mut o, 0x21);
        u2(&mut o, self.this);
        u2(&mut o, self.sup);
        u2(&mut o, 0);
        u2(&mut o, self.n_fields);
        o.extend_from_slice(&self.fields);
        u2(&mut o, self.n_methods);
        o.extend_from_slice(&self.methods);
        u2(&mut o, 0);
        o
    }
}

/// The demo class `demo/Calc`: add, greet (prints a string), pick (a six-way tableswitch), main
/// (calls add), and broken (an opcode that does not exist).
pub fn demo_class() -> Vec<u8> {
    let mut b = ClassBuilder::new("demo/Calc", "java/lang/Object");
    b.field(0x19, "LIMIT", "I");
    b.field(0x02, "name", "Ljava/lang/String;");
    let out = b.member(9, "java/lang/System", "out", "Ljava/io/PrintStream;");
    let println = b.member(10, "java/io/PrintStream", "println", "(Ljava/lang/String;)V");
    let add = b.member(10, "demo/Calc", "add", "(II)I");
    let hello = b.string("hello jvm");
    b.string("only in the pool");
    b.method(0x09, "add", "(II)I", &[0x1a, 0x1b, 0x60, 0xac], &[(0, 10), (3, 11)]);
    let greet = [0xb2, (out >> 8) as u8, out as u8, 0x12, hello as u8, 0xb6, (println >> 8) as u8, println as u8, 0xb1];
    b.method(0x01, "greet", "()V", &greet, &[]);
    // tableswitch at offset 1 (padded to 4): lo 0, hi 5, default and six targets, all to the return at 40
    let mut pick = vec![0x1a, 0xaa, 0, 0];
    for v in [36i32, 0, 5] {
        pick.extend_from_slice(&v.to_be_bytes());
    }
    for _ in 0..6 {
        pick.extend_from_slice(&35i32.to_be_bytes());
    }
    pick.extend_from_slice(&[0x03, 0xac]);
    b.method(0x09, "pick", "(I)I", &pick, &[]);
    let main = [0x05, 0x06, 0xb8, (add >> 8) as u8, add as u8, 0x57, 0xb1];
    b.method(0x09, "main", "([Ljava/lang/String;)V", &main, &[]);
    b.method(0x09, "broken", "()V", &[0x03, 0xfe, 0xb1], &[]);
    b.build()
}

/// A zip with each `(name, bytes, deflate)` entry.
pub fn zip(entries: &[(&str, Vec<u8>, bool)]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut central = Vec::new();
    for (name, data, deflate) in entries {
        let body = if *deflate { miniz_oxide::deflate::compress_to_vec(data, 6) } else { data.clone() };
        let off = out.len() as u32;
        let method: u16 = if *deflate { 8 } else { 0 };
        out.extend_from_slice(b"PK\x03\x04");
        for v in [20u16, 0, method, 0, 0] {
            out.extend_from_slice(&v.to_le_bytes());
        }
        for v in [0u32, body.len() as u32, data.len() as u32] {
            out.extend_from_slice(&v.to_le_bytes());
        }
        out.extend_from_slice(&(name.len() as u16).to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes());
        out.extend_from_slice(name.as_bytes());
        out.extend_from_slice(&body);
        central.extend_from_slice(b"PK\x01\x02");
        for v in [20u16, 20, 0, method, 0, 0] {
            central.extend_from_slice(&v.to_le_bytes());
        }
        for v in [0u32, body.len() as u32, data.len() as u32] {
            central.extend_from_slice(&v.to_le_bytes());
        }
        central.extend_from_slice(&(name.len() as u16).to_le_bytes());
        for _ in 0..4 {
            central.extend_from_slice(&0u16.to_le_bytes());
        }
        central.extend_from_slice(&0u32.to_le_bytes());
        central.extend_from_slice(&off.to_le_bytes());
        central.extend_from_slice(name.as_bytes());
    }
    let cd_off = out.len() as u32;
    out.extend_from_slice(&central);
    out.extend_from_slice(b"PK\x05\x06");
    out.extend_from_slice(&[0, 0, 0, 0]);
    out.extend_from_slice(&(entries.len() as u16).to_le_bytes());
    out.extend_from_slice(&(entries.len() as u16).to_le_bytes());
    out.extend_from_slice(&(central.len() as u32).to_le_bytes());
    out.extend_from_slice(&cd_off.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    out
}
