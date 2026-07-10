#![allow(non_snake_case)]
//! Ported from tests/test_*.py — base (structured) CCodeRenderer tests, now
//! asserted against the Rust chunk backend (`render_rs`).
//!
//! PORT DISPOSITIONS (C backend deleted):
//!   ported:    59 tests — C-text assertions rewritten per the token map
//!              (case 0xNNNN: -> 0xNNNN => {, jump_table -> jump_table_,
//!              lcall_table -> lcall_table_, long_jump -> long_jump_, ip= ->
//!              set_ip, ...). Notables:
//!              - ir_to_c__call_to_unknown_address_still_translates_literally
//!                was #[ignore]d (the C renderer process::exit(2) on unknown
//!                direct-call targets); the Rust backend dispatches through
//!                call_table_ — re-activated as
//!                ir_to_c__call_to_unknown_address_dispatches_via_call_table.
//!              - jump_table_rcb__jmp_word_ptr_es_rcb_field_uses_rcb_read16,
//!                jump_table_rcb__ljmp_es_rcb_fields_use_rcb_read16,
//!                jump_table_rcb__jmp_word_ptr_es_lowercase_hex: the old
//!                port-divergence notes (missing match_rcb_access rewrite) do
//!                not apply — ir_to_rust wires RCB into indirect_jump_target
//!                and render_arg, asserted directly.
//!              - label_goto__jmp_to_label_sets_pc and
//!                prefix__name_prefix_applied dropped their renderer()
//!                func_names pokes (no label channel in the flat state
//!                machine); the semantic core (pc transfer, name prefix on the
//!                emitted _impl/dispatch symbols) is asserted.
//!              - assert_jump_table's `!contains("pc =")` now applies to the
//!                jump-table ARM only (the whole chunk always carries pc
//!                assignments in the ret epilogue/boilerplate).
//!   collapsed: the Rust backend does not specialize int 21h per AH (every
//!              int 21h emits `dos_api();` after in-order register writes) nor
//!              int 10h per AH (`run_interrupt(0x10);`). Families are
//!              represented by ir_to_c__dos_interrupt_with_known_ah_emits_dos_api
//!              (renamed from ..._is_rendered_as_named_call),
//!              ir_to_c__dos_interrupt_preserves_unrelated_registers, and
//!              ir_to_c__bios_video_mode_interrupt_emits_run_interrupt
//!              (renamed from ..._emits_named_call). Collapsed (deleted) DOS
//!              per-AH members: xchg_invalidates_cached_dos_register_arguments,
//!              dos_open_file_is_rendered_as_named_call,
//!              dos_read_file_uses_buffer_and_len, dos_print_string_uses_pointer,
//!              dos_open_file_uses_pointer, dos_alloc_mem_includes_constant,
//!              dos_exec_uses_pointer, dos_set_interrupt_vector_builds_far_pointer,
//!              dos_get_interrupt_vector_emits_mov_ax,
//!              dos_reset_disk_is_rendered_as_named_call,
//!              dos_set_dta_includes_constant, dos_select_drive_uses_dl,
//!              dos_get_current_drive_is_rendered,
//!              dos_get_disk_free_space_uses_dl, dos_make_dir_uses_pointer,
//!              dos_change_dir_uses_pointer, dos_find_first_uses_pointer_and_cx,
//!              dos_lseek_includes_arguments, last_ah_al__mov_ah_invalidates_al
//!              (the last-ah/last-dx argument CACHE was a C-renderer construct;
//!              Rust writes registers immediately, so no staleness exists).
//!              Collapsed BIOS per-AH members:
//!              bios_set_palette_interrupt_emits_named_call,
//!              bios_cga_palette_interrupt_emits_named_call,
//!              bios_cursor_position_interrupt_emits_named_call,
//!              bios_teletype_interrupt_emits_named_call.
//!   deleted:   ir_to_c__invalidate_register_clears_cached_bx_for_byte_writes
//!              (empty #[ignore] stub; private C-renderer register cache),
//!              ir_to_c__unsupported_bios_interrupt_raises (empty #[ignore]
//!              stub; the Rust backend lowers every int NN to run_interrupt —
//!              nothing to raise), jump_table_rcb__match_rcb_access_lowercase_name
//!              and jump_table_rcb__match_rcb_access_lowercase_hex (empty
//!              #[ignore] stubs; private API — behavior covered by the active
//!              rcb rendering tests), metadata__function_name_metadata and
//!              metadata__instruction_comment_metadata (func_names/comments
//!              are C-renderer reverse-engineering annotation channels; the
//!              Rust chunk backend has none by design).
mod common;
use common::*;
use serde_json::{json, Value};

// the original helpers built an `app_`-prefixed / unprefixed CCodeRenderer;
// same signatures, now rendering through the Rust chunk backend.
fn render_app(func: &Value, known_addrs: &[i64]) -> String {
    render_rs(func, known_addrs, "app_")
}

fn render_plain(func: &Value, known_addrs: &[i64]) -> String {
    render_rs(func, known_addrs, "")
}

/// Slice out one dispatch-match arm (`0xNNNN => { ... }`) from a chunk text.
/// Arms close with a brace at 12-space indent; inner blocks are deeper.
fn arm(src: &str, addr: i64) -> String {
    let key = format!("0x{addr:04X} => {{");
    src.split(&key)
        .nth(1)
        .unwrap_or_else(|| panic!("arm {key} must exist in:\n{src}"))
        .split("\n            }")
        .next()
        .unwrap()
        .to_string()
}

/// The Rust jump-table contract: the arm computes the 20-bit linear target
/// with wrapping arithmetic, calls the noreturn jump_table_ helper, and makes
/// no direct pc transfer of its own. (All jump-table fixtures start at 0x0000.)
fn assert_jump_table(src: &str) {
    let body = arm(src, 0x0000);
    assert!(
        body.contains("jump_table_((((cs() as u32) << 4).wrapping_add("),
        "{body}"
    );
    assert!(body.contains(") & 0xFFFFF, expected_retip);"), "{body}");
    assert!(body.contains("return;"), "{body}");
    assert!(!body.contains("pc ="), "{body}");
}

// ============================================================================
// ============================================================================

#[test]
fn ir_to_c__call_to_known_address_emits_function_call() {
    let func = json!({
        "start": 0x0000,
        "instructions": [
            {"address":0x0000,"mnemonic":"call","op_str":"0x05F9","bytes":"E80000","target":0x05F9},
            {"address":0x0003,"mnemonic":"ret","op_str":"","bytes":"C3"},
        ],
    });
    let src = render_app(&func, &[0x0000, 0x05F9]);
    // push the cs-relative return IP, then transfer to the sibling arm
    assert!(
        src.contains("memw_write(ss(), sp(), ((0x3u32).wrapping_add(0x10100).wrapping_sub((cs() as u32) << 4)) as u16);"),
        "{src}"
    );
    assert!(src.contains("pc = 0x05F9;"), "{src}");
}

