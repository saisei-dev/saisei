//! Ported from tests/test_*.py — rcl, rcr, relocation, repe, repne, ret, ror,
//! sar, sbb_rol_loop, scasb, shr, sign_flag, single_header_instruction,
//! ss_interrupt_shadow, stack_indexed_bp, stick_loop, sub_cf, switch, terminates.
//!
//! PORT DISPOSITIONS (C backend deleted):
//!   ported:    40 tests — every C-text assertion re-asserted against the Rust
//!              chunk backend (`render_rs`/`render_rs_ir`). The reloc tests
//!              drive relocations through the IR (`{"functions": …,
//!              "relocations": [{"segment", "offset"}]}`) instead of poking the
//!              retired renderer's `reloc_offsets` field. A formerly
//!              #[ignore]d test is live again: terminates__on_noreturn_funcs
//!              (the private
//!              _terminates/_NORETURN_FUNCS check re-expressed as the
//!              observable state-machine behavior: a noreturn runtime call ends
//!              its block with `return -1;` and drops the unreachable tail).
//!              Purely C-structural assertions dropped from otherwise-ported
//!              tests: switch__* (×4: the translate-time jump-table decode via
//!              code_bytes and the C `switch`/case-label structuring have no
//!              Rust equivalent by design — the Rust backend keeps the register
//!              prep and dispatches the indirect jmp through jump_table_ at
//!              runtime, which is what is asserted now),
//!              single_header_instruction__loop (do-loop shape -> memb_write +
//!              pc back-edge), stick_loop (while(1) -> pc back-edge),
//!              ret__near / ret__far_only (file/function-tail shape -> the
//!              popped-ip epilogue / arm-final `return;`),
//!              sbb_rol_loop__loopne (goto-absence is vacuous in the state
//!              machine; the dec-and-branch core kept), sign_flag__* (the
//!              private handle_arithmetic call was already re-expressed via a
//!              full render in the C port; same here via render_rs).
//!   collapsed: (none)
//!   deleted:   (none)
#![allow(non_snake_case)]
mod common;
use common::*;
use serde_json::{json, Value};

// ---------------------------------------------------------------------------
// local helpers
// ---------------------------------------------------------------------------

/// Mirrors the old test_ir_to_c_relocation._render: one instruction whose
/// segment word (at linear `reloc_off`) is relocated. The Rust backend takes
/// relocations from the IR itself: (segment<<4)+offset == reloc_off, and its
/// load_segment is the fixed 0x1010.
fn reloc_render(insn: Value, reloc_off: i64) -> String {
    let start = as_i(&insn, "address");
    let ir = json!({
        "functions": [{ "start": start, "instructions": [insn] }],
        "relocations": [{ "segment": reloc_off >> 4, "offset": reloc_off & 0xF }],
    });
    render_rs_ir(&ir, &[], "").expect("emit_rust")
}

// ===========================================================================
// ===========================================================================

#[test]
fn rcl__rotates_through_cf() {
    let func = json!({
        "start": 0x0000,
        "instructions": [
            {"address": 0x0000, "mnemonic": "rcl", "op_str": "ax, 1", "bytes": "D1D0"},
            {"address": 0x0002, "mnemonic": "ret", "op_str": "", "bytes": "C3"},
        ],
    });
    let src = render_rs(&func, &[], "");
    assert!(
        src.contains("let mut count: u32 = ((0x1) as u32 & 0x1F) % 17;"),
        "{src}"
    );
    assert!(src.contains("let orig_count = count;"), "{src}");
    assert!(src.contains("while count != 0 { count -= 1;"), "{src}");
    assert!(
        src.contains("let new_cf: u8 = ((tmp >> 15) & 1) as u8;"),
        "{src}"
    );
    assert!(
        src.contains("tmp = ((((tmp as u32) << 1) | (r.CF() as u32)) & 0xFFFF) as u16;"),
        "{src}"
    );
    assert!(src.contains("r.set_CF(new_cf);"), "{src}");
    assert!(src.contains("if orig_count == 1 {"), "{src}");
    assert!(
        src.contains("r.set_OF((((tmp >> 15) & 1) as u8) ^ r.CF());"),
        "{src}"
    );
    // rcl affects only CF/OF — no unconditional OF clear, no ZF/PF/SF writes
    assert!(!src.contains("r.set_OF(0)"), "{src}");
    assert!(!src.contains("set_ZF"), "{src}");
    assert!(!src.contains("set_PF"), "{src}");
    assert!(!src.contains("set_SF"), "{src}");
}

// ===========================================================================
// ===========================================================================

#[test]
fn rcr__register_translate() {
    let func = json!({
        "start": 0x0000,
        "instructions": [
            {"address": 0x0000, "mnemonic": "rcr", "op_str": "ax, 1", "bytes": "D1D8"},
            {"address": 0x0002, "mnemonic": "ret", "op_str": "", "bytes": "C3"},
        ],
    });
    let src = render_rs(&func, &[], "");
    assert!(
        src.contains("let mut count: u32 = ((0x1) as u32 & 0x1F) % 17;"),
        "{src}"
    );
    assert!(src.contains("let orig_count = count;"), "{src}");
    assert!(src.contains("let new_cf: u8 = (tmp & 1) as u8;"), "{src}");
    assert!(
        src.contains("tmp = ((((tmp as u32) >> 1) | ((r.CF() as u32) << 15)) & 0xFFFF) as u16;"),
        "{src}"
    );
    assert!(
        src.contains("r.set_OF((((tmp >> 15) & 1) ^ ((tmp >> 14) & 1)) as u8);"),
        "{src}"
    );
    assert!(!src.contains("r.set_OF(0)"), "{src}");
    assert!(!src.contains("set_ZF"), "{src}");
    assert!(!src.contains("set_SF"), "{src}");
    assert!(!src.contains("set_PF"), "{src}");
}

// ===========================================================================
// ===========================================================================

