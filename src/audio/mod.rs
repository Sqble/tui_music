use anyhow::{Context, Result};
use rodio::Source;
use rodio::cpal::traits::{DeviceTrait, HostTrait};
use rodio::{Decoder, OutputStream, OutputStreamBuilder, Sink};
use std::fs::File;
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

    fn open_output_stream(output: Option<&str>) -> Result<(OutputStream, Sink)> {
        let mut stream = if let Some(requested) = output {
            let device = rodio::cpal::default_host()
                .output_devices()
                .context("failed to enumerate output devices")?
                .find(|candidate| candidate.name().ok().as_deref() == Some(requested))
                .with_context(|| format!("audio output device not found: {requested}"))?;
            OutputStreamBuilder::from_device(device)
                .context("failed to open selected output device")?
                .with_error_callback(|_| {})
                .open_stream_or_fallback()
                .context("failed to start selected output stream")?
        } else {
            OutputStreamBuilder::from_default_device()
                .context("failed to open default system output stream")?
                .with_error_callback(|_| {})
                .open_stream_or_fallback()
                .context("failed to start default output stream")?
        };
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
        self.track_duration = source.total_duration();
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
        let next_duration = source.total_duration();
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
        let mut outputs: Vec<String> = rodio::cpal::default_host()
            .output_devices()
            .ok()
            .into_iter()
            .flatten()
            .filter_map(|device| device.name().ok())
            .collect();
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

pub struct NullAudioEngine {
    paused: bool,
    current: Option<PathBuf>,
    volume: f32,
}

impl NullAudioEngine {
    pub fn new() -> Self {
        Self {
            paused: false,
            current: None,
            volume: 1.0,
        }
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
        Ok(())
    }

    fn queue_crossfade(&mut self, path: &Path) -> Result<()> {
        self.play(path)
    }

    fn tick(&mut self) {}

    fn pause(&mut self) {
        self.paused = true;
    }

    fn resume(&mut self) {
        self.paused = false;
    }

    fn stop(&mut self) {
        self.current = None;
    }

    fn is_paused(&self) -> bool {
        self.paused
    }

    fn current_track(&self) -> Option<&Path> {
        self.current.as_deref()
    }

    fn position(&self) -> Option<Duration> {
        None
    }

    fn duration(&self) -> Option<Duration> {
        None
    }

    fn seek_to(&mut self, _position: Duration) -> Result<()> {
        if self.current.is_none() {
            return Err(anyhow::anyhow!("no active track"));
        }
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
        false
    }
}
