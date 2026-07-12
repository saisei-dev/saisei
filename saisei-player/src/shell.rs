//! The window: the logo, the library, and the in-game overlay.
//!
//! One window for the whole session. It opens here, shows the logo, fades into
//! the library, and is still the same window the game draws into and the overlay
//! draws over. The runtime owns the SDL bits (see its `saisei_ui_*` entry
//! points); this owns what is on screen and what the keys do.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use saisei_runtime::sdl as rt;
use saisei_ui::{Action, Canvas, GameView, Image, Key, SaveView, Ui};

use crate::host::LaunchSpec;
use crate::library;
use crate::save;

/// The window we ask for. Resizable from here; the interface lays itself out to
/// whatever it actually gets.
const WIN_W: i32 = 1280;
const WIN_H: i32 = 800;

/// The logo holds the window, then dissolves into the library behind it.
const LOGO_HOLD_MS: u32 = 1500;
const LOGO_FADE_MS: u32 = 500;

/// The art is the runtime's — the same file its pre-game splash is baked from, so
/// the player's logo and the game's are one image, not two that can drift apart.
fn logo() -> Image {
    Image::decode_png(include_bytes!("../../runtime/assets/saisei_logo.png")).unwrap_or_default()
}

/// The running game (repo root, bundle key), for the overlay: the runtime calls
/// back into us with no context of its own, so we leave it here on the way in.
///
/// A `OnceLock` and not a `static mut`: this owns heap, and it is read once per
/// F12 — moving it out by `ptr::read` would bitwise-copy it while the static
/// still held the same buffers, and the second overlay would be reading memory
/// the first one's copy had already freed.
static RUNNING: OnceLock<(PathBuf, String)> = OnceLock::new();

// ---- painting ---------------------------------------------------------------

/// Holds the pixel buffer across frames — reallocating a window-sized RGBA buffer
/// every frame would be pure churn.
struct Painter {
    cv: Canvas,
}

impl Painter {
    fn new() -> Painter {
        Painter {
            cv: Canvas::new(1, 1),
        }
    }

    /// The size the runtime wants us to paint at, in real pixels.
    fn size() -> (usize, usize) {
        let (mut w, mut h) = (0i32, 0i32);
        rt::saisei_ui_size(&mut w, &mut h);
        (w.max(1) as usize, h.max(1) as usize)
    }

    fn resize_to_window(&mut self) -> bool {
        let (w, h) = Painter::size();
        if self.cv.w != w || self.cv.h != h {
            self.cv = Canvas::new(w, h);
            return true;
        }
        false
    }

    fn present(&self, over_game: bool) {
        rt::saisei_ui_present(
            self.cv.px.as_ptr(),
            self.cv.w as i32,
            self.cv.h as i32,
            over_game,
        );
    }

    fn paint(&mut self, ui: &mut Ui, over_game: bool) {
        ui.paint(&mut self.cv, over_game);
        self.present(over_game);
    }
}

/// The clipboard, as text.
///
/// A pasted link never reaches us as typed characters — SDL delivers Ctrl+V as a
/// key chord and nothing else — so the interface asks for it and we go and fetch
/// it. The UI crate cannot: it knows nothing about SDL, which is the point of it.
fn clipboard() -> Option<String> {
    let mut buf = [0 as std::ffi::c_char; 4096];
    if !rt::saisei_ui_clipboard(buf.as_mut_ptr(), buf.len() as i32) {
        return None;
    }
    let s = unsafe { std::ffi::CStr::from_ptr(buf.as_ptr()) }
        .to_string_lossy()
        .into_owned();
    Some(s).filter(|s| !s.trim().is_empty())
}

// ---- events -----------------------------------------------------------------

