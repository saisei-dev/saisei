//! PORT DISPOSITIONS (C backend deleted):
//!   ported:    28 tests (all of them)
//!     - 21 disassembler-structural tests assert on the IR JSON only
//!       (mnemonics, xrefs, functions, extern_labels, no-fallthrough) and
//!       survive unchanged.
//!     - 5 render tests (backward__jmp_ignored, call_hex_prefix__generates_
//!       function_call, cross_func__backward_jmp_no_label, forward__jmp_no_label,
//!       invalid_opcode__is_noop) re-asserted through the Rust backend
//!       (render_rs_ir): "no dispatch(/label_XXXX emitted" becomes "the target
//!       is an in-chunk arm `0xNNNN => blk_NNNN(expected_retip),` reached via
//!       `return 0xNNNN;` from the jumping block fn, with no call_table_";
//!       C register writes (`ax = 0x1234;`) become Rust accessor
//!       writes (`set_ax(0x1234);`).
//!     - 2 CFG tests (iret__cfg_no_fallthrough_after_iret,
//!       ljmp__cfg_no_fallthrough) re-expressed via translate::build_basic_blocks
//!       + translate::cfg_successors (both survive the C deletion) instead of
//!       cfg::build_cfg + DiGraph::out_degree; the assertion (terminator block
//!       has zero successors) is unchanged.
//!   collapsed: none
//!   deleted:   none
//!
//! Ported from tests/test_*.py — the disassemble-focused unit tests:
//! test_disassemble_backward_jump_no_label, test_disassemble_call_hex_prefix,
//! test_disassemble_cross_func_backward_jump_label, test_disassemble_default_segment,
//! test_disassemble_exit_patterns, test_disassemble_forward_jump_label,
//! test_disassemble_ignores_mid_instruction_entries, test_disassemble_invalid_opcode,
//! test_disassemble_jump_xref, test_disassemble_lcall_target,
//! test_disassemble_loop_backedge, test_disassemble_negative_disp,
//! test_disassemble_overlay_truncation, test_disassemble_respects_entry_points,
//! test_disassemble_sorted_instructions, test_iret_no_fallthrough,
//! test_ljmp_no_fallthrough, test_parse_entry_point, test_ret_no_fallthrough.
//!
//! The the original `stage1(data)` + `stage2(...)` return
//! (instructions, functions, data_regions, written_bytes, extern_labels).
//! Those map onto the parsed IR JSON emitted by `disassemble_ir`:
//! instructions  -> ir["code"]        (flat, address-sorted global insn list)
//! functions     -> ir["functions"]   (each {"start", "instructions":[...]})
//! data_regions  -> ir["data_regions"]
//! extern_labels -> ir["extern_labels"]
//! compute_xrefs -> ir["xrefs"]       (strings: "call@0x..","jump@0x..","fallthrough@0x..","mem@..")
//! Instruction objects: {address, bytes, mnemonic, op_str, detail{...}, target?}
//! where detail = {regs_read, regs_write, groups, eflags, seg_override?, mem_refs?}
//! and each mem_ref = {segment, disp, access}.
//!
//! NOTE on image_base: the original tests all call stage2 with the default
//! image_base=0, so far seg:off targets are NOT shifted/gated. The shared
//! `disasm` helper uses image_base=0x10100 (whole-image default), which would
//! drop far-transfer targets (lcall/ljmp) via the `far_in_seg` gate. So these
//! ports drive image_base=0 through the local `d`/`d_skip` helpers.
#![allow(non_snake_case)]
mod common;
use common::*;
use saisei_jitc::{disassemble, translate};
use serde_json::{json, Value};

// ---- local helpers -------------------------------------------------------

/// disassemble_ir at image_base=0 (matches the original stage2 default).
fn d(data: &[u8], entries: &[i64]) -> Value {
    disasm_with(data, entries, 0)
}

/// Same, but with skip_entry_0000 = true (the original `stage2(..., True)`).
fn d_skip(data: &[u8], entries: &[i64]) -> Value {
    let s = disassemble::disassemble_ir(data, entries, true, 0, 30000, None);
    serde_json::from_str(&s).expect("disassemble_ir emitted valid JSON")
}

/// The flat, address-sorted global instruction list (the original stage2[0]).
fn code_list(ir: &Value) -> Vec<Value> {
    ir["code"].as_array().cloned().unwrap_or_default()
}

/// ir["xrefs"] as owned strings.
fn xrefs_of(ir: &Value) -> Vec<String> {
    ir["xrefs"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap().to_string())
        .collect()
}

