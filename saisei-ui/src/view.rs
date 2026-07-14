//! Painting the interface.
//!
//! Every screen is laid out in the same rect — the window, less a margin — and
//! wears the same bar at the top: a way back, and a title saying where you are.
//! That is what lets the library, a game's page, the settings and the pause menu
//! be one interface rather than four, and it is what makes walking between them
//! hold still. Nothing here resizes, moves or reflows because of *where* it was
//! opened from; when a game is paused behind the page, the only thing that
//! changes is what is behind it.

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

/// The height of the bar's button row. The title sits below it, and the page
/// below that — the same distances on every screen, so the page never jumps.
const BAR_H: f32 = 40.0;
const TITLE_DROP: f32 = 18.0;
const SUB_DROP: f32 = 38.0;
const CONTENT_DROP: f32 = 30.0;

pub fn paint(ui: &mut Ui, cv: &mut Canvas, over_game: bool) {
    let s = scale(cv);
    let pad = 44.0 * s;
    backdrop(cv, over_game);

    let full = Rect::new(pad, pad, cv.w as f32 - 2.0 * pad, cv.h as f32 - 2.0 * pad);
    match ui.screen {
        Screen::Library => paint_library(ui, cv, full, s),
        Screen::Game => paint_game(ui, cv, full, s),
        Screen::Settings => paint_settings(ui, cv, full, s),
        Screen::AddGame => paint_add(ui, cv, full, s),
    }

    // The sheets, over whatever screen raised them. Each is modal: everything
    // behind it stops being clickable, not just stops being drawn. The page's hit
    // rects are pushed as it paints, so they are dropped again here — or a click
    // meant for the sheet could fall through onto a card.
    if ui.confirm.is_some() || ui.menu.is_some() || ui.picking.is_some() {
        ui.hot.clear();
        let screen = cv_rect(cv);
        if ui.confirm.is_some() {
            paint_confirm(ui, cv, screen, s);
        } else if ui.menu.is_some() {
            paint_card_menu(ui, cv, screen, s);
        } else {
            paint_pick_program(ui, cv, screen, s);
        }
    }
}

/// What is behind the page — and the *only* thing that differs between a screen
/// with a game paused behind it and the same screen without.
///
/// The page is full-bleed either way, so the scrim has to carry all the contrast
/// that a floating panel used to carry with an opaque face: heavy enough to read
/// against, thin enough that the frame you paused is still visibly there.
fn backdrop(cv: &mut Canvas, over_game: bool) {
    let all = cv_rect(cv);
    if over_game {
        cv.clear(Color(0, 0, 0, 0));
        cv.fill(all, t::SCRIM);
    } else {
        cv.clear(t::BG);
    }
    // The same slow wash of petal colour off the top in both, so the two
    // backdrops read as one interface with different things behind it.
    cv.gradient_v(
        Rect::new(0.0, 0.0, all.w, all.h * 0.55),
        t::ACCENT_DEEP.alpha(26),
        t::BG.alpha(0),
    );
}

fn cv_rect(cv: &Canvas) -> Rect {
    Rect::new(0.0, 0.0, cv.w as f32, cv.h as f32)
}

// ---- the bar every screen wears ---------------------------------------------

