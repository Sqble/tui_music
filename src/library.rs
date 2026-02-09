use crate::model::Track;
use std::ffi::OsStr;
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};
use symphonia::core::formats::FormatOptions;
use symphonia::core::io::{MediaSourceStream, MediaSourceStreamOptions};
use symphonia::core::meta::{MetadataOptions, StandardTagKey};
use symphonia::core::probe::Hint;
use symphonia::default::get_probe;
use walkdir::WalkDir;

const AUDIO_EXTENSIONS: &[&str] = &["mp3", "flac", "wav", "ogg", "m4a", "aac", "opus"];

#[derive(Default)]
struct TrackMetadata {
    title: Option<String>,
    artist: Option<String>,
    album: Option<String>,
}

pub fn scan_folder(root: &Path) -> Vec<Track> {
    let mut tracks = Vec::new();

    for entry in WalkDir::new(root)
        .follow_links(true)
        .into_iter()
        .filter_map(Result::ok)
    {
        let path = entry.path();
        if !entry.file_type().is_file() || !is_audio(path) {
            continue;
        }

        let metadata = metadata_for(path);
        let title = metadata
            .title
            .filter(|title| !title.trim().is_empty())
            .unwrap_or_else(|| {
                path.file_stem()
                    .and_then(OsStr::to_str)
                    .unwrap_or("unknown")
                    .to_string()
            });

        tracks.push(Track {
            path: PathBuf::from(path),
            title,
            artist: metadata.artist,
            album: metadata.album,
        });
    }

    tracks.sort_by(|a, b| a.path.cmp(&b.path));
    tracks
}

fn metadata_for(path: &Path) -> TrackMetadata {
    let stripped = crate::config::strip_windows_verbatim_prefix(path);
    let symphonia_meta = symphonia_metadata(&stripped);
    if symphonia_meta.title.is_some()
        || symphonia_meta.artist.is_some()
        || symphonia_meta.album.is_some()
    {
        return symphonia_meta;
    }
    id3v2_fallback(&stripped)
}

fn symphonia_metadata(path: &Path) -> TrackMetadata {
    let Ok(file) = File::open(path) else {
        return TrackMetadata::default();
    };
    let source = MediaSourceStream::new(Box::new(file), MediaSourceStreamOptions::default());

    let mut hint = Hint::new();
    if let Some(extension) = path.extension().and_then(OsStr::to_str) {
        hint.with_extension(extension);
    }

    let Ok(mut probed) = get_probe().format(
        &hint,
        source,
        &FormatOptions::default(),
        &MetadataOptions::default(),
    ) else {
        return TrackMetadata::default();
    };

    let metadata = probed.format.metadata();
    let Some(revision) = metadata.current() else {
        return TrackMetadata::default();
    };

    let tags = revision.tags();

    let title = tag_value(tags, StandardTagKey::TrackTitle, &["title"]);
    let artist = tag_value(
        tags,
        StandardTagKey::Artist,
        &["artist", "albumartist", "album_artist"],
    );
    let album = tag_value(tags, StandardTagKey::Album, &["album"]);

    TrackMetadata {
        title,
        artist,
        album,
    }
}

fn id3v2_fallback(path: &Path) -> TrackMetadata {
    let mut file = match File::open(path) {
        Ok(f) => f,
        Err(_) => return TrackMetadata::default(),
    };
    let mut header = [0u8; 10];
    if file.read_exact(&mut header).is_err() {
        return TrackMetadata::default();
    }
    if !header.starts_with(b"ID3") {
        return TrackMetadata::default();
    }
    let major_version = header[3];
    let size = {
        let bytes = &header[6..10];
        ((bytes[0] as u32) & 0x7f) << 21
            | ((bytes[1] as u32) & 0x7f) << 14
            | ((bytes[2] as u32) & 0x7f) << 7
            | ((bytes[3] as u32) & 0x7f)
    } as usize;
    let mut tag_bytes = vec![0u8; size];
    if file.read_exact(&mut tag_bytes).is_err() {
        return TrackMetadata::default();
    }
    let mut pos = 0;
    let mut title = None;
    let mut artist = None;
    let mut album = None;
    while pos < tag_bytes.len() {
        let (frame_id, frame_size, data_start) = if major_version == 2 {
            if pos + 6 > tag_bytes.len() {
                break;
            }
            let frame_id = std::str::from_utf8(&tag_bytes[pos..pos + 3]).unwrap_or("");
            let bytes = &tag_bytes[pos + 3..pos + 6];
            let frame_size =
                ((bytes[0] as u32) << 16 | (bytes[1] as u32) << 8 | (bytes[2] as u32)) as usize;
            (frame_id, frame_size, pos + 6)
        } else {
            if pos + 10 > tag_bytes.len() {
                break;
            }
            let frame_id = std::str::from_utf8(&tag_bytes[pos..pos + 4]).unwrap_or("");
            let bytes = &tag_bytes[pos + 4..pos + 8];
            let frame_size = if major_version == 4 {
                (((bytes[0] as u32) & 0x7f) << 21
                    | ((bytes[1] as u32) & 0x7f) << 14
                    | ((bytes[2] as u32) & 0x7f) << 7
                    | ((bytes[3] as u32) & 0x7f)) as usize
            } else {
                ((bytes[0] as u32) << 24
                    | (bytes[1] as u32) << 16
                    | (bytes[2] as u32) << 8
                    | (bytes[3] as u32)) as usize
            };
            (frame_id, frame_size, pos + 10)
        };

        if frame_id.trim_matches('\0').is_empty() || frame_size == 0 {
            break;
        }

        let data_end = data_start + frame_size;
        if data_end > tag_bytes.len() {
            break;
        }
        let payload = &tag_bytes[data_start..data_end];

        let text = decode_id3_text(payload);
        if !text.is_empty() {
            match frame_id {
                "TIT2" | "TT2" => title = Some(text),
                "TPE1" | "TP1" => artist = Some(text),
                "TALB" | "TAL" => album = Some(text),
                _ => {}
            }
        }
        pos = data_end;
    }
    TrackMetadata {
        title,
        artist,
        album,
    }
}

