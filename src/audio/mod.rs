use anyhow::{Context, Result};
use rodio::Source;
use rodio::cpal::traits::{DeviceTrait, HostTrait};
use rodio::{Decoder, OutputStream, OutputStreamBuilder, Sink};
#[cfg(unix)]
use std::ffi::CString;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::Duration;
use std::time::Instant;

const MAX_VOLUME: f32 = 2.5;

pub trait AudioEngine {
    fn play(&mut self, path: &Path) -> Result<()>;
    fn queue_crossfade(&mut self, path: &Path) -> Result<()>;
    fn tick(&mut self);
    fn pause(&mut self);
    fn resume(&mut self);
    fn stop(&mut self);
    fn is_paused(&self) -> bool;
    fn current_track(&self) -> Option<&Path>;
    fn position(&self) -> Option<Duration>;
    fn duration(&self) -> Option<Duration>;
    fn seek_to(&mut self, position: Duration) -> Result<()>;
    fn volume(&self) -> f32;
    fn set_volume(&mut self, volume: f32);
    fn output_name(&self) -> Option<String>;
    fn reload_driver(&mut self) -> Result<()>;
    fn available_outputs(&self) -> Vec<String>;
    fn selected_output_device(&self) -> Option<String>;
    fn set_output_device(&mut self, output: Option<&str>) -> Result<()>;
    fn loudness_normalization(&self) -> bool;
    fn set_loudness_normalization(&mut self, enabled: bool);
    fn crossfade_seconds(&self) -> u16;
    fn set_crossfade_seconds(&mut self, seconds: u16);
    fn crossfade_queued_track(&self) -> Option<&Path>;
    fn is_finished(&self) -> bool;
}

pub struct WasapiAudioEngine {
    stream: OutputStream,
    sink: Sink,
    next_sink: Option<Sink>,
    current: Option<PathBuf>,
    next_track: Option<PathBuf>,
    track_duration: Option<Duration>,
    next_track_duration: Option<Duration>,
    crossfade_started_at: Option<Instant>,
    volume: f32,
    selected_output: Option<String>,
    loudness_normalization: bool,
    crossfade_seconds: u16,
    track_gain: f32,
    next_track_gain: f32,
}

impl WasapiAudioEngine {
    pub fn new() -> Result<Self> {
        let (stream, sink) = Self::open_output_stream(None)?;

        Ok(Self {
            stream,
            sink,
            next_sink: None,
            current: None,
            next_track: None,
            track_duration: None,
            next_track_duration: None,
            crossfade_started_at: None,
            volume: 1.0,
            selected_output: None,
            loudness_normalization: false,
            crossfade_seconds: 0,
            track_gain: 1.0,
            next_track_gain: 1.0,
        })
    }

    fn effective_volume(&self) -> f32 {
        (self.volume * self.track_gain).clamp(0.0, MAX_VOLUME)
    }

    fn promote_next_if_ready(&mut self) {
        if !self.sink.empty() {
            return;
        }

        let Some(next_sink) = self.next_sink.take() else {
            return;
        };

        self.sink = next_sink;
        self.current = self.next_track.take();
        self.track_duration = self.next_track_duration.take();
        self.track_gain = self.next_track_gain;
        self.next_track_gain = 1.0;
        self.crossfade_started_at = None;
    }

    fn clear_next(&mut self) {
        if let Some(next) = self.next_sink.take() {
            next.stop();
        }
        self.next_track = None;
        self.next_track_duration = None;
        self.next_track_gain = 1.0;
        self.crossfade_started_at = None;
    }

    fn crossfade_progress(&self) -> f32 {
        let Some(started) = self.crossfade_started_at else {
            return 0.0;
        };
        let duration = self.crossfade_seconds.max(1) as f32;
        (started.elapsed().as_secs_f32() / duration).clamp(0.0, 1.0)
    }

