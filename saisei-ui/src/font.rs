//! Text: glyph rasterization, a cache, and just enough layout to place a line.

use std::collections::HashMap;

use crate::canvas::{Canvas, Color};

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Weight {
    Regular,
    Bold,
}

/// Rasterized glyphs are cached by (weight, char, size): the UI redraws every
/// frame while a menu is open, and re-rasterizing the same handful of strings at
/// 60Hz would be pure waste.
#[derive(PartialEq, Eq, Hash)]
struct GlyphKey(Weight, char, u32);

struct Glyph {
    bitmap: Vec<u8>,
    w: usize,
    h: usize,
    xmin: i32,
    ymin: i32,
    advance: f32,
}

pub struct Fonts {
    regular: fontdue::Font,
    bold: fontdue::Font,
    cache: HashMap<GlyphKey, Glyph>,
}

impl Fonts {
    pub fn new() -> Fonts {
        let opts = fontdue::FontSettings::default();
        Fonts {
            regular: fontdue::Font::from_bytes(
                include_bytes!("../assets/NotoSans-Regular.ttf").as_slice(),
                opts,
            )
            .expect("bundled regular font"),
            bold: fontdue::Font::from_bytes(
                include_bytes!("../assets/NotoSans-Bold.ttf").as_slice(),
                opts,
            )
            .expect("bundled bold font"),
            cache: HashMap::new(),
        }
    }

    fn face(&self, w: Weight) -> &fontdue::Font {
        match w {
            Weight::Regular => &self.regular,
            Weight::Bold => &self.bold,
        }
    }

    fn glyph(&mut self, w: Weight, ch: char, px: f32) -> &Glyph {
        // Quantize the size so a resize doesn't blow the cache away on every
        // intermediate pixel.
        let key = GlyphKey(w, ch, (px * 4.0).round() as u32);
        if !self.cache.contains_key(&key) {
            let size = (px * 4.0).round() / 4.0;
            let (m, bitmap) = self.face(w).rasterize(ch, size);
            self.cache.insert(
                key,
                Glyph {
                    bitmap,
                    w: m.width,
                    h: m.height,
                    xmin: m.xmin,
                    ymin: m.ymin,
                    advance: m.advance_width,
                },
            );
            // Re-derive the key: `key` was moved into the map.
            return self
                .cache
                .get(&GlyphKey(w, ch, (px * 4.0).round() as u32))
                .unwrap();
        }
        self.cache.get(&key).unwrap()
    }

    pub fn width(&mut self, text: &str, w: Weight, px: f32) -> f32 {
        text.chars().map(|c| self.glyph(w, c, px).advance).sum()
    }

    /// The distance from a line's top to its baseline, so callers can lay out in
    /// terms of a text box rather than a baseline.
    pub fn ascent(&self, w: Weight, px: f32) -> f32 {
        let m = self.face(w).horizontal_line_metrics(px).unwrap();
        m.ascent
    }

    pub fn line_height(&self, w: Weight, px: f32) -> f32 {
        let m = self.face(w).horizontal_line_metrics(px).unwrap();
        m.ascent - m.descent + m.line_gap
    }

    /// Draw `text` with its left edge at `x` and its *baseline* at `y`.
    pub fn draw(
        &mut self,
        cv: &mut Canvas,
        text: &str,
        x: f32,
        y: f32,
        w: Weight,
        px: f32,
        color: Color,
    ) -> f32 {
        let mut pen = x;
        for ch in text.chars() {
            let g = self.glyph(w, ch, px);
            let (gw, gh, xmin, ymin, adv) = (g.w, g.h, g.xmin, g.ymin, g.advance);
            if gw > 0 && gh > 0 {
                // fontdue's ymin is the offset of the bitmap's *bottom* from the
                // baseline, y-up; the canvas is y-down.
                let gx = (pen + xmin as f32).round() as i32;
                let gy = (y - ymin as f32 - gh as f32).round() as i32;
                let bitmap = self.cache[&GlyphKey(w, ch, (px * 4.0).round() as u32)]
                    .bitmap
                    .clone();
                cv.glyph(&bitmap, gw, gh, gx, gy, color);
            }
            pen += adv;
        }
        pen
    }

