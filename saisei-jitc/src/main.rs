//! saisei-jitc — the JIT translator CLI: disasm (bytes → IR), emit (IR → Rust
//! chunk text), jit-compile (the runtime-invoked segment → .so pipeline).
//!
//! disasm <input> --outdir DIR [--image-base HEX] [--entry HEX ...]
//! [--skip-entry-0000] [--cs-base HEX] [--max-insns N]
//! — writes DIR/program.ir.json.

use saisei_jitc::{codegen, disassemble};

use std::path::PathBuf;
use std::process::exit;

fn parse_int(s: &str) -> i64 {
    let s = s.trim();
    let (neg, s) = s.strip_prefix('-').map_or((false, s), |r| (true, r));
    let v = if let Some(h) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        i64::from_str_radix(h, 16).unwrap_or_else(|_| die(&format!("bad hex int: {s}")))
    } else {
        s.parse::<i64>()
            .or_else(|_| i64::from_str_radix(s, 16))
            .unwrap_or_else(|_| die(&format!("bad int: {s}")))
    };
    if neg {
        -v
    } else {
        v
    }
}

fn die(msg: &str) -> ! {
    eprintln!("saisei-jitc: {msg}");
    exit(2)
}

struct DisasmArgs {
    input: PathBuf,
    outdir: PathBuf,
    image_base: i64,
    entries: Vec<i64>,
    skip_entry_0000: bool,
    cs_base: Option<i64>,
    max_insns: i64,
}

fn parse_disasm(mut it: std::vec::IntoIter<String>) -> DisasmArgs {
    let mut input: Option<PathBuf> = None;
    let mut outdir = PathBuf::from("build/disassemble");
    let mut image_base = 0i64;
    let mut entries = Vec::new();
    let mut skip_entry_0000 = false;
    let mut cs_base = None;
    let mut max_insns = 0i64;
    while let Some(a) = it.next() {
        match a.as_str() {
            "--outdir" => {
                outdir = PathBuf::from(it.next().unwrap_or_else(|| die("--outdir needs a value")))
            }
            "--image-base" => {
                image_base = parse_int(
                    &it.next()
                        .unwrap_or_else(|| die("--image-base needs a value")),
                )
            }
            "--entry" => entries.push(parse_int(
                &it.next().unwrap_or_else(|| die("--entry needs a value")),
            )),
            "--skip-entry-0000" => skip_entry_0000 = true,
            "--cs-base" => {
                cs_base = Some(parse_int(
                    &it.next().unwrap_or_else(|| die("--cs-base needs a value")),
                ))
            }
            "--max-insns" => {
                max_insns = parse_int(
                    &it.next()
                        .unwrap_or_else(|| die("--max-insns needs a value")),
                )
            }
            other if other.starts_with("--") => die(&format!("unknown flag: {other}")),
            other => {
                if input.is_some() {
                    die(&format!("unexpected positional: {other}"));
                }
                input = Some(PathBuf::from(other));
            }
        }
    }
    DisasmArgs {
        input: input.unwrap_or_else(|| die("disasm needs an input path")),
        outdir,
        image_base,
        entries,
        skip_entry_0000,
        cs_base,
        max_insns,
    }
}

fn cmd_disasm(args: DisasmArgs) {
    let data = std::fs::read(&args.input)
        .unwrap_or_else(|e| die(&format!("read {}: {e}", args.input.display())));
    std::fs::create_dir_all(&args.outdir).ok();

    let meta = disassemble::stage1(&data);
    let s2 = disassemble::stage2(
        &meta.load_module,
        meta.entry_off,
        &args.entries,
        args.skip_entry_0000,
        args.image_base,
        args.max_insns,
        args.cs_base,
    );
    let ir = disassemble::emit_program_ir(&meta, s2);

    let out = args.outdir.join("program.ir.json");
    std::fs::write(&out, ir).unwrap_or_else(|e| die(&format!("write {}: {e}", out.display())));
}

