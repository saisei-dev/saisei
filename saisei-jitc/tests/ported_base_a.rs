#![allow(non_snake_case)]
//! Ported from tests/test_*.py — test_and_jcc_preserved, test_basic_block_comment,
//! test_block_order, test_cfg_indirect_jump, test_cfg_int_terminate_no_fallthrough,
//! test_cfg_shared_postdom, test_if_merge_target, test_interrupt_flag_clobber,
//! test_ir_to_c_aaa, test_ir_to_c_adc, test_ir_to_c_add_cf, test_ir_to_c_call_table,
//! test_ir_to_c_cf_return, test_ir_to_c_cli_sti, test_ir_to_c_cmp_cf,
//! test_ir_to_c_cmpsb, test_ir_to_c_cs_negative, test_ir_to_c_cwde_stc,
//! test_ir_to_c_default_ss.
//!
//! PORT DISPOSITIONS (C backend deleted):
//!   ported:    27 tests — C-text assertions rewritten against the Rust chunk
//!              backend (`render_rs`). Structural-only content dropped from
//!              otherwise-ported tests: block_order__blocks_follow_cfg_traversal_order
//!              (AstNode Goto/Return shapes + traversal-order vec -> the
//!              transfers asserted as `return 0xNNNN;` next-pc edges in the
//!              per-block fns),
//!              ir_to_c_add_cf (do-while `while (CF == 1)` shape -> CF-taken
//!              back-edge `if CF() == 1` + `return 0x0000;`; the C-port
//!              do-while negation divergence note is obsolete),
//!              if_merge_target (if-before-merge line ordering -> branch block +
//!              merge block containment). Two formerly #[ignore]d tests are now
//!              active: basic_block_comment__unhandled_instruction_raises (the
//!              C renderer process::exit(2)'d; the Rust backend returns a
//!              catchable Unsupported) and ir_to_c_add_cf (see above).
//!              interrupt_flag_clobber__int_clobbers_previous_cmp_flag keeps its
//!              semantics with `dos_api()` standing in for the per-AH
//!              `dos_open_file` (the Rust backend does not specialize int 21h);
//!              the ir_to_c_call_table__* five keep the per-variant segment
//!              defaults (cs override / bp+disp -> ss mem operand / no-segment
//!              -> ds) via the emitted `call_table_(..., memw(<seg>(), ...))`.
//!   collapsed: (none — no per-AH DOS or RCB families in this file)
//!   deleted:   block_order__comment_node_precedes_instruction (renderer
//!              comment-node machinery: AstNode::Comment + node.render() lines —
//!              C structuring internals with no Rust equivalent),
//!              cfg_shared_postdom__extract_shared_postdom_keeps_original_block
//!              (extract_shared_postdom_blocks/DiGraph postdominator internals
//!              of the deleted structured renderer),
//!              ir_to_c_cs_negative__offset_uses_named_table (already an
//!              #[ignore]d stub: the C renderer's _MEMORY_FIELD_MAP named-table
//!              substitution was never ported and has no Rust-backend
//!              equivalent).
//!   unchanged: cfg_indirect_jump__indirect_jump_ignored,
//!              cfg_int_terminate__no_fallthrough_after_int_20,
//!              cfg_int_terminate__no_fallthrough_after_mov_ax_4c00_int_21,
//!              cfg_int_terminate__no_fallthrough_after_mov_ah_4c_int_21
//!              — front-half (normalize_indirect_jumps/build_basic_blocks);
//!              only cfg::build_cfg().out_degree() re-expressed via
//!              translate::cfg_successors.
mod common;
use common::*;
use saisei_jitc::translate::{self, Insn};
use serde_json::{json, Value};
use std::collections::BTreeSet;

/// Convert a JSON array of instruction objects into `Vec<Insn>` (the Rust
/// equivalent of a the original list of instruction dicts).
fn insns(v: Value) -> Vec<Insn> {
    v.as_array()
        .expect("instruction list must be a JSON array")
        .iter()
        .map(|i| {
            i.as_object()
                .expect("instruction must be an object")
                .clone()
        })
        .collect()
}

// ---------------------------------------------------------------------------
// ---------------------------------------------------------------------------