// The C renderer aborted (process::exit(2)) on a direct call to an unknown
// target, so the original test was #[ignore]d. The Rust backend dispatches it
// through call_table_ at the live cs-relative linear address (the JIT compiles
// the target on reach) — active again.
#[test]
fn ir_to_c__call_to_unknown_address_dispatches_via_call_table() {
    let func = json!({
        "start": 0x0000,
        "instructions": [
            {"address":0x0000,"mnemonic":"call","op_str":"0x1234","bytes":"E80000","target":0x1234},
            {"address":0x0003,"mnemonic":"ret","op_str":"","bytes":"C3"},
        ],
    });
    let src = render_app(&func, &[0x0000]);
    assert!(
        src.contains("call_table_(((0x3u32).wrapping_add(0x10100).wrapping_sub((cs() as u32) << 4)) as u16, (((cs() as u32) << 4).wrapping_add(0x1234)) & 0xFFFFF);"),
        "{src}"
    );
    assert!(!src.contains("pc = 0x1234;"), "{src}");
}

#[test]
fn ir_to_c__ret_instruction_emits_return_statement() {
    let func = json!({
        "start": 0x0000,
        "instructions": [
            {"address":0x0000,"mnemonic":"ret","op_str":"","bytes":"C3"},
        ],
    });
    let src = render_app(&func, &[]);
    // near ret pops the return IP and re-enters the dispatch loop with it
    assert!(src.contains("let popped_ip = memw(ss(), sp());"), "{src}");
    assert!(
        src.contains("set_sp((sp().wrapping_add(2)) & 0xFFFF);"),
        "{src}"
    );
    assert!(
        src.contains("pc = (((cs() as u32) << 4).wrapping_add(popped_ip as u32).wrapping_sub(0x10100)) as i32;"),
        "{src}"
    );
}

#[test]
fn ir_to_c__and_instruction_is_rendered() {
    let func = json!({
        "start": 0x0000,
        "instructions": [
            {"address":0x0000,"mnemonic":"and","op_str":"al, dh","bytes":"22F0"},
            {"address":0x0002,"mnemonic":"and","op_str":"al, 0x44","bytes":"2444"},
            {"address":0x0004,"mnemonic":"ret","op_str":"","bytes":"C3"},
        ],
    });
    let src = render_app(&func, &[]);
    assert!(
        src.contains("let tmp: u8 = (((al()) as u32 & (dh()) as u32) & 0xFF) as u8;"),
        "{src}"
    );
    assert!(
        src.contains("let tmp: u8 = (((al()) as u32 & (0x44) as u32) & 0xFF) as u8;"),
        "{src}"
    );
    assert!(src.contains("set_al(tmp);"), "{src}");
}

// ----------------------------------------------------------------------------
// The Rust backend does not specialize int 21h per AH — register writes happen
// in program order, then `dos_api();` reads the live registers. These two are
// the representatives for the collapsed per-AH DOS family (see file header).
// ----------------------------------------------------------------------------

#[test]
fn ir_to_c__dos_interrupt_with_known_ah_emits_dos_api() {
    let func = json!({
        "start": 0x0000,
        "instructions": [
            {"address":0x0000,"mnemonic":"mov","op_str":"ah, 9","bytes":"B409"},
            {"address":0x0002,"mnemonic":"int","op_str":"21","bytes":"CD21"},
            {"address":0x0004,"mnemonic":"ret","op_str":"","bytes":"C3"},
        ],
    });
    let src = render_app(&func, &[]);
    assert!(src.contains("set_ah(0x9);"), "{src}");
    assert!(src.contains("dos_api();"), "{src}");
}

#[test]
fn ir_to_c__dos_interrupt_without_ah_uses_generic_call() {
    let func = json!({
        "start": 0x0000,
        "instructions": [
            {"address":0x0000,"mnemonic":"int","op_str":"21","bytes":"CD21"},
            {"address":0x0002,"mnemonic":"ret","op_str":"","bytes":"C3"},
        ],
    });
    let src = render_app(&func, &[]);
    assert!(src.contains("dos_api();"), "{src}");
}

#[test]
fn ir_to_c__dos_interrupt_preserves_unrelated_registers() {
    let func = json!({
        "start": 0x0000,
        "instructions": [
            {"address":0x0000,"mnemonic":"mov","op_str":"cx, 0x1234","bytes":"B93412"},
            {"address":0x0003,"mnemonic":"mov","op_str":"ah, 0x02","bytes":"B402"},
            {"address":0x0005,"mnemonic":"mov","op_str":"dl, 0x41","bytes":"B241"},
            {"address":0x0007,"mnemonic":"int","op_str":"21","bytes":"CD21"},
            {"address":0x0009,"mnemonic":"ret","op_str":"","bytes":"C3"},
        ],
    });
    let src = render_app(&func, &[]);
    assert!(src.contains("set_cx(0x1234);"), "{src}");
    assert!(src.contains("set_ah(0x2);"), "{src}");
    assert!(src.contains("set_dl(0x41);"), "{src}");
    // all register writes execute, in program order, before the DOS call
    let cx = src.find("set_cx(0x1234);").unwrap();
    let dl = src.find("set_dl(0x41);").unwrap();
    let call = src.find("dos_api();").unwrap();
    assert!(cx < dl && dl < call, "{src}");
}

#[test]
fn ir_to_c__interrupt_emits_run_interrupt() {
    let func = json!({
        "start": 0x0000,
        "instructions": [
            {"address":0x0000,"mnemonic":"int","op_str":"60","bytes":"CD60"},
            {"address":0x0002,"mnemonic":"ret","op_str":"","bytes":"C3"},
        ],
    });
    let src = render_app(&func, &[]);
    assert!(src.contains("set_ip(0x0000);"), "{src}");
    assert!(src.contains("run_interrupt(0x60);"), "{src}");
}

#[test]
fn ir_to_c__interrupt_1a_emits_run_interrupt() {
    let func = json!({
        "start": 0x0000,
        "instructions": [
            {"address":0x0000,"mnemonic":"int","op_str":"1a","bytes":"CD1A"},
            {"address":0x0002,"mnemonic":"ret","op_str":"","bytes":"C3"},
        ],
    });
    let src = render_app(&func, &[]);
    assert!(src.contains("set_ip(0x0000);"), "{src}");
    assert!(src.contains("run_interrupt(0x1A);"), "{src}");
}

#[test]
fn ir_to_c__mov_ax_without_interrupt_is_emitted() {
    let func = json!({
        "start": 0x0000,
        "instructions": [
            {"address":0x0000,"mnemonic":"mov","op_str":"ax, 1234h","bytes":"B83412"},
            {"address":0x0003,"mnemonic":"ret","op_str":"","bytes":"C3"},
        ],
    });
    let src = render_app(&func, &[]);
    assert!(src.contains("set_ax(0x1234);"), "{src}");
}

