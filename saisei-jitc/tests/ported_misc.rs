//! Ported from tests/test_*.py — miscellaneous compiler + one C-shim test.
//!
//! PORT DISPOSITIONS (C backend deleted):
//!   unchanged: disassemble_retf__retf (front-half); parity8_helper (ported
//!              to a rustc-compiled chunk-shell — no C toolchain)
//!   ported:    filter_internal_labels (in-function extern label → match arm),
//!              chunk_wrappers (was ir_to_c_header: .h prototypes/macros are a
//!              C artifact; the Rust equivalent is the per-function `_impl`
//!              wrapper emission), rcb_mov round-trip (active, was #[ignore]d
//!              under the C header test), io_port_shim_coverage (scans
//!              runtime/src/*.rs + build/*/jit/*.rs — the old scan over
//!              runtime/**/*.c and build/*.c had gone vacuous: no .c exists)
//!   collapsed: rcb ARITHMETIC (add via es:[0xFF2C]) → asserted Unsupported
//!              ("rcb") — the Rust backend supports RCB operands on mov only
//!
//! This file mixes compiler tests (`mod common`) with one C-shim-style test
//! (`mod shim_common`, for repo_root/guard).
#![allow(non_snake_case)]
mod common;
mod shim_common;

use common::*;
use regex::Regex;
use serde_json::json;
use std::collections::HashSet;
use std::path::PathBuf;
use std::process::Command;

// ==========================================================================
// The original CLI writes disasm.json = the flat instruction list; that maps to
// the parsed IR's `code` array. A lone 0xCB byte decodes to `retf`.
// ==========================================================================
#[test]
fn disassemble_retf__retf() {
    let ir = disasm(&[0xCB], &[0x0]);
    let code = ir["code"].as_array().cloned().unwrap_or_default();
    assert!(
        code.iter()
            .any(|insn| insn["mnemonic"].as_str() == Some("retf")),
        "{code:?}"
    );
}

// ==========================================================================
// An extern_label that falls inside a known function's body stays an
// in-function block: the chunk renders it as its own dispatch match arm.
// ==========================================================================
#[test]
fn filter_internal_labels__internal_label_is_an_in_function_arm() {
    let func = json!({
        "start": 0x0100,
        "instructions": [
            {"address": 0x0100, "mnemonic": "mov", "op_str": "al, 1", "bytes": "B001"},
            {"address": 0x0102, "mnemonic": "jmp", "op_str": "0x200", "bytes": "E9FB00", "target": 0x0200},
            {"address": 0x0200, "mnemonic": "ret", "op_str": "", "bytes": "C3"},
        ],
    });
    let src = render_rs(&func, &[], "");
    assert!(
        src.contains("0x0200 => blk_0200(r, expected_retip),"),
        "{src}"
    );
    assert!(blk(&src, 0x0100).contains("return 0x0200;"), "{src}");
}

// ==========================================================================
// Was ir_to_c_header__emits_header_and_includes: the .h prototypes and call
// macros are C artifacts. The Rust equivalent contract: every function gets an
// exported `#[no_mangle]` `_impl` wrapper in the chunk.
// ==========================================================================
#[test]
fn chunk_wrappers__every_function_gets_an_exported_impl() {
    let funcs = [
        json!({"start": 0x0000, "instructions": [
            {"address": 0x0000, "mnemonic": "ret", "op_str": "", "bytes": "C3"}]}),
        json!({"start": 0x0003, "instructions": [
            {"address": 0x0003, "mnemonic": "ret", "op_str": "", "bytes": "C3"}]}),
    ];
    let src = render_rs_dispatch(&funcs, &[]);
    assert!(
        src.contains("pub extern \"C\" fn func_0000_impl(expected_retip: u16"),
        "{src}"
    );
    assert!(
        src.contains("pub extern \"C\" fn func_0003_impl(expected_retip: u16"),
        "{src}"
    );
    // Both route through the shared per-chunk dispatch state machine.
    assert!(src.contains("dispatch(0x0000, expected_retip"), "{src}");
    assert!(src.contains("dispatch(0x0003, expected_retip"), "{src}");
}

