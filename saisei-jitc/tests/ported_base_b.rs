#![allow(non_snake_case)]
//! Ported from tests/test_*.py — df_steps, dispatch_helper, distinct_mem_refs,
//! div, dos_api, dx_in_branch, empty_header_loop, enter_leave,
//! extra_instructions, flag_reset, flag_setting, for_loop, inc_dec_flags,
//! inc_dec_jcc_memory, io, iret, jcc_memory_operand.
//!
//! PORT DISPOSITIONS (C backend deleted):
//!   ported:    38 tests — C-text assertions rewritten against the Rust chunk
//!              backend (`render_rs`); purely C-structural assertions
//!              (for/while/do-while shape, if-body nesting, extra_func_blocks,
//!              renderer() pokes) dropped from otherwise-ported tests:
//!              dispatch_helper__preserves_multiple_statements (semantic core
//!              kept: 0x0200 arm + mov/inc bodies; extra_func_blocks machinery
//!              dropped), dx_in_branch (if-nesting -> arm containment),
//!              empty_header_loop (while(1) -> pc back-edges),
//!              flag_setting__* (do-while -> CF write + branch + back-edge),
//!              for_loop__* (for(...) headers -> arithmetic blocks, branch
//!              conditions, pc back-edges).
//!   collapsed: dos_api__dos_write_char_passes_argument,
//!              dos_api__dos_close_file_passes_bx,
//!              dos_api__dos_exit_preserves_ah_after_al_write
//!              — the Rust backend does not specialize int 21h per AH (every
//!              int 21h emits `dos_api();`, registers are written immediately
//!              beforehand); family represented by the two kept tests
//!              (dos_direct_console_io_uses_dl, dos_string_pointer_...).
//!   deleted:   (none)
//!   unchanged: distinct_mem_refs__do_not_clobber_condition,
//!              flag_reset__readwrite_mem_access_clobbers_cmp_before_jcc
//!              — they test translate::normalize_flags directly (the shared
//!              front-half), not C text.
mod common;
use common::*;
use saisei_jitc::translate;
use serde_json::{json, Value};

/// Wrap a single instruction (at 0x0000) followed by a ret at 0x0010 — mirrors
fn wrap(mnemonic: &str, op_str: &str, bytes: &str) -> Value {
    json!({
        "start": 0x0000,
        "instructions": [
            {"address": 0x0000, "mnemonic": mnemonic, "op_str": op_str, "bytes": bytes},
            {"address": 0x0010, "mnemonic": "ret", "op_str": "", "bytes": "C3"},
        ],
    })
}

/// Convert a json object Value into an `Insn` (serde_json Map) for normalize_flags.
fn to_insn(v: Value) -> translate::Insn {
    v.as_object().expect("insn must be a JSON object").clone()
}

// ---------------------------------------------------------------------------
// ---------------------------------------------------------------------------

#[test]
fn df_steps__lodsb_decrements_si_when_df_set() {
    let func = json!({
        "start": 0x0000,
        "instructions": [
            {"address": 0x0000, "mnemonic": "std", "op_str": "", "bytes": "FD"},
            {"address": 0x0001, "mnemonic": "lodsb", "op_str": "", "bytes": "AC"},
            {"address": 0x0002, "mnemonic": "ret", "op_str": "", "bytes": "C3"},
        ],
    });
    let src = render_rs(&func, &[], "");
    assert!(src.contains("r.set_DF(1);"), "{src}");
    assert!(
        src.contains("let delta: i32 = if r.DF() != 0 { -1 } else { 1 };"),
        "{src}"
    );
    assert!(
        src.contains("r.set_si(((r.si() as i32 + delta) & 0xFFFF) as u16);"),
        "{src}"
    );
}

