use rand::Rng;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

const ROOM_CODE_LEN: usize = 6;
const MAX_SHARED_QUEUE_ITEMS: usize = 512;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum OnlineRoomMode {
    Collaborative,
    HostOnly,
}

impl OnlineRoomMode {
    pub fn toggle(self) -> Self {
        match self {
            Self::Collaborative => Self::HostOnly,
            Self::HostOnly => Self::Collaborative,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Collaborative => "Collaborative",
            Self::HostOnly => "Host-only DJ",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum StreamQuality {
    Lossless,
    Balanced,
}

impl StreamQuality {
    pub fn next(self) -> Self {
        match self {
            Self::Lossless => Self::Balanced,
            Self::Balanced => Self::Lossless,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Lossless => "Lossless",
            Self::Balanced => "Balanced",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum QueueDelivery {
    PreferLocalWithStreamFallback,
    HostStreamOnly,
}

impl QueueDelivery {
    pub fn label(self) -> &'static str {
        match self {
            Self::PreferLocalWithStreamFallback => "Local+stream fallback",
            Self::HostStreamOnly => "Host stream",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SharedQueueItem {
    pub path: PathBuf,
    pub title: String,
    pub delivery: QueueDelivery,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Participant {
    pub nickname: String,
    pub is_local: bool,
    pub is_host: bool,
    pub ping_ms: u16,
    pub manual_extra_delay_ms: u16,
    pub auto_ping_delay: bool,
}

impl Participant {
    pub fn effective_delay_ms(&self) -> u16 {
        if self.auto_ping_delay {
            self.ping_ms.saturating_add(self.manual_extra_delay_ms)
        } else {
            self.manual_extra_delay_ms
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum TransportCommand {
    SetPaused {
        paused: bool,
    },
    PlayTrack {
        path: PathBuf,
    },
    SetPlaybackState {
        path: PathBuf,
        position_ms: u64,
        paused: bool,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TransportEnvelope {
    pub seq: u64,
    pub origin_nickname: String,
    pub command: TransportCommand,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OnlineSession {
    pub room_code: String,
    pub mode: OnlineRoomMode,
    pub quality: StreamQuality,
    pub participants: Vec<Participant>,
    pub shared_queue: Vec<SharedQueueItem>,
    pub last_sync_drift_ms: i32,
    pub last_transport: Option<TransportEnvelope>,
}

impl OnlineSession {
    pub fn host(local_nickname: &str) -> Self {
        Self {
            room_code: generate_room_code(),
            mode: OnlineRoomMode::Collaborative,
            quality: StreamQuality::Lossless,
            participants: vec![Participant {
                nickname: normalized_nickname(local_nickname),
                is_local: true,
                is_host: true,
                ping_ms: 18,
                manual_extra_delay_ms: 0,
                auto_ping_delay: true,
            }],
            shared_queue: Vec::new(),
            last_sync_drift_ms: 0,
            last_transport: None,
        }
    }

    pub fn join(room_code: &str, local_nickname: &str) -> Self {
        let mut code = room_code.trim().to_ascii_uppercase();
        if code.is_empty() {
            code = generate_room_code();
        }
        Self {
            room_code: code,
            mode: OnlineRoomMode::Collaborative,
            quality: StreamQuality::Lossless,
            participants: vec![Participant {
                nickname: normalized_nickname(local_nickname),
                is_local: true,
                is_host: false,
                ping_ms: 42,
                manual_extra_delay_ms: 0,
                auto_ping_delay: true,
            }],
            shared_queue: Vec::new(),
            last_sync_drift_ms: 0,
            last_transport: None,
        }
    }

    pub fn local_participant(&self) -> Option<&Participant> {
        self.participants.iter().find(|entry| entry.is_local)
    }

    pub fn local_participant_mut(&mut self) -> Option<&mut Participant> {
        self.participants.iter_mut().find(|entry| entry.is_local)
    }

    pub fn can_local_control_playback(&self) -> bool {
        self.local_participant()
            .is_some_and(|local| self.mode == OnlineRoomMode::Collaborative || local.is_host)
    }

    pub fn toggle_mode(&mut self) {
        self.mode = self.mode.toggle();
    }

    pub fn cycle_quality(&mut self) {
        self.quality = self.quality.next();
    }

    pub fn toggle_local_auto_delay(&mut self) {
        if let Some(local) = self.local_participant_mut() {
            local.auto_ping_delay = !local.auto_ping_delay;
        }
    }

    pub fn adjust_local_manual_delay(&mut self, delta_ms: i16) {
        if let Some(local) = self.local_participant_mut() {
            let current = i32::from(local.manual_extra_delay_ms);
            let updated = (current + i32::from(delta_ms)).clamp(0, i32::from(u16::MAX));
            local.manual_extra_delay_ms = updated as u16;
        }
    }

    pub fn recalibrate_local_ping(&mut self) {
        let remote_count = self
            .participants
            .iter()
            .filter(|entry| !entry.is_local)
            .count();
        if let Some(local) = self.local_participant_mut() {
            let mut rng = rand::rng();
            let base = 18_u16.saturating_add((remote_count as u16).saturating_mul(4));
            let jitter = rng.random_range(0..18_u16);
            local.ping_ms = base.saturating_add(jitter);
            self.last_sync_drift_ms = rng.random_range(-35_i32..35_i32);
        }
    }

    pub fn add_simulated_listener(&mut self) -> String {
        let next_index = self
            .participants
            .iter()
            .filter(|entry| !entry.is_local)
            .count()
            .saturating_add(1);
        let nickname = format!("listener-{next_index}");
        let mut rng = rand::rng();
        self.participants.push(Participant {
            nickname: nickname.clone(),
            is_local: false,
            is_host: false,
            ping_ms: rng.random_range(22..120_u16),
            manual_extra_delay_ms: 0,
            auto_ping_delay: true,
        });
        nickname
    }

    pub fn remove_latest_listener(&mut self) -> Option<String> {
        let index = self
            .participants
            .iter()
            .rposition(|entry| !entry.is_local)?;
        Some(self.participants.remove(index).nickname)
    }

    pub fn push_shared_track(&mut self, path: &Path, title: String) {
        let delivery = if path.exists() {
            QueueDelivery::PreferLocalWithStreamFallback
        } else {
            QueueDelivery::HostStreamOnly
        };
        self.shared_queue.push(SharedQueueItem {
            path: path.to_path_buf(),
            title,
            delivery,
        });
        if self.shared_queue.len() > MAX_SHARED_QUEUE_ITEMS {
            let remove = self
                .shared_queue
                .len()
                .saturating_sub(MAX_SHARED_QUEUE_ITEMS);
            self.shared_queue.drain(0..remove);
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct OnlineState {
    pub session: Option<OnlineSession>,
}

impl OnlineState {
    pub fn leave_room(&mut self) {
        self.session = None;
    }

    pub fn host_room(&mut self, local_nickname: &str) {
        self.session = Some(OnlineSession::host(local_nickname));
    }

    pub fn join_room(&mut self, room_code: &str, local_nickname: &str) {
        self.session = Some(OnlineSession::join(room_code, local_nickname));
    }
}

fn normalized_nickname(input: &str) -> String {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        String::from("you")
    } else {
        trimmed.to_string()
    }
}

fn generate_room_code() -> String {
    const CHARS: &[u8] = b"ABCDEFGHJKLMNPQRSTUVWXYZ23456789";
    let mut rng = rand::rng();
    let mut out = String::with_capacity(ROOM_CODE_LEN);
    for _ in 0..ROOM_CODE_LEN {
        let idx = rng.random_range(0..CHARS.len());
        out.push(char::from(CHARS[idx]));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_session_starts_in_collaborative_lossless_mode() {
        let session = OnlineSession::host("dj");
        assert_eq!(session.mode, OnlineRoomMode::Collaborative);
        assert_eq!(session.quality, StreamQuality::Lossless);
        assert_eq!(session.participants.len(), 1);
        assert!(session.participants[0].is_host);
    }

    #[test]
    fn host_only_blocks_non_host_local_control() {
        let mut session = OnlineSession::join("ROOM22", "listener");
        session.mode = OnlineRoomMode::HostOnly;
        assert!(!session.can_local_control_playback());
    }

    #[test]
    fn effective_delay_uses_ping_when_auto_enabled() {
        let participant = Participant {
            nickname: String::from("x"),
            is_local: true,
            is_host: false,
            ping_ms: 35,
            manual_extra_delay_ms: 40,
            auto_ping_delay: true,
        };
        assert_eq!(participant.effective_delay_ms(), 75);
    }
}
