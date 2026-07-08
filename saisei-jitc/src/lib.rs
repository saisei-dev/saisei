//! saisei-jitc library — the Rust port of the reference `compiler/` translator.
//!
//! Exposes the translator/JIT internals as a library so both the `saisei-jitc`
//! binary and the cargo test suite (ports of the reference `tests/` unit tests)
//! can drive them. `ast`/`cfg`/`graph`/`patterns` are the structured
//! (readable-C) renderer's support crates — dead on the JIT PCSwitch path but
//! exercised by the ~100 structured-renderer tests.
#![allow(dead_code)]

pub mod ast;
pub mod cfg;
pub mod disassemble;
pub mod graph;
pub mod ir_to_c;
pub mod patterns;