/// The way back, the title, and the room they take. Returns the rect the screen's
/// own content gets, below it.
///
/// The back button is drawn, not implied. Escape does the same thing, but a key
/// you have to be told about is not an interface — and the line of grey hints
/// this replaces ("Arrows move  Enter choose  Esc back") was exactly that: the
/// exits, printed at the foot of the page, for anyone still reading.
fn nav(
    ui: &mut Ui,
    cv: &mut Canvas,
    r: Rect,
    s: f32,
    title: &str,
    sub: Option<(&str, bool)>,
) -> Rect {
    if ui.has_back() {
        // "‹" rather than a drawn chevron: it is one glyph in the font we already
        // ship, and `back_arrow_has_a_glyph` keeps it honest.
        let label = "‹   Back";
        let bh = BAR_H * s;
        let bw = ui.fonts.width(label, Weight::Bold, 15.0 * s) + 32.0 * s;
        let btn = Rect::new(r.x, r.y, bw, bh);
        cv.rounded(btn, bh / 2.0, t::SURFACE_HI);
        cv.stroke(btn, bh / 2.0, 1.0, t::BORDER);
        ui.fonts.draw_centered(
            cv,
            label,
            btn.x,
            btn.w,
            btn.y + (bh - 20.0 * s) / 2.0,
            Weight::Bold,
            15.0 * s,
            t::TEXT,
        );
        ui.push_hot(btn, Hit::Back);
    }

    let ty = r.y + (BAR_H + TITLE_DROP) * s;
    let title = ui.fonts.elide(title, Weight::Bold, 30.0 * s, r.w * 0.7);
    ui.fonts
        .draw_top(cv, &title, r.x, ty, Weight::Bold, 30.0 * s, t::TEXT);
    if let Some((text, accent)) = sub {
        let text = ui.fonts.elide(text, Weight::Regular, 14.5 * s, r.w * 0.8);
        ui.fonts.draw_top(
            cv,
            &text,
            r.x,
            ty + SUB_DROP * s,
            Weight::Regular,
            14.5 * s,
            if accent { t::ACCENT } else { t::TEXT_DIM },
        );
    }

    // The content starts at the same y whether or not there was a subtitle: a
    // page that shifted up when its subtitle went away would be a page that moves
    // for no reason the player can see.
    let top = ty + (SUB_DROP + CONTENT_DROP) * s;
    Rect::new(r.x, top, r.w, (r.y + r.h - top).max(0.0))
}

// ---- the question asked before anything cannot be undone ---------------------

fn paint_confirm(ui: &mut Ui, cv: &mut Canvas, screen: Rect, s: f32) {
    let Some(c) = ui.confirm.clone() else { return };

    cv.fill(screen, t::SCRIM);

    let w = (540.0 * s).min(screen.w - 40.0 * s);
    let h = 232.0 * s;
    let d = Rect::new((screen.w - w) / 2.0, (screen.h - h) / 2.0, w, h);
    shadow(cv, d, t::RADIUS * 1.6, 20.0 * s);
    cv.rounded(d, t::RADIUS * 1.6, t::SURFACE);
    cv.stroke(d, t::RADIUS * 1.6, 1.0, t::BORDER);

    let x = d.x + 28.0 * s;
    let inner = d.w - 56.0 * s;
    let title = ui.fonts.elide(&c.title, Weight::Bold, 22.0 * s, inner);
    ui.fonts.draw_top(
        cv,
        &title,
        x,
        d.y + 28.0 * s,
        Weight::Bold,
        22.0 * s,
        t::TEXT,
    );
    let detail = ui.fonts.elide(&c.detail, Weight::Regular, 14.5 * s, inner);
    ui.fonts.draw_top(
        cv,
        &detail,
        x,
        d.y + 66.0 * s,
        Weight::Regular,
        14.5 * s,
        t::TEXT_DIM,
    );
    if let Some(note) = &c.note {
        let note = ui.fonts.elide(note, Weight::Regular, 14.5 * s, inner);
        ui.fonts.draw_top(
            cv,
            &note,
            x,
            d.y + 90.0 * s,
            Weight::Regular,
            14.5 * s,
            t::TEXT_OFF,
        );
    }

    // Buttons, bottom-right: Cancel then the answer, so the safe one is nearer.
    let bh = 44.0 * s;
    let by = d.y + d.h - bh - 26.0 * s;
    let yes_w = ui.fonts.width(&c.yes, Weight::Bold, 15.0 * s) + 44.0 * s;
    let no_w = ui.fonts.width("Cancel", Weight::Bold, 15.0 * s) + 44.0 * s;
    let yes = Rect::new(d.x + d.w - 28.0 * s - yes_w, by, yes_w, bh);
    let no = Rect::new(yes.x - 10.0 * s - no_w, by, no_w, bh);

    // Whichever button holds the focus is the filled one — and the destructive
    // answer wears the warning colour only when it is the one that is armed.
    let on = c.yes_focused;
    let hot = if c.danger { t::DANGER } else { t::ACCENT };
    let ink = if c.danger {
        Color::rgb(0xFF, 0xF2, 0xF5)
    } else {
        Color::rgb(0x1A, 0x0C, 0x14)
    };
    button(ui, cv, yes, s, &c.yes, on, hot, ink);
    ui.push_hot(yes, Hit::ConfirmYes);
    button(
        ui,
        cv,
        no,
        s,
        "Cancel",
        !on,
        t::ACCENT,
        Color::rgb(0x1A, 0x0C, 0x14),
    );
    ui.push_hot(no, Hit::ConfirmNo);
}