#[test]
fn relocation__lcall_immediate_segment_is_relocated() {
    // lcall 0:0 (9A 00 00 00 00) at 0x1518B; seg word at 0x1518E is relocated
    // -> segment becomes load_segment 0x1010; return IP is the fallthrough.
    let src = reloc_render(
        json!({"address": 0x1518B, "mnemonic": "lcall", "op_str": "0, 0",
               "bytes": "9a00000000"}),
        0x1518E,
    );
    assert!(
        src.contains(
            "r.lcall_table_(((0x15190u32).wrapping_add(0x10100).wrapping_sub((r.cs() as u32) << 4)) as u16, \
             0x1010, 0x0000);"
        ),
        "{src}"
    );
}

#[test]
fn relocation__lcall_immediate_segment_not_relocated_unchanged() {
    // No relocation on the seg word -> segment stays as decoded.
    let func = json!({"start": 0x100, "instructions": [
        {"address": 0x100, "mnemonic": "lcall", "op_str": "0x6c0, 0x4a7",
         "bytes": "9aa704c006"}]});
    let src = render_rs(&func, &[], "");
    assert!(
        src.contains(
            "r.lcall_table_(((0x105u32).wrapping_add(0x10100).wrapping_sub((r.cs() as u32) << 4)) as u16, \
             0x06C0, 0x04A7);"
        ),
        "{src}"
    );
}

#[test]
fn relocation__ljmp_immediate_segment_is_relocated() {
    // ljmp 0:0x100 (EA 00 01 00 00) at 0x2000; seg word at 0x2003 relocated.
    let src = reloc_render(
        json!({"address": 0x2000, "mnemonic": "ljmp", "op_str": "0:0x100",
               "bytes": "ea00010000"}),
        0x2003,
    );
    assert!(src.contains("r.long_jump_(0x1010, 0x0100);"), "{src}");
}

// ===========================================================================
// ===========================================================================

#[test]
fn repe__cmpsb_translates_faithful_inline() {
    let func = json!({
        "start": 0x0000,
        "instructions": [
            {"address": 0x0000, "mnemonic": "repe cmpsb",
             "op_str": "byte ptr [si], byte ptr es:[di]", "bytes": "F3A6"},
            {"address": 0x0002, "mnemonic": "ret", "op_str": "", "bytes": "C3"},
        ],
    });
    let src = render_rs(&func, &[], "");
    assert!(
        src.contains("let delta: i32 = if r.DF() != 0 { -1 } else { 1 };"),
        "{src}"
    );
    assert!(src.contains("lv = r.memb(r.ds(), r.si());"), "{src}");
    assert!(src.contains("rv = r.memb(r.es(), r.di());"), "{src}");
    assert!(src.contains("if lv != rv { break; }"), "{src}"); // ZF->0 ends repe
    assert!(src.contains("r.set_cx(count.wrapping_sub(i));"), "{src}"); // remaining count
    assert!(src.contains("r.set_ZF((res == 0) as u8);"), "{src}");
    assert!(src.contains("r.set_CF((l32 < r32) as u8);"), "{src}");
}

#[test]
fn repe__scasb_and_cmpsw_translate() {
    // repe cmpsb/cmpsw/scasb all have dedicated handlers; they must translate.
    for (mnem, b) in [("repe cmpsw", "F3A7"), ("repe scasb", "F3AE")] {
        let func = json!({
            "start": 0x0000,
            "instructions": [
                {"address": 0x0000, "mnemonic": mnem,
                 "op_str": "byte ptr es:[di]", "bytes": b},
                {"address": 0x0002, "mnemonic": "ret", "op_str": "", "bytes": "C3"},
            ],
        });
        let body = render_rs(&func, &[], "");
        assert!(body.contains("while i < count {"), "{body}");
    }
}

/// Every prefix x string-compare pair the 8086 can encode must emit. The four
/// bases (cmpsb/cmpsw/scasb/scasw) x repe/repne are one loop with one difference:
/// the ZF that ends it. Popcorn far-jumps into runtime-loaded code holding
/// `repne cmpsb` (F2 A6) — the whole chunk failed to translate over it, so the
/// game could not boot (crash bundle: jit_compile_failed at 2AD2:0113).
#[test]
fn rep_cmp__every_prefix_and_base_translates() {
    // (mnemonic, bytes, expect-si-advance, expect-word-width)
    let cases = [
        ("repe cmpsb", "F3A6", true, false),
        ("repe cmpsw", "F3A7", true, true),
        ("repe scasb", "F3AE", false, false),
        ("repe scasw", "F3AF", false, true),
        ("repne cmpsb", "F2A6", true, false),
        ("repne cmpsw", "F2A7", true, true),
        ("repne scasw", "F2AF", false, true),
    ];
    for (mnem, bytes, advances_si, word) in cases {
        let func = json!({
            "start": 0x0000,
            "instructions": [
                {"address": 0x0000, "mnemonic": mnem, "op_str": "", "bytes": bytes},
                {"address": 0x0002, "mnemonic": "ret", "op_str": "", "bytes": "C3"},
            ],
        });
        let src = render_rs(&func, &[], "");
        assert!(src.contains("while i < count {"), "{mnem}: {src}");
        // repe runs while ZF=1 (stop on the first mismatch); repne runs while
        // ZF=0 (stop on the first match). That inversion is the prefix.
        let brk = if mnem.starts_with("repne") {
            "if lv == rv { break; }"
        } else {
            "if lv != rv { break; }"
        };
        assert!(src.contains(brk), "{mnem} wrong stop condition: {src}");
        // cmps compares [seg:si] with [es:di] and advances both; scas compares
        // the accumulator with [es:di] and advances di alone.
        let acc = if word { "r.ax()" } else { "r.al()" };
        let m = if word { "memw" } else { "memb" };
        if advances_si {
            assert!(
                src.contains(&format!("lv = r.{m}(r.ds(), r.si());")),
                "{mnem}: {src}"
            );
            assert!(src.contains("r.set_si("), "{mnem} must advance si: {src}");
        } else {
            assert!(src.contains(&format!("lv = {acc};")), "{mnem}: {src}");
            assert!(
                !src.contains("r.set_si("),
                "{mnem} must not touch si: {src}"
            );
        }
        assert!(
            src.contains(&format!("rv = r.{m}(r.es(), r.di());")),
            "{mnem}: {src}"
        );
        assert!(src.contains("r.set_di("), "{mnem} must advance di: {src}");
        // cx is left holding the untraversed remainder, and the flags come from
        // the last compare actually performed (cx=0 on entry leaves them alone).
        assert!(
            src.contains("r.set_cx(count.wrapping_sub(i));"),
            "{mnem}: {src}"
        );
        assert!(src.contains("if i > 0 {"), "{mnem}: {src}");
        assert!(src.contains("r.set_ZF((res == 0) as u8);"), "{mnem}: {src}");
        assert!(
            src.contains("r.set_CF((l32 < r32) as u8);"),
            "{mnem}: {src}"
        );
    }
}

