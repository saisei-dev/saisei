//! Ported from tests/test_*.py — rcl, rcr, relocation, repe, repne, ret, ror,
//! sar, sbb_rol_loop, scasb, shr, sign_flag, single_header_instruction,
//! ss_interrupt_shadow, stack_indexed_bp, stick_loop, sub_cf, switch, terminates.
#![allow(non_snake_case)]
mod common;
use common::*;
use serde_json::json;

// ---------------------------------------------------------------------------
// local helpers
// ---------------------------------------------------------------------------

/// Mirrors test_ir_to_c_relocation._render: base renderer with a single reloc
/// offset (segment word of the instruction) and load_segment 0x1010 (default).
fn reloc_render(insn: serde_json::Value, reloc_seg_word_off: i64) -> String {
    let start = as_i(&insn, "address");
    let mut r = renderer("");
    // relocations=[{segment: off>>4, offset: off&0xF}] -> (seg<<4)+off == off.
    r.reloc_offsets = known(&[reloc_seg_word_off]);
    r.load_segment = 0x1010;
    let func = json!({ "start": start, "instructions": [insn] });
    r.render_function_c(&func, &known(&[])).join("\n")
}

// ===========================================================================
// ===========================================================================

#[test]
fn rcl__rotates_through_cf() {
    let func = json!({
        "start": 0x0000,
        "instructions": [
            {"address": 0x0000, "mnemonic": "rcl", "op_str": "ax, 1", "bytes": ""},
            {"address": 0x0002, "mnemonic": "ret", "op_str": "", "bytes": ""},
        ],
    });
    let src = render_c(&func, &[], "");
    assert!(src.contains("unsigned count = (1 & 0x1F) % 17;"), "{src}");
    assert!(src.contains("unsigned orig_count = count;"), "{src}");
    assert!(src.contains("while (count--) {"), "{src}");
    assert!(src.contains("unsigned new_cf = (ax >> 15) & 1;"), "{src}");
    assert!(src.contains("ax = ((ax << 1) | CF) & 0xFFFF;"), "{src}");
    assert!(src.contains("CF = new_cf;"), "{src}");
    assert!(src.contains("if (orig_count == 1) {"), "{src}");
    assert!(src.contains("OF = ((ax >> 15) & 1) ^ CF;"), "{src}");
    assert!(!src.contains("OF = 0;"), "{src}");
    assert!(!src.contains("ZF = ax == 0;"), "{src}");
    assert!(!src.contains("PF = parity8((uint8_t)ax);"), "{src}");
    assert!(!src.contains("SF = (ax >> 15) & 1;"), "{src}");
}

// ===========================================================================
// ===========================================================================

#[test]
fn rcr__register_translate() {
    let func = json!({
        "start": 0x0000,
        "instructions": [
            {"address": 0x0000, "mnemonic": "rcr", "op_str": "ax, 1", "bytes": ""},
            {"address": 0x0002, "mnemonic": "ret", "op_str": "", "bytes": ""},
        ],
    });
    let src = render_c(&func, &[], "");
    assert!(src.contains("unsigned count = (1 & 0x1F) % 17;"), "{src}");
    assert!(src.contains("unsigned orig_count = count;"), "{src}");
    assert!(src.contains("unsigned new_cf = ax & 1;"), "{src}");
    assert!(
        src.contains("ax = ((ax >> 1) | (CF << 15)) & 0xFFFF;"),
        "{src}"
    );
    assert!(!src.contains("ZF = ax == 0;"), "{src}");
    assert!(!src.contains("SF = (ax >> 15) & 1;"), "{src}");
    assert!(!src.contains("PF = parity8((uint8_t)ax);"), "{src}");
    assert!(
        src.contains("OF = ((ax >> 15) & 1) ^ ((ax >> 14) & 1);"),
        "{src}"
    );
    assert!(!src.contains("OF = 0;"), "{src}");
    assert!(!src.contains("// TODO ASM: rcr"), "{src}");
}

