use super::*;

fn t(raw: u32) -> String {
    decode(raw, 0x1000).text()
}

fn at(raw: u32, pc: u64) -> String {
    decode(raw, pc).text()
}

/// Dev-time check against `llvm-objdump` over a shared object, run by hand:
/// `A64_ORACLE=/path/lib.so cargo test -p nexo-bin a64::tests::oracle -- --ignored --nocapture`.
#[test]
#[ignore]
fn oracle() {
    use object::{Object, ObjectSection};
    let Ok(path) = std::env::var("A64_ORACLE") else { return };
    let data = std::fs::read(&path).unwrap();
    let file = object::File::parse(&data[..]).unwrap();
    let norm = |s: &str| -> String {
        let re = regex::Regex::new(r"0x[0-9a-fA-F]+|\d+").unwrap();
        let s = s.split("//").next().unwrap().split('<').next().unwrap().trim().to_lowercase();
        let s = re.replace_all(&s, |c: &regex::Captures| {
            let m = &c[0];
            match m.strip_prefix("0x") {
                Some(h) => u128::from_str_radix(h, 16).unwrap().to_string(),
                None => m.to_string(),
            }
        });
        s.split_whitespace().collect::<Vec<_>>().join(" ").replace(", ", ",").replace("{ ", "{").replace(" }", "}")
    };
    let out = std::process::Command::new("llvm-objdump")
        .args(["-d", "--no-show-raw-insn", "--print-imm-hex"])
        .arg(&path)
        .output()
        .unwrap();
    let dump = String::from_utf8_lossy(&out.stdout);
    let mut theirs = std::collections::HashMap::new();
    for line in dump.lines() {
        let l = line.trim_start();
        if let Some((a, rest)) = l.split_once(':') {
            if let Ok(addr) = u64::from_str_radix(a, 16) {
                theirs.insert(addr, rest.trim().to_string());
            }
        }
    }
    let (mut same, mut diff, mut undec) = (0, 0, 0);
    let mut kinds = std::collections::BTreeMap::<String, (usize, String)>::new();
    for sec in file.sections().filter(|s| s.kind() == object::SectionKind::Text) {
        let d = sec.data().unwrap();
        for (k, w) in d.as_chunks::<4>().0.iter().enumerate() {
            let addr = sec.address() + 4 * k as u64;
            let raw = u32::from_le_bytes(*w);
            let i = decode(raw, addr);
            let Some(th) = theirs.get(&addr) else { continue };
            if !i.is_decoded() {
                undec += 1;
                let key = th.split_whitespace().next().unwrap_or("").to_string();
                let e = kinds.entry(format!("undec {key}")).or_insert((0, String::new()));
                e.0 += 1;
                e.1 = format!("{raw:08x} {th}");
                continue;
            }
            let (a, b) = (norm(&i.text()), norm(th));
            if a == b {
                same += 1;
            } else {
                diff += 1;
                let key = format!("{} | {}", a.split(' ').next().unwrap_or(""), b.split(' ').next().unwrap_or(""));
                let e = kinds.entry(key).or_insert((0, String::new()));
                e.0 += 1;
                e.1 = format!("{addr:#x} {raw:08x}  mine: {}   theirs: {th}", i.text());
            }
        }
    }
    println!("same {same}, different {diff}, undecoded {undec}");
    let mut v: Vec<_> = kinds.into_iter().collect();
    v.sort_by_key(|(_, (n, _))| std::cmp::Reverse(*n));
    for (k, (n, ex)) in v.into_iter().take(60) {
        println!("{n:6} {k}\n         e.g. {ex}");
    }
}

#[test]
fn common_instructions_read_like_assembly() {
    assert_eq!(t(0xd503201f), "nop");
    assert_eq!(t(0xd65f03c0), "ret");
    assert_eq!(t(0xa9bf7bfd), "stp x29, x30, [sp, #-16]!");
    assert_eq!(t(0x910003fd), "mov x29, sp");
    assert_eq!(t(0xa8c17bfd), "ldp x29, x30, [sp], #16");
    assert_eq!(t(0xd10083ff), "sub sp, sp, #0x20");
    assert_eq!(t(0xf9400be0), "ldr x0, [sp, #16]");
    assert_eq!(t(0xb9000fe0), "str w0, [sp, #12]");
    assert_eq!(t(0x52800000), "mov w0, #0x0");
    assert_eq!(t(0x52800540), "mov w0, #0x2a");
    assert_eq!(t(0x12800000), "mov w0, #-0x1");
    assert_eq!(t(0x1100040a), "add w10, w0, #0x1");
    assert_eq!(t(0x6b01001f), "cmp w0, w1");
    assert_eq!(t(0xf100041f), "cmp x0, #0x1");
    assert_eq!(t(0x1a9f17e0), "cset w0, eq");
    assert_eq!(t(0x9b017c00), "mul x0, x0, x1");
    assert_eq!(t(0x93407c00), "sxtw x0, w0");
    assert_eq!(t(0xd37df000), "lsl x0, x0, #3");
    assert_eq!(t(0x53017c00), "lsr w0, w0, #1");
    assert_eq!(t(0xd53bd041), "mrs x1, tpidr_el0");
    assert_eq!(t(0xd4000001), "svc #0x0");
    assert_eq!(t(0xd5033bbf), "dmb ish");
    assert_eq!(t(0xd503233f), "paciasp");
}

#[test]
fn branches_carry_their_targets() {
    let i = decode(0x9400_0010, 0x1000);
    assert_eq!(i.flow, Flow::Call);
    assert_eq!(i.target, Some(0x1040));
    assert_eq!(i.text(), "bl 0x1040");
    assert_eq!(at(0x1400_0004, 0x2000), "b 0x2010");
    assert_eq!(at(0x5400_0040, 0x2000), "b.eq 0x2008");
    assert_eq!(at(0xb400_0060, 0x2000), "cbz x0, 0x200c");
    assert_eq!(at(0x3618_0040, 0x2000), "tbz w0, #3, 0x2008");
    assert_eq!(decode(0xd63f0100, 0).flow, Flow::CallReg);
    assert_eq!(decode(0xd61f0220, 0).flow, Flow::JumpReg);
    assert_eq!(t(0xd61f0220), "br x17");
    assert_eq!(decode(0xd65f03c0, 0).flow, Flow::Ret);
}

#[test]
fn pc_relative_addresses_are_resolved() {
    let i = decode(0x9000_0010, 0x400a14);
    assert_eq!(i.adrp, Some((16, 0x400000)));
    assert_eq!(at(0x9000_0010, 0x400a14), "adrp x16, 0x400000");
    let j = decode(0x1000_0040, 0x1000);
    assert_eq!(j.adr, Some((0, 0x1008)));
    let l = decode(0x5800_0040, 0x1000);
    assert_eq!(l.literal, Some(0x1008));
    assert_eq!(l.text(), "ldr x0, 0x1008");
}

#[test]
fn words_it_does_not_decode_say_so() {
    let i = decode(0x4ea01c00, 0x1000);
    assert!(!i.is_decoded());
    assert_eq!(i.text(), ".inst 0x4ea01c00");
    assert_eq!(i.undecoded, Some(Why::Simd));
    assert!(!decode(0, 0).is_decoded() || decode(0, 0).mn.as_str() == "udf");
}
