//! Painting the interface.
//!
//! The game page is drawn into a rect, which is what lets the library show it
//! full-window and the overlay show it inside a panel over the frozen game
//! without a second implementation.

use crate::canvas::{Canvas, Color, Rect};
use crate::font::Weight;
use crate::theme as t;
use crate::{Focus, Hit, Screen, Ui};

/// Everything is sized off the window height, so the interface keeps its
/// proportions from a small window to a 4K one instead of turning into a few
/// enormous or unreadably tiny widgets.
fn scale(cv: &Canvas) -> f32 {
    (cv.h as f32 / 800.0).clamp(0.62, 2.4)
}

pub fn paint(ui: &mut Ui, cv: &mut Canvas, over_game: bool) {
    let s = scale(cv);
    let pad = 44.0 * s;

    if over_game {
        // The frozen frame is already on screen underneath; lay a scrim over it so
        // the menu is unmistakably in front, but you can still see where you were.
        cv.clear(Color(0, 0, 0, 0));
        cv.fill(Rect::new(0.0, 0.0, cv.w as f32, cv.h as f32), t::SCRIM);
    } else {
        cv.clear(t::BG);
        // A slow wash of petal colour off the top-left, so the page is not a flat
        // black field.
        cv.gradient_v(
            Rect::new(0.0, 0.0, cv.w as f32, cv.h as f32 * 0.55),
            t::ACCENT_DEEP.alpha(26),
            t::BG.alpha(0),
        );
    }

    let full = Rect::new(pad, pad, cv.w as f32 - 2.0 * pad, cv.h as f32 - 2.0 * pad);

    match ui.screen {
        Screen::Library => paint_library(ui, cv, full, s),
        Screen::Game => {
            if over_game {
                // Centre the page in a panel rather than letting it sprawl over the
                // whole frozen frame.
                let w = (1080.0 * s).min(full.w);
                let h = (620.0 * s).min(full.h);
                let panel = Rect::new((cv.w as f32 - w) / 2.0, (cv.h as f32 - h) / 2.0, w, h);
                cv.rounded(panel, t::RADIUS * 1.6, t::SURFACE.alpha(250));
                cv.stroke(panel, t::RADIUS * 1.6, 1.0, t::BORDER);
                paint_game(ui, cv, panel.inset(32.0 * s), s);
            } else {
                paint_game(ui, cv, full, s);
            }
        }
        Screen::Settings => paint_settings(ui, cv, full, s, over_game),
        Screen::AddGame => paint_add(ui, cv, full, s),
    }
}

// ---- library ----------------------------------------------------------------

