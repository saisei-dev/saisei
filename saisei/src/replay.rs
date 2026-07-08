//! `saisei replay` — re-run a session log against a fresh game.
//! . A session log is the input track recorded by
//! the shim's session_log machinery (`vns=<virtual_ns> bytes=<hex>` lines).
//! Replaying against a game launched with `--replay` reproduces the original
//! state: for each entry it sends step(vns-now) then write(bytes), so the
//! shim's virtual clock lands on exactly the recorded virtual time.
//!
//! This tool does NOT start the game; it just writes to the FIFO. Start the
//! game with `saisei run <game> --replay` before invoking.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::exit;
use std::time::Duration;

// 1 BIOS tick at IRQ0 18.2 Hz = 54_925_000 ns.
const NS_PER_TICK: i128 = 54_925_000;

fn fail(msg: &str) -> ! {
    eprintln!("{msg}");
    exit(1)
}

fn usage_err(msg: &str) -> ! {
    eprintln!("usage: saisei replay <log> [--fifo PATH] [--inter-write-gap-ms MS]");
    eprintln!("replay: error: {msg}");
    exit(2)
}

/// Faithful-enough reproduction of the reference's `repr()` for the strings that
/// appear in the parse warnings (ASCII log lines / hex strings).
fn py_repr(s: &str) -> String {
    let has_single = s.contains('\'');
    let has_double = s.contains('"');
    let quote = if has_single && !has_double { '"' } else { '\'' };
    let mut out = String::new();
    out.push(quote);
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c == quote => {
                out.push('\\');
                out.push(c);
            }
            c if (c as u32) < 0x20 || (c as u32) == 0x7f => {
                out.push_str(&format!("\\x{:02x}", c as u32));
            }
            c => out.push(c),
        }
    }
    out.push(quote);
    out
}

/// Result of matching a stripped, non-comment line against the log grammar
/// `^vns=(\d+)\s+bytes=([0-9A-Fa-f ]+)$`.
enum LineMatch {
    NoMatch,
    Matched { vns: i128, hex_group: String },
}

fn match_line(line: &str) -> LineMatch {
    let rest = match line.strip_prefix("vns=") {
        Some(r) => r,
        None => return LineMatch::NoMatch,
    };
    // \d+
    let digits_end = rest
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(rest.len());
    if digits_end == 0 {
        return LineMatch::NoMatch;
    }
    let (num, after_num) = rest.split_at(digits_end);
    // \s+
    let ws_end = after_num
        .find(|c: char| !c.is_whitespace())
        .unwrap_or(after_num.len());
    if ws_end == 0 {
        return LineMatch::NoMatch;
    }
    let after_ws = &after_num[ws_end..];
    // bytes=
    let hex_group = match after_ws.strip_prefix("bytes=") {
        Some(h) => h,
        None => return LineMatch::NoMatch,
    };
    // [0-9A-Fa-f ]+  (at least one char, only hex digits or spaces)
    if hex_group.is_empty() || !hex_group.chars().all(|c| c.is_ascii_hexdigit() || c == ' ') {
        return LineMatch::NoMatch;
    }
    let vns: i128 = match num.parse() {
        Ok(v) => v,
        Err(_) => return LineMatch::NoMatch,
    };
    LineMatch::Matched {
        vns,
        hex_group: hex_group.to_string(),
    }
}

fn decode_hex(hex: &str) -> Option<Vec<u8>> {
    // the reference's bytes.fromhex, after the spaces are stripped, needs an even
    // number of nibbles (an empty string decodes to b"").
    if hex.len() % 2 != 0 {
        return None;
    }
    let bytes = hex.as_bytes();
    let mut out = Vec::with_capacity(hex.len() / 2);
    let mut i = 0;
    while i < bytes.len() {
        let hi = (bytes[i] as char).to_digit(16)?;
        let lo = (bytes[i + 1] as char).to_digit(16)?;
        out.push(((hi << 4) | lo) as u8);
        i += 2;
    }
    Some(out)
}

fn parse_log(path: &Path) -> std::io::Result<Vec<(i128, Vec<u8>)>> {
    let text = std::fs::read_to_string(path)?;
    let mut entries: Vec<(i128, Vec<u8>)> = Vec::new();
    for (idx, raw) in text.split('\n').enumerate() {
        // the reference's splitlines() drops a trailing empty final line; split('\n')
        // would leave one, so skip an empty trailing segment.
        let lineno = idx + 1;
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        match match_line(line) {
            LineMatch::NoMatch => {
                eprintln!(
                    "replay: ignoring unparseable line {lineno}: {}",
                    py_repr(line)
                );
            }
            LineMatch::Matched { vns, hex_group } => {
                let hex_str: String = hex_group.chars().filter(|&c| c != ' ').collect();
                match decode_hex(&hex_str) {
                    Some(data) => entries.push((vns, data)),
                    None => {
                        eprintln!("replay: bad hex on line {lineno}: {}", py_repr(&hex_str));
                    }
                }
            }
        }
    }
    Ok(entries)
}

fn send(fifo: &Path, data: &[u8]) {
    match std::fs::OpenOptions::new().write(true).open(fifo) {
        Ok(mut f) => {
            let _ = f.write_all(data);
        }
        Err(e) => fail(&format!("replay: cannot open FIFO {}: {e}", fifo.display())),
    }
}

