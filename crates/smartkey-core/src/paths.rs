// Cross-platform configuration and corpus path resolution.
//
// Linux:   $XDG_CONFIG_HOME/smartkey/ or ~/.config/smartkey/
// Windows: %APPDATA%\smartkey\
// macOS:   ~/Library/Application Support/smartkey/

use std::path::PathBuf;

/// Return the platform-specific SmartKey configuration directory.
pub fn config_dir() -> PathBuf {
    platform_config_dir().join("smartkey")
}

/// Return the path to `smartkey.json` config file.
pub fn config_file() -> PathBuf {
    config_dir().join("smartkey.json")
}

/// Return paths to all corpus files (corpus.json + corpus_*.json).
pub fn corpus_files() -> Vec<PathBuf> {
    let dir = config_dir();
    let mut files: Vec<PathBuf> = Vec::new();

    // Per-language corpora: corpus_en.json, corpus_bg.json, etc.
    if let Ok(entries) = std::fs::read_dir(&dir) {
        let mut lang_files: Vec<PathBuf> = entries
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| {
                p.extension().is_some_and(|ext| ext == "json")
                    && p.file_stem()
                        .and_then(|s| s.to_str())
                        .is_some_and(|s| s.starts_with("corpus_"))
            })
            .collect();
        lang_files.sort();
        files.append(&mut lang_files);
    }

    // Legacy single corpus.
    let legacy = dir.join("corpus.json");
    if legacy.is_file() {
        files.push(legacy);
    }

    files
}

/// Return the platform base config directory (without "smartkey" suffix).
fn platform_config_dir() -> PathBuf {
    #[cfg(target_os = "linux")]
    {
        if let Ok(xdg) = std::env::var("XDG_CONFIG_HOME") {
            return PathBuf::from(xdg);
        }
        home_dir().join(".config")
    }

    #[cfg(target_os = "windows")]
    {
        if let Ok(appdata) = std::env::var("APPDATA") {
            return PathBuf::from(appdata);
        }
        home_dir().join("AppData").join("Roaming")
    }

    #[cfg(target_os = "macos")]
    {
        home_dir().join("Library").join("Application Support")
    }

    #[cfg(not(any(target_os = "linux", target_os = "windows", target_os = "macos")))]
    {
        home_dir().join(".config")
    }
}

/// Best-effort home directory lookup.
fn home_dir() -> PathBuf {
    std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("."))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_dir_ends_with_smartkey() {
        let dir = config_dir();
        assert_eq!(dir.file_name().and_then(|s| s.to_str()), Some("smartkey"));
    }

    #[test]
    fn config_file_is_json() {
        let file = config_file();
        assert_eq!(
            file.file_name().and_then(|s| s.to_str()),
            Some("smartkey.json")
        );
    }

    #[test]
    fn corpus_files_no_panic() {
        // Smoke test: doesn't panic on a missing/empty directory.
        let _files = corpus_files();
    }
}
