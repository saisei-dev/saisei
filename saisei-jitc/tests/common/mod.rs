//! Shared helpers for the translator unit tests. Included in each
//! integration-test file via `mod common;`.
#![allow(dead_code)]

use saisei_jitc::codegen::{self, Unsupported};
use saisei_jitc::disassemble;
use serde_json::{json, Value};
use std::collections::BTreeSet;

/// Build a known-funcs set from a slice of addresses.
pub fn known(addrs: &[i64]) -> BTreeSet<i64> {
    addrs.iter().copied().collect()
}

/// Render one function through the chunk emitter and return the whole chunk
/// text (dispatch + `_impl` wrapper). Substring assertions run against this.
/// `known_addrs` marks sibling call targets as intra-chunk (direct calls
/// render as `pc = 0x…; continue;`). Panics on `Unsupported` — use
/// `try_render_rs` for tests asserting a construct is (intentionally) not
/// emittable.
pub fn render_rs(func: &Value, known_addrs: &[i64], prefix: &str) -> String {
    try_render_rs(func, known_addrs, prefix).expect("emit_chunk")
}

pub fn try_render_rs(
    func: &Value,
    known_addrs: &[i64],
    prefix: &str,
) -> Result<String, Unsupported> {
    render_rs_ir(&json!({ "functions": [func] }), known_addrs, prefix)
}

/// Render a whole IR (multiple functions, optional "relocations") through the
/// chunk emitter. The reloc-sensitive tests pass
/// `{"functions": […], "relocations": [{"segment": s, "offset": o}, …]}`.
pub fn render_rs_ir(ir: &Value, known_addrs: &[i64], prefix: &str) -> Result<String, Unsupported> {
    codegen::emit_chunk_known(ir, prefix, None, "saisei_rt.rs", &known(known_addrs))
}

/// Render several functions as one chunk (the JIT dispatch view). Dispatch
/// match arms and every function body share the one returned text.
pub fn render_rs_dispatch(funcs: &[Value], known_addrs: &[i64]) -> String {
    render_rs_ir(&json!({ "functions": funcs }), known_addrs, "").expect("emit_chunk (dispatch)")
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

/// Build a `func` dict from (start, instructions) — convenience for tests.
pub fn func(start: i64, instrs: Value) -> Value {
    serde_json::json!({ "start": start, "instructions": instrs })
}

/// Parse an int field from a Value ("start", "address", …).
pub fn as_i(v: &Value, key: &str) -> i64 {
    v.get(key).and_then(Value::as_i64).unwrap_or(0)
}
