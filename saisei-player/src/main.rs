//! Saisei — the player app.
//!
//! With no arguments it opens its library: the logo, then the games you have,
//! then the game you pick, then the in-game overlay — all in one window, one
//! process. `--play <game>` skips the library and boots straight into a game,
//! which is what the dev CLI (`saisei-cli run`/`play`) and the save-load re-exec
//! both use.

mod host;
mod relaunch;

use host::LaunchSpec;

const USAGE: &str = "\
saisei — play DOS games.

usage: saisei                       open the game library
       saisei --play <game>         play a game directly
       saisei --list                list the games you have

play options:
  --program <name>         pick a program in a multi-executable bundle
  --restore-from <save>    resume from a save
  --speedup <n>            emulation speed multiplier (default 1)

Developer commands live in `saisei-cli` (run `saisei-cli help`).";

/// Flags we hand through to the runtime untouched. Everything the runtime's own
/// argv parser understands, minus the ones we consume ourselves.
fn is_runtime_flag(a: &str) -> bool {
    matches!(a, "--headless" | "--verbose" | "--replay" | "--no-splash")
}
fn is_runtime_flag_with_value(a: &str) -> bool {
    matches!(
        a,
        "--restore-from"
            | "--speedup"
            | "--trace-file"
            | "--lifecycle-file"
            | "--screenshot-secs"
            | "--patch-bundle"
    )
}

fn main() {
    let root = saisei::resolve_root();
    let argv: Vec<String> = std::env::args().skip(1).collect();

    let mut spec = LaunchSpec::default();
    let mut play = false;
    let mut it = argv.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "help" | "--help" | "-h" => {
                println!("{USAGE}");
                return;
            }
            "--list" => {
                for g in library::games(&root) {
                    println!("{}", g.key);
                }
                return;
            }
            "--play" => {
                play = true;
                spec.game = it
                    .next()
                    .cloned()
                    .unwrap_or_else(|| fail("--play needs a game name"));
            }
            "--program" => {
                spec.program = Some(
                    it.next()
                        .cloned()
                        .unwrap_or_else(|| fail("--program needs a name")),
                );
            }
            _ if is_runtime_flag(a) => spec.runtime_args.push(a.clone()),
            _ if is_runtime_flag_with_value(a) => {
                spec.runtime_args.push(a.clone());
                spec.runtime_args.push(
                    it.next()
                        .cloned()
                        .unwrap_or_else(|| fail(&format!("{a} needs a value"))),
                );
            }
            // A bare game name is the friendly form of --play.
            _ if !a.starts_with('-') && spec.game.is_empty() => {
                play = true;
                spec.game = a.clone();
            }
            _ => fail(&format!("unrecognized argument: {a}\n\n{USAGE}")),
        }
    }

    if play {
        std::process::exit(host::run(&root, &spec));
    }
    shell::run(&root);
}

fn fail(msg: &str) -> ! {
    eprintln!("saisei: {msg}");
    std::process::exit(2);
}

mod library;
mod shell;