    fn estimate_track_gain(path: &Path) -> Result<f32> {
        let file = File::open(path).with_context(|| {
            format!("failed to open track for loudness scan {}", path.display())
        })?;
        let source = Decoder::try_from(file)
            .with_context(|| format!("failed to decode for loudness scan {}", path.display()))?;

        let channels = usize::from(source.channels()).max(1);
        let sample_rate = usize::try_from(source.sample_rate())
            .unwrap_or(44_100)
            .max(1);
        let max_samples = sample_rate.saturating_mul(channels).saturating_mul(10);

        let mut sum_sq = 0.0_f64;
        let mut count = 0_u64;
        for sample in source.take(max_samples) {
            let v = f64::from(sample);
            sum_sq += v * v;
            count = count.saturating_add(1);
        }

        if count == 0 {
            return Ok(1.0);
        }

        let rms = (sum_sq / count as f64).sqrt();
        if !(rms.is_finite()) || rms <= 0.000_01 {
            return Ok(1.0);
        }

        let target_rms = 0.20_f64;
        Ok((target_rms / rms).clamp(0.5, 1.8) as f32)
    }

    fn streamed_wav_has_unknown_duration(path: &Path) -> bool {
        if !path
            .to_string_lossy()
            .to_ascii_lowercase()
            .contains("tunetui_stream_cache")
        {
            return false;
        }
        if !path
            .extension()
            .and_then(|ext| ext.to_str())
            .is_some_and(|ext| ext.eq_ignore_ascii_case("wav"))
        {
            return false;
        }
        let mut file = match File::open(path) {
            Ok(file) => file,
            Err(_) => return false,
        };
        if file.seek(SeekFrom::Start(40)).is_err() {
            return false;
        }
        let mut data_size = [0_u8; 4];
        if file.read_exact(&mut data_size).is_err() {
            return false;
        }
        u32::from_le_bytes(data_size) == u32::MAX
    }

    fn open_output_stream(output: Option<&str>) -> Result<(OutputStream, Sink)> {
        let mut stream = with_silenced_stderr(|| {
            let host = rodio::cpal::default_host();
            if let Some(requested) = output {
                let device = host
                    .output_devices()
                    .context("failed to enumerate output devices")?
                    .find(|candidate| candidate.name().ok().as_deref() == Some(requested))
                    .with_context(|| format!("audio output device not found: {requested}"))?;
                OutputStreamBuilder::from_device(device)
                    .context("failed to open selected output device")?
                    .with_error_callback(|_| {})
                    .open_stream_or_fallback()
                    .context("failed to start selected output stream")
            } else {
                match OutputStreamBuilder::from_default_device()
                    .context("failed to open default system output stream")
                    .and_then(|builder| {
                        builder
                            .with_error_callback(|_| {})
                            .open_stream_or_fallback()
                            .context("failed to start default output stream")
                    }) {
                    Ok(stream) => Ok(stream),
                    Err(default_err) => {
                        let mut candidates: Vec<String> = host
                            .output_devices()
                            .ok()
                            .into_iter()
                            .flatten()
                            .filter_map(|device| device.name().ok())
                            .collect();
                        candidates.sort_by_cached_key(|name| {
                            let lower = name.to_ascii_lowercase();
                            let rank = if lower.contains("pulse") {
                                0_u8
                            } else if lower.contains("pipewire") {
                                1_u8
                            } else if lower.contains("default") {
                                2_u8
                            } else {
                                3_u8
                            };
                            (rank, lower)
                        });
                        candidates.dedup();

                        let mut started: Option<OutputStream> = None;
                        for candidate in candidates {
                            let device =
                                match host.output_devices().ok().into_iter().flatten().find(
                                    |entry| {
                                        entry.name().ok().as_deref() == Some(candidate.as_str())
                                    },
                                ) {
                                    Some(device) => device,
                                    None => continue,
                                };
                            let opened = OutputStreamBuilder::from_device(device)
                                .context("failed to open fallback output device")
                                .and_then(|builder| {
                                    builder
                                        .with_error_callback(|_| {})
                                        .open_stream_or_fallback()
                                        .context("failed to start fallback output stream")
                                });
                            if let Ok(stream) = opened {
                                started = Some(stream);
                                break;
                            }
                        }

                        started.with_context(|| {
                            format!(
                                "unable to start any audio output stream after default failed: {default_err:#}"
                            )
                        })
                    }
                }
            }
        })?;
        stream.log_on_drop(false);
        let sink = Sink::connect_new(stream.mixer());
        Ok((stream, sink))
    }

    fn reload_stream(&mut self) -> Result<()> {
        let current_track = self.current.clone();
        let was_paused = self.sink.is_paused();
        let selected = self.selected_output.clone();

        let (stream, sink) = Self::open_output_stream(selected.as_deref())?;
        self.stream = stream;
        self.sink = sink;
        self.sink.set_volume(self.effective_volume());
        self.clear_next();

        if let Some(path) = current_track {
            self.play(&path)?;
            if was_paused {
                self.pause();
            }
        }

        Ok(())
    }
}