fn paint_library(ui: &mut Ui, cv: &mut Canvas, r: Rect, s: f32) {
    ui.fonts
        .draw_top(cv, "Saisei", r.x, r.y, Weight::Bold, 30.0 * s, t::TEXT);
    let sub = if ui.games.is_empty() {
        "No games yet".to_string()
    } else if ui.games.len() == 1 {
        "1 game".to_string()
    } else {
        format!("{} games", ui.games.len())
    };
    ui.fonts.draw_top(
        cv,
        &sub,
        r.x,
        r.y + 38.0 * s,
        Weight::Regular,
        15.0 * s,
        t::TEXT_DIM,
    );

    // Add game: a real button, in the header, always on screen.
    //
    // It used to be the last tile of the grid, which meant that the more games you
    // had the further it slid down — and past a screenful it was off the bottom
    // entirely, so the one way to add a game was the one thing you couldn't see.
    // Up here it is never scrolled away and never competes with the games.
    let add_sel = ui.game == ui.games.len();
    let label = "+  Add game";
    let bw = ui.fonts.width(label, Weight::Bold, 15.0 * s) + 34.0 * s;
    let btn = Rect::new(r.x + r.w - bw, r.y + 2.0 * s, bw, 42.0 * s);
    cv.rounded(
        btn,
        21.0 * s,
        if add_sel { t::ACCENT } else { t::SURFACE_HI },
    );
    cv.stroke(
        btn,
        21.0 * s,
        1.0,
        if add_sel { t::ACCENT } else { t::BORDER },
    );
    ui.fonts.draw_centered(
        cv,
        label,
        btn.x,
        btn.w,
        btn.y + 11.0 * s,
        Weight::Bold,
        15.0 * s,
        if add_sel {
            Color::rgb(0x1A, 0x0C, 0x14)
        } else {
            t::TEXT
        },
    );
    ui.push_hot(btn, Hit::Add);

    let top = r.y + 84.0 * s;
    let gap = 22.0 * s;
    let card_w = 250.0 * s;
    let cover_h = card_w * 0.625; // 16:10
    let card_h = cover_h + 52.0 * s;

    let cols = (((r.w + gap) / (card_w + gap)).floor() as usize).max(1);
    ui.cols = cols;

    let n = ui.games.len();
    if n == 0 {
        ui.fonts.draw_top(
            cv,
            "Nothing here yet.",
            r.x,
            top + 20.0 * s,
            Weight::Bold,
            20.0 * s,
            t::TEXT_DIM,
        );
        ui.fonts.draw_top(
            cv,
            "Drop a game's zip on this window, or choose Add game.",
            r.x,
            top + 52.0 * s,
            Weight::Regular,
            15.0 * s,
            t::TEXT_OFF,
        );
        hint(ui, cv, r, s, "Enter add a game    Esc quit");
        return;
    }
    let rows = n.div_ceil(cols);

    // Scroll by whole rows, keeping the cursor on screen.
    let avail = (r.y + r.h - 30.0 * s) - top; // 30: the hint line at the foot
    let per_screen = (((avail + gap) / (card_h + gap)).floor() as usize).clamp(1, rows);
    // Only follow the cursor while it is in the grid. On the Add button it is not
    // in any row, and dragging the grid to wherever the last game happens to be
    // would scroll the library for no reason the player can see.
    if ui.game < n {
        let sel_row = ui.game / cols;
        if sel_row < ui.scroll {
            ui.scroll = sel_row;
        } else if sel_row >= ui.scroll + per_screen {
            ui.scroll = sel_row + 1 - per_screen;
        }
    }
    ui.scroll = ui.scroll.min(rows.saturating_sub(per_screen));

    for i in 0..n {
        let (row, col) = (i / cols, i % cols);
        if row < ui.scroll || row >= ui.scroll + per_screen {
            continue;
        }
        let x = r.x + col as f32 * (card_w + gap);
        let y = top + (row - ui.scroll) as f32 * (card_h + gap);
        let card = Rect::new(x, y, card_w, card_h);
        paint_card(ui, cv, card, i, ui.game == i, s, cover_h);
        ui.push_hot(card, Hit::Game(i));
    }

    if rows > per_screen {
        let of = format!("{} of {rows} rows", ui.scroll + per_screen);
        let w = ui.fonts.width(&of, Weight::Regular, 12.5 * s);
        ui.fonts.draw_top(
            cv,
            &of,
            r.x + r.w - w,
            r.y + 54.0 * s,
            Weight::Regular,
            12.5 * s,
            t::TEXT_OFF,
        );
    }

    hint(
        ui,
        cv,
        r,
        s,
        "Arrows move    Enter open    Esc quit    \u{2022}    or drop a game's zip on this window",
    );
}

fn paint_card(ui: &mut Ui, cv: &mut Canvas, card: Rect, i: usize, sel: bool, s: f32, cover_h: f32) {
    let bg = if sel { t::SURFACE_HI } else { t::SURFACE };
    cv.rounded(card, t::RADIUS, bg);

    let cover = Rect::new(card.x, card.y, card.w, cover_h);
    // The most recent save's thumbnail is the game's cover: what you were last
    // looking at is a better picture of "your" game than any box art.
    let thumb = ui.games[i].saves.first().and_then(|sv| sv.thumb.as_ref());
    match thumb {
        Some(img) => {
            let img = img.clone();
            cv.image_fit(&img, cover, 255);
        }
        None => {
            cv.gradient_v(cover, t::ACCENT_DEEP.alpha(90), t::SURFACE.alpha(255));
            // A big, quiet initial, so an unplayed game still has a face.
            let ch: String = ui.games[i]
                .title
                .chars()
                .next()
                .map(|c| c.to_string())
                .unwrap_or_default();
            let px = cover_h * 0.62;
            let w = ui.fonts.width(&ch, Weight::Bold, px);
            let asc = ui.fonts.ascent(Weight::Bold, px);
            ui.fonts.draw(
                cv,
                &ch,
                cover.x + (cover.w - w) / 2.0,
                cover.y + (cover.h + asc * 0.72) / 2.0,
                Weight::Bold,
                px,
                t::ACCENT.alpha(60),
            );
        }
    }

    let title = ui.fonts.elide(
        &ui.games[i].title,
        Weight::Bold,
        16.0 * s,
        card.w - 24.0 * s,
    );
    ui.fonts.draw_top(
        cv,
        &title,
        card.x + 12.0 * s,
        cover.y + cover.h + 10.0 * s,
        Weight::Bold,
        16.0 * s,
        if sel { t::TEXT } else { t::TEXT.alpha(220) },
    );
    let saves = ui.games[i].saves.len();
    let sub = match saves {
        0 => "Not played".to_string(),
        1 => "1 save".to_string(),
        n => format!("{n} saves"),
    };
    ui.fonts.draw_top(
        cv,
        &sub,
        card.x + 12.0 * s,
        cover.y + cover.h + 30.0 * s,
        Weight::Regular,
        12.5 * s,
        t::TEXT_DIM,
    );

    if sel {
        cv.stroke(card, t::RADIUS, 2.0, t::ACCENT);
    }
}

