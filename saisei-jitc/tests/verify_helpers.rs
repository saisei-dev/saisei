//! Verifies the DISASM (disasm→JSON) and PCSW (render_pc_dispatch) helper paths
//! by porting one representative test from each category.
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
    let src = render_pc_dispatch(&[f], &[]);
    let switch_section = src.split("switch (pc)").nth(1).unwrap();
    assert!(switch_section.contains("case 0x0100:"), "{switch_section}");
    assert!(switch_section.contains("pc = 0x0109;"));
    assert!(switch_section.contains("pc = 0x0105;"));
    let block_0100 = switch_section
        .split("case 0x0100:")
        .nth(1)
        .unwrap()
        .split("case")
        .next()
        .unwrap();
    assert_eq!(block_0100.matches("continue;").count(), 1);
}

#[test]
fn forward_jmp_no_label() {
    let data = [0xE9u8, 0x01, 0x00, 0x90, 0xC3];
    let ir = disasm(&data, &[0x0]);
    let labels = extern_labels(&ir);
    assert!(!labels.contains(&0x0004));
    let funcs = functions(&ir);
    let mut r = renderer("");
    r.extern_labels = labels;
    let start = as_i(&funcs[0], "start");
    let src = r.render_function_c(&funcs[0], &known(&[start])).join("\n");
    assert!(!src.contains("dispatch("), "{src}");
    assert!(!src.contains("label_0004"), "{src}");
}
