use std::path::PathBuf;
use tune::core::TuneCore;
use tune::model::{PersistedState, PlaybackMode, Track};

#[test]
fn playlist_flow_works() {
    let mut core = TuneCore::from_persisted(PersistedState::default());
    core.tracks = vec![
        Track {
            path: PathBuf::from("a.mp3"),
            title: String::from("a"),
            artist: None,
            album: None,
        },
        Track {
            path: PathBuf::from("b.mp3"),
            title: String::from("b"),
            artist: None,
            album: None,
        },
    ];
    core.reset_main_queue();

    core.create_playlist("mix");
    core.add_selected_to_playlist("mix");
    core.select_next();
    core.add_selected_to_playlist("mix");
    core.load_playlist_queue("mix");

    assert_eq!(core.queue.len(), 2);
}

#[test]
fn loop_one_repeats_same_track() {
    let mut core = TuneCore::from_persisted(PersistedState::default());
    core.tracks = vec![Track {
        path: PathBuf::from("song.mp3"),
        title: String::from("song"),
        artist: None,
        album: None,
    }];
    core.reset_main_queue();
    core.playback_mode = PlaybackMode::LoopOne;
    core.current_queue_index = Some(0);

    let next = core.next_track_path().expect("next");
    assert_eq!(next, PathBuf::from("song.mp3"));
}
