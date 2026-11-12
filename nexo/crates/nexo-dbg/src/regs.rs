//! The x86-64 registers of one thread.

/// The general registers in the order they are shown.
pub const GP: [&str; 16] =
    ["rax", "rbx", "rcx", "rdx", "rsi", "rdi", "rbp", "rsp", "r8", "r9", "r10", "r11", "r12", "r13", "r14", "r15"];
const SEG: [&str; 6] = ["cs", "ss", "ds", "es", "fs", "gs"];
const FLAGS: [(u32, &str); 9] =
    [(0, "CF"), (2, "PF"), (4, "AF"), (6, "ZF"), (7, "SF"), (8, "TF"), (9, "IF"), (10, "DF"), (11, "OF")];

pub const RBP: usize = 6;
pub const RSP: usize = 7;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Regs {
    /// In the order of [`GP`].
    pub gp: [u64; 16],
    pub rip: u64,
    pub eflags: u64,
    /// cs, ss, ds, es, fs, gs.
    pub seg: [u64; 6],
    pub fs_base: u64,
    pub gs_base: u64,
    /// Linux keeps the system call number here; it goes back unchanged.
    pub orig_rax: u64,
    /// False where the system does not give the segment bases (Windows).
    pub has_bases: bool,
}

/// Where each [`GP`] register sits in Linux's `user_regs_struct` (and in a core file's `pr_reg`).
const USER_GP: [usize; 16] = [10, 5, 11, 12, 13, 14, 4, 19, 9, 8, 7, 6, 3, 2, 1, 0];

impl Regs {
    /// From Linux's `user_regs_struct`: 27 words.
    pub fn from_user(u: &[u64; 27]) -> Regs {
        let mut r = Regs { has_bases: true, ..Regs::default() };
        for (i, &at) in USER_GP.iter().enumerate() {
            r.gp[i] = u[at];
        }
        r.orig_rax = u[15];
        r.rip = u[16];
        r.eflags = u[18];
        r.seg = [u[17], u[20], u[23], u[24], u[25], u[26]];
        r.fs_base = u[21];
        r.gs_base = u[22];
        r
    }

    pub fn to_user(&self) -> [u64; 27] {
        let mut u = [0u64; 27];
        for (i, &at) in USER_GP.iter().enumerate() {
            u[at] = self.gp[i];
        }
        u[15] = self.orig_rax;
        u[16] = self.rip;
        u[18] = self.eflags;
        u[17] = self.seg[0];
        u[20] = self.seg[1];
        u[23] = self.seg[2];
        u[24] = self.seg[3];
        u[25] = self.seg[4];
        u[26] = self.seg[5];
        u[21] = self.fs_base;
        u[22] = self.gs_base;
        u
    }

    pub fn rsp(&self) -> u64 {
        self.gp[RSP]
    }

    pub fn rbp(&self) -> u64 {
        self.gp[RBP]
    }

    /// The value of a register by name: the 64-bit names, their 32, 16 and 8-bit low parts,
    /// `rip`/`pc`, `sp`, `fp`, `eflags`, the segment registers and their bases.
    pub fn get(&self, name: &str) -> Option<u64> {
        let n = name.trim_start_matches('$').to_ascii_lowercase();
        if let Some(i) = GP.iter().position(|g| *g == n) {
            return Some(self.gp[i]);
        }
        match n.as_str() {
            "rip" | "pc" => return Some(self.rip),
            "sp" => return Some(self.gp[RSP]),
            "fp" => return Some(self.gp[RBP]),
            "eflags" | "rflags" | "flags" => return Some(self.eflags),
            "fs_base" if self.has_bases => return Some(self.fs_base),
            "gs_base" if self.has_bases => return Some(self.gs_base),
            _ => {}
        }
        if let Some(i) = SEG.iter().position(|s| *s == n) {
            return Some(self.seg[i]);
        }
        let (idx, bits) = low_part(&n)?;
        Some(self.gp[idx] & (u64::MAX >> (64 - bits)))
    }

