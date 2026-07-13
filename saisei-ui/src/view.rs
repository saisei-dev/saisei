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

    // The three things that open over the library. Each is modal: everything
    // behind it stops being clickable, not just stops being drawn. The library's
    // hit rects are pushed as it paints, so they are dropped again after — or a
    // click meant for the sheet could fall through onto a card.
    if ui.deleting.is_some() || ui.menu.is_some() || ui.picking.is_some() {
        paint_library(ui, cv, full, s);
        ui.hot.clear();
        let screen = cv_rect(cv);
        if ui.deleting.is_some() {
            paint_confirm_delete(ui, cv, screen, s);
        } else if ui.menu.is_some() {
            paint_card_menu(ui, cv, screen, s);
        } else {
            paint_pick_program(ui, cv, screen, s);
        }
        return;
    }

    match ui.screen {
        Screen::Library => paint_library(ui, cv, full, s),
        // The game page and settings are two pages of one surface. In the overlay
        // they share a single panel, drawn once, here — stepping into Settings
        // must not resize the thing you are looking at underneath your cursor.
        Screen::Game | Screen::Settings => {
            let inner = if over_game {
                overlay_panel(cv, full, s)
            } else {
                full
            };
            if ui.screen == Screen::Game {
                paint_game(ui, cv, inner, s);
            } else {
                paint_settings(ui, cv, inner, s);
            }
        }
        Screen::AddGame => paint_add(ui, cv, full, s),
    }
}

/// The overlay's panel, and the rect its contents get.
///
/// The scrim over the frozen game is deliberately light, so the panel has to
/// carry the separation itself: a soft shadow and an opaque face, rather than
/// washing out the whole window.
fn overlay_panel(cv: &mut Canvas, full: Rect, s: f32) -> Rect {
    let w = (1080.0 * s).min(full.w);
    let h = (620.0 * s).min(full.h);
    let panel = Rect::new((cv.w as f32 - w) / 2.0, (cv.h as f32 - h) / 2.0, w, h);
    shadow(cv, panel, t::RADIUS * 1.6, 18.0 * s);
    cv.rounded(panel, t::RADIUS * 1.6, t::SURFACE);
    cv.stroke(panel, t::RADIUS * 1.6, 1.0, t::BORDER);
    panel.inset(32.0 * s)
}

fn cv_rect(cv: &Canvas) -> Rect {
    Rect::new(0.0, 0.0, cv.w as f32, cv.h as f32)
}

// ---- removing a game --------------------------------------------------------

