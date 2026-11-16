//! Where a 64-bit call's arguments are at the function's entry (System V and Win64), and the arguments a
//! printf- or scanf-style format string says follow it.

use crate::platform::Os;

/// How an argument travels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Class {
    Int,
    /// `float` or `double`: an xmm register while one is free.
    Float,
    /// System V `long double`: always in memory, 16 bytes aligned to 16.
    X87,
    /// A struct, a union, an `__int128` or a complex number: `size` bytes aligned to `align`, split as System V
    /// classifies it. Win64 passes one of 1, 2, 4 or 8 bytes as an integer and any other by its address.
    Agg {
        size: u64,
        align: u64,
        sysv: SysV,
    },
}

/// How System V passes an aggregate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SysV {
    /// In registers while enough are free: each of its one or two eightbytes in an xmm register when `true`, else
    /// in an integer register.
    Regs(bool, Option<bool>),
    /// In memory: more than 16 bytes, or a member that is not aligned.
    Memory,
    /// It holds a `long double`: in memory as an argument, in st0 as a result.
    X87,
}

/// One eightbyte of an aggregate System V passes in registers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Half {
    Gp(usize),
    Xmm(usize),
}

/// Where one argument is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Loc {
    /// The `n`th integer argument register.
    Gp(usize),
    Xmm(usize),
    /// Bytes past the first stack argument.
    Stack(u64),
    /// An aggregate in registers (System V), eightbyte by eightbyte.
    Split(Half, Option<Half>),
    /// Win64: the address of the caller's copy, in the `n`th integer argument register.
    RefGp(usize),
    /// Win64: the address of the caller's copy, in the stack slot this many bytes past the first.
    RefStack(u64),
}
/// The argument places of one call, handed out in order.
pub struct Abi {
    win: bool,
    gp: usize,
    xmm: usize,
    stack: u64,
}

/// The integer argument registers of a 64-bit call.
pub fn int_regs(os: Os) -> &'static [&'static str] {
    match os {
        Os::Linux | Os::MacOS => &["rdi", "rsi", "rdx", "rcx", "r8", "r9"],
        Os::Windows => &["rcx", "rdx", "r8", "r9"],
    }
}

/// The first stack argument at a function's entry, past the return address; on Win64 also past the 32 bytes the
/// caller keeps for the four register arguments. `at_call`: the thread is on the call instruction, before the
/// return address is pushed.
pub fn first_stack_arg(os: Os, rsp: u64, at_call: bool) -> u64 {
    rsp + if at_call { 0 } else { 8 } + if os == Os::Windows { 0x20 } else { 0 }
}

impl Abi {
    pub fn new(os: Os) -> Abi {
        Abi { win: os == Os::Windows, gp: 0, xmm: 0, stack: 0 }
    }

    fn on_stack(&mut self, size: u64, align: u64) -> Loc {
        self.stack = self.stack.next_multiple_of(align);
        let at = self.stack;
        self.stack += size;
        Loc::Stack(at)
    }

    /// The next argument. `vararg`: it is one of the `...` arguments, which Win64 passes in the integer register
    /// of its position even when it is a double (the caller copies it to the xmm one too).
    pub fn next(&mut self, class: Class, vararg: bool) -> Loc {
        if self.win {
            // Win64 counts positions, not registers of each kind; a long double there is a double
            let pos = self.gp;
            self.gp += 1;
            let int = match class {
                Class::Agg { size, .. } if !matches!(size, 1 | 2 | 4 | 8) => {
                    return match pos {
                        0..=3 => Loc::RefGp(pos),
                        _ => match self.on_stack(8, 8) {
                            Loc::Stack(at) => Loc::RefStack(at),
                            other => other,
                        },
                    }
                }
                Class::Int | Class::Agg { .. } => true,
                _ => vararg,
            };
            return match pos {
                0..=3 if int => Loc::Gp(pos),
                0..=3 => Loc::Xmm(pos),
                _ => self.on_stack(8, 8),
            };
        }
        if let Class::Agg { size, align, sysv } = class {
            if let SysV::Regs(first, second) = sysv {
                let halves = [Some(first), second];
                let sse = halves.iter().flatten().filter(|&&x| x).count();
                let int = halves.iter().flatten().count() - sse;
                // the whole value goes in registers or none of it does; registers it does not take stay for later ones
                if self.gp + int <= 6 && self.xmm + sse <= 8 {
                    let mut half = |x: bool| {
                        if x {
                            self.xmm += 1;
                            Half::Xmm(self.xmm - 1)
                        } else {
                            self.gp += 1;
                            Half::Gp(self.gp - 1)
                        }
                    };
                    let a = half(first);
                    return Loc::Split(a, second.map(half));
                }
            }
            return self.on_stack(size.next_multiple_of(8), align.clamp(8, 16));
        }
        match class {
            Class::Int if self.gp < 6 => {
                self.gp += 1;
                Loc::Gp(self.gp - 1)
            }
            Class::Float if self.xmm < 8 => {
                self.xmm += 1;
                Loc::Xmm(self.xmm - 1)
            }
            Class::X87 => self.on_stack(16, 16),
            _ => self.on_stack(8, 8),
        }
    }
}

