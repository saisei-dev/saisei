//! Offline chunk-emitter coverage sweep. Disassembles every game EXE (and captured
//! segment) from its real entry point — the CFG walker + jump-table/push-imm
//! discovery reaches most statically-resolvable code — and emits each
//! function, aggregating the Unsupported reasons `codegen` still can't handle.
//! This is the gap frontier for "full coverage, no fallbacks".
//!
//! Run: cargo test -p saisei-jitc --test gap_sweep -- --nocapture --ignored
use saisei_jitc::{codegen, disassemble};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::path::PathBuf;

fn normalize(s: &str) -> String {
    let mut out = String::new();
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '0' && matches!(chars.peek(), Some('x') | Some('X')) {
            chars.next();
            while chars.peek().map_or(false, |c| c.is_ascii_hexdigit()) {
                chars.next();
            }
            out.push_str("0xN");
        } else if c.is_ascii_digit() {
            while chars.peek().map_or(false, |c| c.is_ascii_digit()) {
                chars.next();
            }
            out.push('N');
        } else {
            out.push(c);
        }
    }
    out
}

#[test]
#[ignore]
fn gap_sweep() {
    let repo = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .to_path_buf();
    // Corpus: every game EXE/COM image (full static code) + captured segments.
    let mut inputs: Vec<(PathBuf, Option<i64>)> = Vec::new();
    if let Ok(games) = std::fs::read_dir(repo.join("games")) {
        for g in games.flatten() {
            if let Ok(files) = std::fs::read_dir(g.path()) {
                for f in files.flatten() {
                    let n = f.file_name().to_string_lossy().to_uppercase();
                    if n.ends_with(".EXE") || n.ends_with(".COM") {
                        inputs.push((f.path(), None)); // MZ entry from stage1
                    }
                }
            }
        }
    }
    // Runtime segment dumps (real post-unpack code) — disassembled densely below.
    let mut seg_inputs: Vec<(PathBuf, i64)> = Vec::new();
    if let Ok(games) = std::fs::read_dir(repo.join("build")) {
        for g in games.flatten() {
            if let Ok(files) = std::fs::read_dir(g.path().join("jit")) {
                for f in files.flatten() {
                    let name = f.file_name().to_string_lossy().to_string();
                    if let Some(base) = name
                        .strip_prefix("seg_")
                        .and_then(|s| s.strip_suffix(".bin"))
                        .and_then(|s| i64::from_str_radix(s, 16).ok())
                    {
                        seg_inputs.push((f.path(), base));
                    }
                }
            }
        }
    }

    let mut gaps: BTreeMap<String, (usize, String)> = BTreeMap::new();
    let (mut total, mut ok) = (0usize, 0usize);
    let mut bad_files = 0usize;
    for (path, base) in &inputs {
        let blob = match std::fs::read(path) {
            Ok(b) => b,
            Err(_) => continue,
        };
        let ir_str = std::panic::catch_unwind(|| {
            disassemble::disassemble_ir(&blob, &[], false, base.unwrap_or(0x10100), 2_000_000, None)
        });
        let ir_str = match ir_str {
            Ok(s) => s,
            Err(_) => {
                bad_files += 1;
                continue;
            }
        };
        let ir: Value = match serde_json::from_str(&ir_str) {
            Ok(v) => v,
            Err(_) => continue,
        };
        let relocs = ir.get("relocations").cloned().unwrap_or(json!([]));
        let funcs = ir
            .get("functions")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let img_base = base.unwrap_or(0x10100);
        for f in &funcs {
            total += 1;
            let one = json!({ "functions": [f.clone()], "relocations": relocs });
            // An untranslatable instruction no longer fails the chunk — it becomes
            // a run-time trap — so the frontier is read from the reported gaps,
            // not from an Err. A function is "ok" only if it has none.
            match codegen::emit_chunk_gaps(&one, "sweep_", Some(img_base), "saisei_rt.rs") {
                Ok((_, found)) if found.is_empty() => ok += 1,
                Ok((_, found)) => {
                    for u in found {
                        let e = gaps.entry(normalize(&u)).or_insert((0, u.clone()));
                        e.0 += 1;
                    }
                }
                Err(u) => {
                    let e = gaps.entry(normalize(&u.0)).or_insert((0, u.0.clone()));
                    e.0 += 1;
                }
            }
        }
    }

    // Seg dumps: dense entry seeding to cover ALL runtime code (incl. paths not
    // reached at boot). Garbage decodes just add functions to test — harmless.
    for (path, base) in &seg_inputs {
        let blob = match std::fs::read(path) {
            Ok(b) => b,
            Err(_) => continue,
        };
        let entries: Vec<i64> = (0..blob.len() as i64).collect();
        let ir_str = match std::panic::catch_unwind(|| {
            disassemble::disassemble_ir(&blob, &entries, true, *base, 4_000_000, None)
        }) {
            Ok(s) => s,
            Err(_) => continue,
        };
        let ir: Value = match serde_json::from_str(&ir_str) {
            Ok(v) => v,
            Err(_) => continue,
        };
        let relocs = ir.get("relocations").cloned().unwrap_or(json!([]));
        let funcs = ir
            .get("functions")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        for f in &funcs {
            total += 1;
            let one = json!({ "functions": [f.clone()], "relocations": relocs });
            match codegen::emit_chunk(&one, "sweep_", Some(*base), "saisei_rt.rs") {
                Ok(_) => ok += 1,
                Err(u) => {
                    let e = gaps.entry(normalize(&u.0)).or_insert((0, u.0.clone()));
                    e.0 += 1;
                }
            }
        }
    }

    println!(
        "\nSWEEP: {} EXE + {} seg dumps ({} unreadable), {} functions, {} ok-rust, {} distinct gaps\n",
        inputs.len(),
        seg_inputs.len(),
        bad_files,
        total,
        ok,
        gaps.len()
    );
    let mut v: Vec<_> = gaps.into_iter().collect();
    v.sort_by_key(|(_, (c, _))| std::cmp::Reverse(*c));
    for (norm, (c, sample)) in v {
        println!("GAP x{c:<5} {norm}    e.g. [{sample}]");
    }
}
