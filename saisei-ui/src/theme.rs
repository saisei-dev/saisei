//! The palette, taken from the logo.
//!
//! The splash art is a pixel-art cherry tree over a beige PC on black: near-black
//! ground, blossom pinks, a warm cream for the machine. The interface uses the
//! same three notes so the library reads as the same thing the logo does, rather
//! than as a generic dark theme that happens to appear first.

use crate::canvas::Color;

/// The page behind everything.
pub const BG: Color = Color::rgb(0x0B, 0x07, 0x0B);
/// A surface lifted off the page — a card, a panel.
pub const SURFACE: Color = Color::rgb(0x18, 0x11, 0x19);
/// The same, one step brighter: hovered, or holding focus.
pub const SURFACE_HI: Color = Color::rgb(0x24, 0x19, 0x26);
/// Hairlines and card edges.
pub const BORDER: Color = Color::rgb(0x33, 0x25, 0x36);

/// The blossom. Selection, focus, anything the eye should land on.
pub const ACCENT: Color = Color::rgb(0xE8, 0x7B, 0xA8);
/// A deeper petal, for fills behind accent text.
pub const ACCENT_DEEP: Color = Color::rgb(0x8E, 0x3D, 0x63);
/// The machine's warm cream — primary text.
pub const TEXT: Color = Color::rgb(0xEC, 0xE3, 0xD6);
/// Secondary text: timestamps, hints, counts.
pub const TEXT_DIM: Color = Color::rgb(0x9A, 0x86, 0x94);
/// Text on something that cannot be chosen.
pub const TEXT_OFF: Color = Color::rgb(0x5A, 0x4C, 0x56);

/// The scrim the in-game overlay lays over the frozen frame.
///
/// Light. The game you paused is the thing you are coming back to, and the point
/// of showing it at all is that you can see where you were — a scrim heavy enough
/// to make it a texture behind the menu may as well not be there. The menu reads
/// in front because its panel is opaque and its edge is drawn, not because the
/// game has been drowned.
pub const SCRIM: Color = Color(0x0B, 0x07, 0x0B, 0x73);

/// Corner radius for cards and buttons.
pub const RADIUS: f32 = 10.0;

/// Removal. Warm enough to belong to the same palette, red enough that nobody
/// presses it by accident.
pub const DANGER: Color = Color::rgb(0xD9, 0x45, 0x53);