/// F3 in front of a string compare *is* REPE — same opcode, same semantics.
/// A decoder that spells it "rep cmpsb" must not fall off the supported set.
#[test]
fn rep_cmp__bare_rep_prefix_on_a_compare_is_repe() {
    for (mnem, bytes) in [("rep cmpsb", "F3A6"), ("rep scasw", "F3AF")] {
        let func = json!({
            "start": 0x0000,
            "instructions": [
                {"address": 0x0000, "mnemonic": mnem, "op_str": "", "bytes": bytes},
                {"address": 0x0002, "mnemonic": "ret", "op_str": "", "bytes": "C3"},
            ],
        });
        let src = render_rs(&func, &[], "");
        assert!(src.contains("if lv != rv { break; }"), "{mnem}: {src}");
    }
}

/// Only the compares set flags, so only they can end a repeat early on ZF. On
/// every other string op F2 and F3 mean the same thing — repeat CX times — and
/// the assembler's choice of spelling must not decide whether we can run it.
/// MechWarrior clears a buffer with `repne stosw`, which is `rep stosw`.
#[test]
fn rep__zf_prefix_on_a_non_compare_is_a_plain_rep() {
    for (mnem, bytes, plain) in [
        ("repne stosw", "F2AB", "rep stosw"),
        ("repe stosw", "F3AB", "rep stosw"),
        ("repne movsb", "F2A4", "rep movsb"),
        ("repne lodsb", "F2AC", "rep lodsb"),
    ] {
        let one = |m: &str, b: &str| {
            let func = json!({
                "start": 0x0000,
                "instructions": [
                    {"address": 0x0000, "mnemonic": m, "op_str": "", "bytes": b},
                    {"address": 0x0002, "mnemonic": "ret", "op_str": "", "bytes": "C3"},
                ],
            });
            render_rs(&func, &[], "")
        };
        // Byte-for-byte the same body as the F3 spelling, and never the compare
        // loop (there is no ZF here for a repeat to test).
        assert_eq!(one(mnem, bytes), one(plain, bytes), "{mnem} != {plain}");
        assert!(!one(mnem, bytes).contains("if lv == rv"), "{mnem}");
    }
}

/// The port-string ops. Unlike rep movs/stos these must stay a real loop: each
/// iteration is a separate port access, and a device answers differently every
/// time — collapsing it into a block copy would read one byte and duplicate it.
#[test]
fn ins_outs__port_string_ops_translate() {
    let cases = [
        ("insb", "6C", "r.memb_write(r.es(), r.di(), r.inb(r.dx()));"),
        ("insw", "6D", "r.memw_write(r.es(), r.di(), r.inw(r.dx()));"),
        ("outsb", "6E", "r.outb(r.dx(), r.memb(r.ds(), r.si()));"),
        ("outsw", "6F", "r.outw(r.dx(), r.memw(r.ds(), r.si()));"),
    ];
    for (mnem, bytes, expect) in cases {
        for prefixed in [false, true] {
            let m = if prefixed {
                format!("rep {mnem}")
            } else {
                mnem.to_string()
            };
            let func = json!({
                "start": 0x0000,
                "instructions": [
                    {"address": 0x0000, "mnemonic": m, "op_str": "", "bytes": bytes},
                    {"address": 0x0002, "mnemonic": "ret", "op_str": "", "bytes": "C3"},
                ],
            });
            let src = render_rs(&func, &[], "");
            assert!(src.contains(expect), "{m}: {src}");
            // ins walks di, outs walks si — each in the direction DF picks.
            let walks = if mnem.starts_with("ins") {
                "r.set_di("
            } else {
                "r.set_si("
            };
            assert!(src.contains(walks), "{m} must walk its pointer: {src}");
            assert!(src.contains("if r.DF() != 0"), "{m} must honor DF: {src}");
            if prefixed {
                assert!(src.contains("while r.cx() != 0 {"), "{m} must loop: {src}");
                assert!(
                    src.contains("r.set_cx(r.cx().wrapping_sub(1));"),
                    "{m} must count down: {src}"
                );
            }
        }
    }
}

// ===========================================================================
// ===========================================================================

#[test]
fn repne__scasb_calls_scan_memory_for_al() {
    let func = json!({
        "start": 0x0000,
        "instructions": [
            {"address": 0x0000, "mnemonic": "repne scasb",
             "op_str": "byte ptr es:[di]", "bytes": "F2AE"},
            {"address": 0x0002, "mnemonic": "ret", "op_str": "", "bytes": "C3"},
        ],
    });
    let src = render_rs(&func, &[], "");
    assert!(
        src.contains("let delta: i32 = if r.DF() != 0 { -1 } else { 1 };"),
        "{src}"
    );
    assert!(src.contains("let mut last_byte: u8 = 0;"), "{src}");
    assert!(
        src.contains(
            "let index = unsafe { scanMemoryForAl(seg_off(r.es(), r.di()) as *const u8, \
             r.al(), count, delta, &mut last_byte) };"
        ),
        "{src}"
    );
    assert!(
        src.contains("let advance = if index < count { index + 1 } else { index };"),
        "{src}"
    );
    assert!(src.contains("if advance > 0 {"), "{src}");
    assert!(src.contains("r.set_CF((l32 < r32) as u8);"), "{src}");
    assert!(src.contains("r.set_SF(((res >> 7) & 1) as u8);"), "{src}");
    assert!(
        src.contains("r.set_OF((((l32 ^ r32) & (l32 ^ (res as u32)) & 0x80) != 0) as u8);"),
        "{src}"
    );
    assert!(src.contains("r.set_ZF((res == 0) as u8);"), "{src}");
    assert!(
        src.contains("r.set_cx(count.wrapping_sub(advance));"),
        "{src}"
    );
    assert!(
        src.contains("r.set_di(((r.di() as i32 + advance as i32 * delta) & 0xFFFF) as u16);"),
        "{src}"
    );
}

