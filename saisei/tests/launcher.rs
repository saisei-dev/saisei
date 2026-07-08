//! Integration tests for the `saisei` launcher crate — Rust ports of the original
//!
//! The the original new-game test used games/cat/CAT.EXE, which is not committed here
//! (games are uncommitted/copyrighted), so we build a synthetic MZ fixture in a
//! temp dir instead.

use saisei::new_game::{build_seed_config, is_executable};
use saisei::triage::{class_map_has, summarize, triage, CLASS_MAP};
use saisei::validate_speedup;

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

static COUNTER: AtomicUsize = AtomicUsize::new(0);

/// A unique, freshly-created temp directory to act as a test sandbox.
fn temp_parent(tag: &str) -> PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!(
        "saisei_launcher_test_{tag}_{}_{}_{n}",
        std::process::id(),
        nanos
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn is_executable_detects_mz_and_extension() {
    let dir = temp_parent("newgame_isexec");
    // Synthetic fixture: an MZ image at CAT.EXE (first two bytes 'M' 'Z').
    let cat_exe = dir.join("CAT.EXE");
    std::fs::write(&cat_exe, b"MZ\x90\x00\x03\x00").unwrap();
    let cat_json = dir.join("cat.json");
    std::fs::write(&cat_json, br#"{"name":"cat"}"#).unwrap();

    assert!(is_executable(&cat_exe));
    assert!(!is_executable(&cat_json));

    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn seed_config_is_jit_only() {
    let dir = temp_parent("newgame_seed");
    let cat_exe = dir.join("CAT.EXE");
    std::fs::write(&cat_exe, b"MZ\x90\x00\x03\x00").unwrap();
    // A sibling <name>.json is excluded from the runtime list by build_seed_config.
    std::fs::write(dir.join("cat.json"), br#"{"name":"cat"}"#).unwrap();

    let cfg = build_seed_config("cat", &cat_exe, &dir);

    // JIT-only seed: just the program image + the data files to copy. No binaries
    // list, entry symbol, or call-target table.
    assert_eq!(
        cfg.get("program_path").and_then(|v| v.as_str()),
        Some("CAT.EXE")
    );
    let runtime = cfg
        .get("runtime")
        .and_then(|v| v.as_array())
        .expect("runtime array present");
    assert!(runtime
        .iter()
        .any(|r| r.get("dest").and_then(|v| v.as_str()) == Some("CAT.EXE")));
    assert!(cfg.get("binaries").is_none());
    assert!(cfg.get("entry_symbol").is_none());
    assert!(cfg.get("call_targets").is_none());

    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn speedup_accepts_positive_rejects_non_positive() {
    // `--speedup 4` is accepted and carries through as 4.0.
    assert_eq!(validate_speedup(4.0), Ok(4.0));
    // `--speedup 0` (and negatives) are rejected with the exact message.
    assert_eq!(
        validate_speedup(0.0),
        Err("--speedup must be a positive number".to_string())
    );
    assert_eq!(
        validate_speedup(-1.0),
        Err("--speedup must be a positive number".to_string())
    );
}

/// Port of the original `_bundle` helper: build a synthetic crash bundle.
fn bundle(parent: &Path, name: &str, manifest: Option<&str>, crash: &str, trace: &str) -> PathBuf {
    let b = parent.join(name);
    std::fs::create_dir_all(&b).unwrap();
    if let Some(m) = manifest {
        std::fs::write(b.join("manifest.json"), m).unwrap();
    }
    std::fs::write(b.join("crash.txt"), crash).unwrap();
    std::fs::write(b.join("trace.tail.log"), trace).unwrap();
    b
}

#[test]
fn stack_drift_is_operator_class() {
    let dir = temp_parent("triage_stack");
    let b = bundle(
        &dir,
        "crash_1_1_stack_drift_0x10",
        Some(
            r#"{"kind":"stack_drift","runtime_version":"abc123","active_binary":"app","fault_addr":"0x10"}"#,
        ),
        "[FATAL] stack drift at lcall boundary\n",
        "",
    );
    assert_eq!(triage(&b), 0);
    let out = summarize(&b);
    assert!(out.contains("operator"), "summary was:\n{out}");
    assert!(out.contains("abc123"), "summary was:\n{out}");

    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn kind_from_dirname_without_manifest() {
    let dir = temp_parent("triage_dirname");
    let b = bundle(
        &dir,
        "crash_1_1_unhandled_pc_0xABCD",
        None,
        "[BUG] Unhandled pc=ABCD\n",
        "",
    );
    assert_eq!(triage(&b), 0);
    let out = summarize(&b);
    assert!(out.contains("bad-transfer"), "summary was:\n{out}"); // unhandled_pc class

    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn missing_file_detected_from_logs() {
    let dir = temp_parent("triage_missing");
    let b = bundle(
        &dir,
        "crash_1_1_unhandled_pc_0x0",
        None,
        "Error: dos_open_file: failed to open LEVELS.DAT\n",
        "",
    );
    assert_eq!(triage(&b), 0);
    let out = summarize(&b);
    assert!(out.contains("missing-file"), "summary was:\n{out}");

    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn class_map_has_known_kinds() {
    assert!(class_map_has("stack_drift"));
    assert!(class_map_has("unhandled_pc"));
    assert_eq!(CLASS_MAP.len(), 9);
}
