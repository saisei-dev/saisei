#![allow(non_snake_case)]
//! Ported from tests/test_*.py — CFG/basic_block + PCSwitch batches.
//!
//! PORT DISPOSITIONS (C backend deleted):
//!   unchanged: flag_setters__normalize_flags_attaches_previous,
//!              parse_imm__parse_negative_immediates (front-half)
//!   ported:    if_else__with_hex_suffix, exec_stack_fields, rcb FIELD_1 write,
//!              sequential_jumps (fold-count → one `if` per jz), empty_then,
//!              test/or fold (branch conditions), cmp_multiple_jumps,
//!              jcc_in_separate_block (linear cond_prev survives an
//!              interleaved ret), bp_relative + nested_bp (nested → collapsed,
//!              see below), if_then/if_else_spans_blocks, when_then_returns,
//!              guard_if (flat model), call_no_return (block fn has push +
//!              `return next-pc`), jump_table reg×8 (base & pcsw variants
//!              merged — one backend now), retf helper, call pushes retip ×2,
//!              indirect_near_jmp, default arm (near_ret_tail_), late entry
//!              block, dos_exit_returns (SEMANTICS FLIPPED: int 21h/AH=4Ch is
//!              dos_api() — not statically nore turn — so the trailing insn
//!              legitimately renders; asserted PRESENT, was asserted absent)
//!   collapsed: nested_bp_relative ([si + [bp-4]] is Unsupported in the Rust
//!              backend — asserted is_err), dos_get_version → dos_api()
//!   deleted:   cfg_cycle__* (2, postdominator internals),
//!              flag_setters__propagate_flag_conditions_across_blocks (C-only
//!              cross-block propagation pass), merge_tails__* (2, cfg
//!              merge_shared_tails), detect_if_else_avoids_self_referential
//!              (patterns.rs), dos_get_version_rendered_before_if +
//!              if_else_statement_is_structured + pc_switch_renderer_emits_cases
//!              (exact duplicates of tests in emit_smoke.rs / verify_helpers.rs)
mod common;
use common::*;
use saisei_jitc::disassemble;
use saisei_jitc::translate::{self, normalize_flags};
use serde_json::{json, Value};

// ---------- local helpers (this file only) ----------

/// serde_json object -> Insn (== serde_json::Map<String, Value>).
fn obj(v: Value) -> serde_json::Map<String, Value> {
    v.as_object().unwrap().clone()
}

/// The `cond_prev.mnemonic` attached to an instruction, if any.
fn cond_prev_mnem(insn: &serde_json::Map<String, Value>) -> Option<&str> {
    insn.get("cond_prev")
        .and_then(|v| v.get("mnemonic"))
        .and_then(Value::as_str)
}

// ==========================================================================
// attach the prior flag-setting insn as cond_prev. (parametrized, front-half)
// ==========================================================================

const FLAG_CASES: &[(&str, &str)] = &[
    ("add", "al, 1"),
    ("sub", "al, 1"),
    ("xor", "al, al"),
    ("adc", "al, 1"),
    ("sbb", "al, 1"),
    ("shl", "ax, 1"),
    ("shr", "ax, 1"),
    ("sal", "ax, 1"),
    ("sar", "ax, 1"),
    ("rol", "al, 1"),
    ("ror", "al, 1"),
    ("rcl", "al, 1"),
    ("rcr", "al, 1"),
];

#[test]
fn flag_setters__normalize_flags_attaches_previous() {
    for &(mnemonic, op) in FLAG_CASES {
        let instrs = vec![
            obj(json!({"address": 0, "mnemonic": mnemonic, "op_str": op, "bytes": ""})),
            obj(json!({"address": 2, "mnemonic": "je", "op_str": "0005", "bytes": ""})),
        ];
        let result = normalize_flags(&instrs);
        assert_eq!(
            cond_prev_mnem(&result[1]),
            Some(mnemonic),
            "mnemonic {mnemonic}"
        );
    }
}

// ==========================================================================
// parse negative immediates identically. (front-half)
// ==========================================================================

