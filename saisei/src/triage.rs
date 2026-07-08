//! `saisei triage` — .
//!
//! A crash bundle (`build/<game>/crashes/crash_*/`) is the platform's bug-report
//! unit. This reads one and prints a maintainer-facing summary: what failed, the
//! runtime version it happened on, and — crucially — which *class* of problem it
//! is and where the fix lives. The three classes:
//!
//! 1. operator      a translated instruction's effect is wrong (stack drift,
//! unsupported opcode)            -> fix in compiler/runtime
//! 2. bad-transfer  control reached a wrong/unmapped code address (unhandled pc,
//! unmapped call) -> read OUR shims/codegen
//! 3. missing-file  the game opened a file not in the bundle (dos_open failure)
//! -> add to games/<name>.json

use regex::Regex;
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::exit;

/// kind (from manifest/dir name) -> (class, one-line meaning, where to fix).
/// Faithful  `_CLASS`.
pub const CLASS_MAP: &[(&str, (&str, &str, &str))] = &[
    (
        "stack_drift",
        (
            "operator",
            "simulated stack pointer drifted across a call",
            "a translated instruction dropped a push/pop, or a callee-cleanup (retf N) the runtime didn't expect",
        ),
    ),
    (
        "retf_drift",
        (
            "operator",
            "far return saw an unexpected stack pointer",
            "the far-called function's body left the stack unbalanced (translator stack-effect bug)",
        ),
    ),
    (
        "dispatch_recursion",
        (
            "operator/disasm",
            "dispatch recursed without bound",
            "a near-ret popped a bogus return IP; trace the near_ret_tail/call_table chain for the first expected_retip divergence",
        ),
    ),
    (
        "unhandled_pc",
        (
            "bad-transfer",
            "dispatched to an address that won't decode",
            "real code is JIT'd automatically, so this is a genuinely wrong address -- stack corruption or an unfaithful transfer upstream",
        ),
    ),
    (
        "lcall_table",
        (
            "bad-transfer",
            "far call to an unmapped/garbage target",
            "check relocation of the call's segment; a 0x0 target is often an un-relocated imm",
        ),
    ),
    (
        "call_table",
        (
            "bad-transfer",
            "near/indirect call to an unmapped target",
            "the computed target is garbage (wrong ds/register upstream)",
        ),
    ),
    (
        "mapswap_straddle",
        (
            "memory",
            "overlay chunk swap straddled a region",
            "overlay mapping issue; inspect file_mappings.json",
        ),
    ),
    (
        "rcb_overwrite",
        (
            "memory",
            "a resident-control-block field was overwritten",
            "a write aliased an RCB field; inspect the write site",
        ),
    ),
    (
        "cross_binary_overwrite",
        (
            "memory",
            "a write crossed into another binary",
            "segment/pointer bug; inspect the write site",
        ),
    ),
];

/// Look up a kind in the class map, returning `(class, meaning, fix)`.
pub fn class_lookup(kind: &str) -> Option<(&'static str, &'static str, &'static str)> {
    CLASS_MAP.iter().find(|(k, _)| *k == kind).map(|(_, v)| *v)
}

/// True if the class map knows this kind (mirrors the reference's `kind in _CLASS`).
pub fn class_map_has(kind: &str) -> bool {
    CLASS_MAP.iter().any(|(k, _)| *k == kind)
}

fn read_text(p: &Path) -> String {
    std::fs::read(p)
        .map(|b| String::from_utf8_lossy(&b).into_owned())
        .unwrap_or_default()
}

/// Render a JSON scalar the way the reference's `str()` would inside the f-strings.
fn pyval(v: &Value) -> String {
    match v {
        Value::Null => "None".to_string(),
        Value::Bool(true) => "True".to_string(),
        Value::Bool(false) => "False".to_string(),
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        other => other.to_string(),
    }
}

/// `dict.get(key, default)` where an absent key yields `default` and a present
/// value is stringified like the reference's `str()`.
fn field(m: &Value, k: &str, default: &str) -> String {
    match m.get(k) {
        Some(v) => pyval(v),
        None => default.to_string(),
    }
}

fn nonempty_object(v: &Value) -> bool {
    v.as_object().map_or(false, |o| !o.is_empty())
}