// ===========================================================================
// ===========================================================================

#[test]
fn ret__near() {
    let func = json!({
        "start": 0x0000,
        "instructions": [
            {"address": 0x0000, "mnemonic": "mov", "op_str": "ax, ax", "bytes": "89C0"},
            {"address": 0x0002, "mnemonic": "ret", "op_str": "", "bytes": "C3"},
        ],
    });
    let src = render_rs(&func, &[], "");
    // near ret pops the return IP and re-enters the dispatch loop — no retf
    assert!(
        src.contains("let popped_ip = r.memw(r.ss(), r.sp());"),
        "{src}"
    );
    assert!(
        src.contains(
            "return (((r.cs() as u32) << 4).wrapping_add(popped_ip as u32).wrapping_sub(0x10100)) as i32;"
        ),
        "{src}"
    );
    assert!(!src.contains("retf_"), "{src}");
}

#[test]
fn ret__far_invokes_helper() {
    let func = json!({
        "start": 0x0000,
        "instructions": [
            {"address": 0x0000, "mnemonic": "mov", "op_str": "ax, ax", "bytes": "89C0"},
            {"address": 0x0002, "mnemonic": "retf", "op_str": "", "bytes": "CB"},
        ],
    });
    let src = render_rs(&func, &[], "");
    assert!(src.contains("r.retf_();"), "{src}");
    assert!(src.contains("r.retf_();\n    return -1;"), "{src}");
}

#[test]
fn ret__far_only() {
    let func = json!({
        "start": 0x0000,
        "instructions": [
            {"address": 0x0000, "mnemonic": "retf", "op_str": "", "bytes": "CB"},
        ],
    });
    let src = render_rs(&func, &[], "");
    assert!(src.contains("r.retf_();"), "{src}");
    // the retf terminates its block: the next statement is the done sentinel
    let body = blk(&src, 0x0000);
    assert!(body.contains("r.retf_();\n    return -1;"), "{body}");
    assert!(body.trim_end().ends_with("return -1;"), "{body}");
}

#[test]
fn ret__immediate_adjusts_sp() {
    let func = json!({
        "start": 0x0000,
        "instructions": [
            {"address": 0x0000, "mnemonic": "mov", "op_str": "ax, ax", "bytes": "89C0"},
            {"address": 0x0002, "mnemonic": "ret", "op_str": "4", "bytes": "C20400"},
        ],
    });
    let src = render_rs(&func, &[], "");
    // pop the return IP, then discard the 4 argument bytes
    assert!(
        src.contains("let popped_ip = r.memw(r.ss(), r.sp());"),
        "{src}"
    );
    assert!(
        src.contains("r.set_sp((r.sp().wrapping_add(2)) & 0xFFFF);"),
        "{src}"
    );
    assert!(
        src.contains("r.set_sp((r.sp().wrapping_add(0x4)) & 0xFFFF);"),
        "{src}"
    );
}

#[test]
fn ret__far_immediate_uses_retf_pop() {
    let func = json!({
        "start": 0x0000,
        "instructions": [
            {"address": 0x0000, "mnemonic": "retf", "op_str": "4", "bytes": "CA0400"},
        ],
    });
    let src = render_rs(&func, &[], "");
    assert!(src.contains("r.retf_pop_(0x4);"), "{src}");
    assert!(!src.contains("r.set_sp("), "{src}");
}

#[test]
fn ret__far_decimal_immediate_preserves_decimal_value() {
    let func = json!({
        "start": 0x0000,
        "instructions": [
            {"address": 0x0000, "mnemonic": "retf", "op_str": "12", "bytes": "CA0C00"},
        ],
    });
    let src = render_rs(&func, &[], "");
    // "12" is decimal 12 (0xC) — a hex misparse would give retf_pop_(0x12)
    assert!(src.contains("r.retf_pop_(0xC);"), "{src}");
    assert!(!src.contains("r.retf_pop_(0x12);"), "{src}");
}

#[test]
fn ret__far_in_if_else() {
    let func = json!({
        "start": 0x0000,
        "instructions": [
            {"address": 0x0000, "mnemonic": "cmp", "op_str": "ax, 0", "bytes": "3D0000"},
            {"address": 0x0003, "mnemonic": "jnz", "op_str": "0x0008",
             "target": 0x0008, "bytes": "7503"},
            {"address": 0x0005, "mnemonic": "mov", "op_str": "ax, ax", "bytes": "89C0"},
            {"address": 0x0007, "mnemonic": "ret", "op_str": "", "bytes": "C3"},
            {"address": 0x0008, "mnemonic": "retf", "op_str": "", "bytes": "CB"},
        ],
    });
    let src = render_rs(&func, &[], "");
    assert!(src.contains("if r.ZF() == 0 {"), "{src}");
    assert!(src.contains("return 0x0008;"), "{src}");
    assert!(src.contains("r.retf_();"), "{src}");
}

// ===========================================================================
// ===========================================================================