// ===========================================================================
// ===========================================================================

#[test]
fn relocation__lcall_immediate_segment_is_relocated() {
    // lcall 0:0 (9A 00 00 00 00) at 0x1518B; seg word at 0x1518E is relocated.
    let src = reloc_render(
        json!({"address": 0x1518B, "mnemonic": "lcall", "op_str": "0, 0",
               "bytes": "9a00000000"}),
        0x1518E,
    );
    assert!(
        src.contains(
            "lcall_table((uint16_t)(0x15190U + 0x10100U - ((uint32_t)cs << 4)), \
             0x1010, 0x0000);"
        ),
        "{src}"
    );
}

#[test]
fn relocation__lcall_immediate_segment_not_relocated_unchanged() {
    // No relocation on the seg word -> segment stays as decoded.
    let mut r = renderer("");
    r.reloc_offsets = known(&[]);
    r.load_segment = 0x1010;
    let func = json!({"start": 0x100, "instructions": [
        {"address": 0x100, "mnemonic": "lcall", "op_str": "0x6c0, 0x4a7",
         "bytes": "9aa704c006"}]});
    let src = r.render_function_c(&func, &known(&[])).join("\n");
    assert!(
        src.contains(
            "lcall_table((uint16_t)(0x00105U + 0x10100U - ((uint32_t)cs << 4)), \
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
    assert!(src.contains("long_jump(0x1010, 0x0100);"), "{src}");
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
    let src = render_c(&func, &[], "");
    assert!(src.contains("int delta = DF ? -1 : 1;"), "{src}");
    assert!(src.contains("lb = memb(ds, si);"), "{src}");
    assert!(src.contains("rb = memb(es, di);"), "{src}");
    assert!(src.contains("if (lb != rb) break;"), "{src}"); // ZF->0 ends repe
    assert!(src.contains("cx = (count - i) & 0xFFFF;"), "{src}"); // remaining count
    assert!(src.contains("ZF = res == 0;"), "{src}");
    assert!(src.contains("CF = lb < rb;"), "{src}");
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
        let body = render_c(&func, &[], "");
        assert!(!body.to_lowercase().contains("abort"), "{body}");
    }
}

#[test]
#[ignore = "port-divergence: Rust emit_unsupported_abort calls std::process::exit(2)"]
fn repe__other_repe_instructions_abort_translation() {
    // NOTE(port-divergence): the original raises a catchable UnsupportedInstructionError
    // for `repe scasw` (asserted via.raises with "scasw" in the message).
    // The Rust port's `emit_unsupported_abort` instead calls std::process::exit(2),
    // which terminates the whole test process and cannot be caught inside a normal
    // `#[test]` (it is not a panic, so catch_unwind does not help). The abort
    // behavior itself is faithful — it just can't be asserted from within cargo
    // test — so this is left #[ignore]d rather than deleted/weakened.
    let func = json!({
        "start": 0x0000,
        "instructions": [
            {"address": 0x0000, "mnemonic": "repe scasw",
             "op_str": "word ptr es:[di]", "bytes": "F3AF"},
            {"address": 0x0002, "mnemonic": "ret", "op_str": "", "bytes": "C3"},
        ],
    });
    // Would call std::process::exit(2) — do not run.
    let _ = render_c(&func, &[], "");
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
    let src = render_c(&func, &[], "");
    assert!(src.contains("int delta = DF ? -1 : 1;"), "{src}");
    assert!(src.contains("uint8_t last_byte = 0;"), "{src}");
    assert!(
        src.contains(
            "uint16_t index = scanMemoryForAl((const uint8_t *)seg_off(es, di), \
             al, count, delta, &last_byte);"
        ),
        "{src}"
    );
    assert!(
        src.contains("uint16_t advance = (index < count) ? index + 1 : index;"),
        "{src}"
    );
    assert!(src.contains("if (advance > 0) {"), "{src}");
    assert!(src.contains("CF = left_val < right_val;"), "{src}");
    assert!(src.contains("SF = (result >> 7) & 1;"), "{src}");
    assert!(
        src.contains("OF = ((left_val ^ right_val) & (left_val ^ result) & 0x80) != 0;"),
        "{src}"
    );
    assert!(src.contains("ZF = result == 0;"), "{src}");
    assert!(src.contains("cx = (count - advance) & 0xFFFF;"), "{src}");
    assert!(
        src.contains("di = (di + advance * delta) & 0xFFFF;"),
        "{src}"
    );
    assert!(!src.contains("// TODO ASM: repne scasb"), "{src}");
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
    let src = render_c(&func, &[], "");
    assert!(src.contains("return;"), "{src}");
    assert!(!src.contains("retf();"), "{src}");
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
    let src = render_c(&func, &[], "");
    assert!(src.contains("retf();"), "{src}");
    assert!(src.contains("retf();\n    return;"), "{src}");
}

#[test]
fn ret__far_only() {
    let func = json!({
        "start": 0x0000,
        "instructions": [
            {"address": 0x0000, "mnemonic": "retf", "op_str": "", "bytes": "CB"},
        ],
    });
    let src = render_c(&func, &[], "");
    assert!(src.contains("retf();"), "{src}");
    assert!(src.trim_end().ends_with("retf();\n}"), "{src}");
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
    let src = render_c(&func, &[], "");
    assert!(src.contains("sp = (sp + 4) & 0xFFFF;"), "{src}");
    assert!(src.contains("return;"), "{src}");
}

#[test]
fn ret__far_immediate_uses_retf_pop() {
    let func = json!({
        "start": 0x0000,
        "instructions": [
            {"address": 0x0000, "mnemonic": "retf", "op_str": "4", "bytes": "CA0400"},
        ],
    });
    let src = render_c(&func, &[], "");
    assert!(src.contains("retf_pop(4);"), "{src}");
    assert!(!src.contains("sp = (sp + 4) & 0xFFFF;"), "{src}");
}

#[test]
fn ret__far_decimal_immediate_preserves_decimal_value() {
    let func = json!({
        "start": 0x0000,
        "instructions": [
            {"address": 0x0000, "mnemonic": "retf", "op_str": "12", "bytes": "CA0C00"},
        ],
    });
    let src = render_c(&func, &[], "");
    assert!(src.contains("retf_pop(12);"), "{src}");
    assert!(!src.contains("retf_pop(18);"), "{src}");
}

#[test]
fn ret__far_in_if_else() {
    let func = json!({
        "start": 0x0000,
        "instructions": [
            {"address": 0x0000, "mnemonic": "cmp", "op_str": "ax, 0", "bytes": "3D0000"},
            {"address": 0x0003, "mnemonic": "jnz", "op_str": "0x0008", "bytes": "7503"},
            {"address": 0x0005, "mnemonic": "mov", "op_str": "ax, ax", "bytes": "89C0"},
            {"address": 0x0007, "mnemonic": "ret", "op_str": "", "bytes": "C3"},
            {"address": 0x0008, "mnemonic": "retf", "op_str": "", "bytes": "CB"},
        ],
    });
    let src = render_c(&func, &[], "");
    assert!(src.contains("retf();"), "{src}");
}

// ===========================================================================
// ===========================================================================

#[test]
fn ror__translates_to_rotate_right() {
    let func = json!({
        "start": 0x0000,
        "instructions": [
            {"address": 0x0000, "mnemonic": "ror", "op_str": "al, 1", "bytes": ""},
            {"address": 0x0002, "mnemonic": "ret", "op_str": "", "bytes": ""},
        ],
    });
    let src = render_c(&func, &[], "");
    assert!(src.contains("unsigned count = 1 & 7;"), "{src}");
    assert!(src.contains("if (count) {"), "{src}");
    assert!(
        src.contains("al = (al >> count) | (al << (8 - count));"),
        "{src}"
    );
    assert!(src.contains("CF = al >> 7;"), "{src}");
    assert!(
        src.contains("OF = ((al >> 7) & 1) ^ ((al >> 6) & 1);"),
        "{src}"
    );
    assert!(!src.contains("ZF = al == 0;"), "{src}");
    assert!(!src.contains("PF = parity8((uint8_t)al);"), "{src}");
    assert!(!src.contains("// TODO ASM: ror"), "{src}");
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
    let src = render_c(&func, &[], "");
    assert_eq!(
        src.matches("unsigned count = 1 & 0x1F;").count(),
        2,
        "{src}"
    );
    assert_eq!(
        src.matches("unsigned orig_count = count;").count(),
        2,
        "{src}"
    );
    assert!(src.contains("int8_t tmp = (int8_t)al;"), "{src}");
    assert!(src.contains("int16_t tmp = (int16_t)ax;"), "{src}");
    assert_eq!(src.matches("CF = tmp & 1;").count(), 2, "{src}");
    assert!(src.contains("al = (uint8_t)tmp;"), "{src}");
    assert!(src.contains("ax = (uint16_t)tmp;"), "{src}");
    assert_eq!(src.matches("OF = 0;").count(), 2, "{src}");
}

// ===========================================================================
// ===========================================================================

#[test]
fn sbb_rol_loop__rol_translates_to_rotate_left() {
    let func = json!({
        "start": 0x0000,
        "instructions": [
            {"address": 0x0000, "mnemonic": "rol", "op_str": "al, 1", "bytes": ""},
            {"address": 0x0002, "mnemonic": "ret", "op_str": "", "bytes": ""},
        ],
    });
    let src = render_c(&func, &[], "");
    assert!(src.contains("unsigned count = 1 & 7;"), "{src}");
    assert!(src.contains("if (count) {"), "{src}");
    assert!(
        src.contains("al = (al << count) | (al >> (8 - count));"),
        "{src}"
    );
    assert!(src.contains("CF = al & 1;"), "{src}");
    assert!(src.contains("OF = ((al >> 7) & 1) ^ CF;"), "{src}");
    assert!(!src.contains("ZF = al == 0;"), "{src}");
    assert!(!src.contains("PF = parity8((uint8_t)al);"), "{src}");
    assert!(!src.contains("// TODO ASM: rol"), "{src}");
}

#[test]
fn sbb_rol_loop__sbb_subtracts_with_carry() {
    let func = json!({
        "start": 0x0000,
        "instructions": [
            {"address": 0x0000, "mnemonic": "sbb", "op_str": "al, 0x1", "bytes": ""},
            {"address": 0x0002, "mnemonic": "ret", "op_str": "", "bytes": ""},
        ],
    });
    let src = render_c(&func, &[], "");
    assert!(src.contains("uint32_t old = al;"), "{src}");
    assert!(src.contains("uint32_t src = 0x1 + CF;"), "{src}");
    assert!(src.contains("uint32_t tmp = old - src;"), "{src}");
    assert!(src.contains("CF = old < src;"), "{src}");
    assert!(src.contains("al = tmp & 0xFF;"), "{src}");
    assert!(
        src.contains("OF = ((old ^ src) & (old ^ tmp) & 0x80) != 0;"),
        "{src}"
    );
    assert!(!src.contains("// TODO ASM: sbb"), "{src}");
}

#[test]
fn sbb_rol_loop__loopne_decrements_cx_and_branches() {
    let func = json!({
        "start": 0x0000,
        "instructions": [
            {"address": 0x0000, "mnemonic": "loopne", "op_str": "0x0010", "bytes": ""},
            {"address": 0x0002, "mnemonic": "ret", "op_str": "", "bytes": ""},
        ],
    });
    let src = render_c(&func, &[], "");
    assert!(src.contains("cx--;"), "{src}");
    assert!(src.contains("pc = 0x0010;"), "{src}");
    assert!(src.contains("continue;"), "{src}");
    assert!(!src.contains("goto"), "{src}");
    assert!(!src.contains("// TODO ASM: loopne"), "{src}");
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
    let src = render_c(&func, &[], "");
    let cf_index = src.find("CF = left_val < right_val;").unwrap();
    let tmp_index = src.find("uint32_t tmp = left_val - right_val;").unwrap();
    assert!(cf_index < tmp_index, "{src}");
    assert!(src.contains("uint32_t left_val = al;"), "{src}");
    assert!(src.contains("uint32_t right_val = memb(es, di);"), "{src}");
    assert!(src.contains("int delta = DF ? -1 : 1;"), "{src}");
    assert!(src.contains("di = (di + delta) & 0xFFFF;"), "{src}");
    assert!(!src.contains("// TODO ASM: scasb"), "{src}");
}

// ===========================================================================
// ===========================================================================

#[test]
fn shr__sets_cf_of() {
    let func = json!({
        "start": 0x0000,
        "instructions": [
            {"address": 0x0000, "mnemonic": "shr", "op_str": "al, 1", "bytes": ""},
            {"address": 0x0002, "mnemonic": "shr", "op_str": "ax, 1", "bytes": ""},
            {"address": 0x0004, "mnemonic": "ret", "op_str": "", "bytes": ""},
        ],
    });
    let src = render_c(&func, &[], "");
    assert_eq!(
        src.matches("unsigned count = 1 & 0x1F;").count(),
        2,
        "{src}"
    );
    assert_eq!(
        src.matches("unsigned orig_count = count;").count(),
        2,
        "{src}"
    );
    assert_eq!(src.matches("while (count--) {").count(), 2, "{src}");
    assert_eq!(src.matches("CF = tmp & 1;").count(), 2, "{src}");
    assert!(
        src.contains("unsigned orig_sign = (tmp >> 7) & 1;"),
        "{src}"
    );
    assert!(
        src.contains("unsigned orig_sign = (tmp >> 15) & 1;"),
        "{src}"
    );
    assert_eq!(
        src.matches("OF = (orig_count == 1) ? orig_sign : 0;")
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
             "op_str": "byte ptr ds:[0x1234], 1", "bytes": ""},
            {"address": 0x0002, "mnemonic": "ret", "op_str": "", "bytes": ""},
        ],
    });
    let src = render_c(&func, &[], "");
    assert!(src.contains("memb_write(ds, 0x1234, tmp);"), "{src}");
    assert!(src.contains("ZF = memb(ds, 0x1234) == 0;"), "{src}");
}