/// A pill with a label: filled and inked when it holds the focus, quiet when it
/// does not.
#[allow(clippy::too_many_arguments)]
fn button(
    ui: &mut Ui,
    cv: &mut Canvas,
    r: Rect,
    s: f32,
    label: &str,
    on: bool,
    fill: Color,
    ink: Color,
) {
    cv.rounded(r, 8.0, if on { fill } else { t::SURFACE_HI });
    cv.stroke(r, 8.0, 1.0, if on { fill } else { t::BORDER });
    ui.fonts.draw_centered(
        cv,
        label,
        r.x,
        r.w,
        r.y + (r.h - 20.0 * s) / 2.0,
        Weight::Bold,
        15.0 * s,
        if on { ink } else { t::TEXT },
    );
}

// ---- the card menu, and the programs on a game's disk -----------------------

/// A centred sheet: a title, a list of rows, and its own way out. The "…" menu
/// and the list of programs are both this, and there is no reason for them to be
/// two layouts.
///
/// The way out is a *button*. A sheet covers the page's back button, so it has to
/// bring one of its own — otherwise the only exit is a key nobody mentioned.
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
    cancel: &str,
) -> Vec<Rect> {
    cv.fill(screen, t::SCRIM);

    let rh = 46.0 * s;
    let top = if subtitle.is_some() {
        96.0 * s
    } else {
        70.0 * s
    };
    let foot = 72.0 * s; // the way out
    let w = (480.0 * s).min(screen.w - 40.0 * s);
    let h = top + rows.len() as f32 * rh + foot;
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

    let bh = 40.0 * s;
    let bw = ui.fonts.width(cancel, Weight::Bold, 15.0 * s) + 40.0 * s;
    let btn = Rect::new(d.x + d.w - 24.0 * s - bw, d.y + d.h - bh - 18.0 * s, bw, bh);
    button(ui, cv, btn, s, cancel, false, t::ACCENT, t::TEXT);
    ui.push_hot(btn, Hit::SheetCancel);
    out
}

/// Everything you can do to a game that is not playing it.
fn paint_card_menu(ui: &mut Ui, cv: &mut Canvas, screen: Rect, s: f32) {
    let Some(i) = ui.menu else { return };
    let Some(g) = ui.games.get(i) else { return };
    let title = g.title.clone();
    let rows: Vec<String> = ui.menu_items(i).into_iter().map(str::to_string).collect();
    let sel = ui.menu_idx.min(rows.len().saturating_sub(1));

    let hits = sheet(ui, cv, screen, s, &title, None, &rows, sel, "Close");
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

    // "Back", not "Close": this opened from the card menu, and that is where
    // leaving it lands.
    let hits = sheet(
        ui,
        cv,
        screen,
        s,
        "Run a file",
        Some(&sub),
        &rows,
        sel,
        "Back",
    );
    for (i, r) in hits.into_iter().enumerate() {
        ui.push_hot(r, Hit::Program(i));
    }
}

