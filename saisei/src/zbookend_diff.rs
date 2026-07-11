//! `saisei zbookend-diff` — diff two bookend RAM snapshots.
//! . Both inputs are raw `virtual_memory`
//! dumps (MEMORY_SIZE = 2 MiB); output is the list of changed byte runs sorted
//! by address, optionally annotated with the writers found in a bookend write
//! log. Defaults skip VGA (0xA0000-0xBFFFF) and the BIOS data area
//! (0x00400-0x004FF).

use std::collections::HashMap;
use std::path::Path;
use std::process::exit;

fn die(msg: &str) -> ! {
    eprintln!("zbookend-diff: {msg}");
    exit(2)
}

/// (lo, hi_exclusive) ranges excluded from the diff by default.
const SKIP_RANGES_DEFAULT: [(usize, usize); 2] = [
    (0xA0000, 0xC0000), // VGA pages
    (0x00400, 0x00500), // BIOS data area (clock tick, etc.)
];

fn in_skip(i: usize, skip_ranges: &[(usize, usize)]) -> bool {
    skip_ranges.iter().any(|&(lo, hi)| lo <= i && i < hi)
}

/// Port of find_runs: list of (lo, hi_exclusive) changed runs, merging up to 8
/// bytes of equal padding inside a run and trimming trailing equal bytes.
fn find_runs(a: &[u8], b: &[u8], skip_ranges: &[(usize, usize)]) -> Vec<(usize, usize)> {
    let mut runs = Vec::new();
    let n = a.len().min(b.len());
    let mut i = 0usize;
    while i < n {
        if in_skip(i, skip_ranges) {
            // jump to the end of the nearest skip range
            for &(lo, hi) in skip_ranges {
                if lo <= i && i < hi {
                    i = hi;
                    break;
                }
            }
            continue;
        }
        if a[i] != b[i] {
            let mut j = i + 1;
            // allow up to 8 bytes of equal padding inside a run
            let mut equal_streak = 0;
            while j < n && equal_streak < 8 {
                if in_skip(j, skip_ranges) {
                    break;
                }
                if a[j] != b[j] {
                    equal_streak = 0;
                } else {
                    equal_streak += 1;
                }
                j += 1;
            }
            // trim trailing equal padding from run
            while j > i + 1 && a[j - 1] == b[j - 1] {
                j -= 1;
            }
            runs.push((i, j));
            i = j;
        } else {
            i += 1;
        }
    }
    runs
}

/// Parse one `W <hex> size=<dec>...` write-log line into (addr, size).
fn parse_wline(line: &str) -> Option<(u64, u64)> {
    let rest = line.strip_prefix("W ")?;
    let addr_end = rest.find(|c: char| !c.is_ascii_hexdigit())?;
    if addr_end == 0 {
        return None;
    }
    let addr = u64::from_str_radix(&rest[..addr_end], 16).ok()?;
    let after = rest[addr_end..].strip_prefix(" size=")?;
    let dig_end = after
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(after.len());
    if dig_end == 0 {
        return None;
    }
    let size = after[..dig_end].parse::<u64>().ok()?;
    Some((addr, size))
}

/// Port of parse_log: map addr -> log lines (rstripped) that wrote to it.
fn parse_log(path: &str) -> HashMap<u64, Vec<String>> {
    let mut by_addr: HashMap<u64, Vec<String>> = HashMap::new();
    if path.is_empty() || !Path::new(path).exists() {
        return by_addr;
    }
    let bytes = match std::fs::read(path) {
        Ok(b) => b,
        Err(_) => return by_addr,
    };
    let text = String::from_utf8_lossy(&bytes);
    for line in text.lines() {
        if let Some((addr, size)) = parse_wline(line) {
            let entry = line.trim_end().to_string();
            for a in addr..addr + size {
                by_addr.entry(a).or_default().push(entry.clone());
            }
        }
    }
    by_addr
}

/// Port of fmt_run: inline old/new hex for runs <= 16 bytes, else a length note.
fn fmt_run(a: &[u8], b: &[u8], lo: usize, hi: usize) -> String {
    let length = hi - lo;
    if length <= 16 {
        let old: Vec<String> = (lo..hi).map(|x| format!("{:02X}", a[x])).collect();
        let new: Vec<String> = (lo..hi).map(|x| format!("{:02X}", b[x])).collect();
        format!("  {}\n  -> {}", old.join(" "), new.join(" "))
    } else {
        format!("  ({length} bytes — too long to print inline)")
    }
}

struct Args {
    snap1: String,
    snap2: String,
    log: String,
    max: usize,
    addr: Option<String>,
    no_default_skip: bool,
    max_writers: usize,
}

fn parse_args(args: &[String]) -> Args {
    let mut positional: Vec<String> = Vec::new();
    let mut log = "/tmp/zbookend.log".to_string();
    let mut max = 200usize;
    let mut addr: Option<String> = None;
    let mut no_default_skip = false;
    let mut max_writers = 4usize;

    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--log" => {
                log = it
                    .next()
                    .cloned()
                    .unwrap_or_else(|| die("--log needs a value"))
            }
            "--max" => {
                max = it
                    .next()
                    .and_then(|v| v.parse().ok())
                    .unwrap_or_else(|| die("--max needs an integer"))
            }
            "--addr" => {
                addr = Some(
                    it.next()
                        .cloned()
                        .unwrap_or_else(|| die("--addr needs LO:HI")),
                )
            }
            "--no-default-skip" => no_default_skip = true,
            "--max-writers" => {
                max_writers = it
                    .next()
                    .and_then(|v| v.parse().ok())
                    .unwrap_or_else(|| die("--max-writers needs an integer"))
            }
            other if other.starts_with("--") => die(&format!("unrecognized argument: {other}")),
            other => positional.push(other.to_string()),
        }
    }
    if positional.len() < 2 {
        die("usage: zbookend-diff snap1 snap2 [--log P] [--max N] [--addr LO:HI] [--no-default-skip] [--max-writers N]");
    }
    Args {
        snap1: positional[0].clone(),
        snap2: positional[1].clone(),
        log,
        max,
        addr,
        no_default_skip,
        max_writers,
    }
}

