use crate::model::PersistedState;
use anyhow::{Context, Result};
use std::env;
use std::fs;
use std::path::{Path, PathBuf};

const APP_DIR: &str = "tunetui";
const STATE_FILE: &str = "state.json";

pub fn config_root() -> Result<PathBuf> {
    if let Ok(override_dir) = env::var("TUNETUI_CONFIG_DIR") {
        return Ok(PathBuf::from(override_dir));
    }

    let home = env::var("USERPROFILE").context("USERPROFILE is not set")?;
    Ok(PathBuf::from(home).join(".config").join(APP_DIR))
}

pub fn state_path() -> Result<PathBuf> {
    Ok(config_root()?.join(STATE_FILE))
}

pub fn ensure_config_dir() -> Result<PathBuf> {
    let root = config_root()?;
    fs::create_dir_all(&root).with_context(|| format!("failed to create {}", root.display()))?;
    Ok(root)
}

pub fn load_state() -> Result<PersistedState> {
    let path = state_path()?;
    if !path.exists() {
        return Ok(PersistedState::default());
    }

    let raw = fs::read_to_string(&path)
        .with_context(|| format!("failed to read state file {}", path.display()))?;
    let state: PersistedState = serde_json::from_str(&raw)
        .with_context(|| format!("failed to parse state file {}", path.display()))?;
    Ok(state)
}

pub fn save_state(state: &PersistedState) -> Result<()> {
    ensure_config_dir()?;
    let path = state_path()?;
    let json = serde_json::to_string_pretty(state)?;
    fs::write(&path, json).with_context(|| format!("failed to write {}", path.display()))?;
    Ok(())
}

pub fn normalize_path(path: &Path) -> PathBuf {
    let canonical = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    strip_windows_verbatim_prefix(&canonical)
}

pub fn strip_windows_verbatim_prefix(path: &Path) -> PathBuf {
    let raw = path.to_string_lossy();

    if let Some(trimmed) = raw.strip_prefix(r"\\?\UNC\") {
        return PathBuf::from(format!(r"\\{trimmed}"));
    }

    if let Some(trimmed) = raw.strip_prefix(r"\\?\") {
        return PathBuf::from(trimmed);
    }

    path.to_path_buf()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn save_and_load_round_trip() {
        let dir = tempdir().expect("tempdir");
        unsafe {
            env::set_var("TUNETUI_CONFIG_DIR", dir.path().to_string_lossy().as_ref());
        }

        let state = PersistedState {
            playback_mode: crate::model::PlaybackMode::Loop,
            ..PersistedState::default()
        };
        save_state(&state).expect("save");
        let loaded = load_state().expect("load");
        assert_eq!(loaded.playback_mode, crate::model::PlaybackMode::Loop);
    }

    #[test]
    fn strips_windows_verbatim_prefix() {
        let cleaned = strip_windows_verbatim_prefix(Path::new(r"\\?\E:\LOCALMUSIC\a.mp3"));
        assert_eq!(cleaned, PathBuf::from(r"E:\LOCALMUSIC\a.mp3"));
    }
}