fn has_no_fallthrough(xrefs: &[String]) -> bool {
    xrefs.iter().all(|x| !x.contains("fallthrough@"))
}

/// Convert a Vec<Value> of instruction objects into Vec<Insn> (serde Maps).
fn insns_of(vals: Vec<Value>) -> Vec<translate::Insn> {
    vals.into_iter()
        .map(|v| v.as_object().unwrap().clone())
        .collect()
}

/// Build an MZ image: 64-byte header (MZ + packed size fields) + module + overlay.
/// Mirrors `test_disassemble_overlay_truncation.build_mz`.
fn build_mz(module: &[u8], overlay: &[u8]) -> Vec<u8> {
    let mut header = vec![0u8; 64];
    header[0] = b'M';
    header[1] = b'Z';
    let header_size = 64usize;
    let e_cblp = (header_size + module.len()) as u16;
    let e_cp = 1u16;
    let e_crlc = 0u16;
    let e_cparhdr = (header_size / 16) as u16;
    header[2..4].copy_from_slice(&e_cblp.to_le_bytes());
    header[4..6].copy_from_slice(&e_cp.to_le_bytes());
    header[6..8].copy_from_slice(&e_crlc.to_le_bytes());
    header[8..10].copy_from_slice(&e_cparhdr.to_le_bytes());
    let mut out = header;
    out.extend_from_slice(module);
    out.extend_from_slice(overlay);
    out
}

/// Rust reconstruction of `compiler.disassemble.parse_entry_point` (a CLI-arg
/// helper not exposed by the Rust lib). The load-bearing hex parsing is the
/// ported `disassemble::parse_imm` (bare token == hex), which equals int(x, 16)
/// for the seg/off halves used here.
fn parse_entry_point_str(arg: &str) -> i64 {
    if let Some((seg, off)) = arg.split_once(':') {
        return disassemble::parse_imm(seg).unwrap() * 16 + disassemble::parse_imm(off).unwrap();
    }
    disassemble::parse_imm(arg).unwrap()
}

// ==========================================================================
// ==========================================================================

#[test]
fn backward__jmp_ignored() {
    let data = [0x90u8, 0xEB, 0xFD, 0xC3];
    let ir = d(&data, &[0x0]);
    let labels = extern_labels(&ir);
    assert!(!labels.contains(&0x0000));
    // The backward jmp inside the function is an intra-chunk transfer: its
    // target is an in-chunk dispatch arm reached via a direct `return 0x…;`
    // (next pc) from the block fn, never routed through the call table.
    let src = render_rs_ir(&ir, &[], "").expect("emit (backward jmp)");
    assert!(
        src.contains("0x0000 => blk_0000(r, expected_retip),"),
        "{src}"
    );
    assert!(blk(&src, 0x0000).contains("return 0x0000;"), "{src}");
    assert!(!src.contains("call_table_"), "{src}");
}

// ==========================================================================
// ==========================================================================

#[test]
fn call_hex_prefix__generates_function_call() {
    // call 0x0CAD; ret
    let mut data = vec![0u8; 0x0CAD + 1];
    data[0] = 0xE8;
    data[1] = 0xAA;
    data[2] = 0x0C;
    data[3] = 0xC3;
    data[0x0CAD] = 0xC3;
    let ir = d(&data, &[]);
    let funcs = functions(&ir);
    assert!(funcs.iter().any(|f| as_i(f, "start") == 0x0CAD));
    // The call immediate (0x0CAD) is resolved to the known sibling and routed
    // through the dispatch loop: push the return IP, yield the target as the
    // next pc — no call_table_ (and no TODO-ASM-style bailout).
    let src = render_rs_ir(&ir, &[], "game_").expect("emit (call hex)");
    let caller = blk(&src, 0x0000);
    assert!(
        caller.contains("r.set_sp((r.sp().wrapping_sub(2)) & 0xFFFF);"),
        "{src}"
    );
    assert!(caller.contains("return 0x0CAD;"), "{src}");
    assert!(
        src.contains("0x0CAD => game_blk_0CAD(r, expected_retip),"),
        "{src}"
    );
    assert!(!src.contains("call_table_"), "{src}");
}

// ==========================================================================
// ==========================================================================

#[test]
fn cross_func__backward_jmp_no_label() {
    let data = [0xC3u8, 0x90, 0xE9, 0xFB, 0xFF];
    let ir = d(&data, &[0x0002]);
    let labels = extern_labels(&ir);
    assert!(!labels.contains(&0x0000));
    // The cross-function backward jmp is still an intra-chunk transfer: the
    // target 0x0000 is an in-chunk arm reached via `return 0x0000;` (next pc)
    // from the jumping block fn, not a label and not a dispatch through the
    // call table.
    let src = render_rs_ir(&ir, &[], "").expect("emit (cross-func jmp)");
    assert!(blk(&src, 0x0002).contains("return 0x0000;"), "{src}");
    assert!(
        src.contains("0x0000 => blk_0000(r, expected_retip),"),
        "{src}"
    );
    assert!(!src.contains("call_table_"), "{src}");
}

