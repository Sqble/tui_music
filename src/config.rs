use crate::model::PersistedState;
use anyhow::{Context, Result};
use std::env;
use std::fs;
use std::path::Component;
use std::path::{Path, PathBuf};
#[cfg(test)]
use std::sync::OnceLock;

const APP_DIR: &str = "tunetui";
const STATE_FILE: &str = "state.json";
const STATS_FILE: &str = "stats.json";
const LYRICS_DIR: &str = "lyrics";

pub fn config_root() -> Result<PathBuf> {
    #[cfg(test)]
    {
        if env::var("TUNETUI_CONFIG_DIR").is_err() {
            return Ok(test_config_root());
        }
    }

    if let Ok(override_dir) = env::var("TUNETUI_CONFIG_DIR") {
        return Ok(PathBuf::from(override_dir));
    }

    let home = env::var("USERPROFILE").context("USERPROFILE is not set")?;
    Ok(PathBuf::from(home).join(".config").join(APP_DIR))
}

#[cfg(test)]
fn test_config_root() -> PathBuf {
    static TEST_CONFIG_ROOT: OnceLock<PathBuf> = OnceLock::new();
    TEST_CONFIG_ROOT
        .get_or_init(|| {
            let root = env::temp_dir().join(format!("tunetui-test-{}", std::process::id()));
            let _ = fs::create_dir_all(&root);
            root
        })
        .clone()
}

pub fn state_path() -> Result<PathBuf> {
    Ok(config_root()?.join(STATE_FILE))
}

pub fn ensure_config_dir() -> Result<PathBuf> {
    let root = config_root()?;
    fs::create_dir_all(&root).with_context(|| format!("failed to create {}", root.display()))?;
    Ok(root)
}

pub fn stats_path() -> Result<PathBuf> {
    Ok(config_root()?.join(STATS_FILE))
}

pub fn lyrics_root() -> Result<PathBuf> {
    Ok(config_root()?.join(LYRICS_DIR))
}

pub fn ensure_lyrics_dir() -> Result<PathBuf> {
    let root = lyrics_root()?;
    fs::create_dir_all(&root).with_context(|| format!("failed to create {}", root.display()))?;
    Ok(root)
}

pub fn lyrics_path_for_track(track_path: &Path) -> Result<PathBuf> {
    let normalized = normalize_path(track_path);
    let normalized_display = sanitize_display_text(&normalized.to_string_lossy());
    let hash = stable_fnv1a_64(&normalized_display);

    let stem = track_path
        .file_stem()
        .and_then(|name| name.to_str())
        .map(sanitize_lyrics_stem)
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| String::from("track"));

    Ok(lyrics_root()?.join(format!("{stem}-{hash:016x}.lrc")))
}

pub fn load_state() -> Result<PersistedState> {
    let path = state_path()?;
    load_state_from_path(&path)
}

fn load_state_from_path(path: &Path) -> Result<PersistedState> {
    if !path.exists() {
        return Ok(PersistedState::default());
    }

    let raw = fs::read_to_string(path)
        .with_context(|| format!("failed to read state file {}", path.display()))?;
    let mut state: PersistedState = serde_json::from_str(&raw)
        .with_context(|| format!("failed to parse state file {}", path.display()))?;

    state.folders = state
        .folders
        .iter()
        .map(|folder| recover_existing_path(folder))
        .collect();
    for playlist in state.playlists.values_mut() {
        playlist.tracks = playlist
            .tracks
            .iter()
            .map(|track| recover_existing_path(track))
            .collect();
    }

    Ok(state)
}

pub fn save_state(state: &PersistedState) -> Result<()> {
    ensure_config_dir()?;
    let path = state_path()?;
    save_state_to_path(&path, state)
}

fn save_state_to_path(path: &Path, state: &PersistedState) -> Result<()> {
    if path.exists() {
        let backup = path.with_extension("json.bak");
        let _ = fs::copy(path, &backup);
    }
    let json = serde_json::to_string_pretty(state)?;
    fs::write(path, json).with_context(|| format!("failed to write {}", path.display()))?;
    Ok(())
}

pub fn normalize_path(path: &Path) -> PathBuf {
    let canonical = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    strip_windows_verbatim_prefix(&canonical)
}

pub fn sanitize_user_folder_path(path: &Path) -> PathBuf {
    let raw = path.to_string_lossy();
    let mut cleaned = raw.trim();
    cleaned = cleaned.trim_start_matches('\u{feff}');

    let marker_stripped = strip_copy_markers_anywhere(cleaned);
    cleaned = marker_stripped.as_str();

    for marker in ['•', '●', '◦', '▪'] {
        if let Some(rest) = cleaned.strip_prefix(marker) {
            cleaned = rest.trim_start();
            return PathBuf::from(cleaned);
        }
    }

    for marker in ['*', '-'] {
        match cleaned.strip_prefix(marker) {
            Some(rest) if rest.starts_with(char::is_whitespace) => {
                cleaned = rest.trim_start();
                return PathBuf::from(cleaned);
            }
            _ => {}
        }
    }

    PathBuf::from(cleaned)
}