/// What a conversion of a format string takes from the arguments.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Conv {
    /// An integer of this many bytes (a smaller one travels as an int and is cut back), signed or not, shown in hex.
    Int {
        bytes: u32,
        signed: bool,
        hex: bool,
    },
    Char,
    Text,
    Ptr,
    Double,
    LongDouble,
}

/// The sizes the length letters stand for on one system.
#[derive(Debug, Clone, Copy)]
pub struct Sizes {
    /// `long`: 8 on 64-bit Linux and macOS, 4 on Windows and in a 32-bit program.
    pub long: u32,
    pub ptr: u32,
}

/// The arguments a format string takes, in order: printf conversions (`scan`: scanf, where each one is a
/// pointer). The flag is false when a conversion this reader does not know ended the list: the arguments after it
/// cannot be placed.
pub fn conversions(fmt: &[u8], scan: bool, sz: Sizes) -> (Vec<Conv>, bool) {
    let mut out = Vec::new();
    let mut i = 0;
    while i < fmt.len() {
        if fmt[i] != b'%' {
            i += 1;
            continue;
        }
        i += 1;
        if fmt.get(i) == Some(&b'%') {
            i += 1;
            continue;
        }
        // flags, width and precision; a `*` takes an int argument in printf and drops the value in scanf
        let mut suppressed = false;
        while let Some(&c) = fmt.get(i) {
            match c {
                b'-' | b'+' | b' ' | b'#' | b'0'..=b'9' | b'.' | b'\'' | b'$' => {}
                b'*' if scan => suppressed = true,
                b'*' => out.push(Conv::Int { bytes: 4, signed: true, hex: false }),
                _ => break,
            }
            i += 1;
        }
        let mut bytes = 4;
        let mut long_double = false;
        loop {
            let rest = &fmt[i.min(fmt.len())..];
            let (n, b) = match rest {
                [b'h', b'h', ..] => (2, 1),
                [b'l', b'l', ..] => (2, 8),
                [b'I', b'6', b'4', ..] => (3, 8),
                [b'I', b'3', b'2', ..] => (3, 4),
                [b'h', ..] => (1, 2),
                [b'l', ..] => (1, sz.long),
                [b'q', ..] | [b'j', ..] => (1, 8),
                [b'z', ..] | [b't', ..] | [b'I', ..] => (1, sz.ptr),
                [b'L', ..] => {
                    long_double = true;
                    (1, 8)
                }
                _ => break,
            };
            bytes = b;
            i += n;
        }
        let Some(&c) = fmt.get(i) else { break };
        i += 1;
        // glibc's %m prints errno and takes nothing
        if c == b'm' && !scan {
            continue;
        }
        if scan {
            if c == b'[' {
                // a set: `]` right after `[` or `[^` belongs to it
                if fmt.get(i) == Some(&b'^') {
                    i += 1;
                }
                if fmt.get(i) == Some(&b']') {
                    i += 1;
                }
                while fmt.get(i).is_some_and(|&b| b != b']') {
                    i += 1;
                }
                i += 1;
            } else if !b"diouxXaAeEfFgGsScCpn".contains(&c) {
                return (out, false);
            }
            if !suppressed {
                out.push(Conv::Ptr);
            }
            continue;
        }
        out.push(match c {
            b'd' | b'i' => Conv::Int { bytes, signed: true, hex: false },
            b'u' | b'o' => Conv::Int { bytes, signed: false, hex: false },
            b'x' | b'X' => Conv::Int { bytes, signed: false, hex: true },
            b'c' | b'C' => Conv::Char,
            b's' => Conv::Text,
            b'S' | b'p' | b'n' => Conv::Ptr,
            b'a' | b'A' | b'e' | b'E' | b'f' | b'F' | b'g' | b'G' if long_double => Conv::LongDouble,
            b'a' | b'A' | b'e' | b'E' | b'f' | b'F' | b'g' | b'G' => Conv::Double,
            _ => return (out, false),
        });
    }
    (out, true)
}