#[test]
fn ror__translates_to_rotate_right() {
    let func = json!({
        "start": 0x0000,
        "instructions": [
            {"address": 0x0000, "mnemonic": "ror", "op_str": "al, 1", "bytes": "D0C8"},
            {"address": 0x0002, "mnemonic": "ret", "op_str": "", "bytes": "C3"},
        ],
    });
    let src = render_rs(&func, &[], "");
    assert!(src.contains("let count: u32 = (0x1) as u32 & 7;"), "{src}");
    assert!(src.contains("if count != 0 {"), "{src}");
    assert!(
        src.contains("let v = ((d0 >> count) | (d0 << (8 - count))) & 0xFF;"),
        "{src}"
    );
    assert!(
        src.contains("r.set_CF((((r.al()) >> 7) & 1) as u8);"),
        "{src}"
    );
    assert!(
        src.contains(
            "r.set_OF(if count == 1 { ((((r.al()) >> 7) & 1) ^ (((r.al()) >> 6) & 1)) as u8 } else { 0 });"
        ),
        "{src}"
    );
    assert!(!src.contains("set_ZF"), "{src}");
    assert!(!src.contains("set_PF"), "{src}");
}

// ===========================================================================
// ===========================================================================

#[test]
fn sar__uses_arithmetic_shift() {
    let func = json!({
        "start": 0x0000,
        "instructions": [
            {"address": 0x0000, "mnemonic": "sar", "op_str": "al, 1", "bytes": "D0F8"},
            {"address": 0x0002, "mnemonic": "sar", "op_str": "ax, 1", "bytes": "D1F8"},
            {"address": 0x0004, "mnemonic": "ret", "op_str": "", "bytes": "C3"},
        ],
    });
    let src = render_rs(&func, &[], "");
    assert_eq!(
        src.matches("let mut count: u32 = (0x1) as u32 & 0x1F;")
            .count(),
        2,
        "{src}"
    );
    assert_eq!(src.matches("let orig_count = count;").count(), 2, "{src}");
    assert!(src.contains("let mut tmp: i8 = (r.al()) as i8;"), "{src}");
    assert!(src.contains("let mut tmp: i16 = (r.ax()) as i16;"), "{src}");
    assert_eq!(
        src.matches("r.set_CF((tmp & 1) as u8);").count(),
        2,
        "{src}"
    );
    assert!(src.contains("r.set_al((tmp as u8));"), "{src}");
    assert!(src.contains("r.set_ax((tmp as u16));"), "{src}");
    assert_eq!(src.matches("r.set_OF(0);").count(), 2, "{src}");
}

// ===========================================================================
// ===========================================================================

#[test]
fn sbb_rol_loop__rol_translates_to_rotate_left() {
    let func = json!({
        "start": 0x0000,
        "instructions": [
            {"address": 0x0000, "mnemonic": "rol", "op_str": "al, 1", "bytes": "D0C0"},
            {"address": 0x0002, "mnemonic": "ret", "op_str": "", "bytes": "C3"},
        ],
    });
    let src = render_rs(&func, &[], "");
    assert!(src.contains("let count: u32 = (0x1) as u32 & 7;"), "{src}");
    assert!(src.contains("if count != 0 {"), "{src}");
    assert!(
        src.contains("let v = ((d0 << count) | (d0 >> (8 - count))) & 0xFF;"),
        "{src}"
    );
    assert!(src.contains("r.set_CF(((r.al()) & 1) as u8);"), "{src}");
    assert!(
        src.contains(
            "r.set_OF(if count == 1 { (((r.al()) >> 7) & 1) as u8 ^ r.CF() } else { 0 });"
        ),
        "{src}"
    );
    assert!(!src.contains("set_ZF"), "{src}");
    assert!(!src.contains("set_PF"), "{src}");
}

#[test]
fn sbb_rol_loop__sbb_subtracts_with_carry() {
    let func = json!({
        "start": 0x0000,
        "instructions": [
            {"address": 0x0000, "mnemonic": "sbb", "op_str": "al, 0x1", "bytes": "1C01"},
            {"address": 0x0002, "mnemonic": "ret", "op_str": "", "bytes": "C3"},
        ],
    });
    let src = render_rs(&func, &[], "");
    assert!(src.contains("let old: u32 = (r.al()) as u32;"), "{src}");
    assert!(
        src.contains("let src: u32 = (0x1 as u32).wrapping_add(r.CF() as u32);"),
        "{src}"
    );
    assert!(src.contains("r.set_CF((old < src) as u8);"), "{src}");
    assert!(
        src.contains("let tmp: u32 = old.wrapping_sub(src);"),
        "{src}"
    );
    assert!(src.contains("r.set_al((tmp & 0xFF) as u8);"), "{src}");
    assert!(
        src.contains("r.set_OF((((old ^ src) & (old ^ tmp) & 0x80) != 0) as u8);"),
        "{src}"
    );
}

#[test]
fn sbb_rol_loop__loopne_decrements_cx_and_branches() {
    let func = json!({
        "start": 0x0000,
        "instructions": [
            {"address": 0x0000, "mnemonic": "loopne", "op_str": "0x0010", "bytes": "E0FE"},
            {"address": 0x0002, "mnemonic": "ret", "op_str": "", "bytes": "C3"},
        ],
    });
    let src = render_rs(&func, &[], "");
    assert!(src.contains("r.set_cx(r.cx().wrapping_sub(1));"), "{src}");
    assert!(src.contains("if r.cx() != 0 && r.ZF() == 0 {"), "{src}");
    // the taken branch yields the target pc from inside the guard
    assert!(
        src.contains("if r.cx() != 0 && r.ZF() == 0 {\n        return 0x0010;\n    }"),
        "{src}"
    );
}

// ===========================================================================
// ===========================================================================

