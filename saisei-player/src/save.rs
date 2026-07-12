//! Writing a player's save.
//!
//! A save is a whole-machine snapshot — the runtime's bundle — plus the two
//! things a person needs to tell one save from another: when it was made, and
//! what was on screen.
//!
//! This is not the runtime's `saves/slot_N` ring. That is an automatic rolling
//! rewind buffer with age-based promotion, and it lives under `build/`. These are
//! saves you make on purpose, they never overwrite each other, and they live
//! somewhere a rebuild cannot delete them.

use std::ffi::CString;
use std::path::Path;

use saisei_runtime::save_manager::save_manager_can_save_now;
use saisei_runtime::sdl::saisei_last_frame;
use saisei_runtime::snapshot::snapshot_capture_and_write;

use crate::library;

/// Thumbnails are stored at the guest's own resolution — a 320x200 frame is
/// already thumbnail-sized, and downscaling it here would only throw away detail
/// the interface might want on a large display.
const THUMB_MAX_W: u32 = 640;

/// Snapshot the running game into a new save.
///
/// The caller must be inside the overlay: the machine is frozen at a dispatcher
/// resting point, which is the only place a snapshot is valid at all. We check
/// anyway rather than write a bundle that would refuse to restore.
pub fn write(root: &Path, game_key: &str) -> Result<(), String> {
    let _ = root;
    if save_manager_can_save_now() == 0 {
        return Err("the game is not at a point that can be saved".into());
    }

    let dir = library::new_save_dir(game_key);
    std::fs::create_dir_all(&dir).map_err(|e| format!("{e}"))?;

    let c_dir = CString::new(dir.display().to_string()).map_err(|e| format!("{e}"))?;
    if snapshot_capture_and_write(c_dir.as_ptr()) != 0 {
        // Don't leave a half-written bundle behind to be offered as a save.
        std::fs::remove_dir_all(&dir).ok();
        return Err("the snapshot could not be written".into());
    }

    let now = library::now_unix();
    let meta = serde_json::json!({
        "game": game_key,
        "created_unix": now,
        "created_utc": library::iso8601(now),
    });
    std::fs::write(
        dir.join("meta.json"),
        serde_json::to_vec_pretty(&meta).unwrap_or_default(),
    )
    .map_err(|e| format!("{e}"))?;

    write_thumb(&dir.join("thumb.png"));
    Ok(())
}

/// The frame the player was looking at when they saved.
///
/// Taken from the runtime's *presented* frame — the pixels that went to the
/// window, with the video mode already resolved — and not from
/// `shim_render_screenshot_png`, which re-reads guest VRAM as a linear 320x200 at
/// 0xA000 with no planar branch and would hand us garbage for an EGA game.
///
/// A missing thumbnail is not an error: the save is still a save, and the
/// interface falls back to a plain tile.
fn write_thumb(path: &Path) {
    let (mut w, mut h) = (0i32, 0i32);
    let px = saisei_last_frame(&mut w, &mut h);
    if px.is_null() || w <= 0 || h <= 0 {
        return;
    }
    let (w, h) = (w as u32, h as u32);
    let rgb = unsafe { std::slice::from_raw_parts(px, (w * h * 3) as usize) }.to_vec();
    let Some(img) = image::RgbImage::from_raw(w, h, rgb) else {
        return;
    };
    let img = if w > THUMB_MAX_W {
        image::imageops::resize(
            &img,
            THUMB_MAX_W,
            h * THUMB_MAX_W / w,
            image::imageops::FilterType::CatmullRom,
        )
    } else {
        img
    };
    img.save(path).ok();
}