// ===========================================================================
// ===========================================================================
// the original calls CCodeRenderer().handle_arithmetic(insn, set()) directly. That
// method is private in the Rust port, so we exercise the same instruction via
// the base renderer (render_function_c); the SF flag line is not pruned, so the
// same substring assertions hold.

fn sign_flag_src(mnemonic: &str, op_str: &str) -> String {
    let func = json!({
        "start": 0x0000,
        "instructions": [
            {"address": 0x0000, "mnemonic": mnemonic, "op_str": op_str, "bytes": ""},
            {"address": 0x0002, "mnemonic": "ret", "op_str": "", "bytes": ""},
        ],
    });
    render_c(&func, &[], "")
}

#[test]
fn sign_flag__add_sets_sf() {
    assert!(sign_flag_src("add", "al, 1").contains("SF = (al >> 7) & 1;"));
}

#[test]
fn sign_flag__sub_sets_sf() {
    assert!(sign_flag_src("sub", "ax, bx").contains("SF = (ax >> 15) & 1;"));
}

#[test]
fn sign_flag__or_sets_sf() {
    assert!(sign_flag_src("or", "al, al").contains("SF = (al >> 7) & 1;"));
}

#[test]
fn sign_flag__and_sets_sf() {
    assert!(sign_flag_src("and", "ax, bx").contains("SF = (ax >> 15) & 1;"));
}

