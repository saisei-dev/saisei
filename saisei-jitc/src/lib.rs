//! saisei-jitc library — the JIT translator: DOS MZ program images to native
//! Rust chunks.
//!
//! `disassemble` decodes bytes to the lossless JSON IR; `translate` is the
//! shared front-half (operand rewriting, flag normalization, basic blocks);
//! `codegen` emits each chunk as Rust for rustc. Exposed as a library so both
//! the `saisei-jitc` binary and the cargo test suite can drive them.
#![allow(dead_code)]

pub mod codegen;
pub mod disassemble;
pub mod translate;
