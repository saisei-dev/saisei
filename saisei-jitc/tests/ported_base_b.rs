//! Ported from tests/test_*.py — df_steps, dispatch_helper, distinct_mem_refs,
//! div, dos_api, dx_in_branch, empty_header_loop, enter_leave,
//! extra_instructions, flag_reset, flag_setting, for_loop, inc_dec_flags,
//! inc_dec_jcc_memory, io, iret, jcc_memory_operand.
mod common;
use common::*;
use saisei_jitc::ir_to_c;
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
fn to_insn(v: Value) -> ir_to_c::Insn {
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
    let src = render_c(&func, &[], "");
    assert!(src.contains("DF = 1;"), "{src}");
    assert!(src.contains("int delta = DF ? -1 : 1;"), "{src}");
    assert!(src.contains("si = (si + delta) & 0xFFFF;"), "{src}");
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
    let src = render_c(&func, &[], "");
    assert!(src.contains("DF = 1;"), "{src}");
    assert!(src.contains("int delta = DF ? -1 : 1;"), "{src}");
    assert!(src.contains("si = (si + delta) & 0xFFFF;"), "{src}");
    assert!(src.contains("di = (di + delta) & 0xFFFF;"), "{src}");
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
    let src = render_c(&func, &[], "");
    // std still sets DF; the rep movsw itself is emitted as a call to the
    // rep_movsw_block shim, which reads DF for its step direction.
    assert!(src.contains("DF = 1;"), "{src}");
    assert!(src.contains("rep_movsw_block(es, ds);"), "{src}");
}

// ---------------------------------------------------------------------------
// ---------------------------------------------------------------------------

#[test]
fn dispatch_helper__preserves_multiple_statements() {
    let func = json!({
        "start": 0x0100,
        "instructions": [
            {"address": 0x0100, "mnemonic": "jmp", "op_str": "0200", "bytes": "E9FD01"},
            {"address": 0x0200, "mnemonic": "mov", "op_str": "ax, bx", "bytes": "89D8"},
            {"address": 0x0203, "mnemonic": "inc", "op_str": "ax", "bytes": "40"},
            {"address": 0x0204, "mnemonic": "ret", "op_str": "", "bytes": "C3"},
        ],
    });

    let mut r = renderer("");
    r.func_names.insert(0x0200, "label_0200".into());
    r.render_function_c(&func, &known(&[0x0100]));

    let helper_lines = r
        .extra_func_blocks
        .get(&0x0200)
        .expect("extra_func_blocks[0x0200] must exist");
    assert_eq!(helper_lines[0], "    ip = 0x0200;");
    assert!(
        helper_lines.iter().any(|l| l == "    ax = bx;"),
        "{helper_lines:?}"
    );
    assert!(
        helper_lines
            .iter()
            .any(|l| l == "        ax = (ax + 1) & 0xFFFF;"),
        "{helper_lines:?}"
    );
    // The ret now lowers to the near_ret_tail epilogue block, whose closing
    // brace is the final line of the helper.
    assert!(
        helper_lines
            .iter()
            .any(|l| l == "        near_ret_tail(popped_ip, expected_retip);"),
        "{helper_lines:?}"
    );
    assert_eq!(helper_lines[helper_lines.len() - 1], "    }");
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
    let result = ir_to_c::normalize_flags(&instrs);
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
    let src = render_c(&func, &[], "");
    assert!(src.contains("al = (tmp / divisor) & 0xFF;"), "{src}");
    assert!(src.contains("ah = (tmp % divisor) & 0xFF;"), "{src}");
    assert!(!src.contains("ZF = ax == 0;"), "{src}");
    assert!(!src.contains("// TODO ASM: div cl"), "{src}");
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
    let src = render_c(&func, &[], "");
    assert!(
        src.contains("uint32_t tmp = ((uint32_t)dx << 16) | ax;"),
        "{src}"
    );
    assert!(src.contains("ax = (tmp / divisor) & 0xFFFF;"), "{src}");
    assert!(src.contains("dx = (tmp % divisor) & 0xFFFF;"), "{src}");
    assert!(!src.contains("ZF ="), "{src}");
    assert!(!src.contains("// TODO ASM: div cx"), "{src}");
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
    let src = render_c(&func, &[], "");
    assert!(
        src.contains("int32_t dividend = ((int32_t)(int16_t)dx << 16) | ax;"),
        "{src}"
    );
    assert!(
        src.contains("ax = (uint16_t)(dividend / divisor);"),
        "{src}"
    );
    assert!(
        src.contains("dx = (uint16_t)(dividend % divisor);"),
        "{src}"
    );
    assert!(!src.contains("ZF ="), "{src}");
}

