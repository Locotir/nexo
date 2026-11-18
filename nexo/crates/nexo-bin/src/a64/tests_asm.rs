use super::asm::assemble;
use super::decode;
use crate::Error;

fn words(text: &str, at: u64) -> Vec<u32> {
    assemble(text, at).unwrap().chunks_exact(4).map(|c| u32::from_le_bytes(c.try_into().unwrap())).collect()
}

fn word(text: &str, at: u64) -> u32 {
    let w = words(text, at);
    assert_eq!(w.len(), 1, "{text}");
    w[0]
}

fn refused(text: &str, needle: &str) {
    match assemble(text, 0x1000) {
        Err(Error::Bad(m)) => {
            assert!(m.contains(text.trim()), "the message must name the instruction: {m}");
            assert!(m.contains(needle), "`{text}`: expected `{needle}` in `{m}`");
        }
        other => panic!("`{text}` should be refused, got {other:?}"),
    }
}

#[test]
fn known_encodings() {
    // (text, address, word): the words come from the decoder tests and from the Arm encodings
    let known: &[(&str, u64, u32)] = &[
        ("nop", 0x1000, 0xd503_201f),
        ("ret", 0x1000, 0xd65f_03c0),
        ("ret x17", 0x1000, 0xd65f_0220),
        ("br x17", 0x1000, 0xd61f_0220),
        ("blr x8", 0x1000, 0xd63f_0100),
        ("bl 0x1040", 0x1000, 0x9400_0010),
        ("b 0x2010", 0x2000, 0x1400_0004),
        ("b.eq 0x2008", 0x2000, 0x5400_0040),
        ("cbz x0, 0x200c", 0x2000, 0xb400_0060),
        ("tbz w0, #3, 0x2008", 0x2000, 0x3618_0040),
        ("adrp x16, 0x400000", 0x400a14, 0x9000_0010),
        ("adr x0, 0x1008", 0x1000, 0x1000_0040),
        ("stp x29, x30, [sp, #-16]!", 0, 0xa9bf_7bfd),
        ("ldp x29, x30, [sp], #16", 0, 0xa8c1_7bfd),
        ("mov x29, sp", 0, 0x9100_03fd),
        ("sub sp, sp, #0x20", 0, 0xd100_83ff),
        ("ldr x0, [sp, #16]", 0, 0xf940_0be0),
        ("str w0, [sp, #12]", 0, 0xb900_0fe0),
        ("ldr x0, 0x1008", 0x1000, 0x5800_0040),
        ("mov w0, #0", 0, 0x5280_0000),
        ("mov w0, #0x2a", 0, 0x5280_0540),
        ("mov w0, #-1", 0, 0x1280_0000),
        ("add w10, w0, #1", 0, 0x1100_040a),
        ("cmp w0, w1", 0, 0x6b01_001f),
        ("cmp x0, #1", 0, 0xf100_041f),
        ("mov x1, x2", 0, 0xaa02_03e1),
        ("movk x0, #0x1234, lsl #16", 0, 0xf2a2_4680),
        ("mov x0, #0x5555555555555555", 0, 0xb200_f3e0),
        ("mov w0, #0x55555555", 0, 0x3200_f3e0),
        ("and x0, x1, #0xff", 0, 0x9240_1c20),
        ("add x0, x1, x2", 0, 0x8b02_0020),
        ("brk #0", 0, 0xd420_0000),
        ("svc #0", 0, 0xd400_0001),
    ];
    for &(text, at, want) in known {
        assert_eq!(word(text, at), want, "{text}: {:#010x}", word(text, at));
    }
}

#[test]
fn text_from_the_printer_assembles_back() {
    let lines = [
        "nop",
        "ret",
        "ret x17",
        "br x17",
        "blr x8",
        "b 0x1040",
        "bl 0xff0",
        "b.eq 0x1008",
        "b.lt 0x1100",
        "b.hs 0x1010",
        "cbz x0, 0x100c",
        "cbnz w1, 0x1020",
        "tbz w0, #3, 0x1008",
        "tbnz x2, #40, 0x1010",
        "adr x0, 0x1008",
        "adrp x16, 0x400000",
        "mov w0, #0x2a",
        "mov x0, #0x1234",
        "mov w0, #-0x1",
        "mov x0, #-0x2",
        "movz x0, #0x0, lsl #16",
        "movk x0, #0x1234, lsl #16",
        "movn w0, #0x0, lsl #16",
        "mov x0, #0x12340000",
        "mov x0, #0x5555555555555555",
        "mov x1, x2",
        "mov x29, sp",
        "mov sp, x0",
        "mov x0, xzr",
        "add x0, x1, #0x10",
        "sub sp, sp, #0x20",
        "add x0, x1, #0x1, lsl #12",
        "adds w0, w1, #0x5",
        "subs x0, x1, #0x5",
        "cmp x0, #0x1",
        "cmn w2, #0x8",
        "cmp w0, w1",
        "add x0, x1, x2, lsl #3",
        "sub w0, w1, w2, asr #1",
        "add x0, sp, x1",
        "cmp sp, x1",
        "and x0, x1, x2",
        "orr w0, w1, w2, lsl #4",
        "eor x0, x1, x2, ror #8",
        "tst x0, x1",
        "ands x0, x1, x2",
        "and x0, x1, #0xff",
        "orr x0, x1, #0x5555555555555555",
        "tst w0, #0xf",
        "eor w0, w1, #0xff00ff00",
        "ldr x0, [sp, #16]",
        "str w0, [sp, #12]",
        "ldrb w1, [x2]",
        "strb w1, [x2, #5]",
        "ldrh w3, [x4, #2]",
        "strh w3, [x4, #6]",
        "ldrsw x0, [x1, #8]",
        "ldr x0, [x1, #8]!",
        "str x0, [sp, #-16]!",
        "ldr w0, [x1], #4",
        "ldr x0, 0x1008",
        "ldp x29, x30, [sp], #16",
        "stp x29, x30, [sp, #-16]!",
        "stp x0, x1, [sp, #16]",
        "ldp w0, w1, [x2, #8]",
        "ldp x0, x1, [x2]",
        "stp x0, x1, [x2, #-8]!",
    ];
    assert!(lines.len() >= 30);
    for text in lines {
        let w = word(text, 0x1000);
        let back = decode(w, 0x1000);
        assert!(back.is_decoded(), "{text}: {w:#010x} did not decode");
        assert_eq!(back.text(), text, "{text} assembled to {w:#010x}");
    }
}