#[test]
fn scasb__compares_al_with_memory_and_increments_di() {
    let func = json!({
        "start": 0x0000,
        "instructions": [
            {"address": 0x0000, "mnemonic": "scasb",
             "op_str": "al, byte ptr es:[di]", "bytes": "AE"},
            {"address": 0x0001, "mnemonic": "ret", "op_str": "", "bytes": "C3"},
        ],
    });
    let src = render_rs(&func, &[], "");
    let cf_index = src.find("r.set_CF((left_val < right_val) as u8);").unwrap();
    let tmp_index = src
        .find("let tmp = left_val.wrapping_sub(right_val);")
        .unwrap();
    assert!(cf_index < tmp_index, "{src}");
    assert!(
        src.contains("let left_val: u32 = (r.al()) as u32;"),
        "{src}"
    );
    assert!(
        src.contains("let right_val: u32 = r.memb(r.es(), r.di()) as u32;"),
        "{src}"
    );
    assert!(
        src.contains("let delta: i32 = if r.DF() != 0 { -1 } else { 1 };"),
        "{src}"
    );
    assert!(
        src.contains("r.set_di(((r.di() as i32 + delta) & 0xFFFF) as u16);"),
        "{src}"
    );
}

// ===========================================================================
// ===========================================================================

#[test]
fn shr__sets_cf_of() {
    let func = json!({
        "start": 0x0000,
        "instructions": [
            {"address": 0x0000, "mnemonic": "shr", "op_str": "al, 1", "bytes": "D0E8"},
            {"address": 0x0002, "mnemonic": "shr", "op_str": "ax, 1", "bytes": "D1E8"},
            {"address": 0x0004, "mnemonic": "ret", "op_str": "", "bytes": "C3"},
        ],
    });
    let src = render_rs(&func, &[], "");
    assert_eq!(
        src.matches("let mut count: u32 = (0x1) as u32 & 0x1F;")
            .count(),
        2,
        "{src}"
    );
    assert_eq!(src.matches("let orig_count = count;").count(), 2, "{src}");
    assert_eq!(
        src.matches("while count != 0 { count -= 1;").count(),
        2,
        "{src}"
    );
    assert_eq!(
        src.matches("r.set_CF((tmp & 1) as u8);").count(),
        2,
        "{src}"
    );
    assert!(
        src.contains("let orig_sign = ((tmp >> 7) & 1) as u8;"),
        "{src}"
    );
    assert!(
        src.contains("let orig_sign = ((tmp >> 15) & 1) as u8;"),
        "{src}"
    );
    assert_eq!(
        src.matches("r.set_OF(if orig_count == 1 { orig_sign } else { 0 });")
            .count(),
        2,
        "{src}"
    );
}

#[test]
fn shr__memory_writes_via_helper() {
    let func = json!({
        "start": 0x0000,
        "instructions": [
            {"address": 0x0000, "mnemonic": "shr",
             "op_str": "byte ptr ds:[0x1234], 1", "bytes": "D02E3412"},
            {"address": 0x0004, "mnemonic": "ret", "op_str": "", "bytes": "C3"},
        ],
    });
    let src = render_rs(&func, &[], "");
    assert!(src.contains("r.memb_write(r.ds(), 0x1234, tmp);"), "{src}");
    assert!(
        src.contains("r.set_ZF(((r.memb(r.ds(), 0x1234)) == 0) as u8);"),
        "{src}"
    );
}

// ===========================================================================
// the original calls CCodeRenderer().handle_arithmetic(insn, set()) directly. That
// method is private (in both ports), so we exercise the same instruction via a
// full render; the SF write is emitted unconditionally, so the same substring
// assertions hold. Logic ops write flags off a local `tmp`; add/sub read the
// destination register back.
// ===========================================================================

fn sign_flag_src(mnemonic: &str, op_str: &str, bytes: &str) -> String {
    let func = json!({
        "start": 0x0000,
        "instructions": [
            {"address": 0x0000, "mnemonic": mnemonic, "op_str": op_str, "bytes": bytes},
            {"address": 0x0002, "mnemonic": "ret", "op_str": "", "bytes": "C3"},
        ],
    });
    render_rs(&func, &[], "")
}

#[test]
fn sign_flag__add_sets_sf() {
    let src = sign_flag_src("add", "al, 1", "0401");
    assert!(
        src.contains("r.set_SF((((r.al()) >> 7) & 1) as u8);"),
        "{src}"
    );
}

#[test]
fn sign_flag__sub_sets_sf() {
    let src = sign_flag_src("sub", "ax, bx", "29D8");
    assert!(
        src.contains("r.set_SF((((r.ax()) >> 15) & 1) as u8);"),
        "{src}"
    );
}

#[test]
fn sign_flag__or_sets_sf() {
    let src = sign_flag_src("or", "al, al", "08C0");
    assert!(src.contains("r.set_SF(((tmp >> 7) & 1) as u8);"), "{src}");
}

#[test]
fn sign_flag__and_sets_sf() {
    let src = sign_flag_src("and", "ax, bx", "21D8");
    assert!(src.contains("r.set_SF(((tmp >> 15) & 1) as u8);"), "{src}");
}

#[test]
fn sign_flag__xor_sets_sf() {
    let src = sign_flag_src("xor", "al, ah", "30E0");
    assert!(src.contains("r.set_SF(((tmp >> 7) & 1) as u8);"), "{src}");
}

// ===========================================================================
// ===========================================================================

#[test]
fn single_header_instruction__loop() {
    let func = json!({
        "start": 0x0000,
        "instructions": [
            {"address": 0x0000, "mnemonic": "mov", "op_str": "byte ptr [0xff1a], 0",
             "bytes": "c6061aff00"},
            {"address": 0x0005, "mnemonic": "jmp", "op_str": "0000",
             "target": 0x0000, "bytes": "e9faff"},
        ],
    });
    let src = render_rs(&func, &[], "");
    // the store executes inside the loop body (the 0x0000 block) and the
    // header-only jump forms the pc back edge
    let body = blk(&src, 0x0000);
    assert!(
        body.contains("r.memb_write(r.ds(), 0xFF1A, 0x0);"),
        "{body}"
    );
    assert!(body.contains("return 0x0000;"), "{body}");
}

// ===========================================================================
// ===========================================================================

