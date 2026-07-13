//! Player settings that outlive a session: today, volume.
//!
//! One file, `$XDG_DATA_HOME/saisei/settings.json`, holding a default and a
//! per-game map:
//!
//! ```json
//! { "default_volume": 0.6, "volume": { "zeliard_dos_en": 0.35 } }
//! ```
//!
//! Per game and not just global, because loudness is a property of the game and
//! not of the player: a beeper game and an AdLib game mastered a decade apart do
//! not arrive at the same level, and a volume you set once for one of them is the
//! wrong volume for the next one you open. The default is what a game gets the
//! first time it is ever run, and what it keeps until you say otherwise.
//!
//! Missing, unreadable and corrupt all mean the same thing — defaults — because a
//! settings file is never worth failing to start a game over.

use serde_json::Value;
use std::path::PathBuf;

/// What a game plays at until someone moves the slider.
pub const DEFAULT_VOLUME: f32 = 0.6;

fn data_root() -> PathBuf {
    let base = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")))
        .unwrap_or_else(|| PathBuf::from("."));
    base.join("saisei")
}

pub fn settings_path() -> PathBuf {
    data_root().join("settings.json")
}

fn load() -> Value {
    std::fs::read(settings_path())
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or(Value::Null)
}

/// The volume `game_key` should play at: its own if it has one, else the default.
pub fn volume_for(game_key: &str) -> f32 {
    let v = load();
    let per_game = v
        .get("volume")
        .and_then(|m| m.get(game_key))
        .and_then(Value::as_f64);
    let default = v
        .get("default_volume")
        .and_then(Value::as_f64)
        .unwrap_or(DEFAULT_VOLUME as f64);
    per_game.unwrap_or(default).clamp(0.0, 1.0) as f32
}

/// Remember `volume` for `game_key`. Read-modify-write, so a field this build
/// does not know about survives being written by one that does.
pub fn set_volume_for(game_key: &str, volume: f32) {
    let mut v = load();
    if !v.is_object() {
        v = serde_json::json!({});
    }
    let volume = volume.clamp(0.0, 1.0);
    v["volume"][game_key] = serde_json::json!(volume);
    if v.get("default_volume").is_none() {
        v["default_volume"] = serde_json::json!(DEFAULT_VOLUME);
    }
    write(&v);
}

fn write(v: &Value) {
    let path = settings_path();
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Ok(bytes) = serde_json::to_vec_pretty(v) {
        // Write beside it and rename, so a settings file is never left half
        // written by a player that was killed mid-save.
        let tmp = path.with_extension("json.tmp");
        if std::fs::write(&tmp, bytes).is_ok() {
            let _ = std::fs::rename(&tmp, &path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The settings file is global state on disk, so these have to take turns
    /// with it and put it back.
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn with_temp_home<T>(f: impl FnOnce() -> T) -> T {
        let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = std::env::temp_dir().join(format!("saisei-settings-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let prev = std::env::var_os("XDG_DATA_HOME");
        std::env::set_var("XDG_DATA_HOME", &dir);
        let out = f();
        match prev {
            Some(p) => std::env::set_var("XDG_DATA_HOME", p),
            None => std::env::remove_var("XDG_DATA_HOME"),
        }
        let _ = std::fs::remove_dir_all(&dir);
        out
    }

    #[test]
    fn unset_game_gets_the_default() {
        with_temp_home(|| {
            assert_eq!(volume_for("never_played"), DEFAULT_VOLUME);
        });
    }

    #[test]
    fn a_games_volume_is_remembered_and_is_its_own() {
        with_temp_home(|| {
            set_volume_for("zeliard_dos_en", 0.25);
            assert_eq!(volume_for("zeliard_dos_en"), 0.25);
            // ...and has not become everyone's volume.
            assert_eq!(volume_for("alleycat"), DEFAULT_VOLUME);

            set_volume_for("alleycat", 0.9);
            assert_eq!(volume_for("alleycat"), 0.9);
            assert_eq!(volume_for("zeliard_dos_en"), 0.25);
        });
    }

    #[test]
    fn a_corrupt_file_is_not_a_dead_player() {
        with_temp_home(|| {
            let p = settings_path();
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(&p, b"{ this is not json").unwrap();
            assert_eq!(volume_for("anything"), DEFAULT_VOLUME);
            // And it heals: writing repairs the file rather than refusing.
            set_volume_for("anything", 0.4);
            assert_eq!(volume_for("anything"), 0.4);
        });
    }
}
