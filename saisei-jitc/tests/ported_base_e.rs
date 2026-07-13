//! Ported from tests/test_*.py — unknown_prefix, while_loop, xlatb,
//! iterative_structuring, jcc_after_push, jcc_loop, jcc_parity,
//! jcc_unsupported_address, lcall_block_boundary, lodsb_jmp_jcc,
//! loop_exit_consumed, loop_header_work, or_jcc_preserved, postdom_bug,
//! register_wraparound, runtime_abi_contract, safe_point_insertion,
//! shared_block_after_uncond_jmp.
//!
//! PORT DISPOSITIONS (C backend deleted):
//!   unchanged: jcc_loop (2), jcc_parity, jcc_unsupported_address (front-half
//!              jcc_condition), lcall_block_boundary (cfg::build_cfg out-degree
//!              re-expressed via translate::cfg_successors)
//!   ported:    unknown_prefix (C renderer process::exit(2) → the Rust backend
//!              returns Unsupported, asserted directly — no child process),
//!              the 8 while/do-while/for tests (structure SHAPE deleted; the
//!              semantic core — branch conditions, arithmetic blocks, back
//!              edges — asserted on the state machine), xlatb (2),
//!              jcc_after_push (was #[ignore]d for a C-port handle_call bug;
//!              the Rust backend handles it — now active), lodsb_jmp_jcc,
//!              loop_exit_consumed (block dedup → one match arm), loop_header_work,
//!              or_jcc_preserved, register_wraparound, safe_point_insertion (2 →
//!              per-instruction set_ip/SAFEPOINT pairing invariant),
//!              shared_block_after_uncond_jmp, runtime_abi_contract (2: manifest
//!              re-sourced from rt/saisei_rt.rs — fn names + register-accessor
//!              macros; artifacts scanned at build/*/jit/*.rs)
//!   deleted:   iterative_structuring (AstNode shapes), postdom_bug (structure()
//!              internals), runtime_abi_contract__shims_includes_runtime_abi
//!              (asserted a C header #include)
#![allow(non_snake_case)]
mod common;
use common::*;
use regex::Regex;
use saisei_jitc::translate::{self, Insn};
use serde_json::{json, Value};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

// ---------------------------------------------------------------------------
// local helpers
// ---------------------------------------------------------------------------

/// Convert a JSON array of instruction dicts into a Vec<Insn> (serde_json Maps).
fn to_insns(v: Value) -> Vec<Insn> {
    v.as_array()
        .expect("instruction array")
        .iter()
        .map(|x| x.as_object().expect("instruction object").clone())
        .collect()
}

fn repo_root() -> PathBuf {
    // CARGO_MANIFEST_DIR = <repo>/saisei-jitc; its parent is the repo root.
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .to_path_buf()
}

// ===========================================================================
// An instruction the emitter can't express becomes a run-time trap, not a failed
// chunk: the decode is speculative and mostly reaches bytes the game never
// executes. It still names the construct, and still aborts if control gets there.
// ===========================================================================

