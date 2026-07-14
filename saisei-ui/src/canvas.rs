//! An RGBA8888 pixel buffer and the handful of things we draw into it.
//!
//! Everything here is plain software compositing — no GPU, no SDL. The UI is a
//! few hundred rectangles and glyphs a frame at menu refresh rates, so the
//! simplicity is free, and it keeps the whole interface testable without a
//! window.

/// Straight (non-premultiplied) RGBA.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Color(pub u8, pub u8, pub u8, pub u8);

impl Color {
    pub const fn rgb(r: u8, g: u8, b: u8) -> Self {
        Color(r, g, b, 255)
    }
    /// The same colour at a different opacity.
    pub const fn alpha(self, a: u8) -> Self {
        Color(self.0, self.1, self.2, a)
    }
    /// Toward `other` by `t` in 0..=1.
    pub fn mix(self, other: Color, t: f32) -> Color {
        let t = t.clamp(0.0, 1.0);
        let l = |a: u8, b: u8| (a as f32 + (b as f32 - a as f32) * t).round() as u8;
        Color(
            l(self.0, other.0),
            l(self.1, other.1),
            l(self.2, other.2),
            l(self.3, other.3),
        )
    }
}

#[derive(Clone, Copy, PartialEq, Debug, Default)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl Rect {
    pub const fn new(x: f32, y: f32, w: f32, h: f32) -> Self {
        Rect { x, y, w, h }
    }
    pub fn contains(&self, px: f32, py: f32) -> bool {
        px >= self.x && py >= self.y && px < self.x + self.w && py < self.y + self.h
    }
    pub fn inset(&self, d: f32) -> Rect {
        Rect::new(
            self.x + d,
            self.y + d,
            (self.w - 2.0 * d).max(0.0),
            (self.h - 2.0 * d).max(0.0),
        )
    }
}

/// A decoded RGB24 image — a save thumbnail, or the logo.
#[derive(Clone, Default)]
pub struct Image {
    pub w: usize,
    pub h: usize,
    pub rgb: Vec<u8>,
}

impl Image {
    pub fn decode_png(bytes: &[u8]) -> Option<Image> {
        let img = image::load_from_memory_with_format(bytes, image::ImageFormat::Png).ok()?;
        let rgb = img.to_rgb8();
        Some(Image {
            w: rgb.width() as usize,
            h: rgb.height() as usize,
            rgb: rgb.into_raw(),
        })
    }
    pub fn load_png(path: &std::path::Path) -> Option<Image> {
        Image::decode_png(&std::fs::read(path).ok()?)
    }
    pub fn is_empty(&self) -> bool {
        self.w == 0 || self.h == 0
    }

    /// A sub-rectangle of this image, clamped to its bounds.
    pub fn crop(&self, x: usize, y: usize, w: usize, h: usize) -> Image {
        let x0 = x.min(self.w);
        let y0 = y.min(self.h);
        let w = w.min(self.w - x0);
        let h = h.min(self.h - y0);
        let mut rgb = Vec::with_capacity(w * h * 3);
        for row in y0..y0 + h {
            let s = (row * self.w + x0) * 3;
            rgb.extend_from_slice(&self.rgb[s..s + w * 3]);
        }
        Image { w, h, rgb }
    }

    /// The same image, box-filtered down to `h` pixels tall (or returned as it is,
    /// if it is already smaller).
    ///
    /// The brand art is cut out of a 1402px splash and drawn a hundred pixels tall.
    /// Resampling the full-size source on every frame of a menu would be a megabyte
    /// of reads per frame for a picture that never changes; this is done once.
    pub fn scaled_to_height(&self, h: usize) -> Image {
        if self.is_empty() || self.h <= h || h == 0 {
            return self.clone();
        }
        let w = (self.w * h / self.h).max(1);
        let mut rgb = Vec::with_capacity(w * h * 3);
        for dy in 0..h {
            let (sy0, sy1) = (dy * self.h / h, ((dy + 1) * self.h).div_ceil(h).min(self.h));
            for dx in 0..w {
                let (sx0, sx1) = (dx * self.w / w, ((dx + 1) * self.w).div_ceil(w).min(self.w));
                let (mut r, mut g, mut b, mut n) = (0u32, 0u32, 0u32, 0u32);
                for sy in sy0..sy1.max(sy0 + 1) {
                    for sx in sx0..sx1.max(sx0 + 1) {
                        let s = (sy * self.w + sx) * 3;
                        r += self.rgb[s] as u32;
                        g += self.rgb[s + 1] as u32;
                        b += self.rgb[s + 2] as u32;
                        n += 1;
                    }
                }
                rgb.push((r / n) as u8);
                rgb.push((g / n) as u8);
                rgb.push((b / n) as u8);
            }
        }
        Image { w, h, rgb }
    }
}

