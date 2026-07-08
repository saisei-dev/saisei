//! `saisei state-discover` — find game-state addresses by snapshot diffing.
//! . Reads full 2 MB RAM snapshots
//! (dumped by `saisei control snapshot`) and diffs them under several filters
//! to locate the linear addresses that back a piece of game state.
//!
//! Subcommands:
//! diff S0 S1                 byte-level diff of two snapshots
//! changed S0 S1 [S2 ...]     addresses that differ across every adjacent pair
//! reverted S0 S1 [S2 ...]    changed in S1 then returned to S0 in some later Sn
//! constant S0 S1 [S2 ...]    addresses identical across all snapshots
//! monotonic S0 S1 S2 [...]   value strictly increases/decreases at every step

use std::path::Path;
use std::process::exit;

const RAM_SIZE: usize = 1 << 21; // 2 MB, matches SHIM_MEMORY_SIZE

/// the reference SystemExit(msg): message to stderr, exit code 1.
fn die(msg: &str) -> ! {
    eprintln!("{msg}");
    exit(1)
}

fn usage_err(msg: &str) -> ! {
    eprintln!("usage: saisei state-discover [--max N] [--width {{1,2,4}}] {{diff,changed,reverted,constant,monotonic}} ...");
    eprintln!("state-discover: error: {msg}");
    exit(2)
}

fn load(path: &str) -> Vec<u8> {
    let data = std::fs::read(path)
        .unwrap_or_else(|e| die(&format!("state_discover: cannot read {path}: {e}")));
    if data.len() != RAM_SIZE {
        die(&format!(
            "state_discover: {path} is {} bytes, expected {RAM_SIZE}",
            data.len()
        ));
    }
    data
}

fn format_value(snap: &[u8], addr: usize, width: usize) -> String {
    match width {
        1 => format!("{:02X}", snap[addr]),
        2 => {
            let v = snap[addr] as u32 | (snap[addr + 1] as u32) << 8;
            format!("{v:04X}")
        }
        4 => {
            let v = snap[addr] as u32
                | (snap[addr + 1] as u32) << 8
                | (snap[addr + 2] as u32) << 16
                | (snap[addr + 3] as u32) << 24;
            format!("{v:08X}")
        }
        _ => unreachable!(),
    }
}

fn value_at(snap: &[u8], addr: usize, width: usize) -> u64 {
    let mut v: u64 = 0;
    for i in 0..width {
        v |= (snap[addr + i] as u64) << (8 * i);
    }
    v
}

fn emit(candidates: &[usize], snaps: &[&[u8]], width: usize, limit: usize) {
    let mut out = String::new();
    if candidates.is_empty() {
        out.push_str("(no candidates)\n");
        print_out(&out);
        return;
    }
    let shown = &candidates[..candidates.len().min(limit)];
    for &addr in shown {
        let seg = (addr >> 4) & 0xFFFF;
        let off = addr & 0xF;
        let vals: Vec<String> = snaps.iter().map(|s| format_value(s, addr, width)).collect();
        out.push_str(&format!(
            "0x{addr:06X}  {seg:04X}:{off:04X}  {}\n",
            vals.join(" \u{2192} ")
        ));
    }
    if candidates.len() > limit {
        out.push_str(&format!(
            "... {} more (use --max to widen)\n",
            candidates.len() - limit
        ));
    }
    out.push_str(&format!(
        "# {} total candidate{}\n",
        candidates.len(),
        if candidates.len() != 1 { "s" } else { "" }
    ));
    print_out(&out);
}

/// Write to stdout; on a broken pipe (`head`, closed pipe) exit quietly, like
/// the reference's BrokenPipeError guard.
fn print_out(s: &str) {
    use std::io::Write;
    let stdout = std::io::stdout();
    let mut h = stdout.lock();
    if h.write_all(s.as_bytes()).and_then(|_| h.flush()).is_err() {
        exit(0);
    }
}

fn addresses(width: usize) -> impl Iterator<Item = usize> {
    (0..RAM_SIZE).step_by(width)
}

