//! Shared helpers for the ported compiler unit tests (Rust equivalents of the
//! the original `tests/test_*.py`). Included in each integration-test file via
//! `mod common;`.
#![allow(dead_code)]

use saisei_jitc::disassemble;
use saisei_jitc::ir_to_c::Renderer;
use serde_json::Value;
use std::collections::BTreeSet;

/// Build a known-funcs set from a slice of addresses.
pub fn known(addrs: &[i64]) -> BTreeSet<i64> {
    addrs.iter().copied().collect()
}

/// CCodeRenderer(name_prefix=prefix).render_function(func, known) -> joined C.
/// This drives the BASE (structured, readable-C) renderer, which is what the
/// the original `render_function` test helpers instantiate.
pub fn render_c(func: &Value, known_addrs: &[i64], prefix: &str) -> String {
    let mut r = Renderer::for_test(prefix);
    r.render_function_c(func, &known(known_addrs)).join("\n")
}

/// Same, but returns the Vec<String> lines (for tests that index/inspect lines).
pub fn render_c_lines(func: &Value, known_addrs: &[i64], prefix: &str) -> Vec<String> {
    let mut r = Renderer::for_test(prefix);
    r.render_function_c(func, &known(known_addrs))
}

/// A fresh base renderer; set public fields (extern_labels, program_addrs,
/// code_bytes, comments, reloc_offsets, cs_base, load_segment, load_base) before
/// calling `render_function_c`.
pub fn renderer(prefix: &str) -> Renderer {
    Renderer::for_test(prefix)
}

/// disassemble.disassemble_ir(...) parsed into a serde_json Value. Mirrors a
/// whole-image decode: default flags, image_base default 0x10100 (LOAD_SEG<<4).
pub fn disasm(data: &[u8], entries: &[i64]) -> Value {
    disasm_with(data, entries, 0x10100)
}

pub fn disasm_with(data: &[u8], entries: &[i64], image_base: i64) -> Value {
    let s = disassemble::disassemble_ir(data, entries, false, image_base, 30000, None);
    serde_json::from_str(&s).expect("disassemble_ir emitted valid JSON")
}

/// The `functions` array of a decoded IR.
pub fn functions(ir: &Value) -> Vec<Value> {
    ir.get("functions")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
}

/// The `extern_labels` of a decoded IR as a set.
pub fn extern_labels(ir: &Value) -> BTreeSet<i64> {
    ir.get("extern_labels")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(Value::as_i64).collect())
        .unwrap_or_default()
}

/// PCSwitchRenderer(): render each func through the JIT dispatch-switch path,
/// then return the joined dispatch table (mirrors the original `_dispatch_from`
pub fn render_pc_dispatch(funcs: &[Value], known_addrs: &[i64]) -> String {
    let mut r = Renderer::for_test("");
    let k = known(known_addrs);
    for f in funcs {
        r.render_function(f, &k);
    }
    r.render_dispatch().join("\n")
}

/// PCSwitchRenderer(): render a single func's wrapper (the JIT `_impl` body).
pub fn render_pc(func: &Value, known_addrs: &[i64], prefix: &str) -> Vec<String> {
    let mut r = Renderer::for_test(prefix);
    r.render_function(func, &known(known_addrs))
}

/// Build a `func` dict from (start, instructions) — convenience for tests.
pub fn func(start: i64, instrs: Value) -> Value {
    serde_json::json!({ "start": start, "instructions": instrs })
}

/// Parse an int field from a Value ("start", "address", …).
pub fn as_i(v: &Value, key: &str) -> i64 {
    v.get(key).and_then(Value::as_i64).unwrap_or(0)
}