// ---- a game's page (and the overlay) ----------------------------------------

fn paint_game(ui: &mut Ui, cv: &mut Canvas, r: Rect, s: f32) {
    let Some(g) = ui.games.get(ui.game) else {
        return;
    };
    let title = g.title.clone();
    let saves = g.saves.len();

    let col_w = (300.0 * s).min(r.w * 0.42);
    ui.fonts
        .draw_top(cv, &title, r.x, r.y, Weight::Bold, 34.0 * s, t::TEXT);
    let sub = if ui.in_game {
        "Paused".to_string()
    } else if saves == 0 {
        "Not played yet".to_string()
    } else {
        format!("{saves} save{}", if saves == 1 { "" } else { "s" })
    };
    ui.fonts.draw_top(
        cv,
        &sub,
        r.x,
        r.y + 42.0 * s,
        Weight::Regular,
        14.5 * s,
        if ui.in_game { t::ACCENT } else { t::TEXT_DIM },
    );

    // Actions.
    let mut y = r.y + 86.0 * s;
    let bh = 46.0 * s;
    let labels = ui.actions();
    for (i, label) in labels.iter().enumerate() {
        let row = Rect::new(r.x, y, col_w, bh);
        let enabled = ui.action_enabled(i);
        let sel = ui.focus == Focus::Actions && ui.action == i;
        let (bg, fg) = match (enabled, sel) {
            (false, _) => (t::SURFACE.alpha(110), t::TEXT_OFF),
            (true, true) => (t::ACCENT, Color::rgb(0x1A, 0x0C, 0x14)),
            (true, false) => (t::SURFACE_HI, t::TEXT),
        };
        cv.rounded(row, t::RADIUS, bg);
        if sel && enabled {
            cv.stroke(row, t::RADIUS, 1.0, t::ACCENT);
        }
        ui.fonts.draw_top(
            cv,
            label,
            row.x + 16.0 * s,
            row.y + (bh - 20.0 * s) / 2.0,
            Weight::Bold,
            16.0 * s,
            fg,
        );
        ui.push_hot(row, Hit::Action(i));
        y += bh + 10.0 * s;
    }

    if let Some(msg) = ui.message.clone() {
        ui.fonts.draw_top(
            cv,
            &msg,
            r.x,
            y + 6.0 * s,
            Weight::Regular,
            14.0 * s,
            t::ACCENT,
        );
    }

    // Saves.
    let px = r.x + col_w + 34.0 * s;
    let pw = r.x + r.w - px;
    if pw > 120.0 * s {
        paint_saves(ui, cv, Rect::new(px, r.y + 4.0, pw, r.h - 4.0), s);
    }

    let h = if ui.in_game {
        "Arrows move    Enter choose    F12 or Esc resume"
    } else {
        "Arrows move    Enter choose    Esc back"
    };
    hint(ui, cv, r, s, h);
}