#[test]
fn and_jcc_preserved__and_followed_by_lodsb_and_jcc() {
    let func = json!({
        "start": 0x0000,
        "instructions": [
            {"address": 0x0000, "mnemonic": "and", "op_str": "al, 0x5f", "bytes": "245f"},
            {"address": 0x0002, "mnemonic": "lodsb", "op_str": "", "bytes": "AC"},
            {"address": 0x0003, "mnemonic": "je", "op_str": "0008", "target": 0x0008, "bytes": "7403"},
            {"address": 0x0005, "mnemonic": "ret", "op_str": "", "bytes": "C3"},
            {"address": 0x0008, "mnemonic": "ret", "op_str": "", "bytes": "C3"},
        ],
    });
    let src = render_rs(&func, &[], "");
    // and al, 0x5f: the masked result is written back to al...
    assert!(
        src.contains("let tmp: u8 = (((r.al()) as u32 & (0x5F) as u32) & 0xFF) as u8;"),
        "{src}"
    );
    assert!(src.contains("r.set_al(tmp);"), "{src}");
    // ...and the je still branches on its ZF (lodsb between is flag-neutral)
    assert!(src.contains("if r.ZF() == 1 {"), "{src}");
    assert!(src.contains("return 0x0008;"), "{src}");
}

// ---------------------------------------------------------------------------
// ---------------------------------------------------------------------------

#[test]
fn basic_block_comment__omitted_when_instructions_handled() {
    let func = json!({
        "start": 0x0000,
        "instructions": [
            {"address": 0x0000, "mnemonic": "ret", "op_str": "", "bytes": "C3"},
        ],
    });
    let src = render_rs(&func, &[], "");
    // a fully-handled block carries no fallback comment — the ret lowers to
    // the popped-ip epilogue
    assert!(!src.contains("// Basic block"), "{src}");
    assert!(
        src.contains("let popped_ip = r.memw(r.ss(), r.sp());"),
        "{src}"
    );
}