#[test]
fn ir_to_c__lds_loads_far_pointer_for_dos_call() {
    let func = json!({
        "start": 0x0000,
        "instructions": [
            {"address":0x0000,"mnemonic":"mov","op_str":"ax, 0x2560","bytes":"B86025"},
            {"address":0x0003,"mnemonic":"lds","op_str":"dx, ptr cs:[0x8b9]","bytes":"2EC516B90800"},
            {"address":0x0009,"mnemonic":"int","op_str":"21","bytes":"CD21"},
            {"address":0x000B,"mnemonic":"ret","op_str":"","bytes":"C3"},
        ],
    });
    let src = render_app(&func, &[]);
    // seg word read from [mem+2] BEFORE the offset overwrites dx / ds changes
    assert!(src.contains("let _far_seg = memw(cs(), 0x08BB);"), "{src}");
    assert!(src.contains("set_dx(memw(cs(), 0x08B9));"), "{src}");
    assert!(src.contains("set_ds(_far_seg);"), "{src}");
    assert!(src.contains("dos_api();"), "{src}");
}

#[test]
fn ir_to_c__les_loads_far_pointer() {
    let func = json!({
        "start": 0x0000,
        "instructions": [
            {"address":0x0000,"mnemonic":"les","op_str":"di, ptr cs:[0xf60]","bytes":"2EC43E600F"},
            {"address":0x0005,"mnemonic":"ret","op_str":"","bytes":"C3"},
        ],
    });
    let src = render_app(&func, &[]);
    assert!(src.contains("let _far_seg = memw(cs(), 0x0F62);"), "{src}");
    assert!(src.contains("set_di(memw(cs(), 0x0F60));"), "{src}");
    assert!(src.contains("set_es(_far_seg);"), "{src}");
}

// ----------------------------------------------------------------------------
// The Rust backend does not specialize int 10h per AH either — the register
// writes execute, then run_interrupt(0x10) reads them. Representative for the
// collapsed per-AH BIOS family (see file header).
// ----------------------------------------------------------------------------

#[test]
fn ir_to_c__bios_video_mode_interrupt_emits_run_interrupt() {
    let func = json!({
        "start": 0x0000,
        "instructions": [
            {"address":0x0000,"mnemonic":"mov","op_str":"ax, 13h","bytes":"B81300"},
            {"address":0x0003,"mnemonic":"int","op_str":"10","bytes":"CD10"},
            {"address":0x0005,"mnemonic":"ret","op_str":"","bytes":"C3"},
        ],
    });
    let src = render_app(&func, &[]);
    assert!(src.contains("set_ax(0x13);"), "{src}");
    assert!(src.contains("run_interrupt(0x10);"), "{src}");
}

// ============================================================================
// ============================================================================

#[test]
fn jump_function_entry__jmp_known_function_entry_without_block_sets_pc() {
    let func = json!({
        "start": 0x0000,
        "instructions": [
            {"address":0x0000,"mnemonic":"jmp","op_str":"0100","bytes":"E9FD00","target":0x0100},
            {"address":0x0003,"mnemonic":"ret","op_str":"","bytes":"C3"},
        ],
    });
    let src = render_plain(&func, &[0x0000, 0x0100]);
    assert!(src.contains("pc = 0x0100;"), "{src}");
    assert!(!src.contains("func_0100()"), "{src}");
}

#[test]
fn jump_function_entry__jmp_known_function_entry_with_block_sets_pc() {
    let func = json!({
        "start": 0x0000,
        "instructions": [
            {"address":0x0000,"mnemonic":"jmp","op_str":"0006","bytes":"E90300","target":0x0006},
            {"address":0x0003,"mnemonic":"ret","op_str":"","bytes":"C3"},
            {"address":0x0006,"mnemonic":"ret","op_str":"","bytes":"C3"},
        ],
    });
    let src = render_plain(&func, &[0x0000, 0x0006]);
    assert!(src.contains("pc = 0x0006;"), "{src}");
    // the target block is its own dispatch arm, not a called function
    assert!(src.contains("0x0006 => {"), "{src}");
    assert!(!src.contains("func_0006()"), "{src}");
}

// ============================================================================
// ============================================================================

#[test]
fn jump_table__jmp_word_ptr_cs_uses_jump_table() {
    let func = json!({
        "start": 0x0000,
        "instructions": [
            {"address":0x0000,"mnemonic":"jmp","op_str":"word ptr cs:[0x10c]","bytes":"2EFF260C01",
             "detail":{"mem_refs":[{"segment":"CS","disp":0x10C,"access":"read"}]}},
            {"address":0x0005,"mnemonic":"ret","op_str":"","bytes":"C3"},
        ],
    });
    let src = render_plain(&func, &[]);
    assert!(
        src.contains("jump_table_((((cs() as u32) << 4).wrapping_add((memw(cs(), 0x10C)) as u32)) & 0xFFFFF, expected_retip);"),
        "{src}"
    );
    assert_jump_table(&src);
}

#[test]
fn jump_table__jmp_word_ptr_cs_with_bp_index_uses_jump_table() {
    let func = json!({
        "start": 0x0000,
        "instructions": [
            {"address":0x0000,"mnemonic":"jmp","op_str":"word ptr cs:[bp + 0x10c]","bytes":"2EFFA60C01",
             "detail":{"mem_refs":[{"segment":"CS","disp":0x10C,"access":"read"}]}},
            {"address":0x0005,"mnemonic":"ret","op_str":"","bytes":"C3"},
        ],
    });
    let src = render_plain(&func, &[]);
    assert!(
        src.contains("jump_table_((((cs() as u32) << 4).wrapping_add((memw(cs(), (((bp() as u32).wrapping_add(0x10Cu32)) & 0xFFFF) as u16)) as u32)) & 0xFFFFF, expected_retip);"),
        "{src}"
    );
    assert_jump_table(&src);
}

#[test]
fn jump_table__jmp_word_ptr_cs_with_bx_register_uses_jump_table() {
    let func = json!({
        "start": 0x0000,
        "instructions": [
            {"address":0x0000,"mnemonic":"jmp","op_str":"word ptr cs:[bx]","bytes":"FF27",
             "detail":{"mem_refs":[{"segment":"CS","disp":0x0,"access":"read"}]}},
            {"address":0x0002,"mnemonic":"ret","op_str":"","bytes":"C3"},
        ],
    });
    let src = render_plain(&func, &[]);
    assert!(
        src.contains("jump_table_((((cs() as u32) << 4).wrapping_add((memw(cs(), bx())) as u32)) & 0xFFFFF, expected_retip);"),
        "{src}"
    );
    assert_jump_table(&src);
}

#[test]
fn jump_table__jmp_known_function_sets_pc() {
    let func = json!({
        "start": 0x0000,
        "instructions": [
            {"address":0x0000,"mnemonic":"jmp","op_str":"0100","bytes":"E9FD00","target":0x0100},
            {"address":0x0003,"mnemonic":"ret","op_str":"","bytes":"C3"},
        ],
    });
    let src = render_plain(&func, &[0x0000, 0x0100]);
    assert!(src.contains("pc = 0x0100;"), "{src}");
    assert!(!src.contains("func_0100()"), "{src}");
    assert!(!src.contains("jump_table_"), "{src}");
}

