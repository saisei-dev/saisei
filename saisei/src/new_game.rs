//! `saisei new-game` — bootstrap a game bundle from an archive (URL / .zip / dir).
//! Downloading and unzipping happen *in-process* (ureq + the zip crate), so a
//! fresh clone needs no curl and no unzip on PATH — `cargo build` is the whole
//! install. Fetches + extracts, detects executables, picks the entry exe, writes
//! the seed config, and runs one probe build.

use serde_json::{json, Value};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::exit;

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

/// Download `src` into `workdir`. HTTP(S) is served in-process by ureq —
/// redirects followed, TLS via rustls, so there is no curl, no OpenSSL, and no
/// system CA bundle in the picture.
fn download(src: &str, workdir: &Path) -> Result<PathBuf, String> {
    // Name the download after the URL's last path segment, minus any ?query#frag.
    let name = src
        .rsplit('/')
        .next()
        .map(|s| s.split(['?', '#']).next().unwrap_or(s))
        .filter(|s| !s.is_empty())
        .unwrap_or("archive.zip");
    let dest = workdir.join(name);

    let resp = ureq::get(src)
        .call()
        .map_err(|e| format!("Could not download {src}: {e}"))?;
    // No read limit: game archives run to tens of MB and ureq's default body cap
    // would silently truncate them.
    let mut body = resp.into_body().into_with_config().limit(u64::MAX).reader();
    let mut out =
        std::fs::File::create(&dest).map_err(|e| format!("create {}: {e}", dest.display()))?;
    std::io::copy(&mut body, &mut out).map_err(|e| format!("Could not download {src}: {e}"))?;
    Ok(dest)
}

/// Unpack `archive` into `dest`. A zip is extracted in-process (no unzip on
/// PATH); a directory or a bare game executable is copied. Anything else is
/// refused by name — better a one-line reason than a bundle that silently holds
/// an unextracted .7z.
fn extract_into(archive: &Path, dest: &Path) {
    std::fs::create_dir_all(dest).ok();
    if archive.is_dir() {
        copy_dir_into(archive, dest);
    } else if is_zip(archive) {
        extract_zip(archive, dest);
    } else if is_executable(archive) {
        std::fs::copy(archive, dest.join(archive.file_name().unwrap()))
            .unwrap_or_else(|e| die(&format!("copy {}: {e}", archive.display())));
    } else {
        die(&format!(
            "{}: not a zip.\n  \
             Only .zip archives are supported for now. Unpack it yourself and \
             point new-game at the folder:\n  saisei new-game <folder>",
            archive.display()
        ));
    }
    prune_junk(dest);
    flatten_wrappers(dest);
}

