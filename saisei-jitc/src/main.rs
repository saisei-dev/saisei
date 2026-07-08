//! saisei-jitc — Rust port of the compiler/ JIT translator.
//!
//! Subcommands (mirroring the reference entry points during the port):
//! disasm <input> --outdir DIR [--image-base HEX] [--entry HEX ...]
//! [--skip-entry-0000] [--cs-base HEX] [--max-insns N]
//! — writes DIR/program.ir.json.

use saisei_jitc::{disassemble, ir_to_c};

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

fn cmd_emit_c(mut it: std::vec::IntoIter<String>) {
    let mut positional: Vec<PathBuf> = Vec::new();
    let mut out = PathBuf::from("program.c");
    let mut prefix = String::new();
    let mut image_base: Option<i64> = None;
    let mut _metadata: Option<PathBuf> = None;
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
            "--metadata" => {
                _metadata = Some(PathBuf::from(
                    it.next().unwrap_or_else(|| die("--metadata needs a value")),
                ))
            }
            other if other.starts_with("--") => die(&format!("unknown flag: {other}")),
            other => positional.push(PathBuf::from(other)),
        }
    }
    if positional.len() != 2 {
        die("emit-c needs <ir.json> <exe>");
    }
    let ir_bytes = std::fs::read(&positional[0])
        .unwrap_or_else(|e| die(&format!("read {}: {e}", positional[0].display())));
    let ir: serde_json::Value =
        serde_json::from_slice(&ir_bytes).unwrap_or_else(|e| die(&format!("parse IR: {e}")));
    let code = std::fs::read(&positional[1])
        .unwrap_or_else(|e| die(&format!("read {}: {e}", positional[1].display())));

    let (c_text, h_text) = ir_to_c::emit_c(&ir, &code, &prefix, image_base, &out);
    std::fs::write(&out, c_text).unwrap_or_else(|e| die(&format!("write {}: {e}", out.display())));
    let hpath = out.with_extension("h");
    std::fs::write(&hpath, h_text)
        .unwrap_or_else(|e| die(&format!("write {}: {e}", hpath.display())));
}

