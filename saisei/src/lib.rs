//! saisei launcher — builds/runs game bundles and generates their GameConfig C.
//! Commands: build / run / play / copy-runtime / new-game / triage / replay /
//! state-discover / control / run-with-pty / zbookend-diff / zoom.
//! JIT-only: `build` emits the per-game config and links the runtime; all program
//! code is JIT-compiled at run time by the `saisei-jitc` binary.

use serde_json::Value;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{exit, Command};

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

/// Sanitize a string into a valid C identifier.
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

// ---------- per-game GameConfig C generation ----------

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

/// Emit the per-game GameConfig C from the <name>.json config.
pub fn generate_game_config(root: &Path, game: &GameDef) -> PathBuf {
    let out_path = root
        .join("build")
        .join(format!("{}_game_config.c", game.program_key));
    let data: Value = serde_json::from_slice(&std::fs::read(&game.config_path).unwrap()).unwrap();
    let game_name = data
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
                .unwrap_or_else(|| die("generate_game_config: no matching program"))
        }
    };

    let pick = |k: &str| prog.get(k).or_else(|| data.get(k));
    let init_cs = as_int(pick("init_cs"));
    let psp_seg = as_int(pick("psp_seg"));
    let slots = pick("protected_slots")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();

    let jstr = |s: &str| serde_json::to_string(s).unwrap();
    let mut out = String::new();
    out.push_str("// Generated by the source; do not edit by hand.\n\n");
    out.push_str("#include \"game_config.h\"\n\n");
    if !slots.is_empty() {
        out.push_str("static const ProtectedSlot game_protected_slots[] = {\n");
        for s in &slots {
            let lo = as_int(s.get("lo"));
            let hi = as_int(s.get("hi"));
            let nm = s.get("name").and_then(Value::as_str).unwrap_or("");
            out.push_str(&format!("    {{0x{lo:05X}, 0x{hi:05X}, {}}},\n", jstr(nm)));
        }
        out.push_str("};\n");
        out.push_str(
            "static const size_t game_protected_slot_count = sizeof(game_protected_slots) / sizeof(game_protected_slots[0]);\n\n",
        );
    } else {
        out.push_str("static const ProtectedSlot *const game_protected_slots = NULL;\n");
        out.push_str("static const size_t game_protected_slot_count = 0;\n\n");
    }
    out.push_str("const GameConfig game_config = {\n");
    out.push_str(&format!("    .name = {},\n", jstr(&game_name)));
    out.push_str(&format!(
        "    .program_path = {},\n",
        jstr(&game.program_path)
    ));
    out.push_str("    .entry = NULL,\n");
    out.push_str("    .call_targets = NULL,\n");
    out.push_str("    .call_target_count = 0,\n");
    out.push_str("    .binary_dispatch = NULL,\n");
    out.push_str("    .binary_dispatch_count = 0,\n");
    out.push_str("    .protected_slots = game_protected_slots,\n");
    out.push_str("    .protected_slot_count = game_protected_slot_count,\n");
    out.push_str(&format!("    .init_cs = 0x{:04X},\n", init_cs & 0xFFFF));
    out.push_str(&format!("    .psp_seg = 0x{:04X},\n", psp_seg & 0xFFFF));
    out.push_str("};\n");

    std::fs::create_dir_all(out_path.parent().unwrap()).ok();
    std::fs::write(&out_path, out).unwrap_or_else(|e| die(&format!("write config: {e}")));
    out_path
}

// ---------- compile / link ----------

fn pkg_config(args: &[&str]) -> Option<Vec<String>> {
    for tool in ["pkg-config", "pkgconf"] {
        if let Ok(out) = Command::new(tool).args(args).output() {
            if out.status.success() {
                let s = String::from_utf8_lossy(&out.stdout);
                let s = s.trim();
                return Some(if s.is_empty() {
                    vec![]
                } else {
                    s.split_whitespace().map(str::to_string).collect()
                });
            }
        }
    }
    None
}

fn resolve_sdl_flags() -> (Vec<String>, Vec<String>) {
    let cflags = pkg_config(&["--cflags", "sdl2"]).unwrap_or_else(|| {
        vec![
            "-I/opt/homebrew/include/SDL2".into(),
            "-D_THREAD_SAFE".into(),
        ]
    });
    let libs = pkg_config(&["--libs", "sdl2"])
        .unwrap_or_else(|| vec!["-L/opt/homebrew/lib".into(), "-lSDL2".into()]);
    (cflags, libs)
}

