//! saisei launcher — builds/runs game bundles and generates their GameConfig.
//! Commands: build / run / play / new-game / triage / replay /
//! state-discover / control / run-with-pty / zbookend-diff / zoom (+ help /
//! version). See `docs/console.md` for the tiered command + flag reference.
//! JIT-only: `build` generates the per-game config and has cargo build the
//! saisei-game binary; all program code is JIT-compiled at run time by the
//! `saisei-jitc` binary.

use serde_json::Value;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{exit, Command};

/// The checkout's git short hash, baked in by build.rs (or "unknown" outside a
/// git checkout) — so no `git` process is spawned at run time.
pub const BUILD_REVISION: &str = env!("SAISEI_GIT_HASH");

pub mod control;
pub mod new_game;
pub mod replay;
pub mod run_with_pty;
pub mod state_discover;
pub mod triage;
pub mod zbookend_diff;
pub mod zoom;

fn die(msg: &str) -> ! {
    eprintln!("saisei: {msg}");
    exit(1)
}

/// Sanitize a string into a valid identifier (crate/symbol-safe).
pub fn sanitize_identifier(value: &str) -> String {
    let mut cleaned: String = value
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    cleaned = cleaned.trim_start_matches('_').to_string();
    if cleaned.is_empty() {
        cleaned = "bin".to_string();
    }
    if cleaned.chars().next().map_or(false, |c| c.is_ascii_digit()) {
        cleaned = format!("_{cleaned}");
    }
    cleaned.to_lowercase()
}

pub fn resolve_root() -> PathBuf {
    if let Ok(r) = std::env::var("SAISEI_REPO_ROOT") {
        return PathBuf::from(r);
    }
    if let Ok(exe) = std::env::current_exe() {
        // <repo>/target/{release,debug}/saisei
        if let Some(p) = exe.ancestors().nth(3) {
            if p.join("games").is_dir() && p.join("runtime").is_dir() {
                return p.to_path_buf();
            }
        }
    }
    let mut d = std::env::current_dir().unwrap_or_default();
    loop {
        if d.join("games").is_dir() && d.join("runtime").is_dir() {
            return d;
        }
        if !d.pop() {
            break;
        }
    }
    std::env::current_dir().unwrap_or_default()
}

pub struct GameDef {
    pub name: String,
    pub key: String,
    pub config_path: PathBuf,
    pub runtime: Vec<(String, String)>,
    pub program_path: String,
    pub program: String,
    pub program_key: String,
}

fn programs(data: &Value) -> Vec<Value> {
    if let Some(progs) = data.get("programs").and_then(Value::as_array) {
        if !progs.is_empty() {
            return progs.clone();
        }
    }
    let name = data.get("name").and_then(Value::as_str).unwrap_or("main");
    let mut m = serde_json::Map::new();
    m.insert("name".into(), Value::String(name.into()));
    m.insert(
        "program_path".into(),
        data.get("program_path").cloned().unwrap_or(Value::Null),
    );
    vec![Value::Object(m)]
}