#[test]
fn jump_table__jmp_word_ptr_es_with_bx_uses_jump_table() {
    let func = json!({
        "start": 0x0000,
        "instructions": [
            {"address":0x0000,"mnemonic":"jmp","op_str":"word ptr es:[bx]","bytes":"26FF27",
             "detail":{"mem_refs":[{"segment":"ES","disp":0x0,"access":"read"}]}},
            {"address":0x0003,"mnemonic":"ret","op_str":"","bytes":"C3"},
        ],
    });
    let src = render_plain(&func, &[]);
    assert!(
        src.contains("jump_table_((((cs() as u32) << 4).wrapping_add((memw(es(), bx())) as u32)) & 0xFFFFF, expected_retip);"),
        "{src}"
    );
    assert_jump_table(&src);
}

#[test]
fn jump_table__jmp_word_ptr_es_with_offset_uses_jump_table() {
    let func = json!({
        "start": 0x0000,
        "instructions": [
            {"address":0x0000,"mnemonic":"jmp","op_str":"word ptr es:[0x010C]","bytes":"26FF2E0C01",
             "detail":{"mem_refs":[{"segment":"ES","disp":0x10C,"access":"read"}]}},
            {"address":0x0005,"mnemonic":"ret","op_str":"","bytes":"C3"},
        ],
    });
    let src = render_plain(&func, &[]);
    assert!(
        src.contains("jump_table_((((cs() as u32) << 4).wrapping_add((memw(es(), 0x10C)) as u32)) & 0xFFFFF, expected_retip);"),
        "{src}"
    );
    assert_jump_table(&src);
}

#[test]
fn jump_table__jmp_word_ptr_without_segment_uses_ds() {
    let func = json!({
        "start": 0x0000,
        "instructions": [
            {"address":0x0000,"mnemonic":"jmp","op_str":"word ptr [0x010C]","bytes":"3EFF260C01",
             "detail":{"mem_refs":[{"segment":"DS","disp":0x10C,"access":"read"}]}},
            {"address":0x0005,"mnemonic":"ret","op_str":"","bytes":"C3"},
        ],
    });
    let src = render_plain(&func, &[]);
    assert!(
        src.contains("jump_table_((((cs() as u32) << 4).wrapping_add((memw(ds(), 0x10C)) as u32)) & 0xFFFFF, expected_retip);"),
        "{src}"
    );
    assert_jump_table(&src);
}

#[test]
fn jump_table__jmp_word_ptr_bp_defaults_to_ss() {
    let func = json!({
        "start": 0x0000,
        "instructions": [
            {"address":0x0000,"mnemonic":"jmp","op_str":"word ptr [bp + 0x10c]","bytes":"36FFA60C01",
             "detail":{"mem_refs":[{"segment":"SS","disp":0x10C,"access":"read"}]}},
            {"address":0x0005,"mnemonic":"ret","op_str":"","bytes":"C3"},
        ],
    });
    let src = render_plain(&func, &[]);
    assert!(
        src.contains("jump_table_((((cs() as u32) << 4).wrapping_add((memw(ss(), (((bp() as u32).wrapping_add(0x10Cu32)) & 0xFFFF) as u16)) as u32)) & 0xFFFFF, expected_retip);"),
        "{src}"
    );
    assert_jump_table(&src);
}

#[test]
fn jump_table__jmp_word_ptr_cs_with_negative_offset_uses_jump_table() {
    let func = json!({
        "start": 0x0000,
        "instructions": [
            {"address":0x0000,"mnemonic":"jmp","op_str":"word ptr cs:[bx - 0x10]","bytes":"2EFF67F0",
             "detail":{"mem_refs":[{"segment":"CS","disp":-0x10,"access":"read"}]}},
            {"address":0x0004,"mnemonic":"ret","op_str":"","bytes":"C3"},
        ],
    });
    let src = render_plain(&func, &[]);
    assert!(
        src.contains("jump_table_((((cs() as u32) << 4).wrapping_add((memw(cs(), (((bx() as u32).wrapping_sub(0x10u32)) & 0xFFFF) as u16)) as u32)) & 0xFFFFF, expected_retip);"),
        "{src}"
    );
    assert_jump_table(&src);
}

// ============================================================================
// es:[0xFFxx] operands are RCB (runtime-control-block) fields; the Rust
// backend rewrites reads on the indirect-jump/far-pointer paths through the
// rcb_read16 helper with the named field. (The old port-divergence notes about
// a missing match_rcb_access rewrite applied to the deleted C renderer path.)
// ============================================================================

#[test]
fn jump_table_rcb__jmp_word_ptr_es_rcb_field_uses_rcb_read16() {
    let func = json!({
        "start": 0x0000,
        "instructions": [
            {"address":0x0000,"mnemonic":"jmp","op_str":"word ptr es:[0xFF0C]","bytes":"26FF2E0CFF",
             "detail":{"mem_refs":[{"segment":"ES","disp":0xFF0C,"access":"read"}]}},
            {"address":0x0005,"mnemonic":"ret","op_str":"","bytes":"C3"},
        ],
    });
    let src = render_plain(&func, &[]);
    assert!(
        src.contains("jump_table_((((cs() as u32) << 4).wrapping_add((rcb_read16(DATA_BUF1_OFF)) as u32)) & 0xFFFFF, expected_retip);"),
        "{src}"
    );
    assert_jump_table(&src);
}

#[test]
fn jump_table_rcb__ljmp_es_rcb_fields_use_rcb_read16() {
    let func = json!({
        "start": 0x0000,
        "instructions": [
            {"address":0x0000,"mnemonic":"ljmp","op_str":"es:[0xFF04]","bytes":"26FF2E04FF",
             "detail":{"mem_refs":[{"segment":"ES","disp":0xFF04,"access":"read"}]}},
            {"address":0x0005,"mnemonic":"ret","op_str":"","bytes":"C3"},
        ],
    });
    let src = render_plain(&func, &[]);
    assert!(
        src.contains(
            "long_jump_(rcb_read16(PREV_TIMER_VECTOR_SEG), rcb_read16(PREV_TIMER_VECTOR_OFF));"
        ),
        "{src}"
    );
}

#[test]
fn jump_table_rcb__jmp_word_ptr_es_lowercase_hex() {
    let func = json!({
        "start": 0x0000,
        "instructions": [
            {"address":0x0000,"mnemonic":"jmp","op_str":"word ptr es:[0xff0c]","bytes":"26FF2E0CFF",
             "detail":{"mem_refs":[{"segment":"ES","disp":0xFF0C,"access":"read"}]}},
            {"address":0x0005,"mnemonic":"ret","op_str":"","bytes":"C3"},
        ],
    });
    let src = render_plain(&func, &[]);
    assert!(
        src.contains("jump_table_((((cs() as u32) << 4).wrapping_add((rcb_read16(DATA_BUF1_OFF)) as u32)) & 0xFFFFF, expected_retip);"),
        "{src}"
    );
    assert_jump_table(&src);
}

// ============================================================================
// ============================================================================

#[test]
fn label_goto__jmp_to_label_sets_pc() {
    // (was: renderer() + func_names poke asserting no label_0340: label/goto —
    // the flat state machine has no label channel; the semantic core is the
    // pc transfer into the target block's own arm.)
    let func = json!({
        "start": 0x0000,
        "instructions": [
            {"address":0x0000,"mnemonic":"jmp","op_str":"0x340","bytes":"E93D03","target":0x0340},
            {"address":0x0340,"mnemonic":"ret","op_str":"","bytes":"C3"},
        ],
    });
    let src = render_plain(&func, &[0x0000]);
    assert!(src.contains("pc = 0x0340;"), "{src}");
    assert!(src.contains("0x0340 => {"), "{src}");
    assert!(!src.contains("label_0340"), "{src}");
}

