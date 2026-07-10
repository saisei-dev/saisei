//! `saisei new-game` — bootstrap a game bundle from an archive (URL / .zip / dir).
//! . Downloads via `curl`, unzips via `unzip`
//! (system tools, no the reference). Fetches + extracts, detects executables, picks the
//! entry exe, writes the seed config, and runs one probe build.

use serde_json::{json, Value};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{exit, Command};

fn die(msg: &str) -> ! {
    eprintln!("new-game: {msg}");
    exit(1)
}

const SKIP_EXT: &[&str] = &["txt", "md", "bat", "diz", "nfo"];
const JUNK_NAMES: &[&str] = &["__macosx", ".ds_store", "thumbs.db", ".directory"];

fn is_junk(p: &Path) -> bool {
    let n = p
        .file_name()
        .map(|s| s.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    JUNK_NAMES.contains(&n.as_str()) || n.starts_with("._")
}

fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
    if let Ok(rd) = std::fs::read_dir(dir) {
        for e in rd.flatten() {
            let p = e.path();
            out.push(p.clone());
            if p.is_dir() {
                walk(&p, out);
            }
        }
    }
}

fn prune_junk(root: &Path) {
    let mut all = Vec::new();
    walk(root, &mut all);
    all.sort_by_key(|p| std::cmp::Reverse(p.components().count()));
    for p in all {
        if !is_junk(&p) {
            continue;
        }
        if p.is_dir() {
            std::fs::remove_dir_all(&p).ok();
        } else if p.exists() {
            std::fs::remove_file(&p).ok();
        }
    }
}

fn flatten_wrappers(dest: &Path) {
    loop {
        let entries: Vec<PathBuf> = match std::fs::read_dir(dest) {
            Ok(rd) => rd
                .flatten()
                .map(|e| e.path())
                .filter(|p| !is_junk(p))
                .collect(),
            Err(_) => return,
        };
        if entries.len() != 1 || !entries[0].is_dir() {
            break;
        }
        let sub = &entries[0];
        let tmp = dest.join(format!(
            "{}.__unwrap__",
            sub.file_name().unwrap().to_string_lossy()
        ));
        std::fs::rename(sub, &tmp).ok();
        if let Ok(rd) = std::fs::read_dir(&tmp) {
            for item in rd.flatten() {
                std::fs::rename(item.path(), dest.join(item.file_name())).ok();
            }
        }
        std::fs::remove_dir_all(&tmp).ok();
    }
}

fn copy_dir_into(src: &Path, dest: &Path) {
    for item in std::fs::read_dir(src).into_iter().flatten().flatten() {
        let target = dest.join(item.file_name());
        if item.path().is_dir() {
            std::fs::create_dir_all(&target).ok();
            copy_dir_into(&item.path(), &target);
        } else {
            std::fs::copy(item.path(), &target).ok();
        }
    }
}

fn is_zip(p: &Path) -> bool {
    std::fs::File::open(p)
        .and_then(|mut f| {
            let mut b = [0u8; 4];
            use std::io::Read;
            f.read_exact(&mut b)?;
            Ok(b == *b"PK\x03\x04")
        })
        .unwrap_or(false)
}

fn fetch(src: &str, workdir: &Path) -> PathBuf {
    if src.starts_with("http://") || src.starts_with("https://") {
        let name = src
            .rsplit('/')
            .next()
            .filter(|s| !s.is_empty())
            .unwrap_or("archive.zip");
        let dest = workdir.join(name);
        println!("fetch: downloading {src}");
        let ok = Command::new("curl")
            .args(["-fsSL", src, "-o"])
            .arg(&dest)
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        if !ok {
            die(&format!("download failed: {src}"));
        }
        dest
    } else {
        let p = PathBuf::from(src);
        if !p.exists() {
            die(&format!("not found: {src}"));
        }
        p
    }
}

fn extract_into(archive: &Path, dest: &Path) {
    std::fs::create_dir_all(dest).ok();
    if archive.is_dir() {
        copy_dir_into(archive, dest);
    } else if is_zip(archive) {
        let ok = Command::new("unzip")
            .arg("-q")
            .arg("-o")
            .arg(archive)
            .arg("-d")
            .arg(dest)
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        if !ok {
            die("unzip failed");
        }
    } else {
        std::fs::copy(archive, dest.join(archive.file_name().unwrap())).ok();
    }
    prune_junk(dest);
    flatten_wrappers(dest);
}