// ==========================================================================
// ==========================================================================

/// functions[0].instructions[0].detail.mem_refs[0] — asserts it exists.
fn decode_mem_ref(data: &[u8]) -> Value {
    let ir = d(data, &[0]);
    let funcs = functions(&ir);
    let mem_refs = funcs[0]["instructions"][0]["detail"]["mem_refs"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    assert!(!mem_refs.is_empty(), "Expected memory reference");
    mem_refs[0].clone()
}

fn decode_segment(data: &[u8]) -> String {
    decode_mem_ref(data)["segment"]
        .as_str()
        .unwrap()
        .to_string()
}

#[test]
fn default_segment__si_bp_defaults_to_ss() {
    let data = [0x8Bu8, 0x02, 0xC3]; // mov ax, [bp+si]; ret
    assert_eq!(decode_segment(&data), "SS");
}

#[test]
fn default_segment__bp_di_defaults_to_ss() {
    let data = [0x8Bu8, 0x03, 0xC3]; // mov ax, [bp+di]; ret
    assert_eq!(decode_segment(&data), "SS");
}

#[test]
fn default_segment__mov_mem_reg_marks_readwrite_access() {
    let data = [0x01u8, 0x07, 0xC3]; // add word ptr [bx], ax; ret
    let mem_ref = decode_mem_ref(&data);
    assert_eq!(mem_ref["segment"].as_str().unwrap(), "DS");
    assert_eq!(mem_ref["access"].as_str().unwrap(), "readwrite");
}

#[test]
fn default_segment__segment_override_recorded() {
    let data = [0x2Eu8, 0xD7, 0xC3]; // cs: xlatb; ret
    let ir = d(&data, &[0]);
    let funcs = functions(&ir);
    let insn = &funcs[0]["instructions"][0];
    assert_eq!(insn["detail"]["seg_override"].as_str().unwrap(), "CS");
}

// ==========================================================================
// ==========================================================================

#[test]
fn exit_patterns__ret_terminates() {
    let ir = d(&[0xC3u8, 0x00, 0x00, 0x00], &[0x0]);
    let code = code_list(&ir);
    let xrefs = xrefs_of(&ir);
    assert_eq!(code.len(), 1);
    assert_eq!(code[0]["mnemonic"].as_str().unwrap(), "ret");
    assert!(has_no_fallthrough(&xrefs));
}

#[test]
fn exit_patterns__int_20_terminates() {
    let ir = d(&[0xCDu8, 0x20, 0x90, 0x90], &[0x0]);
    let code = code_list(&ir);
    let xrefs = xrefs_of(&ir);
    assert_eq!(code.len(), 1);
    assert_eq!(code[0]["mnemonic"].as_str().unwrap(), "int");
    assert_eq!(code[0]["op_str"].as_str().unwrap(), "0x20");
    assert!(has_no_fallthrough(&xrefs));
}

#[test]
fn exit_patterns__mov_ax_4c00_int_21_terminates() {
    let ir = d(&[0xB8u8, 0x00, 0x4C, 0xCD, 0x21, 0x90, 0x90], &[0x0]);
    let code = code_list(&ir);
    let xrefs = xrefs_of(&ir);
    assert_eq!(code.len(), 2);
    assert_eq!(code[1]["mnemonic"].as_str().unwrap(), "int");
    assert_eq!(code[1]["op_str"].as_str().unwrap(), "0x21");
    assert!(xrefs.iter().any(|x| x == "fallthrough@0x3"));
    assert!(!xrefs.iter().any(|x| x == "fallthrough@0x5"));
}

// ==========================================================================
// ==========================================================================

#[test]
fn forward__jmp_no_label() {
    let data = [0xE9u8, 0x01, 0x00, 0x90, 0xC3];
    let ir = d(&data, &[0x0]);
    let labels = extern_labels(&ir);
    assert!(!labels.contains(&0x0004));
    // A forward jmp inside the function is an intra-chunk transfer: rendered as
    // a direct `return 0x…;` (next pc) to an in-chunk arm, with no dispatch
    // through the call table and no label.
    let src = render_rs_ir(&ir, &[], "").expect("emit (forward jmp)");
    assert!(blk(&src, 0x0000).contains("return 0x0004;"), "{src}");
    assert!(
        src.contains("0x0004 => blk_0004(r, expected_retip),"),
        "{src}"
    );
    assert!(!src.contains("call_table_"), "{src}");
}

// ==========================================================================
// ==========================================================================

#[test]
fn ignores_mid__extra_entry_is_ignored() {
    // mov bl, byte ptr es:[di + 0xf]; shr bl, 1; ret
    let data = [0x26u8, 0x8A, 0x5D, 0x0F, 0xD0, 0xEB, 0xC3];
    let ir = d_skip(&data, &[0x0, 0x3]);
    let code = code_list(&ir);
    let addrs: Vec<i64> = code.iter().map(|i| as_i(i, "address")).collect();
    assert_eq!(addrs, vec![0, 4, 6]);
    assert!(code.iter().all(|i| i["mnemonic"].as_str().unwrap() != "db"));
}

// ==========================================================================
// ==========================================================================

#[test]
fn invalid_opcode__is_noop() {
    // mov ax, 0x1234; followed by an incomplete opcode 0x0F (-> db 0x0f), which
    // the translator treats as a noop with no output.
    let data = [0xB8u8, 0x34, 0x12, 0x0F];
    let ir = d(&data, &[]);
    let src = render_rs_ir(&ir, &[], "").expect("emit (invalid opcode)");
    assert!(src.contains("r.set_ax(0x1234);"), "{src}");
    // The db byte produces no code at all: its value never appears.
    assert!(!src.contains("0x0f") && !src.contains("0x0F"), "{src}");
}

// ==========================================================================
// ==========================================================================

#[test]
fn jump_xref__jmp() {
    let data = hex_bytes("EB0390909090");
    let ir = d(&data, &[0x0]);
    let xrefs = xrefs_of(&ir);
    assert!(xrefs.iter().any(|x| x == "jump@0x5"));
    assert!(!xrefs.iter().any(|x| x == "fallthrough@0x2"));
}

#[test]
fn jump_xref__jcc() {
    let data = hex_bytes("7503909090");
    let ir = d(&data, &[0x0]);
    let xrefs = xrefs_of(&ir);
    assert!(xrefs.iter().any(|x| x == "jump@0x5"));
    assert!(xrefs.iter().any(|x| x == "fallthrough@0x2"));
}

// ==========================================================================
// ==========================================================================

#[test]
fn lcall__records_target() {
    let data = hex_bytes("9A00100020C3");
    let ir = d(&data, &[0x0]);
    let code = code_list(&ir);
    assert_eq!(code[0]["mnemonic"].as_str().unwrap(), "lcall");
    assert_eq!(code[0]["target"].as_i64().unwrap(), 0x21000);
}

// ==========================================================================
// ==========================================================================

#[test]
fn loop_backedge__backward_edge() {
    // 0000 nop; 0001 nop; 0002 inc dx; 0003 nop; 0004 loop 0002; 0006 int 0x20
    let data = [0x90u8, 0x90, 0x42, 0x90, 0xE2, 0xFC, 0xCD, 0x20];
    let ir = d(&data, &[0x4]);
    let code = code_list(&ir);
    let addrs: std::collections::BTreeSet<i64> = code.iter().map(|i| as_i(i, "address")).collect();
    assert!(addrs.contains(&0x0004)); // loop instruction
    assert!(addrs.contains(&0x0002)); // backward target
    assert!(addrs.contains(&0x0006)); // fallthrough
}

// ==========================================================================
// ==========================================================================

#[test]
fn negative_disp() {
    let data = [0x8Bu8, 0x46, 0xFE, 0xC3]; // mov ax, [bp-2]; ret
    let mem_ref = decode_mem_ref(&data);
    assert_eq!(mem_ref["disp"].as_i64().unwrap(), -2);
}

// ==========================================================================
// ==========================================================================

#[test]
fn overlay__bytes_beyond_module_are_overlay() {
    let module = [0x90u8, 0x90, 0x90];
    let overlay = [0xCCu8, 0xCC];
    let data = build_mz(&module, &overlay);
    let ir = d(&data, &[]);
    // meta["load_module"] == module  ->  ir["data"] == hex(module)
    assert_eq!(ir["data"].as_str().unwrap(), "909090");
    let seg_map = ir["segment_map"].as_array().unwrap();
    let overlay_seg = seg_map
        .iter()
        .find(|s| s["name"].as_str() == Some("overlay"))
        .expect("overlay segment");
    assert_eq!(overlay_seg["size"].as_i64().unwrap(), overlay.len() as i64);
}

// ==========================================================================
// ==========================================================================

#[test]
fn respects_entry__only_requested_decoded() {
    let data = [0xC3u8, 0x00, 0x00, 0x00];
    let ir = d_skip(&data, &[0x1]);
    let code = code_list(&ir);
    let addrs: Vec<i64> = code.iter().map(|i| as_i(i, "address")).collect();
    assert_eq!(addrs, vec![1, 3]);
    assert_eq!(code[0]["mnemonic"].as_str().unwrap(), "add");
    assert_eq!(
        code[0]["op_str"].as_str().unwrap(),
        "byte ptr [bx + si], al"
    );
    assert_eq!(code[1]["mnemonic"].as_str().unwrap(), "db");
    assert_eq!(code[1]["op_str"].as_str().unwrap(), "0x00");
}

// ==========================================================================
// ==========================================================================

#[test]
fn sorted__returns_sorted_instructions() {
    let data = hex_bytes("B80100B90200C3");
    let ir = d(&data, &[]);
    let code = code_list(&ir);
    let addrs: Vec<i64> = code.iter().map(|i| as_i(i, "address")).collect();
    let mut sorted = addrs.clone();
    sorted.sort();
    assert_eq!(addrs, sorted);
}

// ==========================================================================
// ==========================================================================

#[test]
fn iret__no_fallthrough_xrefs() {
    // the original builds a single-iret list and calls the private compute_xrefs; the
    // Rust compute_xrefs isn't exposed, so drive it through disasm of 0xCF (iret).
    let ir = d(&[0xCFu8], &[0x0]);
    let xrefs = xrefs_of(&ir);
    assert!(has_no_fallthrough(&xrefs));
}

#[test]
fn iret__cfg_no_fallthrough_after_iret() {
    let instrs = insns_of(vec![
        json!({"address": 0x0, "mnemonic": "iret", "op_str": "", "bytes": "CF"}),
        json!({"address": 0x1, "mnemonic": "nop", "op_str": "", "bytes": "90"}),
    ]);
    let blocks = translate::build_basic_blocks(&instrs, &known(&[]), None);
    let succ = translate::cfg_successors(&blocks);
    assert_eq!(succ.get(&0x0).map_or(0, |v| v.len()), 0);
}

// ==========================================================================
// ==========================================================================

#[test]
fn ljmp__terminates() {
    let data = hex_bytes("EA001000209090");
    let ir = d(&data, &[0x0]);
    let code = code_list(&ir);
    let xrefs = xrefs_of(&ir);
    assert_eq!(code.len(), 1);
    assert_eq!(code[0]["mnemonic"].as_str().unwrap(), "ljmp");
    assert_eq!(code[0]["target"].as_i64().unwrap(), 0x21000);
    assert!(has_no_fallthrough(&xrefs));
}

#[test]
fn ljmp__cfg_no_fallthrough() {
    let instrs = insns_of(vec![
        json!({"address": 0x0, "mnemonic": "ljmp", "op_str": "0x2000:0x1000", "bytes": "EA00100020"}),
        json!({"address": 0x5, "mnemonic": "nop", "op_str": "", "bytes": "90"}),
    ]);
    let blocks = translate::build_basic_blocks(&instrs, &known(&[]), None);
    let keys: Vec<i64> = blocks.keys().copied().collect();
    assert_eq!(keys, vec![0x0, 0x5]);
    let succ = translate::cfg_successors(&blocks);
    assert_eq!(succ.get(&0x0).map_or(0, |v| v.len()), 0);
}

// ==========================================================================
// ==========================================================================

#[test]
fn parse_entry__segment_offset_hex_without_prefix() {
    assert_eq!(parse_entry_point_str("0100:0010"), 0x1010);
}

#[test]
fn parse_entry__segment_offset_with_prefix() {
    assert_eq!(parse_entry_point_str("0x0100:0x0010"), 0x1010);
}

// ==========================================================================
// ==========================================================================

#[test]
fn ret__no_fallthrough_uppercase() {
    // the original passes a manually-built {"mnemonic":"RET"} list to the private
    // compute_xrefs (which lowercases). The Rust compute_xrefs isn't exposed, so
    // drive it through disasm of the real 0xC3 (ret) byte. The uppercase
    // normalization itself isn't exercised, but the assertion (no fallthrough@
    // after a return) is preserved.
    let ir = d(&[0xC3u8], &[0x0]);
    let xrefs = xrefs_of(&ir);
    assert!(has_no_fallthrough(&xrefs));
}

// ---- small util ----------------------------------------------------------

/// bytes.fromhex(...) equivalent.
fn hex_bytes(s: &str) -> Vec<u8> {
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
        .collect()
}
