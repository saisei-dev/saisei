//! Smoke tests for the chunk emitter (renamed from ir_to_c_smoke.rs).
//!
//! PORT DISPOSITIONS (C backend deleted):
//!   ported:    dos_get_version_rendered_before_if (dos_get_version() collapsed
//!              to dos_api() — the Rust backend never specializes int 21h),
//!              if_else_statement_is_structured (structure → pc-state-machine
//!              branch arms; the if/else SHAPE assertion is C-only, replaced by
//!              asserting both branch targets and the ZF condition)
//!   unchanged: parse_imm_negative (front-half)
mod common;
use common::*;
use serde_json::json;

#[test]
fn dos_get_version_rendered_before_if() {
    let func = json!({
        "start": 0x0000,
        "instructions": [
            {"address":0x0000,"mnemonic":"mov","op_str":"ah, 0x30","bytes":"B430"},
            {"address":0x0002,"mnemonic":"int","op_str":"21","bytes":"CD21"},
            {"address":0x0004,"mnemonic":"cmp","op_str":"al, 2","bytes":"3C02"},
            {"address":0x0006,"mnemonic":"jb","op_str":"0010","bytes":"7208"},
            {"address":0x0008,"mnemonic":"mov","op_str":"ax, bx","bytes":"89D8"},
            {"address":0x000A,"mnemonic":"ret","op_str":"","bytes":"C3"},
        ],
    });
    let src = render_rs(&func, &[], "");
    assert!(src.contains("r.set_ah(0x30);"), "{src}");
    assert!(src.contains("r.dos_api();"), "{src}");
    assert!(src.contains("if r.CF() == 1"), "{src}");
    // The DOS call executes before the branch on its CF result.
    assert!(src.find("r.dos_api();").unwrap() < src.find("if r.CF() == 1").unwrap());
}

#[test]
fn if_else_statement_is_structured() {
    let func = json!({
        "start": 0x0000,
        "instructions": [
            {"address":0x0000,"mnemonic":"jz","op_str":"0x8","bytes":"7406"},
            {"address":0x0002,"mnemonic":"call","op_str":"0x1000","bytes":"E80000","target":0x1000},
            {"address":0x0005,"mnemonic":"jmp","op_str":"0xB","bytes":"EB04","target":0xB},
            {"address":0x0008,"mnemonic":"call","op_str":"0x2000","bytes":"E80000","target":0x2000},
            {"address":0x000B,"mnemonic":"ret","op_str":"","bytes":"C3"},
        ],
    });
    let src = render_rs(&func, &[0x0000, 0x1000, 0x2000], "g_");
    // Both jcc arms exist: the branch block's if/else yields each successor pc…
    let b0 = blk(&src, 0x0000);
    assert!(b0.contains("if r.ZF() == 1"), "{src}");
    assert!(b0.contains("return 0x0008;"), "{src}");
    assert!(b0.contains("return 0x0002;"), "{src}");
    // …and each branch's block reaches its own call target (intra-chunk call
    // renders as `return 0x…;` — the next pc — in that per-block fn).
    assert!(blk(&src, 0x0002).contains("return 0x1000;"), "{src}");
    assert!(blk(&src, 0x0008).contains("return 0x2000;"), "{src}");
}

#[test]
fn parse_imm_negative() {
    use saisei_jitc::{disassemble, translate};
    for tok in ["-42", "-0x2A", "-2Ah"] {
        // ir_to_c._parse_imm and cfg._parse_imm (== disassemble's) both handle
        // these negative immediates identically.
        assert_eq!(translate::parse_imm(tok), Some(-42), "ir_to_c {tok}");
        assert_eq!(disassemble::parse_imm(tok), Some(-42), "disasm {tok}");
    }
}

/// FS/GS are 386 registers and this CPU has neither — the prelude's `word_reg!`
/// block declares cs/ds/es/ss and stops. The emitter used to pass capstone's
/// segment straight through anyway, so `fs:[si]` came out as `memw(fs, si)`:
/// a call to an `fs()` accessor that does not exist, which failed the WHOLE
/// chunk at rustc with "cannot find function `fs`". The bytes come from the
/// speculative decoder reading data as code, so nothing was ever *meant* to run
/// them — but a chunk that will not compile is a chunk silently lost.
///
/// So they take the route every other inexpressible construct takes: a run-time
/// `jit_unsupported_instruction` trap, paid only if control actually arrives.
#[test]
fn an_fs_segment_override_traps_instead_of_calling_a_register_that_does_not_exist() {
    let func = json!({
        "start": 0x0000,
        "instructions": [
            {"address":0x0000,"mnemonic":"mov","op_str":"ax, word ptr fs:[si]","bytes":"648B04",
             "detail": {"mem_refs": [{"segment":"FS","disp":0,"access":"read"}]}},
            {"address":0x0003,"mnemonic":"ret","op_str":"","bytes":"C3"},
        ],
    });
    let src = render_rs(&func, &[], "");
    assert!(src.contains("jit_unsupported_instruction"), "{src}");
    assert!(
        !src.contains("fs()"),
        "emitted a call to a nonexistent register: {src}"
    );
}

#[test]
fn the_gs_register_itself_traps_too() {
    let func = json!({
        "start": 0x0000,
        "instructions": [
            {"address":0x0000,"mnemonic":"push","op_str":"gs","bytes":"0FA8"},
            {"address":0x0002,"mnemonic":"ret","op_str":"","bytes":"C3"},
        ],
    });
    let src = render_rs(&func, &[], "");
    assert!(src.contains("jit_unsupported_instruction"), "{src}");
    assert!(
        !src.contains("gs()"),
        "emitted a call to a nonexistent register: {src}"
    );
}

/// The control: the segments this CPU *does* have must still lower normally, or
/// the guard above would be quietly throwing real code away.
#[test]
fn an_es_segment_override_still_lowers() {
    let func = json!({
        "start": 0x0000,
        "instructions": [
            {"address":0x0000,"mnemonic":"mov","op_str":"ax, word ptr es:[si]","bytes":"268B04",
             "detail": {"mem_refs": [{"segment":"ES","disp":0,"access":"read"}]}},
            {"address":0x0003,"mnemonic":"ret","op_str":"","bytes":"C3"},
        ],
    });
    let src = render_rs(&func, &[], "");
    assert!(src.contains("r.memw(r.es(), r.si())"), "{src}");
    assert!(!src.contains("jit_unsupported_instruction"), "{src}");
}