pub struct Canvas {
    pub w: usize,
    pub h: usize,
    /// RGBA8888, row-major, no padding.
    pub px: Vec<u8>,
}

impl Canvas {
    pub fn new(w: usize, h: usize) -> Canvas {
        Canvas {
            w,
            h,
            px: vec![0; w * h * 4],
        }
    }

    pub fn clear(&mut self, c: Color) {
        for p in self.px.chunks_exact_mut(4) {
            p[0] = c.0;
            p[1] = c.1;
            p[2] = c.2;
            p[3] = c.3;
        }
    }

    /// Source-over blend of `c` at `cov` coverage (0..=1) onto one pixel.
    #[inline]
    fn blend(&mut self, x: i32, y: i32, c: Color, cov: f32) {
        if x < 0 || y < 0 || x >= self.w as i32 || y >= self.h as i32 {
            return;
        }
        let a = (c.3 as f32 / 255.0) * cov.clamp(0.0, 1.0);
        if a <= 0.0 {
            return;
        }
        let i = (y as usize * self.w + x as usize) * 4;
        let dst = &mut self.px[i..i + 4];
        let da = dst[3] as f32 / 255.0;
        let out_a = a + da * (1.0 - a);
        if out_a <= 0.0 {
            dst.copy_from_slice(&[0, 0, 0, 0]);
            return;
        }
        let ch = |s: u8, d: u8| -> u8 {
            let s = s as f32 / 255.0;
            let d = d as f32 / 255.0;
            (((s * a + d * da * (1.0 - a)) / out_a) * 255.0)
                .round()
                .clamp(0.0, 255.0) as u8
        };
        dst[0] = ch(c.0, dst[0]);
        dst[1] = ch(c.1, dst[1]);
        dst[2] = ch(c.2, dst[2]);
        dst[3] = (out_a * 255.0).round() as u8;
    }

    pub fn fill(&mut self, r: Rect, c: Color) {
        self.rounded(r, 0.0, c);
    }

    /// A rounded rectangle, antialiased on its edges.
    ///
    /// Coverage comes from a signed distance to the rounded box, which gives
    /// clean corners and costs nothing at these sizes.
    pub fn rounded(&mut self, r: Rect, radius: f32, c: Color) {
        if r.w <= 0.0 || r.h <= 0.0 {
            return;
        }
        let rad = radius.min(r.w / 2.0).min(r.h / 2.0).max(0.0);
        let x0 = (r.x.floor() as i32 - 1).max(0);
        let y0 = (r.y.floor() as i32 - 1).max(0);
        let x1 = ((r.x + r.w).ceil() as i32 + 1).min(self.w as i32);
        let y1 = ((r.y + r.h).ceil() as i32 + 1).min(self.h as i32);
        let (cx, cy) = (r.x + r.w / 2.0, r.y + r.h / 2.0);
        let (hx, hy) = (r.w / 2.0 - rad, r.h / 2.0 - rad);
        for y in y0..y1 {
            for x in x0..x1 {
                // Distance from the rounded box, negative inside.
                let dx = ((x as f32 + 0.5) - cx).abs() - hx;
                let dy = ((y as f32 + 0.5) - cy).abs() - hy;
                let d = if dx > 0.0 && dy > 0.0 {
                    (dx * dx + dy * dy).sqrt() - rad
                } else {
                    dx.max(dy) - rad
                };
                // One pixel of feather across the boundary.
                let cov = (0.5 - d).clamp(0.0, 1.0);
                if cov > 0.0 {
                    self.blend(x, y, c, cov);
                }
            }
        }
    }