// ---------------------------------------------------------------------------
// ---------------------------------------------------------------------------

#[test]
fn dos_api__dos_write_char_passes_argument() {
    let func = json!({
        "start": 0x0000,
        "instructions": [
            {"address": 0x0000, "mnemonic": "mov", "op_str": "dl, 0x41", "bytes": "B241"},
            {"address": 0x0002, "mnemonic": "mov", "op_str": "ah, 0x02", "bytes": "B402"},
            {"address": 0x0004, "mnemonic": "int", "op_str": "0x21", "bytes": "CD21"},
            {"address": 0x0006, "mnemonic": "ret", "op_str": "", "bytes": "C3"},
        ],
    });
    let src = render_c(&func, &[], "");
    assert!(src.contains("CF = dos_write_char(0x41);"), "{src}");
}

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
    let src = render_c(&func, &[], "");
    assert!(src.contains("dl = 0x41;"), "{src}");
    assert!(src.contains("dos_direct_console_io(dl);"), "{src}");
}

#[test]
fn dos_api__dos_close_file_passes_bx() {
    let func = json!({
        "start": 0x0000,
        "instructions": [
            {"address": 0x0000, "mnemonic": "mov", "op_str": "bx, 0x0005", "bytes": "BB0500"},
            {"address": 0x0003, "mnemonic": "mov", "op_str": "ah, 0x3E", "bytes": "B43E"},
            {"address": 0x0005, "mnemonic": "int", "op_str": "0x21", "bytes": "CD21"},
            {"address": 0x0007, "mnemonic": "ret", "op_str": "", "bytes": "C3"},
        ],
    });
    let src = render_c(&func, &[], "");
    assert!(src.contains("CF = dos_close_file(0x0005);"), "{src}");
}

#[test]
fn dos_api__dos_exit_preserves_ah_after_al_write() {
    let func = json!({
        "start": 0x0000,
        "instructions": [
            {"address": 0x0000, "mnemonic": "mov", "op_str": "ah, 0x4C", "bytes": "B44C"},
            {"address": 0x0002, "mnemonic": "mov", "op_str": "al, 0x00", "bytes": "B000"},
            {"address": 0x0004, "mnemonic": "int", "op_str": "0x21", "bytes": "CD21"},
            {"address": 0x0006, "mnemonic": "ret", "op_str": "", "bytes": "C3"},
        ],
    });
    let src = render_c(&func, &[], "");
    assert!(src.contains("dos_exit();"), "{src}");
}

#[test]
fn dos_api__dos_string_pointer_does_not_use_stale_dx_after_dl_write() {
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
    let src = render_c(&func, &[], "");
    assert!(src.contains("dx = 0x1200;"), "{src}");
    assert!(src.contains("dl = 0x41;"), "{src}");
    assert!(
        src.contains("CF = dos_print_string((const char *)seg_off(ds, dx));"),
        "{src}"
    );
    assert!(
        !src.contains("CF = dos_print_string((const char *)seg_off(ds, 0x1200));"),
        "{src}"
    );
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
    let src = render_c(&func, &[], "");
    assert!(src.contains("if (ZF != 1) {"), "{src}");
    let body = src.split("if (ZF != 1) {").nth(1).unwrap();
    let body = body.split("}\n").next().unwrap();
    assert!(body.contains("dx = 0xffff;"), "{body}");
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
    let src = render_c(&func, &[], "");
    assert!(src.contains("while (1)"), "{src}");
    assert!(!src.contains("// TODO ASM: nop"), "{src}");
}

// ---------------------------------------------------------------------------
// ---------------------------------------------------------------------------

#[test]
fn enter_leave__leave_is_mov_sp_bp_then_pop_bp() {
    // LEAVE (0xC9): SP <- BP; BP <- pop(). Common epilogue instruction.
    let src = render_c(&wrap("leave", "", "C9"), &[], "");
    let body = &src[src.find("sp = bp;").unwrap()..];
    assert!(body.contains("sp = bp;"), "{body}");
    assert!(body.contains("bp = memw(ss, sp);"), "{body}");
    assert!(body.contains("sp = (sp + 2) & 0xFFFF;"), "{body}");
    // order matters: SP is set from BP *before* the pop reads [SS:SP].
    assert!(body.find("sp = bp;").unwrap() < body.find("bp = memw(ss, sp);").unwrap());
    assert!(
        body.find("bp = memw(ss, sp);").unwrap() < body.find("sp = (sp + 2) & 0xFFFF;").unwrap()
    );
}