/// The question asked before a game is removed.
///
/// It names what will go — the bundle *and* the saves — because both do, and
/// because a library is the only record you have of either. Cancel holds the
/// focus: the answer that destroys something should never be the one a stray
/// Enter gives you.
fn paint_confirm_delete(ui: &mut Ui, cv: &mut Canvas, screen: Rect, s: f32) {
    let Some(i) = ui.deleting else { return };
    let Some(g) = ui.games.get(i) else { return };
    let title = g.title.clone();
    let saves = g.saves.len();

    cv.fill(screen, t::SCRIM);

    let w = (520.0 * s).min(screen.w - 40.0 * s);
    let h = 230.0 * s;
    let d = Rect::new((screen.w - w) / 2.0, (screen.h - h) / 2.0, w, h);
    shadow(cv, d, t::RADIUS * 1.6, 20.0 * s);
    cv.rounded(d, t::RADIUS * 1.6, t::SURFACE);
    cv.stroke(d, t::RADIUS * 1.6, 1.0, t::BORDER);

    let x = d.x + 28.0 * s;
    ui.fonts.draw_top(
        cv,
        &format!("Remove {title}?"),
        x,
        d.y + 26.0 * s,
        Weight::Bold,
        22.0 * s,
        t::TEXT,
    );
    let detail = match saves {
        0 => "The game and its files will be deleted.".to_string(),
        1 => "The game, its files and 1 save will be deleted.".to_string(),
        n => format!("The game, its files and {n} saves will be deleted."),
    };
    ui.fonts.draw_top(
        cv,
        &detail,
        x,
        d.y + 62.0 * s,
        Weight::Regular,
        14.5 * s,
        t::TEXT_DIM,
    );
    ui.fonts.draw_top(
        cv,
        "This cannot be undone.",
        x,
        d.y + 84.0 * s,
        Weight::Regular,
        14.5 * s,
        t::TEXT_OFF,
    );

    // Buttons, bottom-right: Cancel then Remove, so the safe one is nearer.
    let bh = 42.0 * s;
    let by = d.y + d.h - bh - 24.0 * s;
    let rm_w = ui.fonts.width("Remove", Weight::Bold, 15.0 * s) + 40.0 * s;
    let ca_w = ui.fonts.width("Cancel", Weight::Bold, 15.0 * s) + 40.0 * s;
    let rm = Rect::new(d.x + d.w - 28.0 * s - rm_w, by, rm_w, bh);
    let ca = Rect::new(rm.x - 10.0 * s - ca_w, by, ca_w, bh);

    let yes = ui.delete_yes;
    // The destructive button carries the warning colour only when it is the one
    // that is actually armed.
    cv.rounded(rm, 8.0, if yes { t::DANGER } else { t::SURFACE_HI });
    cv.stroke(rm, 8.0, 1.0, if yes { t::DANGER } else { t::BORDER });
    ui.fonts.draw_centered(
        cv,
        "Remove",
        rm.x,
        rm.w,
        rm.y + 11.0 * s,
        Weight::Bold,
        15.0 * s,
        if yes {
            Color::rgb(0xFF, 0xF2, 0xF5)
        } else {
            t::TEXT
        },
    );
    ui.push_hot(rm, Hit::ConfirmYes);

    cv.rounded(ca, 8.0, if yes { t::SURFACE_HI } else { t::ACCENT });
    cv.stroke(ca, 8.0, 1.0, if yes { t::BORDER } else { t::ACCENT });
    ui.fonts.draw_centered(
        cv,
        "Cancel",
        ca.x,
        ca.w,
        ca.y + 11.0 * s,
        Weight::Bold,
        15.0 * s,
        if yes {
            t::TEXT
        } else {
            Color::rgb(0x1A, 0x0C, 0x14)
        },
    );
    ui.push_hot(ca, Hit::ConfirmNo);

    ui.fonts.draw_top(
        cv,
        "Arrows choose    Enter confirm    Esc cancel",
        x,
        d.y + 116.0 * s,
        Weight::Regular,
        13.0 * s,
        t::TEXT_OFF,
    );
}

// ---- the card menu, and the programs on a game's disk -----------------------

