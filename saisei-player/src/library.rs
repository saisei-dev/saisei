//! What the library shows: the games you have, and the saves you made in them.

use std::path::{Path, PathBuf};

pub struct Game {
    /// Bundle id — the `games/<key>/` directory name.
    pub key: String,
    /// How the game is named to a person ("Kings Bounty", not
    /// "kings_bounty_dos_en"). The window title is built the same way, so the
    /// library and the titlebar always agree.
    pub display: String,
}

/// Every bundle under `games/`, alphabetical by display name.
pub fn games(root: &Path) -> Vec<Game> {
    let mut out = Vec::new();
    let Ok(rd) = std::fs::read_dir(root.join("games")) else {
        return out;
    };
    for e in rd.flatten() {
        if !e.path().is_dir() {
            continue;
        }
        let key = e.file_name().to_string_lossy().into_owned();
        // A bundle is a directory with its config in it; anything else under
        // games/ (the README) is not a game.
        if !e.path().join(format!("{key}.json")).is_file() {
            continue;
        }
        let display = saisei_runtime::sdl::prettify_name(&key).unwrap_or_else(|| key.clone());
        out.push(Game { key, display });
    }
    out.sort_by(|a, b| a.display.to_lowercase().cmp(&b.display.to_lowercase()));
    out
}

/// Remove a game: its bundle, its build directory, and its saves.
///
/// All three, because all three are the game — a bundle left behind would come
/// straight back in the library, and saves left behind would attach themselves to
/// whatever was added under the same name next. The player is told exactly this
/// before being asked to confirm.
///
/// `saves` is passed in rather than taken from `saves_root()`, and that is not
/// ceremony: the saves live outside the repo, under the user's data directory, so
/// a version of this that reached for them itself would delete a *real* person's
/// saves the moment a test called it with a plausible-looking key. (It did. That
/// is how this signature came to be.) Now the only thing that can name the real
/// save store is the caller who means to.
///
/// `key` is a bundle directory name and nothing else: it is joined onto the roots
/// below and never used as a path, so it cannot climb out of them.
pub fn delete_game(root: &Path, saves: &Path, key: &str) -> Result<(), String> {
    if key.is_empty() || key.contains('/') || key.contains('\\') || key.contains("..") {
        return Err(format!("not a game: {key}"));
    }
    let bundle = root.join("games").join(key);
    if !bundle.join(format!("{key}.json")).is_file() {
        return Err(format!("no game called '{key}'"));
    }
    std::fs::remove_dir_all(&bundle).map_err(|e| format!("{}: {e}", bundle.display()))?;
    // The build dir and the saves are best-effort: with the bundle gone the game
    // is gone from the library either way, and failing here would only leave the
    // player looking at an error about something they cannot see.
    std::fs::remove_dir_all(root.join("build").join(key)).ok();
    std::fs::remove_dir_all(saves.join(key)).ok();
    Ok(())
}

// ---- saves ------------------------------------------------------------------

/// Where a player's saves live.
///
/// Deliberately NOT under `build/`: that is a build-artifact directory, and a
/// player's saves must survive anything that rebuilds or cleans the tree. This
/// is also a different thing from the runtime's `saves/slot_N` rewind ring,
/// which is an automatic rolling rewind buffer, not something you name and come
/// back to a week later.
pub fn saves_root() -> PathBuf {
    let base = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")))
        .unwrap_or_else(|| PathBuf::from("."));
    base.join("saisei").join("saves")
}

pub fn game_saves_dir(game_key: &str) -> PathBuf {
    saves_root().join(game_key)
}

pub struct Save {
    pub dir: PathBuf,
    /// Seconds since the epoch, from the save's own meta — not the file mtime,
    /// which a copy or a backup restore would rewrite.
    pub created_unix: u64,
    /// "12 Jul 2026, 14:03"
    pub when: String,
    /// The frame the game was showing when it was saved.
    pub thumb: Option<PathBuf>,
}

/// A game's saves, newest first.
pub fn saves(game_key: &str) -> Vec<Save> {
    let mut out = Vec::new();
    let Ok(rd) = std::fs::read_dir(game_saves_dir(game_key)) else {
        return out;
    };
    for e in rd.flatten() {
        let dir = e.path();
        if !dir.is_dir() {
            continue;
        }
        // A save is only a save if the snapshot itself is there. A half-written
        // bundle (killed mid-save) has no memory image and must not be offered.
        if !dir.join("pre_last_key.bin").is_file() {
            continue;
        }
        let meta: serde_json::Value = std::fs::read(dir.join("meta.json"))
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or(serde_json::Value::Null);
        let created_unix = meta
            .get("created_unix")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0);
        let thumb = Some(dir.join("thumb.png")).filter(|p| p.is_file());
        out.push(Save {
            when: format_when(created_unix),
            dir,
            created_unix,
            thumb,
        });
    }
    out.sort_by(|a, b| b.created_unix.cmp(&a.created_unix));
    out
}

/// A fresh, unused save directory for `game_key`.
///
/// Named by UTC timestamp so the directory listing sorts chronologically, with a
/// counter suffix so two saves in the same second cannot collide.
pub fn new_save_dir(game_key: &str) -> PathBuf {
    let root = game_saves_dir(game_key);
    std::fs::create_dir_all(&root).ok();
    let secs = now_unix();
    let (y, mo, d, h, mi, s) = civil_from_unix(secs);
    let stem = format!("{y:04}{mo:02}{d:02}-{h:02}{mi:02}{s:02}");
    let mut n = 0;
    loop {
        let cand = root.join(if n == 0 {
            stem.clone()
        } else {
            format!("{stem}-{n}")
        });
        if !cand.exists() {
            return cand;
        }
        n += 1;
    }
}