// ============================================================================
// ============================================================================

#[test]
fn lahf_sahf__lahf_sahf_translate_flags() {
    let func = json!({
        "start": 0x0000,
        "instructions": [
            {"address":0x0000,"mnemonic":"lahf","op_str":"","bytes":"9F"},
            {"address":0x0001,"mnemonic":"sahf","op_str":"","bytes":"9E"},
            {"address":0x0002,"mnemonic":"ret","op_str":"","bytes":"C3"},
        ],
    });
    let src = render_plain(&func, &[]);
    assert!(
        src.contains("set_ah((SF() << 7) | (ZF() << 6) | (PF() << 2) | 0x02 | CF());"),
        "{src}"
    );
    assert!(src.contains("set_SF((ah() >> 7) & 1);"), "{src}");
    assert!(src.contains("set_ZF((ah() >> 6) & 1);"), "{src}");
    assert!(src.contains("set_PF((ah() >> 2) & 1);"), "{src}");
    assert!(src.contains("set_CF(ah() & 1);"), "{src}");
}

// ============================================================================
// ============================================================================

#[test]
fn lcall__lcall_cs_indirect() {
    let func = json!({
        "start": 0x0000,
        "instructions": [
            {"address":0x0000,"mnemonic":"lcall","op_str":"cs:[0xff10]","bytes":"2EFF1E10FF",
             "detail":{"mem_refs":[{"segment":"CS","disp":0xFF10,"access":"read"}]}},
            {"address":0x0005,"mnemonic":"ret","op_str":"","bytes":"C3"},
        ],
    });
    let src = render_plain(&func, &[]);
    assert!(
        src.contains("lcall_table_(((0x5u32).wrapping_add(0x10100).wrapping_sub((cs() as u32) << 4)) as u16, memw(cs(), 0xFF12), memw(cs(), 0xFF10));"),
        "{src}"
    );
}

#[test]
fn lcall__lcall_cs_indirect_other_offset() {
    let func = json!({
        "start": 0x0000,
        "instructions": [
            {"address":0x0000,"mnemonic":"lcall","op_str":"cs:[0xff0c]","bytes":"2EFF1E0CFF",
             "detail":{"mem_refs":[{"segment":"CS","disp":0xFF0C,"access":"read"}]}},
            {"address":0x0005,"mnemonic":"ret","op_str":"","bytes":"C3"},
        ],
    });
    let src = render_plain(&func, &[]);
    assert!(
        src.contains("lcall_table_(((0x5u32).wrapping_add(0x10100).wrapping_sub((cs() as u32) << 4)) as u16, memw(cs(), 0xFF0E), memw(cs(), 0xFF0C));"),
        "{src}"
    );
}

#[test]
fn lcall__lcall_indirect_register() {
    let func = json!({
        "start": 0x0000,
        "instructions": [
            {"address":0x0000,"mnemonic":"lcall","op_str":"[bx]","bytes":"FF1F",
             "detail":{"mem_refs":[{"segment":"DS","base":"BX","index":null,"scale":1,"disp":0,"access":"read"}]}},
            {"address":0x0005,"mnemonic":"ret","op_str":"","bytes":"C3"},
        ],
    });
    let src = render_plain(&func, &[]);
    assert!(
        src.contains("lcall_table_(((0x5u32).wrapping_add(0x10100).wrapping_sub((cs() as u32) << 4)) as u16, memw(ds(), (((bx() as u32).wrapping_add(0x2u32)) & 0xFFFF) as u16), memw(ds(), bx()));"),
        "{src}"
    );
}

#[test]
fn lcall__call_after_push_cs_pop_ds_is_near() {
    let func = json!({
        "start": 0x0000,
        "instructions": [
            {"address":0x0000,"mnemonic":"push","op_str":"cs","bytes":"0E"},
            {"address":0x0001,"mnemonic":"pop","op_str":"ds","bytes":"1F"},
            {"address":0x0002,"mnemonic":"call","op_str":"word ptr cs:[0x1000]","bytes":"2EFF161000",
             "detail":{"mem_refs":[{"segment":"CS","disp":0x1000,"access":"read"}]}},
            {"address":0x0007,"mnemonic":"ret","op_str":"","bytes":"C3"},
        ],
    });
    let src = render_plain(&func, &[]);
    assert!(
        src.contains("call_table_(((0x7u32).wrapping_add(0x10100).wrapping_sub((cs() as u32) << 4)) as u16, (((cs() as u32) << 4).wrapping_add((memw(cs(), 0x1000)) as u32)) & 0xFFFFF);"),
        "{src}"
    );
    assert!(!src.contains("lcall_table_"), "{src}");
}

#[test]
fn lcall__call_after_push_cs_with_intervening_instruction_is_near() {
    let func = json!({
        "start": 0x0000,
        "instructions": [
            {"address":0x0000,"mnemonic":"push","op_str":"cs","bytes":"0E"},
            {"address":0x0001,"mnemonic":"mov","op_str":"ax, ax","bytes":"89C0"},
            {"address":0x0003,"mnemonic":"call","op_str":"word ptr cs:[0x1000]","bytes":"2EFF161000",
             "detail":{"mem_refs":[{"segment":"CS","disp":0x1000,"access":"read"}]}},
            {"address":0x0008,"mnemonic":"ret","op_str":"","bytes":"C3"},
        ],
    });
    let src = render_plain(&func, &[]);
    assert!(
        src.contains("call_table_(((0x8u32).wrapping_add(0x10100).wrapping_sub((cs() as u32) << 4)) as u16, (((cs() as u32) << 4).wrapping_add((memw(cs(), 0x1000)) as u32)) & 0xFFFFF);"),
        "{src}"
    );
    assert!(!src.contains("lcall_table_"), "{src}");
}

// ============================================================================
// ============================================================================

#[test]
fn lodsb__rep_lodsb_loads_last_byte_and_updates_regs() {
    let func = json!({
        "start": 0x0000,
        "instructions": [
            {"address":0x0000,"mnemonic":"rep lodsb","op_str":"","bytes":"F3AC"},
            {"address":0x0002,"mnemonic":"ret","op_str":"","bytes":"C3"},
        ],
    });
    let src = render_plain(&func, &[]);
    assert!(src.contains("if cx() != 0 {"), "{src}");
    assert!(
        src.contains("let delta: i32 = if DF() != 0 { -1 } else { 1 };"),
        "{src}"
    );
    assert!(
        src.contains(
            "set_al(memb(ds(), ((si() as i32 + (cx() as i32 - 1) * delta) & 0xFFFF) as u16));"
        ),
        "{src}"
    );
    assert!(
        src.contains("set_si(((si() as i32 + cx() as i32 * delta) & 0xFFFF) as u16);"),
        "{src}"
    );
    assert!(src.contains("set_cx(0);"), "{src}");
}