#[test]
fn ss_interrupt_shadow__mov_ss_sets_interrupt_shadow() {
    let func = json!({
        "start": 0,
        "instructions": [
            {"address": 0, "mnemonic": "mov", "op_str": "ss, ax", "bytes": "8ED0"},
            {"address": 2, "mnemonic": "ret", "op_str": "", "bytes": "C3"},
        ],
    });
    let src = render_rs(&func, &[], "");
    assert!(src.contains("r.set_ss(r.ax());"), "{src}");
    assert_eq!(src.matches("set_interrupt_shadow(1);").count(), 1, "{src}");
}

#[test]
fn ss_interrupt_shadow__pop_ss_sets_interrupt_shadow() {
    let func = json!({
        "start": 0,
        "instructions": [
            {"address": 0, "mnemonic": "pop", "op_str": "ss", "bytes": "17"},
            {"address": 1, "mnemonic": "ret", "op_str": "", "bytes": "C3"},
        ],
    });
    let src = render_rs(&func, &[], "");
    assert!(src.contains("r.set_ss(r.memw(r.ss(), r.sp()));"), "{src}");
    assert!(
        src.contains("r.set_sp((r.sp().wrapping_add(2)) & 0xFFFF);"),
        "{src}"
    );
    assert_eq!(src.matches("set_interrupt_shadow(1);").count(), 1, "{src}");
}

// ===========================================================================
// ===========================================================================

#[test]
fn stack_indexed_bp__keeps_bp_expression() {
    let func = json!({
        "start": 0x0000,
        "instructions": [
            {"address": 0x0000, "mnemonic": "mov",
             "op_str": "ax, word ptr [bp + si - 4]", "bytes": "8B42FC"},
            {"address": 0x0003, "mnemonic": "ret", "op_str": "", "bytes": "C3"},
        ],
    });
    let src = render_rs(&func, &[], "");
    // bp-based defaults to SS; the bp+si expression survives (no var_4 rewrite)
    assert!(
        src.contains(
            "r.set_ax(r.memw(r.ss(), (((r.bp() as u32).wrapping_add((r.si() as u32)).wrapping_sub(0x4u32)) & 0xFFFF) as u16));"
        ),
        "{src}"
    );
    assert!(!src.contains("var_4"), "{src}");
}

// ===========================================================================
// ===========================================================================

#[test]
fn stick_loop__stick_style_loop() {
    let func = json!({
        "start": 0x0000,
        "instructions": [
            {"address": 0x0000, "mnemonic": "lodsb", "op_str": "", "bytes": "AC"},
            {"address": 0x0001, "mnemonic": "dec", "op_str": "dx", "bytes": "4A"},
            {"address": 0x0002, "mnemonic": "cmp", "op_str": "al, 0xff", "bytes": "3CFF"},
            {"address": 0x0004, "mnemonic": "jne", "op_str": "0x9",
             "target": 0x9, "bytes": "7503"},
            {"address": 0x0006, "mnemonic": "ret", "op_str": "", "bytes": "C3"},
            {"address": 0x0007, "mnemonic": "inc", "op_str": "si", "bytes": "46"},
            {"address": 0x0008, "mnemonic": "dec", "op_str": "dx", "bytes": "4A"},
            {"address": 0x0009, "mnemonic": "jmp", "op_str": "0x0",
             "target": 0x0, "bytes": "E9F5FF"},
        ],
    });
    let src = render_rs(&func, &[], "");
    assert!(src.contains("if r.ZF() == 0 {"), "{src}");
    assert!(
        src.contains("let delta: i32 = if r.DF() != 0 { -1 } else { 1 };"),
        "{src}"
    );
    // exactly one lodsb step and one inc-si step
    assert_eq!(
        src.matches("r.set_si(((r.si() as i32 + delta) & 0xFFFF) as u16);")
            .count(),
        1,
        "{src}"
    );
    assert_eq!(
        src.matches("r.set_si((old.wrapping_add(1) & 0xFFFF) as u16);")
            .count(),
        1,
        "{src}"
    );
    // the loop closes through the pc back edge to the header
    assert!(src.contains("return 0x0000;"), "{src}");
}

// ===========================================================================
// ===========================================================================

#[test]
fn sub_cf__sets_cf_before_subtraction() {
    let func = json!({
        "start": 0x0000,
        "instructions": [
            {"address": 0x0000, "mnemonic": "sub", "op_str": "ax, bx", "bytes": "29D8"},
            {"address": 0x0002, "mnemonic": "ret", "op_str": "", "bytes": "C3"},
        ],
    });
    let src = render_rs(&func, &[], "");
    let cf_index = src.find("r.set_CF((old < src) as u8);").unwrap();
    let tmp_index = src.find("let tmp: u32 = old.wrapping_sub(src);").unwrap();
    assert!(cf_index < tmp_index, "{src}");
}

// ===========================================================================
// The C backend decoded the jump table out of code_bytes at translate time and
// structured a C `switch` (case labels, sibling func_XXXX() calls). The Rust
// backend intentionally does no translate-time table decode: the register prep
// runs as x86 and the indirect jmp dispatches through jump_table_ at runtime.
// The structuring assertions are gone; what must survive is the semantic core.
// ===========================================================================

#[test]
fn switch__pattern_emits_switch_statement() {
    let func = json!({
        "start": 0x0000,
        "instructions": [
            {"address": 0x0000, "mnemonic": "mov", "op_str": "bl, byte ptr cs:[0x8E7]",
             "bytes": "8A1E8708"},
            {"address": 0x0004, "mnemonic": "mov", "op_str": "bh, 0", "bytes": "B700"},
            {"address": 0x0006, "mnemonic": "add", "op_str": "bx, bx", "bytes": "01DB"},
            {"address": 0x0008, "mnemonic": "jmp", "op_str": "word ptr cs:[bx + 0x100]",
             "bytes": "2EFFA70001"},
        ],
    });
    let src = render_rs(&func, &[], "");
    // the selector prep is preserved in program order
    assert!(src.contains("r.set_bl(r.memb(r.cs(), 0x8E7));"), "{src}");
    assert!(src.contains("r.set_bh(0x0);"), "{src}");
    assert!(
        src.contains("let tmp: u32 = old.wrapping_add(src);"),
        "{src}"
    ); // add bx,bx
       // the indirect jmp routes through the runtime jump table at cs:[bx+0x100]
    assert!(src.contains("r.jump_table_("), "{src}");
    assert!(
        src.contains("r.memw(r.cs(), (((r.bx() as u32).wrapping_add(0x100u32)) & 0xFFFF) as u16)"),
        "{src}"
    );
}

