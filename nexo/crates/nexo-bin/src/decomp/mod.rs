//! `decomp`: pseudo-C for one x86-64 function.
#![allow(dead_code, clippy::needless_range_loop)]

pub mod divrec;
pub mod flow;
pub mod ir;
pub mod lift;

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

    /// Calls to 0x1100 are known to leave rdi alone.
    struct KeepsRdi;
    impl Sigs for KeepsRdi {
        fn sig_of(&mut self, _: &Target) -> Option<Sig> {
            Some(Sig { args: 1, void: true, ..Sig::default() })
        }
        fn cstr(&self, _: u64) -> Option<String> {
            None
        }
        fn keeps(&mut self, t: &Target) -> Vec<u8> {
            if matches!(t, Target::Direct(0x1100)) {
                vec![7]
            } else {
                Vec::new()
            }
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

    #[test]
    fn calls_inside_a_loop_stay() {
        // L: mov edi,1 ; call X ; cmp eax,12 ; je L ; ret
        let code = vec![0xbf, 1, 0, 0, 0, 0xe8, 0x40, 0, 0, 0, 0x83, 0xf8, 0x0c, 0x74, 0xf1, 0xc3];
        let env = Raw { base: 0x1000, code: code.clone() };
        let mut f = lift(&env, 0x1000, 0x1010, Abi::SysV);
        println!("LIFTED\n{}", dump(&f));
        flow::run(&mut f, &mut NoSigs);
        println!("FLOW\n{}", dump(&f));
        let t = text(code);
        println!("{t}");
        assert!(t.contains("do {") && t.contains("} while ((u32)v1 == 0xc);"), "{t}");
    }

    #[test]
    fn setcc_then_movzx_is_a_plain_comparison() {
        // mov eax,[rdi+0x6c] ; sub eax,1 ; cmp eax,8 ; setbe al ; movzx eax,al ; ret
        let t =
            text(vec![0x8b, 0x47, 0x6c, 0x83, 0xe8, 0x01, 0x83, 0xf8, 0x08, 0x0f, 0x96, 0xc0, 0x0f, 0xb6, 0xc0, 0xc3]);
        println!("{t}");
        assert!(!t.contains("INSERT"), "{t}");
        assert!(t.contains("return") && t.contains("<= 8"), "{t}");
    }

    #[test]
    fn flags_survive_a_register_being_reused() {
        // test eax,eax ; mov eax,[rdi] ; je L ; mov eax,1 ; L: ret
        let t = text(vec![0x85, 0xc0, 0x8b, 0x07, 0x74, 0x05, 0xb8, 1, 0, 0, 0, 0xc3]);
        println!("{t}");
        assert!(!t.contains("flags"), "{t}");
    }

    #[test]
    fn test_and_branch_folds_flags() {
        // test edi,1 ; je L ; mov eax,1 ; ret ; L: mov eax,2 ; ret
        let t = text(vec![0xf7, 0xc7, 1, 0, 0, 0, 0x74, 6, 0xb8, 1, 0, 0, 0, 0xc3, 0xb8, 2, 0, 0, 0, 0xc3]);
        println!("{t}");
        assert!(!t.contains("flags"), "{t}");
        assert!(t.contains("a1 & 1"), "{t}");
    }

    #[test]
    fn a_register_the_callee_leaves_alone_is_still_the_argument_after_the_call() {
        // f: sub rsp,8 ; call 0x1100 ; mov rax,[rdi] ; add rsp,8 ; ret
        let mut code =
            vec![0x48, 0x83, 0xec, 0x08, 0xe8, 0xf7, 0, 0, 0, 0x48, 0x8b, 0x07, 0x48, 0x83, 0xc4, 0x08, 0xc3];
        code.resize(0x100, 0x90);
        // g: mov qword ptr [rdi+8],0 ; ret
        code.extend([0x48, 0xc7, 0x47, 0x08, 0, 0, 0, 0, 0xc3]);
        let env = Raw { base: 0x1000, code };
        let run = |sigs: &mut dyn Sigs| {
            let l = super::decompile(&env, 0x1000, 0x1011, Abi::SysV, sigs, &NoNames, "f");
            l.iter().map(|x| format!("{}\n", x.text)).collect::<String>()
        };
        let kept = run(&mut KeepsRdi);
        println!("{kept}");
        assert!(kept.contains("return *(u64*)a1;"), "{kept}");
        // without the knowledge, rdi after the call is lost and the read has no source
        let lost = run(&mut NoSigs);
        assert!(!lost.contains("return *(u64*)a1;"), "{lost}");
    }

    fn after_a_call(tail: &[u8]) -> String {
        // sub rsp,8 ; call 0x1100 ; <tail>
        let mut code = vec![0x48, 0x83, 0xec, 0x08, 0xe8, 0xf7, 0, 0, 0];
        code.extend_from_slice(tail);
        let end = 0x1000 + code.len() as u64;
        code.resize(0x100, 0x90);
        code.push(0xc3);
        let env = Raw { base: 0x1000, code };
        let l = super::decompile(&env, 0x1000, end, Abi::SysV, &mut NoSigs, &NoNames, "f");
        l.iter().map(|x| format!("{}\n", x.text)).collect()
    }

    #[test]
    fn a_signed_clamp_is_not_rendered_unsigned() {
        // mov edx,10 ; cmp eax,edx ; cmovl eax,edx ; add rsp,8 ; ret   (if (x < 10) x = 10)
        let t = after_a_call(&[0xba, 10, 0, 0, 0, 0x39, 0xd0, 0x0f, 0x4c, 0xc2, 0x48, 0x83, 0xc4, 8, 0xc3]);
        println!("{t}");
        assert!(t.contains("(i32)v1 < 0xa"), "{t}");
        assert!(!t.contains("(u32)v1 < 0xa"), "{t}");
    }

    #[test]
    fn a_signed_guard_is_not_rendered_unsigned() {
        // cmp eax,0xff ; jg L ; add rsp,8 ; ret ; L: xor eax,eax ; add rsp,8 ; ret   (if (c > 255) return 0)
        let t = after_a_call(&[
            0x3d, 0xff, 0, 0, 0, 0x7f, 5, 0x48, 0x83, 0xc4, 8, 0xc3, 0x31, 0xc0, 0x48, 0x83, 0xc4, 8, 0xc3,
        ]);
        println!("{t}");
        assert!(t.contains("(i32)v1 > 0xff"), "{t}");
        assert!(!t.contains("(u32)v1 > 0xff"), "{t}");
    }

    /// The terminators of a lifted function, as text.
    fn terms(f: &super::lift::Lifted) -> String {
        f.blocks.iter().map(|b| format!("{:?}", b.term)).collect::<Vec<_>>().join("\n")
    }

    #[test]
    fn a_jump_is_not_a_tail_call_when_the_stack_pointer_is_unknown() {
        // mov rsp, rdx ; jmp rax
        let env = Raw { base: 0x1000, code: vec![0x48, 0x89, 0xd4, 0xff, 0xe0] };
        let f = lift(&env, 0x1000, 0x1005, Abi::SysV);
        let t = terms(&f);
        assert!(!t.contains("TailCall"), "an unknown frame is not a torn-down frame\n{t}");
        assert_eq!(f.unresolved_jumps, 1, "{t}");
    }

    #[test]
    fn a_register_jump_after_a_scaled_table_load_is_left_unresolved() {
        // mov rax, [rdi*8+0x1020] ; jmp rax ; ret padding ; the table at 0x1020 points back into the function
        let mut code = vec![0x48, 0x8b, 0x04, 0xfd, 0x20, 0x10, 0x00, 0x00, 0xff, 0xe0];
        code.resize(0x20, 0xc3);
        code.extend_from_slice(&0x1010u64.to_le_bytes());
        code.extend_from_slice(&0x1011u64.to_le_bytes());
        let env = Raw { base: 0x1000, code };
        let f = lift(&env, 0x1000, 0x1020, Abi::SysV);
        let t = terms(&f);
        assert!(!t.contains("TailCall"), "a table dispatch is not a tail call\n{t}");
        assert_eq!(f.unresolved_jumps, 1, "{t}");
    }

    #[test]
    fn a_switch_whose_table_address_was_loaded_far_back_is_left_unresolved() {
        // lea rcx,[rip+0x39] ; 12 nops ; movsxd rax,[rcx+rdi*4] ; add rax,rcx ; jmp rax ; ret padding
        let mut code = vec![0x48, 0x8d, 0x0d, 0x39, 0, 0, 0];
        code.extend(std::iter::repeat_n(0x90, 12));
        code.extend_from_slice(&[0x48, 0x63, 0x04, 0xb9, 0x48, 0x01, 0xc8, 0xff, 0xe0]);
        code.resize(0x30, 0xc3);
        let env = Raw { base: 0x1000, code };
        let f = lift(&env, 0x1000, 0x1030, Abi::SysV);
        let t = terms(&f);
        assert!(!t.contains("TailCall"), "the lea is far back but it is still a table dispatch\n{t}");
        assert_eq!(f.unresolved_jumps, 1, "{t}");
    }

    #[test]
    fn a_memory_jump_through_a_table_that_points_into_the_function_is_left_unresolved() {
        // cmp edi,1 ; ja L ; jmp [rdi*8+0x1020] ; L: ret ; the table has one entry here and one far away
        let mut code = vec![0x83, 0xff, 0x01, 0x77, 0x07, 0xff, 0x24, 0xfd, 0x20, 0x10, 0x00, 0x00];
        code.resize(0x20, 0xc3);
        code.extend_from_slice(&0x100cu64.to_le_bytes());
        code.extend_from_slice(&0x9999u64.to_le_bytes());
        let env = Raw { base: 0x1000, code };
        let f = lift(&env, 0x1000, 0x1020, Abi::SysV);
        let t = terms(&f);
        assert!(!t.contains("TailCall"), "{t}");
        assert_eq!(f.unresolved_jumps, 1, "{t}");
    }
    #[test]
    fn a_jump_through_a_pointer_field_with_the_frame_gone_is_still_a_tail_call() {
        // mov rax, [rdi+8] ; jmp rax
        let env = Raw { base: 0x1000, code: vec![0x48, 0x8b, 0x47, 0x08, 0xff, 0xe0] };
        let f = lift(&env, 0x1000, 0x1006, Abi::SysV);
        let t = terms(&f);
        assert!(t.contains("TailCall"), "{t}");
        assert_eq!(f.unresolved_jumps, 0, "{t}");
    }
}
pub mod api;
pub mod render;
pub mod structure;
pub mod types;

/// The blocks as plain text, for debugging (`NEXO_DECOMP_IR=1` prints it on stderr).
pub fn ir_text(f: &lift::Lifted) -> String {
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
    let debug = std::env::var_os("NEXO_DECOMP_IR").is_some();
    if debug {
        eprintln!("LIFTED\n{}", ir_text(&f));
    }
    let mut facts = flow::run(&mut f, sigs);
    if debug {
        eprintln!("FLOW\n{}", ir_text(&f));
    }
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