fn cmd_diff(snap_a: &str, snap_b: &str, width: usize, max: usize) {
    let a = load(snap_a);
    let b = load(snap_b);
    let mut out: Vec<usize> = Vec::new();
    for addr in addresses(width) {
        if a[addr..addr + width] != b[addr..addr + width] {
            out.push(addr);
        }
    }
    emit(&out, &[&a, &b], width, max);
}

fn cmd_changed(paths: &[String], width: usize, max: usize) {
    let snaps: Vec<Vec<u8>> = paths.iter().map(|p| load(p)).collect();
    if snaps.len() < 2 {
        die("changed needs >=2 snapshots");
    }
    let mut out: Vec<usize> = Vec::new();
    for addr in addresses(width) {
        if (0..snaps.len() - 1)
            .all(|i| snaps[i][addr..addr + width] != snaps[i + 1][addr..addr + width])
        {
            out.push(addr);
        }
    }
    let refs: Vec<&[u8]> = snaps.iter().map(|s| s.as_slice()).collect();
    emit(&out, &refs, width, max);
}

fn cmd_reverted(paths: &[String], width: usize, max: usize) {
    let snaps: Vec<Vec<u8>> = paths.iter().map(|p| load(p)).collect();
    if snaps.len() < 3 {
        die("reverted needs >=3 snapshots (baseline, after, back)");
    }
    let baseline = &snaps[0];
    let after = &snaps[1];
    let laters = &snaps[2..];
    let mut out: Vec<usize> = Vec::new();
    for addr in addresses(width) {
        let b = &baseline[addr..addr + width];
        let a = &after[addr..addr + width];
        if a == b {
            continue;
        }
        if laters.iter().any(|later| &later[addr..addr + width] == b) {
            out.push(addr);
        }
    }
    let refs: Vec<&[u8]> = snaps.iter().map(|s| s.as_slice()).collect();
    emit(&out, &refs, width, max);
}

fn cmd_constant(paths: &[String], width: usize, max: usize) {
    let snaps: Vec<Vec<u8>> = paths.iter().map(|p| load(p)).collect();
    if snaps.len() < 2 {
        die("constant needs >=2 snapshots");
    }
    let first = &snaps[0];
    let mut out: Vec<usize> = Vec::new();
    for addr in addresses(width) {
        let chunk = &first[addr..addr + width];
        if snaps[1..].iter().all(|s| &s[addr..addr + width] == chunk) {
            out.push(addr);
        }
    }
    let refs: Vec<&[u8]> = snaps.iter().map(|s| s.as_slice()).collect();
    emit(&out, &refs, width, max);
}

fn cmd_monotonic(paths: &[String], width: usize, max: usize) {
    let snaps: Vec<Vec<u8>> = paths.iter().map(|p| load(p)).collect();
    if snaps.len() < 3 {
        die("monotonic needs >=3 snapshots");
    }
    let mut out: Vec<usize> = Vec::new();
    for addr in addresses(width) {
        let vals: Vec<u64> = snaps.iter().map(|s| value_at(s, addr, width)).collect();
        let strictly_inc = (0..vals.len() - 1).all(|i| vals[i] < vals[i + 1]);
        if strictly_inc {
            out.push(addr);
            continue;
        }
        let strictly_dec = (0..vals.len() - 1).all(|i| vals[i] > vals[i + 1]);
        if strictly_dec {
            out.push(addr);
        }
    }
    let refs: Vec<&[u8]> = snaps.iter().map(|s| s.as_slice()).collect();
    emit(&out, &refs, width, max);
}