/// A centred sheet: a title, a list of rows, a hint. The "…" menu and the list of
/// programs are both this, and there is no reason for them to be two layouts.
/// Returns the row rects, in order, for the caller to hang its hits on.
#[allow(clippy::too_many_arguments)]
fn sheet(
    ui: &mut Ui,
    cv: &mut Canvas,
    screen: Rect,
    s: f32,
    title: &str,
    subtitle: Option<&str>,
    rows: &[String],
    sel: usize,
    hint: &str,
) -> Vec<Rect> {
    cv.fill(screen, t::SCRIM);

    let rh = 46.0 * s;
    let top = if subtitle.is_some() {
        96.0 * s
    } else {
        70.0 * s
    };
    let w = (480.0 * s).min(screen.w - 40.0 * s);
    let h = top + rows.len() as f32 * rh + 54.0 * s;
    let d = Rect::new((screen.w - w) / 2.0, (screen.h - h) / 2.0, w, h);
    shadow(cv, d, t::RADIUS * 1.6, 20.0 * s);
    cv.rounded(d, t::RADIUS * 1.6, t::SURFACE);
    cv.stroke(d, t::RADIUS * 1.6, 1.0, t::BORDER);

    let x = d.x + 24.0 * s;
    let inner = d.w - 48.0 * s;

    let title = ui.fonts.elide(title, Weight::Bold, 20.0 * s, inner);
    ui.fonts.draw_top(
        cv,
        &title,
        x,
        d.y + 24.0 * s,
        Weight::Bold,
        20.0 * s,
        t::TEXT,
    );
    if let Some(sub) = subtitle {
        let sub = ui.fonts.elide(sub, Weight::Regular, 13.5 * s, inner);
        ui.fonts.draw_top(
            cv,
            &sub,
            x,
            d.y + 58.0 * s,
            Weight::Regular,
            13.5 * s,
            t::TEXT_DIM,
        );
    }

    let mut out = Vec::with_capacity(rows.len());
    for (i, label) in rows.iter().enumerate() {
        let r = Rect::new(
            d.x + 12.0 * s,
            d.y + top + i as f32 * rh,
            d.w - 24.0 * s,
            rh - 6.0 * s,
        );
        let on = i == sel;
        if on {
            cv.rounded(r, 8.0, t::SURFACE_HI);
            cv.stroke(r, 8.0, 1.0, t::ACCENT);
        }
        let label = ui
            .fonts
            .elide(label, Weight::Regular, 15.0 * s, r.w - 26.0 * s);
        ui.fonts.draw_top(
            cv,
            &label,
            r.x + 13.0 * s,
            r.y + 11.0 * s,
            Weight::Regular,
            15.0 * s,
            if on { t::TEXT } else { t::TEXT_DIM },
        );
        out.push(r);
    }

    ui.fonts.draw_top(
        cv,
        hint,
        x,
        d.y + d.h - 32.0 * s,
        Weight::Regular,
        13.0 * s,
        t::TEXT_OFF,
    );
    out
}

/// Everything you can do to a game that is not playing it.
fn paint_card_menu(ui: &mut Ui, cv: &mut Canvas, screen: Rect, s: f32) {
    let Some(i) = ui.menu else { return };
    let Some(g) = ui.games.get(i) else { return };
    let title = g.title.clone();
    let rows: Vec<String> = ui.menu_items(i).into_iter().map(str::to_string).collect();
    let sel = ui.menu_idx.min(rows.len().saturating_sub(1));

    let hits = sheet(
        ui,
        cv,
        screen,
        s,
        &title,
        None,
        &rows,
        sel,
        "Enter choose    Esc close",
    );
    for (i, r) in hits.into_iter().enumerate() {
        ui.push_hot(r, Hit::MenuItem(i));
    }
}

/// The other programs on the game's disk — its setup, its installer.
///
/// It says what running one does and does not do, because that is the whole
/// question a player has here: this boots the machine on that program instead of
/// the game, on the game's own disk, so what the setup writes is what the game
/// will read. It does not become what the game runs.
fn paint_pick_program(ui: &mut Ui, cv: &mut Canvas, screen: Rect, s: f32) {
    let Some(i) = ui.picking else { return };
    let Some(g) = ui.games.get(i) else { return };
    let sub = format!("{} — runs once, on the game's own disk.", g.title);
    let rows = g.programs.clone();
    let sel = ui.pick_idx.min(rows.len().saturating_sub(1));

    let hits = sheet(
        ui,
        cv,
        screen,
        s,
        "Run a file",
        Some(&sub),
        &rows,
        sel,
        "Enter run    Esc back",
    );
    for (i, r) in hits.into_iter().enumerate() {
        ui.push_hot(r, Hit::Program(i));
    }
}

/// Three dots — the button that means "and the rest". Drawn, like the trash, from
/// rectangles: there is no icon font here, and two glyphs are not worth becoming a
/// reason to add one.
fn dots_icon(cv: &mut Canvas, r: Rect, c: Color) {
    let d = (r.w * 0.16).max(1.5);
    let y = r.y + r.h / 2.0 - d / 2.0;
    for k in 0..3 {
        let x = r.x + r.w * (0.5 + 0.31 * (k as f32 - 1.0)) - d / 2.0;
        cv.rounded(Rect::new(x, y, d, d), d / 2.0, c);
    }
}

