//! Render the UI screens to PNGs so they can be looked at without a window.
use saisei_ui::*;

fn save(cv: &Canvas, name: &str) {
    let mut rgb = Vec::with_capacity(cv.w * cv.h * 3);
    for p in cv.px.chunks_exact(4) {
        // Composite onto black, as the window would.
        let a = p[3] as f32 / 255.0;
        for c in 0..3 {
            rgb.push((p[c] as f32 * a) as u8);
        }
    }
    image::save_buffer(name, &rgb, cv.w as u32, cv.h as u32, image::ColorType::Rgb8).unwrap();
    println!("{name}");
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

    let mut u = Ui::new(Image::default(), games);
    let mut cv = Canvas::new(1280, 800);
    u.paint(&mut cv, false);
    save(&cv, &format!("{dir}/ui_library.png"));

    u.key(Key::Enter); // open Zeliard
    let mut cv = Canvas::new(1280, 800);
    u.paint(&mut cv, false);
    save(&cv, &format!("{dir}/ui_game.png"));

    u.open_overlay(0, true);
    let mut cv = Canvas::new(1280, 800);
    u.paint(&mut cv, true);
    save(&cv, &format!("{dir}/ui_overlay.png"));

    u.screen = Screen::Library;
    u.in_game = false;
    u.key(Key::Delete);
    let mut cv = Canvas::new(1280, 800);
    u.paint(&mut cv, false);
    save(&cv, &format!("{dir}/ui_delete.png"));
    u.key(Key::Escape);

    // The "…" on a card, and the programs behind it. Zeliard is the cursor's game
    // and it has an installer, so both rows are on offer.
    u.open_menu(0);
    let mut cv = Canvas::new(1280, 800);
    u.paint(&mut cv, false);
    save(&cv, &format!("{dir}/ui_menu.png"));

    u.key(Key::Enter); // Run a file
    let mut cv = Canvas::new(1280, 800);
    u.paint(&mut cv, false);
    save(&cv, &format!("{dir}/ui_run_file.png"));
    u.key(Key::Escape);
    u.key(Key::Escape);

    u.screen = Screen::AddGame;
    let mut cv = Canvas::new(1280, 800);
    u.paint(&mut cv, false);
    save(&cv, &format!("{dir}/ui_add.png"));
}
