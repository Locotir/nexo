//! Sizes and names of types.

use super::*;

impl Vars {
    pub(super) fn strip(&self, mut t: Option<u64>) -> Option<&Ty> {
        for _ in 0..16 {
            match self.types.get(&t?)? {
                Ty::Alias { to, .. } => t = *to,
                other => return Some(other),
            }
        }
        None
    }

    pub(super) fn size(&self, v: View) -> u64 {
        self.size_in(v, 0)
    }

    /// `size` with a depth guard: damaged debug info can make a type contain itself.
    fn size_in(&self, v: View, depth: usize) -> u64 {
        if depth > MAX_TYPE_DEPTH {
            return 0;
        }
        match self.strip(v.0) {
            Some(Ty::Base { size, .. } | Ty::Record { size, .. } | Ty::Enum { size, .. }) => *size,
            Some(Ty::Ptr { .. }) => 8,
            Some(Ty::Array { elem, dims }) => {
                let n: u64 = dims.iter().skip(v.1).fold(1u64, |a, d| a.saturating_mul(d.unwrap_or(0)));
                n.saturating_mul(self.size_in(View(*elem, 0), depth + 1))
            }
            _ => 0,
        }
    }

    pub(super) fn name(&self, v: View) -> String {
        self.name_in(v, 0)
    }

    /// `name` with a depth guard: a pointer chain that loops back on itself ends in `...`.
    fn name_in(&self, v: View, depth: usize) -> String {
        if depth > MAX_TYPE_DEPTH {
            return "...".into();
        }
        let Some(t) = v.0 else { return "void".into() };
        match self.types.get(&t) {
            Some(Ty::Base { name, .. }) => name.clone(),
            Some(Ty::Ptr { to }) => match self.strip(*to) {
                Some(Ty::Func) => "function pointer".into(),
                _ => format!("{} *", self.name_in(View(*to, 0), depth + 1)),
            },
            Some(Ty::Array { elem, dims }) => {
                let d: String = dims.iter().skip(v.1).map(|d| d.map_or("[]".into(), |n| format!("[{n}]"))).collect();
                format!("{} {d}", self.name_in(View(*elem, 0), depth + 1))
            }
            Some(Ty::Record { name, union, .. }) => {
                let n = if name.is_empty() { "<anonymous>" } else { name };
                if n.contains("::") {
                    n.to_string()
                } else {
                    format!("{} {n}", if *union { "union" } else { "struct" })
                }
            }
            Some(Ty::Alias { name, to }) if name.is_empty() => {
                format!("const {}", self.name_in(View(*to, 0), depth + 1))
            }
            Some(Ty::Alias { name, .. }) => name.clone(),
            Some(Ty::Enum { name, .. }) => format!("enum {name}"),
            Some(Ty::Func) => "function".into(),
            None => "?".into(),
        }
    }

    pub(super) fn char_like(&self, t: Option<u64>) -> bool {
        matches!(self.strip(t), Some(Ty::Base { size: 1, enc, name }) if [6, 8].contains(enc) || name.contains("char"))
    }
}