impl AudioEngine for WasapiAudioEngine {
    fn play(&mut self, path: &Path) -> Result<()> {
        self.sink.stop();
        self.clear_next();
        self.sink = Sink::connect_new(self.stream.mixer());

        let file =
            File::open(path).with_context(|| format!("failed to open track {}", path.display()))?;
        let source = Decoder::try_from(file)
            .with_context(|| format!("failed to decode {}", path.display()))?;
        self.track_duration = if Self::streamed_wav_has_unknown_duration(path) {
            None
        } else {
            source.total_duration()
        };
        self.sink.append(source);

        self.track_gain = if self.loudness_normalization {
            Self::estimate_track_gain(path).unwrap_or(1.0)
        } else {
            1.0
        };
        self.sink.set_volume(self.effective_volume());
        self.current = Some(path.to_path_buf());
        Ok(())
    }

    fn queue_crossfade(&mut self, path: &Path) -> Result<()> {
        if self.crossfade_seconds == 0
            || self.current.is_none()
            || self.sink.empty()
            || self.sink.is_paused()
        {
            return self.play(path);
        }

        self.clear_next();
        let next_sink = Sink::connect_new(self.stream.mixer());

        let file =
            File::open(path).with_context(|| format!("failed to open track {}", path.display()))?;
        let source = Decoder::try_from(file)
            .with_context(|| format!("failed to decode {}", path.display()))?;
        let next_duration = if Self::streamed_wav_has_unknown_duration(path) {
            None
        } else {
            source.total_duration()
        };
        next_sink.append(source);

        let next_gain = if self.loudness_normalization {
            Self::estimate_track_gain(path).unwrap_or(1.0)
        } else {
            1.0
        };
        next_sink.set_volume(0.0);

        if self.sink.is_paused() {
            next_sink.pause();
        }

        self.next_track = Some(path.to_path_buf());
        self.next_track_duration = next_duration;
        self.next_track_gain = next_gain;
        self.next_sink = Some(next_sink);
        self.crossfade_started_at = Some(Instant::now());
        Ok(())
    }

    fn tick(&mut self) {
        let Some(next_sink) = self.next_sink.as_ref() else {
            return;
        };

        let progress = self.crossfade_progress();
        self.sink
            .set_volume((self.effective_volume() * (1.0 - progress)).clamp(0.0, MAX_VOLUME));
        next_sink
            .set_volume((self.volume * self.next_track_gain * progress).clamp(0.0, MAX_VOLUME));

        if self.sink.empty() {
            self.promote_next_if_ready();
            self.sink.set_volume(self.effective_volume());
        }
    }

    fn pause(&mut self) {
        self.sink.pause();
        if let Some(next) = &self.next_sink {
            next.pause();
        }
    }

    fn resume(&mut self) {
        self.sink.play();
        if let Some(next) = &self.next_sink {
            next.play();
        }
    }

    fn stop(&mut self) {
        self.sink.stop();
        self.clear_next();
        self.current = None;
        self.next_track = None;
        self.track_duration = None;
        self.next_track_duration = None;
        self.track_gain = 1.0;
        self.next_track_gain = 1.0;
    }

    fn is_paused(&self) -> bool {
        self.sink.is_paused()
    }

    fn current_track(&self) -> Option<&Path> {
        self.current.as_deref()
    }

    fn position(&self) -> Option<Duration> {
        self.current.as_ref()?;
        Some(self.sink.get_pos())
    }

    fn duration(&self) -> Option<Duration> {
        self.track_duration
    }

    fn seek_to(&mut self, position: Duration) -> Result<()> {
        if self.current.is_none() {
            return Err(anyhow::anyhow!("no active track"));
        }

        self.clear_next();
        self.sink
            .try_seek(position)
            .map_err(|err| anyhow::anyhow!("failed to seek current track: {err:?}"))?;
        self.sink.set_volume(self.effective_volume());
        Ok(())
    }

    fn volume(&self) -> f32 {
        self.volume
    }