#[test]
fn df_steps__movsb_decrements_regs_when_df_set() {
    let func = json!({
        "start": 0x0000,
        "instructions": [
            {"address": 0x0000, "mnemonic": "std", "op_str": "", "bytes": "FD"},
            {"address": 0x0001, "mnemonic": "movsb",
             "op_str": "byte ptr es:[di], byte ptr [si]", "bytes": "A4"},
            {"address": 0x0002, "mnemonic": "ret", "op_str": "", "bytes": "C3"},
        ],
    });
    let src = render_rs(&func, &[], "");
    assert!(src.contains("r.set_DF(1);"), "{src}");
    assert!(
        src.contains("let delta: i32 = if r.DF() != 0 { -1 } else { 1 };"),
        "{src}"
    );
    assert!(
        src.contains("r.memb_write(r.es(), r.di(), r.memb(r.ds(), r.si()));"),
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
fn df_steps__rep_movsw_decrements_regs_when_df_set() {
    let func = json!({
        "start": 0x0000,
        "instructions": [
            {"address": 0x0000, "mnemonic": "std", "op_str": "", "bytes": "FD"},
            {"address": 0x0001, "mnemonic": "rep movsw",
             "op_str": "word ptr es:[di], word ptr [si]", "bytes": "F3A5"},
            {"address": 0x0003, "mnemonic": "ret", "op_str": "", "bytes": "C3"},
        ],
    });
    let src = render_rs(&func, &[], "");
    // std still sets DF; the rep movsw itself is emitted as a call to the
    // rep_movsw_block shim, which reads DF for its step direction.
    assert!(src.contains("r.set_DF(1);"), "{src}");
    assert!(src.contains("r.rep_movsw_block(r.es(), r.ds());"), "{src}");
}

// ---------------------------------------------------------------------------
// ---------------------------------------------------------------------------

#[test]
fn dispatch_helper__preserves_multiple_statements() {
    // The jmp hands control to the 0x0200 block; all statements of that block
    // (mov, the full inc flag block, the near-ret epilogue) live in its arm.
    let func = json!({
        "start": 0x0100,
        "instructions": [
            {"address": 0x0100, "mnemonic": "jmp", "op_str": "0200",
             "target": 0x0200, "bytes": "E9FD01"},
            {"address": 0x0200, "mnemonic": "mov", "op_str": "ax, bx", "bytes": "89D8"},
            {"address": 0x0203, "mnemonic": "inc", "op_str": "ax", "bytes": "40"},
            {"address": 0x0204, "mnemonic": "ret", "op_str": "", "bytes": "C3"},
        ],
    });
    let src = render_rs(&func, &[], "");
    // the entry block transfers to 0x0200 (yields it as the next pc)
    let entry = blk(&src, 0x0100);
    assert!(entry.contains("return 0x0200;"), "{entry}");
    let body = blk(&src, 0x0200);
    assert!(body.contains("r.set_ax(r.bx());"), "{body}");
    // the inc-ax flag block is preserved in full
    assert!(body.contains("let old: u32 = (r.ax()) as u32;"), "{body}");
    assert!(
        body.contains("r.set_ax((old.wrapping_add(1) & 0xFFFF) as u16);"),
        "{body}"
    );
    assert!(body.contains("r.set_OF((old == 0x7FFF) as u8);"), "{body}");
    // the ret lowers to the popped-ip epilogue inside the same arm
    assert!(
        body.contains("let popped_ip = r.memw(r.ss(), r.sp());"),
        "{body}"
    );
}

// ---------------------------------------------------------------------------
// ---------------------------------------------------------------------------

#[test]
fn distinct_mem_refs__do_not_clobber_condition() {
    let instrs = vec![
        to_insn(json!({
            "address": 0,
            "mnemonic": "cmp",
            "op_str": "byte ptr [bx], 0",
            "bytes": "",
            "detail": {
                "regs_read": ["BX"],
                "regs_write": [],
                "mem_refs": [
                    {"segment": "DS", "base": "BX", "index": null, "scale": 1,
                     "disp": 0, "access": "read"}
                ],
            },
        })),
        to_insn(json!({
            "address": 1,
            "mnemonic": "mov",
            "op_str": "byte ptr [si], 1",
            "bytes": "",
            "detail": {
                "regs_read": ["SI"],
                "regs_write": [],
                "mem_refs": [
                    {"segment": "DS", "base": "SI", "index": null, "scale": 1,
                     "disp": 0, "access": "write"}
                ],
            },
        })),
        to_insn(json!({
            "address": 2,
            "mnemonic": "je",
            "op_str": "0005",
            "bytes": "",
        })),
    ];
    let result = translate::normalize_flags(&instrs);
    assert_eq!(
        result[2]
            .get("cond_prev")
            .unwrap()
            .get("mnemonic")
            .and_then(Value::as_str),
        Some("cmp")
    );
}

// ---------------------------------------------------------------------------
// ---------------------------------------------------------------------------

#[test]
fn div__div_cl_generates_code_without_todo() {
    let func = json!({
        "start": 0x0000,
        "instructions": [
            {"address": 0x0000, "mnemonic": "div", "op_str": "cl", "bytes": "F6F1"},
            {"address": 0x0002, "mnemonic": "ret", "op_str": "", "bytes": "C3"},
        ],
    });
    let src = render_rs(&func, &[], "");
    assert!(src.contains("let tmp: u16 = r.ax();"), "{src}");
    assert!(src.contains("let divisor = (r.cl()) as u16;"), "{src}");
    assert!(
        src.contains("r.set_al((q & 0xFF) as u8); r.set_ah(((tmp % divisor) & 0xFF) as u8);"),
        "{src}"
    );
    // #DE on zero divisor / quotient overflow — never a truncated result
    assert!(src.contains("r.run_interrupt(0x00);"), "{src}");
    // div writes no defined flags
    assert!(!src.contains("set_ZF"), "{src}");
}

#[test]
fn div__div_cx_generates_quotient_and_remainder_without_flag_writes() {
    let func = json!({
        "start": 0x0000,
        "instructions": [
            {"address": 0x0000, "mnemonic": "div", "op_str": "cx", "bytes": "F7F1",
             "detail": {"regs_write": ["AX", "DX"]}},
            {"address": 0x0002, "mnemonic": "ret", "op_str": "", "bytes": "C3"},
        ],
    });
    let src = render_rs(&func, &[], "");
    assert!(
        src.contains("let tmp: u32 = ((r.dx() as u32) << 16) | (r.ax() as u32);"),
        "{src}"
    );
    assert!(
        src.contains("r.set_ax((q & 0xFFFF) as u16); r.set_dx(((tmp % divisor) & 0xFFFF) as u16);"),
        "{src}"
    );
    assert!(!src.contains("set_ZF"), "{src}");
}

#[test]
fn div__idiv_cx_does_not_emit_flag_updates() {
    let func = json!({
        "start": 0x0000,
        "instructions": [
            {"address": 0x0000, "mnemonic": "idiv", "op_str": "cx", "bytes": "F7F9",
             "detail": {"regs_write": ["AX", "DX"]}},
            {"address": 0x0002, "mnemonic": "ret", "op_str": "", "bytes": "C3"},
        ],
    });
    let src = render_rs(&func, &[], "");
    assert!(
        src.contains("let dividend: i32 = ((r.dx() as i16 as i32) << 16) | (r.ax() as i32);"),
        "{src}"
    );
    assert!(
        src.contains("r.set_ax(q as u16); r.set_dx((dividend % divisor) as u16);"),
        "{src}"
    );
    assert!(!src.contains("set_ZF"), "{src}");
}

// ---------------------------------------------------------------------------
// The Rust backend does not specialize int 21h per AH — every int 21h emits
// `dos_api();` after the register writes have already executed (Rust movs
// write cpu state immediately). The per-AH C tests (dos_write_char,
// dos_close_file, dos_exit-via-4C) collapse into the two representatives
// below, which pin (a) register args are written before the call and (b) the
// call itself is emitted.
// ---------------------------------------------------------------------------

#[test]
fn dos_api__dos_direct_console_io_uses_dl() {
    let func = json!({
        "start": 0x0000,
        "instructions": [
            {"address": 0x0000, "mnemonic": "mov", "op_str": "dl, 0x41", "bytes": "B241"},
            {"address": 0x0002, "mnemonic": "mov", "op_str": "ah, 0x06", "bytes": "B406"},
            {"address": 0x0004, "mnemonic": "int", "op_str": "0x21", "bytes": "CD21"},
            {"address": 0x0006, "mnemonic": "ret", "op_str": "", "bytes": "C3"},
        ],
    });
    let src = render_rs(&func, &[], "");
    assert!(src.contains("r.set_dl(0x41);"), "{src}");
    assert!(src.contains("r.dos_api();"), "{src}");
}

#[test]
fn dos_api__dos_string_pointer_does_not_use_stale_dx_after_dl_write() {
    // The C backend reconstructed the DS:DX string pointer and had a staleness
    // hazard (dx vs a captured immediate). In the Rust backend registers are
    // written immediately, so dos_api() always reads live dx — keep this as a
    // regression that the register writes ARE emitted, in program order,
    // before the call.
    let func = json!({
        "start": 0x0000,
        "instructions": [
            {"address": 0x0000, "mnemonic": "mov", "op_str": "dx, 0x1200", "bytes": "BA0012"},
            {"address": 0x0003, "mnemonic": "mov", "op_str": "dl, 0x41", "bytes": "B241"},
            {"address": 0x0005, "mnemonic": "mov", "op_str": "ah, 0x09", "bytes": "B409"},
            {"address": 0x0007, "mnemonic": "int", "op_str": "0x21", "bytes": "CD21"},
            {"address": 0x0009, "mnemonic": "ret", "op_str": "", "bytes": "C3"},
        ],
    });
    let src = render_rs(&func, &[], "");
    let dx = src
        .find("r.set_dx(0x1200);")
        .unwrap_or_else(|| panic!("{src}"));
    let dl = src
        .find("r.set_dl(0x41);")
        .unwrap_or_else(|| panic!("{src}"));
    let call = src.find("r.dos_api();").unwrap_or_else(|| panic!("{src}"));
    assert!(dx < dl, "dx write must precede dl write:\n{src}");
    assert!(dl < call, "dl write must precede r.dos_api():\n{src}");
}

// ---------------------------------------------------------------------------
// ---------------------------------------------------------------------------

#[test]
fn dx_in_branch__dx_assignment_inside_if() {
    let func = json!({
        "start": 0x0000,
        "instructions": [
            {"address": 0x0000, "mnemonic": "test",
             "op_str": "byte ptr cs:[0xff77], 0xff", "bytes": "2ef60677ffff"},
            {"address": 0x0006, "mnemonic": "je", "op_str": "000b", "bytes": "7403"},
            {"address": 0x0008, "mnemonic": "mov", "op_str": "dx, 0xffff", "bytes": "baffff"},
            {"address": 0x000b, "mnemonic": "ret", "op_str": "", "bytes": "c3"},
        ],
    });
    let src = render_rs(&func, &[], "");
    // the test writes ZF off the cs-relative byte, the je branches on it
    assert!(
        src.contains("let left_val: u32 = (r.memb(r.cs(), 0xFF77)) as u32;"),
        "{src}"
    );
    assert!(src.contains("if r.ZF() == 1 {"), "{src}");
    assert!(src.contains("return 0x000B;"), "{src}");
    // the not-taken block (0x0008) carries the dx assignment
    let body = blk(&src, 0x0008);
    assert!(body.contains("r.set_dx(0xFFFF);"), "{body}");
}

// ---------------------------------------------------------------------------
// ---------------------------------------------------------------------------

#[test]
fn empty_header_loop__loop_with_header_only_jump() {
    let func = json!({
        "start": 0x0000,
        "instructions": [
            {"address": 0x0000, "mnemonic": "jmp", "op_str": "0x2", "bytes": "E90200"},
            {"address": 0x0002, "mnemonic": "nop", "op_str": "", "bytes": "90"},
            {"address": 0x0003, "mnemonic": "jmp", "op_str": "0x0", "bytes": "E9FAFF"},
            {"address": 0x0005, "mnemonic": "ret", "op_str": "", "bytes": "C3"},
        ],
    });
    let src = render_rs(&func, &[], "");
    // the loop lowers to pc back-edges: header -> body -> (jmp block) -> header.
    // The jmp@0 fallthrough (0x0003) is its own block leader, so the nop block
    // (0x0002) falls through to it and IT carries the back edge.
    assert!(src.contains("return 0x0002;"), "{src}");
    let body = blk(&src, 0x0003);
    assert!(body.contains("return 0x0000;"), "{body}");
    // nop emits no statements
    assert!(!src.contains("nop"), "{src}");
}

// ---------------------------------------------------------------------------
// ---------------------------------------------------------------------------

#[test]
fn enter_leave__leave_is_mov_sp_bp_then_pop_bp() {
    // LEAVE (0xC9): SP <- BP; BP <- pop(). Common epilogue instruction.
    let src = render_rs(&wrap("leave", "", "C9"), &[], "");
    let body = &src[src
        .find("r.set_sp(r.bp());")
        .unwrap_or_else(|| panic!("{src}"))..];
    assert!(body.contains("r.set_sp(r.bp());"), "{body}");
    assert!(body.contains("r.set_bp(r.memw(r.ss(), r.sp()));"), "{body}");
    assert!(
        body.contains("r.set_sp((r.sp().wrapping_add(2)) & 0xFFFF);"),
        "{body}"
    );
    // order matters: SP is set from BP *before* the pop reads [SS:SP].
    assert!(
        body.find("r.set_sp(r.bp());").unwrap()
            < body.find("r.set_bp(r.memw(r.ss(), r.sp()));").unwrap()
    );
    assert!(
        body.find("r.set_bp(r.memw(r.ss(), r.sp()));").unwrap()
            < body
                .find("r.set_sp((r.sp().wrapping_add(2)) & 0xFFFF);")
                .unwrap()
    );
}

#[test]
fn enter_leave__enter_level0_pushes_bp_sets_frame_and_allocs() {
    // ENTER 0x10, 0: push bp; bp = sp; sp -= 0x10 (no frame-pointer copies).
    let src = render_rs(&wrap("enter", "0x10, 0", "C81000"), &[], "");
    assert!(
        src.contains("r.set_sp((r.sp().wrapping_sub(2)) & 0xFFFF);"),
        "{src}"
    );
    assert!(
        src.contains("r.memw_write(r.ss(), r.sp(), r.bp());"),
        "{src}"
    );
    assert!(src.contains("r.set_bp(frame_temp);"), "{src}");
    assert!(
        src.contains("r.set_sp((r.sp().wrapping_sub(0x10)) & 0xFFFF);"),
        "{src}"
    );
    // level 0 copies no enclosing frame pointers
    assert!(
        !src.contains("r.memw_write(r.ss(), r.sp(), r.memw(r.ss(), r.bp()));"),
        "{src}"
    );
}

#[test]
fn enter_leave__enter_zero_alloc_emits_no_sub() {
    // ENTER 0, 0: push bp; bp = sp (alloc 0 => no SP subtraction).
    let src = render_rs(&wrap("enter", "0, 0", "C80000"), &[], "");
    assert!(
        src.contains("r.memw_write(r.ss(), r.sp(), r.bp());"),
        "{src}"
    );
    assert!(src.contains("r.set_bp(frame_temp);"), "{src}");
    assert!(!src.contains("wrapping_sub(0x0)"), "{src}");
}

#[test]
fn enter_leave__enter_level1_pushes_frame_temp() {
    // ENTER 8, 1: nesting level 1 pushes the just-saved FrameTemp once.
    let src = render_rs(&wrap("enter", "8, 1", "C80800"), &[], "");
    assert!(src.contains("let frame_temp: u16 = r.sp();"), "{src}");
    assert!(
        src.contains("r.memw_write(r.ss(), r.sp(), frame_temp);"),
        "{src}"
    );
    // level 1 has no interior bp-walk copies (that starts at level 2)
    assert!(
        !src.contains("r.memw_write(r.ss(), r.sp(), r.memw(r.ss(), r.bp()));"),
        "{src}"
    );
}

#[test]
fn enter_leave__enter_level2_copies_one_enclosing_pointer() {
    // ENTER 0, 2: level 2 walks bp once and copies one enclosing frame pointer.
    let src = render_rs(&wrap("enter", "0, 2", "C80002"), &[], "");
    assert!(
        src.contains("r.set_bp((r.bp().wrapping_sub(2)) & 0xFFFF);"),
        "{src}"
    );
    assert!(
        src.contains("r.memw_write(r.ss(), r.sp(), r.memw(r.ss(), r.bp()));"),
        "{src}"
    );
    assert!(
        src.contains("r.memw_write(r.ss(), r.sp(), frame_temp);"),
        "{src}"
    );
}

// ---------------------------------------------------------------------------
// ---------------------------------------------------------------------------

#[test]
fn extra_instructions__lodsw_stosw() {
    let func = json!({
        "start": 0x0000,
        "instructions": [
            {"address": 0x0000, "mnemonic": "lodsw", "op_str": "", "bytes": "AD"},
            {"address": 0x0001, "mnemonic": "stosw", "op_str": "", "bytes": "AB"},
            {"address": 0x0002, "mnemonic": "ret", "op_str": "", "bytes": "C3"},
        ],
    });
    let src = render_rs(&func, &[], "");
    assert!(
        src.contains("let delta: i32 = if r.DF() != 0 { -2 } else { 2 };"),
        "{src}"
    );
    assert!(src.contains("r.set_ax(r.memw(r.ds(), r.si()));"), "{src}");
    assert!(
        src.contains("r.set_si(((r.si() as i32 + delta) & 0xFFFF) as u16);"),
        "{src}"
    );
    assert!(
        src.contains("r.memw_write(r.es(), r.di(), r.ax());"),
        "{src}"
    );
    assert!(
        src.contains("r.set_di(((r.di() as i32 + delta) & 0xFFFF) as u16);"),
        "{src}"
    );
}

#[test]
fn extra_instructions__xchg() {
    let func = json!({
        "start": 0x0000,
        "instructions": [
            {"address": 0x0000, "mnemonic": "xchg", "op_str": "ax, cx", "bytes": "91"},
            {"address": 0x0001, "mnemonic": "ret", "op_str": "", "bytes": "C3"},
        ],
    });
    let src = render_rs(&func, &[], "");
    assert!(src.contains("let tmp: u16 = r.ax();"), "{src}");
    assert!(src.contains("r.set_ax(r.cx());"), "{src}");
    assert!(src.contains("r.set_cx(tmp);"), "{src}");
}

#[test]
fn extra_instructions__pushf_popf() {
    let func = json!({
        "start": 0x0000,
        "instructions": [
            {"address": 0x0000, "mnemonic": "pushf", "op_str": "", "bytes": "9C"},
            {"address": 0x0001, "mnemonic": "popf", "op_str": "", "bytes": "9D"},
            {"address": 0x0002, "mnemonic": "ret", "op_str": "", "bytes": "C3"},
        ],
    });
    let src = render_rs(&func, &[], "");
    assert!(
        src.contains(
            "r.memw_write(r.ss(), r.sp(), 0x0002u16 | r.CF() as u16 | ((r.PF() as u16) << 2) | \
             ((r.ZF() as u16) << 6) | ((r.SF() as u16) << 7) | ((r.IF() as u16) << 9) | \
             ((r.DF() as u16) << 10) | ((r.OF() as u16) << 11));"
        ),
        "{src}"
    );
    assert!(src.contains("let old_if = r.IF();"), "{src}");
    assert!(src.contains("let flags = r.memw(r.ss(), r.sp());"), "{src}");
    assert!(src.contains("r.set_CF((flags & 0x0001) as u8);"), "{src}");
    assert!(src.contains("r.set_PF(((flags >> 2) & 1) as u8);"), "{src}");
    assert!(src.contains("r.set_IF(((flags >> 9) & 1) as u8);"), "{src}");
    assert!(
        src.contains("r.set_sp((r.sp().wrapping_add(2)) & 0xFFFF);"),
        "{src}"
    );
}

#[test]
fn extra_instructions__int_16() {
    let func = json!({
        "start": 0x0000,
        "instructions": [
            {"address": 0x0000, "mnemonic": "int", "op_str": "16", "bytes": "CD16"},
            {"address": 0x0001, "mnemonic": "ret", "op_str": "", "bytes": "C3"},
        ],
    });
    let src = render_rs(&func, &[], "");
    assert!(src.contains("r.bios_keyboard();"), "{src}");
}

// ---------------------------------------------------------------------------
// ---------------------------------------------------------------------------

#[test]
fn flag_reset__or_not_skipped_after_popf() {
    // The or's result write must survive even though popf overwrites the flags
    // right after (no dead-flag elimination may drop the register write).
    let func = json!({
        "start": 0x0000,
        "instructions": [
            {"address": 0x0000, "mnemonic": "or", "op_str": "ax, bx", "bytes": "0BC3"},
            {"address": 0x0002, "mnemonic": "push", "op_str": "ax", "bytes": "50"},
            {"address": 0x0003, "mnemonic": "popf", "op_str": "", "bytes": "9D"},
            {"address": 0x0004, "mnemonic": "ret", "op_str": "", "bytes": "C3"},
            {"address": 0x0010, "mnemonic": "loop", "op_str": "0x0010", "bytes": "E2FE"},
        ],
    });
    let src = render_rs(&func, &[], "");
    assert!(
        src.contains("let tmp: u16 = (((r.ax()) as u32 | (r.bx()) as u32) & 0xFFFF) as u16;"),
        "{src}"
    );
    assert!(src.contains("r.set_ax(tmp);"), "{src}");
}

#[test]
fn flag_reset__readwrite_mem_access_clobbers_cmp_before_jcc() {
    let instrs = vec![
        to_insn(json!({
            "address": 0x0000,
            "mnemonic": "cmp",
            "op_str": "ax, word ptr [bx]",
            "bytes": "3B07",
            "detail": {
                "mem_refs": [
                    {"segment": "DS", "base": "BX", "index": "", "scale": 1,
                     "disp": 0, "access": "read"}
                ]
            },
        })),
        to_insn(json!({
            "address": 0x0002,
            "mnemonic": "xchg",
            "op_str": "word ptr [bx], ax",
            "bytes": "8707",
            "detail": {
                "mem_refs": [
                    {"segment": "DS", "base": "BX", "index": "", "scale": 1,
                     "disp": 0, "access": "readwrite"}
                ]
            },
        })),
        to_insn(json!({
            "address": 0x0004,
            "mnemonic": "jnz",
            "op_str": "0x10",
            "bytes": "750A",
        })),
    ];
    let normalized = translate::normalize_flags(&instrs);
    assert!(!normalized.last().unwrap().contains_key("cond_prev"));
}

// ---------------------------------------------------------------------------
// ---------------------------------------------------------------------------

// (Fixed) parse_imm("0000") now returns 0 (all-zero tokens), matching the original.
#[test]
fn flag_setting__stc_followed_by_jb_checks_carry_flag() {
    let func = json!({
        "start": 0x0000,
        "instructions": [
            {"address": 0x0000, "mnemonic": "stc", "op_str": "", "bytes": "F9"},
            {"address": 0x0001, "mnemonic": "jb", "op_str": "0000", "bytes": "7200"},
            {"address": 0x0003, "mnemonic": "ret", "op_str": "", "bytes": "C3"},
        ],
    });
    let src = render_rs(&func, &[], "");
    assert!(src.contains("r.set_CF(1);"), "{src}");
    // the jb branches on CF and loops back to the block start
    assert!(src.contains("if r.CF() == 1 {"), "{src}");
    assert!(src.contains("return 0x0000;"), "{src}");
}

// (Fixed) parse_imm("0000") now returns 0, matching the original.
#[test]
fn flag_setting__clc_followed_by_jnb_checks_carry_flag() {
    let func = json!({
        "start": 0x0000,
        "instructions": [
            {"address": 0x0000, "mnemonic": "clc", "op_str": "", "bytes": "F8"},
            {"address": 0x0001, "mnemonic": "jnb", "op_str": "0000", "bytes": "7300"},
            {"address": 0x0003, "mnemonic": "ret", "op_str": "", "bytes": "C3"},
        ],
    });
    let src = render_rs(&func, &[], "");
    assert!(src.contains("r.set_CF(0);"), "{src}");
    assert!(src.contains("if r.CF() == 0 {"), "{src}");
    assert!(src.contains("return 0x0000;"), "{src}");
}

// ---------------------------------------------------------------------------
// ---------------------------------------------------------------------------

#[test]
fn for_loop__loop_with_jcxz_is_structured_as_for_loop() {
    let func = json!({
        "start": 0x0000,
        "instructions": [
            {"address": 0x0000, "mnemonic": "mov", "op_str": "cx, 3", "bytes": "B90300"},
            {"address": 0x0003, "mnemonic": "jcxz", "op_str": "0x9", "bytes": "E304"},
            {"address": 0x0005, "mnemonic": "int", "op_str": "0x10", "bytes": "CD10"},
            {"address": 0x0007, "mnemonic": "loop", "op_str": "0x5", "bytes": "E2FC"},
            {"address": 0x0009, "mnemonic": "ret", "op_str": "", "bytes": "C3"},
        ],
    });
    let src = render_rs(&func, &[], "");
    assert!(src.contains("r.set_cx(0x3);"), "{src}");
    // jcxz guards entry to the body
    assert!(src.contains("if r.cx() == 0 {"), "{src}");
    assert!(src.contains("return 0x0009;"), "{src}");
    // the body and the loop back-edge with the cx decrement
    assert!(src.contains("r.run_interrupt(0x10);"), "{src}");
    assert!(src.contains("r.set_cx(r.cx().wrapping_sub(1));"), "{src}");
    assert!(src.contains("if r.cx() != 0 {"), "{src}");
    assert!(src.contains("return 0x0005;"), "{src}");
}

#[test]
fn for_loop__simple_for_loop_is_structured() {
    let func = json!({
        "start": 0x0000,
        "instructions": [
            {"address": 0x0000, "mnemonic": "mov", "op_str": "cx, 0", "bytes": "B90000"},
            {"address": 0x0004, "mnemonic": "jmp", "op_str": "0x6", "bytes": "E90200"},
            {"address": 0x0006, "mnemonic": "jnz", "op_str": "0x12", "bytes": "750A"},
            {"address": 0x0008, "mnemonic": "call", "op_str": "0x1000", "bytes": "E80000"},
            {"address": 0x000C, "mnemonic": "inc", "op_str": "cx", "bytes": "41"},
            {"address": 0x000E, "mnemonic": "jmp", "op_str": "0x6", "bytes": "E9F9FF"},
            {"address": 0x0012, "mnemonic": "ret", "op_str": "", "bytes": "C3"},
        ],
    });
    let src = render_rs(&func, &[0x0000, 0x1000], "g_");
    assert!(src.contains("r.set_cx(0x0);"), "{src}");
    // the loop-exit branch
    assert!(src.contains("if r.ZF() == 0 {"), "{src}");
    assert!(src.contains("return 0x0012;"), "{src}");
    // the known-sibling call renders as an intra-chunk transfer
    assert!(src.contains("return 0x1000;"), "{src}");
    // the increment step and the back-edge to the loop header
    assert!(src.contains("let old: u32 = (r.cx()) as u32;"), "{src}");
    assert!(
        src.contains("r.set_cx((old.wrapping_add(1) & 0xFFFF) as u16);"),
        "{src}"
    );
    assert!(src.contains("return 0x0006;"), "{src}");
}

#[test]
fn for_loop__basic_arithmetic_is_translated_to_c() {
    let func = json!({
        "start": 0x0000,
        "instructions": [
            {"address": 0x0000, "mnemonic": "mov", "op_str": "ax, bx", "bytes": "89D8"},
            {"address": 0x0002, "mnemonic": "add", "op_str": "ax, 1", "bytes": "83C001"},
            {"address": 0x0005, "mnemonic": "sub", "op_str": "ax, 2", "bytes": "83E002"},
            {"address": 0x0008, "mnemonic": "inc", "op_str": "ax", "bytes": "40"},
            {"address": 0x0009, "mnemonic": "dec", "op_str": "ax", "bytes": "48"},
            {"address": 0x000A, "mnemonic": "xor", "op_str": "ax, ax", "bytes": "31C0"},
            {"address": 0x000C, "mnemonic": "ret", "op_str": "", "bytes": "C3"},
        ],
    });
    let src = render_rs(&func, &[], "");
    assert!(src.contains("r.set_ax(r.bx());"), "{src}");
    assert!(src.contains("let old: u32 = (r.ax()) as u32;"), "{src}");
    assert!(src.contains("let src: u32 = (0x1) as u32;"), "{src}");
    assert!(
        src.contains("let tmp: u32 = old.wrapping_add(src);"),
        "{src}"
    );
    assert!(src.contains("r.set_ax((tmp & 0xFFFF) as u16);"), "{src}");
    assert!(src.contains("let src: u32 = (0x2) as u32;"), "{src}");
    assert!(
        src.contains("let tmp: u32 = old.wrapping_sub(src);"),
        "{src}"
    );
    assert!(src.contains("r.set_CF((old < src) as u8);"), "{src}");
    assert!(
        src.contains("r.set_ax((old.wrapping_add(1) & 0xFFFF) as u16);"),
        "{src}"
    );
    assert!(
        src.contains("r.set_ax((old.wrapping_sub(1) & 0xFFFF) as u16);"),
        "{src}"
    );
    assert!(src.contains("r.set_ax(0);"), "{src}");
}

#[test]
fn for_loop__cmp_followed_by_jcc_uses_high_level_condition() {
    let func = json!({
        "start": 0x0000,
        "instructions": [
            {"address": 0x0000, "mnemonic": "cmp", "op_str": "al, 2", "bytes": "3C02"},
            {"address": 0x0002, "mnemonic": "jnc", "op_str": "0x6", "bytes": "7302"},
            {"address": 0x0004, "mnemonic": "int", "op_str": "0x20", "bytes": "CD20"},
            {"address": 0x0006, "mnemonic": "ret", "op_str": "", "bytes": "C3"},
        ],
    });
    let src = render_rs(&func, &[], "");
    assert!(
        src.contains("let left_val: u32 = (r.al()) as u32;"),
        "{src}"
    );
    assert!(src.contains("let right_val: u32 = (0x2) as u32;"), "{src}");
    assert!(src.contains("if r.CF() == 0 {"), "{src}");
    assert!(src.contains("r.dos_exit();"), "{src}");
}

#[test]
fn for_loop__dos_call_before_conditional_jump_is_preserved() {
    let func = json!({
        "start": 0x0000,
        "instructions": [
            {"address": 0x0000, "mnemonic": "mov", "op_str": "dx, 7E2h", "bytes": "BAE207"},
            {"address": 0x0003, "mnemonic": "mov", "op_str": "ax, 3D00h", "bytes": "B8003D"},
            {"address": 0x0006, "mnemonic": "int", "op_str": "21", "bytes": "CD21"},
            {"address": 0x0008, "mnemonic": "jnc", "op_str": "000C", "bytes": "7302"},
            {"address": 0x000A, "mnemonic": "ret", "op_str": "", "bytes": "C3"},
            {"address": 0x000C, "mnemonic": "ret", "op_str": "", "bytes": "C3"},
        ],
    });
    let src = render_rs(&func, &[], "");
    assert!(src.contains("r.set_dx(0x7E2);"), "{src}");
    assert!(src.contains("r.set_ax(0x3D00);"), "{src}");
    assert!(src.contains("r.dos_api();"), "{src}");
    assert!(src.contains("if r.CF() == 0 {"), "{src}");
}

#[test]
fn for_loop__for_loop_with_cmp_condition() {
    let func = json!({
        "start": 0x0000,
        "instructions": [
            {"address": 0x0000, "mnemonic": "mov", "op_str": "cx, 0", "bytes": "B90000"},
            {"address": 0x0003, "mnemonic": "jmp", "op_str": "0x8", "bytes": "E904"},
            {"address": 0x0008, "mnemonic": "cmp", "op_str": "cx, 3", "bytes": "83F9"},
            {"address": 0x000A, "mnemonic": "jge", "op_str": "0x15", "bytes": "7D09"},
            {"address": 0x000C, "mnemonic": "mov", "op_str": "ax, bx", "bytes": "89D8"},
            {"address": 0x000E, "mnemonic": "inc", "op_str": "cx", "bytes": "41"},
            {"address": 0x000F, "mnemonic": "jmp", "op_str": "0x8", "bytes": "E9F8FF"},
            {"address": 0x0015, "mnemonic": "ret", "op_str": "", "bytes": "C3"},
        ],
    });
    let src = render_rs(&func, &[], "");
    // the cmp writes flags, the jge exits on SF == OF
    assert!(
        src.contains("let left_val: u32 = (r.cx()) as u32;"),
        "{src}"
    );
    assert!(src.contains("let right_val: u32 = (0x3) as u32;"), "{src}");
    assert!(src.contains("if r.SF() == r.OF() {"), "{src}");
    assert!(src.contains("return 0x0015;"), "{src}");
    // the loop body, increment, and back-edge to the cmp header
    assert!(src.contains("r.set_ax(r.bx());"), "{src}");
    assert!(
        src.contains("r.set_cx((old.wrapping_add(1) & 0xFFFF) as u16);"),
        "{src}"
    );
    assert!(src.contains("return 0x0008;"), "{src}");
}

#[test]
fn for_loop__loop_with_memory_step_rendered_as_while() {
    let func = json!({
        "start": 0x0000,
        "instructions": [
            {"address": 0x0000, "mnemonic": "mov",
             "op_str": "word ptr es:[di+4], 0", "bytes": "00"},
            {"address": 0x0001, "mnemonic": "jmp", "op_str": "0x4", "bytes": "00"},
            {"address": 0x0004, "mnemonic": "cmp",
             "op_str": "word ptr es:[di+4], 16", "bytes": "00"},
            {"address": 0x0005, "mnemonic": "jge", "op_str": "0x9", "bytes": "00"},
            {"address": 0x0006, "mnemonic": "call", "op_str": "0x1000", "bytes": "00"},
            {"address": 0x0007, "mnemonic": "add",
             "op_str": "word ptr es:[di+4], di", "bytes": "00"},
            {"address": 0x0008, "mnemonic": "jmp", "op_str": "0x4", "bytes": "00"},
            {"address": 0x0009, "mnemonic": "ret", "op_str": "", "bytes": "00"},
        ],
    });
    let src = render_rs(&func, &[0x1000], "g_");
    // the memory step: read-modify-write of es:[di+4] with the full add block
    assert!(src.contains("let old: u32 = (r.memw(r.es(), "), "{src}");
    assert!(src.contains("let src: u32 = (r.di()) as u32;"), "{src}");
    assert!(
        src.contains("let tmp: u32 = old.wrapping_add(src);"),
        "{src}"
    );
    assert!(src.contains("r.memw_write(r.es(), "), "{src}");
    assert!(src.contains("(tmp & 0xFFFF) as u16);"), "{src}");
    // the loop back-edge to the cmp header
    assert!(src.contains("return 0x0004;"), "{src}");
}

// ---------------------------------------------------------------------------
//
// the original calls the private `handle_arithmetic` directly; the Rust equivalent is
// private, so we render a full function containing the single inc/dec (the
// inc/dec emit path always writes the full flag block unconditionally) and
// assert the same substrings.
// ---------------------------------------------------------------------------

#[test]
fn inc_dec_flags__inc_sets_sf_and_of() {
    let src = render_rs(&wrap("inc", "al", "FE C0"), &[], "");
    assert!(
        src.contains("r.set_SF((((r.al()) >> 7) & 1) as u8);"),
        "{src}"
    );
    assert!(src.contains("r.set_OF((old == 0x7F) as u8);"), "{src}");
}

#[test]
fn inc_dec_flags__dec_sets_sf_and_of() {
    let src = render_rs(&wrap("dec", "ax", "48"), &[], "");
    assert!(
        src.contains("r.set_SF((((r.ax()) >> 15) & 1) as u8);"),
        "{src}"
    );
    assert!(src.contains("r.set_OF((old == 0x8000) as u8);"), "{src}");
}

// ---------------------------------------------------------------------------
// ---------------------------------------------------------------------------

fn memory_inc_dec_conditional_jump(mnemonic: &str, jcc: &str, bytes_jcc: &str, cond: &str) {
    let func = json!({
        "start": 0x0000,
        "instructions": [
            {"address": 0x0000, "mnemonic": mnemonic,
             "op_str": "byte ptr [bp-4]", "bytes": "0000"},
            {"address": 0x0002, "mnemonic": jcc, "op_str": "0007",
             "target": 0x0007, "bytes": bytes_jcc},
            {"address": 0x0004, "mnemonic": "ret", "op_str": "", "bytes": "C3"},
            {"address": 0x0007, "mnemonic": "ret", "op_str": "", "bytes": "C3"},
        ],
    });
    let src = render_rs(&func, &[], "");
    // the memory operand is rewritten (bp-based defaults to SS), flags written
    assert!(src.contains("r.memb(r.ss(), "), "{src}");
    assert!(src.contains("set_ZF"), "{src}");
    assert!(src.contains(cond), "{src}");
    assert!(!src.contains("byte ptr"), "{src}");
}

#[test]
fn inc_dec_jcc_memory__memory_inc_conditional_jump_jz() {
    memory_inc_dec_conditional_jump("inc", "jz", "7403", "if r.ZF() == 1 {");
}

#[test]
fn inc_dec_jcc_memory__memory_dec_conditional_jump_jnz() {
    memory_inc_dec_conditional_jump("dec", "jnz", "7503", "if r.ZF() == 0 {");
}

// ---------------------------------------------------------------------------
// ---------------------------------------------------------------------------

#[test]
fn io__in_instruction_is_rendered() {
    let func = json!({
        "start": 0x0000,
        "instructions": [
            {"address": 0x0000, "mnemonic": "in", "op_str": "al, 0x60", "bytes": "E460"},
            {"address": 0x0002, "mnemonic": "ret", "op_str": "", "bytes": "C3"},
        ],
    });
    let src = render_rs(&func, &[], "");
    assert!(src.contains("r.set_al(unsafe { r.inb(0x60) });"), "{src}");
}

#[test]
fn io__out_instruction_is_rendered() {
    let func = json!({
        "start": 0x0000,
        "instructions": [
            {"address": 0x0000, "mnemonic": "mov", "op_str": "al, 1", "bytes": "B001"},
            {"address": 0x0002, "mnemonic": "out", "op_str": "0x61, al", "bytes": "E661"},
            {"address": 0x0004, "mnemonic": "ret", "op_str": "", "bytes": "C3"},
        ],
    });
    let src = render_rs(&func, &[], "");
    assert!(src.contains("r.set_al(0x1);"), "{src}");
    assert!(src.contains("unsafe { r.outb(0x61, r.al()); }"), "{src}");
}

// ---------------------------------------------------------------------------
// ---------------------------------------------------------------------------

#[test]
fn iret__iret_invokes_helper() {
    let func = json!({
        "start": 0x0000,
        "instructions": [
            {"address": 0x0000, "mnemonic": "iret", "op_str": "", "bytes": "CF"},
        ],
    });
    let src = render_rs(&func, &[], "");
    assert!(src.contains("r.iret_();"), "{src}");
}

// ---------------------------------------------------------------------------
// ---------------------------------------------------------------------------

#[test]
fn jcc_memory_operand__conditional_jump_rewrites_memory_operand() {
    let func = json!({
        "start": 0x0000,
        "instructions": [
            {"address": 0x0000, "mnemonic": "cmp",
             "op_str": "byte ptr cs:[0x8e8], 0", "bytes": "0000"},
            {"address": 0x0004, "mnemonic": "jnz", "op_str": "0009",
             "target": 0x0009, "bytes": "7503"},
            {"address": 0x0006, "mnemonic": "ret", "op_str": "", "bytes": "C3"},
            {"address": 0x0009, "mnemonic": "ret", "op_str": "", "bytes": "C3"},
        ],
    });
    let src = render_rs(&func, &[], "");
    assert!(src.contains("r.memb(r.cs(), 0x8E8)"), "{src}");
    assert!(!src.contains("byte ptr"), "{src}");
}