#[test]
fn enter_leave__enter_level0_pushes_bp_sets_frame_and_allocs() {
    // ENTER 0x10, 0: push bp; bp = sp; sp -= 0x10 (no frame-pointer copies).
    let src = render_c(&wrap("enter", "0x10, 0", "C81000"), &[], "");
    assert!(src.contains("sp = (sp - 2) & 0xFFFF;"), "{src}");
    assert!(src.contains("memw_write(ss, sp, bp);"), "{src}");
    assert!(src.contains("bp = frame_temp;"), "{src}");
    assert!(src.contains("sp = (sp - 0x10) & 0xFFFF;"), "{src}");
    // level 0 copies no enclosing frame pointers
    assert!(!src.contains("memw_write(ss, sp, memw(ss, bp));"), "{src}");
}

#[test]
fn enter_leave__enter_zero_alloc_emits_no_sub() {
    // ENTER 0, 0: push bp; bp = sp (alloc 0 => no SP subtraction).
    let src = render_c(&wrap("enter", "0, 0", "C80000"), &[], "");
    assert!(src.contains("memw_write(ss, sp, bp);"), "{src}");
    assert!(src.contains("bp = frame_temp;"), "{src}");
    assert!(!src.contains("(sp - 0x0)"), "{src}");
}

#[test]
fn enter_leave__enter_level1_pushes_frame_temp() {
    // ENTER 8, 1: nesting level 1 pushes the just-saved FrameTemp once.
    let src = render_c(&wrap("enter", "8, 1", "C80800"), &[], "");
    assert!(src.contains("uint16_t frame_temp = sp;"), "{src}");
    assert!(src.contains("memw_write(ss, sp, frame_temp);"), "{src}");
    // level 1 has no interior bp-walk copies (that starts at level 2)
    assert!(!src.contains("memw_write(ss, sp, memw(ss, bp));"), "{src}");
}

#[test]
fn enter_leave__enter_level2_copies_one_enclosing_pointer() {
    // ENTER 0, 2: level 2 walks bp once and copies one enclosing frame pointer.
    let src = render_c(&wrap("enter", "0, 2", "C80002"), &[], "");
    assert!(src.contains("bp = (bp - 2) & 0xFFFF;"), "{src}");
    assert!(src.contains("memw_write(ss, sp, memw(ss, bp));"), "{src}");
    assert!(src.contains("memw_write(ss, sp, frame_temp);"), "{src}");
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
    let src = render_c(&func, &[], "");
    assert!(src.contains("int delta = DF ? -2 : 2;"), "{src}");
    assert!(src.contains("ax = memw(ds, si);"), "{src}");
    assert!(src.contains("si = (si + delta) & 0xFFFF;"), "{src}");
    assert!(src.contains("memw_write(es, di, ax);"), "{src}");
    assert!(src.contains("di = (di + delta) & 0xFFFF;"), "{src}");
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
    let src = render_c(&func, &[], "");
    assert!(src.contains("uint16_t tmp = ax;"), "{src}");
    assert!(src.contains("ax = cx;"), "{src}");
    assert!(src.contains("cx = tmp;"), "{src}");
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
    let src = render_c(&func, &[], "");
    assert!(
        src.contains(
            "memw_write(ss, sp, (uint16_t)(0x0002u | CF | (PF << 2) | (ZF << 6) | \
             (SF << 7) | (IF << 9) | (DF << 10) | (OF << 11)));"
        ),
        "{src}"
    );
    assert!(src.contains("uint16_t oldIF = IF;"), "{src}");
    assert!(src.contains("uint16_t flags = memw(ss, sp);"), "{src}");
    assert!(src.contains("CF = flags & 0x0001;"), "{src}");
    assert!(src.contains("PF = (flags >> 2) & 1;"), "{src}");
    assert!(src.contains("IF = (flags >> 9) & 1;"), "{src}");
    assert!(src.contains("sp = (sp + 2) & 0xFFFF;"), "{src}");
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
    let src = render_c(&func, &[], "");
    assert!(src.contains("bios_keyboard();"), "{src}");
}

// ---------------------------------------------------------------------------
// ---------------------------------------------------------------------------

