use crate::model::Track;
use anyhow::{Context, Result};
use lofty::config::WriteOptions;
use lofty::file::{AudioFile, TaggedFileExt};
use lofty::picture::{Picture, PictureType};
use lofty::prelude::ItemKey;
use lofty::probe::Probe;
use lofty::tag::{Tag, TagType};
use rodio::{Decoder, Source};
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

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MetadataEdit {
    pub title: Option<String>,
    pub artist: Option<String>,
    pub album: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MetadataSnapshot {
    pub title: Option<String>,
    pub artist: Option<String>,
    pub album: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioQualityRating {
    Unavailable,
    Red,
    Yellow,
    Green,
    Gold,
}

impl AudioQualityRating {
    pub fn label(self) -> &'static str {
        match self {
            Self::Unavailable => "Unavailable",
            Self::Red => "Red",
            Self::Yellow => "Yellow",
            Self::Green => "Green",
            Self::Gold => "Gold *",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct AudioQualitySnapshot {
    pub format_label: String,
    pub bitrate_kbps: Option<u32>,
    pub sample_rate_hz: Option<u32>,
    pub channels: Option<u8>,
    pub duration_seconds: Option<u32>,
    pub rating: AudioQualityRating,
    pub spectrograph_rows: Vec<String>,
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

pub fn metadata_snapshot_for_path(path: &Path) -> MetadataSnapshot {
    let metadata = metadata_for(path);
    MetadataSnapshot {
        title: metadata.title,
        artist: metadata.artist,
        album: metadata.album,
    }
}

pub fn audio_quality_snapshot(path: &Path) -> AudioQualitySnapshot {
    let stripped = crate::config::strip_windows_verbatim_prefix(path);
    let (bitrate_kbps, sample_rate_hz, channels) = reported_audio_properties(&stripped);
    let format_label = format_label_for_path(&stripped);
    let rating = classify_audio_quality(bitrate_kbps, is_lossless_path(&stripped));
    let duration_seconds = duration_seconds(&stripped);
    let spectrograph_rows = build_static_spectrograph(&stripped, duration_seconds);

    AudioQualitySnapshot {
        format_label,
        bitrate_kbps,
        sample_rate_hz,
        channels,
        duration_seconds,
        rating,
        spectrograph_rows,
    }
}

pub fn write_embedded_metadata(path: &Path, edit: &MetadataEdit) -> Result<()> {
    validate_tag_edit_target(path)?;
    let stripped = crate::config::strip_windows_verbatim_prefix(path);

    let mut tagged_file = Probe::open(&stripped)
        .with_context(|| format!("failed to open {}", stripped.display()))?
        .read()
        .with_context(|| format!("failed to parse tags for {}", stripped.display()))?;

    let tag_type = preferred_tag_type_for_path(&stripped).unwrap_or(tagged_file.primary_tag_type());

    if tagged_file.tag_mut(tag_type).is_none() {
        tagged_file.insert_tag(Tag::new(tag_type));
    }

    let tag = tagged_file
        .tag_mut(tag_type)
        .context("failed to access primary tag")?;

    apply_metadata_edit_to_tag(tag, edit);

    tagged_file
        .save_to_path(&stripped, WriteOptions::default())
        .with_context(|| format!("failed to write metadata for {}", stripped.display()))
}

pub fn clear_embedded_metadata(path: &Path) -> Result<()> {
    write_embedded_metadata(path, &MetadataEdit::default())
}

pub fn write_embedded_cover_art(path: &Path, image_data: &[u8]) -> Result<()> {
    validate_tag_edit_target(path)?;
    let stripped = crate::config::strip_windows_verbatim_prefix(path);

    let mut tagged_file = Probe::open(&stripped)
        .with_context(|| format!("failed to open {}", stripped.display()))?
        .read()
        .with_context(|| format!("failed to parse tags for {}", stripped.display()))?;

    let tag_type = preferred_tag_type_for_path(&stripped).unwrap_or(tagged_file.primary_tag_type());

    if tagged_file.tag_mut(tag_type).is_none() {
        tagged_file.insert_tag(Tag::new(tag_type));
    }

    let tag = tagged_file
        .tag_mut(tag_type)
        .context("failed to access primary tag")?;
    replace_cover_picture(tag, image_data)?;

    tagged_file
        .save_to_path(&stripped, WriteOptions::default())
        .with_context(|| format!("failed to write cover art for {}", stripped.display()))
}

fn replace_cover_picture(tag: &mut Tag, image_data: &[u8]) -> Result<()> {
    let mut cursor = std::io::Cursor::new(image_data);
    let mut picture = Picture::from_reader(&mut cursor)
        .context("cover art bytes are not in a supported image format")?;
    picture.set_pic_type(PictureType::CoverFront);

    while !tag.pictures().is_empty() {
        let _ = tag.remove_picture(0);
    }
    tag.push_picture(picture);
    Ok(())
}

fn apply_metadata_edit_to_tag(tag: &mut Tag, edit: &MetadataEdit) {
    set_tag_text(tag, ItemKey::TrackTitle, edit.title.as_deref());
    set_tag_text(tag, ItemKey::TrackArtist, edit.artist.as_deref());
    set_tag_text(tag, ItemKey::AlbumTitle, edit.album.as_deref());
}

fn set_tag_text(tag: &mut Tag, key: ItemKey, value: Option<&str>) {
    let cleaned = value.and_then(clean_metadata_value);
    tag.remove_key(&key);
    if let Some(text) = cleaned {
        tag.insert_text(key, text);
    }
}

fn clean_metadata_value(value: &str) -> Option<String> {
    let trimmed = value.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

fn validate_tag_edit_target(path: &Path) -> Result<()> {
    let stripped = crate::config::strip_windows_verbatim_prefix(path);
    if !is_audio(&stripped) {
        anyhow::bail!("unsupported audio format for metadata editing")
    }

    if !stripped.exists() {
        anyhow::bail!("track file not found")
    }

    if !stripped.is_file() {
        anyhow::bail!("track path is not a file")
    }

    Ok(())
}

fn preferred_tag_type_for_path(path: &Path) -> Option<TagType> {
    let ext = path.extension().and_then(OsStr::to_str)?;
    if ext.eq_ignore_ascii_case("mp3") {
        return Some(TagType::Id3v2);
    }
    if ext.eq_ignore_ascii_case("flac")
        || ext.eq_ignore_ascii_case("ogg")
        || ext.eq_ignore_ascii_case("opus")
    {
        return Some(TagType::VorbisComments);
    }
    if ext.eq_ignore_ascii_case("m4a") {
        return Some(TagType::Mp4Ilst);
    }
    None
}

pub fn embedded_cover_art(path: &Path) -> Option<Vec<u8>> {
    let stripped = crate::config::strip_windows_verbatim_prefix(path);
    symphonia_embedded_cover_art(&stripped).or_else(|| id3v2_cover_art(&stripped))
}

fn symphonia_embedded_cover_art(path: &Path) -> Option<Vec<u8>> {
    let Ok(file) = File::open(path) else {
        return None;
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
        return None;
    };

    let metadata = probed.format.metadata();
    let revision = metadata.current()?;
    let visual = revision
        .visuals()
        .iter()
        .find(|entry| !entry.data.is_empty())?;
    Some(visual.data.as_ref().to_vec())
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

pub fn duration_seconds(path: &Path) -> Option<u32> {
    let stripped = crate::config::strip_windows_verbatim_prefix(path);

    let Ok(file) = File::open(&stripped) else {
        return None;
    };
    let source = MediaSourceStream::new(Box::new(file), MediaSourceStreamOptions::default());

    let mut hint = Hint::new();
    if let Some(extension) = stripped.extension().and_then(OsStr::to_str) {
        hint.with_extension(extension);
    }

    let Ok(probed) = get_probe().format(
        &hint,
        source,
        &FormatOptions::default(),
        &MetadataOptions::default(),
    ) else {
        return None;
    };

    probed
        .format
        .default_track()
        .and_then(|track| codec_duration_seconds(&track.codec_params))
}

fn codec_duration_seconds(codec_params: &symphonia::core::codecs::CodecParameters) -> Option<u32> {
    if let (Some(time_base), Some(frame_count)) = (codec_params.time_base, codec_params.n_frames) {
        let time = time_base.calc_time(frame_count);
        let mut seconds = time.seconds as u32;
        if time.frac >= 0.5 {
            seconds = seconds.saturating_add(1);
        }
        return Some(seconds);
    }

    if let Some((frame_count, sample_rate)) = codec_params
        .n_frames
        .zip(codec_params.sample_rate)
        .filter(|(_, sample_rate)| *sample_rate > 0)
    {
        let seconds = ((frame_count as f64) / (sample_rate as f64)).round();
        return Some(seconds.clamp(0.0, u32::MAX as f64) as u32);
    }

    None
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

fn id3v2_cover_art(path: &Path) -> Option<Vec<u8>> {
    let mut file = File::open(path).ok()?;
    let mut header = [0u8; 10];
    file.read_exact(&mut header).ok()?;
    if !header.starts_with(b"ID3") {
        return None;
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
    file.read_exact(&mut tag_bytes).ok()?;

    let mut pos = 0;
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

        match frame_id {
            "APIC" => {
                if let Some(bytes) = parse_apic_payload(payload) {
                    return Some(bytes);
                }
            }
            "PIC" => {
                if let Some(bytes) = parse_pic_payload(payload) {
                    return Some(bytes);
                }
            }
            _ => {}
        }

        pos = data_end;
    }

    None
}

fn parse_apic_payload(payload: &[u8]) -> Option<Vec<u8>> {
    if payload.len() < 4 {
        return None;
    }

    let encoding = payload[0];
    let mime_start = 1;
    let mime_end = payload[mime_start..]
        .iter()
        .position(|byte| *byte == 0)
        .map(|idx| mime_start + idx)?;
    let mut pos = mime_end + 1;
    if pos >= payload.len() {
        return None;
    }

    pos += 1;
    let description_end = id3_description_end(&payload[pos..], encoding)?;
    pos = pos.saturating_add(description_end);
    if pos >= payload.len() {
        return None;
    }

    Some(payload[pos..].to_vec())
}

fn parse_pic_payload(payload: &[u8]) -> Option<Vec<u8>> {
    if payload.len() < 6 {
        return None;
    }

    let encoding = payload[0];
    let mut pos = 5;

    let description_end = id3_description_end(&payload[pos..], encoding)?;
    pos = pos.saturating_add(description_end);
    if pos >= payload.len() {
        return None;
    }

    Some(payload[pos..].to_vec())
}

fn id3_description_end(payload: &[u8], encoding: u8) -> Option<usize> {
    match encoding {
        0 | 3 => payload
            .iter()
            .position(|byte| *byte == 0)
            .map(|idx| idx + 1),
        1 | 2 => payload
            .windows(2)
            .position(|window| window[0] == 0 && window[1] == 0)
            .map(|idx| idx + 2),
        _ => payload
            .iter()
            .position(|byte| *byte == 0)
            .map(|idx| idx + 1),
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

fn format_label_for_path(path: &Path) -> String {
    path.extension()
        .and_then(OsStr::to_str)
        .map(|ext| ext.to_ascii_uppercase())
        .unwrap_or_else(|| String::from("Unknown"))
}

fn is_lossless_path(path: &Path) -> bool {
    path.extension()
        .and_then(OsStr::to_str)
        .map(|ext| {
            matches!(
                ext.to_ascii_lowercase().as_str(),
                "wav" | "flac" | "aiff" | "aif" | "alac"
            )
        })
        .unwrap_or(false)
}

fn classify_audio_quality(bitrate_kbps: Option<u32>, lossless: bool) -> AudioQualityRating {
    let Some(bitrate) = bitrate_kbps else {
        return AudioQualityRating::Unavailable;
    };
    if lossless || bitrate >= 320 {
        return AudioQualityRating::Gold;
    }
    if bitrate < 128 {
        return AudioQualityRating::Red;
    }
    if bitrate < 256 {
        return AudioQualityRating::Yellow;
    }
    AudioQualityRating::Green
}

fn reported_audio_properties(path: &Path) -> (Option<u32>, Option<u32>, Option<u8>) {
    let Ok(tagged_file) = Probe::open(path).and_then(|entry| entry.read()) else {
        return (None, None, None);
    };

    let props = tagged_file.properties();
    let bitrate_kbps = props.audio_bitrate();
    let sample_rate_hz = props.sample_rate();
    let channels = props.channels();
    (bitrate_kbps, sample_rate_hz, channels)
}

fn build_static_spectrograph(path: &Path, duration_seconds: Option<u32>) -> Vec<String> {
    const BAND_COUNT: usize = 18;
    const COLUMN_COUNT: usize = 60;
    const WINDOW_FRAMES: usize = 4096;
    const MAX_ANALYSIS_FRAMES: usize = 44_100 * 360;
    const PROBES_PER_BAND: usize = 3;
    const SYMBOLS: &[u8] = b" .:-=+*#%@";

    let Ok(file) = File::open(path) else {
        return vec![String::from("(spectrograph unavailable)")];
    };
    let Ok(source) = Decoder::try_from(file) else {
        return vec![String::from("(spectrograph unavailable)")];
    };

    let channels = usize::from(source.channels()).max(1);
    let sample_rate = source.sample_rate().max(8_000);
    let sample_rate_usize = usize::try_from(sample_rate).unwrap_or(44_100);
    let total_frames_hint = duration_seconds
        .and_then(|seconds| usize::try_from(seconds).ok())
        .map(|seconds| seconds.saturating_mul(sample_rate_usize));

    let downsample_stride = total_frames_hint
        .map(|frames| {
            let ratio = frames as f32 / MAX_ANALYSIS_FRAMES as f32;
            ratio.ceil() as usize
        })
        .unwrap_or(1)
        .max(1);

    let mut mono = Vec::with_capacity(
        total_frames_hint
            .unwrap_or(sample_rate_usize.saturating_mul(180))
            .saturating_div(downsample_stride)
            .clamp(4_096, MAX_ANALYSIS_FRAMES),
    );
    let mut frame_sum = 0.0_f32;
    let mut frame_channels = 0_usize;
    let mut frame_index = 0_usize;

    for sample in source {
        frame_sum += sample;
        frame_channels += 1;
        if frame_channels == channels {
            let mono_frame = frame_sum / channels as f32;
            if frame_index.is_multiple_of(downsample_stride) {
                mono.push(mono_frame);
                if mono.len() >= MAX_ANALYSIS_FRAMES {
                    break;
                }
            }
            frame_sum = 0.0;
            frame_channels = 0;
            frame_index = frame_index.saturating_add(1);
        }
    }

    if mono.len() < 128 {
        return vec![String::from("(spectrograph unavailable)")];
    }

    let effective_sample_rate = sample_rate as f32 / downsample_stride as f32;
    let nyquist = effective_sample_rate * 0.5;
    let max_freq = nyquist.max(1_000.0);
    let edges = linear_spaced_frequencies(0.0, max_freq, BAND_COUNT + 1);
    let mut heatmap = vec![vec![0.0_f32; COLUMN_COUNT]; BAND_COUNT];
    let window_frames = WINDOW_FRAMES.min(mono.len()).max(512);
    let column_ranges: Vec<(usize, usize)> = (0..COLUMN_COUNT)
        .map(|col| {
            let center = if COLUMN_COUNT <= 1 {
                mono.len() / 2
            } else {
                ((col as f32 + 0.5) * (mono.len().saturating_sub(1)) as f32 / COLUMN_COUNT as f32)
                    as usize
            };
            let start = center.saturating_sub(window_frames / 2);
            let end = (start + window_frames).min(mono.len());
            (start, end)
        })
        .collect();

    for (col, (start, end)) in column_ranges.into_iter().enumerate() {
        let window = &mono[start..end];
        if window.len() < 32 {
            continue;
        }

        for band in 0..BAND_COUNT {
            let low = edges[band];
            let high = edges[band + 1].min(max_freq);
            let mut power_sum = 0.0_f32;
            for probe in 0..PROBES_PER_BAND {
                let t = (probe as f32 + 0.5) / PROBES_PER_BAND as f32;
                let frequency_hz = (low + (high - low) * t).max(10.0);
                power_sum += goertzel_power(window, frequency_hz, effective_sample_rate);
            }
            heatmap[band][col] = power_sum / PROBES_PER_BAND as f32;
        }
    }

    smooth_heatmap_columns(&mut heatmap);

    let mut normalized = vec![vec![0.0_f32; COLUMN_COUNT]; BAND_COUNT];
    normalize_heatmap_to_unit(&heatmap, &mut normalized);

    let max_power = normalized
        .iter()
        .flat_map(|row| row.iter())
        .copied()
        .fold(0.0_f32, f32::max);

    if max_power <= f32::EPSILON {
        return vec![String::from("(spectrograph unavailable)")];
    }

    let mut rows = Vec::with_capacity(BAND_COUNT + 2);
    let end_label = duration_seconds
        .map(format_spectro_time)
        .unwrap_or_else(|| String::from("--:--"));
    let timeline = timeline_row(&normalized, max_power, COLUMN_COUNT, SYMBOLS);
    rows.push(format!("Time 00:00 |{timeline}| {end_label}"));

    for band in (0..BAND_COUNT).rev() {
        let label_frequency = if band == BAND_COUNT - 1 {
            max_freq
        } else {
            edges[band]
        };
        let label = frequency_label(label_frequency);
        let mut graph = String::with_capacity(COLUMN_COUNT);
        for power in normalized[band].iter().take(COLUMN_COUNT) {
            let normalized = (*power / max_power).sqrt().clamp(0.0, 1.0);
            let idx = (normalized * (SYMBOLS.len() - 1) as f32).round() as usize;
            graph.push(SYMBOLS[idx.min(SYMBOLS.len() - 1)] as char);
        }
        rows.push(format!("{label:>5} |{graph}|"));
    }
    rows
}

fn normalize_heatmap_to_unit(heatmap: &[Vec<f32>], output: &mut [Vec<f32>]) {
    let mut max_db = f32::NEG_INFINITY;
    let mut min_db = f32::INFINITY;

    for row in heatmap {
        for power in row {
            let db = 10.0 * power.max(1e-12).log10();
            max_db = max_db.max(db);
            min_db = min_db.min(db);
        }
    }

    let floor_db = (max_db - 70.0).max(min_db);
    let range = (max_db - floor_db).max(1.0);

    for (row, out_row) in heatmap.iter().zip(output.iter_mut()) {
        for (power, out) in row.iter().zip(out_row.iter_mut()) {
            let db = 10.0 * power.max(1e-12).log10();
            let unit = ((db - floor_db) / range).clamp(0.0, 1.0);
            *out = unit.powf(0.85);
        }
    }
}

fn timeline_row(heatmap: &[Vec<f32>], max_power: f32, columns: usize, symbols: &[u8]) -> String {
    let mut row = String::with_capacity(columns);
    for col in 0..columns {
        let col_power = heatmap.iter().map(|band| band[col]).fold(0.0_f32, f32::max);
        let normalized = (col_power / max_power).sqrt().clamp(0.0, 1.0);
        let idx = (normalized * (symbols.len() - 1) as f32).round() as usize;
        row.push(symbols[idx.min(symbols.len() - 1)] as char);
    }
    row
}

fn format_spectro_time(total_seconds: u32) -> String {
    let minutes = total_seconds / 60;
    let seconds = total_seconds % 60;
    format!("{minutes:02}:{seconds:02}")
}

fn smooth_heatmap_columns(heatmap: &mut [Vec<f32>]) {
    for row in heatmap.iter_mut() {
        if row.len() < 3 {
            continue;
        }
        let mut smoothed = row.clone();
        for idx in 1..row.len() - 1 {
            smoothed[idx] = (row[idx - 1] + row[idx] * 2.0 + row[idx + 1]) / 4.0;
        }
        *row = smoothed;
    }
}

fn linear_spaced_frequencies(min_hz: f32, max_hz: f32, count: usize) -> Vec<f32> {
    if count <= 1 {
        return vec![min_hz.max(1.0)];
    }

    let min = min_hz.max(0.0);
    let max = max_hz.max(min + 1.0);
    let step = (max - min) / (count - 1) as f32;
    (0..count).map(|idx| min + step * idx as f32).collect()
}

fn frequency_label(frequency_hz: f32) -> String {
    if frequency_hz >= 1_000.0 {
        format!("{:.1}k", frequency_hz / 1_000.0)
    } else {
        format!("{frequency_hz:.0}")
    }
}

fn goertzel_power(samples: &[f32], frequency_hz: f32, sample_rate_hz: f32) -> f32 {
    if samples.len() < 2
        || !frequency_hz.is_finite()
        || !sample_rate_hz.is_finite()
        || frequency_hz <= 0.0
        || frequency_hz >= sample_rate_hz * 0.5
    {
        return 0.0;
    }

    let omega = std::f32::consts::TAU * frequency_hz / sample_rate_hz;
    let coeff = 2.0 * omega.cos();
    let mut q1 = 0.0_f32;
    let mut q2 = 0.0_f32;
    for sample in samples {
        let q0 = coeff * q1 - q2 + *sample;
        q2 = q1;
        q1 = q0;
    }
    let real = q1 - q2 * omega.cos();
    let imag = q2 * omega.sin();
    real.mul_add(real, imag * imag)
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

    #[test]
    fn metadata_value_cleaning_trims_and_drops_empty() {
        assert_eq!(
            clean_metadata_value("  hello  "),
            Some(String::from("hello"))
        );
        assert_eq!(clean_metadata_value("   \t  "), None);
    }

    #[test]
    fn metadata_edit_rejects_non_audio_paths() {
        let dir = tempdir().expect("tempdir");
        let file = dir.path().join("note.txt");
        fs::write(&file, b"x").expect("write text");

        let err = write_embedded_metadata(&file, &MetadataEdit::default()).expect_err("error");
        assert!(
            err.to_string().contains("unsupported audio format"),
            "unexpected error: {err:#}"
        );
    }

    #[test]
    fn cover_edit_rejects_non_audio_paths() {
        let dir = tempdir().expect("tempdir");
        let file = dir.path().join("note.txt");
        fs::write(&file, b"x").expect("write text");

        let err = write_embedded_cover_art(&file, b"not-image").expect_err("error");
        assert!(
            err.to_string().contains("unsupported audio format"),
            "unexpected error: {err:#}"
        );
    }

    #[test]
    fn quality_rating_thresholds_match_issue_rules() {
        assert_eq!(
            classify_audio_quality(None, false),
            AudioQualityRating::Unavailable
        );
        assert_eq!(
            classify_audio_quality(Some(96), false),
            AudioQualityRating::Red
        );
        assert_eq!(
            classify_audio_quality(Some(192), false),
            AudioQualityRating::Yellow
        );
        assert_eq!(
            classify_audio_quality(Some(256), false),
            AudioQualityRating::Green
        );
        assert_eq!(
            classify_audio_quality(Some(320), false),
            AudioQualityRating::Gold
        );
        assert_eq!(
            classify_audio_quality(Some(1411), true),
            AudioQualityRating::Gold
        );
    }

    #[test]
    fn spectrograph_fallback_is_stable_for_missing_file() {
        assert_eq!(
            build_static_spectrograph(Path::new("missing-file.mp3"), Some(180)),
            vec![String::from("(spectrograph unavailable)")]
        );
    }
}