pub fn main(_root: &Path, argv: &[String]) -> ! {
    let args = parse_args(argv);

    let a =
        std::fs::read(&args.snap1).unwrap_or_else(|e| die(&format!("read {}: {e}", args.snap1)));
    let b =
        std::fs::read(&args.snap2).unwrap_or_else(|e| die(&format!("read {}: {e}", args.snap2)));
    if a.len() != b.len() {
        println!(
            "[warn] snapshot sizes differ: {} vs {} — comparing common prefix",
            a.len(),
            b.len()
        );
    }

    let mut skip: Vec<(usize, usize)> = if args.no_default_skip {
        Vec::new()
    } else {
        SKIP_RANGES_DEFAULT.to_vec()
    };
    if let Some(addr) = &args.addr {
        let parts: Vec<&str> = addr.split(':').collect();
        if parts.len() != 2 {
            die("--addr must be LO:HI (hex)");
        }
        let lo = usize::from_str_radix(parts[0], 16).unwrap_or_else(|_| die("bad --addr LO"));
        let hi = usize::from_str_radix(parts[1], 16).unwrap_or_else(|_| die("bad --addr HI"));
        // convert "restrict to [lo,hi)" into "skip everything outside"
        skip.push((0, lo));
        skip.push((hi, a.len().max(b.len())));
    }

    let runs = find_runs(&a, &b, &skip);
    let by_addr = parse_log(&args.log);

    println!("snap1: {}  ({} bytes)", args.snap1, a.len());
    println!("snap2: {}  ({} bytes)", args.snap2, b.len());
    println!(
        "log:   {}  ({})",
        args.log,
        if by_addr.is_empty() {
            "missing/empty"
        } else {
            "present"
        }
    );
    println!("runs:  {} (showing up to {})", runs.len(), args.max);
    println!();

    for (idx, &(lo, hi)) in runs.iter().take(args.max).enumerate() {
        println!(
            "#{:>4}  {:05X}..{:05X}  (+{}B)",
            idx + 1,
            lo,
            hi - 1,
            hi - lo
        );
        println!("{}", fmt_run(&a, &b, lo, hi));
        if !by_addr.is_empty() {
            // collect unique writer lines for this run, in address then log order
            let mut seen: Vec<&String> = Vec::new();
            let mut seen_set: std::collections::HashSet<&String> = std::collections::HashSet::new();
            'outer: for addr in lo as u64..hi as u64 {
                if let Some(lines) = by_addr.get(&addr) {
                    for line in lines {
                        if seen_set.contains(line) {
                            continue;
                        }
                        seen_set.insert(line);
                        seen.push(line);
                        if seen.len() >= args.max_writers {
                            break 'outer;
                        }
                    }
                }
            }
            if !seen.is_empty() {
                println!("  writers:");
                for line in &seen {
                    println!("    {line}");
                }
            } else {
                println!(
                    "  writers: (no log entries — write came from a raw path or was filtered)"
                );
            }
        }
        println!();
    }

    if runs.len() > args.max {
        println!("... {} more runs (use --max to see)", runs.len() - args.max);
    }
    exit(0);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn single_changed_byte() {
        let a = [0u8, 0, 1, 0, 0];
        let b = [0u8, 0, 2, 0, 0];
        assert_eq!(find_runs(&a, &b, &[]), vec![(2, 3)]);
    }

    #[test]
    fn gap_under_8_merges_over_8_splits() {
        // diffs at 0 and at 8: 7 equal bytes in between (indices 1..8) -> merge.
        let mut a = vec![0u8; 20];
        let b = vec![0u8; 20];
        a[0] = 1;
        a[8] = 1;
        assert_eq!(find_runs(&a, &b, &[]), vec![(0, 9)]);

        // diffs at 0 and at 9: 8 equal bytes in between -> two separate runs.
        let mut a2 = vec![0u8; 20];
        let b2 = vec![0u8; 20];
        a2[0] = 1;
        a2[9] = 1;
        assert_eq!(find_runs(&a2, &b2, &[]), vec![(0, 1), (9, 10)]);
    }

    #[test]
    fn skip_ranges_excluded() {
        // change inside a skip range is ignored; change outside is kept.
        let mut a = vec![0u8; 16];
        let b = vec![0u8; 16];
        a[4] = 1; // inside skip [4,8)
        a[10] = 1; // outside
        assert_eq!(find_runs(&a, &b, &[(4, 8)]), vec![(10, 11)]);
    }

    #[test]
    fn wline_parsing() {
        assert_eq!(parse_wline("W 12B00 size=4 by shim"), Some((0x12B00, 4)));
        assert_eq!(parse_wline("W AF size=16"), Some((0xAF, 16)));
        assert_eq!(parse_wline("not a write line"), None);
        assert_eq!(parse_wline("W 12B00 sz=4"), None); // missing " size="
        assert_eq!(parse_wline("W  size=4"), None); // empty addr
    }
}