fn send_step(fifo: &Path, ticks: i128) {
    if ticks <= 0 {
        return;
    }
    let t = if ticks > 0xFFFF { 0xFFFF } else { ticks };
    send(fifo, &[0x17, (t & 0xFF) as u8, ((t >> 8) & 0xFF) as u8]);
}

/// the reference `round()` — round half to even.
fn py_round(x: f64) -> i128 {
    let floor = x.floor();
    let diff = x - floor;
    if diff < 0.5 {
        floor as i128
    } else if diff > 0.5 {
        floor as i128 + 1
    } else {
        let f = floor as i128;
        if f.rem_euclid(2) == 0 {
            f
        } else {
            f + 1
        }
    }
}

pub fn main(_root: &Path, args: &[String]) -> ! {
    let mut log: Option<String> = None;
    let mut fifo_arg = "/tmp/saisei_fifo".to_string();
    let mut gap_ms: f64 = 5.0;

    let mut it = args.iter();
    while let Some(a) = it.next() {
        let s = a.as_str();
        if let Some(v) = s.strip_prefix("--fifo=") {
            fifo_arg = v.to_string();
        } else if s == "--fifo" {
            fifo_arg = it
                .next()
                .cloned()
                .unwrap_or_else(|| usage_err("argument --fifo: expected one argument"));
        } else if let Some(v) = s.strip_prefix("--inter-write-gap-ms=") {
            gap_ms = v.parse().unwrap_or_else(|_| {
                usage_err(&format!(
                    "argument --inter-write-gap-ms: invalid float value: '{v}'"
                ))
            });
        } else if s == "--inter-write-gap-ms" {
            let v = it.next().cloned().unwrap_or_else(|| {
                usage_err("argument --inter-write-gap-ms: expected one argument")
            });
            gap_ms = v.parse().unwrap_or_else(|_| {
                usage_err(&format!(
                    "argument --inter-write-gap-ms: invalid float value: '{v}'"
                ))
            });
        } else if s.starts_with("--") {
            usage_err(&format!("unrecognized arguments: {s}"));
        } else if log.is_some() {
            usage_err(&format!("unrecognized arguments: {s}"));
        } else {
            log = Some(s.to_string());
        }
    }
    let log = log.unwrap_or_else(|| usage_err("the following arguments are required: log"));

    let log_path = PathBuf::from(&log);
    if !log_path.exists() {
        fail(&format!("replay: log not found: {log}"));
    }
    let fifo = PathBuf::from(&fifo_arg);
    if !fifo.exists() {
        fail(&format!(
            "replay: FIFO not found: {fifo_arg}. Start the game first."
        ));
    }

    let entries = match parse_log(&log_path) {
        Ok(e) => e,
        Err(e) => fail(&format!("replay: cannot read log {log}: {e}")),
    };
    if entries.is_empty() {
        fail("replay: log had no replayable entries");
    }
    println!("replay: {} entries from {log}", entries.len());

    let gap = Duration::from_secs_f64(gap_ms / 1000.0);

    // Step the integer-tick delta between entries so vclock advances at the
    // same rate as the original recording (gradual PIT catchup at each
    // safepoint).
    let mut current_vns: i128 = 0;
    let total = entries.len();
    for (i, (vns, data)) in entries.iter().enumerate() {
        if *vns > current_vns {
            let delta = vns - current_vns;
            let ticks = py_round(delta as f64 / NS_PER_TICK as f64);
            if ticks > 0 {
                send_step(&fifo, ticks);
                std::thread::sleep(gap);
                current_vns += ticks * NS_PER_TICK;
            }
        }
        send(&fifo, data);
        std::thread::sleep(gap);
        if (i + 1) % 20 == 0 {
            println!("  ... {}/{}", i + 1, total);
        }
    }

    println!("replay: done");
    exit(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_valid_line() {
        match match_line("vns=1000  bytes=12 4D") {
            LineMatch::Matched { vns, hex_group } => {
                assert_eq!(vns, 1000);
                let hex: String = hex_group.chars().filter(|&c| c != ' ').collect();
                assert_eq!(decode_hex(&hex).unwrap(), vec![0x12, 0x4D]);
            }
            LineMatch::NoMatch => panic!("should match"),
        }
    }

    #[test]
    fn rejects_bad_lines() {
        assert!(matches!(match_line("vns=  bytes=12"), LineMatch::NoMatch));
        assert!(matches!(match_line("vns=10 bytes="), LineMatch::NoMatch));
        assert!(matches!(match_line("vns=10bytes=12"), LineMatch::NoMatch));
        assert!(matches!(match_line("nothing here"), LineMatch::NoMatch));
        // hex with a non-hex char in the bytes group does not match the grammar.
        assert!(matches!(match_line("vns=10 bytes=1g"), LineMatch::NoMatch));
    }

    #[test]
    fn odd_hex_is_bad_even_is_ok() {
        assert!(decode_hex("123").is_none());
        assert_eq!(decode_hex("").unwrap(), Vec::<u8>::new());
        assert_eq!(decode_hex("00ff").unwrap(), vec![0x00, 0xff]);
    }

    #[test]
    fn round_half_to_even() {
        assert_eq!(py_round(0.5), 0);
        assert_eq!(py_round(1.5), 2);
        assert_eq!(py_round(2.5), 2);
        assert_eq!(py_round(2.4), 2);
        assert_eq!(py_round(2.6), 3);
    }

    #[test]
    fn repr_quotes() {
        assert_eq!(py_repr("abc"), "'abc'");
        assert_eq!(py_repr("a\tb"), "'a\\tb'");
    }
}
