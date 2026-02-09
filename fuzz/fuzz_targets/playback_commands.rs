#![no_main]

use libfuzzer_sys::fuzz_target;
use std::path::PathBuf;
use tune::core::TuneCore;
use tune::model::{PersistedState, PlaybackMode};

fuzz_target!(|data: &[u8]| {
    let mut core = TuneCore::from_persisted(PersistedState::default());
    let len = (data.len() % 32).max(1);
    core.queue = (0..len)
        .map(|idx| PathBuf::from(format!("track_{idx}.mp3")))
        .collect();
    core.current_queue_index = Some(0);

    for byte in data {
        match byte % 6 {
            0 => core.playback_mode = PlaybackMode::Normal,
            1 => core.playback_mode = PlaybackMode::Shuffle,
            2 => core.playback_mode = PlaybackMode::Loop,
            3 => core.playback_mode = PlaybackMode::LoopOne,
            4 => {
                let _ = core.next_track_path();
            }
            _ => core.cycle_mode(),
        }
    }
});