#[test]
fn sign_flag__xor_sets_sf() {
    assert!(sign_flag_src("xor", "al, ah").contains("SF = (al >> 7) & 1;"));
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
            {"address": 0x0005, "mnemonic": "jmp", "op_str": "0000", "bytes": "e9faff"},
        ],
    });
    let lines = render_c_lines(&func, &[], "");
    let loop_idx = lines
        .iter()
        .position(|l| l.trim().starts_with("do"))
        .expect("a `do` loop line");
    let memb_idx = lines
        .iter()
        .position(|l| l.contains("memb_write"))
        .expect("a `memb_write` line");
    assert!(memb_idx > loop_idx, "{lines:?}");
    assert!(
        lines.iter().any(|l| l.contains("memb_write(ds, 0xff1a, 0")),
        "{lines:?}"
    );
}

// ===========================================================================
// ===========================================================================

#[test]
fn ss_interrupt_shadow__mov_ss_sets_interrupt_shadow() {
    let func = json!({
        "start": 0,
        "instructions": [
            {"address": 0, "mnemonic": "mov", "op_str": "ss, ax", "bytes": "8E D0"},
            {"address": 2, "mnemonic": "ret", "op_str": "", "bytes": "C3"},
        ],
    });
    let src = render_c(&func, &[], "");
    assert!(src.contains("ss = ax;"), "{src}");
    assert_eq!(src.matches("interrupt_shadow = 1;").count(), 1, "{src}");
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
    let src = render_c(&func, &[], "");
    assert!(src.contains("ss = memw(ss, sp);"), "{src}");
    assert_eq!(src.matches("interrupt_shadow = 1;").count(), 1, "{src}");
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
    let src = render_c(&func, &[], "");
    assert!(src.contains("memw(ss, (bp + si - 4) & 0xFFFF)"), "{src}");
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
            {"address": 0x0004, "mnemonic": "jne", "op_str": "0x9", "bytes": "7503"},
            {"address": 0x0006, "mnemonic": "ret", "op_str": "", "bytes": "C3"},
            {"address": 0x0007, "mnemonic": "inc", "op_str": "si", "bytes": "46"},
            {"address": 0x0008, "mnemonic": "dec", "op_str": "dx", "bytes": "4A"},
            {"address": 0x0009, "mnemonic": "jmp", "op_str": "0x0", "bytes": "E9F5FF"},
        ],
    });
    let src = render_c(&func, &[], "");
    assert!(src.contains("if (ZF != 0)"), "{src}");
    assert!(src.contains("int delta = DF ? -1 : 1;"), "{src}");
    assert_eq!(
        src.matches("si = (si + delta) & 0xFFFF;").count(),
        1,
        "{src}"
    );
    assert_eq!(src.matches("si = (si + 1) & 0xFFFF;").count(), 1, "{src}");
    assert!(src.contains("while (1)"), "{src}");
}

