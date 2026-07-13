//! Saisei — the player app.
//!
//! With no arguments it opens its library: the logo, then the games you have,
//! then the game you pick, then the in-game overlay — all in one window, one
//! process. `--play <game>` skips the library and boots straight into a game,
//! which is what the dev CLI (`saisei-cli run`/`play`) and the save-load re-exec
//! both use.

mod add;
mod host;
mod library;
mod relaunch;
mod save;
mod settings;
mod shell;

use host::LaunchSpec;

const USAGE: &str = "\
saisei — play DOS games.

usage: saisei                       open the game library
       saisei --play <game>         play a game directly
       saisei --list                list the games you have

play options:
  --program <name>         pick a program in a multi-executable bundle
  --exe <FILE.EXE>         run another program off the game's disk (its setup),
                           once, instead of the game
  --restore-from <save>    resume from a save
  --speedup <n>            emulation speed multiplier (default 1)

Developer commands live in `saisei-cli` (run `saisei-cli help`).";

/// Flags we hand through to the runtime untouched. Everything the runtime's own
/// argv parser understands, minus the ones we consume ourselves.
fn is_runtime_flag(a: &str) -> bool {
    matches!(a, "--headless" | "--verbose" | "--replay")
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
    // The logo opens a cold start. Suppressed when we are re-execing out of a
    // menu that has already shown it.
    let mut show_logo = true;
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
            "--no-logo" => show_logo = false,
            "--program" => {
                spec.program = Some(
                    it.next()
                        .cloned()
                        .unwrap_or_else(|| fail("--program needs a name")),
                );
            }
            "--exe" => {
                spec.exe = Some(
                    it.next()
                        .cloned()
                        .unwrap_or_else(|| fail("--exe needs a file name")),
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
        // Through shell::play, not host::run: that is what arms the F12 overlay,
        // and it has to work here too — this is the path `saisei-cli run` takes,
        // and the path the overlay itself re-execs into.
        shell::play(&root, &spec, show_logo);
    }
    shell::run(&root);
}

fn fail(msg: &str) -> ! {
    eprintln!("saisei: {msg}");
    std::process::exit(2);
}