/// A soft drop shadow under `r`: concentric rounded rects, fading outward.
///
/// Cheap, and enough — at these sizes nobody can tell it from a real blur, and it
/// is what lifts the menu off the game now that the game behind it is bright
/// enough to see.
fn shadow(cv: &mut Canvas, r: Rect, radius: f32, spread: f32) {
    let steps = 8;
    for i in (1..=steps).rev() {
        let g = spread * i as f32 / steps as f32;
        let a = (26.0 * (1.0 - i as f32 / steps as f32).powf(0.7)) as u8;
        cv.rounded(
            Rect::new(r.x - g, r.y - g + g * 0.35, r.w + 2.0 * g, r.h + 2.0 * g),
            radius + g,
            Color(0, 0, 0, a.max(6)),
        );
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

        // The "…", on the card the cursor is on — and only that one, so a library
        // of covers is not also a row of little buttons to misclick. Hovering a
        // card selects it, so with a mouse this is "on hover"; with the keyboard it
        // follows the selection, which is where Delete would act anyway.
        //
        // One button, not two. It used to be a trash can, and adding a second icon
        // beside it would have been exactly the row of little buttons that comment
        // was written to prevent — so removing a game moved *into* the menu, where
        // running a file also lives. Delete still opens the same question directly.
        let d = 30.0 * s;
        let btn = Rect::new(card.x + card.w - d - 8.0 * s, card.y + 8.0 * s, d, d);
        cv.rounded(btn, d / 2.0, t::BG.alpha(235));
        cv.stroke(btn, d / 2.0, 1.0, t::BORDER);
        dots_icon(cv, btn, t::TEXT);
        ui.push_hot(btn, Hit::Menu(i));
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

fn paint_settings(ui: &mut Ui, cv: &mut Canvas, r: Rect, s: f32) {
    let title = ui
        .games
        .get(ui.game)
        .map(|g| g.title.clone())
        .unwrap_or_default();
    ui.fonts
        .draw_top(cv, "Settings", r.x, r.y, Weight::Bold, 30.0 * s, t::TEXT);
    // Volume is per game, so say whose. A slider that silently meant something
    // different depending on where you opened it from would be a trap.
    ui.fonts.draw_top(
        cv,
        &title,
        r.x,
        r.y + 42.0 * s,
        Weight::Regular,
        14.5 * s,
        t::ACCENT,
    );

    let col_w = (380.0 * s).min(r.w);
    paint_volume(ui, cv, Rect::new(r.x, r.y + 90.0 * s, col_w, 44.0 * s), s);

    ui.fonts.draw_top(
        cv,
        "Remembered for this game.",
        r.x,
        r.y + 152.0 * s,
        Weight::Regular,
        13.0 * s,
        t::TEXT_OFF,
    );

    hint(ui, cv, r, s, "Left/Right or wheel adjusts    Esc back");
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
    // Paste, as a button. Ctrl+V does it too, but a link is the one thing here you
    // are most likely to have on the clipboard rather than in your head, and a
    // chord you have to know about is not an interface.
    let paste_w = ui.fonts.width("Paste", Weight::Bold, 14.5 * s) + 30.0 * s;
    let field = Rect::new(r.x, fy + 26.0 * s, well.w - paste_w - 8.0 * s, 44.0 * s);
    let paste = Rect::new(field.x + field.w + 8.0 * s, field.y, paste_w, field.h);
    cv.rounded(paste, 8.0, t::SURFACE_HI);
    cv.stroke(paste, 8.0, 1.0, t::BORDER);
    ui.fonts.draw_centered(
        cv,
        "Paste",
        paste.x,
        paste.w,
        paste.y + 12.0 * s,
        Weight::Bold,
        14.5 * s,
        t::TEXT,
    );
    ui.push_hot(paste, Hit::Paste);

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
        .elide_front(&shown, Weight::Regular, 15.0 * s, field.w - 28.0 * s);
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

    hint(ui, cv, r, s, "Ctrl+V paste    Enter fetch    Esc back");
}

// ---- shared -----------------------------------------------------------------

/// The volume slider: a label, a track, a filled portion, and a knob.
///
/// The track rect is what gets registered as the hit target *and* what rides in
/// the `Hit`, because for a slider "where you clicked" is the whole message —
/// the input layer has to be able to turn an x back into a value, and the only
/// honest source for that mapping is the geometry that was actually painted.
fn paint_volume(ui: &mut Ui, cv: &mut Canvas, r: Rect, s: f32) {
    let v = ui.volume.clamp(0.0, 1.0);

    let label_h = 16.0 * s;
    ui.fonts
        .draw_top(cv, "Volume", r.x, r.y, Weight::Bold, 13.0 * s, t::ACCENT);
    let pct = format!("{}%", (v * 100.0).round() as i32);
    let pw = ui.fonts.width(&pct, Weight::Regular, 13.0 * s);
    ui.fonts.draw_top(
        cv,
        &pct,
        r.x + r.w - pw,
        r.y,
        Weight::Regular,
        13.0 * s,
        t::TEXT_DIM,
    );

    // The track is inset by the knob's radius at each end, so that a knob at 0%
    // and a knob at 100% both sit fully inside the widget instead of hanging off.
    let knob_r = 9.0 * s;
    let th = 6.0 * s;
    let ty = r.y + label_h + 12.0 * s;
    let track = Rect::new(r.x + knob_r, ty, (r.w - knob_r * 2.0).max(1.0), th);

    cv.rounded(track, th / 2.0, t::SURFACE_HI);
    let filled = Rect::new(track.x, track.y, track.w * v, th);
    cv.rounded(filled, th / 2.0, t::ACCENT);

    let kx = track.x + track.w * v;
    let knob = Rect::new(
        kx - knob_r,
        ty + th / 2.0 - knob_r,
        knob_r * 2.0,
        knob_r * 2.0,
    );
    cv.rounded(knob, knob_r, t::TEXT);
    cv.stroke(knob, knob_r, 2.0 * s, t::ACCENT);

    // Hit area is generous vertically — a 6px track is not something anyone
    // should have to aim at — but the value always comes from the track's own x.
    let grab = Rect::new(r.x, r.y, r.w, r.h);
    ui.push_hot(grab, Hit::Volume(track));
}

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

#[cfg(test)]
mod volume_tests {
    use crate::{Action, Canvas, GameView, Image, Key, Screen, Ui};

    /// The overlay, stepped into its Settings page — where the slider lives.
    fn ui() -> Ui {
        let logo = Image {
            w: 1,
            h: 1,
            rgb: vec![0, 0, 0],
        };
        let mut ui = Ui::new(
            logo,
            vec![GameView {
                key: "zeliard_dos_en".into(),
                title: "Zeliard".into(),
                saves: vec![],
                programs: vec![],
            }],
        );
        ui.open_overlay(0, true);
        ui.screen = Screen::Settings;
        ui.volume = 0.5;
        ui
    }

    /// Paint once so the hit rects exist — the slider's geometry is *defined* by
    /// the paint, so nothing can be clicked before it has been drawn.
    fn painted(ui: &mut Ui) -> Canvas {
        let mut cv = Canvas::new(1280, 800);
        ui.paint(&mut cv, true);
        cv
    }

    #[test]
    fn dragging_the_knob_sets_the_volume() {
        let mut ui = ui();
        let cv = painted(&mut ui);
        let _ = cv;
        // Find the track the paint registered, then click its far right.
        let track = ui
            .hot
            .iter()
            .find_map(|(_, h)| match h {
                crate::Hit::Volume(t) => Some(*t),
                _ => None,
            })
            .expect("the overlay must have a volume slider");

        let act = ui.click(track.x + track.w, track.y + track.h / 2.0);
        assert_eq!(act, Action::SetVolume(1.0));
        assert_eq!(ui.volume, 1.0);

        // Dragging left keeps tracking the pointer even off the end of the track.
        let act = ui.mouse_move(track.x - 500.0, track.y);
        assert_eq!(act, Action::SetVolume(0.0));
        assert_eq!(ui.volume, 0.0);

        // ...until the button comes up.
        ui.mouse_up();
        assert_eq!(ui.mouse_move(track.x + track.w, track.y), Action::None);
        assert_eq!(ui.volume, 0.0);
    }

    #[test]
    fn the_wheel_moves_it() {
        let mut ui = ui();
        painted(&mut ui);
        // Compared with a tolerance: these are accumulated f32s, and an exact
        // match would be testing IEEE rounding, not the slider.
        assert!(matches!(ui.wheel(1.0), Action::SetVolume(_)));
        assert!((ui.volume - 0.55).abs() < 1e-5, "{}", ui.volume);
        assert!(matches!(ui.wheel(-2.0), Action::SetVolume(_)));
        assert!((ui.volume - 0.45).abs() < 1e-5, "{}", ui.volume);

        // And it cannot be driven past the ends.
        for _ in 0..50 {
            ui.wheel(-1.0);
        }
        assert_eq!(ui.volume, 0.0);
        assert_eq!(ui.wheel(-1.0), Action::None);
    }

    #[test]
    fn arrows_move_it() {
        let mut ui = ui();
        painted(&mut ui);
        // Settings holds one control, so the arrows go straight to it.
        assert_eq!(ui.key(Key::Right), Action::SetVolume(0.55));
        assert_eq!(ui.key(Key::Left), Action::SetVolume(0.5));
        // Esc leaves, and does not move the value on the way out.
        ui.key(Key::Escape);
        assert_eq!(ui.screen, Screen::Game);
        assert_eq!(ui.volume, 0.5);
    }

    /// The overlay and its Settings page must be the same panel: stepping between
    /// them cannot resize the thing under the player's cursor.
    #[test]
    fn settings_shares_the_overlays_panel() {
        let mut ui = ui();
        ui.screen = Screen::Game;
        let mut a = Canvas::new(1280, 800);
        ui.paint(&mut a, true);

        ui.screen = Screen::Settings;
        let mut b = Canvas::new(1280, 800);
        ui.paint(&mut b, true);

        // The panel's face is opaque SURFACE; find its extent on the centre row
        // of each and require them to match.
        let extent = |cv: &Canvas| {
            let y = cv.h / 2;
            let row: Vec<bool> = (0..cv.w)
                .map(|x| cv.px[(y * cv.w + x) * 4 + 3] > 200)
                .collect();
            let l = row.iter().position(|&o| o);
            let r = row.iter().rposition(|&o| o);
            (l, r)
        };
        assert_eq!(
            extent(&a),
            extent(&b),
            "Settings must not resize the overlay panel"
        );
    }

    #[test]
    fn the_knob_is_actually_drawn_where_the_value_says() {
        let mut ui = ui();
        ui.volume = 1.0;
        let cv = painted(&mut ui);
        let track = ui
            .hot
            .iter()
            .find_map(|(_, h)| match h {
                crate::Hit::Volume(t) => Some(*t),
                _ => None,
            })
            .unwrap();
        let y = (track.y + track.h / 2.0) as usize;
        let at = |x: usize| {
            let i = (y * cv.w + x) * 4;
            (cv.px[i], cv.px[i + 1], cv.px[i + 2])
        };
        // At 100% the far end of the track is filled and the knob is on it.
        let right = at((track.x + track.w - 2.0) as usize);
        let left = at((track.x + 2.0) as usize);
        assert!(
            right.0 > 100,
            "track should be filled at full volume: {right:?}"
        );
        assert!(
            left.0 > 100,
            "track should be filled at full volume: {left:?}"
        );

        ui.volume = 0.0;
        let cv = painted(&mut ui);
        let at0 = |x: usize| {
            let i = (y * cv.w + x) * 4;
            (cv.px[i], cv.px[i + 1], cv.px[i + 2])
        };
        let right0 = at0((track.x + track.w - 2.0) as usize);
        assert!(
            right0.0 < 80,
            "track should be empty at zero volume: {right0:?}"
        );
    }
}