// ===========================================================================
// ===========================================================================

#[test]
fn sub_cf__sets_cf_before_subtraction() {
    let func = json!({
        "start": 0x0000,
        "instructions": [
            {"address": 0x0000, "mnemonic": "sub", "op_str": "ax, bx", "bytes": ""},
            {"address": 0x0002, "mnemonic": "ret", "op_str": "", "bytes": ""},
        ],
    });
    let src = render_c(&func, &[], "");
    let cf_index = src.find("CF = old < src;").unwrap();
    let tmp_index = src.find("uint32_t tmp = old - src;").unwrap();
    assert!(cf_index < tmp_index, "{src}");
    assert!(!src.contains("// TODO ASM: sub"), "{src}");
}

// ===========================================================================
// ===========================================================================

/// Base renderer with a code_bytes image: `pad_len` zero bytes then `tail`.
fn switch_renderer(pad_len: usize, tail: &[u8]) -> saisei_jitc::ir_to_c::Renderer {
    let mut r = renderer("");
    let mut bytes = vec![0u8; pad_len];
    bytes.extend_from_slice(tail);
    r.code_bytes = bytes;
    r
}

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
             "bytes": "FF27"},
        ],
    });
    let mut r = switch_renderer(0x100, &[0x00, 0x01, 0x00, 0x02]);
    let src = r
        .render_function_c(&func, &known(&[0x0000, 0x0100, 0x0200]))
        .join("\n");
    assert!(src.contains("switch (memb(cs, 0x8E7))"), "{src}");
    assert!(src.contains("case 0: func_0100();"), "{src}");
    assert!(src.contains("case 1: func_0200();"), "{src}");
}

