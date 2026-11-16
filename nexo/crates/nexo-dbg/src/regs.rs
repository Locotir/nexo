//! The x86-64 registers of one thread. A thread running 32-bit code has the same set and uses the low 32 bits of
//! eight of them: it shows and sets them by their 32-bit names (`eax` .. `edi`, `eip`).

/// The general registers in the order they are shown.
pub const GP: [&str; 16] =
    ["rax", "rbx", "rcx", "rdx", "rsi", "rdi", "rbp", "rsp", "r8", "r9", "r10", "r11", "r12", "r13", "r14", "r15"];
const SEG: [&str; 6] = ["cs", "ss", "ds", "es", "fs", "gs"];
const FLAGS: [(u32, &str); 9] =
    [(0, "CF"), (2, "PF"), (4, "AF"), (6, "ZF"), (7, "SF"), (8, "TF"), (9, "IF"), (10, "DF"), (11, "OF")];

pub const RBP: usize = 6;
pub const RSP: usize = 7;

/// The registers of a 32-bit thread with their [`GP`] index, in the order they are shown.
const GP32: [(&str, usize); 8] =
    [("eax", 0), ("ebx", 1), ("ecx", 2), ("edx", 3), ("esi", 4), ("edi", 5), ("ebp", 6), ("esp", 7)];

/// The code segment selector Linux gives a thread running 32-bit code (64-bit code runs with 0x33).
pub const LINUX_CS32: u64 = 0x23;

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
    /// The registers with no value: set only for a caller's frame (bits by [`GP`] index, bit 16 for eflags), where
    /// the call may have changed them and no unwind rule gave them back. [`Regs::get`] has no value for them.
    pub lost: u32,
    /// The thread runs 32-bit code: its registers are shown by their 32-bit names, eight of them.
    pub x32: bool,
}

/// Where each [`GP`] register sits in Linux's `user_regs_struct` (and in a core file's `pr_reg`).
const USER_GP: [usize; 16] = [10, 5, 11, 12, 13, 14, 4, 19, 9, 8, 7, 6, 3, 2, 1, 0];

/// Where each [`GP`] register sits in macOS's `x86_thread_state64_t` (rdi comes before rsi there).
const MACH_GP: [usize; 16] = [0, 1, 2, 3, 5, 4, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15];

impl Regs {
    /// From macOS's `x86_thread_state64_t`: 21 words, the last five rip, rflags, cs, fs, gs. It holds no segment
    /// bases.
    pub fn from_mach(m: &[u64; 21]) -> Regs {
        let mut r = Regs::default();
        for (i, &at) in MACH_GP.iter().enumerate() {
            r.gp[i] = m[at];
        }
        r.rip = m[16];
        r.eflags = m[17];
        r.seg = [m[18], 0, 0, 0, m[19], m[20]];
        r
    }

    pub fn to_mach(&self) -> [u64; 21] {
        let mut m = [0u64; 21];
        for (i, &at) in MACH_GP.iter().enumerate() {
            m[at] = self.gp[i];
        }
        m[16] = self.rip;
        m[17] = self.eflags;
        (m[18], m[19], m[20]) = (self.seg[0], self.seg[4], self.seg[5]);
        m
    }