pub fn main(_root: &Path, args: &[String]) -> ! {
    let mut max: usize = 200;
    let mut width: usize = 1;
    let mut command: Option<String> = None;
    let mut positionals: Vec<String> = Vec::new();

    // Globals (--max/--width) precede the subcommand, mirroring argparse where
    // they live on the top-level parser; everything after the command token is
    // a positional for that subcommand.
    let mut it = args.iter();
    while let Some(a) = it.next() {
        if command.is_none() {
            let s = a.as_str();
            if let Some(v) = s.strip_prefix("--max=") {
                max = parse_max(v);
            } else if s == "--max" {
                let v = it
                    .next()
                    .cloned()
                    .unwrap_or_else(|| usage_err("argument --max: expected one argument"));
                max = parse_max(&v);
            } else if let Some(v) = s.strip_prefix("--width=") {
                width = parse_width(v);
            } else if s == "--width" {
                let v = it
                    .next()
                    .cloned()
                    .unwrap_or_else(|| usage_err("argument --width: expected one argument"));
                width = parse_width(&v);
            } else if s.starts_with("--") {
                usage_err(&format!("unrecognized arguments: {s}"));
            } else {
                command = Some(s.to_string());
            }
        } else {
            positionals.push(a.clone());
        }
    }

    let command =
        command.unwrap_or_else(|| usage_err("the following arguments are required: command"));

    match command.as_str() {
        "diff" => {
            if positionals.len() != 2 {
                usage_err("diff takes exactly two snapshots: diff S0 S1");
            }
            cmd_diff(&positionals[0], &positionals[1], width, max);
        }
        "changed" => {
            require_snaps(&positionals);
            cmd_changed(&positionals, width, max);
        }
        "reverted" => {
            require_snaps(&positionals);
            cmd_reverted(&positionals, width, max);
        }
        "constant" => {
            require_snaps(&positionals);
            cmd_constant(&positionals, width, max);
        }
        "monotonic" => {
            require_snaps(&positionals);
            cmd_monotonic(&positionals, width, max);
        }
        other => usage_err(&format!(
            "argument command: invalid choice: '{other}' (choose from 'diff', 'changed', 'reverted', 'constant', 'monotonic')"
        )),
    }
    exit(0)
}

fn require_snaps(positionals: &[String]) {
    // argparse nargs="+" requires at least one; the >=2 / >=3 checks live in the
    // handlers to reproduce the exact per-command error strings.
    if positionals.is_empty() {
        usage_err("the following arguments are required: snaps");
    }
}

fn parse_max(v: &str) -> usize {
    v.parse()
        .unwrap_or_else(|_| usage_err(&format!("argument --max: invalid int value: '{v}'")))
}

fn parse_width(v: &str) -> usize {
    match v {
        "1" => 1,
        "2" => 2,
        "4" => 4,
        _ => usage_err(&format!(
            "argument --width: invalid choice: '{v}' (choose from 1, 2, 4)"
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snap(fill: &[(usize, u8)]) -> Vec<u8> {
        let mut v = vec![0u8; RAM_SIZE];
        for &(a, b) in fill {
            v[a] = b;
        }
        v
    }

    #[test]
    fn diff_byte_logic() {
        let a = snap(&[(0x10, 1), (0x20, 5)]);
        let b = snap(&[(0x10, 2), (0x20, 5)]);
        let mut out = Vec::new();
        for addr in addresses(1) {
            if a[addr..addr + 1] != b[addr..addr + 1] {
                out.push(addr);
            }
        }
        assert_eq!(out, vec![0x10]);
    }

    #[test]
    fn value_at_little_endian() {
        let mut s = vec![0u8; RAM_SIZE];
        s[0x100] = 0x34;
        s[0x101] = 0x12;
        assert_eq!(value_at(&s, 0x100, 2), 0x1234);
        assert_eq!(format_value(&s, 0x100, 2), "1234");
        assert_eq!(format_value(&s, 0x100, 1), "34");
    }

    #[test]
    fn monotonic_increasing_and_decreasing() {
        // width-2 value at 0x200 goes 1 -> 2 -> 3 (increasing).
        let mk = |v: u16| {
            let mut s = vec![0u8; RAM_SIZE];
            s[0x200] = (v & 0xFF) as u8;
            s[0x201] = (v >> 8) as u8;
            s
        };
        let snaps = [mk(1), mk(2), mk(3)];
        let vals: Vec<u64> = snaps.iter().map(|s| value_at(s, 0x200, 2)).collect();
        assert!((0..vals.len() - 1).all(|i| vals[i] < vals[i + 1]));
    }

    #[test]
    fn width_parsing() {
        assert_eq!(parse_width("1"), 1);
        assert_eq!(parse_width("2"), 2);
        assert_eq!(parse_width("4"), 4);
    }
}