/// Drain the queue into the UI. Returns the action to carry out, and whether
/// anything happened at all — the interface is static between events, so a frame
/// with no input needs no repaint.
fn pump(ui: &mut Ui) -> (Action, bool) {
    let mut act = Action::None;
    let mut touched = false;
    loop {
        let mut ev: rt::UiEvent = unsafe { std::mem::zeroed() };
        if !rt::saisei_ui_poll(&mut ev) {
            break;
        }
        touched = true;
        let a = match ev.kind {
            rt::UI_EV_QUIT => Action::Quit,
            rt::UI_EV_KEY => match ev.code {
                rt::UI_KEY_UP => ui.key(Key::Up),
                rt::UI_KEY_DOWN => ui.key(Key::Down),
                rt::UI_KEY_LEFT => ui.key(Key::Left),
                rt::UI_KEY_RIGHT => ui.key(Key::Right),
                rt::UI_KEY_ENTER => ui.key(Key::Enter),
                rt::UI_KEY_ESCAPE => ui.key(Key::Escape),
                rt::UI_KEY_BACKSPACE => ui.key(Key::Backspace),
                rt::UI_KEY_OVERLAY => ui.key(Key::Overlay),
                rt::UI_KEY_PASTE => ui.key(Key::Paste),
                rt::UI_KEY_DELETE => ui.key(Key::Delete),
                _ => Action::None,
            },
            rt::UI_EV_TEXT => {
                if let Some(ch) = char::from_u32(ev.code as u32) {
                    ui.text(ch);
                }
                Action::None
            }
            rt::UI_EV_MOUSE_MOVE => {
                ui.mouse_move(ev.x as f32, ev.y as f32);
                Action::None
            }
            rt::UI_EV_MOUSE_DOWN => ui.click(ev.x as f32, ev.y as f32),
            rt::UI_EV_DROP => {
                let path = unsafe { std::ffi::CStr::from_ptr(ev.path.as_ptr()) }
                    .to_string_lossy()
                    .into_owned();
                ui.dropped(&path)
            }
            _ => Action::None,
        };
        if a != Action::None {
            act = a;
        }
    }
    (act, touched)
}

// ---- the library ------------------------------------------------------------

/// Every game, with its saves, as the interface wants them.
fn games_view(root: &Path) -> Vec<GameView> {
    library::games(root)
        .into_iter()
        .map(|g| {
            let saves = library::saves(&g.key)
                .into_iter()
                .map(|s| SaveView {
                    when: s.when,
                    thumb: s.thumb.as_deref().and_then(Image::load_png),
                })
                .collect();
            GameView {
                key: g.key,
                title: g.display,
                saves,
            }
        })
        .collect()
}

/// Hold the logo, then dissolve it into the library that is already behind it.
fn intro(ui: &mut Ui, p: &mut Painter) {
    let logo = ui.logo.clone();
    if logo.is_empty() {
        return;
    }
    let start = std::time::Instant::now();
    loop {
        let t = start.elapsed().as_millis() as u32;
        p.resize_to_window();
        let full = saisei_ui::Rect::new(0.0, 0.0, p.cv.w as f32, p.cv.h as f32);

        if t < LOGO_HOLD_MS {
            p.cv.clear(saisei_ui::Color::rgb(0, 0, 0));
            p.cv.image_fit(&logo, full, 255);
        } else if t < LOGO_HOLD_MS + LOGO_FADE_MS {
            // The library is drawn underneath, and the logo thins out over it.
            ui.paint(&mut p.cv, false);
            let k = 1.0 - (t - LOGO_HOLD_MS) as f32 / LOGO_FADE_MS as f32;
            let k = k * k * (3.0 - 2.0 * k); // smoothstep: linger, then fall away
            p.cv.image_fit(&logo, full, (k * 255.0) as u8);
        } else {
            break;
        }
        p.present(false);

        // A quit during the intro still lands; nothing else does.
        let mut ev: rt::UiEvent = unsafe { std::mem::zeroed() };
        while rt::saisei_ui_poll(&mut ev) {
            if ev.kind == rt::UI_EV_QUIT {
                std::process::exit(0);
            }
        }
        rt::saisei_ui_delay(8);
    }
}

pub fn run(root: &Path) -> ! {
    if !rt::saisei_ui_init(WIN_W, WIN_H) {
        eprintln!("saisei: could not open a window. Is SDL2 installed and a display available?");
        std::process::exit(1);
    }
    let mut ui = Ui::new(logo(), games_view(root));
    let mut p = Painter::new();
    intro(&mut ui, &mut p);

    let mut dirty = true;
    loop {
        let (act, touched) = pump(&mut ui);
        match act {
            Action::Launch { game, restore } => launch(root, &ui, game, restore),
            Action::Quit => std::process::exit(0),
            Action::AddPath(path) => {
                crate::add::from_path(root, &mut ui, Path::new(&path));
                ui.games = games_view(root);
            }
            Action::AddUrl(url) => {
                crate::add::from_url(root, &mut ui, &url);
                ui.games = games_view(root);
            }
            Action::Paste => {
                if let Some(text) = clipboard() {
                    ui.pasted(&text);
                }
            }
            Action::DeleteGame(i) => {
                if let Some(g) = ui.games.get(i) {
                    if let Err(e) = library::delete_game(root, &library::saves_root(), &g.key) {
                        eprintln!("saisei: {e}");
                    }
                }
                ui.games = games_view(root);
                // The cursor was on the game that is no longer there.
                ui.game = ui.game.min(ui.games.len());
                dirty = true;
            }
            Action::PickExe(i) => {
                crate::add::pick_exe(root, &mut ui, i);
                ui.games = games_view(root);
            }
            // Nothing to resume into: the library is all there is.
            Action::Resume | Action::Save | Action::ToLibrary | Action::None => {}
        }
        if touched || dirty || p.resize_to_window() {
            p.paint(&mut ui, false);
            dirty = false;
        }
        rt::saisei_ui_delay(8);
    }
}

