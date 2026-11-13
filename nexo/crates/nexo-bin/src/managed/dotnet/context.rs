//! Token and signature resolution against the metadata tables (the Ctx impl).

use super::*;

impl Ctx<'_> {
    /// A new signature starts: it gets the whole work budget again.
    pub(super) fn fresh(&self) {
        self.work.set(WORK);
    }

    /// `type_of` as a signature of its own (an extends or implements column).
    pub(super) fn type_name(&self, coded: u32) -> String {
        self.fresh();
        self.type_of(coded, 0)
    }

    fn typeref_name(&self, row: usize, depth: u32) -> String {
        let (scope, name, ns) =
            (self.m.cell(TYPE_REF, row, 0), self.m.cell(TYPE_REF, row, 1), self.m.cell(TYPE_REF, row, 2));
        let (name, ns) = (self.m.string(name), self.m.string(ns));
        if let Some((1, outer)) = self.m.decode(scope, &[0, 26, 35, 1], 2) {
            if depth < 8 && outer != row {
                return format!("{}/{name}", self.typeref_name(outer, depth + 1));
            }
        }
        if ns.is_empty() {
            name
        } else {
            format!("{ns}.{name}")
        }
    }

    fn type_of(&self, coded: u32, depth: u32) -> String {
        match self.m.decode(coded, &[2, 1, 27], 2) {
            Some((2, r)) => self.names.types.get(r).cloned().unwrap_or_else(|| format!("TypeDef#{r}?")),
            Some((1, r)) => self.typeref_name(r, 0),
            Some((27, r)) => {
                let blob = self.m.blob(self.m.cell(27, r, 0));
                let mut s = Sig { b: blob, p: 0 };
                self.read_type(&mut s, depth + 1)
            }
            _ => format!("?{coded:#x}"),
        }
    }

    pub(super) fn read_type(&self, s: &mut Sig<'_>, depth: u32) -> String {
        if depth > 12 {
            return "<not read: nested too deep>".into();
        }
        // TypeSpecs may point at each other and fan out 32 or 255 ways: a node count caps the whole signature
        match self.work.get() {
            0 => return "<not read: signature too large>".into(),
            w => self.work.set(w - 1),
        }
        let Some(e) = s.byte() else { return "?".into() };
        let prim = |n: &str| n.to_string();
        match e {
            0x01 => prim("void"),
            0x02 => prim("bool"),
            0x03 => prim("char"),
            0x04 => prim("sbyte"),
            0x05 => prim("byte"),
            0x06 => prim("short"),
            0x07 => prim("ushort"),
            0x08 => prim("int"),
            0x09 => prim("uint"),
            0x0a => prim("long"),
            0x0b => prim("ulong"),
            0x0c => prim("float"),
            0x0d => prim("double"),
            0x0e => prim("string"),
            0x0f => format!("{}*", self.read_type(s, depth + 1)),
            0x10 => format!("{}&", self.read_type(s, depth + 1)),
            0x11 | 0x12 => {
                let v = s.uint().unwrap_or(0);
                self.type_of(v, depth)
            }
            0x13 => format!("!{}", s.uint().unwrap_or(0)),
            0x1e => format!("!!{}", s.uint().unwrap_or(0)),
            0x14 => {
                let t = self.read_type(s, depth + 1);
                // a real array has rank 32 at most; the counts after it cannot exceed the rank either
                let bad = |what: &str| format!("{t}[<not read: {what}>]");
                let Some(rank) = s.uint().filter(|r| (1..=32).contains(r)) else { return bad("array rank") };
                for _ in 0..2 {
                    match s.uint() {
                        Some(n) if n <= rank => (0..n).for_each(|_| _ = s.uint()),
                        _ => return bad("array bounds"),
                    }
                }
                format!("{t}[{}]", ",".repeat(rank as usize - 1))
            }
            0x15 => {
                s.byte();
                let v = s.uint().unwrap_or(0);
                let base = self.type_of(v, depth);
                let n = s.uint().unwrap_or(0);
                let args: Vec<String> = (0..n.min(32)).map(|_| self.read_type(s, depth + 1)).collect();
                format!("{base}<{}>", args.join(","))
            }
            0x16 => prim("typedref"),
            0x18 => prim("nint"),
            0x19 => prim("nuint"),
            0x1b => {
                let (params, ret, _) = self.method_sig(s, depth + 1);
                format!("fnptr({}) -> {ret}", params.join(", "))
            }
            0x1c => prim("object"),
            0x1d => format!("{}[]", self.read_type(s, depth + 1)),
            0x1f | 0x20 => {
                let v = s.uint().unwrap_or(0);
                let m = self.type_of(v, depth);
                let rest = self.read_type(s, depth + 1);
                format!("{rest} modopt({m})")
            }
            0x45 => format!("pinned {}", self.read_type(s, depth + 1)),
            0x41 => self.read_type(s, depth + 1),
            other => format!("?elem{other:#x}"),
        }
    }

    /// `(parameter types, return type, has this)`.
    fn method_sig(&self, s: &mut Sig<'_>, depth: u32) -> (Vec<String>, String, bool) {
        let conv = s.byte().unwrap_or(0);
        if conv & 0x10 != 0 {
            s.uint();
        }
        let n = s.uint().unwrap_or(0);
        let ret = self.read_type(s, depth);
        let params = (0..n.min(255)).map(|_| self.read_type(s, depth)).collect();
        (params, ret, conv & 0x20 != 0)
    }

    pub(super) fn field_type(&self, blob: &[u8]) -> String {
        self.fresh();
        let mut s = Sig { b: blob, p: 0 };
        s.byte();
        self.read_type(&mut s, 0)
    }

    pub(super) fn method_parts(&self, blob: &[u8]) -> (String, String) {
        self.fresh();
        let mut s = Sig { b: blob, p: 0 };
        let (p, r, _) = self.method_sig(&mut s, 0);
        (format!("({})", p.join(",")), format!("({}) -> {r}", p.join(", ")))
    }

    fn field_name(&self, row: usize) -> String {
        let owner = self.names.field_owner.get(row).copied().unwrap_or(0);
        let o = self.names.types.get(owner).cloned().unwrap_or_default();
        format!("{o}.{}", self.m.string(self.m.cell(FIELD, row, 1)))
    }

    /// `(display text, callee key if it is a method)`.
    pub(super) fn token(&self, tok: u32) -> (String, Option<String>) {
        self.fresh();
        let (table, row) = ((tok >> 24) as usize, (tok & 0xFF_FFFF) as usize);
        match table {
            TYPE_DEF | TYPE_REF | 0x1b => (
                self.type_of(
                    ((row as u32) << 2)
                        | [0, 1, 0, 0, 0, 0, 0, 0][0].max(match table {
                            TYPE_DEF => 0,
                            TYPE_REF => 1,
                            _ => 2,
                        }),
                    0,
                ),
                None,
            ),
            FIELD => {
                let t = self.field_type(self.m.blob(self.m.cell(FIELD, row, 2)));
                (format!("{}: {t}", self.field_name(row)), None)
            }
            METHOD => {
                let owner = self.names.method_owner.get(row).copied().unwrap_or(0);
                let o = self.names.types.get(owner).cloned().unwrap_or_default();
                let name = self.m.string(self.m.cell(METHOD, row, 3));
                let (raw, pretty) = self.method_parts(self.m.blob(self.m.cell(METHOD, row, 4)));
                (format!("{o}.{name}{pretty}"), Some(format!("{o}.{name}{raw}")))
            }
            MEMBER_REF => {
                let (parent, name, sigb) =
                    (self.m.cell(10, row, 0), self.m.cell(10, row, 1), self.m.blob(self.m.cell(10, row, 2)));
                let p = match self.m.decode(parent, &[2, 1, 26, 6, 27], 3) {
                    Some((2, r)) => self.names.types.get(r).cloned().unwrap_or_default(),
                    Some((1, r)) => self.typeref_name(r, 0),
                    Some((26, r)) => format!("[{}]", self.m.string(self.m.cell(26, r, 0))),
                    Some((6, r)) => {
                        let o = self.names.method_owner.get(r).copied().unwrap_or(0);
                        self.names.types.get(o).cloned().unwrap_or_default()
                    }
                    Some((27, r)) => {
                        let mut s = Sig { b: self.m.blob(self.m.cell(27, r, 0)), p: 0 };
                        self.read_type(&mut s, 1)
                    }
                    _ => "?".into(),
                };
                let name = self.m.string(name);
                if sigb.first() == Some(&0x06) {
                    (format!("{p}.{name}: {}", self.field_type(sigb)), None)
                } else {
                    let (raw, pretty) = self.method_parts(sigb);
                    (format!("{p}.{name}{pretty}"), Some(format!("{p}.{name}{raw}")))
                }
            }
            0x2b => {
                let (m, blob) = (self.m.cell(43, row, 0), self.m.blob(self.m.cell(43, row, 1)));
                let (base, key) = match self.m.decode(m, &[6, 10], 1) {
                    Some((t, r)) => self.token(((t as u32) << 24) | r as u32),
                    None => ("?".into(), None),
                };
                let mut s = Sig { b: blob, p: 0 };
                s.byte();
                let n = s.uint().unwrap_or(0);
                let args: Vec<String> = (0..n.min(16)).map(|_| self.read_type(&mut s, 1)).collect();
                (format!("{base} <{}>", args.join(",")), key)
            }
            0x11 => {
                let mut s = Sig { b: self.m.blob(self.m.cell(17, row, 0)), p: 0 };
                let (p, r, _) = self.method_sig(&mut s, 0);
                (format!("signature({}) -> {r}", p.join(", ")), None)
            }
            _ => (format!("token {tok:#010x}?"), None),
        }
    }
}