/// Build the maintainer-facing summary text for a crash bundle directory. This
/// is the exact stdout the reference `triage()` prints (each line terminated by a
/// newline). Exposed so tests can assert on the summary directly.
pub fn summarize(bundle: &Path) -> String {
    let name = bundle
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();

    // manifest: prefer it, tolerate absent/corrupt like the reference.
    let manifest: Value = {
        let mp = bundle.join("manifest.json");
        if mp.exists() {
            std::fs::read(&mp)
                .ok()
                .and_then(|b| serde_json::from_slice(&b).ok())
                .unwrap_or(Value::Null)
        } else {
            Value::Null
        }
    };

    // kind: prefer manifest, else parse the dir name (crash_<ts>_<n>_<kind>_<addr>).
    let kind = match manifest.get("kind").and_then(Value::as_str) {
        Some(k) if !k.is_empty() => k.to_string(),
        _ => {
            let re = Regex::new(r"^crash_\d+_\d+_(.+?)_0x[0-9A-Fa-f]+$").unwrap();
            re.captures(&name)
                .and_then(|c| c.get(1))
                .map(|m| m.as_str().to_string())
                .unwrap_or_else(|| "unknown".to_string())
        }
    };

    let crash = read_text(&bundle.join("crash.txt"));
    let trace = read_text(&bundle.join("trace.tail.log"));

    let (mut klass, mut meaning, mut fix): (String, String, String) = match class_lookup(&kind) {
        Some((c, m, f)) => (c.to_string(), m.to_string(), f.to_string()),
        None => (
            "unknown".to_string(),
            "unrecognized failure kind".to_string(),
            "inspect crash.txt".to_string(),
        ),
    };

    // Class-3 override: a failed file open anywhere in the logs.
    let combined = format!("{crash}{trace}");
    let mut missing_file: Option<String> = None;
    let file_fail = Regex::new(r"(?i)dos_(open|find)[^\n]*?(failed|not found|No such)").unwrap();
    if file_fail.is_match(&combined) {
        klass = "missing-file".to_string();
        meaning = "the game tried to open a file that isn't bundled".to_string();
        fix = "add the file to games/<name>.json (binaries + runtime)".to_string();
        let open_path = Regex::new(r"(?i)dos_open_file:\s*(\S+)").unwrap();
        missing_file = open_path
            .captures(&combined)
            .and_then(|c| c.get(1))
            .map(|m| m.as_str().to_string());
    }

    let mut out = String::new();
    out.push_str(&format!("=== crash bundle triage: {name} ===\n"));
    out.push_str(&format!("  kind            : {kind}\n"));
    out.push_str(&format!("  class           : {klass}\n"));
    out.push_str(&format!("  what happened   : {meaning}\n"));
    if nonempty_object(&manifest) {
        out.push_str(&format!(
            "  runtime version : {}\n",
            field(&manifest, "runtime_version", "?")
        ));
        out.push_str(&format!(
            "  active binary   : {}\n",
            field(&manifest, "active_binary", "?")
        ));
        out.push_str(&format!(
            "  fault addr      : {}\n",
            field(&manifest, "fault_addr", "?")
        ));
        let cpu = manifest.get("cpu").cloned().unwrap_or(Value::Null);
        if nonempty_object(&cpu) {
            out.push_str(&format!(
                "  cpu             : cs:ip={}:{} ss:sp={}:{}\n",
                field(&cpu, "cs", "None"),
                field(&cpu, "ip", "None"),
                field(&cpu, "ss", "None"),
                field(&cpu, "sp", "None"),
            ));
        }
        let depths = manifest.get("depths").cloned().unwrap_or(Value::Null);
        if nonempty_object(&depths) {
            out.push_str(&format!(
                "  depths          : lcall={} isr={} dispatch={}\n",
                field(&depths, "lcall", "None"),
                field(&depths, "isr", "None"),
                field(&depths, "dispatch", "None"),
            ));
        }
    } else {
        out.push_str("  (no manifest.json -- older bundle; facts from crash.txt only)\n");
    }
    if let Some(mf) = &missing_file {
        out.push_str(&format!("  missing file    : {mf}\n"));
    }

    // First [BUG]/[FATAL]/Error: line is usually the one-line cause.
    for line in crash.lines() {
        let s = line.trim();
        if s.starts_with("[BUG]") || s.starts_with("[FATAL]") || s.starts_with("Error:") {
            out.push_str(&format!("  message         : {s}\n"));
            break;
        }
    }

    out.push_str(&format!("\n  >> suggested fix ({klass}): {fix}\n"));
    let mut files: Vec<String> = std::fs::read_dir(bundle)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.path().is_file())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    files.sort();
    out.push_str(&format!("  bundle files    : {}\n", files.join(", ")));
    out
}

/// Triage a crash bundle, printing the summary. Returns the process exit code:
/// 0 on success, 2 if `bundle` is not a directory.  `triage()`.
pub fn triage(bundle: &Path) -> i32 {
    if !bundle.is_dir() {
        eprintln!("triage: not a bundle dir: {}", bundle.display());
        return 2;
    }
    print!("{}", summarize(bundle));
    0
}

/// Newest `crash_*` bundle under `build/<game>/crashes/`, or across all games.
fn find_latest(root: &Path, game: Option<&str>) -> Option<PathBuf> {
    let roots: Vec<PathBuf> = match game {
        Some(g) => vec![root.join("build").join(g).join("crashes")],
        None => {
            let bd = root.join("build");
            if bd.is_dir() {
                std::fs::read_dir(&bd)
                    .into_iter()
                    .flatten()
                    .flatten()
                    .map(|e| e.path())
                    .filter(|p| p.is_dir())
                    .map(|p| p.join("crashes"))
                    .collect()
            } else {
                Vec::new()
            }
        }
    };
    let mut bundles: Vec<PathBuf> = Vec::new();
    for r in roots {
        if !r.is_dir() {
            continue;
        }
        for e in std::fs::read_dir(&r).into_iter().flatten().flatten() {
            let p = e.path();
            let is_crash = p
                .file_name()
                .map(|n| n.to_string_lossy().starts_with("crash_"))
                .unwrap_or(false);
            if p.is_dir() && is_crash {
                bundles.push(p);
            }
        }
    }
    bundles.into_iter().max_by_key(|b| {
        b.metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    })
}

/// CLI entry — mirrors the source's `main()`. Shape matches `new_game::main`.
pub fn main(root: &Path, args: &[String]) -> ! {
    let mut bundle: Option<String> = None;
    let mut latest = false;
    let mut game: Option<String> = None;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--latest" => latest = true,
            "--game" => game = it.next().cloned(),
            other if other.starts_with("--game=") => {
                game = Some(other["--game=".len()..].to_string())
            }
            other if other.starts_with("--") => {
                eprintln!("triage: unknown flag: {other}");
                exit(2)
            }
            other => bundle = Some(other.to_string()),
        }
    }

    if latest || bundle.is_none() {
        let want = game
            .as_deref()
            .or(if latest { None } else { bundle.as_deref() });
        match find_latest(root, want) {
            Some(b) => exit(triage(&b)),
            None => {
                eprintln!("triage: no crash bundles found under build/*/crashes/");
                exit(1)
            }
        }
    }
    exit(triage(Path::new(&bundle.unwrap())))
}