#[test]
fn lodsb__lodsb_respects_source_segment_override() {
    let func = json!({
        "start": 0x0000,
        "instructions": [
            {"address":0x0000,"mnemonic":"lodsb","op_str":"","bytes":"2EAC",
             "detail":{"mem_refs":[{"segment":"CS","disp":0,"access":"read"}]}},
            {"address":0x0002,"mnemonic":"ret","op_str":"","bytes":"C3"},
        ],
    });
    let src = render_plain(&func, &[]);
    assert!(src.contains("set_al(memb(cs(), si()));"), "{src}");
}

// ============================================================================
// ============================================================================

#[test]
fn long_jump__ljmp_uses_long_jump() {
    let func = json!({
        "start": 0x0000,
        "instructions": [
            {"address":0x0000,"mnemonic":"ljmp","op_str":"0x2000:0x1000","bytes":"EA00100020"},
            {"address":0x0005,"mnemonic":"ret","op_str":"","bytes":"C3"},
        ],
    });
    let src = render_plain(&func, &[]);
    assert!(src.contains("long_jump_(0x2000, 0x1000);"), "{src}");
    let body = arm(&src, 0x0000);
    assert!(body.contains("return;"), "{body}");
}

#[test]
fn long_jump__ljmp_memory_operand() {
    let func = json!({
        "start": 0x0000,
        "instructions": [
            {"address":0x0000,"mnemonic":"ljmp","op_str":"cs:[0x8ad]","bytes":"2EFF2EAD08"},
            {"address":0x0005,"mnemonic":"ret","op_str":"","bytes":"C3"},
        ],
    });
    let src = render_plain(&func, &[]);
    assert!(
        src.contains("long_jump_(memw(cs(), 0x8AF), memw(cs(), 0x8AD));"),
        "{src}"
    );
    let body = arm(&src, 0x0000);
    assert!(body.contains("return;"), "{body}");
}

#[test]
fn long_jump__ljmp_memory_operand_no_segment_prefix() {
    let func = json!({
        "start": 0x0000,
        "instructions": [
            {"address":0x0000,"mnemonic":"ljmp","op_str":"[0x2ffc]","bytes":"FF2EFC2F",
             "detail":{"mem_refs":[{"segment":"DS","disp":0x2FFC}]}},
            {"address":0x0004,"mnemonic":"ret","op_str":"","bytes":"C3"},
        ],
    });
    let src = render_plain(&func, &[]);
    assert!(
        src.contains("long_jump_(memw(ds(), 0x2FFE), memw(ds(), 0x2FFC));"),
        "{src}"
    );
}

// ============================================================================
// The loop-structure SHAPE (while/do-while) is a C-renderer construct; the
// state machine keeps the semantic content: the break condition, the cx
// decrement with its conditional back edge, and the ret epilogues.
// ============================================================================

#[test]
fn loop_break__loop_with_conditional_break() {
    let func = json!({
        "start": 0x0000,
        "instructions": [
            {"address":0x0000,"mnemonic":"lodsb","op_str":"","bytes":"AC"},
            {"address":0x0001,"mnemonic":"cmp","op_str":"al, 0x2e","bytes":"3C2E"},
            {"address":0x0003,"mnemonic":"je","op_str":"000A","bytes":"7405"},
            {"address":0x0005,"mnemonic":"inc","op_str":"si","bytes":"46"},
            {"address":0x0006,"mnemonic":"loop","op_str":"0000","bytes":"E2F8"},
            {"address":0x0008,"mnemonic":"ret","op_str":"","bytes":"C3"},
            {"address":0x000A,"mnemonic":"ret","op_str":"","bytes":"C3"},
        ],
    });
    let src = render_plain(&func, &[]);
    // the conditional break out of the loop
    assert!(src.contains("if ZF() == 1 {"), "{src}");
    assert!(src.contains("pc = 0x000A;"), "{src}");
    // loop = dec cx + conditional back edge to the header
    assert!(src.contains("set_cx(cx().wrapping_sub(1));"), "{src}");
    assert!(src.contains("if cx() != 0 {"), "{src}");
    assert!(src.contains("pc = 0x0000;"), "{src}");
    // both rets emit their own pop-return epilogue
    assert_eq!(
        src.matches("let popped_ip = memw(ss(), sp());").count(),
        2,
        "{src}"
    );
}

#[test]
fn loop_break__loop_conditional_return() {
    let func = json!({
        "start": 0x0000,
        "instructions": [
            {"address":0x0000,"mnemonic":"nop","op_str":"","bytes":"90"},
            {"address":0x0001,"mnemonic":"jmp","op_str":"0006","bytes":"E90200","target":0x0006},
            {"address":0x0006,"mnemonic":"cmp","op_str":"cx, 3","bytes":"83F903"},
            {"address":0x0009,"mnemonic":"jne","op_str":"000E","bytes":"7503"},
            {"address":0x000B,"mnemonic":"ret","op_str":"","bytes":"C3"},
            {"address":0x000E,"mnemonic":"dec","op_str":"cx","bytes":"49"},
            {"address":0x000F,"mnemonic":"jmp","op_str":"0000","bytes":"E9EEFF","target":0x0000},
        ],
    });
    let src = render_plain(&func, &[]);
    // the conditional exit: jne branches around the ret
    assert!(src.contains("if ZF() == 0 {"), "{src}");
    assert!(src.contains("pc = 0x000E;"), "{src}");
    assert!(src.contains("let popped_ip = memw(ss(), sp());"), "{src}");
    // the dec step and the back edge
    assert!(
        src.contains("set_cx((old.wrapping_sub(1) & 0xFFFF) as u16);"),
        "{src}"
    );
    assert!(src.contains("pc = 0x0000;"), "{src}");
}

#[test]
fn loop_break__xor_self_clears_with_flags() {
    let func = json!({
        "start": 0x0000,
        "instructions": [
            {"address":0x0000,"mnemonic":"xor","op_str":"ah, ah","bytes":"32E4"},
            {"address":0x0002,"mnemonic":"ret","op_str":"","bytes":"C3"},
        ],
    });
    let src = render_plain(&func, &[]);
    // self-xor lowers to the flag-setting helper — no inline flag writes
    assert!(src.contains("xor8(ah_ptr(), ah());"), "{src}");
    assert!(!src.contains("set_ZF"), "{src}");
}

#[test]
fn loop_break__loop_invalidates_cx_before_dos_int() {
    // (C asserted the write-file call read live `cx`, not the cached mov value;
    // Rust registers are written immediately — pin the loop decrement and that
    // dos_api() follows the register setup.)
    let func = json!({
        "start": 0x0000,
        "instructions": [
            {"address":0x0000,"mnemonic":"mov","op_str":"cx, 5","bytes":"B90500"},
            {"address":0x0003,"mnemonic":"loop","op_str":"0x3","bytes":"E2FE"},
            {"address":0x0005,"mnemonic":"mov","op_str":"ah, 0x40","bytes":"B440"},
            {"address":0x0007,"mnemonic":"mov","op_str":"bx, 1","bytes":"BB0100"},
            {"address":0x000A,"mnemonic":"mov","op_str":"dx, 0x1000","bytes":"BA0010"},
            {"address":0x000D,"mnemonic":"int","op_str":"21","bytes":"CD21"},
            {"address":0x000F,"mnemonic":"ret","op_str":"","bytes":"C3"},
        ],
    });
    let src = render_plain(&func, &[]);
    assert!(src.contains("set_cx(0x5);"), "{src}");
    assert!(src.contains("set_cx(cx().wrapping_sub(1));"), "{src}");
    assert!(src.contains("pc = 0x0003;"), "{src}");
    assert!(src.contains("set_ah(0x40);"), "{src}");
    assert!(src.contains("dos_api();"), "{src}");
    assert!(
        src.find("set_ah(0x40);").unwrap() < src.find("dos_api();").unwrap(),
        "{src}"
    );
}

