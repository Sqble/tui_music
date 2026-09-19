use anyhow::{Context, Result};
use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LyricsTimingPrecision {
    None,
    Line,
    Word,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LyricsSource {
    Sidecar,
    Embedded,
    Created,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LyricLine {
    pub timestamp_ms: Option<u32>,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LyricsDocument {
    pub lines: Vec<LyricLine>,
    pub source: LyricsSource,
    pub precision: LyricsTimingPrecision,
}

pub fn sidecar_lrc_path(track_path: &Path) -> Result<PathBuf> {
    crate::config::lyrics_path_for_track(track_path)
}

pub fn load_for_track(track_path: &Path) -> Result<Option<LyricsDocument>> {
    let lrc_path = sidecar_lrc_path(track_path)?;
    if lrc_path.exists() {
        let raw = fs::read_to_string(&lrc_path)
            .with_context(|| format!("failed to read lyrics file {}", lrc_path.display()))?;
        let mut doc = parse_lrc(&raw);
        doc.source = LyricsSource::Sidecar;
        return Ok(Some(doc));
    }

    let legacy_sidecar = track_path.with_extension("lrc");
    if legacy_sidecar.exists() {
        let raw = fs::read_to_string(&legacy_sidecar)
            .with_context(|| format!("failed to read lyrics file {}", legacy_sidecar.display()))?;
        let mut doc = parse_lrc(&raw);
        doc.source = LyricsSource::Sidecar;
        return Ok(Some(doc));
    }

    if let Some(raw) = read_embedded_lyrics(track_path) {
        let mut doc = if looks_like_lrc(&raw) {
            parse_lrc(&raw)
        } else {
            parse_plain_text(&raw)
        };
        doc.source = LyricsSource::Embedded;
        return Ok(Some(doc));
    }

    Ok(None)
}

pub fn parse_plain_text(input: &str) -> LyricsDocument {
    let lines = input
        .lines()
        .map(str::trim_end)
        .filter(|line| !line.is_empty())
        .map(|line| LyricLine {
            timestamp_ms: None,
            text: line.to_string(),
        })
        .collect();

    LyricsDocument {
        lines,
        source: LyricsSource::Embedded,
        precision: LyricsTimingPrecision::None,
    }
}

pub fn parse_lrc(input: &str) -> LyricsDocument {
    let mut lines = Vec::new();
    let mut precision = LyricsTimingPrecision::None;

    for raw_line in input.lines() {
        let line = raw_line.trim_end();
        if line.is_empty() {
            continue;
        }

        if is_metadata_lrc_line(line) {
            continue;
        }

        let (timestamps, text_with_possible_word_tags) = parse_line_timestamps(line);
        let (text, has_word_tags) = strip_word_timestamps(text_with_possible_word_tags);
        if has_word_tags {
            precision = LyricsTimingPrecision::Word;
        }

        if timestamps.is_empty() {
            lines.push(LyricLine {
                timestamp_ms: None,
                text,
            });
            continue;
        }

        if precision == LyricsTimingPrecision::None {
            precision = LyricsTimingPrecision::Line;
        }
        for timestamp_ms in timestamps {
            lines.push(LyricLine {
                timestamp_ms: Some(timestamp_ms),
                text: text.clone(),
            });
        }
    }

    lines.sort_by_key(|line| line.timestamp_ms.unwrap_or(u32::MAX));

    LyricsDocument {
        lines,
        source: LyricsSource::Sidecar,
        precision,
    }
}

pub fn to_lrc(doc: &LyricsDocument) -> String {
    let mut out = String::new();
    for line in &doc.lines {
        if let Some(timestamp_ms) = line.timestamp_ms {
            out.push_str(&format_lrc_timestamp(timestamp_ms));
        }
        out.push_str(&line.text);
        out.push('\n');
    }
    out
}

pub fn write_sidecar(track_path: &Path, doc: &LyricsDocument) -> Result<PathBuf> {
    crate::config::ensure_lyrics_dir()?;
    let target = sidecar_lrc_path(track_path)?;
    let lrc = to_lrc(doc);
    fs::write(&target, lrc)
        .with_context(|| format!("failed to write lyrics file {}", target.display()))?;
    Ok(target)
}

pub fn read_txt_for_import(path: &Path) -> Result<Vec<String>> {
    let raw = fs::read_to_string(path)
        .with_context(|| format!("failed to read txt file {}", path.display()))?;
    Ok(raw
        .lines()
        .map(str::trim_end)
        .filter(|line| !line.is_empty())
        .map(ToOwned::to_owned)
        .collect())
}

pub fn build_seeded_from_lines(lines: Vec<String>, interval_seconds: u32) -> LyricsDocument {
    let step_ms = interval_seconds.max(1).saturating_mul(1000);
    let out_lines = lines
        .into_iter()
        .enumerate()
        .map(|(idx, text)| LyricLine {
            timestamp_ms: Some((idx as u32).saturating_mul(step_ms)),
            text,
        })
        .collect();

    LyricsDocument {
        lines: out_lines,
        source: LyricsSource::Created,
        precision: LyricsTimingPrecision::Line,
    }
}

/// Reads embedded lyrics for a track, preferring a precise ID3v2 USLT parse
/// (explicit encoding, language, and frame selection) and falling back to the
/// generic metadata-tag scan for other formats and frame kinds.
fn read_embedded_lyrics(track_path: &Path) -> Option<String> {
    read_id3v2_uslt_lyrics(track_path).or_else(|| read_metadata_tag_lyrics(track_path))
}

/// A single USLT (unsynchronized lyrics) frame decoded from a raw ID3v2 tag.
struct UsltFrame {
    language: [u8; 3],
    text: String,
}

/// Reads lyrics from ID3v2 USLT frames by parsing the raw tag bytes.
///
/// Handles the text-encoding byte (Latin-1, UTF-16 with/without BOM, UTF-8),
/// the 3-byte ISO-639-2 language code, and multiple USLT frames: English is
/// preferred, otherwise the first non-empty frame wins.
fn read_id3v2_uslt_lyrics(track_path: &Path) -> Option<String> {
    let bytes = fs::read(track_path).ok()?;
    let frames = parse_id3v2_uslt_frames(&bytes);
    frames
        .iter()
        .find(|frame| frame.language.eq_ignore_ascii_case(b"eng"))
        .or_else(|| frames.first())
        .map(|frame| frame.text.clone())
}

fn parse_id3v2_uslt_frames(bytes: &[u8]) -> Vec<UsltFrame> {
    if bytes.len() < 10 || &bytes[..3] != b"ID3" {
        return Vec::new();
    }
    let major = bytes[3];
    // v2.2 uses 3-byte frame ids ("ULT"); v2.3/v2.4 use 4-byte ids ("USLT").
    let (id_len, header_len, wanted) = match major {
        2 => (3, 6, "ULT"),
        3 | 4 => (4, 10, "USLT"),
        _ => return Vec::new(),
    };

    let tag_size = syncsafe_size(&bytes[6..10]) as usize;
    let tag_end = 10usize.saturating_add(tag_size).min(bytes.len());
    let tag = &bytes[10..tag_end];

    let mut frames = Vec::new();
    let mut pos = 0;
    loop {
        if pos + header_len > tag.len() {
            break;
        }
        let frame_id = std::str::from_utf8(&tag[pos..pos + id_len]).unwrap_or("");
        let frame_size = if major == 2 {
            ((tag[pos + 3] as usize) << 16) | ((tag[pos + 4] as usize) << 8) | tag[pos + 5] as usize
        } else if major == 4 {
            syncsafe_size(&tag[pos + 4..pos + 8]) as usize
        } else {
            u32::from_be_bytes([tag[pos + 4], tag[pos + 5], tag[pos + 6], tag[pos + 7]]) as usize
        };

        // Zeroed frame ids mark the start of tag padding.
        if frame_id.trim_matches('\0').is_empty() || frame_size == 0 {
            break;
        }
        let data_start = pos + header_len;
        let data_end = match data_start.checked_add(frame_size) {
            Some(end) if end <= tag.len() => end,
            _ => break,
        };
        if frame_id == wanted
            && let Some(frame) = decode_uslt_payload(&tag[data_start..data_end])
        {
            frames.push(frame);
        }
        pos = data_end;
    }
    frames
}

fn syncsafe_size(bytes: &[u8]) -> u32 {
    ((bytes[0] as u32) & 0x7f) << 21
        | ((bytes[1] as u32) & 0x7f) << 14
        | ((bytes[2] as u32) & 0x7f) << 7
        | (bytes[3] as u32) & 0x7f
}

/// Decodes one USLT/ULT frame payload:
/// `encoding | language[3] | description (null-terminated) | lyrics text`.
fn decode_uslt_payload(payload: &[u8]) -> Option<UsltFrame> {
    if payload.len() < 4 {
        return None;
    }
    let encoding = payload[0];
    let language = [payload[1], payload[2], payload[3]];
    let rest = &payload[4..];

    // The description is null-terminated according to the frame encoding; when
    // the terminator is missing, treat the whole remainder as lyric text.
    let text_start = match encoding {
        0 | 3 => rest
            .iter()
            .position(|&b| b == 0)
            .map(|idx| idx + 1)
            .unwrap_or(0),
        1 | 2 => rest
            .chunks_exact(2)
            .position(|pair| pair == [0, 0])
            .map(|idx| idx * 2 + 2)
            .unwrap_or(0),
        _ => return None,
    };
    let text = decode_id3_text(encoding, rest.get(text_start..)?)?;
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return None;
    }
    Some(UsltFrame {
        language,
        text: trimmed.to_string(),
    })
}

/// Decodes ID3 text bytes according to the frame's encoding byte:
/// 0 = Latin-1, 1 = UTF-16 with BOM (assumed LE when absent),
/// 2 = UTF-16BE, 3 = UTF-8.
fn decode_id3_text(encoding: u8, bytes: &[u8]) -> Option<String> {
    match encoding {
        0 => Some(bytes.iter().map(|&b| char::from(b)).collect()),
        1 => {
            let (little_endian, data) = match bytes {
                [0xFF, 0xFE, ..] => (true, &bytes[2..]),
                [0xFE, 0xFF, ..] => (false, &bytes[2..]),
                _ => (true, bytes),
            };
            let units: Vec<u16> = data
                .chunks_exact(2)
                .map(|pair| {
                    if little_endian {
                        u16::from_le_bytes([pair[0], pair[1]])
                    } else {
                        u16::from_be_bytes([pair[0], pair[1]])
                    }
                })
                .collect();
            Some(String::from_utf16_lossy(&units))
        }
        2 => {
            let units: Vec<u16> = bytes
                .chunks_exact(2)
                .map(|pair| u16::from_be_bytes([pair[0], pair[1]]))
                .collect();
            Some(String::from_utf16_lossy(&units))
        }
        3 => Some(String::from_utf8_lossy(bytes).into_owned()),
        _ => None,
    }
}

fn read_metadata_tag_lyrics(track_path: &Path) -> Option<String> {
    let file = fs::File::open(track_path).ok()?;
    let source = symphonia::core::io::MediaSourceStream::new(
        Box::new(file),
        symphonia::core::io::MediaSourceStreamOptions::default(),
    );

    let mut hint = symphonia::core::probe::Hint::new();
    if let Some(extension) = track_path.extension().and_then(OsStr::to_str) {
        hint.with_extension(extension);
    }

    let mut probed = symphonia::default::get_probe()
        .format(
            &hint,
            source,
            &symphonia::core::formats::FormatOptions::default(),
            &symphonia::core::meta::MetadataOptions::default(),
        )
        .ok()?;

    let metadata = probed.format.metadata();
    let revision = metadata.current()?;
    let tags = revision.tags();

    let mut best: Option<(u8, String)> = None;
    for tag in tags {
        let key = tag.key.to_ascii_lowercase();
        if !(key.contains("lyric") || key == "uslt" || key == "sylt") {
            continue;
        }

        let value = tag.value.to_string();
        let trimmed = value.trim();
        if trimmed.is_empty() {
            continue;
        }

        let score = if looks_like_lrc(trimmed) {
            3
        } else if key == "sylt" {
            2
        } else {
            1
        };

        if best.as_ref().is_none_or(|(existing, _)| score > *existing) {
            best = Some((score, trimmed.to_string()));
        }
    }

    best.map(|(_, value)| value)
}

fn looks_like_lrc(input: &str) -> bool {
    input.lines().any(|line| {
        let trimmed = line.trim_start();
        if !trimmed.starts_with('[') {
            return false;
        }
        // A line only "looks like LRC" when it opens with a real timestamp
        // token; metadata tags like `[ar:artist]` must not count.
        let (timestamps, _) = parse_line_timestamps(trimmed);
        !timestamps.is_empty()
    })
}

fn is_metadata_lrc_line(line: &str) -> bool {
    let lower = line.to_ascii_lowercase();
    lower.starts_with("[ar:")
        || lower.starts_with("[ti:")
        || lower.starts_with("[al:")
        || lower.starts_with("[by:")
        || lower.starts_with("[offset:")
        || lower.starts_with("[length:")
}

fn parse_line_timestamps(input: &str) -> (Vec<u32>, &str) {
    let mut remaining = input;
    let mut out = Vec::new();

    while remaining.starts_with('[') {
        let Some(closing_idx) = remaining.find(']') else {
            break;
        };
        let token = &remaining[..=closing_idx];
        let Some(ms) = parse_single_lrc_timestamp(token) else {
            break;
        };
        out.push(ms);
        remaining = &remaining[closing_idx + 1..];
    }

    (out, remaining.trim_start())
}

fn parse_single_lrc_timestamp(token: &str) -> Option<u32> {
    if !(token.starts_with('[') && token.ends_with(']')) {
        return None;
    }
    let content = &token[1..token.len().saturating_sub(1)];
    let mut parts = content.split(':');
    let minutes = parts.next()?.parse::<u32>().ok()?;
    let seconds_part = parts.next()?;
    if parts.next().is_some() {
        return None;
    }

    let mut seconds_parts = seconds_part.split('.');
    let seconds = seconds_parts.next()?.parse::<u32>().ok()?;
    let fraction_raw = seconds_parts.next().unwrap_or("0");
    if seconds_parts.next().is_some() {
        return None;
    }

    let fraction_2 = if fraction_raw.is_empty() {
        0
    } else if fraction_raw.len() == 1 {
        fraction_raw.parse::<u32>().ok()?.saturating_mul(10)
    } else {
        fraction_raw
            .chars()
            .take(2)
            .collect::<String>()
            .parse::<u32>()
            .ok()?
    };

    Some(
        minutes
            .saturating_mul(60_000)
            .saturating_add(seconds.saturating_mul(1000))
            .saturating_add(fraction_2.saturating_mul(10)),
    )
}

fn strip_word_timestamps(input: &str) -> (String, bool) {
    let mut out = String::with_capacity(input.len());
    let mut remaining = input;
    let mut had_word_tags = false;

    while let Some(open_idx) = remaining.find('<') {
        out.push_str(&remaining[..open_idx]);
        let tail = &remaining[open_idx..];
        let Some(close_idx) = tail.find('>') else {
            out.push_str(tail);
            remaining = "";
            break;
        };
        let token = &tail[..=close_idx];
        if parse_word_timestamp(token).is_some() {
            had_word_tags = true;
        } else {
            out.push_str(token);
        }
        remaining = &tail[close_idx + 1..];
    }

    if !remaining.is_empty() {
        out.push_str(remaining);
    }

    (out.trim().to_string(), had_word_tags)
}

fn parse_word_timestamp(token: &str) -> Option<u32> {
    if !(token.starts_with('<') && token.ends_with('>')) {
        return None;
    }
    let candidate = format!("[{}]", &token[1..token.len().saturating_sub(1)]);
    parse_single_lrc_timestamp(&candidate)
}

fn format_lrc_timestamp(timestamp_ms: u32) -> String {
    let minutes = timestamp_ms / 60_000;
    let seconds = (timestamp_ms % 60_000) / 1000;
    let hundredths = (timestamp_ms % 1000) / 10;
    format!("[{minutes:02}:{seconds:02}.{hundredths:02}]")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_lrc_handles_line_timing() {
        let doc = parse_lrc("[00:01.00]hello\n[00:02.50]world\n");
        assert_eq!(doc.precision, LyricsTimingPrecision::Line);
        assert_eq!(doc.lines.len(), 2);
        assert_eq!(doc.lines[0].timestamp_ms, Some(1000));
        assert_eq!(doc.lines[1].timestamp_ms, Some(2500));
    }

    #[test]
    fn parse_lrc_detects_word_tags() {
        let doc = parse_lrc("[00:01.00]<00:01.20>hel<00:01.50>lo\n");
        assert_eq!(doc.precision, LyricsTimingPrecision::Word);
        assert_eq!(doc.lines[0].text, "hello");
    }

    #[test]
    fn seeded_import_assigns_fixed_intervals() {
        let doc = build_seeded_from_lines(vec!["a".into(), "b".into(), "c".into()], 3);
        assert_eq!(doc.lines[0].timestamp_ms, Some(0));
        assert_eq!(doc.lines[1].timestamp_ms, Some(3000));
        assert_eq!(doc.lines[2].timestamp_ms, Some(6000));
    }

    /// Builds a raw ID3v2.3 USLT frame with the given encoding byte, 3-byte
    /// language code, description bytes, and already-encoded lyric text.
    fn uslt_frame(encoding: u8, language: &[u8; 3], description: &[u8], text: &[u8]) -> Vec<u8> {
        let mut payload = Vec::new();
        payload.push(encoding);
        payload.extend_from_slice(language);
        payload.extend_from_slice(description);
        match encoding {
            1 | 2 => payload.extend_from_slice(&[0, 0]),
            _ => payload.push(0),
        }
        payload.extend_from_slice(text);

        let mut frame = Vec::new();
        frame.extend_from_slice(b"USLT");
        frame.extend_from_slice(&(payload.len() as u32).to_be_bytes());
        frame.extend_from_slice(&[0u8, 0]); // flags
        frame.extend_from_slice(&payload);
        frame
    }

    /// Builds a raw ID3v2.3 text frame (e.g. TIT2) with a UTF-8 payload.
    fn text_frame(id: &[u8; 4], text: &[u8]) -> Vec<u8> {
        let mut payload = Vec::new();
        payload.push(3); // UTF-8
        payload.extend_from_slice(text);

        let mut frame = Vec::new();
        frame.extend_from_slice(id);
        frame.extend_from_slice(&(payload.len() as u32).to_be_bytes());
        frame.extend_from_slice(&[0u8, 0]); // flags
        frame.extend_from_slice(&payload);
        frame
    }

    /// Wraps raw ID3v2.3 frames in a tag header and appends a minimal MPEG1
    /// Layer III frame so tag libraries accept the file as audio.
    fn mp3_with_id3v2(frames: &[Vec<u8>]) -> Vec<u8> {
        let mut body: Vec<u8> = Vec::new();
        for frame in frames {
            body.extend_from_slice(frame);
        }
        let size = body.len() as u32;
        let mut out = Vec::new();
        out.extend_from_slice(b"ID3");
        out.push(3); // v2.3
        out.push(0);
        out.push(0); // flags
        out.push(((size >> 21) & 0x7F) as u8);
        out.push(((size >> 14) & 0x7F) as u8);
        out.push(((size >> 7) & 0x7F) as u8);
        out.push((size & 0x7F) as u8);
        out.extend_from_slice(&body);
        // Two consecutive minimal MPEG1 Layer III frames (128 kbps, 44.1 kHz,
        // 417 bytes each): tag readers validate a frame by comparing it against
        // the next one, so a single frame is rejected as invalid.
        for _ in 0..2 {
            out.extend_from_slice(&[0xFF, 0xFB, 0x90, 0x00]);
            out.resize(out.len() + 417 - 4, 0);
        }
        out
    }

    fn utf16le_with_bom(text: &str) -> Vec<u8> {
        let mut out = vec![0xFF, 0xFE];
        for unit in text.encode_utf16() {
            out.extend_from_slice(&unit.to_le_bytes());
        }
        out
    }

    fn write_synth_mp3(name: &str, frames: &[Vec<u8>]) -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join(name);
        std::fs::write(&path, mp3_with_id3v2(frames)).expect("write synth mp3");
        (dir, path)
    }

    #[test]
    fn embedded_uslt_utf8_lrc_loads_with_line_timing() {
        let frames = [uslt_frame(
            3,
            b"eng",
            b"",
            b"[00:01.00]hello\n[00:02.50]world\n",
        )];
        let (_dir, path) = write_synth_mp3("utf8.mp3", &frames);

        let doc = load_for_track(&path)
            .expect("load lyrics")
            .expect("embedded lyrics present");
        assert_eq!(doc.source, LyricsSource::Embedded);
        assert_eq!(doc.precision, LyricsTimingPrecision::Line);
        assert_eq!(doc.lines.len(), 2);
        assert_eq!(doc.lines[0].timestamp_ms, Some(1000));
        assert_eq!(doc.lines[0].text, "hello");
        assert_eq!(doc.lines[1].text, "world");
    }

    #[test]
    fn embedded_uslt_prefers_english_over_first_frame() {
        let frames = [
            uslt_frame(3, b"fra", b"", b"[00:01.00]bonjour\n"),
            uslt_frame(3, b"eng", b"", b"[00:01.00]hello\n"),
        ];
        let (_dir, path) = write_synth_mp3("multi.mp3", &frames);

        let doc = load_for_track(&path)
            .expect("load lyrics")
            .expect("embedded lyrics present");
        assert_eq!(doc.source, LyricsSource::Embedded);
        assert_eq!(doc.lines.len(), 1);
        assert_eq!(doc.lines[0].text, "hello");
    }

    #[test]
    fn embedded_uslt_decodes_latin1_text() {
        let frames = [uslt_frame(0, b"eng", b"", b"[00:01.00]caf\xe9\n")];
        let (_dir, path) = write_synth_mp3("latin1.mp3", &frames);

        let doc = load_for_track(&path)
            .expect("load lyrics")
            .expect("embedded lyrics present");
        assert_eq!(doc.lines.len(), 1);
        assert_eq!(doc.lines[0].text, "caf\u{e9}");
    }

    #[test]
    fn embedded_uslt_decodes_utf16_text() {
        let text = utf16le_with_bom("[00:01.00]h\u{e9}llo\n");
        let frames = [uslt_frame(1, b"eng", b"", &text)];
        let (_dir, path) = write_synth_mp3("utf16.mp3", &frames);

        let doc = load_for_track(&path)
            .expect("load lyrics")
            .expect("embedded lyrics present");
        assert_eq!(doc.lines.len(), 1);
        assert_eq!(doc.lines[0].text, "h\u{e9}llo");
    }

    #[test]
    fn embedded_uslt_skips_empty_frames() {
        let frames = [
            uslt_frame(3, b"eng", b"", b""),
            uslt_frame(3, b"eng", b"", b"[00:03.00]third\n"),
        ];
        let (_dir, path) = write_synth_mp3("empty.mp3", &frames);

        let doc = load_for_track(&path)
            .expect("load lyrics")
            .expect("embedded lyrics present");
        assert_eq!(doc.lines.len(), 1);
        assert_eq!(doc.lines[0].text, "third");
    }

    #[test]
    fn embedded_plain_text_uslt_loads_as_untimed() {
        let frames = [uslt_frame(3, b"eng", b"", b"just some words\nmore words\n")];
        let (_dir, path) = write_synth_mp3("plain.mp3", &frames);

        let doc = load_for_track(&path)
            .expect("load lyrics")
            .expect("embedded lyrics present");
        assert_eq!(doc.source, LyricsSource::Embedded);
        assert_eq!(doc.precision, LyricsTimingPrecision::None);
        assert_eq!(doc.lines.len(), 2);
        assert_eq!(doc.lines[0].timestamp_ms, None);
    }

    fn count_uslt_frames(raw: &[u8]) -> usize {
        raw.windows(4).filter(|w| *w == *b"USLT").count()
    }

    #[test]
    fn write_embedded_lyrics_round_trips_through_metadata() {
        let frames = [text_frame(b"TIT2", b"Test Song")];
        let (_dir, path) = write_synth_mp3("roundtrip.mp3", &frames);

        let lrc = "[00:01.00]hello\n[00:02.50]world\n";
        crate::library::write_embedded_lyrics(&path, lrc).expect("embed lyrics");

        // The raw tag now carries a USLT frame ...
        let raw = std::fs::read(&path).expect("read mp3");
        assert_eq!(count_uslt_frames(&raw), 1, "expected one USLT frame");

        // ... and it loads back as embedded lyrics.
        let doc = load_for_track(&path)
            .expect("load lyrics")
            .expect("embedded lyrics present");
        assert_eq!(doc.source, LyricsSource::Embedded);
        assert_eq!(doc.precision, LyricsTimingPrecision::Line);
        assert_eq!(doc.lines.len(), 2);
        assert_eq!(doc.lines[0].text, "hello");
        assert_eq!(doc.lines[0].timestamp_ms, Some(1000));
        assert_eq!(doc.lines[1].text, "world");
        assert_eq!(doc.lines[1].timestamp_ms, Some(2500));
    }

    #[test]
    fn write_embedded_lyrics_replaces_existing_uslt_frames() {
        let frames = [
            uslt_frame(3, b"fra", b"", b"[00:01.00]bonjour\n"),
            uslt_frame(3, b"eng", b"", b"[00:01.00]stale\n"),
        ];
        let (_dir, path) = write_synth_mp3("replace.mp3", &frames);

        crate::library::write_embedded_lyrics(&path, "[00:05.00]fresh\n").expect("embed lyrics");

        let raw = std::fs::read(&path).expect("read mp3");
        assert_eq!(
            count_uslt_frames(&raw),
            1,
            "old USLT frames must be replaced, not duplicated"
        );

        let doc = load_for_track(&path)
            .expect("load lyrics")
            .expect("embedded lyrics present");
        assert_eq!(doc.lines.len(), 1);
        assert_eq!(doc.lines[0].text, "fresh");
        assert_eq!(doc.lines[0].timestamp_ms, Some(5000));
    }

    #[test]
    fn write_embedded_lyrics_keeps_other_tag_frames() {
        let frames = [
            text_frame(b"TIT2", b"Test Song"),
            uslt_frame(3, b"eng", b"", b"[00:01.00]stale\n"),
        ];
        let (_dir, path) = write_synth_mp3("preserve.mp3", &frames);

        crate::library::write_embedded_lyrics(&path, "[00:01.00]fresh\n").expect("embed lyrics");

        let raw = std::fs::read(&path).expect("read mp3");
        assert!(
            raw.windows(4).any(|w| *w == *b"TIT2"),
            "title frame must survive the lyrics embed"
        );
    }
}