/// Arm the in-game overlay for `game_key`, then run it. Does not return: the game
/// runs in this process, in this window, and the runtime exits when it is done.
///
/// EVERY way into a game goes through here — the library, `saisei --play`, and
/// the re-exec that loads a save — because F12 has to work in all of them. Wiring
/// it up in the library path alone left the overlay dead on the command line and,
/// worse, dead in the game you re-execed into *from the overlay*.
pub fn play(root: &Path, spec: &LaunchSpec, show_logo: bool) -> ! {
    // The logo is the first thing you see on a cold start. But when the library
    // showed it seconds ago, or we are re-execing out of a menu, showing it again
    // is a stutter, not a flourish. What remains of the splash either way is the
    // hold that covers the JIT's first compiles and a game's text-mode setup.
    rt::virtual_display_set_splash_logo(show_logo);
    RUNNING.set((root.to_path_buf(), spec.game.clone())).ok();
    unsafe { saisei_runtime::shims::saisei_set_overlay_entry(Some(overlay_entry)) };
    std::process::exit(crate::host::run(root, spec));
}

fn launch(root: &Path, ui: &Ui, game: usize, restore: Option<usize>) -> ! {
    let Some(g) = ui.games.get(game) else {
        std::process::exit(0);
    };
    let mut spec = LaunchSpec::new(&g.key);
    if let Some(i) = restore {
        if let Some(s) = library::saves(&g.key).into_iter().nth(i) {
            spec.runtime_args.push("--restore-from".into());
            spec.runtime_args.push(s.dir.display().to_string());
        }
    }
    play(root, &spec, false)
}

// ---- the overlay ------------------------------------------------------------

/// The runtime calls this with the machine frozen at a resting point. Everything
/// the player does here happens while the guest is stopped; returning resumes it
/// exactly where it was.
unsafe extern "C" fn overlay_entry(can_save: bool) {
    let Some((root, key)) = RUNNING.get() else {
        return;
    };
    let mut ui = Ui::new(logo(), games_view(root));
    let Some(idx) = ui.games.iter().position(|g| &g.key == key) else {
        return;
    };
    ui.open_overlay(idx, can_save);

    let mut p = Painter::new();
    let mut dirty = true;
    loop {
        let (act, touched) = pump(&mut ui);
        match act {
            // Back to the game, exactly where it was.
            Action::Resume => return,
            Action::Save => {
                ui.message = Some(match save::write(root, key) {
                    Ok(()) => "Saved.".to_string(),
                    Err(e) => format!("Could not save: {e}"),
                });
                // The new save should be in the list the player is looking at.
                ui.games = games_view(root);
                dirty = true;
            }
            // Leaving the game, or loading a save, or starting over: all of these
            // need clean host state, which only a fresh process gives. The guest
            // has run — its memory, its DOS file table and the JIT registry are
            // all populated, and there is no honest way to unwind that in place.
            Action::ToLibrary => crate::relaunch::exec_player(&[]),
            Action::Launch { game, restore } => {
                let g = &ui.games[game];
                // --no-logo: we came out of a menu, not off a cold start.
                let mut args = vec!["--play".to_string(), g.key.clone(), "--no-logo".into()];
                if let Some(i) = restore {
                    if let Some(s) = library::saves(&g.key).into_iter().nth(i) {
                        args.push("--restore-from".into());
                        args.push(s.dir.display().to_string());
                    }
                }
                crate::relaunch::exec_player(&args);
            }
            Action::Quit => std::process::exit(0),
            Action::AddPath(_)
            | Action::AddUrl(_)
            | Action::PickExe(_)
            | Action::Paste
            | Action::DeleteGame(_)
            | Action::None => {}
        }
        if touched || dirty || p.resize_to_window() {
            p.paint(&mut ui, true);
            dirty = false;
        }
        rt::saisei_ui_delay(8);
    }
}
