//! `saisei zoom` — crop a fixed-grid chunk of a screenshot to look closer.
//! (the `image` crate). The screen is a
//! 4-column x 4-row grid (16 chunks). You name a chunk by (col, row), col 0..3
//! left->right and row 0..3 top->bottom; the right/bottom-most chunk absorbs the
//! integer-division remainder. No upscaling — a smaller crop already yields more
//! output tokens per source pixel.

use std::path::{Path, PathBuf};
use std::process::exit;

const GRID_COLS: u32 = 4;
const GRID_ROWS: u32 = 4;

fn die(msg: &str) -> ! {
    eprintln!("{msg}");
    exit(1)
}

fn parse_int(s: &str, what: &str) -> i64 {
    s.parse::<i64>()
        .unwrap_or_else(|_| die(&format!("zoom: {what} must be an integer: {s}")))
}

pub fn main(_root: &Path, args: &[String]) -> ! {
    let mut positional: Vec<String> = Vec::new();
    let mut out: Option<String> = None;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--out" => {
                out = Some(
                    it.next()
                        .cloned()
                        .unwrap_or_else(|| die("zoom: --out needs a path")),
                )
            }
            other if other.starts_with("--") => {
                die(&format!("zoom: unrecognized argument: {other}"))
            }
            other => positional.push(other.to_string()),
        }
    }
    if positional.len() < 3 {
        die("usage: zoom SRC COL ROW [--out PATH]");
    }
    let src = positional[0].clone();
    let col = parse_int(&positional[1], "col");
    let row = parse_int(&positional[2], "row");

    if !(0..GRID_COLS as i64).contains(&col) || !(0..GRID_ROWS as i64).contains(&row) {
        die(&format!(
            "zoom: col must be 0..{}, row must be 0..{}",
            GRID_COLS - 1,
            GRID_ROWS - 1
        ));
    }
    let col = col as u32;
    let row = row as u32;

    let src_path = PathBuf::from(&src);
    if !src_path.exists() {
        die(&format!("zoom: source not found: {}", src_path.display()));
    }
    let img = image::open(&src_path)
        .unwrap_or_else(|e| die(&format!("zoom: cannot open {}: {e}", src_path.display())))
        .to_rgb8();
    let (sx, sy) = img.dimensions();

    let cw = sx / GRID_COLS;
    let ch = sy / GRID_ROWS;
    let x_lo = col * cw;
    let y_lo = row * ch;
    // Extend the right/bottom-most chunk to absorb the integer-division remainder.
    let x_hi = if col == GRID_COLS - 1 { sx } else { x_lo + cw };
    let y_hi = if row == GRID_ROWS - 1 { sy } else { y_lo + ch };

    let crop = image::imageops::crop_imm(&img, x_lo, y_lo, x_hi - x_lo, y_hi - y_lo).to_image();

    let out_path = match out {
        Some(p) => PathBuf::from(p),
        None => {
            let stem = src_path
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_default();
            let suffix = src_path
                .extension()
                .map(|s| format!(".{}", s.to_string_lossy()))
                .unwrap_or_default();
            let name = format!("{stem}_zoom_{col}_{row}{suffix}");
            match src_path.parent() {
                Some(p) if !p.as_os_str().is_empty() => p.join(name),
                _ => PathBuf::from(name),
            }
        }
    };

    if let Some(parent) = out_path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent).ok();
        }
    }
    crop.save(&out_path)
        .unwrap_or_else(|e| die(&format!("zoom: cannot save {}: {e}", out_path.display())));
    println!("{}", out_path.display());
    exit(0);
}