    /// From Linux's `user_regs_struct`: 27 words. A thread running 32-bit code gives the same struct with its
    /// values zero-extended; its code segment says which one it is.
    pub fn from_user(u: &[u64; 27]) -> Regs {
        let mut r = Regs { has_bases: true, x32: u[17] == LINUX_CS32, ..Regs::default() };
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
        if self.lost != 0 && self.is_lost(name) {
            return None;
        }
        let n = name.trim_start_matches('$').to_ascii_lowercase();
        if let Some(i) = GP.iter().position(|g| *g == n).filter(|&i| !self.x32 || i < 8) {
            return Some(self.gp[i]);
        }
        match n.as_str() {
            "rip" | "pc" => return Some(self.rip),
            "eip" if self.x32 => return Some(self.rip),
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
        let (idx, bits) = low_part(&n).filter(|&(i, _)| !self.x32 || i < 8)?;
        Some(self.gp[idx] & (u64::MAX >> (64 - bits)))
    }

    /// Whether `name` (any spelling [`Regs::get`] takes) is a register this frame has no value for.
    pub fn is_lost(&self, name: &str) -> bool {
        let n = name.trim_start_matches('$').to_ascii_lowercase();
        let bit = match n.as_str() {
            "eflags" | "rflags" | "flags" => 16,
            "sp" => RSP,
            "fp" => RBP,
            _ => match GP.iter().position(|g| *g == n) {
                Some(i) => i,
                None => match low_part(&n) {
                    Some((i, _)) => i,
                    None => return false,
                },
            },
        };
        self.lost >> bit & 1 == 1
    }

    /// The names of the registers this frame has no value for, in the order they are shown.
    pub fn lost_names(&self) -> Vec<&'static str> {
        let mut v: Vec<&'static str> = if self.x32 {
            GP32.iter().filter(|(_, i)| self.lost >> i & 1 == 1).map(|(n, _)| *n).collect()
        } else {
            GP.iter().enumerate().filter(|(i, _)| self.lost >> i & 1 == 1).map(|(_, n)| *n).collect()
        };
        if self.lost >> 16 & 1 == 1 {
            v.push("eflags");
        }
        v
    }

    /// Set a whole register: a 64-bit general one, `rip` or `eflags`; in a 32-bit thread also `eax` .. `edi` and
    /// `eip`, which take the low 32 bits of `v`. False for any other name.
    pub fn set(&mut self, name: &str, v: u64) -> bool {
        let n = name.trim_start_matches('$').to_ascii_lowercase();
        if self.x32 {
            if let Some(&(_, i)) = GP32.iter().find(|(g, _)| *g == n) {
                self.gp[i] = v & 0xffff_ffff;
                return true;
            }
            if n == "eip" {
                self.rip = v & 0xffff_ffff;
                return true;
            }
        }
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

    /// Every register with its name, in the order they are shown; a 32-bit thread's by their 32-bit names.
    pub fn named(&self) -> Vec<(&'static str, u64)> {
        let mut v: Vec<(&'static str, u64)> = if self.x32 {
            let mut v: Vec<(&'static str, u64)> = GP32.iter().map(|&(n, i)| (n, self.gp[i] & 0xffff_ffff)).collect();
            v.push(("eip", self.rip & 0xffff_ffff));
            v
        } else {
            let mut v: Vec<(&'static str, u64)> = GP.iter().copied().zip(self.gp).collect();
            v.push(("rip", self.rip));
            v
        };
        v.push(("eflags", self.eflags));
        v.extend(SEG.iter().copied().zip(self.seg));
        if self.has_bases {
            v.push(("fs_base", self.fs_base));
            v.push(("gs_base", self.gs_base));
        }
        v
    }

    /// The registers whose value differs from `before`: name, old, new. `rip` (`eip`) is left out (a stop says where
    /// it is).
    pub fn changed(&self, before: &Regs) -> Vec<(&'static str, u64, u64)> {
        self.named()
            .into_iter()
            .zip(before.named())
            .filter(|((n, new), (_, old))| new != old && *n != "rip" && *n != "eip")
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
    fn a_32_bit_thread_shows_and_sets_its_32_bit_registers() {
        let mut u = [0u64; 27];
        u[17] = LINUX_CS32;
        u[10] = 7; // rax
        u[19] = 0xffd0_1000; // rsp
        u[16] = 0x5655_6000; // rip
        let mut r = Regs::from_user(&u);
        assert!(r.x32 && !Regs::from_user(&[0; 27]).x32);
        let names: Vec<&str> = r.named().iter().map(|(n, _)| *n).collect();
        assert_eq!(&names[..10], ["eax", "ebx", "ecx", "edx", "esi", "edi", "ebp", "esp", "eip", "eflags"]);
        assert!(!names.contains(&"r8") && !names.contains(&"rip"));
        assert_eq!((r.get("eip"), r.get("esp"), r.get("eax")), (Some(0x5655_6000), Some(0xffd0_1000), Some(7)));
        assert_eq!((r.get("r8"), r.get("r8d")), (None, None));
        assert!(r.set("ecx", 0x1_2345_6789) && r.get("ecx") == Some(0x2345_6789));
        assert!(r.set("eip", 0x5655_6010) && r.rip == 0x5655_6010);
        let before = r;
        r.gp[0] = 9;
        r.rip += 1;
        assert_eq!(r.changed(&before), vec![("eax", 7, 9)]);
        r.lost = 1 << 2;
        assert_eq!(r.lost_names(), ["ecx"]);
    }

    #[test]
    fn the_macos_layout_round_trips_with_rdi_before_rsi() {
        let mut m = [0u64; 21];
        for (i, w) in m.iter_mut().enumerate() {
            *w = 0x2000 + i as u64;
        }
        let r = Regs::from_mach(&m);
        assert_eq!(r.to_mach(), m);
        assert_eq!((r.get("rdi"), r.get("rsi")), (Some(0x2004), Some(0x2005)));
        assert_eq!((r.get("rsp"), r.get("rip"), r.get("r15")), (Some(0x2007), Some(0x2010), Some(0x200f)));
        assert!(!r.has_bases);
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