// ============================================================================
// ============================================================================

#[test]
fn movsb_stosb__movsb_copies_byte_and_increments() {
    let func = json!({
        "start": 0x0000,
        "instructions": [
            {"address":0x0000,"mnemonic":"movsb","op_str":"byte ptr es:[di], byte ptr [si]","bytes":"A4"},
            {"address":0x0001,"mnemonic":"ret","op_str":"","bytes":"C3"},
        ],
    });
    let src = render_plain(&func, &[]);
    assert!(
        src.contains("let delta: i32 = if DF() != 0 { -1 } else { 1 };"),
        "{src}"
    );
    assert!(
        src.contains("memb_write(es(), di(), memb(ds(), si()));"),
        "{src}"
    );
    assert!(
        src.contains("set_si(((si() as i32 + delta) & 0xFFFF) as u16);"),
        "{src}"
    );
    assert!(
        src.contains("set_di(((di() as i32 + delta) & 0xFFFF) as u16);"),
        "{src}"
    );
}

#[test]
fn movsb_stosb__rep_movsb_loops_and_updates_regs() {
    let func = json!({
        "start": 0x0000,
        "instructions": [
            {"address":0x0000,"mnemonic":"rep movsb","op_str":"byte ptr es:[di], byte ptr [si]","bytes":"F3A4"},
            {"address":0x0002,"mnemonic":"ret","op_str":"","bytes":"C3"},
        ],
    });
    let src = render_plain(&func, &[]);
    assert!(src.contains("rep_movsb_block(es(), ds());"), "{src}");
}

#[test]
fn movsb_stosb__stosb_stores_byte_and_increments() {
    let func = json!({
        "start": 0x0000,
        "instructions": [
            {"address":0x0000,"mnemonic":"stosb","op_str":"byte ptr es:[di], al","bytes":"AA"},
            {"address":0x0001,"mnemonic":"ret","op_str":"","bytes":"C3"},
        ],
    });
    let src = render_plain(&func, &[]);
    assert!(
        src.contains("let delta: i32 = if DF() != 0 { -1 } else { 1 };"),
        "{src}"
    );
    assert!(src.contains("memb_write(es(), di(), al());"), "{src}");
    assert!(
        src.contains("set_di(((di() as i32 + delta) & 0xFFFF) as u16);"),
        "{src}"
    );
}

#[test]
fn movsb_stosb__rep_stosb_uses_block_helper_and_updates_regs() {
    let func = json!({
        "start": 0x0000,
        "instructions": [
            {"address":0x0000,"mnemonic":"rep stosb","op_str":"byte ptr es:[di], al","bytes":"F3AA"},
            {"address":0x0002,"mnemonic":"ret","op_str":"","bytes":"C3"},
        ],
    });
    let src = render_plain(&func, &[]);
    assert!(src.contains("rep_stosb_block(es());"), "{src}");
}

#[test]
fn movsb_stosb__movsb_respects_source_segment_override() {
    let func = json!({
        "start": 0x0000,
        "instructions": [
            {"address":0x0000,"mnemonic":"movsb","op_str":"byte ptr es:[di], byte ptr [si]","bytes":"2EA4",
             "detail":{"mem_refs":[
                {"segment":"CS","disp":0,"access":"read"},
                {"segment":"ES","disp":0,"access":"write"}
             ]}},
            {"address":0x0002,"mnemonic":"ret","op_str":"","bytes":"C3"},
        ],
    });
    let src = render_plain(&func, &[]);
    assert!(
        src.contains("memb_write(es(), di(), memb(cs(), si()));"),
        "{src}"
    );
}

// ============================================================================
// ============================================================================

#[test]
fn movsw__movsw_copies_word_and_increments() {
    let func = json!({
        "start": 0x0000,
        "instructions": [
            {"address":0x0000,"mnemonic":"movsw","op_str":"word ptr es:[di], word ptr [si]","bytes":"A5"},
            {"address":0x0001,"mnemonic":"ret","op_str":"","bytes":"C3"},
        ],
    });
    let src = render_plain(&func, &[]);
    assert!(
        src.contains("let delta: i32 = if DF() != 0 { -2 } else { 2 };"),
        "{src}"
    );
    assert!(
        src.contains("memw_write(es(), di(), memw(ds(), si()));"),
        "{src}"
    );
    assert!(
        src.contains("set_si(((si() as i32 + delta) & 0xFFFF) as u16);"),
        "{src}"
    );
    assert!(
        src.contains("set_di(((di() as i32 + delta) & 0xFFFF) as u16);"),
        "{src}"
    );
}

#[test]
fn movsw__rep_movsw_loops_and_updates_regs() {
    let func = json!({
        "start": 0x0000,
        "instructions": [
            {"address":0x0000,"mnemonic":"rep movsw","op_str":"word ptr es:[di], word ptr [si]","bytes":"F3A5"},
            {"address":0x0002,"mnemonic":"ret","op_str":"","bytes":"C3"},
        ],
    });
    let src = render_plain(&func, &[]);
    assert!(src.contains("rep_movsw_block(es(), ds());"), "{src}");
}

// ============================================================================
// ============================================================================

#[test]
fn no_dead_end__nop_instruction_is_ignored() {
    let func = json!({
        "start": 0x0000,
        "instructions": [
            {"address":0x0000,"mnemonic":"nop","op_str":"","bytes":"90"},
            {"address":0x0001,"mnemonic":"ret","op_str":"","bytes":"C3"},
        ],
    });
    let src = render_plain(&func, &[]);
    assert!(!src.contains("nop"), "{src}");
}

// ============================================================================
// ============================================================================

#[test]
fn not__not_inverts_and_sets_zero_flag() {
    let func = json!({
        "start": 0x0000,
        "instructions": [
            {"address":0x0000,"mnemonic":"not","op_str":"al","bytes":"F6D0"},
            {"address":0x0002,"mnemonic":"ret","op_str":"","bytes":"C3"},
        ],
    });
    let src = render_plain(&func, &[]);
    assert!(
        src.contains("set_al(((!((al()) as u32)) & 0xFF) as u8);"),
        "{src}"
    );
    assert!(src.contains("set_ZF(((al()) == 0) as u8);"), "{src}");
}