pub fn load_game_definition(root: &Path, name: &str, program: Option<&str>) -> GameDef {
    let config_path = root.join("games").join(name).join(format!("{name}.json"));
    if !config_path.exists() {
        die(&format!(
            "Unknown game '{name}'. Expected config at {}",
            config_path.display()
        ));
    }
    let data: Value = serde_json::from_slice(
        &std::fs::read(&config_path).unwrap_or_else(|e| die(&format!("read config: {e}"))),
    )
    .unwrap_or_else(|e| die(&format!("parse config: {e}")));

    let progs = programs(&data);
    let want = program
        .map(str::to_string)
        .or_else(|| {
            data.get("default_program")
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .or_else(|| {
            progs
                .first()
                .and_then(|p| p.get("name"))
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .unwrap_or_default();
    let prog = progs
        .iter()
        .find(|p| p.get("name").and_then(Value::as_str) == Some(want.as_str()))
        .unwrap_or_else(|| {
            let names: Vec<&str> = progs
                .iter()
                .filter_map(|p| p.get("name").and_then(Value::as_str))
                .collect();
            die(&format!(
                "Game '{name}' has no program '{want}'. Available: {}",
                names.join(", ")
            ));
        });

    let mut runtime = Vec::new();
    for item in data
        .get("runtime")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
    {
        let source = item
            .get("source")
            .and_then(Value::as_str)
            .unwrap_or_else(|| {
                die(&format!(
                    "Invalid runtime entry in {}",
                    config_path.display()
                ));
            });
        let dest = item
            .get("dest")
            .and_then(Value::as_str)
            .map(str::to_string)
            .unwrap_or_else(|| {
                Path::new(source)
                    .file_name()
                    .unwrap()
                    .to_string_lossy()
                    .into_owned()
            });
        runtime.push((source.to_string(), dest));
    }

    let program_path = prog
        .get("program_path")
        .and_then(Value::as_str)
        .unwrap_or_else(|| {
            die(&format!(
                "Program '{want}' in '{name}' missing program_path"
            ))
        })
        .to_string();
    let game_name = data
        .get("name")
        .and_then(Value::as_str)
        .map(str::to_string)
        .unwrap_or_else(|| {
            config_path
                .file_stem()
                .unwrap()
                .to_string_lossy()
                .into_owned()
        });

    GameDef {
        key: sanitize_identifier(&game_name),
        name: game_name,
        config_path,
        runtime,
        program_path,
        program_key: sanitize_identifier(&want),
        program: want,
    }
}

// ---------- per-game GameConfig generation ----------

fn as_int(v: Option<&Value>) -> i64 {
    match v {
        None | Some(Value::Null) => 0,
        Some(Value::String(s)) => {
            let s = s.trim();
            if let Some(h) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
                i64::from_str_radix(h, 16).unwrap_or(0)
            } else {
                s.parse().unwrap_or(0)
            }
        }
        Some(v) => v.as_i64().unwrap_or(0),
    }
}

/// The machine parameters a GameConfig carries, read out of `<name>.json`.
///
/// Two consumers need these and must not disagree: `generate_game_config` bakes
/// them into a frozen per-game binary, and the player host installs them at run
/// time via `saisei_set_game_config`. Both go through here.
pub struct GameConfigValues {
    pub name: String,
    pub program_path: String,
    pub init_cs: u16,
    pub psp_seg: u16,
    /// (lo, hi, name) — runtime memory-protection ranges.
    pub protected_slots: Vec<(u32, u32, String)>,
}

pub fn game_config_values(game: &GameDef) -> GameConfigValues {
    let data: Value = serde_json::from_slice(&std::fs::read(&game.config_path).unwrap()).unwrap();
    let name = data
        .get("name")
        .and_then(Value::as_str)
        .map(str::to_string)
        .unwrap_or_else(|| {
            game.config_path
                .file_stem()
                .unwrap()
                .to_string_lossy()
                .into_owned()
        });

    // _select_program
    let progs = data.get("programs").and_then(Value::as_array);
    let prog: Value = match progs {
        None => data.clone(),
        Some(ps) if ps.is_empty() => data.clone(),
        Some(ps) => {
            let want = Some(game.program.as_str())
                .filter(|s| !s.is_empty())
                .or_else(|| data.get("default_program").and_then(Value::as_str))
                .or_else(|| {
                    ps.first()
                        .and_then(|p| p.get("name"))
                        .and_then(Value::as_str)
                })
                .unwrap_or("");
            ps.iter()
                .find(|p| p.get("name").and_then(Value::as_str) == Some(want))
                .cloned()
                .unwrap_or_else(|| die("game_config_values: no matching program"))
        }
    };

    let pick = |k: &str| prog.get(k).or_else(|| data.get(k));
    GameConfigValues {
        name,
        program_path: game.program_path.clone(),
        init_cs: (as_int(pick("init_cs")) & 0xFFFF) as u16,
        psp_seg: (as_int(pick("psp_seg")) & 0xFFFF) as u16,
        protected_slots: pick("protected_slots")
            .and_then(Value::as_array)
            .map(|slots| {
                slots
                    .iter()
                    .map(|s| {
                        (
                            as_int(s.get("lo")) as u32,
                            as_int(s.get("hi")) as u32,
                            s.get("name")
                                .and_then(Value::as_str)
                                .unwrap_or("")
                                .to_string(),
                        )
                    })
                    .collect()
            })
            .unwrap_or_default(),
    }
}

/// Emit the per-game GameConfig (Rust data file) from the <name>.json config.
pub fn generate_game_config(root: &Path, game: &GameDef) -> PathBuf {
    let out_path = root
        .join("build")
        .join(format!("{}_game_config.rs", game.program_key));
    let vals = game_config_values(game);
    let game_name = vals.name;
    let init_cs = vals.init_cs;
    let psp_seg = vals.psp_seg;
    let slots = vals.protected_slots;

    // Emit the per-game GameConfig data file, include!d into the saisei-game
    // bin crate. The `#[repr(C)]` struct layout MUST match the `game_config`
    // extern layout the runtime binds (runtime shims.rs) byte-for-byte.
    let byte_arr = |s: &str| -> (String, usize) {
        let bytes: Vec<String> = s
            .bytes()
            .chain(std::iter::once(0u8))
            .map(|b| format!("0x{b:02X}"))
            .collect();
        let len = bytes.len();
        (format!("[{}]", bytes.join(", ")), len)
    };
    let mut out = String::new();
    out.push_str("// Generated by saisei; do not edit by hand. include!d into the\n");
    out.push_str("// saisei-game bin crate (see its build.rs), so no inner attributes.\n\n");
    out.push_str("use core::ffi::{c_char, c_int};\n\n");
    // Type aliases + structs, matching the runtime's game_config layout exactly.
    out.push_str("type GameFunc = Option<unsafe extern \"C\" fn(u16, *const c_char, *const c_char, c_int)>;\n");
    out.push_str("type DispatchFn = Option<unsafe extern \"C\" fn(c_int, u16, *const c_char, *const c_char, c_int)>;\n");
    out.push_str("type PatchFn = Option<unsafe extern \"C\" fn(u16, *const c_char, *const c_char, c_int) -> c_int>;\n");
    out.push_str("#[repr(C)] pub struct ProtectedSlot { pub lo: u32, pub hi: u32, pub name: *const c_char }\n");
    out.push_str("#[repr(C)] pub struct CallTarget { pub addr: u32, pub file: *const c_char, pub func: GameFunc }\n");
    out.push_str("#[repr(C)] pub struct BinaryDispatch { pub file: *const c_char, pub module: *const c_char, pub cs_base: u16, pub func: DispatchFn }\n");
    out.push_str("#[repr(C)] pub struct GamePatch { pub file: *const c_char, pub file_off: u32, pub func: PatchFn, pub name: *const c_char, pub enabled: c_int }\n");
    out.push_str("#[repr(C)] pub struct GameConfig {\n");
    out.push_str(
        "    pub name: *const c_char, pub program_path: *const c_char, pub entry: GameFunc,\n",
    );
    out.push_str("    pub call_targets: *const CallTarget, pub call_target_count: usize,\n");
    out.push_str(
        "    pub binary_dispatch: *const BinaryDispatch, pub binary_dispatch_count: usize,\n",
    );
    out.push_str(
        "    pub protected_slots: *const ProtectedSlot, pub protected_slot_count: usize,\n",
    );
    out.push_str("    pub init_cs: u16, pub psp_seg: u16, pub patches: *const GamePatch, pub patch_count: usize,\n");
    out.push_str("}\n");
    out.push_str("unsafe impl Sync for GameConfig {}\nunsafe impl Sync for ProtectedSlot {}\n\n");
    let (name_arr, name_len) = byte_arr(&game_name);
    out.push_str(&format!("static NAME: [u8; {name_len}] = {name_arr};\n"));
    let (path_arr, path_len) = byte_arr(&game.program_path);
    out.push_str(&format!(
        "static PROGRAM_PATH: [u8; {path_len}] = {path_arr};\n"
    ));
    let slots_expr = if slots.is_empty() {
        "core::ptr::null()".to_string()
    } else {
        for (i, (_, _, nm)) in slots.iter().enumerate() {
            let (arr, len) = byte_arr(nm);
            out.push_str(&format!("static SLOT_NAME_{i}: [u8; {len}] = {arr};\n"));
        }
        out.push_str(&format!(
            "static PROTECTED_SLOTS: [ProtectedSlot; {}] = [\n",
            slots.len()
        ));
        for (i, (lo, hi, _)) in slots.iter().enumerate() {
            out.push_str(&format!(
                "    ProtectedSlot {{ lo: 0x{lo:05X}, hi: 0x{hi:05X}, name: SLOT_NAME_{i}.as_ptr() as *const c_char }},\n"
            ));
        }
        out.push_str("];\n");
        "PROTECTED_SLOTS.as_ptr()".to_string()
    };
    out.push_str("\n#[no_mangle]\npub static game_config: GameConfig = GameConfig {\n");
    out.push_str("    name: NAME.as_ptr() as *const c_char,\n");
    out.push_str("    program_path: PROGRAM_PATH.as_ptr() as *const c_char,\n");
    out.push_str("    entry: None,\n");
    out.push_str("    call_targets: core::ptr::null(), call_target_count: 0,\n");
    out.push_str("    binary_dispatch: core::ptr::null(), binary_dispatch_count: 0,\n");
    out.push_str(&format!(
        "    protected_slots: {slots_expr}, protected_slot_count: {},\n",
        slots.len()
    ));
    out.push_str(&format!(
        "    init_cs: 0x{:04X}, psp_seg: 0x{:04X},\n",
        init_cs & 0xFFFF,
        psp_seg & 0xFFFF
    ));
    out.push_str("    patches: core::ptr::null(), patch_count: 0,\n};\n");
    std::fs::create_dir_all(out_path.parent().unwrap()).ok();
    std::fs::write(&out_path, out).unwrap_or_else(|e| die(&format!("write config: {e}")));
    out_path
}

// ---------- compile / link ----------

/// Build the game binary: cargo compiles the `saisei-game` bin crate (the
/// runtime rlib + the generated per-game GameConfig, pulled in via
/// $SAISEI_GAME_CONFIG by its build.rs, linked -rdynamic for the JIT chunk
/// dlopens) and the result is copied to build/<game>/<program>. No C toolchain
/// anywhere; rustc drives the link.
fn compile_game(root: &Path, game: &GameDef, config_source: &Path, features: &[String]) -> PathBuf {
    let output_dir = root.join("build").join(&game.key);
    std::fs::create_dir_all(&output_dir).ok();
    let binary_path = output_dir.join(&game.program_key);

    let config_abs = config_source
        .canonicalize()
        .unwrap_or_else(|_| config_source.to_path_buf());
    let mut cmd = Command::new("cargo");
    cmd.current_dir(root)
        .env("SAISEI_GAME_CONFIG", config_abs.display().to_string())
        .args(["build", "--release", "-p", "saisei-game"]);
    for f in features {
        cmd.args(["--features", &format!("saisei-game/{f}")]);
    }
    let status = cmd.status();
    if !matches!(status, Ok(s) if s.success()) {
        die("cargo build -p saisei-game failed");
    }

    let built = root.join("target/release/saisei-game");
    std::fs::copy(&built, &binary_path).unwrap_or_else(|e| {
        die(&format!(
            "copy {} -> {}: {e}",
            built.display(),
            binary_path.display()
        ))
    });
    binary_path
}

pub fn copy_runtime(root: &Path, game: &GameDef) -> PathBuf {
    let output_dir = root.join("build").join(&game.key);
    std::fs::create_dir_all(&output_dir).ok();
    for (source, dest) in &game.runtime {
        let src_path = root.join(source);
        if !src_path.exists() {
            die(&format!("Runtime file not found: {}", src_path.display()));
        }
        let dest_path = output_dir.join(dest);
        if src_path.is_dir() {
            copy_dir(&src_path, &dest_path);
        } else {
            if let Some(p) = dest_path.parent() {
                std::fs::create_dir_all(p).ok();
            }
            std::fs::copy(&src_path, &dest_path).ok();
        }
    }
    output_dir
}

fn copy_dir(src: &Path, dst: &Path) {
    std::fs::create_dir_all(dst).ok();
    if let Ok(rd) = std::fs::read_dir(src) {
        for e in rd.flatten() {
            let from = e.path();
            let to = dst.join(e.file_name());
            if from.is_dir() {
                copy_dir(&from, &to);
            } else {
                std::fs::copy(&from, &to).ok();
            }
        }
    }
}

pub fn build(root: &Path, game: &GameDef) -> PathBuf {
    build_with(root, game, &[])
}

pub fn build_with(root: &Path, game: &GameDef, features: &[String]) -> PathBuf {
    let config_source = generate_game_config(root, game);
    compile_game(root, game, &config_source, features)
}

/// `build --warm`: after building, drive the REAL program headless for
/// `secs` seconds so the JIT populates the bundle's chunk cache with exactly
/// the code the boot path executes (plus everything the per-segment
/// speculative pre-compile discovers from it). No ahead-of-time decode and no
/// heuristics — the warm cache is the byproduct of a real run, so a shipped
/// bundle's first `saisei play` starts hot. The cache is content-addressed;
/// re-warming is idempotent and killing the run mid-compile is safe.
pub fn warm_game_cache(root: &Path, game: &GameDef, secs: u64) {
    let binary_path = build(root, game);
    let runtime_dir = copy_runtime(root, game);
    let env = game_process_env(root, game);

    println!(
        "warm: running {} headless for {secs}s to populate the JIT cache",
        game.key
    );
    let child = Command::new(format!(
        "./{}",
        binary_path.file_name().unwrap().to_string_lossy()
    ))
    .args(["--headless", "--speedup", "1"])
    .current_dir(&runtime_dir)
    .env_clear()
    .envs(&env)
    .stdin(std::process::Stdio::null())
    .stdout(std::process::Stdio::null())
    .stderr(std::process::Stdio::null())
    .spawn();
    let mut child = match child {
        Ok(c) => c,
        Err(e) => die(&format!("warm: failed to launch game: {e}")),
    };
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(secs);
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                println!("warm: game exited early ({status}) — cache keeps what it reached");
                break;
            }
            Ok(None) if std::time::Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                break;
            }
            Ok(None) => std::thread::sleep(std::time::Duration::from_millis(200)),
            Err(_) => break,
        }
    }

    // The runtime detaches one `saisei-jitc speculate` per segment; those
    // children keep filling the cache after the game dies. Wait for the jit
    // dir to go quiet (no new/updated files for 5s, capped at 10 minutes).
    let jit_dir = root.join("build").join(&game.key).join("jit");
    let newest_mtime = |dir: &Path| -> Option<std::time::SystemTime> {
        std::fs::read_dir(dir)
            .ok()?
            .flatten()
            .filter_map(|e| e.metadata().ok()?.modified().ok())
            .max()
    };
    let quiesce_cap = std::time::Instant::now() + std::time::Duration::from_secs(600);
    while std::time::Instant::now() < quiesce_cap {
        let before = newest_mtime(&jit_dir);
        std::thread::sleep(std::time::Duration::from_secs(5));
        if newest_mtime(&jit_dir) == before {
            break;
        }
        println!("warm: speculative compiles still running...");
    }
    let (mut chunks, mut bytes) = (0u64, 0u64);
    if let Ok(rd) = std::fs::read_dir(&jit_dir) {
        for e in rd.flatten() {
            if e.path().extension().map_or(false, |x| x == "so") {
                chunks += 1;
                bytes += e.metadata().map(|m| m.len()).unwrap_or(0);
            }
        }
    }
    println!(
        "warm: cache ready — {chunks} compiled chunk(s), {:.1} MB in {}",
        bytes as f64 / 1e6,
        jit_dir.display()
    );
}