    fn set_volume(&mut self, volume: f32) {
        self.volume = volume.clamp(0.0, MAX_VOLUME);
        let progress = self.crossfade_progress();
        self.sink
            .set_volume((self.effective_volume() * (1.0 - progress)).clamp(0.0, MAX_VOLUME));
        if let Some(next) = &self.next_sink {
            next.set_volume((self.volume * self.next_track_gain * progress).clamp(0.0, MAX_VOLUME));
        }
    }

    fn output_name(&self) -> Option<String> {
        self.stream.config().channel_count().checked_sub(0)?;
        Some(
            self.selected_output
                .clone()
                .unwrap_or_else(|| "System default output (CPAL)".to_string()),
        )
    }

    fn reload_driver(&mut self) -> Result<()> {
        self.reload_stream()
    }

    fn available_outputs(&self) -> Vec<String> {
        let mut outputs: Vec<String> = with_silenced_stderr(|| {
            rodio::cpal::default_host()
                .output_devices()
                .ok()
                .into_iter()
                .flatten()
                .filter_map(|device| device.name().ok())
                .collect()
        });
        outputs.sort_by_cached_key(|name| name.to_ascii_lowercase());
        outputs.dedup();
        outputs
    }

    fn selected_output_device(&self) -> Option<String> {
        self.selected_output.clone()
    }

    fn set_output_device(&mut self, output: Option<&str>) -> Result<()> {
        let previous = self.selected_output.clone();
        self.selected_output = output.map(ToOwned::to_owned);
        if let Err(err) = self.reload_stream() {
            self.selected_output = previous;
            return Err(err);
        }
        Ok(())
    }

    fn loudness_normalization(&self) -> bool {
        self.loudness_normalization
    }

    fn set_loudness_normalization(&mut self, enabled: bool) {
        self.loudness_normalization = enabled;
        if !enabled || self.current.is_none() {
            self.track_gain = 1.0;
            self.next_track_gain = 1.0;
            let progress = self.crossfade_progress();
            self.sink
                .set_volume((self.effective_volume() * (1.0 - progress)).clamp(0.0, MAX_VOLUME));
            if let Some(next) = &self.next_sink {
                next.set_volume(
                    (self.volume * self.next_track_gain * progress).clamp(0.0, MAX_VOLUME),
                );
            }
        }
    }

    fn crossfade_seconds(&self) -> u16 {
        self.crossfade_seconds
    }

    fn set_crossfade_seconds(&mut self, seconds: u16) {
        self.crossfade_seconds = seconds.min(10);
    }

    fn crossfade_queued_track(&self) -> Option<&Path> {
        self.next_track.as_deref()
    }

    fn is_finished(&self) -> bool {
        if self.next_sink.is_some() {
            return false;
        }
        self.current.is_some() && !self.sink.is_paused() && self.sink.empty()
    }
}

#[cfg(unix)]
fn with_silenced_stderr<T>(operation: impl FnOnce() -> T) -> T {
    let saved = unsafe { libc::dup(libc::STDERR_FILENO) };
    if saved < 0 {
        return operation();
    }

    let devnull = CString::new("/dev/null")
        .ok()
        .map(|path| unsafe { libc::open(path.as_ptr(), libc::O_WRONLY) })
        .unwrap_or(-1);

    if devnull >= 0 {
        unsafe {
            libc::dup2(devnull, libc::STDERR_FILENO);
            libc::close(devnull);
        }
    }

    let result = operation();

    unsafe {
        libc::dup2(saved, libc::STDERR_FILENO);
        libc::close(saved);
    }

    result
}

#[cfg(not(unix))]
fn with_silenced_stderr<T>(operation: impl FnOnce() -> T) -> T {
    operation()
}

pub struct NullAudioEngine {
    paused: bool,
    current: Option<PathBuf>,
    volume: f32,
    started_at: Option<Instant>,
    position_offset: Duration,
    track_duration: Option<Duration>,
}

impl NullAudioEngine {
    pub fn new() -> Self {
        Self {
            paused: false,
            current: None,
            volume: 1.0,
            started_at: None,
            position_offset: Duration::ZERO,
            track_duration: None,
        }
    }

    fn estimate_duration(path: &Path) -> Option<Duration> {
        let file = File::open(path).ok()?;
        let source = Decoder::try_from(file).ok()?;
        source
            .total_duration()
            .filter(|duration| !duration.is_zero())
    }