/// Three dots — the button that means "and the rest". Drawn from rectangles:
/// there is no icon font here, and one glyph is not worth becoming a reason to
/// add one.
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
/// is what lifts a sheet off the page behind it.
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
    let n = ui.games.len();
    let count = match n {
        0 => "No games yet".to_string(),
        1 => "1 game".to_string(),
        n => format!("{n} games"),
    };
    // With a game paused behind it, say so — this is the list you switch games
    // from, and the one you came from is still sitting there.
    let sub = match ui.running {
        Some(i) => format!("{count}  ·  {} is paused", ui.games[i].title),
        None => count,
    };
    let inner = nav(ui, cv, r, s, "Saisei", Some((&sub, ui.in_game())));

    // Add game: a real button, in the bar, always on screen.
    //
    // It used to be the last tile of the grid, which meant that the more games you
    // had the further it slid down — and past a screenful it was off the bottom
    // entirely, so the one way to add a game was the one thing you couldn't see.
    // Up here it is never scrolled away and never competes with the games.
    if ui.can_edit_library() {
        let add_sel = ui.game == n;
        let label = "+  Add game";
        let bh = BAR_H * s;
        let bw = ui.fonts.width(label, Weight::Bold, 15.0 * s) + 34.0 * s;
        let btn = Rect::new(r.x + r.w - bw, r.y, bw, bh);
        cv.rounded(
            btn,
            bh / 2.0,
            if add_sel { t::ACCENT } else { t::SURFACE_HI },
        );
        cv.stroke(
            btn,
            bh / 2.0,
            1.0,
            if add_sel { t::ACCENT } else { t::BORDER },
        );
        ui.fonts.draw_centered(
            cv,
            label,
            btn.x,
            btn.w,
            btn.y + (bh - 20.0 * s) / 2.0,
            Weight::Bold,
            15.0 * s,
            if add_sel {
                Color::rgb(0x1A, 0x0C, 0x14)
            } else {
                t::TEXT
            },
        );
        ui.push_hot(btn, Hit::Add);
    }

    let top = inner.y;
    let gap = 22.0 * s;
    let card_w = 250.0 * s;
    let cover_h = card_w * 0.625; // 16:10
    let card_h = cover_h + 52.0 * s;

    let cols = (((inner.w + gap) / (card_w + gap)).floor() as usize).max(1);
    ui.cols = cols;

    if n == 0 {
        ui.fonts.draw_top(
            cv,
            "Nothing here yet.",
            inner.x,
            top + 10.0 * s,
            Weight::Bold,
            20.0 * s,
            t::TEXT_DIM,
        );
        ui.fonts.draw_top(
            cv,
            "Drop a game's zip on this window, or choose Add game.",
            inner.x,
            top + 42.0 * s,
            Weight::Regular,
            15.0 * s,
            t::TEXT_OFF,
        );
        return;
    }
    let rows = n.div_ceil(cols);

    // Scroll by whole rows, keeping the cursor on screen.
    let per_screen = (((inner.h + gap) / (card_h + gap)).floor() as usize).clamp(1, rows);
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
        let x = inner.x + col as f32 * (card_w + gap);
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
            inner.x + inner.w - w,
            top - 22.0 * s,
            Weight::Regular,
            12.5 * s,
            t::TEXT_OFF,
        );
    }
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
    // The game you are in says so, wherever you have wandered off to. It is the
    // one card here that is not an offer to start something.
    let running = ui.running == Some(i);
    let saves = ui.games[i].saves.len();
    let sub = match (running, saves) {
        (true, _) => "Paused".to_string(),
        (_, 0) => "Not played".to_string(),
        (_, 1) => "1 save".to_string(),
        (_, n) => format!("{n} saves"),
    };
    ui.fonts.draw_top(
        cv,
        &sub,
        card.x + 12.0 * s,
        cover.y + cover.h + 30.0 * s,
        Weight::Regular,
        12.5 * s,
        if running { t::ACCENT } else { t::TEXT_DIM },
    );

    if sel {
        cv.stroke(card, t::RADIUS, 2.0, t::ACCENT);

        // The "…", on the card the cursor is on — and only that one, so a library
        // of covers is not also a row of little buttons to misclick. Hovering a
        // card selects it, so with a mouse this is "on hover"; with the keyboard it
        // follows the selection, which is where Delete would act anyway.
        //
        // Not while a game is paused: everything behind it renumbers the library,
        // and none of it is work that cannot wait until you have left the game.
        if ui.can_edit_library() {
            let d = 30.0 * s;
            let btn = Rect::new(card.x + card.w - d - 8.0 * s, card.y + 8.0 * s, d, d);
            cv.rounded(btn, d / 2.0, t::BG.alpha(235));
            cv.stroke(btn, d / 2.0, 1.0, t::BORDER);
            dots_icon(cv, btn, t::TEXT);
            ui.push_hot(btn, Hit::Menu(i));
        }
    }
}

// ---- a game's page (and, for the game you are in, the pause menu) ------------

fn paint_game(ui: &mut Ui, cv: &mut Canvas, r: Rect, s: f32) {
    let Some(g) = ui.games.get(ui.game) else {
        return;
    };
    let title = g.title.clone();
    let saves = g.saves.len();
    let paused = ui.running == Some(ui.game);

    let sub = if paused {
        "Paused".to_string()
    } else if saves == 0 {
        "Not played yet".to_string()
    } else {
        format!("{saves} save{}", if saves == 1 { "" } else { "s" })
    };
    let inner = nav(ui, cv, r, s, &title, Some((&sub, paused)));

    // Actions.
    let col_w = (300.0 * s).min(inner.w * 0.42);
    let mut y = inner.y;
    let bh = 46.0 * s;
    let labels = ui.actions();
    for (i, label) in labels.iter().enumerate() {
        let row = Rect::new(inner.x, y, col_w, bh);
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
            inner.x,
            y + 6.0 * s,
            Weight::Regular,
            14.0 * s,
            t::ACCENT,
        );
    }

    // Saves.
    let px = inner.x + col_w + 34.0 * s;
    let pw = inner.x + inner.w - px;
    if pw > 120.0 * s {
        paint_saves(ui, cv, Rect::new(px, inner.y, pw, inner.h), s);
    }
}