#[test]
fn several_instructions_resolve_targets_per_index() {
    assert_eq!(words("mov x0, #1; ret", 0x1000), [0xd280_0020, 0xd65f_03c0]);
    assert_eq!(words("nop\nb 0x1000 // back to the nop\n", 0x1000), [0xd503_201f, 0x17ff_ffff]);
    // the same target from the second slot is one word closer
    assert_eq!(words("nop; b 0x1008", 0x1000), [0xd503_201f, 0x1400_0001]);
}

#[test]
fn accepts_the_usual_spellings() {
    assert_eq!(word("MOV X0, #42", 0), word("mov x0, #0x2a", 0));
    assert_eq!(word("mov x0, 42", 0), word("mov x0, #0x2a", 0));
    assert_eq!(word("b.cs 0x1008", 0x1000), word("b.hs 0x1008", 0x1000));
    assert_eq!(word("b.cc 0x1008", 0x1000), word("b.lo 0x1008", 0x1000));
    assert_eq!(word("add x0, x1, #0x1, lsl #0", 0), word("add x0, x1, #1", 0));
    assert_eq!(word("mov x29, fp", 0), word("mov x29, x29", 0));
    assert_eq!(word("ret lr", 0), word("ret", 0));
}

#[test]
fn mov_picks_the_single_word_form() {
    // movz, movn, then orr with a bitmask
    assert_eq!(word("mov x0, #0x10000", 0), 0xd2a0_0020);
    assert_eq!(word("mov x0, #-1", 0), 0x9280_0000);
    assert_eq!(word("mov x0, #0xffffffffffffffff", 0), 0x9280_0000);
    assert_eq!(word("mov w0, #0xffffffff", 0), 0x1280_0000);
    assert_eq!(word("mov x0, #0xfffffffffffffffe", 0), 0x9280_0020);
    assert_eq!(word("mov x0, #0xff00ff00ff00ff00", 0), 0xb208_9fe0);
}

#[test]
fn refuses_instead_of_guessing() {
    refused("frobnicate x0", "unknown mnemonic");
    refused("b 0x1002", "multiple of 4");
    refused("b 0x20000000", "reaches");
    refused("b.eq 0x300000", "reaches");
    refused("b.xx 0x1000", "condition");
    refused("cbz x0", "operand");
    refused("tbz w0, #40, 0x1004", "x register");
    refused("adrp x0, 0x1234", "page address");
    refused("mov x0, #0x12345678abcd", "one instruction");
    refused("mov w0, #0x100000000", "does not fit");
    refused("mov w0, x1", "size");
    refused("add x0, x1, w2", "size");
    refused("add x0, x1, #0x1000", "12 bits");
    refused("add x0, xzr, #1", "xzr");
    refused("movz x0, #0x10000", "16 bits");
    refused("movz w0, #1, lsl #32", "not allowed");
    refused("and x0, x1, #0x12345", "bitmask");
    refused("ldr x0, [x1, #4]", "multiple of 8");
    refused("ldr x0, [x1, #-8]", "non-negative");
    refused("ldr x0, [x1, #32768]", "too big");
    refused("ldr x0, [x1, x2]", "immediate offset");
    refused("ldr x0, [x0, #8]!", "unpredictable");
    refused("ldp x0, x0, [x1]", "unpredictable");
    refused("ldp x0, x1, [x2, #512]", "out of range");
    refused("ret w0", "64-bit");
    match assemble("nop; frob", 0x1000) {
        Err(Error::Bad(m)) => assert!(m.contains("`frob`"), "{m}"),
        other => panic!("{other:?}"),
    }
    assert!(matches!(assemble("nop", 0x1002), Err(Error::Bad(_))));
    assert!(matches!(assemble(" ; \n ", 0x1000), Err(Error::Bad(_))));
}