fn mtime(p: &Path) -> f64 {
    p.metadata()
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

fn run_clang(root: &Path, args: &[String]) {
    let status = Command::new("clang").args(args).current_dir(root).status();
    if !matches!(status, Ok(s) if s.success()) {
        die("clang failed");
    }
}

fn compile_game(root: &Path, game: &GameDef, config_source: &Path) -> PathBuf {
    let output_dir = root.join("build").join(&game.key);
    std::fs::create_dir_all(&output_dir).ok();
    let binary_path = output_dir.join(&game.program_key);
    let obj_dir = output_dir.join(format!("obj_{}", game.program_key));
    std::fs::create_dir_all(&obj_dir).ok();

    let (cflags, libs) = resolve_sdl_flags();
    let version = Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .current_dir(root)
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "unknown".into());
    let extra_cflags: Vec<String> = std::env::var("CFLAGS")
        .unwrap_or_default()
        .split_whitespace()
        .map(str::to_string)
        .collect();

    let mut cc_flags: Vec<String> = vec![
        "-Iruntime/include".into(),
        format!("-I{}", output_dir.display()),
        "-O2".into(),
        format!("-DRUNTIME_VERSION=\"{version}\""),
    ];
    cc_flags.extend(cflags);
    cc_flags.extend(["-pthread".into(), "-DSDL_MAIN_HANDLED".into()]);
    cc_flags.extend(extra_cflags);

    let inc = root.join("runtime").join("include");
    let mut headers_newest = 0.0f64;
    if let Ok(rd) = std::fs::read_dir(&inc) {
        for e in rd.flatten() {
            if e.path().extension().and_then(|x| x.to_str()) == Some("h") {
                headers_newest = headers_newest.max(mtime(&e.path()));
            }
        }
    }

    let rel = |parts: &[&str]| -> PathBuf {
        let mut p = root.to_path_buf();
        for s in parts {
            p.push(s);
        }
        p
    };
    let shim_sources = [
        rel(&["runtime", "core", "shims.c"]),
        rel(&["runtime", "core", "snapshot.c"]),
        rel(&["runtime", "core", "save_manager.c"]),
        rel(&["runtime", "display", "virtual_display_sdl.c"]),
        rel(&["runtime", "hw", "io_bus.c"]),
        rel(&["runtime", "hw", "audio.c"]),
        rel(&["runtime", "hw", "video.c"]),
        rel(&["runtime", "hw", "keyboard.c"]),
        rel(&["runtime", "hw", "timer.c"]),
        rel(&["runtime", "os", "dos.c"]),
        rel(&["runtime", "os", "bios.c"]),
        rel(&["runtime", "os", "mouse.c"]),
    ];
    let mut all_srcs: Vec<PathBuf> = shim_sources.to_vec();
    all_srcs.push(config_source.to_path_buf());

    let mut obj_paths: Vec<PathBuf> = Vec::new();
    for src in &all_srcs {
        let stem = src.file_stem().unwrap().to_string_lossy();
        let obj = obj_dir.join(format!("{stem}.o"));
        obj_paths.push(obj.clone());
        let need = !obj.exists() || mtime(&obj) < mtime(src) || mtime(&obj) < headers_newest;
        if !need {
            continue;
        }
        let mut args = vec!["-c".to_string(), src.display().to_string()];
        args.extend(cc_flags.clone());
        args.extend(["-o".into(), obj.display().to_string()]);
        run_clang(root, &args);
    }

    let mut args: Vec<String> = obj_paths.iter().map(|o| o.display().to_string()).collect();
    args.extend(cc_flags.clone());
    args.extend(["-o".into(), binary_path.display().to_string()]);
    args.extend(libs);
    args.extend(["-pthread".into(), "-rdynamic".into(), "-ldl".into()]);
    run_clang(root, &args);
    binary_path
}

fn copy_runtime(root: &Path, game: &GameDef) -> PathBuf {
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
    let config_source = generate_game_config(root, game);
    compile_game(root, game, &config_source)
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
}

fn run_game(root: &Path, game: &GameDef, o: &RunOpts) -> ! {
    let binary_path = build(root, game);
    let runtime_dir = copy_runtime(root, game);

    let mut cmd = vec![format!(
        "./{}",
        binary_path.file_name().unwrap().to_string_lossy()
    )];
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

    let mut env: BTreeMap<String, String> = std::env::vars().collect();
    env.insert("SAISEI_REPO_ROOT".into(), root.display().to_string());
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
    if o.verbose {
        env.insert("SAISEI_VERBOSE".into(), "1".into());
    }
    if let Some(t) = &o.trace_file {
        env.insert(
            "TRACE_FILE".into(),
            std::fs::canonicalize(t)
                .unwrap_or_else(|_| PathBuf::from(t))
                .display()
                .to_string(),
        );
    }
    if let Some(l) = &o.lifecycle_file {
        env.insert(
            "LIFECYCLE_FILE".into(),
            std::fs::canonicalize(l)
                .unwrap_or_else(|_| PathBuf::from(l))
                .display()
                .to_string(),
        );
    }
    if let Some(s) = o.screenshot_secs {
        env.insert("SAISEI_SCREENSHOT_SECS".into(), s.to_string());
    }
    if o.replay {
        env.insert("SAISEI_REPLAY".into(), "1".into());
    }

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

pub fn run() {
    let root = resolve_root();
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let (cmd, rest) = argv.split_first().unwrap_or_else(|| {
        eprintln!("usage: saisei <build|run|play|copy-runtime|new-game|triage|replay|state-discover|control|run-with-pty|zbookend-diff|zoom> ...");
        exit(2)
    });
    match cmd.as_str() {
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
            let mut it = rest.iter();
            while let Some(a) = it.next() {
                match a.as_str() {
                    "--program" => program = it.next().cloned(),
                    other if other.starts_with("--") => die(&format!("unknown flag: {other}")),
                    other => game_name = Some(other.to_string()),
                }
            }
            let game = load_game_definition(
                &root,
                &game_name.unwrap_or_else(|| die("build needs a game name")),
                program.as_deref(),
            );
            build(&root, &game);
        }
        "copy-runtime" => {
            let mut program = None;
            let mut game_name = None;
            let mut it = rest.iter();
            while let Some(a) = it.next() {
                match a.as_str() {
                    "--program" => program = it.next().cloned(),
                    other if other.starts_with("--") => die(&format!("unknown flag: {other}")),
                    other => game_name = Some(other.to_string()),
                }
            }
            let game = load_game_definition(
                &root,
                &game_name.unwrap_or_else(|| die("copy-runtime needs a game name")),
                program.as_deref(),
            );
            copy_runtime(&root, &game);
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
        other => die(&format!("unknown command: {other}")),
    }
}