fn paint_saves(ui: &mut Ui, cv: &mut Canvas, r: Rect, s: f32) {
    ui.fonts
        .draw_top(cv, "Saves", r.x, r.y, Weight::Bold, 15.0 * s, t::TEXT_DIM);

    let paused = ui.running == Some(ui.game);
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
            if paused {
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
    // Volume is per game, so say whose. A slider that silently meant something
    // different depending on where you opened it from would be a trap.
    let game = ui
        .games
        .get(ui.game)
        .map(|g| g.title.clone())
        .unwrap_or_default();
    let inner = nav(ui, cv, r, s, "Settings", Some((&game, true)));

    let col_w = (380.0 * s).min(inner.w);
    paint_volume(ui, cv, Rect::new(inner.x, inner.y, col_w, 44.0 * s), s);

    ui.fonts.draw_top(
        cv,
        "Remembered for this game.",
        inner.x,
        inner.y + 62.0 * s,
        Weight::Regular,
        13.0 * s,
        t::TEXT_OFF,
    );
}

// ---- add a game -------------------------------------------------------------

fn paint_add(ui: &mut Ui, cv: &mut Canvas, r: Rect, s: f32) {
    // Once a bundle is unpacked, the only question left is which file is the game.
    let picking = !ui.add.exes.is_empty();
    let sub = if picking {
        "Which one starts the game?"
    } else {
        "Drop a game's zip or folder anywhere on this window."
    };
    let inner = nav(ui, cv, r, s, "Add a game", Some((sub, false)));

    if picking {
        let row_h = 40.0 * s;
        let w = (460.0 * s).min(inner.w);
        for i in 0..ui.add.exes.len() {
            let y = inner.y + i as f32 * (row_h + 6.0 * s);
            if y + row_h > inner.y + inner.h {
                break;
            }
            let row = Rect::new(inner.x, y, w, row_h);
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
        return;
    }

    // The drop target: a well that says what it wants.
    let well = Rect::new(inner.x, inner.y, (620.0 * s).min(inner.w), 150.0 * s);
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
        inner.x,
        fy,
        Weight::Regular,
        14.0 * s,
        t::TEXT_DIM,
    );
    // Paste, as a button. Ctrl+V does it too, but a link is the one thing here you
    // are most likely to have on the clipboard rather than in your head, and a
    // chord you have to know about is not an interface.
    let paste_w = ui.fonts.width("Paste", Weight::Bold, 14.5 * s) + 30.0 * s;
    let field = Rect::new(inner.x, fy + 26.0 * s, well.w - paste_w - 8.0 * s, 44.0 * s);
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
            inner.x,
            field.y + field.h + 18.0 * s,
            Weight::Regular,
            14.5 * s,
            if ui.add.busy { t::TEXT_DIM } else { t::ACCENT },
        );
    }
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

#[cfg(test)]
mod tests {
    use crate::font::Weight;
    use crate::{Action, Canvas, Fonts, GameView, Image, Key, Screen, Ui};

    /// The pause menu, stepped into its Settings page — where the slider lives.
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
    fn the_back_arrow_has_a_glyph() {
        // The back button is a font glyph, not a drawn icon. If the bundled face
        // ever loses it, the way out of every screen quietly becomes a blank.
        let mut f = Fonts::new();
        assert!(f.width("‹", Weight::Bold, 20.0) > 0.0, "no advance");
        let mut cv = Canvas::new(40, 40);
        f.draw_top(
            &mut cv,
            "‹",
            8.0,
            4.0,
            Weight::Bold,
            24.0,
            crate::Color::rgb(255, 255, 255),
        );
        let ink = cv.px.chunks_exact(4).filter(|p| p[0] > 40).count();
        assert!(ink > 8, "the chevron rasterized to nothing ({ink} px)");
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
            .expect("the pause menu must have a volume slider");

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
        // Back leaves, and does not move the value on the way out.
        ui.key(Key::Escape);
        assert_eq!(ui.screen, Screen::Game);
        assert_eq!(ui.volume, 0.5);
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