    /// A rounded outline, `width` px thick, drawn inside `r`.
    pub fn stroke(&mut self, r: Rect, radius: f32, width: f32, c: Color) {
        if r.w <= 0.0 || r.h <= 0.0 || width <= 0.0 {
            return;
        }
        let rad = radius.min(r.w / 2.0).min(r.h / 2.0).max(0.0);
        let x0 = (r.x.floor() as i32 - 1).max(0);
        let y0 = (r.y.floor() as i32 - 1).max(0);
        let x1 = ((r.x + r.w).ceil() as i32 + 1).min(self.w as i32);
        let y1 = ((r.y + r.h).ceil() as i32 + 1).min(self.h as i32);
        let (cx, cy) = (r.x + r.w / 2.0, r.y + r.h / 2.0);
        let (hx, hy) = (r.w / 2.0 - rad, r.h / 2.0 - rad);
        for y in y0..y1 {
            for x in x0..x1 {
                let dx = ((x as f32 + 0.5) - cx).abs() - hx;
                let dy = ((y as f32 + 0.5) - cy).abs() - hy;
                let d = if dx > 0.0 && dy > 0.0 {
                    (dx * dx + dy * dy).sqrt() - rad
                } else {
                    dx.max(dy) - rad
                };
                // Band of |d| within the stroke, feathered on both sides.
                let band = (d + width).max(0.0).min(1.0) * (0.5 - d).clamp(0.0, 1.0).min(1.0);
                let cov = if d < -width {
                    0.0
                } else {
                    band.max(0.0).min(1.0)
                };
                if cov > 0.0 {
                    self.blend(x, y, c, cov);
                }
            }
        }
    }

    /// Draw `img` to fill `r` while preserving aspect (letterboxed, centred),
    /// box-filtered on the way down so a 320x200 frame downscaled to a thumbnail
    /// stays legible instead of aliasing into noise.
    ///
    /// Returns the rect the image actually covered.
    pub fn image_fit(&mut self, img: &Image, r: Rect, alpha: u8) -> Rect {
        if img.is_empty() || r.w <= 0.0 || r.h <= 0.0 {
            return Rect::default();
        }
        let scale = (r.w / img.w as f32).min(r.h / img.h as f32);
        let dw = (img.w as f32 * scale).max(1.0);
        let dh = (img.h as f32 * scale).max(1.0);
        let dst = Rect::new(r.x + (r.w - dw) / 2.0, r.y + (r.h - dh) / 2.0, dw, dh);
        self.blit(img, dst, alpha, false);
        dst
    }

    /// Draw `img` fitted into `r` with its black keyed out: each pixel's coverage
    /// is its own brightness, so the darkest parts of the picture are the parts you
    /// can see through.
    ///
    /// Every piece of the logo is pixel art painted on black — which is a *shape*,
    /// drawn on nothing, and not a black tile with a tree in it. Blitted flat, the
    /// header's mark would be a black rectangle sitting on the page, and worse over
    /// a paused game, where the page is not black. Keyed, it lands as the thing
    /// itself, and its dissolving edge pixels fade out instead of stopping at a
    /// border.
    ///
    /// Returns the rect the image actually covered.
    pub fn image_keyed(&mut self, img: &Image, r: Rect, alpha: u8) -> Rect {
        if img.is_empty() || r.w <= 0.0 || r.h <= 0.0 {
            return Rect::default();
        }
        let scale = (r.w / img.w as f32).min(r.h / img.h as f32);
        let dw = (img.w as f32 * scale).max(1.0);
        let dh = (img.h as f32 * scale).max(1.0);
        let dst = Rect::new(r.x + (r.w - dw) / 2.0, r.y + (r.h - dh) / 2.0, dw, dh);
        self.blit(img, dst, alpha, true);
        dst
    }

    /// Draw `img` stretched to exactly `r`.
    pub fn image_stretch(&mut self, img: &Image, r: Rect, alpha: u8) {
        self.blit(img, r, alpha, false);
    }