/// `structured <ir.json> <code.bin> [--prefix P] [--image-base H]` — run the
/// BASE (readable-C) renderer and print `{hex: [lines], "__extra__": {...}}`
/// JSON to stdout. Used by the differential harness that validates the Rust
/// structured renderer byte-identical against the reference.
fn cmd_structured(mut it: std::vec::IntoIter<String>) {
    let mut positional: Vec<PathBuf> = Vec::new();
    let mut prefix = String::new();
    let mut image_base: Option<i64> = None;
    while let Some(a) = it.next() {
        match a.as_str() {
            "--prefix" => prefix = it.next().unwrap_or_else(|| die("--prefix needs a value")),
            "--image-base" => {
                image_base = Some(parse_int(
                    &it.next()
                        .unwrap_or_else(|| die("--image-base needs a value")),
                ))
            }
            other if other.starts_with("--") => die(&format!("unknown flag: {other}")),
            other => positional.push(PathBuf::from(other)),
        }
    }
    if positional.len() != 2 {
        die("structured needs <ir.json> <code.bin>");
    }
    let ir_bytes = std::fs::read(&positional[0])
        .unwrap_or_else(|e| die(&format!("read {}: {e}", positional[0].display())));
    let ir: serde_json::Value =
        serde_json::from_slice(&ir_bytes).unwrap_or_else(|e| die(&format!("parse IR: {e}")));
    let code = std::fs::read(&positional[1])
        .unwrap_or_else(|e| die(&format!("read {}: {e}", positional[1].display())));
    let out = ir_to_c::render_structured(&ir, &code, &prefix, image_base);
    println!("{}", serde_json::to_string(&out).unwrap());
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

/// Hash the pieces whose change must invalidate a cached chunk: the translator
/// binary itself + the runtime headers (struct layouts compiled into the .so).
fn toolchain_hash() -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    if let Ok(exe) = std::env::current_exe() {
        if let Ok(bytes) = std::fs::read(&exe) {
            h.update(&bytes);
        }
    }
    if let Ok(repo) = std::env::var("SAISEI_REPO_ROOT") {
        let inc = PathBuf::from(&repo).join("runtime").join("include");
        if let Ok(rd) = std::fs::read_dir(&inc) {
            let mut hs: Vec<PathBuf> = rd
                .filter_map(|e| e.ok().map(|e| e.path()))
                .filter(|p| p.extension().and_then(|x| x.to_str()) == Some("h"))
                .collect();
            hs.sort();
            for p in hs {
                if let Ok(b) = std::fs::read(&p) {
                    h.update(&b);
                }
            }
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

    let content_sha = sha_hex16(&blob);
    let base = name_arg
        .unwrap_or_else(|| format!("jit_{entry:x}"))
        .to_lowercase();
    let name = format!("{base}_{content_sha}");
    let p = |ext: &str| outdir.join(format!("{name}.{ext}"));
    let (so, sha_p, range_p, keys_p, code_p, c_p) =
        (p("so"), p("sha"), p("range"), p("keys"), p("code"), p("c"));
    let sym = format!("{name}_dispatch");
    let key = format!("{content_sha}:{}", toolchain_hash());

    // Cross-run cache: same content + toolchain -> reuse the compiled .so.
    if so.exists() && sha_p.exists() && range_p.exists() && keys_p.exists() && code_p.exists() {
        if std::fs::read_to_string(&sha_p)
            .map(|s| s.trim() == key)
            .unwrap_or(false)
        {
            if let Ok(r) = std::fs::read_to_string(&range_p) {
                let parts: Vec<&str> = r.split_whitespace().collect();
                if parts.len() >= 2 {
                    println!("SO {}", so.display());
                    println!("SYM {sym}");
                    println!("RANGE {} {}", parts[0], parts[1]);
                    return;
                }
            }
        }
    }

    // Translate in-process: bytes -> IR -> C (matching the source's sidecar:
    // extra_entries=[entry], skip_entry_0000, image_base, max_insns=30000).
    let ir_str = disassemble::disassemble_ir(&blob, &[entry], true, image_base, 30000, None);
    let ir: serde_json::Value =
        serde_json::from_str(&ir_str).unwrap_or_else(|e| die(&format!("IR parse: {e}")));
    let (c_text, h_text) = ir_to_c::emit_c(&ir, &blob, &format!("{name}_"), Some(image_base), &c_p);
    std::fs::write(&c_p, &c_text).unwrap_or_else(|e| die(&format!("write {}: {e}", c_p.display())));
    std::fs::write(c_p.with_extension("h"), &h_text).ok();

    // Compile to a .so; runtime symbols resolve at dlopen against -rdynamic main.
    let repo = std::env::var("SAISEI_REPO_ROOT").unwrap_or_else(|_| ".".into());
    let inc = format!("{repo}/runtime/include");
    let status = std::process::Command::new("clang")
        .args(["-shared", "-fPIC", "-O1"])
        .arg(&c_p)
        .args(["-I", &inc, "-I"])
        .arg(&outdir)
        .arg("-o")
        .arg(&so)
        .status();
    if !matches!(status, Ok(s) if s.success()) {
        eprintln!("[saisei-jitc] clang failed for {}", c_p.display());
        exit(1);
    }

    // Case-key set (the dispatch `case 0xXXX:` labels), sorted+unique.
    let case_re = regex::Regex::new(r"case 0x([0-9A-Fa-f]+):").unwrap();
    let mut keys: Vec<u32> = case_re
        .captures_iter(&c_text)
        .filter_map(|c| u32::from_str_radix(c.get(1).unwrap().as_str(), 16).ok())
        .collect();
    keys.sort_unstable();
    keys.dedup();

    // Decoded byte coverage + decoded-address range from the IR functions.
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

    write_u32s(&keys_p, keys.len() as u32, &keys);
    let mut code_body: Vec<u32> = Vec::with_capacity(merged.len() * 2);
    for (s, e) in &merged {
        code_body.push(*s as u32);
        code_body.push(*e as u32);
    }
    write_u32s(&code_p, merged.len() as u32, &code_body);
    std::fs::write(&sha_p, format!("{key}\n")).ok();
    std::fs::write(&range_p, format!("0x{lo:X} 0x{hi:X}\n")).ok();

    println!("SO {}", so.display());
    println!("SYM {sym}");
    println!("RANGE 0x{lo:X} 0x{hi:X}");
}

fn main() {
    let mut it = std::env::args().skip(1); // skip argv0
    let sub = it
        .next()
        .unwrap_or_else(|| die("usage: saisei-jitc <disasm|emit-c|jit-compile> ..."));
    let rest: Vec<String> = it.collect();
    match sub.as_str() {
        "disasm" => cmd_disasm(parse_disasm(rest.into_iter())),
        "emit-c" => cmd_emit_c(rest.into_iter()),
        "structured" => cmd_structured(rest.into_iter()),
        "jit-compile" => cmd_jit_compile(rest.into_iter()),
        other => die(&format!("unknown subcommand: {other}")),
    }
}