/// `emit <ir.json> [--out P] [--prefix P] [--image-base H] [--rt PATH]` —
/// emit the chunk as readable Rust. Exits 3 if the chunk uses a construct the
/// emitter can't express yet (the same condition is a hard error on the JIT
/// path — use this subcommand to reproduce and fix such gaps offline).
fn cmd_emit(mut it: std::vec::IntoIter<String>) {
    let mut positional: Vec<PathBuf> = Vec::new();
    let mut out = PathBuf::from("program.rs");
    let mut prefix = String::new();
    let mut image_base: Option<i64> = None;
    let mut rt_path: Option<String> = None;
    while let Some(a) = it.next() {
        match a.as_str() {
            "--out" => out = PathBuf::from(it.next().unwrap_or_else(|| die("--out needs a value"))),
            "--prefix" => prefix = it.next().unwrap_or_else(|| die("--prefix needs a value")),
            "--image-base" => {
                image_base = Some(parse_int(
                    &it.next()
                        .unwrap_or_else(|| die("--image-base needs a value")),
                ))
            }
            "--rt" => rt_path = Some(it.next().unwrap_or_else(|| die("--rt needs a value"))),
            other if other.starts_with("--") => die(&format!("unknown flag: {other}")),
            other => positional.push(PathBuf::from(other)),
        }
    }
    if positional.len() != 1 {
        die("emit needs <ir.json>");
    }
    let ir_bytes = std::fs::read(&positional[0])
        .unwrap_or_else(|e| die(&format!("read {}: {e}", positional[0].display())));
    let ir: serde_json::Value =
        serde_json::from_slice(&ir_bytes).unwrap_or_else(|e| die(&format!("parse IR: {e}")));
    let rt = rt_path.unwrap_or_else(|| "saisei_rt.rs".to_string());
    match codegen::emit_chunk(&ir, &prefix, image_base, &rt) {
        Ok(rs) => std::fs::write(&out, rs)
            .unwrap_or_else(|e| die(&format!("write {}: {e}", out.display()))),
        Err(u) => {
            eprintln!("[saisei-jitc] emit unsupported: {}", u.0);
            exit(3);
        }
    }
}