/// Whether a variadic function named `name` reads its `...` by a format string: `Some(true)` for scanf.
pub fn format_kind(name: &str) -> Option<bool> {
    let n = name.trim_start_matches('_');
    if n.contains("scanf") {
        Some(true)
    } else if n.contains("printf") || matches!(n, "syslog" | "err" | "errx" | "warn" | "warnx" | "error") {
        Some(false)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn system_v_fills_six_integer_and_eight_xmm_registers_then_the_stack_in_order() {
        let mut a = Abi::new(Os::Linux);
        let got: Vec<Loc> = (0..9).map(|_| a.next(Class::Int, false)).collect();
        assert_eq!(got[5], Loc::Gp(5));
        assert_eq!(&got[6..], [Loc::Stack(0), Loc::Stack(8), Loc::Stack(16)]);
        let mut a = Abi::new(Os::Linux);
        assert_eq!(a.next(Class::Float, false), Loc::Xmm(0));
        assert_eq!(a.next(Class::Int, false), Loc::Gp(0));
        assert_eq!(a.next(Class::Float, false), Loc::Xmm(1));
        assert_eq!(a.next(Class::X87, false), Loc::Stack(0));
        for _ in 2..8 {
            a.next(Class::Float, false);
        }
        assert_eq!(a.next(Class::Float, false), Loc::Stack(16));
    }

    #[test]
    fn win64_counts_positions_and_passes_the_fifth_past_the_shadow_space() {
        let mut a = Abi::new(Os::Windows);
        assert_eq!(a.next(Class::Int, false), Loc::Gp(0));
        assert_eq!(a.next(Class::Float, false), Loc::Xmm(1));
        assert_eq!(a.next(Class::Float, true), Loc::Gp(2));
        assert_eq!(a.next(Class::Int, false), Loc::Gp(3));
        assert_eq!(a.next(Class::Float, false), Loc::Stack(0));
        assert_eq!(a.next(Class::Int, false), Loc::Stack(8));
        assert_eq!(first_stack_arg(Os::Windows, 0x1000, false), 0x1028);
        assert_eq!(first_stack_arg(Os::Linux, 0x1000, false), 0x1008);
        assert_eq!(first_stack_arg(Os::Linux, 0x1000, true), 0x1000);
    }

    #[test]
    fn a_struct_by_value_takes_the_registers_of_its_eightbytes_or_a_stack_slot_and_shifts_what_follows() {
        let pair = Class::Agg { size: 16, align: 8, sysv: SysV::Regs(true, Some(false)) };
        let big = Class::Agg { size: 24, align: 8, sysv: SysV::Memory };
        let wide = Class::Agg { size: 16, align: 16, sysv: SysV::Regs(false, Some(false)) };
        let mut a = Abi::new(Os::Linux);
        assert_eq!(a.next(pair, false), Loc::Split(Half::Xmm(0), Some(Half::Gp(0))));
        assert_eq!(a.next(big, false), Loc::Stack(0));
        assert_eq!(a.next(Class::Int, false), Loc::Gp(1));
        assert_eq!(a.next(wide, false), Loc::Split(Half::Gp(2), Some(Half::Gp(3))));
        assert_eq!(a.next(Class::Float, false), Loc::Xmm(1));
        // two integer registers are left: an __int128 that needs both still fits, the next one goes to the stack
        // aligned to 16, and a later int still gets the register it left free
        assert_eq!(a.next(Class::Int, false), Loc::Gp(4));
        assert_eq!(a.next(wide, false), Loc::Stack(32));
        assert_eq!(a.next(Class::Int, false), Loc::Gp(5));
        let mut w = Abi::new(Os::Windows);
        assert_eq!(w.next(big, false), Loc::RefGp(0));
        assert_eq!(w.next(Class::Agg { size: 4, align: 4, sysv: SysV::Regs(true, None) }, false), Loc::Gp(1));
        assert_eq!(w.next(Class::Float, false), Loc::Xmm(2));
        assert_eq!(w.next(Class::Int, false), Loc::Gp(3));
        assert_eq!(w.next(pair, false), Loc::RefStack(0));
    }

    #[test]
    fn a_format_string_names_each_argument_it_takes() {
        let lp64 = Sizes { long: 8, ptr: 8 };
        let (c, ok) = conversions(b"%d %5.2f %s %% %lx %*d %c %p %Lg %zu %m", false, lp64);
        assert!(ok);
        assert_eq!(
            c,
            [
                Conv::Int { bytes: 4, signed: true, hex: false },
                Conv::Double,
                Conv::Text,
                Conv::Int { bytes: 8, signed: false, hex: true },
                Conv::Int { bytes: 4, signed: true, hex: false },
                Conv::Int { bytes: 4, signed: true, hex: false },
                Conv::Char,
                Conv::Ptr,
                Conv::LongDouble,
                Conv::Int { bytes: 8, signed: false, hex: false },
            ]
        );
        let (c, _) = conversions(b"%ld", false, Sizes { long: 4, ptr: 8 });
        assert_eq!(c, [Conv::Int { bytes: 4, signed: true, hex: false }]);
        let (c, ok) = conversions(b"%d %*s %[^,], %99s", true, lp64);
        assert!(ok);
        assert_eq!(c, [Conv::Ptr, Conv::Ptr, Conv::Ptr]);
        let (c, ok) = conversions(b"%d %Q %d", false, lp64);
        assert_eq!((c.len(), ok), (1, false));
        assert_eq!(
            (format_kind("__printf_chk"), format_kind("__isoc99_scanf"), format_kind("open")),
            (Some(false), Some(true), None)
        );
    }
}
