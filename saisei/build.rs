//! Bake the checkout's git revision into the launcher at build time.
//!
//! `saisei version` and the crash-manifest `runtime_version` used to shell out
//! to `git rev-parse` on every game launch. Reading `.git` directly here means
//! git needn't be on PATH at run time — and the hash then describes the binary
//! itself rather than whatever tree it happens to be standing in when it runs.

use std::path::{Path, PathBuf};

/// Resolve HEAD to a short sha by reading `.git` — no git binary involved.
fn head_short_sha(repo: &Path) -> Option<String> {
    let dot_git = repo.join(".git");
    // In a worktree or submodule checkout, .git is a file: "gitdir: <path>".
    let git_dir = if dot_git.is_file() {
        let s = std::fs::read_to_string(&dot_git).ok()?;
        let p = PathBuf::from(s.strip_prefix("gitdir:")?.trim());
        if p.is_absolute() {
            p
        } else {
            repo.join(p)
        }
    } else {
        dot_git
    };

    let head_path = git_dir.join("HEAD");
    println!("cargo:rerun-if-changed={}", head_path.display());
    let head = std::fs::read_to_string(&head_path).ok()?;
    let head = head.trim();

    let sha = match head.strip_prefix("ref: ") {
        // Detached HEAD holds the sha directly.
        None => head.to_string(),
        Some(git_ref) => {
            let loose = git_dir.join(git_ref);
            println!("cargo:rerun-if-changed={}", loose.display());
            match std::fs::read_to_string(&loose) {
                Ok(s) => s.trim().to_string(),
                // A fresh clone keeps its refs only in packed-refs.
                Err(_) => {
                    let packed = git_dir.join("packed-refs");
                    println!("cargo:rerun-if-changed={}", packed.display());
                    std::fs::read_to_string(&packed)
                        .ok()?
                        .lines()
                        .filter(|l| !l.starts_with('#') && !l.starts_with('^'))
                        .find_map(|l| {
                            let (sha, name) = l.split_once(' ')?;
                            (name.trim() == git_ref).then(|| sha.to_string())
                        })?
                }
            }
        }
    };

    let short: String = sha.chars().take(7).collect();
    (short.len() == 7 && short.chars().all(|c| c.is_ascii_hexdigit())).then_some(short)
}

fn main() {
    let manifest = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"));
    let repo = manifest.parent().unwrap_or(&manifest).to_path_buf();
    let rev = head_short_sha(&repo).unwrap_or_else(|| "unknown".to_string());
    println!("cargo:rustc-env=SAISEI_GIT_HASH={rev}");
}
