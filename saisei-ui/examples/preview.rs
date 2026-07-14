//! Render the UI screens to PNGs so they can be looked at without a window.
use saisei_ui::*;

/// A stand-in for the frozen game frame the runtime has already drawn. The
/// overlay screens are composited over it here exactly as SDL composites them
/// over the real one — a menu that reads beautifully against black and not at all
/// against a bright game is a menu that was never really tested.
fn fake_game(w: usize, h: usize) -> Vec<u8> {
    let mut px = vec![0u8; w * h * 3];
    for y in 0..h {
        for x in 0..w {
            let i = (y * w + x) * 3;
            // A coarse, bright, EGA-ish scene: sky, ground, and a few blocks.
            let (r, g, b) = if y < h / 2 {
                (0x55, 0x55 + (y * 90 / h) as u8, 0xFF)
            } else if (x / 40 + y / 40) % 2 == 0 {
                (0xAA, 0x55, 0x00)
            } else {
                (0x00, 0xAA, 0x55)
            };
            px[i] = r;
            px[i + 1] = g;
            px[i + 2] = b;
        }
    }
    px
}

fn save(cv: &Canvas, name: &str, over_game: bool) {
    let under = if over_game {
        fake_game(cv.w, cv.h)
    } else {
        vec![0u8; cv.w * cv.h * 3]
    };
    let mut rgb = Vec::with_capacity(cv.w * cv.h * 3);
    for (i, p) in cv.px.chunks_exact(4).enumerate() {
        // Composite onto what is behind, as the window would.
        let a = p[3] as f32 / 255.0;
        for c in 0..3 {
            let back = under[i * 3 + c] as f32;
            rgb.push((p[c] as f32 * a + back * (1.0 - a)) as u8);
        }
    }
    image::save_buffer(name, &rgb, cv.w as u32, cv.h as u32, image::ColorType::Rgb8).unwrap();
    println!("{name}");
}

fn shot(u: &mut Ui, dir: &str, name: &str, over_game: bool) {
    let mut cv = Canvas::new(1280, 800);
    u.paint(&mut cv, over_game);
    save(&cv, &format!("{dir}/ui_{name}.png"), over_game);
}

fn main() {
    // The programs are the other executables a real bundle stages beside the game:
    // Prince of Persia ships its SETUP.EXE, Dungeon Master its INSTALL and STATS.
    let games: Vec<GameView> = [
        ("Zeliard", 3, &["INSTALL.EXE", "MTINIT.COM"][..]),
        ("Dungeon Master", 1, &["INSTALL.EXE", "STATS.EXE"][..]),
        ("Alley Cat", 0, &[][..]),
        ("Prince Of Persia", 0, &["SETUP.EXE"][..]),
        ("Kings Bounty", 0, &["KB!.COM"][..]),
        ("Mechwarrior", 0, &[][..]),
    ]
    .iter()
    .map(|(t, n, progs)| GameView {
        key: t.to_lowercase(),
        title: t.to_string(),
        saves: (0..*n)
            .map(|i| SaveView {
                when: format!("{} Jul 2026, 1{}:0{}", 12 - i, i, i),
                thumb: None,
            })
            .collect(),
        programs: progs.iter().map(|s| s.to_string()).collect(),
    })
    .collect();
    let dir = std::env::args().nth(1).unwrap();

    // The real splash, because the header is cut out of it: a preview drawn with no
    // logo is a preview of a different interface.
    let logo = Image::decode_png(include_bytes!("../../runtime/assets/saisei_logo.png"))
        .expect("the splash must decode");
    let mut u = Ui::new(logo, games);
    shot(&mut u, &dir, "library", false);

    u.key(Key::Enter); // open Zeliard
    shot(&mut u, &dir, "game", false);

    // Paused: the same page, in the same place, over the frozen frame.
    u.open_overlay(0, true);
    shot(&mut u, &dir, "paused", true);

    // The one screen with a control on it that is neither a button nor a list.
    u.screen = Screen::Settings;
    shot(&mut u, &dir, "settings", true);
    u.screen = Screen::Game;

    // ...and the library, reached from it. Same bar, same cards, same geometry —
    // the game is still sitting there, and the card says so.
    u.screen = Screen::Library;
    shot(&mut u, &dir, "paused_library", true);

    // Picking another game from it is the one thing here that ends the game you
    // paused, so it is the one thing that asks.
    u.key(Key::Right);
    u.key(Key::Enter);
    u.key(Key::Down); // Continue is dead (no saves) -> New game
    u.key(Key::Enter);
    shot(&mut u, &dir, "confirm_switch", true);
    u.key(Key::Escape);

    u.running = None;
    u.screen = Screen::Library;
    u.game = 0;
    u.key(Key::Delete);
    shot(&mut u, &dir, "confirm_delete", false);
    u.key(Key::Escape);

    // The "…" on a card, and the programs behind it. Zeliard is the cursor's game
    // and it has an installer, so both rows are on offer.
    u.open_menu(0);
    shot(&mut u, &dir, "menu", false);

    u.key(Key::Enter); // Run a file
    shot(&mut u, &dir, "run_file", false);
    u.key(Key::Escape);
    u.key(Key::Escape);

    u.screen = Screen::AddGame;
    shot(&mut u, &dir, "add", false);
}