pub fn resolve_existing_path(path: &Path) -> PathBuf {
    recover_existing_path(path)
}

fn strip_copy_markers_anywhere(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut skipping_ws_after_marker = false;

    for ch in input.chars() {
        if skipping_ws_after_marker {
            if ch.is_whitespace() {
                continue;
            }
            skipping_ws_after_marker = false;
        }

        if matches!(ch, '•' | '●' | '◦' | '▪') {
            skipping_ws_after_marker = true;
            continue;
        }

        out.push(ch);
    }

    out
}

pub fn sanitize_display_text(input: &str) -> String {
    input
        .chars()
        .filter(|ch| {
            !ch.is_control()
                && !matches!(
                    ch,
                    '\u{200B}'
                        | '\u{200C}'
                        | '\u{200D}'
                        | '\u{200E}'
                        | '\u{200F}'
                        | '\u{202A}'
                        | '\u{202B}'
                        | '\u{202C}'
                        | '\u{202D}'
                        | '\u{202E}'
                        | '\u{2066}'
                        | '\u{2067}'
                        | '\u{2068}'
                        | '\u{2069}'
                        | '\u{FEFF}'
                )
        })
        .collect()
}

fn recover_existing_path(path: &Path) -> PathBuf {
    if path.exists() {
        return path.to_path_buf();
    }

    let display_clean = PathBuf::from(sanitize_display_text(&path.to_string_lossy()));
    if display_clean.exists() {
        return display_clean;
    }

    let user_clean = sanitize_user_folder_path(path);
    if user_clean.exists() {
        return user_clean;
    }

    if let Some(recovered) = recover_existing_path_by_components(path) {
        return recovered;
    }

    path.to_path_buf()
}

fn recover_existing_path_by_components(path: &Path) -> Option<PathBuf> {
    let mut rebuilt = PathBuf::new();
    let total_components = path
        .components()
        .filter(|component| matches!(component, Component::Normal(_)))
        .count();
    let mut normal_index = 0usize;

    for component in path.components() {
        match component {
            Component::Prefix(prefix) => rebuilt.push(prefix.as_os_str()),
            Component::RootDir => rebuilt.push(component.as_os_str()),
            Component::CurDir => {}
            Component::ParentDir => {
                rebuilt.pop();
            }
            Component::Normal(part) => {
                normal_index = normal_index.saturating_add(1);
                let is_last = normal_index == total_components;
                let parent = if rebuilt.as_os_str().is_empty() {
                    PathBuf::from(".")
                } else {
                    rebuilt.clone()
                };

                let exact = parent.join(part);
                if exact.exists() {
                    rebuilt = exact;
                    continue;
                }

                let want_dir = if is_last { None } else { Some(true) };
                let matched = find_matching_child(&parent, part, want_dir)?;
                rebuilt = parent.join(matched);
            }
        }
    }

    if rebuilt.exists() {
        Some(rebuilt)
    } else {
        None
    }
}

fn find_matching_child(
    parent: &Path,
    target: &std::ffi::OsStr,
    want_dir: Option<bool>,
) -> Option<PathBuf> {
    let target_text = target.to_string_lossy();
    let target_clean = clean_component_for_compare(&target_text);
    let target_key = component_match_key(&target_text);

    let mut exact_casefold_matches = Vec::new();
    let mut clean_matches = Vec::new();
    let mut key_matches = Vec::new();

    let read_dir = fs::read_dir(parent).ok()?;
    for entry in read_dir.filter_map(|result| result.ok()) {
        if let Some(expected_dir) = want_dir {
            let is_dir = entry.file_type().ok().map(|kind| kind.is_dir())?;
            if is_dir != expected_dir {
                continue;
            }
        }

        let name = entry.file_name();
        let name_text = name.to_string_lossy();
        if name_text.eq_ignore_ascii_case(&target_text) {
            exact_casefold_matches.push(name.clone());
            continue;
        }

        let name_clean = clean_component_for_compare(&name_text);
        if !target_clean.is_empty() && !name_clean.is_empty() && name_clean == target_clean {
            clean_matches.push(name.clone());
            continue;
        }

        if !target_key.is_empty() {
            let name_key = component_match_key(&name_text);
            if !name_key.is_empty() && name_key == target_key {
                key_matches.push(name);
            }
        }
    }

    if exact_casefold_matches.len() == 1 {
        return Some(PathBuf::from(exact_casefold_matches.remove(0)));
    }
    if clean_matches.len() == 1 {
        return Some(PathBuf::from(clean_matches.remove(0)));
    }
    if key_matches.len() == 1 {
        return Some(PathBuf::from(key_matches.remove(0)));
    }

    None
}

fn clean_component_for_compare(input: &str) -> String {
    let marker_stripped = strip_copy_markers_anywhere(&sanitize_display_text(input));
    marker_stripped.trim().to_ascii_lowercase()
}

fn component_match_key(input: &str) -> String {
    clean_component_for_compare(input)
        .chars()
        .flat_map(char::to_lowercase)
        .filter(|ch| ch.is_alphanumeric())
        .collect()
}