    /// The one blit. `keyed` takes each pixel's coverage from its own brightness
    /// rather than covering the destination outright.
    fn blit(&mut self, img: &Image, r: Rect, alpha: u8, keyed: bool) {
        if img.is_empty() || r.w <= 0.0 || r.h <= 0.0 {
            return;
        }
        let dw = r.w.round().max(1.0) as usize;
        let dh = r.h.round().max(1.0) as usize;
        let ox = r.x.round() as i32;
        let oy = r.y.round() as i32;
        for dy in 0..dh {
            // Source box for this destination row.
            let sy0 = dy * img.h / dh;
            let sy1 = (((dy + 1) * img.h).div_ceil(dh)).clamp(sy0 + 1, img.h);
            for dx in 0..dw {
                let sx0 = dx * img.w / dw;
                let sx1 = (((dx + 1) * img.w).div_ceil(dw)).clamp(sx0 + 1, img.w);
                let (mut r_, mut g_, mut b_, mut n) = (0u32, 0u32, 0u32, 0u32);
                for sy in sy0..sy1 {
                    for sx in sx0..sx1 {
                        let s = (sy * img.w + sx) * 3;
                        r_ += img.rgb[s] as u32;
                        g_ += img.rgb[s + 1] as u32;
                        b_ += img.rgb[s + 2] as u32;
                        n += 1;
                    }
                }
                let c = Color((r_ / n) as u8, (g_ / n) as u8, (b_ / n) as u8, alpha);
                // The value channel, not a luminance: the blossom is saturated pink
                // and the machine is cream, and weighting those by how *bright* a
                // human finds them would thin the tree out and leave the PC solid.
                let cov = if keyed {
                    c.0.max(c.1).max(c.2) as f32 / 255.0
                } else {
                    1.0
                };
                self.blend(ox + dx as i32, oy + dy as i32, c, cov);
            }
        }
    }

    /// A soft vertical gradient — used to keep large flat panels from reading as
    /// dead space.
    pub fn gradient_v(&mut self, r: Rect, top: Color, bottom: Color) {
        let y0 = r.y.max(0.0) as i32;
        let y1 = ((r.y + r.h).min(self.h as f32)) as i32;
        for y in y0..y1 {
            let t = if r.h > 0.0 {
                (y as f32 - r.y) / r.h
            } else {
                0.0
            };
            let c = top.mix(bottom, t);
            let x0 = r.x.max(0.0) as i32;
            let x1 = ((r.x + r.w).min(self.w as f32)) as i32;
            for x in x0..x1 {
                self.blend(x, y, c, 1.0);
            }
        }
    }