#[test]
fn parse_imm__parse_negative_immediates() {
    for (token, expected) in [("-42", -42), ("-0x2A", -42), ("-2Ah", -42)] {
        assert_eq!(disassemble::parse_imm(token), Some(expected), "cfg {token}");
        assert_eq!(
            translate::parse_imm(token),
            Some(expected),
            "ir_to_c {token}"
        );
    }
}

// ==========================================================================
// h-suffix hex targets (a fixture idiom the C parser accepted) are rejected.
// ==========================================================================

#[test]
fn if_else__hex_suffix_targets_are_rejected() {
    let func = json!({
        "start": 0x0000,
        "instructions": [
            {"address": 0x0000, "mnemonic": "jz", "op_str": "0008h", "bytes": "7406"},
            {"address": 0x0002, "mnemonic": "call", "op_str": "1000h", "bytes": "E80000", "target": 0x1000},
            {"address": 0x0005, "mnemonic": "jmp", "op_str": "000Bh", "bytes": "EB04"},
            {"address": 0x0008, "mnemonic": "call", "op_str": "2000h", "bytes": "E80000", "target": 0x2000},
            {"address": 0x000B, "mnemonic": "ret", "op_str": "", "bytes": "C3"},
        ],
    });
    // h-suffix targets are a hand-written-fixture idiom; real capstone IR is
    // always 0x-prefixed (and carries `target`). The backend cannot express one,
    // so it becomes a trap that names it — reached only if control gets there.
    let src = render_rs_ir(
        &json!({"functions": [func]}),
        &[0x0000, 0x1000, 0x2000],
        "g_",
    )
    .expect("chunk still compiles");
    assert!(
        src.contains(r#"r.jit_unsupported_instruction(c"jz 0008h".as_ptr());"#),
        "{src}"
    );
}

// ==========================================================================
// exec_params / RCB named-field rewrites survive in the Rust backend.
// ==========================================================================

#[test]
fn if_else__exec_stack_fields_rendered_with_names() {
    let func = json!({
        "start": 0x0000,
        "instructions": [
            {"address": 0x0000, "mnemonic": "mov", "op_str": "word ptr cs:[0x8c2], sp", "bytes": "2e8926c208"},
            {"address": 0x0005, "mnemonic": "mov", "op_str": "sp, word ptr cs:[0x8c2]", "bytes": "2e8b26c208"},
            {"address": 0x000A, "mnemonic": "ret", "op_str": "", "bytes": "C3"},
        ],
    });
    let src = render_rs(&func, &[], "");
    assert!(src.contains("set_exec_saved_sp(r.sp());"), "{src}");
    assert!(src.contains("r.set_sp(exec_saved_sp());"), "{src}");
}

#[test]
fn if_else__resident_control_block_fields_named() {
    let func = json!({
        "start": 0x0000,
        "instructions": [
            {"address": 0x0000, "mnemonic": "mov", "op_str": "word ptr es:[0xff00], 0x2d9", "bytes": "26c70600ffd902"},
            {"address": 0x0007, "mnemonic": "ret", "op_str": "", "bytes": "C3"},
        ],
    });
    let src = render_rs(&func, &[], "");
    assert!(src.contains("r.rcb_write16(FIELD_1, 0x2D9);"), "{src}");
}

// ==========================================================================
// branch conditions (the C if/else structuring is gone; each jz is one branch)
// ==========================================================================

#[test]
fn if_else__sequential_jumps_each_emit_a_branch() {
    let func = json!({
        "start": 0x0000,
        "instructions": [
            {"address": 0x0000, "mnemonic": "jz", "op_str": "0x5", "bytes": "7403"},
            {"address": 0x0002, "mnemonic": "call", "op_str": "0x1000", "bytes": "E80000", "target": 0x1000},
            {"address": 0x0005, "mnemonic": "jz", "op_str": "0xA", "bytes": "7403"},
            {"address": 0x0007, "mnemonic": "call", "op_str": "0x2000", "bytes": "E80000", "target": 0x2000},
            {"address": 0x000A, "mnemonic": "jz", "op_str": "0xF", "bytes": "7403"},
            {"address": 0x000C, "mnemonic": "call", "op_str": "0x3000", "bytes": "E80000", "target": 0x3000},
            {"address": 0x000F, "mnemonic": "ret", "op_str": "", "bytes": "C3"},
        ],
    });
    let src = render_rs(&func, &[0x0000, 0x1000, 0x2000, 0x3000], "g_");
    assert_eq!(src.matches("if r.ZF() == 1").count(), 3, "{src}");
    assert!(src.contains("return 0x1000;"), "{src}");
    assert!(src.contains("return 0x2000;"), "{src}");
    assert!(src.contains("return 0x3000;"), "{src}");
}

#[test]
fn if_else__empty_then_branch_renders() {
    let func = json!({
        "start": 0x0000,
        "instructions": [
            {"address": 0x0000, "mnemonic": "jz", "op_str": "0x6", "bytes": "7404"},
            {"address": 0x0002, "mnemonic": "jmp", "op_str": "0xA", "bytes": "EB06", "target": 0xA},
            {"address": 0x0006, "mnemonic": "call", "op_str": "0x1000", "bytes": "E80000", "target": 0x1000},
            {"address": 0x0009, "mnemonic": "jmp", "op_str": "0xA", "bytes": "EBFF", "target": 0xA},
            {"address": 0x000A, "mnemonic": "ret", "op_str": "", "bytes": "C3"},
        ],
    });
    let src = render_rs(&func, &[0x0000, 0x1000], "g_");
    assert!(src.contains("if r.ZF() == 1"), "{src}");
    assert!(src.contains("return 0x1000;"), "{src}");
}

#[test]
fn if_else__test_and_jz_branch_on_zf() {
    let func = json!({
        "start": 0x0000,
        "instructions": [
            {"address": 0x0000, "mnemonic": "test", "op_str": "ax, ax", "bytes": "85C0"},
            {"address": 0x0002, "mnemonic": "jz", "op_str": "0007", "bytes": "7403"},
            {"address": 0x0004, "mnemonic": "mov", "op_str": "bx, 1", "bytes": "BB0100"},
            {"address": 0x0007, "mnemonic": "ret", "op_str": "", "bytes": "C3"},
        ],
    });
    let src = render_rs(&func, &[], "");
    assert!(src.contains("if r.ZF() == 1"), "{src}");
}

#[test]
fn if_else__or_and_jnz_branch_on_zf() {
    let func = json!({
        "start": 0x0000,
        "instructions": [
            {"address": 0x0000, "mnemonic": "or", "op_str": "dx, dx", "bytes": "09D2"},
            {"address": 0x0002, "mnemonic": "jnz", "op_str": "0007", "bytes": "7503"},
            {"address": 0x0004, "mnemonic": "mov", "op_str": "ax, 0", "bytes": "B80000"},
            {"address": 0x0007, "mnemonic": "ret", "op_str": "", "bytes": "C3"},
        ],
    });
    let src = render_rs(&func, &[], "");
    assert!(src.contains("if r.ZF() == 0"), "{src}");
}

#[test]
fn if_else__cmp_followed_by_multiple_jumps_preserves_flags() {
    let func = json!({
        "start": 0x0000,
        "instructions": [
            {"address": 0x0000, "mnemonic": "cmp", "op_str": "al, 0x20", "bytes": "3C20"},
            {"address": 0x0002, "mnemonic": "je", "op_str": "0x0A", "bytes": "7406", "target": 0xA},
            {"address": 0x0004, "mnemonic": "jae", "op_str": "0x0E", "bytes": "7308", "target": 0xE},
            {"address": 0x0006, "mnemonic": "mov", "op_str": "bl, 0", "bytes": "B300"},
            {"address": 0x0008, "mnemonic": "jmp", "op_str": "0x12", "bytes": "EB08", "target": 0x12},
            {"address": 0x000A, "mnemonic": "mov", "op_str": "bl, 1", "bytes": "B301"},
            {"address": 0x000C, "mnemonic": "jmp", "op_str": "0x12", "bytes": "EB04", "target": 0x12},
            {"address": 0x000E, "mnemonic": "mov", "op_str": "bl, 2", "bytes": "B302"},
            {"address": 0x0010, "mnemonic": "jmp", "op_str": "0x12", "bytes": "EB00", "target": 0x12},
            {"address": 0x0012, "mnemonic": "ret", "op_str": "", "bytes": "C3"},
        ],
    });
    let src = render_rs(&func, &[], "");
    // je uses ZF, the following jae reuses the SAME cmp's CF
    assert!(src.contains("if r.ZF() == 1"), "{src}");
    assert!(src.contains("if r.CF() == 0"), "{src}");
}

#[test]
fn if_else__jcc_in_separate_block_uses_prior_cmp() {
    let func = json!({
        "start": 0x0000,
        "instructions": [
            {"address": 0x0000, "mnemonic": "cmp", "op_str": "al, 0x20", "bytes": "3C20"},
            {"address": 0x0002, "mnemonic": "jne", "op_str": "0008", "bytes": "7504"},
            {"address": 0x0004, "mnemonic": "ret", "op_str": "", "bytes": "C3"},
            {"address": 0x0008, "mnemonic": "jae", "op_str": "0012", "bytes": "7308"},
            {"address": 0x000A, "mnemonic": "mov", "op_str": "bl, 2", "bytes": "B302"},
            {"address": 0x000C, "mnemonic": "ret", "op_str": "", "bytes": "C3"},
            {"address": 0x0012, "mnemonic": "ret", "op_str": "", "bytes": "C3"},
        ],
    });
    let src = render_rs(&func, &[], "");
    assert!(src.contains("if r.CF() == 0"), "{src}");
}

// ==========================================================================
// bp-relative operands
// ==========================================================================

#[test]
fn if_else__bp_relative_operands_lower_to_ss() {
    let func = json!({
        "start": 0x0000,
        "instructions": [
            {"address": 0x0000, "mnemonic": "mov", "op_str": "ax, word ptr [bp - 4]", "bytes": "8B46FC"},
            {"address": 0x0003, "mnemonic": "mov", "op_str": "word ptr [bp + 6], ax", "bytes": "894606"},
            {"address": 0x0006, "mnemonic": "ret", "op_str": "", "bytes": "C3"},
        ],
    });
    let src = render_rs(&func, &[], "");
    // [bp±N] defaults to SS; negative disp is 16-bit two's complement
    assert!(src.contains("r.memw(r.ss()"), "{src}");
    assert!(src.contains("r.memw_write(r.ss()"), "{src}");
    assert!(src.contains("0xFFFC"), "{src}");
}

#[test]
fn if_else__nested_bp_relative_operand_traps() {
    // `[si + [bp - 4]]` is not real x86 addressing, so there is nothing faithful
    // to emit for it: it becomes a trap rather than silently computing something.
    let func = json!({
        "start": 0x0000,
        "instructions": [
            {"address": 0x0000, "mnemonic": "mov", "op_str": "ax, [si + [bp - 4]]", "bytes": "8B00"},
            {"address": 0x0003, "mnemonic": "ret", "op_str": "", "bytes": "C3"},
        ],
    });
    let src = render_rs(&func, &[], "");
    assert!(src.contains("r.jit_unsupported_instruction(c\""), "{src}");
    assert!(
        !src.contains("r.set_ax(r.memw("),
        "must not invent an address: {src}"
    );
}

// ==========================================================================
// multi-block regions
// ==========================================================================

#[test]
fn if_else__if_then_spans_multiple_blocks() {
    let func = json!({
        "start": 0x0016,
        "instructions": [
            {"address": 0x0016, "mnemonic": "mov", "op_str": "ax, 0x3d00", "bytes": "B8003D"},
            {"address": 0x0019, "mnemonic": "int", "op_str": "0x21", "bytes": "CD21"},
            {"address": 0x001B, "mnemonic": "jae", "op_str": "0x25", "bytes": "7308"},
            {"address": 0x001D, "mnemonic": "call", "op_str": "0x520", "bytes": "E80005", "target": 0x520},
            {"address": 0x0020, "mnemonic": "mov", "op_str": "ax, 0x4c00", "bytes": "B8004C"},
            {"address": 0x0023, "mnemonic": "int", "op_str": "0x21", "bytes": "CD21"},
            {"address": 0x0025, "mnemonic": "mov", "op_str": "bx, ax", "bytes": "8BD8"},
        ],
    });
    let src = render_rs(&func, &[0x0016, 0x0520], "g_");
    // int 21h (AH=3Dh open / AH=4Ch exit) both go through the register-based
    // DOS dispatcher; the branch reads its CF result.
    assert_eq!(src.matches("r.dos_api();").count(), 2, "{src}");
    assert!(src.contains("if r.CF() == 0"), "{src}");
    assert!(src.contains("return 0x0520;"), "{src}");
    assert!(src.contains("r.set_bx(r.ax());"), "{src}");
}

#[test]
fn if_else__if_else_spans_multiple_blocks() {
    let func = json!({
        "start": 0x0000,
        "instructions": [
            {"address": 0x0000, "mnemonic": "jz", "op_str": "0x8", "bytes": "7406"},
            {"address": 0x0002, "mnemonic": "call", "op_str": "0x1000", "bytes": "E80000", "target": 0x1000},
            {"address": 0x0005, "mnemonic": "jmp", "op_str": "0xE", "bytes": "EB07", "target": 0xE},
            {"address": 0x0008, "mnemonic": "call", "op_str": "0x2000", "bytes": "E80000", "target": 0x2000},
            {"address": 0x000B, "mnemonic": "call", "op_str": "0x3000", "bytes": "E80000", "target": 0x3000},
            {"address": 0x000E, "mnemonic": "ret", "op_str": "", "bytes": "C3"},
        ],
    });
    let src = render_rs(&func, &[0x0000, 0x1000, 0x2000, 0x3000], "g_");
    assert!(src.contains("if r.ZF() == 1"), "{src}");
    assert!(src.contains("return 0x1000;"), "{src}");
    assert!(src.contains("return 0x2000;"), "{src}");
    assert!(src.contains("return 0x3000;"), "{src}");
}

#[test]
fn if_else__if_else_when_then_returns() {
    let func = json!({
        "start": 0x0000,
        "instructions": [
            {"address": 0x0000, "mnemonic": "jz", "op_str": "0x5", "bytes": "7403"},
            {"address": 0x0002, "mnemonic": "ret", "op_str": "", "bytes": "C3"},
            {"address": 0x0005, "mnemonic": "call", "op_str": "0x1000", "bytes": "E80000", "target": 0x1000},
            {"address": 0x0008, "mnemonic": "ret", "op_str": "", "bytes": "C3"},
        ],
    });
    let src = render_rs(&func, &[0x0000, 0x1000], "g_");
    assert!(src.contains("if r.ZF() == 1"), "{src}");
    assert!(src.contains("return 0x1000;"), "{src}");
    // both ret instructions render their own pop-return epilogue
    assert_eq!(
        src.matches("let popped_ip = r.memw(r.ss(), r.sp());")
            .count(),
        2,
        "{src}"
    );
}

#[test]
fn if_else__guard_if_is_flat() {
    let func = json!({
        "start": 0x0000,
        "instructions": [
            {"address": 0x0000, "mnemonic": "jz", "op_str": "0x5", "bytes": "7403"},
            {"address": 0x0002, "mnemonic": "jmp", "op_str": "0x100", "bytes": "E90001", "target": 0x100},
            {"address": 0x0005, "mnemonic": "call", "op_str": "0x2000", "bytes": "E80000", "target": 0x2000},
            {"address": 0x0008, "mnemonic": "ret", "op_str": "", "bytes": "C3"},
        ],
    });
    let src = render_rs(&func, &[0x2000], "g_");
    // Faithful flat model: `jmp 0x100` lowers to `return 0x0100;` — the block
    // yields the next pc to the dispatch loop (never a tail-call into a coined
    // helper symbol).
    assert!(!src.contains("g_func_0100();"), "{src}");
    assert!(src.contains("return 0x0100;"), "{src}");
    assert!(src.contains("return 0x2000;"), "{src}");
}

// ==========================================================================
// call lowering: push return IP, then yield the callee pc to the dispatch
// loop (`return 0xTARGET;`) — never `return -1;` (done) for the call itself.
// ==========================================================================

#[test]
fn call_no_return__intra_chunk_call_continues() {
    let func = json!({
        "start": 0x0000,
        "instructions": [
            {"address": 0x0000, "mnemonic": "call", "op_str": "0100", "bytes": "E80000", "target": 0x0100},
        ],
    });
    let src = render_rs_dispatch(&[func], &[0x0100]);
    let block = blk(&src, 0x0000);
    assert!(
        block.contains("r.set_sp((r.sp().wrapping_sub(2)) & 0xFFFF);"),
        "{block}"
    );
    assert!(
        block.contains("r.memw_write(r.ss(), r.sp(), ((0x3u32).wrapping_add(0x10100).wrapping_sub((r.cs() as u32) << 4)) as u16);"),
        "{block}"
    );
    assert!(block.contains("return 0x0100;"), "{block}");
    // the call transfers by yielding the callee pc — any `return -1;` (done)
    // in the block is only the unreachable backstop AFTER the transfer, never
    // the call itself; and dispatch-level `continue;` no longer exists at all
    assert!(!block.contains("continue;"), "{block}");
    assert!(
        block.find("return 0x0100;").unwrap() < block.find("return -1;").unwrap_or(usize::MAX),
        "{block}"
    );
}

#[test]
fn call__pushes_retip_and_continues() {
    let func = json!({
        "start": 0x0100,
        "instructions": [
            {"address": 0x0100, "mnemonic": "call", "op_str": "0200", "bytes": "E8FD00", "target": 0x0200},
        ],
    });
    let src = render_rs_dispatch(&[func], &[0x0100, 0x0200]);
    let block = blk(&src, 0x0100);
    assert!(
        block.contains("r.memw_write(r.ss(), r.sp(), ((0x103u32).wrapping_add(0x10100).wrapping_sub((r.cs() as u32) << 4)) as u16);"),
        "{block}"
    );
    assert!(block.contains("return 0x0200;"), "{block}");
    // never a direct call to a coined per-function symbol
    assert!(!block.contains("func_0200"), "{block}");
}

#[test]
fn call__to_known_address_sets_pc() {
    let func = json!({
        "start": 0x0000,
        "instructions": [
            {"address": 0x0000, "mnemonic": "call", "op_str": "0CAD", "bytes": "E8AA0C", "target": 0x0CAD},
            {"address": 0x0003, "mnemonic": "ret", "op_str": "", "bytes": "C3"},
        ],
    });
    let src = render_rs_dispatch(&[func], &[0x0000, 0x0CAD]);
    let block = blk(&src, 0x0000);
    assert!(
        block.contains("r.memw_write(r.ss(), r.sp(), ((0x3u32).wrapping_add(0x10100).wrapping_sub((r.cs() as u32) << 4)) as u16);"),
        "{block}"
    );
    assert!(block.contains("return 0x0CAD;"), "{block}");
    assert!(!block.contains("func_0CAD"), "{block}");
}

// ==========================================================================
// register-indirect and memory-indirect jmp -> jump_table_ (parametrized)
// ==========================================================================

const JMP_REGS: &[&str] = &["ax", "bx", "cx", "dx", "si", "di", "bp", "sp"];

#[test]
fn jump_table__jmp_reg_uses_jump_table() {
    for &reg in JMP_REGS {
        let func = json!({
            "start": 0x0000,
            "instructions": [
                {"address": 0x0000, "mnemonic": "jmp", "op_str": reg, "bytes": "FFE0"},
                {"address": 0x0002, "mnemonic": "ret", "op_str": "", "bytes": "C3"},
            ],
        });
        let src = render_rs(&func, &[], "");
        assert!(src.contains("r.jump_table_("), "reg {reg}: {src}");
        assert!(src.contains(&format!("{reg}()")), "reg {reg}: {src}");
        assert!(
            src.contains("& 0xFFFFF, expected_retip);"),
            "reg {reg}: {src}"
        );
    }
}

#[test]
fn jump_table__indirect_near_jmp_through_memory() {
    let func = json!({
        "start": 0x0000,
        "instructions": [
            {
                "address": 0x0000,
                "mnemonic": "jmp",
                "op_str": "word ptr cs:[bx + 0x588]",
                "bytes": "00",
                "detail": {"mem_refs": [{"segment": "CS", "disp": 0x588, "access": "read"}]},
            },
        ],
    });
    let src = render_rs_dispatch(&[func], &[]);
    assert!(src.contains("r.jump_table_("), "{src}");
    assert!(src.contains("r.memw(r.cs()"), "{src}");
    assert!(src.contains("0x588"), "{src}");
    // the jump_table_ transfer ends the block: it yields no next pc — every
    // return is `return -1;` (done: the transfer's own, plus the unreachable
    // backstop before the closing brace)
    let block = blk(&src, 0x0000);
    assert!(!block.contains("return 0x"), "{block}");
    assert_eq!(block.matches("return -1;").count(), 2, "{block}");
}

// ==========================================================================
// dispatch shape
// ==========================================================================

#[test]
fn pc_switch__ret_far_emits_helper() {
    let func = json!({
        "start": 0x0000,
        "instructions": [
            {"address": 0x0000, "mnemonic": "retf", "op_str": "", "bytes": "CB"},
        ],
    });
    let src = render_rs_dispatch(&[func], &[]);
    let block = blk(&src, 0x0000);
    assert!(block.contains("r.retf_();"), "{block}");
}

#[test]
fn pc_switch__default_arm_is_cross_binary_ret() {
    let func = json!({
        "start": 0x0000,
        "instructions": [
            {"address": 0x0000, "mnemonic": "ret", "op_str": "", "bytes": "C3"},
        ],
    });
    let src = render_rs_dispatch(&[func], &[]);
    let default_block = src.split("_ => {").nth(1).unwrap();
    assert!(
        default_block.contains("r.near_ret_tail_(popped_ip, expected_retip);"),
        "{default_block}"
    );
    assert!(default_block.contains("return;"), "{default_block}");
}

#[test]
fn pc_switch__entry_block_emitted_when_start_is_late() {
    let func = json!({
        "start": 0x0100,
        "instructions": [
            {"address": 0x0102, "mnemonic": "mov", "op_str": "ax, ax", "bytes": "89C0"},
            {"address": 0x0104, "mnemonic": "ret", "op_str": "", "bytes": "C3"},
        ],
    });
    let src = render_rs_dispatch(&[func], &[]);
    // the undecoded entry pc gets a forwarder arm to the first real block,
    // which gets a dispatch arm + block fn of its own
    assert!(src.contains("0x0100 => 0x0102,"), "{src}");
    assert!(
        src.contains("0x0102 => blk_0102(r, expected_retip),"),
        "{src}"
    );
    let entry_block = blk(&src, 0x0102);
    assert!(entry_block.contains("r.set_ax(r.ax());"), "{entry_block}");
}

#[test]
fn pc_switch__int21_ah4c_goes_through_dos_api_and_falls_through() {
    // The C backend statically special-cased AH=4Ch as a noreturn dos_exit()
    // and dropped the trailing insn. The Rust backend is faithful to the
    // register-based dispatch: int 21h is dos_api() (which exits at RUNTIME for
    // AH=4Ch), so the following instruction still renders.
    let func = json!({
        "start": 0x0100,
        "instructions": [
            {"address": 0x0100, "mnemonic": "mov", "op_str": "ah, 4Ch", "bytes": "B44C"},
            {"address": 0x0102, "mnemonic": "int", "op_str": "21h", "bytes": "CD21"},
            {"address": 0x0104, "mnemonic": "mov", "op_str": "bx, bx", "bytes": "89DB"},
        ],
    });
    let src = render_rs_dispatch(&[func], &[]);
    let block = blk(&src, 0x0100);
    assert!(block.contains("r.set_ah(0x4C);"), "{block}");
    assert!(block.contains("r.dos_api();"), "{block}");
    assert!(block.contains("r.set_bx(r.bx());"), "{block}");
}
