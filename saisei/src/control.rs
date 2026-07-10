//! `saisei control` — drive an emulated game over its stdin control FIFO.
//! . Attaches to the FIFO owned by the running
//! game process and writes the byte protocol the shim reads in safe_point_impl.
//! It does NOT start, stop, build, or restart the game.
//!
//! Commands: tap / press / release / enter / space / shot / raw / halt /
//! resume / step / read / snapshot / status. Globals: --fifo, --shots-dir,
//! --snapshots-dir, --gap. See docs/playing.md for the launch recipe.

use std::collections::{HashMap, HashSet};
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Component, Path, PathBuf};
use std::process::{exit, Command};
use std::time::{Duration, Instant, UNIX_EPOCH};

const USAGE: &str =
    "control: usage: saisei control [--fifo PATH] [--shots-dir PATH] [--snapshots-dir PATH] [--gap MS] <command> [args]";

fn die(msg: &str) -> ! {
    eprintln!("{msg}");
    exit(1)
}

fn parse_int(s: &str, what: &str) -> i64 {
    s.trim().parse::<i64>().unwrap_or_else(|_| {
        die(&format!(
            "control: argument {what}: invalid int value: '{s}'"
        ))
    })
}

// ---------- key / hex / address helpers (pure, unit-tested) ----------

/// Map a friendly key name to a DOS scancode byte for the control protocol
/// (compiler's SCANCODES / ASCII_TO_SCAN table). Bit 7 marks the extended
/// (grey, 0xE0-prefixed) variant: the named arrow keys are the dedicated grey
/// cursor keys on a real keyboard, so they resolve to 0x80|code — games whose
/// INT9 handlers track the E0 prefix (e.g. DM's IBMIO movement dispatch)
/// distinguish them from the keypad codes. Returns the "control: ..." error
/// text on failure.
fn resolve_key(name: &str) -> Result<u8, String> {
    let n = name.to_lowercase();
    match n.as_str() {
        "up" => return Ok(0x48 | 0x80),
        "down" => return Ok(0x50 | 0x80),
        "left" => return Ok(0x4B | 0x80),
        "right" => return Ok(0x4D | 0x80),
        "space" => return Ok(0x39),
        "enter" => return Ok(0x1C),
        "esc" => return Ok(0x01),
        "tab" => return Ok(0x0F),
        "backspace" => return Ok(0x0E),
        _ => {}
    }
    if n.chars().count() == 1 {
        let c = n.chars().next().unwrap();
        if c.is_ascii_lowercase() {
            return Ok(0x1E + (c as u8 - b'a'));
        }
        if c == '0' {
            return Ok(0x0B);
        }
        if ('1'..='9').contains(&c) {
            return Ok(0x02 + (c as u8 - b'1'));
        }
    }
    // Raw hex (0xHH or just HH); the reference parses both with base 16.
    let hex = n.strip_prefix("0x").unwrap_or(&n);
    match i64::from_str_radix(hex, 16) {
        Ok(v) => {
            if (1..=0x7F).contains(&v) {
                Ok(v as u8)
            } else {
                Err(format!("control: scancode 0x{v:X} out of 7-bit range"))
            }
        }
        Err(_) => Err(format!(
            "control: unknown key '{name}'. Use up/down/left/right/space/enter/esc/tab/backspace, a-z, 0-9, or 0xHH."
        )),
    }
}