#[derive(Default)]
struct RunOpts {
    headless: bool,
    verbose: bool,
    speedup: f64,
    restore_from: Option<String>,
    trace_file: Option<String>,
    lifecycle_file: Option<String>,
    screenshot_secs: Option<i64>,
    replay: bool,
    // Developer: a game-function patch bundle (.so) forwarded to the runtime,
    // which registers it at startup (runtime already parses --patch-bundle).
    patch_bundle: Option<String>,
    // cargo features forwarded to the saisei-game build
    // (e.g. force_exit_after_10s for self-terminating smoke runs).
    features: Vec<String>,
}

/// The environment every game process gets (run, play, and the build --warm
/// cache run): repo root, translator path, per-game JIT cache dir, git hash.
pub fn game_process_env(root: &Path, game: &GameDef) -> BTreeMap<String, String> {
    let mut env: BTreeMap<String, String> = std::env::vars().collect();
    env.insert("SAISEI_REPO_ROOT".into(), root.display().to_string());
    // Crash manifests carry the repo git hash (runtime_version in shims.rs);
    // exported at run time so the runtime never rebuilds per commit. Baked into
    // this binary by build.rs — no `git` process per launch.
    if BUILD_REVISION != "unknown" {
        env.insert("SAISEI_RUNTIME_VERSION".into(), BUILD_REVISION.to_string());
    }
    // The Rust JIT translator; the runtime needs no the reference when this is set.
    if !env.contains_key("SAISEI_JITC") {
        let jitc = root.join("target").join("release").join("saisei-jitc");
        if jitc.exists() {
            env.insert("SAISEI_JITC".into(), jitc.display().to_string());
        }
    }
    env.insert(
        "SAISEI_JIT_DIR".into(),
        root.join("build")
            .join(&game.key)
            .join("jit")
            .display()
            .to_string(),
    );
    env
}