    fn current_position(&self) -> Duration {
        let mut position = self.position_offset;
        if !self.paused
            && self.current.is_some()
            && let Some(started_at) = self.started_at
        {
            position = position.saturating_add(started_at.elapsed());
        }
        if let Some(duration) = self.track_duration {
            return position.min(duration);
        }
        position
    }
}

impl Default for NullAudioEngine {
    fn default() -> Self {
        Self::new()
    }
}

impl AudioEngine for NullAudioEngine {
    fn play(&mut self, path: &Path) -> Result<()> {
        self.paused = false;
        self.current = Some(path.to_path_buf());
        self.started_at = Some(Instant::now());
        self.position_offset = Duration::ZERO;
        self.track_duration = Self::estimate_duration(path);
        Ok(())
    }

    fn queue_crossfade(&mut self, path: &Path) -> Result<()> {
        self.play(path)
    }

    fn tick(&mut self) {}

    fn pause(&mut self) {
        self.position_offset = self.current_position();
        self.started_at = None;
        self.paused = true;
    }

    fn resume(&mut self) {
        if self.current.is_some() {
            self.started_at = Some(Instant::now());
        }
        self.paused = false;
    }

    fn stop(&mut self) {
        self.current = None;
        self.paused = false;
        self.started_at = None;
        self.position_offset = Duration::ZERO;
        self.track_duration = None;
    }

    fn is_paused(&self) -> bool {
        self.paused
    }

    fn current_track(&self) -> Option<&Path> {
        self.current.as_deref()
    }

    fn position(&self) -> Option<Duration> {
        self.current.as_ref()?;
        Some(self.current_position())
    }

    fn duration(&self) -> Option<Duration> {
        self.track_duration
    }

    fn seek_to(&mut self, position: Duration) -> Result<()> {
        if self.current.is_none() {
            return Err(anyhow::anyhow!("no active track"));
        }

        self.position_offset = self
            .track_duration
            .map_or(position, |duration| position.min(duration));
        self.started_at = if self.paused {
            None
        } else {
            Some(Instant::now())
        };
        Ok(())
    }

    fn volume(&self) -> f32 {
        self.volume
    }

    fn set_volume(&mut self, volume: f32) {
        self.volume = volume.clamp(0.0, MAX_VOLUME);
    }

    fn output_name(&self) -> Option<String> {
        Some("Null audio engine".to_string())
    }

    fn reload_driver(&mut self) -> Result<()> {
        Ok(())
    }

    fn available_outputs(&self) -> Vec<String> {
        Vec::new()
    }

    fn selected_output_device(&self) -> Option<String> {
        None
    }

    fn set_output_device(&mut self, _output: Option<&str>) -> Result<()> {
        Ok(())
    }

    fn loudness_normalization(&self) -> bool {
        false
    }

    fn set_loudness_normalization(&mut self, _enabled: bool) {}

    fn crossfade_seconds(&self) -> u16 {
        0
    }

    fn set_crossfade_seconds(&mut self, _seconds: u16) {}

    fn crossfade_queued_track(&self) -> Option<&Path> {
        None
    }

    fn is_finished(&self) -> bool {
        let Some(duration) = self.track_duration else {
            return false;
        };
        self.current.is_some() && !self.paused && self.current_position() >= duration
    }
}

#[cfg(test)]
mod tests {
    use super::{AudioEngine, NullAudioEngine};
    use std::env;
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::thread;
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    fn unique_test_dir(name: &str) -> PathBuf {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time should be valid")
            .as_nanos();
        let dir = env::temp_dir().join(format!("tunetui-{name}-{stamp}"));
        fs::create_dir_all(&dir).expect("temp dir should be created");
        dir
    }