fn paint_saves(ui: &mut Ui, cv: &mut Canvas, r: Rect, s: f32) {
    ui.fonts
        .draw_top(cv, "Saves", r.x, r.y, Weight::Bold, 15.0 * s, t::TEXT_DIM);

    let saves = ui.games[ui.game].saves.len();
    if saves == 0 {
        ui.fonts.draw_top(
            cv,
            "No saves yet.",
            r.x,
            r.y + 32.0 * s,
            Weight::Regular,
            14.0 * s,
            t::TEXT_OFF,
        );
        ui.fonts.draw_top(
            cv,
            if ui.in_game {
                "Choose Save to keep this moment."
            } else {
                "Start a new game, then press F12 to save."
            },
            r.x,
            r.y + 54.0 * s,
            Weight::Regular,
            14.0 * s,
            t::TEXT_OFF,
        );
        return;
    }

    let row_h = 64.0 * s;
    let gap = 8.0 * s;
    let top = r.y + 30.0 * s;
    let max_rows = (((r.h - 30.0 * s) / (row_h + gap)).floor() as usize).max(1);

    // Keep the cursor on screen without a real scrollbar: page the window of rows.
    let first = if ui.save >= max_rows {
        ui.save + 1 - max_rows
    } else {
        0
    };

    for i in first..saves.min(first + max_rows) {
        let y = top + (i - first) as f32 * (row_h + gap);
        let row = Rect::new(r.x, y, r.w, row_h);
        let sel = ui.focus == Focus::Saves && ui.save == i;
        cv.rounded(row, t::RADIUS, if sel { t::SURFACE_HI } else { t::SURFACE });
        if sel {
            cv.stroke(row, t::RADIUS, 2.0, t::ACCENT);
        }

        // The frame the game was showing when it was saved.
        let tw = row_h * 1.6;
        let thumb_box = Rect::new(row.x + 6.0 * s, row.y + 6.0 * s, tw, row_h - 12.0 * s);
        match ui.games[ui.game].saves[i].thumb.as_ref() {
            Some(img) => {
                let img = img.clone();
                cv.image_fit(&img, thumb_box, 255);
            }
            None => {
                cv.rounded(thumb_box, 4.0, t::BG);
            }
        }

        let tx = thumb_box.x + thumb_box.w + 12.0 * s;
        let when = ui.games[ui.game].saves[i].when.clone();
        let when = ui.fonts.elide(
            &when,
            Weight::Regular,
            14.5 * s,
            row.w - (tx - row.x) - 12.0 * s,
        );
        ui.fonts.draw_top(
            cv,
            &when,
            tx,
            row.y + 14.0 * s,
            Weight::Regular,
            14.5 * s,
            t::TEXT,
        );
        if i == 0 {
            ui.fonts.draw_top(
                cv,
                "Newest",
                tx,
                row.y + 34.0 * s,
                Weight::Bold,
                11.5 * s,
                t::ACCENT.alpha(200),
            );
        }
        ui.push_hot(row, Hit::Save(i));
    }

    if saves > max_rows {
        let more = format!("{saves} saves");
        let w = ui.fonts.width(&more, Weight::Regular, 12.0 * s);
        ui.fonts.draw_top(
            cv,
            &more,
            r.x + r.w - w,
            r.y,
            Weight::Regular,
            12.0 * s,
            t::TEXT_OFF,
        );
    }
}

// ---- settings ---------------------------------------------------------------

fn paint_settings(ui: &mut Ui, cv: &mut Canvas, r: Rect, s: f32, over_game: bool) {
    let r = if over_game {
        let w = (620.0 * s).min(r.w);
        let h = (300.0 * s).min(r.h);
        let p = Rect::new((cv.w as f32 - w) / 2.0, (cv.h as f32 - h) / 2.0, w, h);
        cv.rounded(p, t::RADIUS * 1.6, t::SURFACE.alpha(250));
        cv.stroke(p, t::RADIUS * 1.6, 1.0, t::BORDER);
        p.inset(30.0 * s)
    } else {
        r
    };
    ui.fonts
        .draw_top(cv, "Settings", r.x, r.y, Weight::Bold, 30.0 * s, t::TEXT);
    ui.fonts.draw_top(
        cv,
        "Nothing to set yet.",
        r.x,
        r.y + 52.0 * s,
        Weight::Regular,
        15.0 * s,
        t::TEXT_DIM,
    );
    hint(ui, cv, r, s, "Esc back");
}

// ---- add a game -------------------------------------------------------------

