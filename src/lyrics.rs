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

fn read_embedded_lyrics(track_path: &Path) -> Option<String> {
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
        trimmed.starts_with('[') && parse_single_lrc_timestamp(trimmed).is_some()
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
}
