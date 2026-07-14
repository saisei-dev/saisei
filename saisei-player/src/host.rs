//! Running a game in *this* process.
//!
//! The launcher used to spawn a separately-compiled per-game binary. The player
//! is that binary: it installs the game's `GameConfig` at run time and calls the
//! runtime's `saisei_main` directly. Nothing is compiled per game, and the SDL
//! window the player already opened for its library is the window the game draws
//! into.

use std::ffi::{c_char, CString};
use std::path::{Path, PathBuf};
use std::ptr;

use saisei::GameDef;
use saisei_runtime::shims::{
    saisei_main, saisei_set_game_config, shim_boot_machine, GameConfig, ProtectedSlot,
};

/// What to run, and how. `runtime_args` are the flags `saisei_main` itself
/// parses (`--headless`, `--speedup`, `--restore-from`, the dev/drive flags);
/// `program` selects one of a multi-executable bundle's programs.
///
/// `exe` runs a different program off the same game's disk, once — its setup, its
/// installer. It is not a program in the `programs[]` sense and does not become
/// the game's default: it names a file the bundle stages, and it is the image
/// this one launch loads instead of the game's. Everything else about the machine
/// — the drive, the PSP, the protected slots — is the game's, because it is the
/// same machine, and a setup that wrote its config to some *other* disk would be
/// no use to the game that has to read it back.
#[derive(Clone, Default)]
pub struct LaunchSpec {
    pub game: String,
    pub program: Option<String>,
    pub exe: Option<String>,
    pub runtime_args: Vec<String>,
}

impl LaunchSpec {
    pub fn new(game: &str) -> Self {
        LaunchSpec {
            game: game.to_string(),
            ..Default::default()
        }
    }

    /// The argv that reproduces this launch. The runtime's save-load path
    /// re-execs the process to restore a snapshot into clean host state, and it
    /// needs an argv that names the game — which the GUI's own argv (a bare
    /// `saisei`) does not. `relaunch_argv` is what it re-execs with.
    pub fn relaunch_argv(&self, exe: &str) -> Vec<String> {
        let mut v = vec![exe.to_string(), "--play".into(), self.game.clone()];
        if let Some(p) = &self.program {
            v.push("--program".into());
            v.push(p.clone());
        }
        if let Some(f) = &self.exe {
            v.push("--exe".into());
            v.push(f.clone());
        }
        v.extend(self.runtime_args.iter().cloned());
        v
    }
}

/// A `CString` handed to the runtime and never reclaimed. The GameConfig the
/// runtime reads must outlive every read of it, which is the whole process.
fn leak_cstr(s: &str) -> *const c_char {
    CString::new(s)
        .expect("nul byte in game config string")
        .into_raw()
}

/// Install the bundle's machine parameters as the process's active GameConfig.
///
/// This is the run-time twin of `generate_game_config`, which bakes the same
/// values into a frozen per-game binary; both read them through
/// `saisei::game_config_values`, so the two cannot drift. `entry`, `call_targets`,
/// `binary_dispatch` and `patches` are NULL here exactly as they are there —
/// every address routes through the JIT.
pub fn install_game_config(def: &GameDef) {
    let vals = saisei::game_config_values(def);

    let slots: Vec<ProtectedSlot> = vals
        .protected_slots
        .iter()
        .map(|(lo, hi, name)| ProtectedSlot {
            lo: *lo,
            hi: *hi,
            name: leak_cstr(name),
        })
        .collect();
    let slot_count = slots.len();
    let slots: &'static [ProtectedSlot] = Box::leak(slots.into_boxed_slice());

    let config: &'static GameConfig = Box::leak(Box::new(GameConfig {
        name: leak_cstr(&vals.name),
        program_path: leak_cstr(&vals.program_path),
        entry: None,
        call_targets: ptr::null(),
        call_target_count: 0,
        binary_dispatch: ptr::null(),
        binary_dispatch_count: 0,
        protected_slots: if slot_count == 0 {
            ptr::null()
        } else {
            slots.as_ptr()
        },
        protected_slot_count: slot_count,
        init_cs: vals.init_cs,
        psp_seg: vals.psp_seg,
        patches: ptr::null(),
        patch_count: 0,
        command_tail: leak_cstr(&vals.args),
    }));

    unsafe { saisei_set_game_config(config) };

    // The runtime boots the machine from a constructor — it loads the program
    // image the config names, long before we got to parse an argument. That boot
    // ran against the empty default config and loaded nothing. Now that the game
    // is known, boot for real.
    unsafe { shim_boot_machine() };
}