#[test]
fn flag_reset__or_not_skipped_after_popf() {
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
    let src = render_c(&func, &[], "");
    assert!(src.contains("ax = (ax | bx) & 0xFFFF;"), "{src}");
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
    let normalized = ir_to_c::normalize_flags(&instrs);
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
    let src = render_c(&func, &[], "");
    assert!(src.contains("do {"), "{src}");
    assert!(src.contains("while (CF == 1)"), "{src}");
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
    let src = render_c(&func, &[], "");
    assert!(src.contains("do {"), "{src}");
    assert!(src.contains("while (CF == 0)"), "{src}");
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
    let src = render_c(&func, &[], "");
    assert!(src.contains("for (cx = 3; cx != 0; cx--)"), "{src}");
    assert!(src.contains("cx--"), "{src}");
    assert!(!src.contains("while"), "{src}");
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
    let src = render_c(&func, &[0x0000, 0x1000], "g_");
    assert!(src.contains("for (cx = 0; ZF != 0; cx++)"), "{src}");
    assert!(!src.contains("while ("), "{src}");
    assert!(src.contains("pc = 0x1000;"), "{src}");
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
    let src = render_c(&func, &[], "");
    assert!(src.contains("ax = bx;"), "{src}");
    assert!(src.contains("uint32_t old = ax;"), "{src}");
    assert!(src.contains("uint32_t src = 1;"), "{src}");
    assert!(src.contains("uint32_t tmp = old + src;"), "{src}");
    assert!(src.contains("ax = tmp & 0xFFFF;"), "{src}");
    assert!(src.contains("uint32_t src = 2;"), "{src}");
    assert!(src.contains("uint32_t tmp = old - src;"), "{src}");
    assert!(src.contains("CF = old < src;"), "{src}");
    assert!(src.contains("ax = (ax + 1) & 0xFFFF;"), "{src}");
    assert!(src.contains("ax = (ax - 1) & 0xFFFF;"), "{src}");
    assert!(src.contains("xor16(&ax, ax);"), "{src}");
    assert!(!src.contains("// TODO ASM: mov ax, bx"), "{src}");
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
    let src = render_c(&func, &[], "");
    assert!(src.contains("if (CF == 0)"), "{src}");
    assert!(src.contains("dos_exit();"), "{src}");
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
    let src = render_c(&func, &[], "");
    assert!(src.contains("CF = dos_open_file("), "{src}");
    assert!(src.contains("if (CF == 0)"), "{src}");
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
    let src = render_c(&func, &[], "");
    assert!(src.contains("for (cx = 0; SF != OF; cx++)"), "{src}");
    assert!(src.contains("ax = bx;"), "{src}");
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
    let src = render_c(&func, &[0x1000], "g_");
    assert!(!src.contains("for ("), "{src}");
    assert!(src.contains("while ("), "{src}");
    assert!(
        src.contains("uint32_t old = memw(es, (di+4) & 0xFFFF);"),
        "{src}"
    );
    assert!(src.contains("uint32_t src = di;"), "{src}");
    assert!(src.contains("uint32_t tmp = old + src;"), "{src}");
    assert!(
        src.contains("memw_write(es, (di+4) & 0xFFFF, tmp & 0xFFFF);"),
        "{src}"
    );
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
    let src = render_c(&wrap("inc", "al", "FE C0"), &[], "");
    assert!(src.contains("SF = (al >> 7) & 1;"), "{src}");
    assert!(src.contains("OF = old == 0x7F;"), "{src}");
}

#[test]
fn inc_dec_flags__dec_sets_sf_and_of() {
    let src = render_c(&wrap("dec", "ax", "48"), &[], "");
    assert!(src.contains("SF = (ax >> 15) & 1;"), "{src}");
    assert!(src.contains("OF = old == 0x8000;"), "{src}");
}

// ---------------------------------------------------------------------------
// ---------------------------------------------------------------------------

fn memory_inc_dec_conditional_jump(mnemonic: &str, jcc: &str, bytes_jcc: &str) {
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
    let src = render_c(&func, &[], "");
    assert!(src.contains("ZF"), "{src}");
    assert!(!src.contains("byte ptr"), "{src}");
}

#[test]
fn inc_dec_jcc_memory__memory_inc_conditional_jump_jz() {
    memory_inc_dec_conditional_jump("inc", "jz", "7403");
}

#[test]
fn inc_dec_jcc_memory__memory_dec_conditional_jump_jnz() {
    memory_inc_dec_conditional_jump("dec", "jnz", "7503");
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
    let src = render_c(&func, &[], "");
    assert!(src.contains("al = inb(0x60);"), "{src}");
    assert!(!src.contains("// TODO ASM: in al, 0x60"), "{src}");
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
    let src = render_c(&func, &[], "");
    assert!(src.contains("outb(0x61, al);"), "{src}");
    assert!(!src.contains("// TODO ASM: out 0x61, al"), "{src}");
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
    let src = render_c(&func, &[], "");
    assert!(src.contains("iret();"), "{src}");
    assert!(!src.contains("/* iret */"), "{src}");
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
    let src = render_c(&func, &[], "");
    assert!(src.contains("memb(cs, 0x8e8)"), "{src}");
    assert!(!src.contains("byte ptr cs:[0x8e8]"), "{src}");
}
