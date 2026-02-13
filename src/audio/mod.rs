use anyhow::{Context, Result};
use rodio::{Decoder, OutputStream, OutputStreamBuilder, Sink, Source};
use std::fs::File;
use std::path::{Path, PathBuf};
use std::time::Duration;

const MAX_VOLUME: f32 = 2.5;

pub trait AudioEngine {
    fn play(&mut self, path: &Path) -> Result<()>;
    fn pause(&mut self);
    fn resume(&mut self);
    fn stop(&mut self);
    fn is_paused(&self) -> bool;
    fn current_track(&self) -> Option<&Path>;
    fn position(&self) -> Option<Duration>;
    fn duration(&self) -> Option<Duration>;
    fn volume(&self) -> f32;
    fn set_volume(&mut self, volume: f32);
    fn output_name(&self) -> Option<String>;
    fn is_finished(&self) -> bool;
}

pub struct WasapiAudioEngine {
    stream: OutputStream,
    sink: Sink,
    current: Option<PathBuf>,
    track_duration: Option<Duration>,
    volume: f32,
}

impl WasapiAudioEngine {
    pub fn new() -> Result<Self> {
        let stream = OutputStreamBuilder::open_default_stream()
            .context("failed to open default system output stream")?;
        let sink = Sink::connect_new(stream.mixer());

        Ok(Self {
            stream,
            sink,
            current: None,
            track_duration: None,
            volume: 1.0,
        })
    }
}

impl AudioEngine for WasapiAudioEngine {
    fn play(&mut self, path: &Path) -> Result<()> {
        self.sink.stop();
        self.sink = Sink::connect_new(self.stream.mixer());
        self.sink.set_volume(self.volume);

        let file =
            File::open(path).with_context(|| format!("failed to open track {}", path.display()))?;
        let source = Decoder::try_from(file)
            .with_context(|| format!("failed to decode {}", path.display()))?;
        self.track_duration = source.total_duration();
        self.sink.append(source);
        self.current = Some(path.to_path_buf());
        Ok(())
    }

    fn pause(&mut self) {
        self.sink.pause();
    }

    fn resume(&mut self) {
        self.sink.play();
    }

    fn stop(&mut self) {
        self.sink.stop();
        self.current = None;
        self.track_duration = None;
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

    fn volume(&self) -> f32 {
        self.volume
    }

    fn set_volume(&mut self, volume: f32) {
        self.volume = volume.clamp(0.0, MAX_VOLUME);
        self.sink.set_volume(self.volume);
    }

    fn output_name(&self) -> Option<String> {
        self.stream.config().channel_count().checked_sub(0)?;
        Some("System default output (WASAPI/CPAL)".to_string())
    }

    fn is_finished(&self) -> bool {
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

    fn volume(&self) -> f32 {
        self.volume
    }

    fn set_volume(&mut self, volume: f32) {
        self.volume = volume.clamp(0.0, MAX_VOLUME);
    }

    fn output_name(&self) -> Option<String> {
        Some("Null audio engine".to_string())
    }

    fn is_finished(&self) -> bool {
        false
    }
}