fn hex_encode(data: &[u8]) -> String {
    let mut s = String::with_capacity(data.len() * 2);
    for b in data {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

/// Faithful to the reference bytes.fromhex on a whitespace-stripped string.
fn hex_decode(s: &str) -> Result<Vec<u8>, ()> {
    let bytes = s.as_bytes();
    if bytes.len() % 2 != 0 {
        return Err(());
    }
    let mut out = Vec::with_capacity(bytes.len() / 2);
    let mut i = 0;
    while i < bytes.len() {
        let hi = (bytes[i] as char).to_digit(16).ok_or(())?;
        let lo = (bytes[i + 1] as char).to_digit(16).ok_or(())?;
        out.push(((hi << 4) | lo) as u8);
        i += 2;
    }
    Ok(out)
}

/// Port of _parse_addr: 0x/0X → hex, else int(s, 0) (auto-base).
fn parse_addr(s: &str) -> i64 {
    let t = s.trim();
    let (sign, rest) = if let Some(r) = t.strip_prefix('-') {
        (-1i64, r)
    } else if let Some(r) = t.strip_prefix('+') {
        (1, r)
    } else {
        (1, t)
    };
    let low = rest.to_ascii_lowercase();
    let parsed = if let Some(h) = low.strip_prefix("0x") {
        i64::from_str_radix(h, 16)
    } else if let Some(o) = low.strip_prefix("0o") {
        i64::from_str_radix(o, 8)
    } else if let Some(b) = low.strip_prefix("0b") {
        i64::from_str_radix(b, 2)
    } else {
        low.parse::<i64>()
    };
    match parsed {
        Ok(v) => sign * v,
        Err(_) => die(&format!("control: bad address '{s}'")),
    }
}

/// Render a float as "3.0" not "3" (3.0, not 3).
fn fmt_float(x: f64) -> String {
    if x.is_finite() && x == x.trunc() {
        format!("{x:.1}")
    } else {
        format!("{x}")
    }
}

// ---------- path resolution (Path.resolve(strict=False)) ----------

fn lexical_abs(p: &Path) -> PathBuf {
    let mut out = if p.is_absolute() {
        PathBuf::new()
    } else {
        std::env::current_dir().unwrap_or_default()
    };
    for comp in p.components() {
        match comp {
            Component::Prefix(_) | Component::RootDir => out.push(comp.as_os_str()),
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            Component::Normal(s) => out.push(s),
        }
    }
    out
}

/// Mirror the reference Path.resolve(strict=False): make absolute, then resolve
/// symlinks over the longest existing prefix.
fn resolve(p: &Path) -> PathBuf {
    let abs = lexical_abs(p);
    let mut anc = abs.clone();
    let mut tail: Vec<std::ffi::OsString> = Vec::new();
    loop {
        if anc.exists() {
            if let Ok(mut c) = std::fs::canonicalize(&anc) {
                for t in tail.iter().rev() {
                    c.push(t);
                }
                return c;
            }
            break;
        }
        match anc.file_name() {
            Some(f) => tail.push(f.to_os_string()),
            None => break,
        }
        if !anc.pop() {
            break;
        }
    }
    abs
}

// ---------- FIFO I/O ----------

fn open_fifo(path: &Path) -> File {
    if !path.exists() {
        die(&format!(
            "control: FIFO {} doesn't exist. The game must be running with stdin redirected from this FIFO — see docs/playing.md.",
            path.display()
        ));
    }
    OpenOptions::new()
        .write(true)
        .open(path)
        .unwrap_or_else(|e| {
            die(&format!(
                "control: cannot open {} for write: {e}",
                path.display()
            ))
        })
}

fn send_bytes(path: &Path, payload: &[u8]) {
    let mut f = open_fifo(path);
    f.write_all(payload)
        .unwrap_or_else(|e| die(&format!("control: write to {} failed: {e}", path.display())));
}

fn sleep_f64(secs: f64) {
    if secs > 0.0 {
        std::thread::sleep(Duration::from_secs_f64(secs));
    }
}

fn mtime(p: &Path) -> Option<f64> {
    p.metadata()
        .ok()?
        .modified()
        .ok()?
        .duration_since(UNIX_EPOCH)
        .ok()
        .map(|d| d.as_secs_f64())
}

fn glob_files(dir: &Path, prefix: &str, suffix: &str) -> Vec<PathBuf> {
    let mut v = Vec::new();
    if let Ok(rd) = std::fs::read_dir(dir) {
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            if name.starts_with(prefix) && name.ends_with(suffix) {
                let p = e.path();
                if p.is_file() {
                    v.push(p);
                }
            }
        }
    }
    v
}

// ---------- directory resolution ----------

fn resolve_shots_dir(root: &Path, fifo_arg: &str, override_dir: Option<&str>) -> PathBuf {
    if let Some(o) = override_dir {
        return resolve(Path::new(o));
    }
    let fname = Path::new(fifo_arg)
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let name = fname.strip_suffix("_fifo").unwrap_or(&fname);
    let name = if name.is_empty() { "game" } else { name };
    resolve(&root.join("build").join(name).join("screenshots"))
}

fn resolve_snapshots_dir(root: &Path, fifo_arg: &str, override_dir: Option<&str>) -> PathBuf {
    if let Some(o) = override_dir {
        return resolve(Path::new(o));
    }
    let shots = resolve_shots_dir(root, fifo_arg, None);
    let parent = shots.parent().unwrap_or_else(|| Path::new("/"));
    resolve(&parent.join("snapshots"))
}

// ---------- sub-argument parsing ----------

/// Positional-only commands (tap/press/release/enter/space/step) — reject
/// any `--flag`. A leading single '-' (negative number) stays a positional.
fn positionals(sub: &[String]) -> Vec<String> {
    let mut v = Vec::new();
    for a in sub {
        if a.starts_with("--") {
            die(&format!("control: unrecognized argument: {a}"));
        }
        v.push(a.clone());
    }
    v
}

/// Parse `--out`/`--timeout` plus optional positionals (shot/read/snapshot).
fn parse_out_timeout(
    sub: &[String],
    default_timeout: f64,
    allow_positional: bool,
) -> (Vec<String>, Option<String>, f64) {
    let mut out = None;
    let mut timeout = default_timeout;
    let mut pos = Vec::new();
    let mut i = 0;
    while i < sub.len() {
        let a = &sub[i];
        if a == "--out" {
            i += 1;
            out = Some(
                sub.get(i)
                    .cloned()
                    .unwrap_or_else(|| die("control: argument --out: expected one argument")),
            );
            i += 1;
        } else if let Some(v) = a.strip_prefix("--out=") {
            out = Some(v.to_string());
            i += 1;
        } else if a == "--timeout" {
            i += 1;
            let v = sub
                .get(i)
                .cloned()
                .unwrap_or_else(|| die("control: argument --timeout: expected one argument"));
            timeout = v.trim().parse().unwrap_or_else(|_| {
                die(&format!(
                    "control: argument --timeout: invalid float value: '{v}'"
                ))
            });
            i += 1;
        } else if let Some(v) = a.strip_prefix("--timeout=") {
            timeout = v.trim().parse().unwrap_or_else(|_| {
                die(&format!(
                    "control: argument --timeout: invalid float value: '{v}'"
                ))
            });
            i += 1;
        } else if a.starts_with("--") {
            die(&format!("control: unrecognized argument: {a}"));
        } else {
            if !allow_positional {
                die(&format!("control: unrecognized argument: {a}"));
            }
            pos.push(a.clone());
            i += 1;
        }
    }
    (pos, out, timeout)
}

fn copy_out(new_path: &Path, out: &str) {
    let dst = resolve(Path::new(out));
    if let Some(p) = dst.parent() {
        std::fs::create_dir_all(p).ok();
    }
    std::fs::copy(new_path, &dst)
        .unwrap_or_else(|e| die(&format!("control: copy to {} failed: {e}", dst.display())));
    println!("{}", dst.display());
}

// ---------- entry point ----------

pub fn main(root: &Path, args: &[String]) -> ! {
    let mut fifo_arg = String::from("/tmp/saisei_fifo");
    let mut shots_dir: Option<String> = None;
    let mut snapshots_dir: Option<String> = None;
    let mut gap: i64 = 80;

    // Parse global flags up to the subcommand positional (argparse order).
    let mut idx = 0usize;
    let command: String;
    loop {
        let a = match args.get(idx) {
            Some(a) => a.as_str(),
            None => die(USAGE),
        };
        if a == "--fifo" {
            fifo_arg = next_val(args, &mut idx, "--fifo");
        } else if let Some(v) = a.strip_prefix("--fifo=") {
            fifo_arg = v.to_string();
            idx += 1;
        } else if a == "--shots-dir" {
            shots_dir = Some(next_val(args, &mut idx, "--shots-dir"));
        } else if let Some(v) = a.strip_prefix("--shots-dir=") {
            shots_dir = Some(v.to_string());
            idx += 1;
        } else if a == "--snapshots-dir" {
            snapshots_dir = Some(next_val(args, &mut idx, "--snapshots-dir"));
        } else if let Some(v) = a.strip_prefix("--snapshots-dir=") {
            snapshots_dir = Some(v.to_string());
            idx += 1;
        } else if a == "--gap" {
            gap = parse_int(&next_val(args, &mut idx, "--gap"), "--gap");
        } else if let Some(v) = a.strip_prefix("--gap=") {
            gap = parse_int(v, "--gap");
            idx += 1;
        } else if a.starts_with('-') {
            die(&format!("control: unrecognized argument: {a}"));
        } else {
            command = a.to_string();
            idx += 1;
            break;
        }
    }
    let sub = &args[idx..];
    let fifo = PathBuf::from(&fifo_arg);

    match command.as_str() {
        "tap" => {
            let pos = positionals(sub);
            if pos.is_empty() {
                die("control: tap requires a key");
            }
            if pos.len() > 2 {
                die("control: tap takes at most a key and ticks");
            }
            let sc = resolve_key(&pos[0]).unwrap_or_else(|e| die(&e));
            let ticks_raw = pos.get(1).map(|s| parse_int(s, "ticks")).unwrap_or(10);
            let ticks = if ticks_raw > 0 { ticks_raw } else { 1 };
            if ticks > 0xFFFF {
                die("control: ticks must fit in 16 bits");
            }
            send_bytes(
                &fifo,
                &[0x12, sc, (ticks & 0xFF) as u8, ((ticks >> 8) & 0xFF) as u8],
            );
        }
        "press" => {
            let pos = positionals(sub);
            if pos.len() != 1 {
                die("control: press requires exactly a key");
            }
            let sc = resolve_key(&pos[0]).unwrap_or_else(|e| die(&e));
            send_bytes(&fifo, &[0x10, sc]);
        }
        "release" => {
            let pos = positionals(sub);
            if pos.len() != 1 {
                die("control: release requires exactly a key");
            }
            let sc = resolve_key(&pos[0]).unwrap_or_else(|e| die(&e));
            send_bytes(&fifo, &[0x11, sc]);
        }
        "enter" => {
            let pos = positionals(sub);
            if pos.len() > 1 {
                die("control: enter takes at most a count");
            }
            let count = pos.first().map(|s| parse_int(s, "count")).unwrap_or(1);
            let secs = gap as f64 / 1000.0;
            let mut f = open_fifo(&fifo);
            let mut i = 0i64;
            while i < count {
                f.write_all(b"\r")
                    .unwrap_or_else(|e| die(&format!("control: write failed: {e}")));
                if i + 1 < count {
                    sleep_f64(secs);
                }
                i += 1;
            }
        }
        "space" => {
            let pos = positionals(sub);
            if pos.len() > 1 {
                die("control: space takes at most a count");
            }
            let count = pos.first().map(|s| parse_int(s, "count")).unwrap_or(1);
            let secs = gap as f64 / 1000.0;
            let mut f = open_fifo(&fifo);
            let mut i = 0i64;
            while i < count {
                f.write_all(&[0x12, 0x39, 0x03, 0x00])
                    .unwrap_or_else(|e| die(&format!("control: write failed: {e}")));
                if i + 1 < count {
                    sleep_f64(secs);
                }
                i += 1;
            }
        }
        "shot" => {
            let (_pos, out, timeout) = parse_out_timeout(sub, 3.0, false);
            let shots = resolve_shots_dir(root, &fifo_arg, shots_dir.as_deref());
            std::fs::create_dir_all(&shots).ok();
            let mut before: HashMap<String, f64> = HashMap::new();
            for p in glob_files(&shots, "screenshot", ".png") {
                if let Some(m) = mtime(&p) {
                    before.insert(p.file_name().unwrap().to_string_lossy().into_owned(), m);
                }
            }
            send_bytes(&fifo, &[0x14]);

            let start = Instant::now();
            let mut new_path: Option<PathBuf> = None;
            while start.elapsed().as_secs_f64() < timeout {
                for p in glob_files(&shots, "screenshot", ".png") {
                    let name = p.file_name().unwrap().to_string_lossy().into_owned();
                    let m = match mtime(&p) {
                        Some(m) => m,
                        None => continue,
                    };
                    let fresh = match before.get(&name) {
                        None => true,
                        Some(prev) => m > prev + 0.001,
                    };
                    if fresh {
                        new_path = Some(p);
                        break;
                    }
                }
                if new_path.is_some() {
                    break;
                }
                sleep_f64(0.05);
            }
            let new_path = new_path.unwrap_or_else(|| {
                die(&format!(
                    "control: no screenshot appeared in {} within {}s. The game may not be processing input right now (paused on a SAFEPOINT-less code path, or stdin gating is off).",
                    shots.display(),
                    fmt_float(timeout)
                ))
            });
            match out {
                Some(o) => copy_out(&new_path, &o),
                None => println!("{}", resolve(&new_path).display()),
            }
        }
        "raw" => {
            if sub.is_empty() {
                die("control: raw requires hex bytes");
            }
            let joined: String = sub.concat();
            let cleaned = joined.replace(' ', "").replace("0x", "");
            let data = hex_decode(&cleaned)
                .unwrap_or_else(|_| die(&format!("control: bad hex '{cleaned}'")));
            send_bytes(&fifo, &data);
        }
        "halt" => {
            if !sub.is_empty() {
                die(&format!("control: unrecognized argument: {}", sub[0]));
            }
            send_bytes(&fifo, &[0x15]);
        }
        "resume" => {
            if !sub.is_empty() {
                die(&format!("control: unrecognized argument: {}", sub[0]));
            }
            send_bytes(&fifo, &[0x16]);
        }
        "step" => {
            let pos = positionals(sub);
            if pos.len() > 1 {
                die("control: step takes at most a tick count");
            }
            let ticks_raw = pos.first().map(|s| parse_int(s, "ticks")).unwrap_or(1);
            let ticks = if ticks_raw > 0 { ticks_raw } else { 1 };
            if ticks > 0xFFFF {
                die("control: ticks must fit in 16 bits");
            }
            send_bytes(
                &fifo,
                &[0x17, (ticks & 0xFF) as u8, ((ticks >> 8) & 0xFF) as u8],
            );
        }
        "read" => {
            let (pos, out, timeout) = parse_out_timeout(sub, 3.0, true);
            if pos.is_empty() {
                die("control: read requires an address");
            }
            if pos.len() > 2 {
                die("control: read takes at most addr and len");
            }
            let addr = parse_addr(&pos[0]);
            let length = pos.get(1).map(|s| parse_int(s, "len")).unwrap_or(1);
            if !(1..=255).contains(&length) {
                die("control: read len must be 1..255");
            }
            if !(0..=0xFFFF_FFFFi64).contains(&addr) || addr + length > (1 << 21) {
                die("control: addr out of bounds (RAM is 0..0x1FFFFF)");
            }
            let snaps = resolve_snapshots_dir(root, &fifo_arg, snapshots_dir.as_deref());
            std::fs::create_dir_all(&snaps).ok();
            let target = snaps.join("last_read.bin");
            let before_m = if target.exists() {
                mtime(&target)
            } else {
                None
            };

            send_bytes(
                &fifo,
                &[
                    0x18,
                    (addr & 0xFF) as u8,
                    ((addr >> 8) & 0xFF) as u8,
                    ((addr >> 16) & 0xFF) as u8,
                    ((addr >> 24) & 0xFF) as u8,
                    length as u8,
                ],
            );

            let start = Instant::now();
            let mut ready = false;
            while start.elapsed().as_secs_f64() < timeout {
                if target.exists() {
                    if let Some(m) = mtime(&target) {
                        if before_m.is_none() || m > before_m.unwrap() + 0.001 {
                            ready = true;
                            break;
                        }
                    }
                }
                sleep_f64(0.02);
            }
            if !ready {
                die(&format!(
                    "control: read did not produce {} within {}s. The game may not be processing input (try `halt` first).",
                    target.display(),
                    fmt_float(timeout)
                ));
            }
            let data = std::fs::read(&target).unwrap_or_else(|e| {
                die(&format!("control: cannot read {}: {e}", target.display()))
            });
            match out {
                Some(o) => {
                    let dst = resolve(Path::new(&o));
                    if let Some(p) = dst.parent() {
                        std::fs::create_dir_all(p).ok();
                    }
                    std::fs::write(&dst, &data).unwrap_or_else(|e| {
                        die(&format!("control: write to {} failed: {e}", dst.display()))
                    });
                    println!("{}", dst.display());
                }
                None => println!(
                    "{} ({} bytes: {})",
                    target.display(),
                    data.len(),
                    hex_encode(&data)
                ),
            }
        }
        "snapshot" => {
            let (_pos, out, timeout) = parse_out_timeout(sub, 5.0, false);
            let snaps = resolve_snapshots_dir(root, &fifo_arg, snapshots_dir.as_deref());
            std::fs::create_dir_all(&snaps).ok();
            let before: HashSet<String> = glob_files(&snaps, "snap_", ".bin")
                .iter()
                .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
                .collect();
            send_bytes(&fifo, &[0x1A]);

            let start = Instant::now();
            let mut new_path: Option<PathBuf> = None;
            while start.elapsed().as_secs_f64() < timeout {
                for p in glob_files(&snaps, "snap_", ".bin") {
                    let name = p.file_name().unwrap().to_string_lossy().into_owned();
                    let sz = p.metadata().map(|m| m.len()).unwrap_or(0);
                    if !before.contains(&name) && sz > 0 {
                        new_path = Some(p);
                        break;
                    }
                }
                if new_path.is_some() {
                    break;
                }
                sleep_f64(0.02);
            }
            let new_path = new_path.unwrap_or_else(|| {
                die(&format!(
                    "control: no new snapshot appeared in {} within {}s. The game may not be processing input (try `halt` first).",
                    snaps.display(),
                    fmt_float(timeout)
                ))
            });
            match out {
                Some(o) => copy_out(&new_path, &o),
                None => println!("{}", resolve(&new_path).display()),
            }
        }
        "status" => {
            if !sub.is_empty() {
                die(&format!("control: unrecognized argument: {}", sub[0]));
            }
            println!("fifo: {fifo_arg}");
            let exists = fifo.exists();
            println!("exists: {}", if exists { "True" } else { "False" });
            if !exists {
                exit(0);
            }
            // Best-effort: who has the FIFO open (fuser may not be installed).
            let mut found_reader = false;
            if let Ok(o) = Command::new("fuser").arg(&fifo_arg).output() {
                let s = String::from_utf8_lossy(&o.stdout);
                let s = s.trim();
                if !s.is_empty() {
                    println!("holders: {s}");
                    found_reader = true;
                }
            }
            if !found_reader {
                println!("holders: (run `lsof {fifo_arg}` to check)");
            }
            let shots = resolve_shots_dir(root, &fifo_arg, shots_dir.as_deref());
            if shots.is_dir() {
                let mut files: Vec<(PathBuf, f64)> = glob_files(&shots, "screenshot", ".png")
                    .into_iter()
                    .filter_map(|p| mtime(&p).map(|m| (p, m)))
                    .collect();
                if !files.is_empty() {
                    files
                        .sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
                    println!("latest shot: {}", files[0].0.display());
                }
            }
        }
        other => die(&format!("control: unknown command: {other}")),
    }
    exit(0);
}

fn next_val(args: &[String], idx: &mut usize, flag: &str) -> String {
    *idx += 1;
    let v = args
        .get(*idx)
        .cloned()
        .unwrap_or_else(|| die(&format!("control: argument {flag}: expected one argument")));
    *idx += 1;
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_named() {
        // Arrow names are the grey cursor keys: bit 7 = extended (E0-prefixed).
        assert_eq!(resolve_key("up"), Ok(0xC8));
        assert_eq!(resolve_key("DOWN"), Ok(0xD0));
        assert_eq!(resolve_key("left"), Ok(0xCB));
        assert_eq!(resolve_key("right"), Ok(0xCD));
        assert_eq!(resolve_key("space"), Ok(0x39));
        assert_eq!(resolve_key("enter"), Ok(0x1C));
        assert_eq!(resolve_key("esc"), Ok(0x01));
        assert_eq!(resolve_key("tab"), Ok(0x0F));
        assert_eq!(resolve_key("backspace"), Ok(0x0E));
    }

    #[test]
    fn keys_ascii_and_hex() {
        assert_eq!(resolve_key("a"), Ok(0x1E));
        assert_eq!(resolve_key("z"), Ok(0x37));
        assert_eq!(resolve_key("0"), Ok(0x0B));
        assert_eq!(resolve_key("1"), Ok(0x02));
        assert_eq!(resolve_key("9"), Ok(0x0A));
        assert_eq!(resolve_key("0x4d"), Ok(0x4D));
        assert_eq!(resolve_key("4d"), Ok(0x4D));
        assert!(resolve_key("zz").is_err());
        assert!(resolve_key("0x00").is_err()); // 0 is outside 1..=0x7F
        assert!(resolve_key("0x80").is_err()); // outside 7-bit range
    }

    #[test]
    fn hex_roundtrip() {
        assert_eq!(hex_encode(&[0xde, 0xad, 0xbe, 0xef]), "deadbeef");
        assert_eq!(hex_decode("104d").unwrap(), vec![0x10, 0x4d]);
        assert_eq!(hex_decode("").unwrap(), Vec::<u8>::new());
        assert!(hex_decode("abc").is_err()); // odd length
        assert!(hex_decode("zz").is_err()); // non-hex
    }

    #[test]
    fn addresses() {
        assert_eq!(parse_addr("0x10"), 16);
        assert_eq!(parse_addr("16"), 16);
        assert_eq!(parse_addr(" 0X20 "), 32);
        assert_eq!(parse_addr("0o17"), 15);
    }

    #[test]
    fn tap_payload_shape() {
        // ticks=300 -> 0x012C little-endian; 'up' = grey Up = 0x48 with the
        // extended bit (0x80) set so the shim emits the E0-prefixed make/break.
        let sc = resolve_key("up").unwrap();
        let ticks: i64 = 300;
        let payload = [sc, (ticks & 0xFF) as u8, ((ticks >> 8) & 0xFF) as u8];
        assert_eq!(payload, [0xC8, 0x2C, 0x01]);
    }

    #[test]
    fn float_str_format() {
        assert_eq!(fmt_float(3.0), "3.0");
        assert_eq!(fmt_float(5.0), "5.0");
        assert_eq!(fmt_float(3.5), "3.5");
    }
}