#[test]
fn switch__structures_case_bodies() {
    let func = json!({
        "start": 0x0000,
        "instructions": [
            {"address": 0x0000, "mnemonic": "jmp", "op_str": "0010",
             "target": 0x0010, "bytes": "E90000"},
            {"address": 0x0003, "mnemonic": "jmp", "op_str": "0016",
             "target": 0x0016, "bytes": "E90000"},
            {"address": 0x0006, "mnemonic": "mov", "op_str": "bl, byte ptr cs:[0x8E7]",
             "bytes": "8A1E8708"},
            {"address": 0x000A, "mnemonic": "mov", "op_str": "bh, 0", "bytes": "B700"},
            {"address": 0x000C, "mnemonic": "add", "op_str": "bx, bx", "bytes": "01DB"},
            {"address": 0x000E, "mnemonic": "jmp", "op_str": "word ptr cs:[bx + 0x100]",
             "bytes": "FF27"},
            {"address": 0x0010, "mnemonic": "mov", "op_str": "ax, 1", "bytes": "B80100"},
            {"address": 0x0013, "mnemonic": "jmp", "op_str": "0020",
             "target": 0x0020, "bytes": "E90000"},
            {"address": 0x0016, "mnemonic": "mov", "op_str": "ax, 2", "bytes": "B80200"},
            {"address": 0x0019, "mnemonic": "jmp", "op_str": "0020",
             "target": 0x0020, "bytes": "E90000"},
            {"address": 0x0020, "mnemonic": "ret", "op_str": "", "bytes": "C3"},
        ],
    });
    let src = render_rs(&func, &[], "");
    // the table dispatch is a runtime jump_table_ call ...
    assert!(src.contains("r.jump_table_("), "{src}");
    // ... and the case bodies stay dispatchable as their own blocks
    let case0 = blk(&src, 0x0010);
    assert!(case0.contains("r.set_ax(0x1);"), "{case0}");
    let case1 = blk(&src, 0x0016);
    assert!(case1.contains("r.set_ax(0x2);"), "{case1}");
}

#[test]
fn switch__pattern_detects_xor_zeroing_bh() {
    let func = json!({
        "start": 0x0000,
        "instructions": [
            {"address": 0x0000, "mnemonic": "mov", "op_str": "bl, al", "bytes": "88C3"},
            {"address": 0x0002, "mnemonic": "xor", "op_str": "bh, bh", "bytes": "30FF"},
            {"address": 0x0004, "mnemonic": "add", "op_str": "bx, bx", "bytes": "01DB"},
            {"address": 0x0006, "mnemonic": "jmp", "op_str": "word ptr es:[bx + 0x100]",
             "bytes": "26FFA70001"},
        ],
    });
    let src = render_rs(&func, &[], "");
    assert!(src.contains("r.set_bl(r.al());"), "{src}");
    // xor bh,bh zeroes the selector high byte
    assert!(src.contains("r.set_bh(0);"), "{src}");
    assert!(src.contains("r.jump_table_("), "{src}");
    assert!(
        src.contains("r.memw(r.es(), (((r.bx() as u32).wrapping_add(0x100u32)) & 0xFFFF) as u16)"),
        "{src}"
    );
}

#[test]
fn switch__pattern_uses_current_function_addresses_for_cases() {
    let func = json!({
        "start": 0x0000,
        "instructions": [
            {"address": 0x0000, "mnemonic": "mov", "op_str": "bl, al", "bytes": "88C3"},
            {"address": 0x0002, "mnemonic": "xor", "op_str": "bh, bh", "bytes": "30FF"},
            {"address": 0x0004, "mnemonic": "add", "op_str": "bx, bx", "bytes": "01DB"},
            {"address": 0x0006, "mnemonic": "jmp", "op_str": "word ptr cs:[bx + 0x0100]",
             "bytes": "FFA70001"},
            {"address": 0x000A, "mnemonic": "ret", "op_str": "", "bytes": "C3"},
            {"address": 0x000B, "mnemonic": "ret", "op_str": "", "bytes": "C3"},
        ],
    });
    let src = render_rs(&func, &[], "");
    // no translate-time case decode: the transfer goes through jump_table_,
    // and the in-function ret targets keep their own pop-return epilogues
    assert!(src.contains("r.jump_table_("), "{src}");
    assert_eq!(
        src.matches("let popped_ip = r.memw(r.ss(), r.sp());")
            .count(),
        2,
        "{src}"
    );
}

// ===========================================================================
// ===========================================================================

#[test]
fn terminates__on_noreturn_funcs() {
    // The original asserted CCodeRenderer()._terminates("<fn>();") over the
    // private _NORETURN_FUNCS list. The observable Rust-backend equivalent:
    // after a noreturn runtime call (dos_exit here), the block yields the
    // done sentinel (-1) and the unreachable in-block tail (the ret) is
    // dropped.
    let func = json!({
        "start": 0x0000,
        "instructions": [
            {"address": 0x0000, "mnemonic": "int", "op_str": "0x20", "bytes": "CD20"},
            {"address": 0x0002, "mnemonic": "ret", "op_str": "", "bytes": "C3"},
        ],
    });
    let src = render_rs(&func, &[], "");
    assert!(src.contains("r.dos_exit();\n    return -1;"), "{src}");
    assert!(
        !src.contains("let popped_ip = r.memw(r.ss(), r.sp());"),
        "{src}"
    );
}