// An instruction the translator cannot emit does NOT fail the chunk: decoding is
// speculative (a packed game's CFG runs straight into its own ciphertext, which
// its stub rewrites into real code before ever jumping there), so refusing the
// chunk would throw away the runnable code next to it — including the code that
// does the decrypting. It becomes a trap instead, so the gap is paid only if
// control actually arrives, and it is still named, still fatal, still reported.
#[test]
fn basic_block_comment__unhandled_instruction_traps_at_run_time() {
    let func = json!({
        "start": 0x0000,
        "instructions": [
            {"address": 0x0000, "mnemonic": "foo", "op_str": "", "bytes": "90"},
            {"address": 0x0001, "mnemonic": "ret", "op_str": "", "bytes": "C3"},
        ],
    });
    let src = render_rs(&func, &[], "");
    // The IP is set before it, so the crash names the exact instruction, and the
    // block ends there — nothing after an instruction we cannot model may run.
    assert!(src.contains("r.set_ip(0x0000);"), "{src}");
    assert!(
        src.contains(r#"r.jit_unsupported_instruction(c"mnemonic:foo".as_ptr());"#),
        "{src}"
    );
    assert!(src.contains("return -1;"), "{src}");
    // ...and the chunk still compiles: the `ret` after it is unreachable, but the
    // rest of the chunk (other functions, other blocks) is intact.
    assert!(src.contains("_dispatch"), "{src}");
}

/// The gaps stay visible. They are no longer an Err, so anything that wants to
/// see the frontier (jit-compile's log, the gap_sweep test) asks for them.
#[test]
fn unsupported_constructs_are_reported_even_though_the_chunk_compiles() {
    let ir = json!({
        "functions": [{
            "start": 0x0000,
            "instructions": [
                {"address": 0x0000, "mnemonic": "foo", "op_str": "", "bytes": "90"},
                {"address": 0x0001, "mnemonic": "ret", "op_str": "", "bytes": "C3"},
            ],
        }],
    });
    let (src, gaps) = saisei_jitc::codegen::emit_chunk_gaps(&ir, "t_", Some(0x10100), "rt.rs")
        .expect("chunk still compiles");
    assert!(src.contains("jit_unsupported_instruction"), "{src}");
    assert_eq!(gaps, vec!["mnemonic:foo".to_string()]);
}

// ---------------------------------------------------------------------------
// ---------------------------------------------------------------------------

#[test]
fn block_order__blocks_follow_cfg_traversal_order() {
    // C-structural traversal-order/AstNode assertions dropped; what survives is
    // the transfer semantics: entry jumps forward to 0x100, which jumps back to
    // the ret block at 0x2 — each an explicit next-pc edge in its own block fn.
    let func = json!({
        "start": 0x0,
        "instructions": [
            {"address": 0x0, "mnemonic": "jmp", "bytes": "00", "op_str": "0x100", "target": 0x100},
            {"address": 0x2, "mnemonic": "ret", "bytes": "00", "op_str": ""},
            {"address": 0x100, "mnemonic": "jmp", "bytes": "00", "op_str": "0x2", "target": 0x2},
        ],
    });
    let src = render_rs(&func, &[], "");
    let entry = blk(&src, 0x0000);
    assert!(entry.contains("return 0x0100;"), "{entry}");
    let via = blk(&src, 0x0100);
    assert!(via.contains("return 0x0002;"), "{via}");
    let out = blk(&src, 0x0002);
    assert!(
        out.contains("let popped_ip = r.memw(r.ss(), r.sp());"),
        "{out}"
    );
}

// ---------------------------------------------------------------------------
// ---------------------------------------------------------------------------

#[test]
fn cfg_indirect_jump__indirect_jump_ignored() {
    let instrs = insns(json!([
        {"address": 0x0, "mnemonic": "jmp", "bytes": "00", "op_str": "word ptr cs:[bx + 0x588]"},
        {"address": 0x100, "mnemonic": "nop", "bytes": "00", "op_str": ""},
        {"address": 0x588, "mnemonic": "ret", "bytes": "00", "op_str": ""},
    ]));
    let instrs = translate::normalize_indirect_jumps(&instrs);
    let blocks = translate::build_basic_blocks(&instrs, &BTreeSet::new(), None);
    let keys: Vec<i64> = blocks.keys().copied().collect();
    assert_eq!(keys, vec![0x0]);
    let op = blocks[&0x0].instructions[0]
        .get("op")
        .and_then(Value::as_str);
    assert_eq!(op, Some("INDIRECT_NEAR_JMP"));
    let succ = translate::cfg_successors(&blocks);
    assert_eq!(succ.get(&0x0).map_or(0, |v| v.len()), 0);
}

// ---------------------------------------------------------------------------
// ---------------------------------------------------------------------------

#[test]
fn cfg_int_terminate__no_fallthrough_after_int_20() {
    let instrs = insns(json!([
        {"address": 0x0, "mnemonic": "int", "op_str": "0x20", "bytes": "CD20"},
        {"address": 0x2, "mnemonic": "nop", "op_str": "", "bytes": "90"},
    ]));
    let blocks = translate::build_basic_blocks(&instrs, &known(&[0x2]), None);
    let succ = translate::cfg_successors(&blocks);
    assert_eq!(succ.get(&0x0).map_or(0, |v| v.len()), 0);
}

#[test]
fn cfg_int_terminate__no_fallthrough_after_mov_ax_4c00_int_21() {
    let instrs = insns(json!([
        {"address": 0x0, "mnemonic": "mov", "op_str": "ax, 0x4c00", "bytes": "B8004C"},
        {"address": 0x3, "mnemonic": "int", "op_str": "0x21", "bytes": "CD21"},
        {"address": 0x5, "mnemonic": "nop", "op_str": "", "bytes": "90"},
    ]));
    let blocks = translate::build_basic_blocks(&instrs, &known(&[0x5]), None);
    let succ = translate::cfg_successors(&blocks);
    assert_eq!(succ.get(&0x0).map_or(0, |v| v.len()), 0);
}

#[test]
fn cfg_int_terminate__no_fallthrough_after_mov_ah_4c_int_21() {
    let instrs = insns(json!([
        {"address": 0x0, "mnemonic": "mov", "op_str": "ah, 0x4c", "bytes": "B44C"},
        {"address": 0x2, "mnemonic": "int", "op_str": "0x21", "bytes": "CD21"},
        {"address": 0x4, "mnemonic": "nop", "op_str": "", "bytes": "90"},
    ]));
    let blocks = translate::build_basic_blocks(&instrs, &known(&[0x4]), None);
    let succ = translate::cfg_successors(&blocks);
    assert_eq!(succ.get(&0x0).map_or(0, |v| v.len()), 0);
}

// ---------------------------------------------------------------------------
// ---------------------------------------------------------------------------

#[test]
fn if_merge_target__if_with_merge_as_target_preserves_merge_block() {
    let func = json!({
        "start": 0x0000,
        "instructions": [
            {"address": 0x0000, "mnemonic": "cmp", "op_str": "cx, 0xf", "bytes": "83f90f"},
            {"address": 0x0003, "mnemonic": "jb", "op_str": "0x0008", "target": 0x0008, "bytes": "7203"},
            {"address": 0x0005, "mnemonic": "mov", "op_str": "cx, 0xf", "bytes": "b90f00"},
            {"address": 0x0008, "mnemonic": "mov", "op_str": "di, 0x88b", "bytes": "bf8b08"},
            {"address": 0x000b, "mnemonic": "ret", "op_str": "", "bytes": "c3"},
        ],
    });
    let src = render_rs(&func, &[], "");
    // jb branches on CF; both edges are explicit
    assert!(src.contains("if r.CF() == 1 {"), "{src}");
    assert!(src.contains("return 0x0008;"), "{src}");
    assert!(src.contains("return 0x0005;"), "{src}");
    // the not-taken block holds the clamp, the merge block keeps the di write
    let clamp = blk(&src, 0x0005);
    assert!(clamp.contains("r.set_cx(0xF);"), "{clamp}");
    let merge = blk(&src, 0x0008);
    assert!(merge.contains("r.set_di(0x88B);"), "{merge}");
    assert!(
        src.find("if r.CF() == 1").unwrap() < src.find("r.set_di(0x88B);").unwrap(),
        "{src}"
    );
}

// ---------------------------------------------------------------------------
// ---------------------------------------------------------------------------

#[test]
fn interrupt_flag_clobber__int_clobbers_previous_cmp_flag() {
    // The jb after the DOS call must branch on the live CF the call produced,
    // not on a precomputed high-level `al < 1` condition from the earlier cmp.
    let func = json!({
        "start": 0x0000,
        "instructions": [
            {"address": 0x0000, "mnemonic": "cmp", "op_str": "al, 1", "bytes": "3C01"},
            {"address": 0x0002, "mnemonic": "mov", "op_str": "ah, 0x3d", "bytes": "B43D"},
            {"address": 0x0004, "mnemonic": "int", "op_str": "21", "bytes": "CD21"},
            {"address": 0x0006, "mnemonic": "jmp", "op_str": "000A", "target": 0x000A, "bytes": "E90300"},
            {"address": 0x000A, "mnemonic": "jb", "op_str": "000E", "target": 0x000E, "bytes": "7202"},
            {"address": 0x000C, "mnemonic": "ret", "op_str": "", "bytes": "C3"},
            {"address": 0x000E, "mnemonic": "ret", "op_str": "", "bytes": "C3"},
        ],
    });
    let src = render_rs(&func, &[], "");
    // int 21h lowers to the generic dos_api() (no per-AH specialization)
    assert!(src.contains("r.set_ah(0x3D);"), "{src}");
    assert!(src.contains("r.dos_api();"), "{src}");
    // the branch reads CF after the call, in a later arm
    assert!(src.contains("if r.CF() == 1 {"), "{src}");
    assert!(
        src.find("r.dos_api();").unwrap() < src.find("if r.CF() == 1").unwrap(),
        "{src}"
    );
}

#[test]
fn interrupt_flag_clobber__non_dos_int_does_not_preadvance_ip_before_run_interrupt() {
    let func = json!({
        "start": 0x0000,
        "instructions": [
            {"address": 0x0000, "mnemonic": "int", "op_str": "60", "bytes": "CD60"},
            {"address": 0x0002, "mnemonic": "ret", "op_str": "", "bytes": "C3"},
        ],
    });
    let src = render_rs(&func, &[], "");
    assert!(src.contains("r.run_interrupt(0x60);"), "{src}");
    // ip advances to the next instruction only AFTER the interrupt has run
    assert!(
        src.find("r.set_ip(0x0000);").unwrap() < src.find("r.run_interrupt(0x60);").unwrap(),
        "{src}"
    );
    assert!(
        src.find("r.run_interrupt(0x60);").unwrap() < src.find("r.set_ip(0x0002);").unwrap(),
        "{src}"
    );
}

// ---------------------------------------------------------------------------
// ---------------------------------------------------------------------------

#[test]
fn ir_to_c_aaa__translates_adjust() {
    let func = json!({
        "start": 0x0000,
        "instructions": [
            {"address": 0x0000, "mnemonic": "aaa", "op_str": "", "bytes": ""},
            {"address": 0x0001, "mnemonic": "ret", "op_str": "", "bytes": ""},
        ],
    });
    let src = render_rs(&func, &[], "");
    assert!(src.contains("let tmp: u8 = r.al();"), "{src}");
    assert!(!src.contains("(tmp & 0x10)"), "{src}");
    assert!(
        src.contains("r.set_al(tmp.wrapping_add(6) & 0x0F);"),
        "{src}"
    );
    assert!(
        src.contains("r.set_ah(r.ah().wrapping_add(1) & 0xFF);"),
        "{src}"
    );
    assert!(src.contains("r.set_CF(1);"), "{src}");
    assert!(src.contains("r.set_al(tmp & 0x0F);"), "{src}");
    assert!(src.contains("r.set_CF(0);"), "{src}");
}

// ---------------------------------------------------------------------------
// ---------------------------------------------------------------------------

#[test]
fn ir_to_c_adc__adds_with_carry() {
    let func = json!({
        "start": 0x0000,
        "instructions": [
            {"address": 0x0000, "mnemonic": "adc", "op_str": "ax, bx", "bytes": ""},
            {"address": 0x0002, "mnemonic": "ret", "op_str": "", "bytes": ""},
        ],
    });
    let src = render_rs(&func, &[], "");
    assert!(src.contains("let old: u32 = (r.ax()) as u32;"), "{src}");
    assert!(src.contains("let src: u32 = (r.bx()) as u32;"), "{src}");
    assert!(
        src.contains("let tmp: u32 = old.wrapping_add(src).wrapping_add(r.CF() as u32);"),
        "{src}"
    );
    assert!(src.contains("r.set_CF((tmp > 0xFFFF) as u8);"), "{src}");
    assert!(src.contains("r.set_ax((tmp & 0xFFFF) as u16);"), "{src}");
    assert!(
        src.contains("r.set_OF(((!(old ^ src) & (old ^ tmp) & 0x8000) != 0) as u8);"),
        "{src}"
    );
}

// ---------------------------------------------------------------------------
// ---------------------------------------------------------------------------

#[test]
fn ir_to_c_add_cf__add_sets_cf_flag() {
    // The C do-while shape (`while (CF == 1)`) is gone; the backward jb is a
    // CF-taken back edge to the block start.
    let func = json!({
        "start": 0x0000,
        "instructions": [
            {"address": 0x0000, "mnemonic": "add", "op_str": "al, al", "bytes": "00C0"},
            {"address": 0x0002, "mnemonic": "jb", "op_str": "0000", "target": 0x0000, "bytes": "7200"},
            {"address": 0x0004, "mnemonic": "ret", "op_str": "", "bytes": "C3"},
        ],
    });
    let src = render_rs(&func, &[], "");
    assert!(src.contains("let old: u32 = (r.al()) as u32;"), "{src}");
    assert!(src.contains("let src: u32 = (r.al()) as u32;"), "{src}");
    assert!(
        src.contains("let tmp: u32 = old.wrapping_add(src);"),
        "{src}"
    );
    assert!(src.contains("r.set_CF((tmp > 0xFF) as u8);"), "{src}");
    assert!(src.contains("if r.CF() == 1 {"), "{src}");
    assert!(src.contains("return 0x0000;"), "{src}");
}

// ---------------------------------------------------------------------------
// ---------------------------------------------------------------------------

#[test]
fn ir_to_c_call_table__call_word_ptr_cs_uses_call_table() {
    let func = json!({
        "start": 0x0000,
        "instructions": [
            {
                "address": 0x0000,
                "mnemonic": "call",
                "op_str": "word ptr cs:[0x10c]",
                "bytes": "FF1E0C01",
                "detail": {"mem_refs": [{"segment": "CS", "disp": 0x10C, "access": "read"}]},
            },
            {"address": 0x0005, "mnemonic": "ret", "op_str": "", "bytes": "C3"},
        ],
    });
    let src = render_rs(&func, &[], "");
    assert!(
        src.contains("r.call_table_(((0x5u32).wrapping_add(0x10100).wrapping_sub((r.cs() as u32) << 4)) as u16, (((r.cs() as u32) << 4).wrapping_add((r.memw(r.cs(), 0x10C)) as u32)) & 0xFFFFF);"),
        "{src}"
    );
}

#[test]
fn ir_to_c_call_table__call_word_ptr_cs_with_bp_index_uses_call_table() {
    let func = json!({
        "start": 0x0000,
        "instructions": [
            {
                "address": 0x0000,
                "mnemonic": "call",
                "op_str": "word ptr cs:[bp + 0x10c]",
                "bytes": "FF9E0C01",
                "detail": {"mem_refs": [{"segment": "CS", "disp": 0x10C, "access": "read"}]},
            },
            {"address": 0x0005, "mnemonic": "ret", "op_str": "", "bytes": "C3"},
        ],
    });
    let src = render_rs(&func, &[], "");
    assert!(
        src.contains("r.call_table_(((0x5u32).wrapping_add(0x10100).wrapping_sub((r.cs() as u32) << 4)) as u16, (((r.cs() as u32) << 4).wrapping_add((r.memw(r.cs(), (((r.bp() as u32).wrapping_add(0x10Cu32)) & 0xFFFF) as u16)) as u32)) & 0xFFFFF);"),
        "{src}"
    );
}

#[test]
fn ir_to_c_call_table__call_word_ptr_without_segment_uses_cs_for_call_table() {
    let func = json!({
        "start": 0x0000,
        "instructions": [
            {
                "address": 0x0000,
                "mnemonic": "call",
                "op_str": "word ptr [0x010C]",
                "bytes": "FF160C01",
                "detail": {"mem_refs": [{"segment": "DS", "disp": 0x10C, "access": "read"}]},
            },
            {"address": 0x0005, "mnemonic": "ret", "op_str": "", "bytes": "C3"},
        ],
    });
    let src = render_rs(&func, &[], "");
    // the pointer is read from ds; the linear target is still cs-based
    assert!(
        src.contains("r.call_table_(((0x5u32).wrapping_add(0x10100).wrapping_sub((r.cs() as u32) << 4)) as u16, (((r.cs() as u32) << 4).wrapping_add((r.memw(r.ds(), 0x10C)) as u32)) & 0xFFFFF);"),
        "{src}"
    );
}

#[test]
fn ir_to_c_call_table__call_word_ptr_bp_defaults_to_ss_for_mem_and_cs_for_call_table() {
    let func = json!({
        "start": 0x0000,
        "instructions": [
            {
                "address": 0x0000,
                "mnemonic": "call",
                "op_str": "word ptr [bp + 0x10c]",
                "bytes": "FF960C01",
                "detail": {"mem_refs": [{"segment": "SS", "disp": 0x10C, "access": "read"}]},
            },
            {"address": 0x0005, "mnemonic": "ret", "op_str": "", "bytes": "C3"},
        ],
    });
    let src = render_rs(&func, &[], "");
    // [bp + …] defaults the mem read to ss; the linear target stays cs-based
    assert!(
        src.contains("r.call_table_(((0x5u32).wrapping_add(0x10100).wrapping_sub((r.cs() as u32) << 4)) as u16, (((r.cs() as u32) << 4).wrapping_add((r.memw(r.ss(), (((r.bp() as u32).wrapping_add(0x10Cu32)) & 0xFFFF) as u16)) as u32)) & 0xFFFFF);"),
        "{src}"
    );
}

#[test]
fn ir_to_c_call_table__call_register_uses_call_table() {
    let func = json!({
        "start": 0x0000,
        "instructions": [
            {"address": 0x0000, "mnemonic": "call", "op_str": "ax", "bytes": "FFD0"},
            {"address": 0x0002, "mnemonic": "ret", "op_str": "", "bytes": "C3"},
        ],
    });
    let src = render_rs(&func, &[], "");
    assert!(
        src.contains("r.call_table_(((0x2u32).wrapping_add(0x10100).wrapping_sub((r.cs() as u32) << 4)) as u16, (((r.cs() as u32) << 4).wrapping_add(r.ax() as u32)) & 0xFFFFF);"),
        "{src}"
    );
}

// ---------------------------------------------------------------------------
// ---------------------------------------------------------------------------

#[test]
fn ir_to_c_cf_return__jnc_to_ret_not_negated() {
    // Direct calls render as the intra-chunk push-ret + pc transfer, which
    // requires the target in known_addrs (the original: name_prefix="app_").
    let func = json!({
        "start": 0x0000,
        "instructions": [
            {"address": 0x0000, "mnemonic": "call", "op_str": "0x0010", "bytes": "E81000", "target": 0x0010},
            {"address": 0x0003, "mnemonic": "jnc", "op_str": "0x0008", "target": 0x0008, "bytes": "7303"},
            {"address": 0x0005, "mnemonic": "mov", "op_str": "ax, bx", "bytes": "89D8"},
            {"address": 0x0007, "mnemonic": "ret", "op_str": "", "bytes": "C3"},
            {"address": 0x0008, "mnemonic": "ret", "op_str": "", "bytes": "C3"},
        ],
    });
    let src = render_rs(&func, &[0x0010], "app_");
    assert!(src.contains("if r.CF() == 0 {"), "{src}");
    assert!(src.contains("return 0x0008;"), "{src}");
}

// ---------------------------------------------------------------------------
// ---------------------------------------------------------------------------

#[test]
fn ir_to_c_cli_sti__translation() {
    let func = json!({
        "start": 0,
        "instructions": [
            {"address": 0, "mnemonic": "cli", "op_str": "", "bytes": "FA"},
            {"address": 1, "mnemonic": "sti", "op_str": "", "bytes": "FB"},
            {"address": 2, "mnemonic": "ret", "op_str": "", "bytes": "C3"},
        ],
    });
    let src = render_rs(&func, &[], "");
    assert!(src.contains("r.set_IF(0);"), "{src}");
    assert!(src.contains("r.set_IF(1);"), "{src}");
    assert!(src.contains("set_interrupt_shadow(1);"), "{src}");
    // One safepoint poll per basic block (debiting the block's summed
    // per-class weights: cli 1 + sti 1 + ret 3), not one per instruction.
    assert_eq!(src.matches("r.budget(5);").count(), 1, "{src}");
    assert_eq!(src.matches("SAFEPOINT();").count(), 0, "{src}");
}

// ---------------------------------------------------------------------------
// ---------------------------------------------------------------------------

#[test]
fn ir_to_c_cmp_cf__cmp_sets_cf_before_subtraction() {
    let func = json!({
        "start": 0x0000,
        "instructions": [
            {"address": 0x0000, "mnemonic": "cmp", "op_str": "ax, bx", "bytes": ""},
            {"address": 0x0002, "mnemonic": "ret", "op_str": "", "bytes": ""},
        ],
    });
    let src = render_rs(&func, &[], "");
    let cf_index = src
        .find("r.set_CF((left_val < right_val) as u8);")
        .expect("CF line missing");
    let tmp_index = src
        .find("let tmp = left_val.wrapping_sub(right_val);")
        .expect("tmp line missing");
    assert!(cf_index < tmp_index, "{src}");
}

// ---------------------------------------------------------------------------
// ---------------------------------------------------------------------------

#[test]
fn ir_to_c_cmpsb__compares_memory_and_increments_si_di() {
    let func = json!({
        "start": 0x0000,
        "instructions": [
            {"address": 0x0000, "mnemonic": "cmpsb", "op_str": "byte ptr [si], byte ptr es:[di]", "bytes": "A6"},
            {"address": 0x0001, "mnemonic": "ret", "op_str": "", "bytes": "C3"},
        ],
    });
    let src = render_rs(&func, &[], "");
    let cf_index = src
        .find("r.set_CF((left_val < right_val) as u8);")
        .expect("CF line missing");
    let tmp_index = src
        .find("let tmp = left_val.wrapping_sub(right_val);")
        .expect("tmp line missing");
    assert!(cf_index < tmp_index, "{src}");
    assert!(
        src.contains("let left_val: u32 = r.memb(r.ds(), r.si()) as u32;"),
        "{src}"
    );
    assert!(
        src.contains("let right_val: u32 = r.memb(r.es(), r.di()) as u32;"),
        "{src}"
    );
    assert!(
        src.contains("r.set_si(((r.si() as i32 + delta) & 0xFFFF) as u16);"),
        "{src}"
    );
    assert!(
        src.contains("r.set_di(((r.di() as i32 + delta) & 0xFFFF) as u16);"),
        "{src}"
    );
}

#[test]
fn ir_to_c_cmpsb__respects_source_segment_override() {
    let func = json!({
        "start": 0x0000,
        "instructions": [
            {
                "address": 0x0000,
                "mnemonic": "cmpsb",
                "op_str": "byte ptr [si], byte ptr es:[di]",
                "bytes": "2EA6",
                "detail": {
                    "mem_refs": [
                        {"segment": "CS", "disp": 0, "access": "read"},
                        {"segment": "ES", "disp": 0, "access": "read"},
                    ]
                },
            },
            {"address": 0x0002, "mnemonic": "ret", "op_str": "", "bytes": "C3"},
        ],
    });
    let src = render_rs(&func, &[], "");
    assert!(
        src.contains("let left_val: u32 = r.memb(r.cs(), r.si()) as u32;"),
        "{src}"
    );
}

// ---------------------------------------------------------------------------
// ---------------------------------------------------------------------------

#[test]
fn ir_to_c_cwde_stc__cwde_sign_extends_al_into_ax() {
    let func = json!({
        "start": 0x0000,
        "instructions": [
            {"address": 0x0000, "mnemonic": "mov", "op_str": "al, 0x80", "bytes": "B080"},
            {"address": 0x0002, "mnemonic": "cwde", "op_str": "", "bytes": "98"},
            {"address": 0x0003, "mnemonic": "ret", "op_str": "", "bytes": "C3"},
        ],
    });
    let src = render_rs(&func, &[], "");
    assert!(src.contains("r.set_al(0x80);"), "{src}");
    assert!(
        src.contains("r.set_ax(((r.al() as i8) as i16) as u16);"),
        "{src}"
    );
}

#[test]
fn ir_to_c_cwde_stc__stc_sets_carry_flag() {
    let func = json!({
        "start": 0x0000,
        "instructions": [
            {"address": 0x0000, "mnemonic": "stc", "op_str": "", "bytes": "F9"},
            {"address": 0x0001, "mnemonic": "ret", "op_str": "", "bytes": "C3"},
        ],
    });
    let src = render_rs(&func, &[], "");
    assert!(src.contains("r.set_CF(1);"), "{src}");
}

#[test]
fn ir_to_c_cwde_stc__clc_clears_carry_flag() {
    let func = json!({
        "start": 0x0000,
        "instructions": [
            {"address": 0x0000, "mnemonic": "clc", "op_str": "", "bytes": "F8"},
            {"address": 0x0001, "mnemonic": "ret", "op_str": "", "bytes": "C3"},
        ],
    });
    let src = render_rs(&func, &[], "");
    assert!(src.contains("r.set_CF(0);"), "{src}");
}

#[test]
fn ir_to_c_cwde_stc__cmc_complements_carry_flag() {
    let func = json!({
        "start": 0x0000,
        "instructions": [
            {"address": 0x0000, "mnemonic": "stc", "op_str": "", "bytes": "F9"},
            {"address": 0x0001, "mnemonic": "cmc", "op_str": "", "bytes": "F5"},
            {"address": 0x0002, "mnemonic": "ret", "op_str": "", "bytes": "C3"},
        ],
    });
    let src = render_rs(&func, &[], "");
    assert!(src.contains("r.set_CF(r.CF() ^ 1);"), "{src}");
}

#[test]
fn ir_to_c_cwde_stc__cld_clears_direction_flag() {
    let func = json!({
        "start": 0x0000,
        "instructions": [
            {"address": 0x0000, "mnemonic": "cld", "op_str": "", "bytes": "FC"},
            {"address": 0x0001, "mnemonic": "ret", "op_str": "", "bytes": "C3"},
        ],
    });
    let src = render_rs(&func, &[], "");
    assert!(src.contains("r.set_DF(0);"), "{src}");
}

#[test]
fn ir_to_c_cwde_stc__std_sets_direction_flag() {
    let func = json!({
        "start": 0x0000,
        "instructions": [
            {"address": 0x0000, "mnemonic": "std", "op_str": "", "bytes": "FD"},
            {"address": 0x0001, "mnemonic": "ret", "op_str": "", "bytes": "C3"},
        ],
    });
    let src = render_rs(&func, &[], "");
    assert!(src.contains("r.set_DF(1);"), "{src}");
}

// ---------------------------------------------------------------------------
// ---------------------------------------------------------------------------

#[test]
fn ir_to_c_default_ss__bp_relative_defaults_to_ss() {
    let func = json!({
        "start": 0x0000,
        "instructions": [
            {"address": 0x0000, "mnemonic": "mov", "op_str": "ax, word ptr [bp + 4]", "bytes": ""},
            {"address": 0x0003, "mnemonic": "ret", "op_str": "", "bytes": ""},
        ],
    });
    let src = render_rs(&func, &[0x0000], "");
    assert!(
        src.contains(
            "r.set_ax(r.memw(r.ss(), (((r.bp() as u32).wrapping_add(0x4u32)) & 0xFFFF) as u16));"
        ),
        "{src}"
    );
}