fn sanitize_lyrics_stem(input: &str) -> String {
    let mut out = String::with_capacity(input.len().min(40));
    for ch in input.chars() {
        if out.len() >= 40 {
            break;
        }
        if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_') {
            out.push(ch);
        } else if ch.is_ascii_whitespace() {
            out.push('_');
        }
    }
    out
}

fn stable_fnv1a_64(input: &str) -> u64 {
    let mut hash: u64 = 0xcbf29ce484222325;
    for byte in input.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
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
        let path = dir.path().join(STATE_FILE);

        let state = PersistedState {
            playback_mode: crate::model::PlaybackMode::Loop,
            ..PersistedState::default()
        };
        save_state_to_path(&path, &state).expect("save");
        let loaded = load_state_from_path(&path).expect("load");
        assert_eq!(loaded.playback_mode, crate::model::PlaybackMode::Loop);
    }

    #[test]
    fn strips_windows_verbatim_prefix() {
        let cleaned = strip_windows_verbatim_prefix(Path::new(r"\\?\E:\LOCALMUSIC\a.mp3"));
        assert_eq!(cleaned, PathBuf::from(r"E:\LOCALMUSIC\a.mp3"));
    }

    #[test]
    fn sanitize_user_folder_path_strips_leading_bullet_symbol() {
        let cleaned = sanitize_user_folder_path(Path::new("• E:\\LOCALMUSIC"));
        assert_eq!(cleaned, PathBuf::from(r"E:\LOCALMUSIC"));
    }

    #[test]
    fn sanitize_user_folder_path_strips_leading_bullet_without_space() {
        let cleaned = sanitize_user_folder_path(Path::new("•E:\\LOCALMUSIC"));
        assert_eq!(cleaned, PathBuf::from(r"E:\LOCALMUSIC"));
    }

    #[test]
    fn sanitize_display_text_removes_directional_control_chars() {
        let cleaned = sanitize_display_text("A\u{202E}B\u{200B}C");
        assert_eq!(cleaned, "ABC");
    }

    #[test]
    fn sanitize_user_folder_path_preserves_internal_characters() {
        let cleaned = sanitize_user_folder_path(Path::new("E:\\MUSIC\\4 \u{0007} WOD"));
        assert_eq!(cleaned, PathBuf::from("E:\\MUSIC\\4 \u{0007} WOD"));
    }

    #[test]
    fn sanitize_user_folder_path_preserves_real_leading_dash_names() {
        let cleaned = sanitize_user_folder_path(Path::new("-mixes"));
        assert_eq!(cleaned, PathBuf::from("-mixes"));
    }

    #[test]
    fn sanitize_user_folder_path_strips_bullets_inside_path() {
        let cleaned = sanitize_user_folder_path(Path::new(r"E:\MUSIC\•Albums\▪Live"));
        assert_eq!(cleaned, PathBuf::from(r"E:\MUSIC\Albums\Live"));
    }

    #[test]
    fn recover_existing_path_uses_clean_display_variant_when_original_missing() {
        let dir = tempdir().expect("tempdir");
        let existing = dir.path().join("A B");
        fs::create_dir_all(&existing).expect("create dir");

        let missing = PathBuf::from(existing.to_string_lossy().replace("A B", "A\u{0007} B"));
        let recovered = recover_existing_path(&missing);
        assert_eq!(recovered, existing);
    }

    #[test]
    fn recover_existing_path_recovers_directory_with_incompatible_middle_characters() {
        let dir = tempdir().expect("tempdir");
        let existing = dir.path().join("Albums").join("Live");
        fs::create_dir_all(&existing).expect("create dir");

        let broken = PathBuf::from(
            existing
                .to_string_lossy()
                .replace("Albums", "A•lbums")
                .replace("Live", "L\u{202E}ive"),
        );

        let recovered = recover_existing_path(&broken);
        assert_eq!(recovered, existing);
    }

    #[test]
    fn recover_existing_path_recovers_file_with_incompatible_middle_characters() {
        let dir = tempdir().expect("tempdir");
        let albums = dir.path().join("Albums");
        fs::create_dir_all(&albums).expect("create dir");
        let existing = albums.join("track_01.mp3");
        fs::write(&existing, b"x").expect("write file");

        let broken = PathBuf::from(
            existing
                .to_string_lossy()
                .replace("Albums", "Alb▪ums")
                .replace("track_01.mp3", "tra\u{200B}ck_01.mp3"),
        );

        let recovered = recover_existing_path(&broken);
        assert_eq!(recovered, existing);
    }

    #[test]
    fn lyrics_path_for_track_uses_config_lyrics_directory() {
        let path =
            lyrics_path_for_track(Path::new(r"D:\Music\Artist\song.mp3")).expect("lyrics path");
        assert!(path.to_string_lossy().contains("tunetui"));
        assert!(path.to_string_lossy().contains("lyrics"));
        assert!(path.extension().and_then(|ext| ext.to_str()) == Some("lrc"));
        let filename = path
            .file_name()
            .and_then(|name| name.to_str())
            .expect("filename");
        assert!(filename.starts_with("song-"));
    }
}
