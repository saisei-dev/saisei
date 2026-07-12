//! Adding a game from the window: drop a zip on it, or paste a link to one.
//!
//! The unpacking is `new_game`'s — the same code the CLI's `new-game` runs, so a
//! game added by dropping it on the window and a game added from a terminal are
//! the same bundle. The only thing that differs is how the one question it asks
//! ("which file starts the game?") is put: a numbered prompt on a tty, a list of
//! tiles here.

use std::path::Path;

use saisei::new_game::{self, Staged};
use saisei_ui::Ui;

/// The bundle we unpacked and are waiting on an executable choice for.
static mut STAGED: Option<Staged> = None;

fn offer(ui: &mut Ui, staged: Staged) {
    // One executable and there is nothing to ask: that is the game.
    if staged.execs.len() == 1 {
        finish(ui, &staged, 0);
        return;
    }
    ui.add.status = Some(format!("Unpacked '{}'.", staged.name));
    ui.add.exes = staged.exe_names();
    ui.add.exe_idx = 0;
    unsafe { STAGED = Some(staged) };
}

fn finish(ui: &mut Ui, staged: &Staged, exe: usize) {
    let Some(path) = staged.execs.get(exe) else {
        return;
    };
    match new_game::finish(staged, path) {
        Ok(_) => {
            ui.add.status = Some(format!("Added '{}'.", staged.name));
            ui.add.exes.clear();
            // Back to the library, where the game now is.
            ui.screen = saisei_ui::Screen::Library;
        }
        Err(e) => ui.add.status = Some(e),
    }
}

/// A dropped zip or folder.
pub fn from_path(root: &Path, ui: &mut Ui, path: &Path) {
    ui.add.busy = true;
    ui.add.status = Some("Unpacking…".into());
    match new_game::stage(root, &path.display().to_string(), None, false) {
        Ok(s) => offer(ui, s),
        Err(e) => ui.add.status = Some(e),
    }
    ui.add.busy = false;
}

/// A pasted link.
///
/// The download runs on this thread, so the window stops responding for its
/// duration. That is honest for now — it is a few seconds on a game archive — and
/// the alternative is a background thread feeding progress into the UI, which is
/// worth doing once anything else here needs to be asynchronous too.
pub fn from_url(root: &Path, ui: &mut Ui, url: &str) {
    ui.add.busy = true;
    ui.add.status = Some("Downloading…".into());
    match new_game::stage(root, url, None, false) {
        Ok(s) => offer(ui, s),
        Err(e) => ui.add.status = Some(e),
    }
    ui.add.busy = false;
}

/// The player picked which executable starts the game.
pub fn pick_exe(_root: &Path, ui: &mut Ui, exe: usize) {
    let staged = unsafe { core::ptr::addr_of!(STAGED).read() };
    if let Some(s) = staged {
        finish(ui, &s, exe);
        unsafe { STAGED = None };
    }
}
