//! Integration tests for the `saisei` launcher crate — Rust ports of the original
//!
//! The the original new-game test used games/cat/CAT.EXE, which is not committed here
//! (games are uncommitted/copyrighted), so we build a synthetic MZ fixture in a
//! temp dir instead.

use saisei::new_game::{build_seed_config, finish, is_executable, stage};
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

/// `stage` + `finish` are the whole of adding a game, and both the CLI's
/// `new-game` and the player's drop-a-zip-on-the-window path run them — so a
/// bundle added from a terminal and one added from the window are the same
/// bundle. They return errors rather than exiting, because the player is a
/// window and has to be able to say "that didn't work" and carry on.
#[test]
fn staging_a_folder_finds_its_programs_and_writes_a_config() {
    let root = temp_parent("stage");
    let src = root.join("src");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::write(src.join("GAME.EXE"), b"MZ\x90\x00\x03\x00").unwrap();
    std::fs::write(src.join("SETUP.EXE"), b"MZ\x90\x00\x03\x00").unwrap();
    std::fs::write(src.join("DATA.DAT"), b"\x00\x01\x02").unwrap();
    std::fs::write(src.join("readme.txt"), b"hi").unwrap();

    let staged = stage(&root, &src.display().to_string(), Some("mygame"), false).unwrap();
    assert_eq!(staged.name, "mygame");
    // Both programs are offered; which one starts the game is the player's call.
    assert_eq!(staged.exe_names(), ["GAME.EXE", "SETUP.EXE"]);

    let cfg_path = finish(&staged, &staged.execs[0]).unwrap();
    let cfg: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&cfg_path).unwrap()).unwrap();
    assert_eq!(cfg["program_path"], "GAME.EXE");
    let runtime: Vec<String> = cfg["runtime"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["dest"].as_str().unwrap().to_string())
        .collect();
    assert!(runtime.contains(&"DATA.DAT".to_string()));
    assert!(
        !runtime.iter().any(|d| d.ends_with(".txt")),
        "a readme is not a runtime file: {runtime:?}"
    );

    // Adding it twice does not silently clobber the one you already have.
    let again = stage(&root, &src.display().to_string(), Some("mygame"), false);
    assert!(again.is_err(), "a second add must not overwrite the first");

    std::fs::remove_dir_all(&root).ok();
}

#[test]
fn staging_something_with_no_program_in_it_is_an_error_not_a_bundle() {
    let root = temp_parent("stage_empty");
    let src = root.join("src");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::write(src.join("DATA.DAT"), b"\x00\x01\x02").unwrap();

    let e = stage(&root, &src.display().to_string(), Some("nothing"), false).unwrap_err();
    assert!(e.contains("No DOS program"), "unhelpful message: {e}");
    // And it leaves no half-made bundle behind for the library to show.
    assert!(!root.join("games/nothing").exists());

    std::fs::remove_dir_all(&root).ok();
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

/// A bundle laid out like a real one: a game, a setup, a config the setup rewrites.
fn game_bundle(root: &Path, key: &str) {
    let dir = root.join("games").join(key);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("GAME.EXE"), b"MZ\x90\x00\x03\x00").unwrap();
    std::fs::write(dir.join("SETUP.EXE"), b"MZ\x90\x00\x03\x00").unwrap();
    std::fs::write(dir.join("SETUP.CFG"), b"sound=none").unwrap();
    std::fs::write(dir.join("DATA.DAT"), b"\x00\x01\x02").unwrap();
    std::fs::write(
        dir.join(format!("{key}.json")),
        format!(
            r#"{{"name":"{key}","program_path":"GAME.EXE","runtime":[
                {{"source":"games/{key}/GAME.EXE","dest":"GAME.EXE"}},
                {{"source":"games/{key}/SETUP.EXE","dest":"SETUP.EXE"}},
                {{"source":"games/{key}/SETUP.CFG","dest":"SETUP.CFG"}},
                {{"source":"games/{key}/DATA.DAT","dest":"DATA.DAT"}}]}}"#
        ),
    )
    .unwrap();
}