fn paint_add(ui: &mut Ui, cv: &mut Canvas, r: Rect, s: f32) {
    ui.fonts
        .draw_top(cv, "Add a game", r.x, r.y, Weight::Bold, 30.0 * s, t::TEXT);

    // Once a bundle is unpacked, the only question left is which file is the game.
    if !ui.add.exes.is_empty() {
        ui.fonts.draw_top(
            cv,
            "Which one starts the game?",
            r.x,
            r.y + 46.0 * s,
            Weight::Regular,
            15.0 * s,
            t::TEXT_DIM,
        );
        let row_h = 40.0 * s;
        let w = (460.0 * s).min(r.w);
        for i in 0..ui.add.exes.len() {
            let y = r.y + 86.0 * s + i as f32 * (row_h + 6.0 * s);
            if y + row_h > r.y + r.h - 40.0 * s {
                break;
            }
            let row = Rect::new(r.x, y, w, row_h);
            let sel = ui.add.exe_idx == i;
            cv.rounded(row, 8.0, if sel { t::ACCENT } else { t::SURFACE_HI });
            let name = ui.add.exes[i].clone();
            ui.fonts.draw_top(
                cv,
                &name,
                row.x + 14.0 * s,
                row.y + (row_h - 18.0 * s) / 2.0,
                Weight::Bold,
                15.0 * s,
                if sel {
                    Color::rgb(0x1A, 0x0C, 0x14)
                } else {
                    t::TEXT
                },
            );
            ui.push_hot(row, Hit::Exe(i));
        }
        hint(ui, cv, r, s, "Arrows move    Enter choose    Esc cancel");
        return;
    }

    ui.fonts.draw_top(
        cv,
        "Drop a game's zip or folder anywhere on this window.",
        r.x,
        r.y + 46.0 * s,
        Weight::Regular,
        15.0 * s,
        t::TEXT_DIM,
    );

    // The drop target: a dashed-looking well that says what it wants.
    let well = Rect::new(r.x, r.y + 90.0 * s, (620.0 * s).min(r.w), 150.0 * s);
    cv.rounded(well, t::RADIUS, t::SURFACE.alpha(150));
    cv.stroke(well, t::RADIUS, 1.0, t::BORDER);
    ui.fonts.draw_centered(
        cv,
        "Drop a zip here",
        well.x,
        well.w,
        well.y + well.h / 2.0 - 16.0 * s,
        Weight::Bold,
        17.0 * s,
        t::TEXT_DIM,
    );

    // ...or type a URL.
    let fy = well.y + well.h + 26.0 * s;
    ui.fonts.draw_top(
        cv,
        "or paste a link to one",
        r.x,
        fy,
        Weight::Regular,
        14.0 * s,
        t::TEXT_DIM,
    );
    let field = Rect::new(r.x, fy + 26.0 * s, well.w, 44.0 * s);
    cv.rounded(field, 8.0, t::SURFACE_HI);
    cv.stroke(field, 8.0, 1.0, t::ACCENT.alpha(160));
    let shown = if ui.add.url.is_empty() {
        "https://…".to_string()
    } else {
        ui.add.url.clone()
    };
    let dim = ui.add.url.is_empty();
    let shown = ui
        .fonts
        .elide(&shown, Weight::Regular, 15.0 * s, field.w - 28.0 * s);
    let end = ui.fonts.draw_top(
        cv,
        &shown,
        field.x + 14.0 * s,
        field.y + 12.0 * s,
        Weight::Regular,
        15.0 * s,
        if dim { t::TEXT_OFF } else { t::TEXT },
    );
    if !dim {
        // A caret, so the field reads as something you are typing into.
        cv.fill(
            Rect::new(end + 2.0, field.y + 11.0 * s, 2.0, 21.0 * s),
            t::ACCENT,
        );
    }

    if let Some(st) = ui.add.status.clone() {
        ui.fonts.draw_top(
            cv,
            &st,
            r.x,
            field.y + field.h + 18.0 * s,
            Weight::Regular,
            14.5 * s,
            if ui.add.busy { t::TEXT_DIM } else { t::ACCENT },
        );
    }

    hint(ui, cv, r, s, "Enter fetch    Esc back");
}

// ---- shared -----------------------------------------------------------------

fn hint(ui: &mut Ui, cv: &mut Canvas, r: Rect, s: f32, text: &str) {
    ui.fonts.draw_top(
        cv,
        text,
        r.x,
        r.y + r.h - 20.0 * s,
        Weight::Regular,
        13.0 * s,
        t::TEXT_OFF,
    );
}
