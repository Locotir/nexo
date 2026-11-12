//! `decomp`: pseudo-C for one x86-64 function.
#![allow(dead_code)]

pub mod ir;
pub mod lift;
pub mod flow;

#[cfg(test)]
pub(crate) mod testenv {
    use super::lift::Env;

    /// Raw code at a fixed address, no symbols.
    pub struct Raw {
        pub base: u64,
        pub code: Vec<u8>,
    }

    impl Env for Raw {
        fn read(&self, addr: u64, len: usize) -> Option<&[u8]> {
            let o = addr.checked_sub(self.base)? as usize;
            self.code.get(o..).map(|s| &s[..len.min(s.len())])
        }
        fn name_of(&self, _: u64) -> Option<String> {
            None
        }
        fn import_slot(&self, _: u64) -> Option<usize> {
            None
        }
        fn import_name(&self, _: usize) -> String {
            String::new()
        }
        fn image_base(&self) -> u64 {
            self.base
        }
        fn is_pe(&self) -> bool {
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::flow::{self, Sig, Sigs};
    use super::ir::*;
    use super::lift::{lift, Abi};
    use super::testenv::Raw;

    struct NoSigs;
    impl Sigs for NoSigs {
        fn sig_of(&mut self, _: &Target) -> Option<Sig> {
            None
        }
        fn cstr(&self, _: u64) -> Option<String> {
            None
        }
    }

    fn dump(f: &super::lift::Lifted) -> String {
        let mut s = String::new();
        for (i, b) in f.blocks.iter().enumerate() {
            s += &format!("B{i}:\n");
            for st in &b.stmts {
                s += &format!("  {:?}\n", st.st);
            }
            s += &format!("  {:?}\n", b.term);
        }
        s
    }

    #[test]
    fn min_of_two() {
        // cmp edi,esi ; jge L ; mov eax,edi ; ret ; L: mov eax,esi ; ret
        let code = vec![0x39, 0xf7, 0x7d, 0x03, 0x89, 0xf8, 0xc3, 0x89, 0xf0, 0xc3];
        let env = Raw { base: 0x1000, code };
        let mut f = lift(&env, 0x1000, 0x100a, Abi::SysV);
        flow::run(&mut f, &mut NoSigs);
        let d = dump(&f);
        println!("{d}");
        assert_eq!(f.blocks.len(), 3, "{d}");
        assert!(d.contains("Return(Some(Var(Reg(7), 32)))") || d.contains("Return(Some("), "{d}");
    }
}