/// The whole point of being able to run a setup: what it writes has to still be
/// there the next time the game boots.
///
/// `build/<game>/` is the guest's disk, and a setup writes its answers onto it —
/// into SETUP.CFG, a file that also *ships in the bundle*. Staging used to copy
/// every bundle file over the drive on every launch, so the game read back the
/// shipped default and the player's choice was silently gone. Now a file the guest
/// has written is the guest's, and only a file still exactly as we staged it gets
/// refreshed from the bundle.
#[test]
fn a_setup_s_writes_survive_the_next_launch_and_the_bundle_still_refreshes() {
    let root = temp_parent("stage_provenance");
    game_bundle(&root, "game");
    let def = saisei::load_game_definition(&root, "game", None);

    // First launch: the drive is seeded from the bundle.
    let drive = saisei::copy_runtime(&root, &def);
    assert_eq!(
        std::fs::read(drive.join("SETUP.CFG")).unwrap(),
        b"sound=none"
    );

    // The setup runs and writes the player's choice onto the drive.
    std::fs::write(drive.join("SETUP.CFG"), b"sound=adlib").unwrap();

    // Next launch: the choice is still there, and is still there the launch after
    // that — the second one is the one that catches a staging record which
    // "adopts" the guest's file and hands it back to us to clobber.
    for launch in 1..=2 {
        saisei::copy_runtime(&root, &def);
        assert_eq!(
            std::fs::read(drive.join("SETUP.CFG")).unwrap(),
            b"sound=adlib",
            "launch {launch} reverted the setup's config"
        );
    }

    // A file the guest never touched is still ours: fix it in the bundle and the
    // fix reaches the drive. (Seed-once staging would have frozen this forever.)
    std::fs::write(root.join("games/game/DATA.DAT"), b"\x09\x09\x09").unwrap();
    saisei::copy_runtime(&root, &def);
    assert_eq!(
        std::fs::read(drive.join("DATA.DAT")).unwrap(),
        b"\x09\x09\x09",
        "a bundle file the guest never wrote must still refresh"
    );
    // ...and doing so did not disturb the guest's.
    assert_eq!(
        std::fs::read(drive.join("SETUP.CFG")).unwrap(),
        b"sound=adlib"
    );

    std::fs::remove_dir_all(&root).ok();
}

/// The list the "Run a file" menu is built from: everything the machine can be
/// booted on, except the game, which already has a Play button.
///
/// A `.COM` is deliberately NOT on it. `load_executable` reads the image size, the
/// entry cs:ip and the relocations out of an MZ header; a `.COM` has no header, so
/// it would take four code bytes for one and jump somewhere meaningless — which is
/// exactly what it did. Offering a file that can only hang is worse than not
/// offering it, so the list is what the loader can actually load.
#[test]
fn a_bundle_offers_the_programs_the_loader_can_actually_boot() {
    let root = temp_parent("bundle_exes");
    game_bundle(&root, "game");

    // A .COM helper, on the disk and in runtime[] exactly as a real one is.
    let dir = root.join("games/game");
    std::fs::write(dir.join("EXISTS.COM"), b"\xb4\x09\xcd\x21\xc3").unwrap();
    let cfg = dir.join("game.json");
    let text = std::fs::read_to_string(&cfg).unwrap().replace(
        r#"{"source":"games/game/DATA.DAT","dest":"DATA.DAT"}"#,
        r#"{"source":"games/game/DATA.DAT","dest":"DATA.DAT"},
           {"source":"games/game/EXISTS.COM","dest":"EXISTS.COM"}"#,
    );
    std::fs::write(&cfg, text).unwrap();

    let def = saisei::load_game_definition(&root, "game", None);
    assert_eq!(
        saisei::bundle_executables(&root, &def),
        ["SETUP.EXE"],
        "the game itself and the unbootable .COM are both left off"
    );
    std::fs::remove_dir_all(&root).ok();
}