    /// Blend one glyph's 8-bit coverage bitmap at (x, y).
    pub fn glyph(&mut self, bitmap: &[u8], gw: usize, gh: usize, x: i32, y: i32, c: Color) {
        for gy in 0..gh {
            for gx in 0..gw {
                let cov = bitmap[gy * gw + gx];
                if cov != 0 {
                    self.blend(x + gx as i32, y + gy as i32, c, cov as f32 / 255.0);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(c: &Canvas, x: usize, y: usize) -> Color {
        let i = (y * c.w + x) * 4;
        Color(c.px[i], c.px[i + 1], c.px[i + 2], c.px[i + 3])
    }

    #[test]
    fn fill_covers_its_interior_and_stops_at_its_edge() {
        let mut c = Canvas::new(10, 10);
        c.clear(Color::rgb(0, 0, 0));
        c.fill(Rect::new(2.0, 2.0, 4.0, 4.0), Color::rgb(255, 0, 0));
        assert_eq!(at(&c, 3, 3), Color::rgb(255, 0, 0));
        assert_eq!(at(&c, 0, 0), Color::rgb(0, 0, 0));
        assert_eq!(at(&c, 9, 9), Color::rgb(0, 0, 0));
    }

    #[test]
    fn half_alpha_over_black_is_half_the_colour() {
        let mut c = Canvas::new(4, 4);
        c.clear(Color::rgb(0, 0, 0));
        c.fill(Rect::new(0.0, 0.0, 4.0, 4.0), Color(255, 255, 255, 128));
        let p = at(&c, 1, 1);
        assert!((p.0 as i32 - 128).abs() <= 1, "got {p:?}");
        assert_eq!(p.3, 255);
    }

    #[test]
    fn image_fit_preserves_aspect_and_centres() {
        // A 2:1 image into a square box lands as a letterboxed band.
        let img = Image {
            w: 20,
            h: 10,
            rgb: vec![200; 20 * 10 * 3],
        };
        let mut c = Canvas::new(40, 40);
        c.clear(Color::rgb(0, 0, 0));
        let dst = c.image_fit(&img, Rect::new(0.0, 0.0, 40.0, 40.0), 255);
        assert_eq!((dst.w, dst.h), (40.0, 20.0));
        assert_eq!(dst.y, 10.0);
        // Inside the band: the image. Above it: untouched.
        assert_eq!(at(&c, 20, 20), Color::rgb(200, 200, 200));
        assert_eq!(at(&c, 20, 2), Color::rgb(0, 0, 0));
    }

    #[test]
    fn a_keyed_image_lets_its_black_through() {
        // The logo is a shape painted on black, not a black picture. Keyed, it is
        // drawn as *light*: its black is nothing at all, and a lit pixel covers the
        // page in proportion to how lit it is. (So even the brightest is a shade shy
        // of opaque, which is the point — the page keeps showing faintly through the
        // art, rather than the art punching a hole in the page.)
        let img = Image {
            w: 2,
            h: 1,
            rgb: vec![0, 0, 0, 0xE8, 0x7B, 0xA8],
        };
        let mut c = Canvas::new(2, 1);
        c.clear(Color::rgb(0, 0, 255));
        c.image_keyed(&img, Rect::new(0.0, 0.0, 2.0, 1.0), 255);
        assert_eq!(at(&c, 0, 0), Color::rgb(0, 0, 255), "the black was painted");
        let lit = at(&c, 1, 0);
        assert!(
            lit.0 > 0xD0 && lit.1 > 0x60 && (lit.2 as i32 - 0xA8).abs() < 20,
            "the blossom should land as itself, got {lit:?}"
        );

        // Flat, the same image lays its black down over the blue — which is what a
        // header drawn this way would have put in the corner of every screen.
        c.clear(Color::rgb(0, 0, 255));
        c.image_stretch(&img, Rect::new(0.0, 0.0, 2.0, 1.0), 255);
        assert_eq!(at(&c, 0, 0), Color::rgb(0, 0, 0));
    }

    #[test]
    fn a_crop_takes_the_rectangle_it_asked_for_and_scaling_keeps_the_aspect() {
        // A 4x2 image, left half red and right half green.
        let mut rgb = Vec::new();
        for _ in 0..2 {
            rgb.extend_from_slice(&[255, 0, 0, 255, 0, 0, 0, 255, 0, 0, 255, 0]);
        }
        let img = Image { w: 4, h: 2, rgb };

        let right = img.crop(2, 0, 2, 2);
        assert_eq!((right.w, right.h), (2, 2));
        assert!(right.rgb.chunks_exact(3).all(|p| p == [0, 255, 0]));

        // Clamped, not panicking, when the rect runs off the edge.
        assert_eq!(img.crop(3, 1, 99, 99).w, 1);

        let small = img.crop(0, 0, 4, 2).scaled_to_height(1);
        assert_eq!((small.w, small.h), (2, 1));
        // Nothing to do for an image already at or under the height.
        assert_eq!(small.scaled_to_height(4).h, 1);
    }

    #[test]
    fn rounded_corners_are_cut_away() {
        let mut c = Canvas::new(20, 20);
        c.clear(Color::rgb(0, 0, 0));
        c.rounded(
            Rect::new(0.0, 0.0, 20.0, 20.0),
            8.0,
            Color::rgb(255, 255, 255),
        );
        // Centre is filled; the extreme corner is outside the radius.
        assert_eq!(at(&c, 10, 10), Color::rgb(255, 255, 255));
        assert_eq!(at(&c, 0, 0), Color::rgb(0, 0, 0));
    }
}
