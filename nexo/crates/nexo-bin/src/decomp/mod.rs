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

    pub(crate) struct NoNames;
    impl super::render::Names for NoNames {
        fn func(&self, _: u64) -> Option<String> {
            None
        }
        fn import(&self, i: usize) -> String {
            format!("imp{i}")
        }
        fn string(&self, _: u64) -> Option<String> {
            None
        }
        fn global(&self, _: u64) -> Option<String> {
            None
        }
    }

    fn text(code: Vec<u8>) -> String {
        let n = code.len() as u64;
        let env = Raw { base: 0x1000, code };
        let l = super::decompile(&env, 0x1000, 0x1000 + n, Abi::SysV, &mut NoSigs, &NoNames, "f");
        l.iter().map(|x| format!("{}{}\n", "    ".repeat(x.indent), x.text)).collect()
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
        let t = text(vec![0x39, 0xf7, 0x7d, 0x03, 0x89, 0xf8, 0xc3, 0x89, 0xf0, 0xc3]);
        println!("{t}");
        assert!(t.contains("if (a1 < a2)") || t.contains("if (a1 >= a2)"), "{t}");
        assert!(d.contains("Return(Some(Var(Reg(7), 32)))") || d.contains("Return(Some("), "{d}");
    }
}
pub mod structure;
pub mod api;
pub mod types;
pub mod render;

pub fn decompile(
    env: &dyn lift::Env,
    start: u64,
    end: u64,
    abi: lift::Abi,
    sigs: &mut dyn flow::Sigs,
    names: &dyn render::Names,
    name: &str,
) -> Vec<render::Line> {
    decompile_with(env, start, end, abi, sigs, names, name, None)
}

/// Lift, analyse, structure and print one function; a prototype from the person fixes the signature.
#[allow(clippy::too_many_arguments)]
pub fn decompile_with(
    env: &dyn lift::Env,
    start: u64,
    end: u64,
    abi: lift::Abi,
    sigs: &mut dyn flow::Sigs,
    names: &dyn render::Names,
    name: &str,
    proto: Option<&types::Proto>,
) -> Vec<render::Line> {
    let mut f = lift::lift(env, start, end, abi);
    let mut facts = flow::run(&mut f, sigs);
    if let Some(p) = proto {
        facts.argc = p.args.len().min(abi.arg_count_max());
        if p.ret == "void" {
            facts.ret = None;
        }
    }
    let web = render::make_webs(&mut f, &facts);
    let body = structure::structure(&mut f);
    render::render(&f, &facts, &web, &body, names, name, proto)
}