/// Build the player, which is the host that runs every game.
///
/// `run`/`play` go through it rather than through a per-game binary: the game's
/// GameConfig is read from its `<name>.json` at run time, so nothing is compiled
/// per game and switching games costs nothing. (`build` still produces the
/// per-game binary — that is the artifact the future frozen build is made of.)
fn build_player(root: &Path, features: &[String]) -> PathBuf {
    let mut cmd = Command::new("cargo");
    cmd.current_dir(root)
        .args(["build", "--release", "-p", "saisei-player"]);
    for f in features {
        cmd.args(["--features", &format!("saisei-player/{f}")]);
    }
    if !matches!(cmd.status(), Ok(s) if s.success()) {
        die("cargo build -p saisei-player failed");
    }
    root.join("target/release/saisei")
}

fn run_game(root: &Path, game: &GameDef, o: &RunOpts) -> ! {
    let player = build_player(root, &o.features);
    let runtime_dir = copy_runtime(root, game);

    // The player does its own staging (it copies the bundle's runtime files and
    // enters build/<game>/), so it is given the game by name, not a path.
    let mut cmd = vec![
        player.display().to_string(),
        "--play".into(),
        game.key.clone(),
    ];
    if !game.program.is_empty() {
        cmd.push("--program".into());
        cmd.push(game.program.clone());
    }
    if o.headless {
        cmd.push("--headless".into());
    }
    cmd.push("--speedup".into());
    cmd.push(format!("{}", o.speedup));
    if let Some(r) = &o.restore_from {
        cmd.push("--restore-from".into());
        cmd.push(
            std::fs::canonicalize(r)
                .unwrap_or_else(|_| PathBuf::from(r))
                .display()
                .to_string(),
        );
    }
    // Developer / drive knobs go straight to the game binary's argv (parsed in
    // the runtime's saisei_main). Trace/lifecycle paths are canonicalized here
    // because the child runs with cwd = runtime_dir, not the launcher's cwd.
    if o.verbose {
        cmd.push("--verbose".into());
    }
    if let Some(t) = &o.trace_file {
        cmd.push("--trace-file".into());
        cmd.push(
            std::fs::canonicalize(t)
                .unwrap_or_else(|_| PathBuf::from(t))
                .display()
                .to_string(),
        );
    }
    if let Some(l) = &o.lifecycle_file {
        cmd.push("--lifecycle-file".into());
        cmd.push(
            std::fs::canonicalize(l)
                .unwrap_or_else(|_| PathBuf::from(l))
                .display()
                .to_string(),
        );
    }
    if let Some(s) = o.screenshot_secs {
        cmd.push("--screenshot-secs".into());
        cmd.push(s.to_string());
    }
    if o.replay {
        cmd.push("--replay".into());
    }
    if let Some(pb) = &o.patch_bundle {
        cmd.push("--patch-bundle".into());
        cmd.push(
            std::fs::canonicalize(pb)
                .unwrap_or_else(|_| PathBuf::from(pb))
                .display()
                .to_string(),
        );
    }

    let env = game_process_env(root, game);

    let status = Command::new(&cmd[0])
        .args(&cmd[1..])
        .current_dir(&runtime_dir)
        .env_clear()
        .envs(&env)
        .status();
    let rc = match status {
        Ok(s) => s.code().unwrap_or_else(|| {
            #[cfg(unix)]
            {
                use std::os::unix::process::ExitStatusExt;
                s.signal().map(|sig| -sig).unwrap_or(1)
            }
            #[cfg(not(unix))]
            {
                1
            }
        }),
        Err(e) => die(&format!("failed to launch {}: {e}", cmd[0])),
    };
    exit(rc);
}