    /// Draw with the text box's *top* at `y` — how the layout code thinks.
    pub fn draw_top(
        &mut self,
        cv: &mut Canvas,
        text: &str,
        x: f32,
        y: f32,
        w: Weight,
        px: f32,
        color: Color,
    ) -> f32 {
        let base = y + self.ascent(w, px);
        self.draw(cv, text, x, base, w, px, color)
    }

    /// Draw centred within `[x, x + width]`.
    pub fn draw_centered(
        &mut self,
        cv: &mut Canvas,
        text: &str,
        x: f32,
        width: f32,
        y: f32,
        w: Weight,
        px: f32,
        color: Color,
    ) {
        let tw = self.width(text, w, px);
        self.draw_top(cv, text, x + (width - tw) / 2.0, y, w, px, color);
    }

    /// Shorten `text` with an ellipsis until it fits `max` px. A game title or a
    /// long file name must never bleed out of its card.
    pub fn elide(&mut self, text: &str, w: Weight, px: f32, max: f32) -> String {
        if self.width(text, w, px) <= max {
            return text.to_string();
        }
        let mut chars: Vec<char> = text.chars().collect();
        while !chars.is_empty() {
            chars.pop();
            let cand: String = chars.iter().collect::<String>() + "…";
            if self.width(&cand, w, px) <= max {
                return cand;
            }
        }
        String::new()
    }
}

impl Default for Fonts {
    fn default() -> Self {
        Fonts::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::canvas::Rect;

    #[test]
    fn width_grows_with_text_and_size() {
        let mut f = Fonts::new();
        let a = f.width("Zeliard", Weight::Regular, 20.0);
        let b = f.width("Zeliard", Weight::Regular, 40.0);
        let c = f.width("Zeliard and more", Weight::Regular, 20.0);
        assert!(a > 0.0);
        assert!(b > a * 1.8, "40px should be ~2x 20px, got {a} -> {b}");
        assert!(c > a);
    }

    #[test]
    fn elide_fits_within_the_budget() {
        let mut f = Fonts::new();
        let long = "Dungeon Master and the Chaos Strikes Back Expansion";
        let out = f.elide(long, Weight::Regular, 20.0, 120.0);
        assert!(out.ends_with('…'));
        assert!(f.width(&out, Weight::Regular, 20.0) <= 120.0);
        // Something that already fits is returned untouched.
        assert_eq!(f.elide("Pop", Weight::Regular, 20.0, 400.0), "Pop");
    }

    #[test]
    fn draw_puts_ink_on_the_canvas_inside_its_box() {
        let mut f = Fonts::new();
        let mut cv = Canvas::new(200, 60);
        cv.clear(crate::canvas::Color::rgb(0, 0, 0));
        f.draw_top(
            &mut cv,
            "Saisei",
            10.0,
            10.0,
            Weight::Bold,
            28.0,
            crate::canvas::Color::rgb(255, 255, 255),
        );
        let lit = cv.px.chunks_exact(4).filter(|p| p[0] > 40).count();
        assert!(lit > 40, "expected glyph coverage, got {lit} lit pixels");

        // And nothing outside the line box.
        let box_h = f.line_height(Weight::Bold, 28.0) + 10.0;
        let stray = (0..cv.h)
            .filter(|&y| y as f32 > box_h + 4.0)
            .flat_map(|y| (0..cv.w).map(move |x| (x, y)))
            .filter(|&(x, y)| cv.px[(y * cv.w + x) * 4] > 40)
            .count();
        assert_eq!(stray, 0, "glyphs spilled below the line box");
        let _ = Rect::default();
    }
}