// ==========================================================================
// es:[0xFF2C] is an RCB (register-control-block) field. The Rust backend
// rewrites RCB operands on `mov` (rcb_read16/rcb_write16); RCB operands inside
// ARITHMETIC instructions are intentionally Unsupported (a JIT hard error) —
// no game-reached chunk uses them outside mov.
// ==========================================================================
#[test]
fn rcb_mov__reads_and_writes_via_rcb_helpers() {
    let func = json!({
        "start": 0x0000,
        "instructions": [
            {"address": 0x0000, "mnemonic": "mov", "op_str": "word ptr es:[0xFF2C], ax", "bytes": "66"},
            {"address": 0x0003, "mnemonic": "mov", "op_str": "ax, word ptr es:[0xFF2C]", "bytes": "66"},
            {"address": 0x0006, "mnemonic": "ret", "op_str": "", "bytes": "C3"},
        ],
    });
    let src = render_rs(&func, &[], "");
    assert!(
        src.contains("r.rcb_write16(DATA_BASE_SEG, r.ax());"),
        "{src}"
    );
    assert!(
        src.contains("r.set_ax(r.rcb_read16(DATA_BASE_SEG));"),
        "{src}"
    );
}

#[test]
fn rcb_arithmetic__traps_rather_than_reading_the_wrong_place() {
    // An RCB field as an arithmetic operand has no faithful lowering, so it must
    // not be emitted as an ordinary memory access: it becomes a named trap.
    let func = json!({
        "start": 0x0000,
        "instructions": [
            {"address": 0x0000, "mnemonic": "add", "op_str": "ax, word ptr es:[0xFF2C]", "bytes": "66"},
            {"address": 0x0003, "mnemonic": "ret", "op_str": "", "bytes": "C3"},
        ],
    });
    let src = render_rs(&func, &[], "");
    assert!(
        src.contains(r#"r.jit_unsupported_instruction(c"add rcb".as_ptr());"#),
        "{src}"
    );
}

// ==========================================================================
// parity8 is an inline helper of the chunk prelude (rt/saisei_rt.rs), not an
// exported runtime symbol — it cannot be reached via dlopen/FFI on the shim
// .so. So exercise it exactly the way a chunk does: compile a chunk-shell .rs
// that include!s the prelude (the same rustc flags as compile_chunk), dlopen
// it beside the runtime .so, and check all 256 inputs against a popcount
// reference. Doubles as a "the prelude compiles standalone" regression test.
// ==========================================================================
#[test]
fn parity8_helper__matches_x86_even_parity() {
    let _g = shim_common::guard();
    let root = shim_common::repo_root();

    let dir = std::env::temp_dir().join(format!("saisei_parity8_{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("create temp dir");
    std::fs::copy(
        root.join("saisei-jitc/rt/saisei_rt.rs"),
        dir.join("saisei_rt.rs"),
    )
    .expect("copy prelude");

    // Precompile the prelude rlib, then link the chunk-shell against it —
    // mirroring codegen::emit_chunk + compile_chunk exactly (`--extern
    // saisei_rt`, no more `include!`). Doubles as the "prelude compiles as a
    // standalone rlib and a chunk links it" regression.
    let rlib = dir.join("libsaisei_rt.rlib");
    let rlib_ok = Command::new("rustc")
        .current_dir(&dir)
        .args([
            "--edition",
            "2021",
            "--crate-type",
            "rlib",
            "--crate-name",
            "saisei_rt",
            "-C",
            "opt-level=0",
            "-C",
            "overflow-checks=off",
            "-C",
            "debug-assertions=off",
            "-C",
            "panic=abort",
            "-o",
        ])
        .arg(&rlib)
        .arg(dir.join("saisei_rt.rs"))
        .status()
        .expect("spawn rustc (rlib)");
    assert!(rlib_ok.success(), "rustc failed to build saisei_rt rlib");

    // The chunk-shell header mirrors codegen::emit_chunk's exactly.
    let src = r#"#![no_std]
#![allow(dead_code, unused_imports, non_snake_case, non_upper_case_globals, non_camel_case_types)]
#![allow(unused_parens, unused_mut, unused_assignments, unused_unsafe, unused_variables)]
#![allow(unreachable_code, unreachable_patterns)]
extern crate saisei_rt;
use saisei_rt::*;
use core::ffi::{c_char, c_int, c_void};
#[no_mangle] pub extern "C" fn saisei_site_name() -> *const c_char { c"parity8_check".as_ptr() }

/// 0 if parity8 matches even-popcount parity for every byte; else input+1.
#[no_mangle]
pub extern "C" fn parity8_check() -> c_int {
    let mut i: u32 = 0;
    while i < 256 {
        let v = i as u8;
        let expect = (v.count_ones() % 2 == 0) as u8;
        if parity8(v) != expect {
            return (i as c_int) + 1;
        }
        i += 1;
    }
    0
}
"#;
    let rs = dir.join("parity8_check.rs");
    let so = dir.join("parity8_check.so");
    std::fs::write(&rs, src).expect("write parity8_check.rs");

    let compile = Command::new("rustc")
        .current_dir(&dir)
        .args([
            "--edition",
            "2021",
            "--crate-type",
            "cdylib",
            "-C",
            "opt-level=0",
            "-C",
            "overflow-checks=off",
            "-C",
            "debug-assertions=off",
            "-C",
            "panic=abort",
            "--extern",
        ])
        .arg(format!("saisei_rt={}", rlib.display()))
        .arg("-o")
        .arg(&so)
        .arg(&rs)
        .status()
        .expect("spawn rustc");
    assert!(
        compile.success(),
        "rustc failed to compile parity8_check.rs"
    );

    // The prelude binds extern DATA (cpu, virtual_memory, …): preload the
    // runtime .so RTLD_GLOBAL so those resolve when the chunk-shell loads.
    unsafe {
        let rt = libloading::os::unix::Library::open(
            Some(shim_common::runtime_so()),
            libc::RTLD_NOW | libc::RTLD_GLOBAL,
        )
        .expect("dlopen runtime .so RTLD_GLOBAL");
        let chk = libloading::Library::new(&so).expect("dlopen parity8_check.so");
        let f: libloading::Symbol<unsafe extern "C" fn() -> i32> =
            chk.get(b"parity8_check").expect("parity8_check symbol");
        let rc = f();
        assert_eq!(rc, 0, "parity8 mismatch at input 0x{:02X}", rc - 1);
        drop(chk);
        drop(rt);
    }

    let _ = std::fs::remove_dir_all(&dir);
}

// ==========================================================================
// Every inb/inw/outb/outw port a JIT chunk artifact touches must be handled
// somewhere in the runtime shims (runtime/src). The chunk artifacts are the
// build/*/jit/*.rs files; the shim surface is runtime-rs/src/*.rs.
// ==========================================================================

/// int(s, 16) — s is bare hex digits (no 0x prefix).
fn parse_hex(s: &str) -> i64 {
    i64::from_str_radix(s, 16).expect("hex literal")
}

/// int(s, 0) — auto-detect base: 0x/0X prefix => hex, else decimal.
fn parse_base0(s: &str) -> i64 {
    let s = s.trim();
    if let Some(h) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        i64::from_str_radix(h, 16).expect("hex literal")
    } else {
        s.parse::<i64>().expect("decimal literal")
    }
}

/// Every port the Rust runtime handles: `port == 0x…` comparisons, `0x… =>`
/// match arms, and `…_PORTS`-style array literals across runtime/src.
fn extract_shim_ports() -> HashSet<i64> {
    let root = shim_common::repo_root();
    let src_dir = root.join("runtime").join("src");

    let mut sources: Vec<PathBuf> = Vec::new();
    if let Ok(entries) = std::fs::read_dir(&src_dir) {
        for entry in entries.flatten() {
            let p = entry.path();
            if p.extension().and_then(|e| e.to_str()) == Some("rs") {
                sources.push(p);
            }
        }
    }
    sources.sort();
    assert!(
        !sources.is_empty(),
        "no runtime sources found under {src_dir:?}"
    );

    let text: String = sources
        .iter()
        .map(|p| std::fs::read_to_string(p).unwrap_or_default())
        .collect::<Vec<_>>()
        .join("\n");

    let mut handled: HashSet<i64> = HashSet::new();

    let port_eq = Regex::new(r"port == 0x([0-9A-Fa-f]+)").unwrap();
    for c in port_eq.captures_iter(&text) {
        handled.insert(parse_hex(&c[1]));
    }
    // Match arms: `0x61 => …` / `0x40..=0x43 =>` (both bounds collected).
    let arm_re = Regex::new(r"0x([0-9A-Fa-f]+)\s*(?:\.\.=\s*0x([0-9A-Fa-f]+)\s*)?=>").unwrap();
    for c in arm_re.captures_iter(&text) {
        let lo = parse_hex(&c[1]);
        if let Some(hi) = c.get(2) {
            let hi = parse_hex(hi.as_str());
            for v in lo..=hi.min(lo + 0x400) {
                handled.insert(v);
            }
        } else {
            handled.insert(lo);
        }
    }
    // `static OPL2_PORTS: [u16; 3] = [0x388, 0x389, 0xFFFF];`
    let ports_re = Regex::new(r"(?i)_ports\s*(?::[^=]*)?=\s*\[([^\]]*)\]").unwrap();
    let hex_re = Regex::new(r"0x([0-9A-Fa-f]+)").unwrap();
    for body in ports_re.captures_iter(&text) {
        for m in hex_re.captures_iter(&body[1]) {
            let v = parse_hex(&m[1]);
            if v != 0xFFFF {
                handled.insert(v);
            }
        }
    }
    handled
}

/// Literal port, or a `set_dx(…)`/`set_dl(…)` backscan (chunk .rs syntax).
fn resolve_port(arg: &str, lines: &[&str], line_no: usize) -> Option<i64> {
    let full = Regex::new(r"^(?:0x[0-9A-Fa-f]+|\d+)$").unwrap();
    if full.is_match(arg) {
        return Some(parse_base0(arg));
    }
    if !arg.starts_with("dx") {
        return None;
    }

    let dx_re = Regex::new(r"set_dx\((0x[0-9A-Fa-f]+|\d+)\);").unwrap();
    let dl_re = Regex::new(r"set_dl\((0x[0-9A-Fa-f]+|\d+)\);").unwrap();

    let mut dx_value: Option<i64> = None;
    let stop = std::cmp::max(-1i64, line_no as i64 - 24);
    let mut back = line_no as i64 - 1;
    while back > stop {
        let text = lines[back as usize];
        if let Some(c) = dx_re.captures(text) {
            dx_value = Some(parse_base0(&c[1]));
            back -= 1;
            continue;
        }
        if let Some(c) = dl_re.captures(text) {
            if let Some(dv) = dx_value {
                return Some((dv & 0xFF00) | parse_base0(&c[1]));
            }
        }
        back -= 1;
    }
    dx_value
}

/// [(chunk filename, 1-based line, op, port)] across build/*/jit/*.rs.
fn artifact_io_calls() -> Vec<(String, usize, String, i64)> {
    let root = shim_common::repo_root();
    let build = root.join("build");

    let mut paths: Vec<PathBuf> = Vec::new();
    if let Ok(games) = std::fs::read_dir(&build) {
        for game in games.flatten() {
            let jit = game.path().join("jit");
            if let Ok(entries) = std::fs::read_dir(&jit) {
                for entry in entries.flatten() {
                    let p = entry.path();
                    if p.is_file() && p.extension().and_then(|e| e.to_str()) == Some("rs") {
                        paths.push(p);
                    }
                }
            }
        }
    }
    paths.sort();

    let io_re = Regex::new(r"\b(inb|inw|outb|outw)\(([^,)]+)").unwrap();
    let mut calls: Vec<(String, usize, String, i64)> = Vec::new();

    for path in paths {
        let content = std::fs::read_to_string(&path).unwrap_or_default();
        let lines: Vec<&str> = content.lines().collect();
        for (idx, line) in lines.iter().enumerate() {
            if let Some(m) = io_re.captures(line) {
                let op = m[1].to_string();
                let arg = m[2].trim().trim_end_matches("()").to_string();
                if let Some(port) = resolve_port(&arg, &lines, idx) {
                    let name = path
                        .file_name()
                        .and_then(|s| s.to_str())
                        .unwrap_or("")
                        .to_string();
                    calls.push((name, idx + 1, op, port));
                }
            }
        }
    }
    calls
}

#[test]
fn io_port_shim_coverage__resolved_artifact_io_ports_are_implemented_in_shims() {
    let handled = extract_shim_ports();
    let mut missing: Vec<(String, usize, String, i64)> = Vec::new();

    for (filename, line_no, op, port) in artifact_io_calls() {
        let _ = &op;
        if !handled.contains(&port) {
            missing.push((filename, line_no, op, port));
        }
    }

    assert!(
        missing.is_empty(),
        "Found translated inb/outb/inw/outw ports without shim handling: {}",
        missing
            .iter()
            .map(|(name, line, op, port)| format!("{name}:{line} {op}(0x{port:04X})"))
            .collect::<Vec<_>>()
            .join(", ")
    );
}
