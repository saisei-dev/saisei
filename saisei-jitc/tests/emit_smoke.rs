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