pub fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

const MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

/// How a save's time is shown to the person who made it.
///
/// In *their* timezone, not UTC: a save made at ten past two in the afternoon has
/// to read as ten past two, or the list is useless for the one thing it is for —
/// telling you which of these is the one you were just playing. (The save's own
/// meta.json keeps the unix seconds and a UTC stamp; that is storage, this is
/// for reading.)
fn format_when(unix: u64) -> String {
    if unix == 0 {
        return "unknown".into();
    }
    let t = unix as libc::time_t;
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    // localtime_r honours TZ; if it fails for any reason, fall back to UTC rather
    // than show nothing.
    let (y, mo, d, h, mi) = if unsafe { !libc::localtime_r(&t, &mut tm).is_null() } {
        (
            tm.tm_year as i64 + 1900,
            tm.tm_mon as u32 + 1,
            tm.tm_mday as u32,
            tm.tm_hour as u32,
            tm.tm_min as u32,
        )
    } else {
        let (y, mo, d, h, mi, _) = civil_from_unix(unix);
        (y, mo, d, h, mi)
    };
    format!(
        "{d} {} {y}, {h:02}:{mi:02}",
        MONTHS[(mo as usize - 1).min(11)]
    )
}

/// Unix seconds → (y, m, d, h, m, s), UTC. Howard Hinnant's civil_from_days.
///
/// Hand-rolled rather than pulled from `chrono`/`time`: the whole toolchain's
/// system prerequisites are a C linker and SDL2, and a date formatter is not
/// worth widening that.
pub fn civil_from_unix(unix: u64) -> (i64, u32, u32, u32, u32, u32) {
    let days = (unix / 86_400) as i64;
    let secs_of_day = unix % 86_400;
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let y = if m <= 2 { y + 1 } else { y };
    (
        y,
        m,
        d,
        (secs_of_day / 3600) as u32,
        (secs_of_day % 3600 / 60) as u32,
        (secs_of_day % 60) as u32,
    )
}

/// ISO-8601 UTC, for the save's own meta.json.
pub fn iso8601(unix: u64) -> String {
    let (y, mo, d, h, mi, s) = civil_from_unix(unix);
    format!("{y:04}-{mo:02}-{d:02}T{h:02}:{mi:02}:{s:02}Z")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn civil_from_unix_matches_known_instants() {
        assert_eq!(civil_from_unix(0), (1970, 1, 1, 0, 0, 0));
        // `date -u -d @1783864989` => 2026-07-12 14:03:09
        assert_eq!(civil_from_unix(1_783_864_989), (2026, 7, 12, 14, 3, 9));
        // A leap day, to catch the era arithmetic.
        assert_eq!(civil_from_unix(1_709_164_800), (2024, 2, 29, 0, 0, 0));
    }

    #[test]
    fn iso8601_is_zero_padded() {
        assert_eq!(iso8601(1_709_164_800), "2024-02-29T00:00:00Z");
    }

    /// A sandbox with two games in it, each with a save.
    fn two_games(tag: &str) -> (PathBuf, PathBuf) {
        let root = std::env::temp_dir().join(format!("saisei_{tag}_{}", std::process::id()));
        std::fs::remove_dir_all(&root).ok();
        let saves = root.join("saves");
        for g in ["gone", "kept"] {
            let bundle = root.join("games").join(g);
            std::fs::create_dir_all(bundle.join("DATA")).unwrap();
            std::fs::write(bundle.join(format!("{g}.json")), b"{}").unwrap();
            std::fs::write(bundle.join("DATA/x.dat"), b"x").unwrap();
            std::fs::create_dir_all(root.join("build").join(g).join("jit")).unwrap();
            std::fs::create_dir_all(saves.join(g).join("20260101-000000")).unwrap();
            std::fs::write(saves.join(g).join("20260101-000000/pre_last_key.bin"), b"m").unwrap();
        }
        (root, saves)
    }

    #[test]
    fn delete_game_removes_the_bundle_its_build_dir_and_its_saves() {
        let (root, saves) = two_games("del");

        delete_game(&root, &saves, "gone").unwrap();
        assert!(!root.join("games/gone").exists(), "the bundle is gone");
        assert!(!root.join("build/gone").exists(), "and its build dir");
        assert!(!saves.join("gone").exists(), "and its saves");

        // And nothing of the other game was touched. This is the whole reason
        // `saves` is a parameter: a version that reached for the real save store
        // itself deleted a real game's saves the first time a test ran.
        assert!(root.join("games/kept/kept.json").is_file());
        assert!(root.join("build/kept").exists());
        assert!(saves
            .join("kept/20260101-000000/pre_last_key.bin")
            .is_file());

        // A second removal is an error, not a silent success — the game is not there.
        assert!(delete_game(&root, &saves, "gone").is_err());

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn delete_game_refuses_anything_that_is_not_a_bundle_name() {
        let (root, saves) = two_games("del2");
        // `key` is joined onto games/, build/ and the save store, so it must never
        // be able to climb out of them.
        for bad in ["", "..", "../../etc", "a/b", "..\\x", "../kept"] {
            assert!(delete_game(&root, &saves, bad).is_err(), "accepted {bad:?}");
        }
        assert!(root.join("games/kept/kept.json").is_file());
        assert!(saves.join("kept").exists());
        std::fs::remove_dir_all(&root).ok();
    }
}
