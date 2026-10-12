//! Where the program keeps its files when no paths are given.
//!
//! In a checkout (a `cities.csv` in the current directory) that is the current directory. Otherwise
//! it is a per-user folder, seeded on first run with the default config and cities.

use std::path::{Path, PathBuf};

const CONFIG: &str = include_str!("../assets/config.toml");
const CITIES: &str = include_str!("../assets/cities.csv");

fn user_dir() -> PathBuf {
    let env = |k: &str| std::env::var_os(k).filter(|v| !v.is_empty()).map(PathBuf::from);
    let base = if cfg!(target_os = "windows") {
        env("APPDATA")
    } else if cfg!(target_os = "macos") {
        env("HOME").map(|h| h.join("Library/Application Support"))
    } else {
        env("XDG_DATA_HOME").or_else(|| env("HOME").map(|h| h.join(".local/share")))
    };
    base.unwrap_or_else(|| ".".into()).join("wedgewalks")
}

/// The folder holding config.toml, cities.csv, data/ and out/ unless overridden.
pub fn dir() -> PathBuf {
    if Path::new("cities.csv").exists() { ".".into() } else { user_dir() }
}

/// Writes the default config and cities into `dir` where missing.
pub fn seed(dir: &Path, config: &Path, cities: &Path) {
    let _ = std::fs::create_dir_all(dir);
    for (path, text) in [(config, CONFIG), (cities, CITIES)] {
        if !path.exists() {
            let _ = std::fs::write(path, text);
        }
    }
}