/// Stage the bundle's runtime files under `build/<game>/` and make that the cwd,
/// as the guest's DOS working directory. The runtime resolves the program image
/// and every game data file relative to it.
fn stage(root: &Path, def: &GameDef) -> PathBuf {
    let runtime_dir = saisei::copy_runtime(root, def);
    for (k, v) in saisei::game_process_env(root, def) {
        std::env::set_var(k, v);
    }
    std::env::set_current_dir(&runtime_dir).unwrap_or_else(|e| {
        eprintln!("saisei: cannot enter {}: {e}", runtime_dir.display());
        std::process::exit(1);
    });
    runtime_dir
}

/// Run the game to completion in this process. Returns the runtime's exit code.
///
/// The runtime exits the process itself on a DOS terminate, a crash, or a window
/// close, so in practice this rarely returns.
pub fn run(root: &Path, spec: &LaunchSpec) -> i32 {
    let mut def = saisei::load_game_definition(root, &spec.game, spec.program.as_deref());

    // A one-off run of another program on this game's disk. Only a file the
    // bundle actually stages can be named: the drive is built from `runtime[]`,
    // so anything else is a path the guest could not open anyway — and this is
    // the one place a launch takes a filename rather than a game name.
    if let Some(want) = &spec.exe {
        let found = saisei::bundle_executables(root, &def)
            .into_iter()
            .find(|e| e.eq_ignore_ascii_case(want))
            .unwrap_or_else(|| {
                eprintln!("saisei: {} has no program called '{want}'", def.name);
                std::process::exit(1);
            });
        // argv[0] and the crash bundles should name what is actually running.
        def.program_key = saisei::sanitize_identifier(
            Path::new(&found)
                .file_stem()
                .unwrap_or_default()
                .to_string_lossy()
                .as_ref(),
        );
        def.program_path = found;
    }

    stage(root, &def);
    install_game_config(&def);

    // The runtime re-execs itself to load a save (clean host state is the only
    // honest way to restore one), so it needs an argv naming this game.
    crate::relaunch::arm(spec);

    // argv[0] mimics the old per-game binary's, so crash bundles and the
    // save-restore log keep naming the program rather than the launcher.
    let mut args = vec![format!("./{}", def.program_key)];
    args.extend(spec.runtime_args.iter().cloned());

    let cstrs: Vec<CString> = args
        .iter()
        .map(|a| CString::new(a.as_str()).expect("nul in arg"))
        .collect();
    let mut argv: Vec<*mut c_char> = cstrs.iter().map(|c| c.as_ptr() as *mut c_char).collect();
    argv.push(ptr::null_mut());

    unsafe { saisei_main((argv.len() - 1) as i32, argv.as_mut_ptr()) }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The runtime re-execs to load a save, and it re-execs with *this* argv. It
    /// has to come back into the same game, with the same program and the same
    /// flags — a re-exec that dropped `--play` would land the player in its
    /// library instead of in the save it was asked for.
    #[test]
    fn relaunch_argv_names_the_game_it_came_from() {
        let mut spec = LaunchSpec::new("zeliard_dos_en");
        spec.program = Some("setup".into());
        spec.runtime_args = vec!["--speedup".into(), "2".into()];

        assert_eq!(
            spec.relaunch_argv("/usr/bin/saisei"),
            [
                "/usr/bin/saisei",
                "--play",
                "zeliard_dos_en",
                "--program",
                "setup",
                "--speedup",
                "2"
            ]
        );
    }
}