fn parse_run_opts(args: &[String], with_headless: bool) -> (Option<String>, RunOpts) {
    let mut program = None;
    let mut o = RunOpts {
        speedup: 1.0,
        ..Default::default()
    };
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--program" => program = it.next().cloned(),
            "--headless" if with_headless => o.headless = true,
            "--verbose" => o.verbose = true,
            "--speedup" => {
                o.speedup = it
                    .next()
                    .and_then(|v| v.parse().ok())
                    .unwrap_or_else(|| die("--speedup needs a number"))
            }
            "--restore-from" => o.restore_from = it.next().cloned(),
            "--trace-file" => o.trace_file = it.next().cloned(),
            "--lifecycle-file" => o.lifecycle_file = it.next().cloned(),
            "--screenshot-secs" => o.screenshot_secs = it.next().and_then(|v| v.parse().ok()),
            "--replay" => o.replay = true,
            "--patch-bundle" => o.patch_bundle = it.next().cloned(),
            "--features" => o.features.extend(
                it.next()
                    .unwrap_or_else(|| die("--features needs a value"))
                    .split(',')
                    .map(str::to_string),
            ),
            other if other.starts_with("--speedup=") => {
                o.speedup = other["--speedup=".len()..]
                    .parse()
                    .unwrap_or_else(|_| die("bad --speedup"))
            }
            other => die(&format!("unrecognized argument: {other}")),
        }
    }
    o.speedup = validate_speedup(o.speedup).unwrap_or_else(|e| die(&e));
    (program, o)
}