#[test]
fn not__not_memory_uses_write_helper() {
    let func = json!({
        "start": 0x0000,
        "instructions": [
            {"address":0x0000,"mnemonic":"not","op_str":"byte ptr cs:[0xff27]","bytes":"2EF61627FF"},
            {"address":0x0005,"mnemonic":"ret","op_str":"","bytes":"C3"},
        ],
    });
    let src = render_plain(&func, &[]);
    assert!(
        src.contains("memb_write(cs(), 0xFF27, ((!((memb(cs(), 0xFF27)) as u32)) & 0xFF) as u8);"),
        "{src}"
    );
    assert!(
        src.contains("set_ZF(((memb(cs(), 0xFF27)) == 0) as u8);"),
        "{src}"
    );
}

// ============================================================================
// ============================================================================

#[test]
fn prefix__name_prefix_applied() {
    // (was: renderer("foo_") + func_names poke; the Rust equivalent contract is
    // the prefix on the emitted dispatch/_impl symbols, with the jmp remaining
    // a pc transfer rather than a prefixed call.)
    let func = json!({
        "start": 0x0000,
        "instructions": [
            {"address":0x0000,"mnemonic":"jmp","op_str":"0x0100","bytes":"E9FD00","target":0x0100},
        ],
    });
    let src = render_rs(&func, &[0x0000], "foo_");
    assert!(src.contains("fn foo_dispatch("), "{src}");
    assert!(src.contains("fn foo_func_0000_impl("), "{src}");
    assert!(src.contains("pc = 0x0100;"), "{src}");
    assert!(!src.contains("foo_func_0100()"), "{src}");
}

// ============================================================================
// ============================================================================

#[test]
fn push_pop__push_register() {
    let func = json!({
        "start": 0x0000,
        "instructions": [
            {"address":0x0000,"mnemonic":"push","op_str":"ax","bytes":"50"},
            {"address":0x0001,"mnemonic":"ret","op_str":"","bytes":"C3"},
        ],
    });
    let src = render_plain(&func, &[]);
    assert!(
        src.contains("set_sp((sp().wrapping_sub(2)) & 0xFFFF);"),
        "{src}"
    );
    assert!(src.contains("memw_write(ss(), sp(), ax());"), "{src}");
}

#[test]
fn push_pop__push_immediate() {
    let func = json!({
        "start": 0x0000,
        "instructions": [
            {"address":0x0000,"mnemonic":"push","op_str":"0x1234","bytes":"683412"},
            {"address":0x0003,"mnemonic":"ret","op_str":"","bytes":"C3"},
        ],
    });
    let src = render_plain(&func, &[]);
    assert!(
        src.contains("set_sp((sp().wrapping_sub(2)) & 0xFFFF);"),
        "{src}"
    );
    assert!(src.contains("memw_write(ss(), sp(), 0x1234);"), "{src}");
}

#[test]
fn push_pop__push_sp_uses_pre_decrement_value() {
    let func = json!({
        "start": 0x0000,
        "instructions": [
            {"address":0x0000,"mnemonic":"push","op_str":"sp","bytes":"54"},
            {"address":0x0001,"mnemonic":"ret","op_str":"","bytes":"C3"},
        ],
    });
    let src = render_plain(&func, &[]);
    assert!(src.contains("let push_value = sp();"), "{src}");
    assert!(
        src.contains("set_sp((sp().wrapping_sub(2)) & 0xFFFF);"),
        "{src}"
    );
    assert!(src.contains("memw_write(ss(), sp(), push_value);"), "{src}");
    // the capture happens BEFORE the decrement (286+ semantics)
    assert!(
        src.find("let push_value = sp();").unwrap()
            < src
                .find("set_sp((sp().wrapping_sub(2)) & 0xFFFF);")
                .unwrap(),
        "{src}"
    );
}

#[test]
fn push_pop__push_memory() {
    let func = json!({
        "start": 0x0000,
        "instructions": [
            {"address":0x0000,"mnemonic":"push","op_str":"word ptr es:[di]","bytes":"26FF35"},
            {"address":0x0003,"mnemonic":"ret","op_str":"","bytes":"C3"},
        ],
    });
    let src = render_plain(&func, &[]);
    assert!(
        src.contains("set_sp((sp().wrapping_sub(2)) & 0xFFFF);"),
        "{src}"
    );
    assert!(
        src.contains("memw_write(ss(), sp(), memw(es(), di()));"),
        "{src}"
    );
}

#[test]
fn push_pop__pop_register() {
    let func = json!({
        "start": 0x0000,
        "instructions": [
            {"address":0x0000,"mnemonic":"pop","op_str":"ax","bytes":"58"},
            {"address":0x0001,"mnemonic":"ret","op_str":"","bytes":"C3"},
        ],
    });
    let src = render_plain(&func, &[]);
    assert!(src.contains("set_ax(memw(ss(), sp()));"), "{src}");
    assert!(
        src.contains("set_sp((sp().wrapping_add(2)) & 0xFFFF);"),
        "{src}"
    );
}

#[test]
fn push_pop__pop_memory() {
    let func = json!({
        "start": 0x0000,
        "instructions": [
            {"address":0x0000,"mnemonic":"pop","op_str":"word ptr [bp-2]","bytes":"8F46FE"},
            {"address":0x0003,"mnemonic":"ret","op_str":"","bytes":"C3"},
        ],
    });
    let src = render_plain(&func, &[]);
    // bp-based operands default to SS; -2 renders as the wrapping +0xFFFE
    assert!(
        src.contains("memw_write(ss(), (((bp() as u32).wrapping_add(0xFFFEu32)) & 0xFFFF) as u16, memw(ss(), sp()));"),
        "{src}"
    );
    assert!(
        src.contains("set_sp((sp().wrapping_add(2)) & 0xFFFF);"),
        "{src}"
    );
}

#[test]
fn push_pop__pop_memory_with_segment_override() {
    let func = json!({
        "start": 0x0000,
        "instructions": [
            {"address":0x0000,"mnemonic":"pop","op_str":"word ptr es:[di]","bytes":"268F05"},
            {"address":0x0003,"mnemonic":"ret","op_str":"","bytes":"C3"},
        ],
    });
    let src = render_plain(&func, &[]);
    assert!(
        src.contains("memw_write(es(), di(), memw(ss(), sp()));"),
        "{src}"
    );
    assert!(
        src.contains("set_sp((sp().wrapping_add(2)) & 0xFFFF);"),
        "{src}"
    );
}

#[test]
fn push_pop__push_pop_pair_preserves_stack_effect() {
    let func = json!({
        "start": 0x0000,
        "instructions": [
            {"address":0x0000,"mnemonic":"push","op_str":"cs","bytes":"0E"},
            {"address":0x0001,"mnemonic":"pop","op_str":"es","bytes":"07"},
            {"address":0x0002,"mnemonic":"ret","op_str":"","bytes":"C3"},
        ],
    });
    let src = render_plain(&func, &[]);
    assert!(
        src.contains("set_sp((sp().wrapping_sub(2)) & 0xFFFF);"),
        "{src}"
    );
    assert!(src.contains("memw_write(ss(), sp(), cs());"), "{src}");
    assert!(src.contains("set_es(memw(ss(), sp()));"), "{src}");
    assert!(
        src.contains("set_sp((sp().wrapping_add(2)) & 0xFFFF);"),
        "{src}"
    );
}