fn sha_hex16(data: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(data);
    let d = h.finalize();
    let mut s = String::new();
    for b in d.iter().take(8) {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

/// Hash the piece whose change must invalidate a cached chunk: the translator
/// binary itself. The embedded chunk prelude (`SAISEI_RT`, rt/saisei_rt.rs)
/// and every emitter are compiled into the exe, so its bytes cover them —
/// NOTE: the prelude's `#[repr(C)]` structs and the runtime's shims.rs layouts
/// must be edited together; rebuilding this binary then recompiles all chunks.
fn toolchain_hash() -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    if let Ok(exe) = std::env::current_exe() {
        if let Ok(bytes) = std::fs::read(&exe) {
            h.update(&bytes);
        }
    }
    let d = h.finalize();
    let mut s = String::new();
    for b in d.iter().take(8) {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

fn write_u32s(path: &std::path::Path, header: u32, body: &[u32]) {
    let mut buf = Vec::with_capacity(4 + body.len() * 4);
    buf.extend(header.to_le_bytes());
    for v in body {
        buf.extend(v.to_le_bytes());
    }
    std::fs::write(path, buf).unwrap_or_else(|e| die(&format!("write {}: {e}", path.display())));
}

/// The `saisei_rt` prelude, embedded so `jit-compile` can drop it next to each
/// chunk (the Rust equivalent of the C chunks `#include "shims.h"`).
const SAISEI_RT: &str = include_str!("../rt/saisei_rt.rs");

/// Compile one chunk: write the (already emitted) Rust text, compile it with
/// rustc to `so`. Returns the case-key set on success, or the failure reason —
/// which the JIT caller treats as a hard error (there is no fallback backend).
fn compile_chunk(
    rs_text: &str,
    name: &str,
    outdir: &std::path::Path,
    so: &std::path::Path,
) -> Result<Vec<u32>, String> {
    // Drop the prelude next to the chunk so `include!("saisei_rt.rs")` resolves.
    let rt_p = outdir.join("saisei_rt.rs");
    let need_rt = std::fs::read_to_string(&rt_p)
        .map(|s| s != SAISEI_RT)
        .unwrap_or(true);
    if need_rt {
        std::fs::write(&rt_p, SAISEI_RT).ok();
    }
    // Both the input .rs and the output .so are compiled through PID-private
    // temp paths and renamed into place at the end. The foreground jit-compile
    // and a background speculate can race on the same chunk NAME (identical
    // content -> identical sha -> same path); if one truncates the shared
    // `{name}.rs` (fs::write truncates first) while the other's rustc is
    // parsing it, rustc fails on a partial read and the runtime hard-crashes
    // the game on provably-valid source. Isolating the input per process
    // closes that race, exactly as the .so temp+rename already closed the
    // dlopen-half-written-object race.
    let pid = std::process::id();
    let rs_tmp = outdir.join(format!("{name}.rs.tmp{pid}"));
    let rs_p = outdir.join(format!("{name}.rs"));
    if let Err(e) = std::fs::write(&rs_tmp, &rs_text) {
        return Err(format!("write {}: {e}", rs_tmp.display()));
    }
    let so_tmp = so.with_extension(format!("so.tmp{pid}"));

    // rustc: cdylib with C-like wrapping arithmetic (overflow-checks off) so the
    // translated math matches the x86 model and never panics across the FFI edge.
    let run_rustc = || {
        std::process::Command::new("rustc")
            .args([
                "--edition",
                "2021",
                "--crate-type",
                "cdylib",
                // Set the crate name explicitly: the input is a PID-suffixed
                // temp (`{name}.rs.tmp{pid}`), and rustc would otherwise infer
                // a crate name from the file stem — which contains a '.' and is
                // rejected. `name` is a valid identifier (jit_<hex>_<hex>_<sha>).
                "--crate-name",
                name,
                "-C",
                "opt-level=1",
                "-C",
                "overflow-checks=off",
                "-C",
                "debug-assertions=off",
                "-C",
                "panic=abort",
                "-o",
            ])
            .arg(&so_tmp)
            .arg(&rs_tmp)
            .output()
    };
    // A rustc failure on this content-addressed source is either a genuine
    // codegen gap (fails identically every time -> the retry costs one extra
    // run, then a real hard error carrying rustc's stderr) or a transient
    // toolchain/OS hiccup (a killed compiler, a truncated concurrent read, a
    // momentary resource limit -> the retry succeeds). Retrying once never
    // masks a genuine gap and rescues the transient case that otherwise
    // crashes the whole game.
    let mut last_err = String::new();
    let mut compiled = false;
    for attempt in 0..2 {
        match run_rustc() {
            Ok(out) if out.status.success() => {
                compiled = true;
                break;
            }
            Ok(out) => {
                let stderr = String::from_utf8_lossy(&out.stderr);
                let tail: String = stderr
                    .chars()
                    .rev()
                    .take(1200)
                    .collect::<Vec<_>>()
                    .into_iter()
                    .rev()
                    .collect();
                last_err = format!(
                    "rustc exited {} for {} (attempt {}):\n{}",
                    out.status
                        .code()
                        .map(|c| c.to_string())
                        .unwrap_or_else(|| "signal".into()),
                    rs_tmp.display(),
                    attempt + 1,
                    tail.trim()
                );
            }
            Err(e) => {
                last_err = format!(
                    "rustc spawn failed for {} (attempt {attempt}): {e}",
                    rs_tmp.display()
                );
            }
        }
        std::fs::remove_file(&so_tmp).ok();
    }
    if !compiled {
        // Keep the failing source under its canonical name for offline repro.
        std::fs::rename(&rs_tmp, &rs_p).ok();
        return Err(last_err);
    }
    if let Err(e) = std::fs::rename(&so_tmp, so) {
        std::fs::remove_file(&so_tmp).ok();
        std::fs::remove_file(&rs_tmp).ok();
        return Err(format!(
            "rename {} -> {}: {e}",
            so_tmp.display(),
            so.display()
        ));
    }
    // Publish the source under its canonical name (atomic; harmless if a
    // concurrent compile of the same chunk already did).
    std::fs::rename(&rs_tmp, &rs_p).ok();
    eprintln!("[saisei-jitc] compiled {}", rs_p.display());
    // Case-key set from the dispatch match arms (`0xXXXX =>`), sorted+unique.
    let case_re = regex::Regex::new(r"(?m)^\s*0x([0-9A-Fa-f]+) =>").unwrap();
    let mut keys: Vec<u32> = case_re
        .captures_iter(&rs_text)
        .filter_map(|c| u32::from_str_radix(c.get(1).unwrap().as_str(), 16).ok())
        .collect();
    keys.sort_unstable();
    keys.dedup();
    Ok(keys)
}

/// Cache probe shared by `jit-compile` and `speculate`: a chunk `name` is
/// servable when its sidecars are present, its `.sha` matches this toolchain,
/// and a compiled object exists — either the chunk's own `.so`, or (batched
/// speculative compiles) the shared object named by its `.sofile` sidecar.
/// Returns (so path, range-lo, range-hi) on hit.
fn cached_chunk_so(outdir: &std::path::Path, name: &str) -> Option<(PathBuf, String, String)> {
    let p = |ext: &str| outdir.join(format!("{name}.{ext}"));
    let (so, sofile, sha_p, range_p, keys_p, code_p) = (
        p("so"),
        p("sofile"),
        p("sha"),
        p("range"),
        p("keys"),
        p("code"),
    );
    if !(sha_p.exists() && range_p.exists() && keys_p.exists() && code_p.exists()) {
        return None;
    }
    let rs_sha = name.rsplit('_').next()?;
    let key = format!("{rs_sha}:{}", toolchain_hash());
    if !std::fs::read_to_string(&sha_p)
        .map(|s| s.trim() == key)
        .unwrap_or(false)
    {
        return None;
    }
    // A standalone .so (foreground compile, or a hand-instrumented rebuild)
    // takes precedence over the batch object.
    let so_path = if so.exists() {
        so
    } else {
        let batch = std::fs::read_to_string(&sofile).ok()?;
        let batch = PathBuf::from(batch.trim());
        if !batch.exists() {
            return None;
        }
        batch
    };
    let r = std::fs::read_to_string(&range_p).ok()?;
    let parts: Vec<&str> = r.split_whitespace().collect();
    if parts.len() < 2 {
        return None;
    }
    Some((so_path, parts[0].to_string(), parts[1].to_string()))
}

/// Decoded-instruction spans of an IR: (lo, hi, merged byte-coverage spans).
fn ir_spans(ir: &serde_json::Value, entry: i64) -> (i64, i64, Vec<(i64, i64)>) {
    let empty = Vec::new();
    let mut addrs: Vec<i64> = Vec::new();
    let mut spans: Vec<(i64, i64)> = Vec::new();
    for f in ir
        .get("functions")
        .and_then(|v| v.as_array())
        .unwrap_or(&empty)
    {
        for ins in f
            .get("instructions")
            .and_then(|v| v.as_array())
            .unwrap_or(&empty)
        {
            let a = ins.get("address").and_then(|v| v.as_i64()).unwrap_or(0);
            let blen = ins
                .get("bytes")
                .and_then(|v| v.as_str())
                .map(|s| (s.len() / 2) as i64)
                .unwrap_or(0);
            addrs.push(a);
            addrs.push(a + blen);
            if blen > 0 {
                spans.push((a, a + blen));
            }
        }
    }
    let (lo, hi) = if addrs.is_empty() {
        (entry, entry + 1)
    } else {
        (*addrs.iter().min().unwrap(), *addrs.iter().max().unwrap())
    };
    spans.sort_unstable();
    let mut merged: Vec<(i64, i64)> = Vec::new();
    for (st, en) in spans {
        match merged.last_mut() {
            Some(last) if st <= last.1 => {
                if en > last.1 {
                    last.1 = en;
                }
            }
            _ => merged.push((st, en)),
        }
    }
    (lo, hi, merged)
}

/// Write a chunk's cache sidecars. The `.sha` goes LAST: it is the validity
/// marker `cached_chunk_so` trusts, so everything it vouches for must already
/// be on disk (concurrent foreground/speculate compilers race on these names).
fn write_chunk_sidecars(
    outdir: &std::path::Path,
    name: &str,
    keys: &[u32],
    lo: i64,
    hi: i64,
    merged: &[(i64, i64)],
    sha_key: &str,
) {
    let p = |ext: &str| outdir.join(format!("{name}.{ext}"));
    write_u32s(&p("keys"), keys.len() as u32, keys);
    let mut code_body: Vec<u32> = Vec::with_capacity(merged.len() * 2);
    for (s, e) in merged {
        code_body.push(*s as u32);
        code_body.push(*e as u32);
    }
    write_u32s(&p("code"), merged.len() as u32, &code_body);
    std::fs::write(p("range"), format!("0x{lo:X} 0x{hi:X}\n")).ok();
    std::fs::write(p("sha"), format!("{sha_key}\n")).ok();
}

/// the runtime-invoked orchestrator.
fn cmd_jit_compile(mut it: std::vec::IntoIter<String>) {
    let mut mem: Option<PathBuf> = None;
    let mut entry: Option<i64> = None;
    let mut name_arg: Option<String> = None;
    let mut image_base: i64 = 0;
    let mut outdir: Option<PathBuf> = None;
    while let Some(a) = it.next() {
        match a.as_str() {
            "--mem" => mem = Some(PathBuf::from(it.next().unwrap_or_else(|| die("--mem")))),
            "--entry" => entry = Some(parse_int(&it.next().unwrap_or_else(|| die("--entry")))),
            "--name" => name_arg = Some(it.next().unwrap_or_else(|| die("--name"))),
            "--image-base" => {
                image_base = parse_int(&it.next().unwrap_or_else(|| die("--image-base")))
            }
            "--outdir" => {
                outdir = Some(PathBuf::from(it.next().unwrap_or_else(|| die("--outdir"))))
            }
            other => die(&format!("jit-compile: unexpected arg {other}")),
        }
    }
    let mem_path = mem.unwrap_or_else(|| die("--mem required"));
    let entry = entry.unwrap_or_else(|| die("--entry required"));
    let outdir = outdir.unwrap_or_else(|| die("--outdir required"));
    let blob = std::fs::read(&mem_path)
        .unwrap_or_else(|e| die(&format!("read {}: {e}", mem_path.display())));
    std::fs::create_dir_all(&outdir).ok();

    let base = name_arg
        .unwrap_or_else(|| format!("jit_{entry:x}"))
        .to_lowercase();

    // Chunks are keyed by what actually reaches rustc: the emitted Rust text,
    // hashed with the chunk name normalized out (name = {base}_{rs_sha},
    // .sha = "{rs_sha}:{toolchain}"). Two segment dumps whose *data* bytes
    // churned but whose decoded code is identical emit identical Rust and
    // reuse one compiled .so — data-byte churn in the 64KB dump no longer
    // forces a ~seconds rustc run. A per-blob alias file
    // ({base}_{blob_sha}.alias -> chunk name) keeps the warm-start fast path:
    // an identical dump resolves its chunk without even re-decoding.
    let blob_sha = sha_hex16(&blob);
    let alias_p = outdir.join(format!("{base}_{blob_sha}.alias"));
    let emit_cached = |name: &str| -> bool {
        match cached_chunk_so(&outdir, name) {
            Some((so, lo, hi)) => {
                println!("SO {}", so.display());
                println!("SYM {name}_dispatch");
                println!("RANGE {lo} {hi}");
                true
            }
            None => false,
        }
    };

    // Fast path: this exact 64KB dump was translated before -> its alias names
    // the chunk; reuse it if the artifacts are still valid for this toolchain.
    if let Ok(aliased) = std::fs::read_to_string(&alias_p) {
        let aliased = aliased.trim();
        if aliased.starts_with(&base) && emit_cached(aliased) {
            return;
        }
    }

    // Translate in-process: bytes -> IR -> Rust (matching the source's sidecar:
    // extra_entries=[entry], skip_entry_0000, image_base, max_insns=30000).
    let ir_str = disassemble::disassemble_ir(&blob, &[entry], true, image_base, 30000, None);
    let ir: serde_json::Value =
        serde_json::from_str(&ir_str).unwrap_or_else(|e| die(&format!("IR parse: {e}")));

    // Emit under a placeholder name, hash the name-independent text, then
    // stamp the real (content-addressed) name in.
    const PLACEHOLDER: &str = "SAISEI_CHUNKNAME";
    let rs_ph = match codegen::emit_chunk(&ir, &format!("{PLACEHOLDER}_"), Some(image_base), "saisei_rt.rs")
    {
        Ok(t) => t,
        Err(u) => die(&format!(
            "could not compile chunk {base} (seg-base 0x{image_base:X}): unsupported construct: {}. \
             Add the missing construct to saisei-jitc/src/codegen.rs. Use \
             `saisei-jitc emit` on the segment, or the offline `gap_sweep` test, \
             to reproduce the reason.",
            u.0
        )),
    };
    let rs_sha = sha_hex16(rs_ph.as_bytes());
    let name = format!("{base}_{rs_sha}");
    let so = outdir.join(format!("{name}.so"));
    let sym = format!("{name}_dispatch");
    let key = format!("{rs_sha}:{}", toolchain_hash());

    // Same emitted Rust + toolchain -> reuse the compiled .so (only the data
    // bytes around the code changed); record the alias for the fast path.
    if emit_cached(&name) {
        std::fs::write(&alias_p, format!("{name}\n")).ok();
        return;
    }
    let rs_text = rs_ph.replace(PLACEHOLDER, &name);

    // Emit readable Rust and compile it with rustc — the one and only backend.
    // A construct the emitter can't express, or a rustc failure, is a HARD
    // error: extend codegen.rs to cover it (there is no fallback backend).
    let keys: Vec<u32> = match compile_chunk(&rs_text, &name, &outdir, &so) {
        Ok(keys) => keys,
        Err(why) => die(&format!(
            "could not compile chunk {name} (seg-base 0x{image_base:X}): {why}. \
             Add the missing construct to saisei-jitc/src/codegen.rs. Use \
             `saisei-jitc emit` on the segment, or the offline `gap_sweep` test, \
             to reproduce the reason."
        )),
    };

    // Decoded byte coverage + decoded-address range from the IR functions.
    let (lo, hi, merged) = ir_spans(&ir, entry);
    write_chunk_sidecars(&outdir, &name, &keys, lo, hi, &merged, &key);
    std::fs::write(&alias_p, format!("{name}\n")).ok();

    println!("SO {}", so.display());
    println!("SYM {sym}");
    println!("RANGE 0x{lo:X} 0x{hi:X}");
}

// ============================================================================
// speculate: background pre-compilation of a segment dump's other entries
// ============================================================================

/// One entry translated and ready to compile.
struct SpecEmit {
    off: i64,
    base: String,
    name: String,
    rs_text: String,
    keys: Vec<u32>,
    lo: i64,
    hi: i64,
    merged: Vec<(i64, i64)>,
}

/// Split a standalone chunk text into (prologue, body): the prologue is
/// everything up to and including the `pub const SAISEI_SITE …` line — shared
/// crate attrs, the prelude include, and the site constant — the body is all
/// the chunk's fns, every one of whose names is prefixed with the chunk name.
fn split_chunk_text(rs_text: &str) -> Option<(&str, &str)> {
    let site_at = rs_text.find("pub const SAISEI_SITE")?;
    let body_at = site_at + rs_text[site_at..].find('\n')? + 1;
    Some((&rs_text[..site_at], &rs_text[body_at..]))
}

/// Translate one candidate entry exactly like `jit-compile` would (per-entry
/// decode of the same blob), returning None when it's already cached or when
/// the decode/emit rejects it (static discovery can hit data-as-code — that's
/// a skip, not an error).
fn spec_translate_one(
    blob: &[u8],
    blob_sha: &str,
    off: i64,
    image_base: i64,
    outdir: &std::path::Path,
) -> Option<SpecEmit> {
    let base = format!("jit_{image_base:05x}_{off:04x}");
    let alias_p = outdir.join(format!("{base}_{blob_sha}.alias"));
    if let Ok(aliased) = std::fs::read_to_string(&alias_p) {
        let aliased = aliased.trim();
        if aliased.starts_with(&base) && cached_chunk_so(outdir, aliased).is_some() {
            return None;
        }
    }
    let ir_str = disassemble::disassemble_ir(blob, &[off], true, image_base, 30000, None);
    let ir: serde_json::Value = serde_json::from_str(&ir_str).ok()?;
    const PLACEHOLDER: &str = "SAISEI_CHUNKNAME";
    let rs_ph = codegen::emit_chunk(
        &ir,
        &format!("{PLACEHOLDER}_"),
        Some(image_base),
        "saisei_rt.rs",
    )
    .ok()?;
    let rs_sha = sha_hex16(rs_ph.as_bytes());
    let name = format!("{base}_{rs_sha}");
    if cached_chunk_so(outdir, &name).is_some() {
        std::fs::write(&alias_p, format!("{name}\n")).ok();
        return None;
    }
    let rs_text = rs_ph.replace(PLACEHOLDER, &name);
    let case_re = regex::Regex::new(r"(?m)^\s*0x([0-9A-Fa-f]+) =>").unwrap();
    let mut keys: Vec<u32> = case_re
        .captures_iter(&rs_text)
        .filter_map(|c| u32::from_str_radix(c.get(1).unwrap().as_str(), 16).ok())
        .collect();
    keys.sort_unstable();
    keys.dedup();
    let (lo, hi, merged) = ir_spans(&ir, off);
    Some(SpecEmit {
        off,
        base,
        name,
        rs_text,
        keys,
        lo,
        hi,
        merged,
    })
}

/// Finalize one translated entry after its object is on disk: sidecars, the
/// per-blob alias, and (batched members) the `.sofile` indirection.
fn spec_finalize(
    e: &SpecEmit,
    blob_sha: &str,
    outdir: &std::path::Path,
    batch_so: Option<&std::path::Path>,
) {
    let rs_sha = e.name.rsplit('_').next().unwrap_or("");
    let key = format!("{rs_sha}:{}", toolchain_hash());
    // The canonical `{name}.rs` is a debug/repro artifact only (compilation
    // reads a PID-private temp — see compile_chunk); write it atomically so a
    // concurrent foreground compile of the same chunk never leaves a torn
    // copy for a future offline `saisei-jitc jit-compile` repro.
    let rs_final = outdir.join(format!("{}.rs", e.name));
    let rs_tmp = outdir.join(format!("{}.rs.stmp{}", e.name, std::process::id()));
    if std::fs::write(&rs_tmp, &e.rs_text).is_ok() {
        std::fs::rename(&rs_tmp, &rs_final).ok();
    }
    if let Some(bso) = batch_so {
        // before the sidecars: .sha (their tail) is the validity marker
        std::fs::write(
            outdir.join(format!("{}.sofile", e.name)),
            format!("{}\n", bso.display()),
        )
        .ok();
    }
    write_chunk_sidecars(outdir, &e.name, &e.keys, e.lo, e.hi, &e.merged, &key);
    std::fs::write(
        outdir.join(format!("{}_{blob_sha}.alias", e.base)),
        format!("{}\n", e.name),
    )
    .ok();
}

/// Background pre-compiler: decode a 64KB segment dump once, discover every
/// statically reachable entry, and compile the not-yet-cached ones across
/// cores — small chunks batched several-per-rustc to amortize process startup.
/// Invoked detached by the runtime after a foreground compile; everything it
/// produces is plain cache content that later `jit-compile` runs resolve.
fn cmd_speculate(mut it: std::vec::IntoIter<String>) {
    let mut mem: Option<PathBuf> = None;
    let mut image_base: i64 = 0;
    let mut outdir: Option<PathBuf> = None;
    let mut exclude: Vec<i64> = Vec::new();
    let mut jobs: usize = 0;
    let mut delete_input = false;
    while let Some(a) = it.next() {
        match a.as_str() {
            "--mem" => mem = Some(PathBuf::from(it.next().unwrap_or_else(|| die("--mem")))),
            "--image-base" => {
                image_base = parse_int(&it.next().unwrap_or_else(|| die("--image-base")))
            }
            "--outdir" => {
                outdir = Some(PathBuf::from(it.next().unwrap_or_else(|| die("--outdir"))))
            }
            "--exclude" => exclude.push(parse_int(&it.next().unwrap_or_else(|| die("--exclude")))),
            "--jobs" => jobs = parse_int(&it.next().unwrap_or_else(|| die("--jobs"))) as usize,
            "--delete-input" => delete_input = true,
            other => die(&format!("speculate: unexpected arg {other}")),
        }
    }
    let mem_path = mem.unwrap_or_else(|| die("--mem required"));
    let outdir = outdir.unwrap_or_else(|| die("--outdir required"));
    let blob = std::fs::read(&mem_path)
        .unwrap_or_else(|e| die(&format!("read {}: {e}", mem_path.display())));
    std::fs::create_dir_all(&outdir).ok();
    let blob_sha = sha_hex16(&blob);
    let done_marker = outdir.join(format!("speculate_{image_base:05x}_{blob_sha}.done"));
    let cleanup = || {
        if delete_input {
            std::fs::remove_file(&mem_path).ok();
        }
    };
    if done_marker.exists() {
        cleanup();
        return;
    }
    if jobs == 0 {
        jobs = std::thread::available_parallelism()
            .map(|n| (n.get() / 2).max(2))
            .unwrap_or(2);
    }

    // Discovery: one multi-root decode (the roots are the foreground entry
    // plus every offset this segbase has ever compiled — chunk/alias names in
    // the cache dir); its function starts are the candidate entries.
    let base_prefix = format!("jit_{image_base:05x}_");
    let mut roots: Vec<i64> = exclude.clone();
    if let Ok(rd) = std::fs::read_dir(&outdir) {
        for ent in rd.flatten() {
            let fname = ent.file_name();
            let fname = fname.to_string_lossy().into_owned();
            if let Some(rest) = fname.strip_prefix(&base_prefix) {
                if let Some(off_hex) = rest.get(..4) {
                    if let Ok(off) = i64::from_str_radix(off_hex, 16) {
                        roots.push(off);
                    }
                }
            }
        }
    }
    roots.sort_unstable();
    roots.dedup();
    if roots.is_empty() {
        roots.push(0);
    }
    let ir_str = disassemble::disassemble_ir(&blob, &roots, true, image_base, 30000, None);
    let ir: serde_json::Value = match serde_json::from_str(&ir_str) {
        Ok(v) => v,
        Err(_) => {
            cleanup();
            return;
        }
    };
    let empty = Vec::new();
    let mut candidates: Vec<i64> = ir
        .get("functions")
        .and_then(|v| v.as_array())
        .unwrap_or(&empty)
        .iter()
        .filter_map(|f| f.get("start").and_then(|v| v.as_i64()))
        .chain(roots.iter().copied())
        .filter(|&o| o > 0 && o <= 0xFFFF && !exclude.contains(&o))
        .collect();
    candidates.sort_unstable();
    candidates.dedup();
    eprintln!(
        "[speculate] segbase 0x{image_base:05X}: {} candidate entries, {jobs} jobs",
        candidates.len()
    );

    // Translate candidates in parallel (decode+emit are pure CPU).
    let work = std::sync::Mutex::new(candidates.into_iter());
    let results: std::sync::Mutex<Vec<SpecEmit>> = std::sync::Mutex::new(Vec::new());
    std::thread::scope(|s| {
        for _ in 0..jobs {
            s.spawn(|| loop {
                let off = match work.lock().unwrap().next() {
                    Some(o) => o,
                    None => return,
                };
                let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    spec_translate_one(&blob, &blob_sha, off, image_base, &outdir)
                }));
                if let Ok(Some(e)) = r {
                    results.lock().unwrap().push(e);
                }
            });
        }
    });
    let mut emits = results.into_inner().unwrap();
    // Distinct entries can decode to identical placeholder text under
    // different names; each still needs its own object (the name is baked into
    // every symbol), so only exact-name duplicates are dropped.
    emits.sort_by(|a, b| a.name.cmp(&b.name));
    emits.dedup_by(|a, b| a.name == b.name);
    emits.sort_by_key(|e| e.off);
    eprintln!("[speculate] {} entries to compile", emits.len());

    // Compile: big chunks individually, small ones batched N-per-rustc (the
    // median chunk is rustc-startup-bound). A batch failure falls back to
    // individual compiles; an individual failure just skips that entry.
    const SMALL_RS_BYTES: usize = 131_072;
    const BATCH_N: usize = 10;
    let (smalls, bigs): (Vec<&SpecEmit>, Vec<&SpecEmit>) =
        emits.iter().partition(|e| e.rs_text.len() < SMALL_RS_BYTES);
    let mut units: Vec<Vec<&SpecEmit>> = Vec::new();
    for b in bigs {
        units.push(vec![b]);
    }
    for group in smalls.chunks(BATCH_N) {
        units.push(group.to_vec());
    }
    let unit_work = std::sync::Mutex::new(units.into_iter());
    std::thread::scope(|s| {
        for _ in 0..jobs {
            s.spawn(|| loop {
                let unit = match unit_work.lock().unwrap().next() {
                    Some(u) => u,
                    None => return,
                };
                if unit.len() == 1 {
                    let e = unit[0];
                    let so = outdir.join(format!("{}.so", e.name));
                    if compile_chunk(&e.rs_text, &e.name, &outdir, &so).is_ok() {
                        spec_finalize(e, &blob_sha, &outdir, None);
                    } else {
                        eprintln!("[speculate] skip 0x{:04X}: rustc failed", e.off);
                    }
                    continue;
                }
                // batch: one crate, one shared prologue + site const, N bodies
                let mut member_shas = String::new();
                for e in &unit {
                    member_shas.push_str(&e.name);
                }
                let batch_name = format!(
                    "jit_{image_base:05x}_batch_{}",
                    sha_hex16(member_shas.as_bytes())
                );
                let batch_so = outdir.join(format!("{batch_name}.so"));
                let mut text = String::new();
                let mut ok = true;
                for (i, e) in unit.iter().enumerate() {
                    match split_chunk_text(&e.rs_text) {
                        Some((prologue, body)) => {
                            if i == 0 {
                                text.push_str(prologue);
                                text.push_str(&format!(
                                    "pub const SAISEI_SITE: &core::ffi::CStr = c\"{batch_name}\";\n"
                                ));
                            }
                            text.push_str(body);
                        }
                        None => {
                            ok = false;
                            break;
                        }
                    }
                }
                if ok && compile_chunk(&text, &batch_name, &outdir, &batch_so).is_ok() {
                    for e in &unit {
                        spec_finalize(e, &blob_sha, &outdir, Some(&batch_so));
                    }
                } else {
                    // fall back to individual compiles
                    for e in &unit {
                        let so = outdir.join(format!("{}.so", e.name));
                        if compile_chunk(&e.rs_text, &e.name, &outdir, &so).is_ok() {
                            spec_finalize(e, &blob_sha, &outdir, None);
                        } else {
                            eprintln!("[speculate] skip 0x{:04X}: rustc failed", e.off);
                        }
                    }
                }
            });
        }
    });
    std::fs::write(&done_marker, "").ok();
    cleanup();
    eprintln!("[speculate] segbase 0x{image_base:05X}: done");
}

fn main() {
    let mut it = std::env::args().skip(1); // skip argv0
    let sub = it
        .next()
        .unwrap_or_else(|| die("usage: saisei-jitc <disasm|emit|jit-compile|speculate> ..."));
    let rest: Vec<String> = it.collect();
    match sub.as_str() {
        "disasm" => cmd_disasm(parse_disasm(rest.into_iter())),
        "emit" => cmd_emit(rest.into_iter()),
        "jit-compile" => cmd_jit_compile(rest.into_iter()),
        "speculate" => cmd_speculate(rest.into_iter()),
        other => die(&format!("unknown subcommand: {other}")),
    }
}