/// Validate a parsed `--speedup` value. 's positivity check:
/// a positive number passes through; 0 or negative is rejected with the exact
/// message the reference raised via SystemExit.
pub fn validate_speedup(v: f64) -> Result<f64, String> {
    if v > 0.0 {
        Ok(v)
    } else {
        Err("--speedup must be a positive number".to_string())
    }
}

/// The full command surface, grouped by audience (see `print_usage`). Printed
/// on `help`/`--help`, and (to stderr) when invoked with no command.
const USAGE: &str = "\
saisei — runs DOS MZ executables by JIT-recompiling them to native code.

usage: saisei <command> [options]

Player — play a game:
  play <game>              build and run in the SDL window
  run <game>               build and run (headless-capable; for automation)
  build <game>             build the game binary (run/play do this for you)
  new-game <archive>       create a game bundle from a zip / directory / url

Developer — debug and reverse-engineer:
  triage                   inspect the newest crash bundle
  state-discover           discover memory-state predicates across snapshots
  zbookend-diff <a> <b>    diff two snapshots to find who wrote an address
  zoom <img> <col> <row>   pixel-zoom a screenshot tile

Drive — automate a running game (often from an AI/agent):
  control <cmd>            drive a running game via its FIFO
                           (tap press release enter space shot raw
                            halt resume step read snapshot status; 'control help')
  replay <log>             replay a recorded session against a --replay run
  run-with-pty <cmd>       run a command under a pseudo-terminal