pub fn is_executable(p: &Path) -> bool {
    let ext = p
        .extension()
        .map(|s| s.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    if ext == "exe" || ext == "com" {
        return true;
    }
    std::fs::File::open(p)
        .and_then(|mut f| {
            let mut b = [0u8; 2];
            use std::io::Read;
            f.read_exact(&mut b)?;
            Ok(&b == b"MZ")
        })
        .unwrap_or(false)
}

fn choose_entry(execs: &[PathBuf], requested: Option<&str>) -> PathBuf {
    let nm = |p: &Path| p.file_name().unwrap().to_string_lossy().to_string();
    if let Some(req) = requested {
        for e in execs {
            if nm(e).to_lowercase() == req.to_lowercase() {
                return e.clone();
            }
        }
        die(&format!(
            "--exe {req:?} not among executables: {}",
            execs.iter().map(|e| nm(e)).collect::<Vec<_>>().join(", ")
        ));
    }
    if execs.len() == 1 {
        return execs[0].clone();
    }
    if execs.is_empty() {
        die("no executables (.exe/.com/MZ) found in archive");
    }
    use std::io::IsTerminal;
    if !std::io::stdin().is_terminal() {
        die(&format!(
            "multiple executables found; rerun with --exe NAME.\n  candidates: {}",
            execs.iter().map(|e| nm(e)).collect::<Vec<_>>().join(", ")
        ));
    }
    println!("\nWhich executable starts the game?");
    for (i, e) in execs.iter().enumerate() {
        let sz = e.metadata().map(|m| m.len()).unwrap_or(0);
        println!("  {}. {}  ({sz} bytes)", i + 1, nm(e));
    }
    loop {
        print!("Enter number (or exe name): ");
        std::io::stdout().flush().ok();
        let mut line = String::new();
        if std::io::stdin().read_line(&mut line).unwrap_or(0) == 0 {
            die("no input; rerun with --exe NAME to pick non-interactively.");
        }
        let raw = line.trim();
        if let Ok(n) = raw.parse::<usize>() {
            if n >= 1 && n <= execs.len() {
                return execs[n - 1].clone();
            }
        }
        for e in execs {
            if nm(e).to_lowercase() == raw.to_lowercase() {
                return e.clone();
            }
        }
        println!(
            "  (invalid: type 1-{} or one of: {})",
            execs.len(),
            execs.iter().map(|e| nm(e)).collect::<Vec<_>>().join(", ")
        );
    }
}

pub fn build_seed_config(name: &str, exe: &Path, game_dir: &Path) -> Value {
    let mut files = Vec::new();
    walk(game_dir, &mut files);
    files.sort_by_key(|p| p.to_string_lossy().to_lowercase());
    let mut runtime = Vec::new();
    for f in &files {
        if !f.is_file() || is_junk(f) {
            continue;
        }
        let ext = f
            .extension()
            .map(|s| s.to_string_lossy().to_lowercase())
            .unwrap_or_default();
        if SKIP_EXT.contains(&ext.as_str()) {
            continue;
        }
        let rel = f
            .strip_prefix(game_dir)
            .unwrap()
            .to_string_lossy()
            .replace('\\', "/");
        if rel == format!("{name}.json") {
            continue;
        }
        runtime.push(json!({ "source": format!("games/{name}/{rel}"), "dest": rel }));
    }
    json!({
        "name": name,
        "program_path": exe.file_name().unwrap().to_string_lossy(),
        "runtime": runtime,
    })
}

pub fn main(root: &Path, args: &[String]) -> ! {
    let mut archive: Option<String> = None;
    let mut exe: Option<String> = None;
    let mut name_arg: Option<String> = None;
    let mut no_probe = false;
    let mut force = false;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--exe" => exe = it.next().cloned(),
            "--name" => name_arg = it.next().cloned(),
            "--no-probe" => no_probe = true,
            "--force" => force = true,
            other if other.starts_with("--") => die(&format!("unknown flag: {other}")),
            other => archive = Some(other.to_string()),
        }
    }
    let archive = archive.unwrap_or_else(|| die("usage: saisei new-game <url|zip|dir> [--exe NAME] [--name NAME] [--force] [--no-probe]"));

    let stem = Path::new(archive.trim_end_matches('/'))
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let name = crate::sanitize_identifier(&name_arg.unwrap_or(stem));
    let game_dir = root.join("games").join(&name);
    if game_dir.exists() {
        if !force {
            die(&format!(
                "games/{name}/ already exists (use --force to overwrite)"
            ));
        }
        std::fs::remove_dir_all(&game_dir).ok();
    }

    let tmp = std::env::temp_dir().join(format!("saisei_newgame_{name}"));
    std::fs::create_dir_all(&tmp).ok();
    let fetched = fetch(&archive, &tmp);
    println!("extract: -> games/{name}/");
    extract_into(&fetched, &game_dir);
    std::fs::remove_dir_all(&tmp).ok();

    let mut execs = Vec::new();
    walk(&game_dir, &mut execs);
    execs.retain(|p| p.is_file() && is_executable(p));
    execs.sort_by_key(|p| p.file_name().unwrap().to_string_lossy().to_lowercase());
    let exe_path = choose_entry(&execs, exe.as_deref());
    println!(
        "detect: {} executable(s): {}",
        execs.len(),
        execs
            .iter()
            .map(|e| e.file_name().unwrap().to_string_lossy().into_owned())
            .collect::<Vec<_>>()
            .join(", ")
    );

    let config = build_seed_config(&name, &exe_path, &game_dir);
    let config_path = game_dir.join(format!("{name}.json"));
    std::fs::write(
        &config_path,
        format!("{}\n", serde_json::to_string_pretty(&config).unwrap()),
    )
    .ok();
    println!(
        "scaffold: wrote games/{name}/{name}.json\n  program_path  = {}\n  runtime files = {}",
        exe_path.file_name().unwrap().to_string_lossy(),
        config["runtime"].as_array().map(|a| a.len()).unwrap_or(0)
    );

    if no_probe {
        exit(0);
    }
    println!("\nprobe: building bundle '{name}' (emit config + link runtime)...");
    let game = crate::load_game_definition(root, &name, None);
    crate::build(root, &game); // dies on build failure
    println!("\nprobe: OK -- bundle '{name}' builds.\n  next: saisei run {name} --headless   to see it boot.");
    exit(0);
}