    fn write_test_wav(path: &Path, duration_ms: u32) {
        let sample_rate: u32 = 44_100;
        let channels: u16 = 1;
        let bits_per_sample: u16 = 16;
        let bytes_per_sample = u32::from(bits_per_sample / 8);
        let total_samples = (u64::from(sample_rate) * u64::from(duration_ms) / 1_000) as u32;
        let data_size = total_samples * u32::from(channels) * bytes_per_sample;
        let byte_rate = sample_rate * u32::from(channels) * bytes_per_sample;
        let block_align = channels * (bits_per_sample / 8);
        let riff_chunk_size = 36_u32.saturating_add(data_size);

        let mut bytes = Vec::with_capacity((44_u32 + data_size) as usize);
        bytes.extend_from_slice(b"RIFF");
        bytes.extend_from_slice(&riff_chunk_size.to_le_bytes());
        bytes.extend_from_slice(b"WAVE");
        bytes.extend_from_slice(b"fmt ");
        bytes.extend_from_slice(&16_u32.to_le_bytes());
        bytes.extend_from_slice(&1_u16.to_le_bytes());
        bytes.extend_from_slice(&channels.to_le_bytes());
        bytes.extend_from_slice(&sample_rate.to_le_bytes());
        bytes.extend_from_slice(&byte_rate.to_le_bytes());
        bytes.extend_from_slice(&block_align.to_le_bytes());
        bytes.extend_from_slice(&bits_per_sample.to_le_bytes());
        bytes.extend_from_slice(b"data");
        bytes.extend_from_slice(&data_size.to_le_bytes());
        bytes.resize((44_u32 + data_size) as usize, 0_u8);

        fs::write(path, bytes).expect("wav fixture should be written");
    }

    #[test]
    fn null_engine_position_advances_when_playing() {
        let mut engine = NullAudioEngine::new();
        engine
            .play(Path::new("nonexistent-track.flac"))
            .expect("play should still work in null mode");
        let before = engine.position().expect("position should be present");
        thread::sleep(Duration::from_millis(20));
        let after = engine.position().expect("position should be present");
        assert!(after > before, "position should advance while playing");
    }

    #[test]
    fn null_engine_pause_and_resume_control_position_progression() {
        let mut engine = NullAudioEngine::new();
        engine
            .play(Path::new("nonexistent-track.flac"))
            .expect("play should still work in null mode");
        thread::sleep(Duration::from_millis(20));

        engine.pause();
        let paused = engine.position().expect("position should be present");
        thread::sleep(Duration::from_millis(20));
        let paused_later = engine.position().expect("position should be present");
        assert_eq!(paused_later, paused, "position should freeze while paused");

        engine.resume();
        thread::sleep(Duration::from_millis(20));
        let resumed = engine.position().expect("position should be present");
        assert!(resumed > paused, "position should continue after resume");
    }

    #[test]
    fn null_engine_seek_updates_position() {
        let mut engine = NullAudioEngine::new();
        engine
            .play(Path::new("nonexistent-track.flac"))
            .expect("play should still work in null mode");

        let target = Duration::from_secs(12);
        engine.seek_to(target).expect("seek should succeed");
        let position = engine.position().expect("position should be present");
        assert!(position >= target, "seek should move logical position");
    }

    #[test]
    fn null_engine_finishes_when_known_duration_elapses() {
        let dir = unique_test_dir("null-engine-duration");
        let track = dir.join("fixture.wav");
        write_test_wav(&track, 80);

        let mut engine = NullAudioEngine::new();
        engine
            .play(&track)
            .expect("play should succeed for wav fixture");
        let duration = engine.duration().expect("duration should be detected");
        assert!(duration >= Duration::from_millis(70));

        thread::sleep(Duration::from_millis(120));
        assert!(
            engine.is_finished(),
            "known-duration playback should finish"
        );

        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn null_engine_unknown_duration_does_not_auto_finish() {
        let mut engine = NullAudioEngine::new();
        engine
            .play(Path::new("nonexistent-track.flac"))
            .expect("play should still work in null mode");
        assert_eq!(engine.duration(), None);

        thread::sleep(Duration::from_millis(80));
        assert!(
            !engine.is_finished(),
            "unknown-duration playback should remain active"
        );
    }

    #[test]
    fn null_engine_zero_length_duration_does_not_pin_position_to_zero() {
        let dir = unique_test_dir("null-engine-zero-duration");
        let track = dir.join("zero.wav");
        write_test_wav(&track, 0);

        let mut engine = NullAudioEngine::new();
        engine
            .play(&track)
            .expect("play should succeed for zero-length wav fixture");

        thread::sleep(Duration::from_millis(20));
        let position = engine.position().expect("position should be present");
        assert!(
            position > Duration::ZERO,
            "logical clock should still advance when decoded duration is zero"
        );
        assert!(
            !engine.is_finished(),
            "zero-length decoded duration should be treated as unknown"
        );

        let _ = fs::remove_dir_all(dir);
    }
}
