//! Verifies the DISASM (disasm→JSON) and chunk-render helper paths
//! by porting one representative test from each category.
//!
//! PORT DISPOSITIONS (C backend deleted):
//!   ported: 2 (pc_switch_renderer_emits_cases → dispatch match arms;
//!           forward_jmp_no_label → intra-chunk pc transfer, no call_table_)
mod common;
use common::*;
use serde_json::json;

#[test]
fn pc_switch_renderer_emits_cases() {
    let f = json!({
        "start": 0x0100,
        "instructions": [
            {"address":0x0100,"mnemonic":"cmp","op_str":"ax, 1","bytes":"3D0100"},
            {"address":0x0103,"mnemonic":"jne","op_str":"0109","bytes":"7504"},
            {"address":0x0105,"mnemonic":"mov","op_str":"bx, bx","bytes":"89DB"},
            {"address":0x0107,"mnemonic":"ret","op_str":"","bytes":"C3"},
            {"address":0x0109,"mnemonic":"mov","op_str":"cx, cx","bytes":"89C9"},
            {"address":0x010B,"mnemonic":"ret","op_str":"","bytes":"C3"},
        ],
    });
    let src = render_rs_dispatch(&[f], &[]);
    let match_section = src.split("match pc").nth(1).unwrap();
    assert!(
        match_section.contains("0x0100 => blk_0100(r, expected_retip),"),
        "{match_section}"
    );
    assert!(
        match_section.contains("0x0105 => blk_0105(r, expected_retip),"),
        "{match_section}"
    );
    // The cmp+jne block ends in an if/else where both branches return the
    // next pc (dispatch-level `continue;` no longer exists anywhere).
    let block_0100 = blk(&src, 0x0100);
    assert_eq!(
        block_0100.matches("return 0x0109;").count(),
        1,
        "{block_0100}"
    );
    assert_eq!(
        block_0100.matches("return 0x0105;").count(),
        1,
        "{block_0100}"
    );
    assert!(!src.contains("continue;"), "{src}");
}

#[test]
fn forward_jmp_no_label() {
    let data = [0xE9u8, 0x01, 0x00, 0x90, 0xC3];
    let ir = disasm(&data, &[0x0]);
    let labels = extern_labels(&ir);
    assert!(!labels.contains(&0x0004));
    // A forward jmp inside the function is an intra-chunk transfer: rendered as
    // a direct `return 0x…;` (the next pc) with no dispatch through the call table.
    let funcs = functions(&ir);
    let src = render_rs_ir(&ir, &[], "").expect("emit (forward jmp)");
    assert!(!funcs.is_empty());
    assert!(src.contains("return 0x0004;"), "{src}");
    assert!(!src.contains("call_table_"), "{src}");
}