#[test]
fn unknown_prefix__traps_when_reached() {
    let func = json!({
        "start": 0x0000,
        "instructions": [
            {"address": 0x0000, "mnemonic": "lock inc", "op_str": "ax", "bytes": ""},
            {"address": 0x0001, "mnemonic": "ret", "op_str": "", "bytes": ""},
        ],
    });
    let src = render_rs(&func, &[0x0000], "");
    assert!(
        src.contains(r#"r.jit_unsupported_instruction(c"prefix:lock inc".as_ptr());"#),
        "{src}"
    );
}

// ===========================================================================
// The while/do-while/for structuring tests: the Rust backend is a flat
// pc-state-machine, so the loop SHAPE is gone; what must survive is the
// semantic content — conditions, arithmetic, and the back edges.
// ===========================================================================

#[test]
fn while_loop__top_checked_condition_and_body() {
    let func = json!({
        "start": 0x0000,
        "instructions": [
            {"address": 0x0000, "mnemonic": "cmp", "op_str": "cx, 3", "bytes": "83F9"},
            {"address": 0x0002, "mnemonic": "jge", "op_str": "000C", "bytes": "7D08"},
            {"address": 0x0004, "mnemonic": "mov", "op_str": "ax, bx", "bytes": "89D8"},
            {"address": 0x0006, "mnemonic": "inc", "op_str": "cx", "bytes": "41"},
            {"address": 0x0007, "mnemonic": "jmp", "op_str": "0000", "bytes": "E9F9FF", "target": 0x0000},
            {"address": 0x000C, "mnemonic": "ret", "op_str": "", "bytes": "C3"},
        ],
    });
    let src = render_rs(&func, &[], "");
    assert!(src.contains("if r.SF() == r.OF()"), "{src}");
    assert!(src.contains("r.set_ax(r.bx());"), "{src}");
    // loop back edge to the cmp header
    assert!(src.contains("return 0x0000;"), "{src}");
}

#[test]
fn while_loop__loop_instruction_decrements_cx() {
    let func = json!({
        "start": 0x0000,
        "instructions": [
            {"address": 0x0000, "mnemonic": "cmp", "op_str": "byte ptr [si], 0x20", "bytes": "803C20"},
            {"address": 0x0003, "mnemonic": "jne", "op_str": "000A", "bytes": "7505"},
            {"address": 0x0005, "mnemonic": "inc", "op_str": "si", "bytes": "46"},
            {"address": 0x0006, "mnemonic": "loop", "op_str": "0000", "bytes": "E2F8", "target": 0x0000},
            {"address": 0x0008, "mnemonic": "ret", "op_str": "", "bytes": "C3"},
            {"address": 0x000A, "mnemonic": "ret", "op_str": "", "bytes": "C3"},
        ],
    });
    let src = render_rs(&func, &[], "");
    // inc si (full flag block off `old`)
    assert!(
        src.contains("r.set_si((old.wrapping_add(1) & 0xFFFF) as u16);"),
        "{src}"
    );
    // loop = dec cx + branch on cx != 0 back to the header
    assert!(src.contains("wrapping_sub(1)"), "{src}");
    assert!(src.contains("return 0x0000;"), "{src}");
}

#[test]
fn while_loop__nested_if_conditions_present() {
    let func = json!({
        "start": 0x0000,
        "instructions": [
            {"address": 0x0000, "mnemonic": "cmp", "op_str": "cx, 3", "bytes": "83F9"},
            {"address": 0x0002, "mnemonic": "jge", "op_str": "0x10", "bytes": "7D0C"},
            {"address": 0x0004, "mnemonic": "cmp", "op_str": "ax, 0", "bytes": "3D0000"},
            {"address": 0x0007, "mnemonic": "je", "op_str": "0xB", "bytes": "7402"},
            {"address": 0x0009, "mnemonic": "inc", "op_str": "bx", "bytes": "43"},
            {"address": 0x000A, "mnemonic": "jmp", "op_str": "0xB", "bytes": "EBFF", "target": 0xB},
            {"address": 0x000B, "mnemonic": "inc", "op_str": "cx", "bytes": "41"},
            {"address": 0x000C, "mnemonic": "jmp", "op_str": "0x0", "bytes": "E9F1FF", "target": 0x0},
            {"address": 0x0010, "mnemonic": "ret", "op_str": "", "bytes": "C3"},
        ],
    });
    let src = render_rs(&func, &[], "");
    assert!(src.contains("if r.SF() == r.OF()"), "{src}");
    assert!(src.contains("if r.ZF() == 1"), "{src}");
    assert!(
        src.contains("r.set_bx((old.wrapping_add(1) & 0xFFFF) as u16);"),
        "{src}"
    );
}

#[test]
fn while_loop__initialization_before_loop_rendered_first() {
    let func = json!({
        "start": 0x0000,
        "instructions": [
            {"address": 0x0000, "mnemonic": "mov", "op_str": "cx, 0", "bytes": "00"},
            {"address": 0x0001, "mnemonic": "jmp", "op_str": "0x4", "bytes": "00", "target": 0x4},
            {"address": 0x0004, "mnemonic": "cmp", "op_str": "cx, 3", "bytes": "00"},
            {"address": 0x0005, "mnemonic": "jge", "op_str": "0x8", "bytes": "00"},
            {"address": 0x0006, "mnemonic": "inc", "op_str": "cx", "bytes": "00"},
            {"address": 0x0007, "mnemonic": "jmp", "op_str": "0x4", "bytes": "00", "target": 0x4},
            {"address": 0x0008, "mnemonic": "ret", "op_str": "", "bytes": "00"},
        ],
    });
    let src = render_rs(&func, &[], "");
    // The init executes in the entry block, not in the loop-header block.
    let entry = blk(&src, 0x0000);
    assert!(
        entry.contains("r.set_cx(0x0);") || entry.contains("r.set_cx(0);"),
        "{src}"
    );
    let header = blk(&src, 0x0004);
    assert!(!header.contains("r.set_cx(0"), "{src}");
}

#[test]
fn while_loop__do_while_back_edge_on_zf() {
    let func = json!({
        "start": 0x0000,
        "instructions": [
            {"address": 0x0000, "mnemonic": "mov", "op_str": "dl, 0xff", "bytes": "B2FF"},
            {"address": 0x0002, "mnemonic": "int", "op_str": "0x21", "bytes": "CD21"},
            {"address": 0x0004, "mnemonic": "jne", "op_str": "0000", "bytes": "75FA"},
            {"address": 0x0006, "mnemonic": "ret", "op_str": "", "bytes": "C3"},
        ],
    });
    let src = render_rs(&func, &[], "");
    assert!(src.contains("r.set_dl(0xFF);"), "{src}");
    assert!(src.contains("r.dos_api();"), "{src}");
    // the back edge re-enters the loop head on ZF == 0
    assert!(src.contains("if r.ZF() == 0"), "{src}");
    assert!(src.contains("return 0x0000;"), "{src}");
}

#[test]
fn while_loop__conditional_back_edge() {
    let func = json!({
        "start": 0x0000,
        "instructions": [
            {"address": 0x0000, "mnemonic": "cmp", "op_str": "byte ptr [si], 0x20", "bytes": "803C20"},
            {"address": 0x0003, "mnemonic": "jb", "op_str": "0010", "bytes": "720B"},
            {"address": 0x0005, "mnemonic": "inc", "op_str": "dx", "bytes": "42"},
            {"address": 0x0006, "mnemonic": "cmp", "op_str": "byte ptr [si], 0x20", "bytes": "803C20"},
            {"address": 0x0009, "mnemonic": "jae", "op_str": "0000", "bytes": "7300"},
            {"address": 0x000B, "mnemonic": "ret", "op_str": "", "bytes": "C3"},
            {"address": 0x0010, "mnemonic": "ret", "op_str": "", "bytes": "C3"},
        ],
    });
    let src = render_rs(&func, &[], "");
    assert!(
        src.contains("r.set_dx((old.wrapping_add(1) & 0xFFFF) as u16);"),
        "{src}"
    );
    // jae takes the back edge on CF == 0
    assert!(src.contains("if r.CF() == 0"), "{src}");
    assert!(src.contains("return 0x0000;"), "{src}");
}

#[test]
fn while_loop__cond_prev_annotation_respected() {
    let func = json!({
        "start": 0x0000,
        "instructions": [
            {"address": 0x0000, "mnemonic": "mov", "op_str": "al, byte ptr [bx]", "bytes": "8A07"},
            {"address": 0x0002, "mnemonic": "or", "op_str": "al, al", "bytes": "08C0"},
            {"address": 0x0004, "mnemonic": "jz", "op_str": "000A", "bytes": "7404",
             "cond_prev": {"mnemonic": "or", "op_str": "al, al"}},
            {"address": 0x0006, "mnemonic": "inc", "op_str": "bx", "bytes": "43"},
            {"address": 0x0007, "mnemonic": "jmp", "op_str": "0000", "bytes": "E9F6FF", "target": 0x0000},
            {"address": 0x000A, "mnemonic": "ret", "op_str": "", "bytes": "C3"},
        ],
    });
    let src = render_rs(&func, &[], "");
    assert!(
        src.contains("r.set_bx((old.wrapping_add(1) & 0xFFFF) as u16);"),
        "{src}"
    );
    assert!(src.contains("if r.ZF() == 1"), "{src}");
    assert!(src.contains("return 0x0000;"), "{src}");
}

#[test]
fn while_loop__dec_dx_jne_branches_on_zf() {
    let func = json!({
        "start": 0x0000,
        "instructions": [
            {"address": 0x0000, "mnemonic": "cmp", "op_str": "ax, 0xffff", "bytes": "3DFFFF"},
            {"address": 0x0003, "mnemonic": "mov", "op_str": "al, 0", "bytes": "B000"},
            {"address": 0x0005, "mnemonic": "dec", "op_str": "dx", "bytes": "4A"},
            {"address": 0x0006, "mnemonic": "jne", "op_str": "0x5", "bytes": "75FD"},
            {"address": 0x0008, "mnemonic": "ret", "op_str": "", "bytes": "C3"},
        ],
    });
    let src = render_rs(&func, &[], "");
    assert!(
        src.contains("r.set_dx((old.wrapping_sub(1) & 0xFFFF) as u16);"),
        "{src}"
    );
    assert!(src.contains("if r.ZF() == 0"), "{src}");
    assert!(src.contains("return 0x0005;"), "{src}");
}

// ===========================================================================
// ===========================================================================

#[test]
fn xlatb__loads_table_byte() {
    let func = json!({
        "start": 0x0000,
        "instructions": [
            {"address": 0x0000, "mnemonic": "xlatb", "op_str": "", "bytes": "D7"},
            {"address": 0x0001, "mnemonic": "ret", "op_str": "", "bytes": "C3"},
        ],
    });
    let src = render_rs(&func, &[], "");
    // al <- [ds:(bx+al) & 0xFFFF]
    assert!(src.contains("r.memb(r.ds()"), "{src}");
    assert!(src.contains("r.set_al("), "{src}");
    assert!(src.contains("& 0xFFFF"), "{src}");
}

#[test]
fn xlatb__respects_segment_override() {
    let func = json!({
        "start": 0x0000,
        "instructions": [
            {"address": 0x0000, "mnemonic": "xlatb", "op_str": "", "bytes": "2ED7",
             "detail": {"seg_override": "CS"}},
            {"address": 0x0002, "mnemonic": "ret", "op_str": "", "bytes": "C3"},
        ],
    });
    let src = render_rs(&func, &[], "");
    assert!(src.contains("r.memb(r.cs()"), "{src}");
}

// ===========================================================================
// jcc after a flag-neutral push: the condition must survive.
// (Was #[ignore]d for a bug in the C renderer's Rust port; the live backend
// handles it.)
// ===========================================================================

#[test]
fn jcc_after_push__condition_survives_push() {
    let func = json!({
        "start": 0x0000,
        "instructions": [
            {"address": 0x0000, "mnemonic": "or", "op_str": "al, al", "bytes": "08C0"},
            {"address": 0x0002, "mnemonic": "push", "op_str": "ax", "bytes": "50"},
            {"address": 0x0003, "mnemonic": "je", "op_str": "0x0008", "bytes": "7403"},
            {"address": 0x0005, "mnemonic": "call", "op_str": "loadFile", "bytes": "E80000", "target": 0x0008},
            {"address": 0x0008, "mnemonic": "ret", "op_str": "", "bytes": "C3"},
        ],
    });
    let src = render_rs(&func, &[0x0008], "app_");
    assert!(src.contains("if r.ZF() == 1"), "{src}");
}

// ===========================================================================
// front-half jcc_condition tests — unchanged
// ===========================================================================

#[test]
fn jcc_loop__loopne_condition_uses_zero_flag() {
    let prev = json!({"mnemonic": "cmp", "op_str": "ax, bx"});
    let prev = prev.as_object().unwrap().clone();
    assert_eq!(
        translate::jcc_condition("loopne", Some(&prev), None),
        "--cx != 0 && ZF == 0"
    );
}

#[test]
fn jcc_loop__loope_condition_uses_zero_flag() {
    let prev = json!({"mnemonic": "cmp", "op_str": "ax, bx"});
    let prev = prev.as_object().unwrap().clone();
    assert_eq!(
        translate::jcc_condition("loope", Some(&prev), None),
        "--cx != 0 && ZF == 1"
    );
}

#[test]
fn jcc_parity__parity_conditions() {
    assert_eq!(translate::jcc_condition("jpo", None, None), "PF == 0");
    assert_eq!(translate::jcc_condition("jpe", None, None), "PF == 1");
}

#[test]
fn jcc_unsupported_address__includes_address_for_unsupported() {
    let comment = translate::jcc_condition("jfoo", None, Some(0x1234));
    assert_eq!(comment, "/* unsupported jcc at 0x1234 */");
}

// ===========================================================================
// ===========================================================================

#[test]
fn lcall_block_boundary__creates_fallthrough_block() {
    let instrs = to_insns(json!([
        {"address": 0x0, "mnemonic": "lcall", "op_str": "0x2000:0x1000", "bytes": "9A00100020"},
        {"address": 0x5, "mnemonic": "nop", "op_str": "", "bytes": "90"},
    ]));
    let blocks = translate::build_basic_blocks(&instrs, &BTreeSet::new(), None);
    assert_eq!(blocks.keys().copied().collect::<Vec<_>>(), vec![0x0, 0x5]);
    let succ = translate::cfg_successors(&blocks);
    assert_eq!(succ.get(&0x0).map(|v| v.len()), Some(1));
}

// ===========================================================================
// ===========================================================================

#[test]
fn lodsb_jmp_jcc__jmp_over_lodsb_drops_prev_condition() {
    let func = json!({
        "start": 0,
        "instructions": [
            {"address": 0, "mnemonic": "cmp", "op_str": "al, 0x5f", "bytes": "3c5f",
             "detail": {"regs_read": ["AL"], "regs_write": [], "mem_refs": []}},
            {"address": 2, "mnemonic": "lodsb", "op_str": "al, byte ptr [si]", "bytes": "ac",
             "detail": {"regs_read": ["SI", "FLAGS"], "regs_write": ["AL", "SI"],
                        "mem_refs": [{"segment": "DS", "disp": 0, "access": "read"}]}},
            {"address": 3, "mnemonic": "jmp", "op_str": "0x0005", "bytes": "e90000", "target": 0x0005},
            {"address": 5, "mnemonic": "jne", "op_str": "0x0008", "bytes": "7501"},
            {"address": 7, "mnemonic": "ret", "op_str": "", "bytes": "c3"},
            {"address": 8, "mnemonic": "ret", "op_str": "", "bytes": "c3"},
        ],
    });
    let src = render_rs(&func, &[], "");
    assert!(src.contains("if r.ZF() == 0"), "{src}");
    assert!(!src.contains("unsupported jcc"), "{src}");
}

// ===========================================================================
// A block shared by several predecessors renders exactly once (one match arm).
// ===========================================================================

#[test]
fn loop_exit_consumed__exit_block_rendered_once() {
    let func = json!({
        "start": 0x0000,
        "instructions": [
            {"address": 0x0000, "mnemonic": "mov", "op_str": "cx, 2", "bytes": "B90200"},
            {"address": 0x0003, "mnemonic": "cmp", "op_str": "al, 1", "bytes": "3C01"},
            {"address": 0x0005, "mnemonic": "je", "op_str": "0xF", "bytes": "7408"},
            {"address": 0x0007, "mnemonic": "inc", "op_str": "di", "bytes": "47"},
            {"address": 0x0008, "mnemonic": "loop", "op_str": "0x3", "bytes": "E2F9", "target": 0x3},
            {"address": 0x000A, "mnemonic": "ret", "op_str": "", "bytes": "C3"},
            {"address": 0x000F, "mnemonic": "mov", "op_str": "bx, bx", "bytes": "89DB"},
            {"address": 0x0011, "mnemonic": "ret", "op_str": "", "bytes": "C3"},
        ],
    });
    let src = render_rs(&func, &[], "");
    assert_eq!(src.matches("r.set_bx(r.bx());").count(), 1, "{src}");
    // both ret instructions emit their own pop-return epilogue
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
fn loop_header_work__header_instructions_preserved() {
    let func = json!({
        "start": 0x0000,
        "instructions": [
            {"address": 0x0000, "mnemonic": "lodsb", "op_str": "", "bytes": "AC"},
            {"address": 0x0001, "mnemonic": "cmp", "op_str": "al, 0", "bytes": "3C00"},
            {"address": 0x0003, "mnemonic": "je", "op_str": "000A", "bytes": "7405"},
            {"address": 0x0005, "mnemonic": "inc", "op_str": "cx", "bytes": "41"},
            {"address": 0x0006, "mnemonic": "jmp", "op_str": "0000", "bytes": "E9F9FF", "target": 0x0000},
            {"address": 0x000A, "mnemonic": "ret", "op_str": "", "bytes": "C3"},
        ],
    });
    let src = render_rs(&func, &[], "");
    assert!(
        src.contains("let delta: i32 = if r.DF() != 0 { -1 } else { 1 };"),
        "{src}"
    );
    assert!(src.contains("r.set_al(r.memb(r.ds(), r.si()));"), "{src}");
    assert!(
        src.contains("r.set_si(((r.si() as i32 + delta) & 0xFFFF) as u16);"),
        "{src}"
    );
    assert!(
        src.contains("r.set_cx((old.wrapping_add(1) & 0xFFFF) as u16);"),
        "{src}"
    );
}

// ===========================================================================
// ===========================================================================

#[test]
fn or_jcc_preserved__or_followed_by_jcc_preserved() {
    let func = json!({
        "start": 0x0000,
        "instructions": [
            {"address": 0x0000, "mnemonic": "or", "op_str": "ax, bx", "bytes": "0BC3"},
            {"address": 0x0002, "mnemonic": "je", "op_str": "0005", "bytes": "7401"},
            {"address": 0x0004, "mnemonic": "ret", "op_str": "", "bytes": "C3"},
            {"address": 0x0005, "mnemonic": "ret", "op_str": "", "bytes": "C3"},
        ],
    });
    let src = render_rs(&func, &[], "");
    // the or writes ax and its flags are USED by the je — not dropped
    assert!(src.contains("| (r.bx()) as u32"), "{src}");
    assert!(src.contains("if r.ZF() == 1"), "{src}");
}

// ===========================================================================
// ===========================================================================

#[test]
fn register_wraparound__arithmetic_masks_results() {
    let func = json!({
        "start": 0x0000,
        "instructions": [
            {"address": 0x0000, "mnemonic": "add", "op_str": "ax, bx", "bytes": ""},
            {"address": 0x0002, "mnemonic": "sub", "op_str": "ax, bx", "bytes": ""},
            {"address": 0x0004, "mnemonic": "ret", "op_str": "", "bytes": ""},
        ],
    });
    let src = render_rs(&func, &[], "");
    // both results are masked back to 16 bits before the register write
    assert_eq!(
        src.matches("& 0xFFFF) as u16);").count() >= 2,
        true,
        "{src}"
    );
    assert!(src.contains("wrapping_add"), "{src}");
    assert!(src.contains("wrapping_sub"), "{src}");
}

// ===========================================================================
// Runtime ABI contract: every runtime call the emitter can produce must be a
// name the chunk prelude (rt/saisei_rt.rs) actually provides. The prelude IS
// the ABI manifest now (was runtime/include/runtime_abi.h).
// ===========================================================================

/// All callable names the prelude provides: every `fn NAME(` (definitions and
/// extern declarations) plus the register/flag accessors generated by the
/// word_reg!/byte_regs!/flag! macro invocations.
fn load_abi_manifest() -> BTreeSet<String> {
    let path = repo_root()
        .join("saisei-jitc")
        .join("rt")
        .join("saisei_rt.rs");
    let text = std::fs::read_to_string(&path).expect("read rt/saisei_rt.rs");

    let mut names: BTreeSet<String> = BTreeSet::new();
    let fn_re = Regex::new(r"\bfn\s+([A-Za-z_]\w*)\s*\(").unwrap();
    for c in fn_re.captures_iter(&text) {
        names.insert(c[1].to_string());
    }
    // word_reg!(ax, set_ax, ax_ptr, r_ax); byte_regs!(al, set_al, al_ptr, …);
    // flag!(CF, set_CF, CF); — every comma-separated ident except the last
    // (the underlying field) is a generated accessor fn.
    let macro_re = Regex::new(r"(?m)^(word_reg|byte_regs|flag)!\(([^)]*)\);").unwrap();
    for c in macro_re.captures_iter(&text) {
        let args: Vec<&str> = c[2].split(',').map(str::trim).collect();
        for a in &args[..args.len().saturating_sub(1)] {
            names.insert((*a).to_string());
        }
    }
    // byte_regs! last-triple args (ax, set_ax, ax_ptr) are accessors too — the
    // simple rule above already keeps them; only the trailing field name of
    // word_reg!/flag! is dropped, which is correct.
    names
}

/// Call-shaped names in generated Rust chunk text, minus method calls,
/// keywords, locals, and the translator's own coined names.
fn abi_runtime_calls(src: &str) -> BTreeSet<String> {
    // strip comments
    let src = Regex::new(r"//[^\n]*").unwrap().replace_all(src, " ");
    let call_re = Regex::new(r"(^|[^\w.])([A-Za-z_]\w*)\s*\(").unwrap();
    let keywords: BTreeSet<&str> = [
        "if", "else", "while", "for", "loop", "match", "return", "fn", "let", "unsafe", "pub",
        "extern", "include", "allow", "Some", "Ok", "Err", "None",
    ]
    .into_iter()
    .collect();
    // The translator's own coined names: `<chunkname>_func_NNNN_impl`,
    // `<chunkname>_blk_NNNN` (per-block fns), `<chunkname>_dispatch` (chunk
    // names carry several `_`-separated segments, e.g.
    // jit_17330_0000_<sha>_func_0000_impl).
    let coined =
        Regex::new(r"^(\w+_)*(func_[0-9A-Fa-f]+_impl|blk_[0-9A-Fa-f]+|dispatch)$").unwrap();

    let mut out = BTreeSet::new();
    for c in call_re.captures_iter(&src) {
        let name = c[2].to_string();
        if keywords.contains(name.as_str()) || coined.is_match(&name) {
            continue;
        }
        out.insert(name);
    }
    out
}

fn abi_render_broad_spread() -> String {
    let specs: &[(&str, &str)] = &[
        ("mov", "ah, 0x30"),
        ("int", "21"),
        ("int", "1a"),
        ("cmp", "al, 2"),
        ("jb", "0x8"),
        ("add", "ax, bx"),
        ("xor", "ax, ax"),
        ("xor", "al, al"),
        ("mov", "ax, bx"),
        ("rep movsb", "byte ptr es:[di], byte ptr ds:[si]"),
        ("rep movsw", "word ptr es:[di], word ptr ds:[si]"),
        ("rep stosb", "byte ptr es:[di]"),
        ("jmp", "ax"),
        ("in", "al, dx"),
        ("out", "dx, al"),
        ("ret", ""),
        ("retf", ""),
        ("iret", ""),
    ];
    let instrs: Vec<Value> = specs
        .iter()
        .enumerate()
        .map(|(i, (m, o))| {
            // 1-byte insns at CONTIGUOUS addresses — the state machine computes
            // fallthrough from address+len, so gaps break block termination.
            let mut v = json!({"address": i as i64, "mnemonic": m, "op_str": o, "bytes": "90"});
            // Direct jumps/jcc need a resolved target (real IR always has one).
            if m.starts_with('j') {
                if let Some(h) = o.strip_prefix("0x") {
                    v["target"] = json!(i64::from_str_radix(h, 16).unwrap());
                }
            }
            v
        })
        .collect();
    let func = json!({"start": 0x0000, "instructions": instrs});
    render_rs(&func, &[], "app_")
}

#[test]
fn runtime_abi_contract__translator_emits_only_declared_abi() {
    let allowed = load_abi_manifest();
    let emitted = abi_runtime_calls(&abi_render_broad_spread());
    let unexpected: Vec<String> = emitted.difference(&allowed).cloned().collect();
    assert!(
        unexpected.is_empty(),
        "translator emits runtime calls the prelude does not provide: {unexpected:?}. \
         Either add them to rt/saisei_rt.rs on purpose, or they are a bug."
    );
}

#[test]
fn runtime_abi_contract__generated_artifacts_respect_abi_if_present() {
    let build = repo_root().join("build");
    let mut gen: Vec<PathBuf> = Vec::new();
    if let Ok(games) = std::fs::read_dir(&build) {
        for game in games.flatten() {
            let jit = game.path().join("jit");
            if let Ok(rd) = std::fs::read_dir(&jit) {
                for entry in rd.flatten() {
                    let p = entry.path();
                    // Only jit_* chunks are artifacts — each jit dir also holds
                    // a copy of the saisei_rt.rs prelude (the manifest itself).
                    let is_chunk = p
                        .file_name()
                        .and_then(|s| s.to_str())
                        .map(|n| n.starts_with("jit_"))
                        .unwrap_or(false);
                    if is_chunk && p.extension().map(|e| e == "rs").unwrap_or(false) {
                        gen.push(p);
                    }
                }
            }
        }
    }
    gen.sort();
    if gen.is_empty() {
        // no generated artifacts present
        return;
    }
    let allowed = load_abi_manifest();
    let mut offenders: Vec<(String, Vec<String>)> = Vec::new();
    for f in &gen {
        let bytes = std::fs::read(f).unwrap_or_default();
        let text = String::from_utf8_lossy(&bytes);
        let unexpected: Vec<String> = abi_runtime_calls(&text)
            .difference(&allowed)
            .cloned()
            .collect();
        if !unexpected.is_empty() {
            let name = f.file_name().unwrap().to_string_lossy().into_owned();
            offenders.push((name, unexpected));
        }
    }
    assert!(
        offenders.is_empty(),
        "artifacts call undeclared runtime symbols: {offenders:?}"
    );
}

// ===========================================================================
// Instrumentation invariant: every instruction gets `set_ip(0xNNNN);`, and
// every basic block opens with one `JIT_BUDGET(n);` poll (n = the block's
// summed per-class instruction weights) right after the block-head set_ip —
// including loop back-edge blocks, so tight loops still hit the safepoint
// slow path.
// ===========================================================================

#[test]
fn safe_point_insertion__block_head_budget_poll_after_ip() {
    let func = json!({
        "start": 0x0000,
        "instructions": [
            {"address": 0x0000, "mnemonic": "mov", "op_str": "ax, bx", "bytes": "89D8"},
            {"address": 0x0002, "mnemonic": "ret", "op_str": "", "bytes": "C3"},
        ],
    });
    let src = render_rs(&func, &[], "");
    let lines: Vec<&str> = src.lines().map(str::trim).collect();
    let ip_lines: Vec<usize> = lines
        .iter()
        .enumerate()
        .filter(|(_, l)| l.starts_with("r.set_ip(0x"))
        .map(|(i, _)| i)
        .collect();
    assert_eq!(ip_lines.len(), 2, "{src}");
    // one poll for the whole 2-instruction block (weights: mov reg,reg 1 +
    // ret 3), directly after the head set_ip
    assert_eq!(lines[ip_lines[0] + 1], "r.budget(4);", "{src}");
    assert_eq!(src.matches("r.budget(").count(), 1, "{src}");
    assert_eq!(src.matches("SAFEPOINT();").count(), 0, "{src}");
}

#[test]
fn safe_point_insertion__loop_back_edge_block_has_budget_poll() {
    let func = json!({
        "start": 0x0000,
        "instructions": [
            {"address": 0x0000, "mnemonic": "mov", "op_str": "cx, 0", "bytes": "B90000"},
            {"address": 0x0003, "mnemonic": "cmp", "op_str": "cx, 3", "bytes": "83F9"},
            {"address": 0x0006, "mnemonic": "jge", "op_str": "0x10", "bytes": "7D08"},
            {"address": 0x0008, "mnemonic": "inc", "op_str": "cx", "bytes": "41"},
            {"address": 0x0009, "mnemonic": "jmp", "op_str": "0x3", "bytes": "E9F9FF", "target": 0x3},
            {"address": 0x0010, "mnemonic": "ret", "op_str": "", "bytes": "C3"},
        ],
    });
    let src = render_rs(&func, &[], "");
    // the body block (inc cx + back edge) is its own block fn and is
    // instrumented (weights: inc 1 + jmp 2)
    let arm = blk(&src, 0x0008);
    assert!(arm.contains("r.budget(3);"), "{arm}");
    assert!(arm.contains("return 0x0003;"), "{arm}");
}

// ===========================================================================
// ===========================================================================

#[test]
fn shared_block_after_uncond_jmp__rendered_once() {
    let func = json!({
        "start": 0x0000,
        "instructions": [
            {"address": 0x0000, "mnemonic": "jmp", "op_str": "0008", "bytes": "EB06", "target": 0x0008},
            {"address": 0x0002, "mnemonic": "nop", "op_str": "", "bytes": "90"},
            {"address": 0x0003, "mnemonic": "ret", "op_str": "", "bytes": "C3"},
            {"address": 0x0008, "mnemonic": "jmp", "op_str": "0002", "bytes": "E9F5FF", "target": 0x0002},
            {"address": 0x000B, "mnemonic": "jmp", "op_str": "0002", "bytes": "E9F2FF", "target": 0x0002},
        ],
    });
    let src = render_rs(&func, &[], "");
    assert_eq!(src.matches("r.set_ip(0x0002);").count(), 1, "{src}");
}