#[test]
fn switch__structures_case_bodies() {
    let func = json!({
        "start": 0x0000,
        "instructions": [
            {"address": 0x0000, "mnemonic": "jmp", "op_str": "0010", "bytes": "E90000"},
            {"address": 0x0003, "mnemonic": "jmp", "op_str": "0016", "bytes": "E90000"},
            {"address": 0x0006, "mnemonic": "mov", "op_str": "bl, byte ptr cs:[0x8E7]",
             "bytes": "8A1E8708"},
            {"address": 0x000A, "mnemonic": "mov", "op_str": "bh, 0", "bytes": "B700"},
            {"address": 0x000C, "mnemonic": "add", "op_str": "bx, bx", "bytes": "01DB"},
            {"address": 0x000E, "mnemonic": "jmp", "op_str": "word ptr cs:[bx + 0x100]",
             "bytes": "FF27"},
            {"address": 0x0010, "mnemonic": "mov", "op_str": "ax, 1", "bytes": "B80100"},
            {"address": 0x0013, "mnemonic": "jmp", "op_str": "0020", "bytes": "E90000"},
            {"address": 0x0016, "mnemonic": "mov", "op_str": "ax, 2", "bytes": "B80200"},
            {"address": 0x0019, "mnemonic": "jmp", "op_str": "0020", "bytes": "E90000"},
            {"address": 0x0020, "mnemonic": "ret", "op_str": "", "bytes": "C3"},
        ],
    });
    let mut r = switch_renderer(0x100, &[0x10, 0x00, 0x16, 0x00]);
    let src = r.render_function_c(&func, &known(&[0x0000])).join("\n");
    assert!(src.contains("switch (memb(cs, 0x8E7))"), "{src}");
    assert!(src.contains("case 0:"), "{src}");
    assert!(src.contains("case 1:"), "{src}");
    assert!(src.contains("ax = 1;"), "{src}");
    assert!(src.contains("ax = 2;"), "{src}");
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
    let mut r = switch_renderer(0x100, &[0x00, 0x01, 0x00, 0x02]);
    let src = r
        .render_function_c(&func, &known(&[0x0000, 0x0100, 0x0200]))
        .join("\n");
    assert!(src.contains("switch (al)"), "{src}");
    assert!(src.contains("case 0: func_0100();"), "{src}");
    assert!(src.contains("case 1: func_0200();"), "{src}");
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
    let mut r = switch_renderer(0x100, &[0x0A, 0x00, 0x0B, 0x00]);
    let src = r.render_function_c(&func, &known(&[0x0000])).join("\n");
    assert!(src.contains("switch (al)"), "{src}");
    assert!(src.contains("case 0:"), "{src}");
    assert!(src.contains("case 1:"), "{src}");
    assert!(src.contains("case 1: /* 0x000B */"), "{src}");
    assert!(!src.contains("jump_table("), "{src}");
}

// ===========================================================================
// ===========================================================================

#[test]
#[ignore = "port-divergence: CCodeRenderer._terminates / _NORETURN_FUNCS are private in the Rust port"]
fn terminates__on_noreturn_funcs() {
    // NOTE(port-divergence): the original asserts CCodeRenderer()._terminates("<fn>();")
    // is True for each name in the private module list _NORETURN_FUNCS. In the
    // Rust port both `Renderer::terminates` and `NORETURN_FUNCS` are private
    // (no `pub`), so they are not reachable from an integration-test crate and
    // there is no public API equivalent. Left #[ignore]d rather than weakened.
}
