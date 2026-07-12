//! The library shell. Placeholder until the UI layer lands.

use std::path::Path;

pub fn run(root: &Path) -> ! {
    for g in crate::library::games(root) {
        println!("{:<24} {}", g.key, g.display);
    }
    std::process::exit(0);
}
