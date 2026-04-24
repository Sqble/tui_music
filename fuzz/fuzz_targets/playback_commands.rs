#![no_main]

use libfuzzer_sys::fuzz_target;
use std::path::PathBuf;
use tune::core::TuneCore;
use tune::model::{PersistedState, RepeatMode, Track};

fuzz_target!(|data: &[u8]| {
    let mut core = TuneCore::from_persisted(PersistedState::default());
    let len = (data.len() % 32).max(1);
    core.tracks = (0..len)
        .map(|idx| Track {
            path: PathBuf::from(format!("track_{idx}.mp3")),
            title: format!("track_{idx}"),
            artist: None,
            album: None,
        })
        .collect();
    core.reset_main_queue();
    core.current_queue_index = Some(0);

    for byte in data {
        match byte % 6 {
            0 => core.shuffle_enabled = false,
            1 => core.shuffle_enabled = true,
            2 => core.repeat_mode = RepeatMode::Off,
            3 => core.repeat_mode = RepeatMode::All,
            4 => {
                let _ = core.next_track_path();
            }
            _ => core.cycle_repeat_mode(),
        }
    }
});