    /// Set a whole register: a 64-bit general one, `rip` or `eflags`. False for any other name.
    pub fn set(&mut self, name: &str, v: u64) -> bool {
        let n = name.trim_start_matches('$').to_ascii_lowercase();
        if let Some(i) = GP.iter().position(|g| *g == n) {
            self.gp[i] = v;
            return true;
        }
        match n.as_str() {
            "rip" | "pc" => self.rip = v,
            "eflags" | "rflags" => self.eflags = v,
            _ => return false,
        }
        true
    }

    /// Every register with its name, in the order they are shown.
    pub fn named(&self) -> Vec<(&'static str, u64)> {
        let mut v: Vec<(&'static str, u64)> = GP.iter().copied().zip(self.gp).collect();
        v.push(("rip", self.rip));
        v.push(("eflags", self.eflags));
        v.extend(SEG.iter().copied().zip(self.seg));
        if self.has_bases {
            v.push(("fs_base", self.fs_base));
            v.push(("gs_base", self.gs_base));
        }
        v
    }

    /// The registers whose value differs from `before`: name, old, new. `rip` is left out (a stop says where it is).
    pub fn changed(&self, before: &Regs) -> Vec<(&'static str, u64, u64)> {
        self.named()
            .into_iter()
            .zip(before.named())
            .filter(|((n, new), (_, old))| new != old && *n != "rip")
            .map(|((n, new), (_, old))| (n, old, new))
            .collect()
    }

    /// The flags that are set, as letters: `ZF PF`.
    pub fn flags_text(&self) -> String {
        FLAGS.iter().filter(|(bit, _)| self.eflags >> bit & 1 == 1).map(|(_, n)| *n).collect::<Vec<_>>().join(" ")
    }
}

/// `eax`, `ax`, `al`, `r8d`, `r8w`, `r8b`: the general register and how many low bits.
fn low_part(n: &str) -> Option<(usize, u32)> {
    if let Some(rest) = n.strip_prefix('r') {
        for (suffix, bits) in [("d", 32), ("w", 16), ("b", 8)] {
            if let Some(num) = rest.strip_suffix(suffix) {
                if let Ok(k) = num.parse::<usize>() {
                    return (8..16).contains(&k).then_some((k, bits));
                }
            }
        }
    }
    let old = ["ax", "bx", "cx", "dx", "si", "di", "bp", "sp"];
    if let Some(i) = n.strip_prefix('e').and_then(|x| old.iter().position(|o| *o == x)) {
        return Some((i, 32));
    }
    if let Some(i) = old.iter().position(|o| *o == n) {
        return Some((i, 16));
    }
    ["al", "bl", "cl", "dl", "sil", "dil", "bpl", "spl"].iter().position(|o| *o == n).map(|i| (i, 8))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn user_layout_round_trips_and_names_resolve() {
        let mut u = [0u64; 27];
        for (i, w) in u.iter_mut().enumerate() {
            *w = 0x1000 + i as u64;
        }
        let r = Regs::from_user(&u);
        assert_eq!(r.to_user(), u);
        assert_eq!(r.get("rax"), Some(0x1000 + 10));
        assert_eq!(r.get("rsp"), Some(0x1000 + 19));
        assert_eq!(r.get("pc"), Some(0x1000 + 16));
        assert_eq!(r.get("r15"), Some(0x1000));
        let mut r = Regs::default();
        r.gp[0] = 0x1122_3344_5566_7788;
        r.gp[8] = 0xffff_ffff_0000_00ab;
        assert_eq!(r.get("eax"), Some(0x5566_7788));
        assert_eq!(r.get("ax"), Some(0x7788));
        assert_eq!(r.get("al"), Some(0x88));
        assert_eq!(r.get("r8d"), Some(0xab));
        assert_eq!(r.get("r8b"), Some(0xab));
        assert_eq!(r.get("xmm0"), None);
        assert!(r.set("rdi", 7) && r.get("edi") == Some(7));
        assert!(!r.set("eax", 1));
    }

    #[test]
    fn only_what_changed_is_listed() {
        let a = Regs::default();
        let mut b = a;
        b.gp[3] = 5;
        b.rip = 0x40;
        b.eflags = 0x44;
        assert_eq!(b.changed(&a), vec![("rdx", 0, 5), ("eflags", 0, 0x44)]);
        assert_eq!(b.flags_text(), "PF ZF");
    }
}