fn decode_id3_text(payload: &[u8]) -> String {
    if payload.is_empty() {
        return String::new();
    }

    let encoding = payload[0];
    let bytes = &payload[1..];

    let text = match encoding {
        0 => bytes.iter().map(|b| char::from(*b)).collect::<String>(),
        1 => decode_utf16_with_bom(bytes),
        2 => decode_utf16(bytes, true),
        3 => String::from_utf8_lossy(bytes).into_owned(),
        _ => String::from_utf8_lossy(payload).into_owned(),
    };

    text.trim_matches('\0').trim().to_string()
}

fn decode_utf16_with_bom(bytes: &[u8]) -> String {
    if bytes.len() >= 2 {
        if bytes[0] == 0xFE && bytes[1] == 0xFF {
            return decode_utf16(&bytes[2..], true);
        }
        if bytes[0] == 0xFF && bytes[1] == 0xFE {
            return decode_utf16(&bytes[2..], false);
        }
    }
    decode_utf16(bytes, false)
}

fn decode_utf16(bytes: &[u8], big_endian: bool) -> String {
    let mut units = Vec::with_capacity(bytes.len() / 2);
    for pair in bytes.chunks_exact(2) {
        let value = if big_endian {
            u16::from_be_bytes([pair[0], pair[1]])
        } else {
            u16::from_le_bytes([pair[0], pair[1]])
        };
        if value == 0 {
            break;
        }
        units.push(value);
    }
    String::from_utf16_lossy(&units)
}

fn tag_value(
    tags: &[symphonia::core::meta::Tag],
    standard_key: StandardTagKey,
    fallback_keys: &[&str],
) -> Option<String> {
    let from_standard = tags
        .iter()
        .find(|tag| tag.std_key == Some(standard_key))
        .map(|tag| tag.value.to_string());

    let from_fallback = || {
        tags.iter()
            .find(|tag| {
                fallback_keys
                    .iter()
                    .any(|key| tag.key.eq_ignore_ascii_case(key))
            })
            .map(|tag| tag.value.to_string())
    };

    from_standard.or_else(from_fallback).and_then(|value| {
        let trimmed = value.trim();
        (!trimmed.is_empty()).then_some(trimmed.to_string())
    })
}

pub fn scan_many(roots: &[PathBuf]) -> Vec<Track> {
    let mut all = Vec::new();
    for root in roots {
        all.extend(scan_folder(root));
    }
    all.sort_by(|a, b| a.path.cmp(&b.path));
    all.dedup_by(|a, b| a.path == b.path);
    all
}

fn is_audio(path: &Path) -> bool {
    let ext = path.extension().and_then(OsStr::to_str).unwrap_or_default();
    AUDIO_EXTENSIONS
        .iter()
        .any(|supported| ext.eq_ignore_ascii_case(supported))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn scan_filters_non_audio_files() {
        let dir = tempdir().expect("tempdir");
        fs::write(dir.path().join("a.mp3"), b"x").expect("write mp3");
        fs::write(dir.path().join("b.txt"), b"x").expect("write txt");

        let tracks = scan_folder(dir.path());
        assert_eq!(tracks.len(), 1);
        assert!(tracks[0].path.ends_with("a.mp3"));
        assert_eq!(tracks[0].title, "a");
        assert_eq!(tracks[0].artist, None);
        assert_eq!(tracks[0].album, None);
    }
}