/// Extract a zip with the `zip` crate: stored + deflate, which is what every
/// archive site serves. A 1990s PKZIP-imploded file would land here as an
/// unsupported-method error naming the entry, not as a corrupt bundle.
fn extract_zip(archive: &Path, dest: &Path) {
    let f = std::fs::File::open(archive)
        .unwrap_or_else(|e| die(&format!("open {}: {e}", archive.display())));
    let mut zip = zip::ZipArchive::new(f)
        .unwrap_or_else(|e| die(&format!("{}: not a readable zip: {e}", archive.display())));

    for i in 0..zip.len() {
        let mut entry = zip.by_index(i).unwrap_or_else(|e| {
            die(&format!(
                "{}: unreadable zip entry #{i}: {e}\n  \
                 (only stored and deflate compression are supported)",
                archive.display()
            ))
        });
        // enclosed_name() rejects absolute paths and `..` traversal, so a hostile
        // or just-malformed zip can never write outside games/<name>/.
        let Some(rel) = entry.enclosed_name() else {
            eprintln!("new-game: skipping unsafe path in zip: {}", entry.name());
            continue;
        };
        let out = dest.join(rel);
        if entry.is_dir() {
            std::fs::create_dir_all(&out).ok();
            continue;
        }
        if let Some(parent) = out.parent() {
            std::fs::create_dir_all(parent).ok();
        }
        let mut w = std::fs::File::create(&out)
            .unwrap_or_else(|e| die(&format!("create {}: {e}", out.display())));
        std::io::copy(&mut entry, &mut w).unwrap_or_else(|e| {
            die(&format!(
                "{}: extracting {}: {e}\n  \
                 (only stored and deflate compression are supported)",
                archive.display(),
                entry.name()
            ))
        });
    }
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

/// A bundle unpacked into `games/<name>/`, with the executables found in it.
///
/// The only question left is which of them starts the game — which the CLI asks
/// on a tty and the player app asks with a list of tiles.
#[derive(Debug)]
pub struct Staged {
    pub name: String,
    pub game_dir: PathBuf,
    pub execs: Vec<PathBuf>,
}

impl Staged {
    /// The executables by bare file name, for showing to a person.
    pub fn exe_names(&self) -> Vec<String> {
        self.execs
            .iter()
            .map(|e| e.file_name().unwrap().to_string_lossy().into_owned())
            .collect()
    }
}

/// Fetch `src` (a URL, a zip, or a folder) and unpack it into `games/<name>/`.
///
/// Returns errors rather than exiting: the CLI can print them and stop, but the
/// player app is a window and has to be able to say "that didn't work" and carry
/// on.
pub fn stage(
    root: &Path,
    src: &str,
    name_hint: Option<&str>,
    force: bool,
) -> Result<Staged, String> {
    let stem = Path::new(src.trim_end_matches('/'))
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let name = crate::sanitize_identifier(&name_hint.map(str::to_string).unwrap_or(stem));
    if name.is_empty() {
        return Err("could not work out a name for this game".into());
    }
    let game_dir = root.join("games").join(&name);
    if game_dir.exists() {
        if !force {
            return Err(format!("You already have a game called '{name}'."));
        }
        std::fs::remove_dir_all(&game_dir).ok();
    }

    // Refuse what we cannot unpack *before* unpacking, so the extract path has
    // nothing left to fail on.
    let is_url = src.starts_with("http://") || src.starts_with("https://");
    if !is_url {
        let p = Path::new(src);
        if !p.exists() {
            return Err(format!("not found: {src}"));
        }
        if !p.is_dir() && !is_zip(p) && !is_executable(p) {
            return Err("Only .zip archives and folders can be added.".into());
        }
    }

    let tmp = std::env::temp_dir().join(format!("saisei_newgame_{name}"));
    std::fs::create_dir_all(&tmp).ok();
    let fetched = if is_url {
        download(src, &tmp)?
    } else {
        PathBuf::from(src)
    };
    extract_into(&fetched, &game_dir);
    std::fs::remove_dir_all(&tmp).ok();

    let mut execs = Vec::new();
    walk(&game_dir, &mut execs);
    execs.retain(|p| p.is_file() && is_executable(p));
    execs.sort_by_key(|p| p.file_name().unwrap().to_string_lossy().to_lowercase());
    if execs.is_empty() {
        std::fs::remove_dir_all(&game_dir).ok();
        return Err("No DOS program (.exe/.com) in there.".into());
    }

    Ok(Staged {
        name,
        game_dir,
        execs,
    })
}

/// Write the bundle's config, naming `exe` as the program to run.
pub fn finish(staged: &Staged, exe: &Path) -> Result<PathBuf, String> {
    let config = build_seed_config(&staged.name, exe, &staged.game_dir);
    let config_path = staged.game_dir.join(format!("{}.json", staged.name));
    std::fs::write(
        &config_path,
        format!("{}\n", serde_json::to_string_pretty(&config).unwrap()),
    )
    .map_err(|e| format!("write {}: {e}", config_path.display()))?;
    Ok(config_path)
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

    let staged = stage(root, &archive, name_arg.as_deref(), force).unwrap_or_else(|e| die(&e));
    let name = staged.name.clone();
    println!("extract: -> games/{name}/");
    let exe_path = choose_entry(&staged.execs, exe.as_deref());
    println!(
        "detect: {} executable(s): {}",
        staged.execs.len(),
        staged.exe_names().join(", ")
    );

    finish(&staged, &exe_path).unwrap_or_else(|e| die(&e));
    println!(
        "scaffold: wrote games/{name}/{name}.json\n  program_path  = {}",
        exe_path.file_name().unwrap().to_string_lossy()
    );

    if no_probe {
        exit(0);
    }
    println!("\nprobe: building bundle '{name}' (emit config + link runtime)...");
    let game = crate::load_game_definition(root, &name, None);
    crate::build(root, &game); // dies on build failure
    println!("\nprobe: OK -- bundle '{name}' builds.\n  next: saisei-cli run {name} --headless   to see it boot.");
    exit(0);
}