run / play options:
  --program <name>         pick a program in a multi-executable bundle   [player]
  --restore-from <save>    resume from a snapshot                        [player]
  --speedup <n>            emulation speed multiplier (default 1)        [player]
  --headless               run without an SDL window (run only)         [drive]
  --screenshot-secs <n>    auto-dump PNGs every n seconds (headless)     [drive]
  --replay                 record inputs for later replay               [drive]
  --verbose                stream the shim trace to stdout              [dev]
  --trace-file <path>      write the execution trace to a file          [dev]
  --lifecycle-file <path>  write LOAD/CALL/JMP lifecycle events          [dev]
  --patch-bundle <path>    load a game-function patch .so                [dev]
  --features <list>        cargo features for the game build            [dev]

build options:
  --warm[-secs <n>]        after building, warm the JIT cache for n seconds

Run 'saisei version' for the build revision.";

/// Build revision for `saisei version` — baked in at compile time by build.rs.
fn version_string() -> String {
    BUILD_REVISION.to_string()
}

pub fn run() {
    let root = resolve_root();
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let (cmd, rest) = argv.split_first().unwrap_or_else(|| {
        eprintln!("{USAGE}");
        exit(2)
    });
    match cmd.as_str() {
        "help" | "--help" | "-h" => {
            println!("{USAGE}");
            exit(0)
        }
        "version" | "--version" | "-V" => {
            println!("saisei {}", version_string());
            exit(0)
        }
        "new-game" => new_game::main(&root, rest),
        "triage" => triage::main(&root, rest),
        "replay" => replay::main(&root, rest),
        "state-discover" => state_discover::main(&root, rest),
        "control" => control::main(&root, rest),
        "run-with-pty" => run_with_pty::main(&root, rest),
        "zbookend-diff" => zbookend_diff::main(&root, rest),
        "zoom" => zoom::main(&root, rest),
        "build" => {
            let mut program = None;
            let mut game_name = None;
            let mut warm = false;
            let mut warm_secs: u64 = 60;
            let mut it = rest.iter();
            while let Some(a) = it.next() {
                match a.as_str() {
                    "--program" => program = it.next().cloned(),
                    "--warm" => warm = true,
                    "--warm-secs" => {
                        warm = true;
                        warm_secs = it
                            .next()
                            .and_then(|v| v.parse().ok())
                            .unwrap_or_else(|| die("--warm-secs needs a number of seconds"));
                    }
                    other if other.starts_with("--") => die(&format!("unknown flag: {other}")),
                    other => game_name = Some(other.to_string()),
                }
            }
            let game = load_game_definition(
                &root,
                &game_name.unwrap_or_else(|| die("build needs a game name")),
                program.as_deref(),
            );
            if warm {
                warm_game_cache(&root, &game, warm_secs);
            } else {
                build(&root, &game);
            }
        }
        "run" | "play" => {
            let game_name = rest
                .iter()
                .find(|a| !a.starts_with("--"))
                .cloned()
                .unwrap_or_else(|| die("run needs a game name"));
            let flags: Vec<String> = {
                // everything except the (first) positional game name
                let mut seen = false;
                rest.iter()
                    .filter(|a| {
                        if !a.starts_with("--") && !seen {
                            seen = true;
                            false
                        } else {
                            true
                        }
                    })
                    .cloned()
                    .collect()
            };
            let (program, opts) = parse_run_opts(&flags, cmd == "run");
            let game = load_game_definition(&root, &game_name, program.as_deref());
            run_game(&root, &game, &opts);
        }
        other => die(&format!(
            "unknown command: {other}\nrun 'saisei help' to see available commands"
        )),
    }
}
