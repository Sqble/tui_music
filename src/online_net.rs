use crate::online::{OnlineSession, SharedQueueItem, StreamQuality, TransportEnvelope};
use anyhow::Context;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpListener, TcpStream, UdpSocket};
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::thread;
use std::time::Duration;

const MAX_PEERS: usize = 8;
const INVITE_PREFIX_NO_PASSWORD: &str = "T1";
const INVITE_PREFIX_WITH_PASSWORD: &str = "T1P";
const INVITE_MAX_PASSWORD_BYTES: usize = 32;
const INVITE_ALPHABET: &[u8] = b"ABCDEFGHJKLMNPQRSTUVWXYZ23456789";
const INVITE_OBFUSCATION_KEY: &[u8] = b"TuneTuiInviteKeyV1";

pub struct DecodedInvite {
    pub server_addr: String,
    pub room_code: String,
    pub password: Option<String>,
}

#[derive(Debug, Clone)]
pub enum NetworkRole {
    Host,
    Client,
}

#[derive(Debug)]
pub enum NetworkEvent {
    SessionSync(OnlineSession),
    Status(String),
}

#[derive(Debug, Clone)]
pub enum LocalAction {
    SetMode(crate::online::OnlineRoomMode),
    SetQuality(StreamQuality),
    QueueAdd(SharedQueueItem),
    DelayUpdate {
        manual_extra_delay_ms: u16,
        auto_ping_delay: bool,
    },
    Transport(TransportEnvelope),
}

#[derive(Debug)]
enum NetworkCommand {
    LocalAction(LocalAction),
    Shutdown,
}

pub struct OnlineNetwork {
    role: NetworkRole,
    cmd_tx: Sender<NetworkCommand>,
    event_rx: Receiver<NetworkEvent>,
}

impl OnlineNetwork {
    pub fn role(&self) -> &NetworkRole {
        &self.role
    }

    pub fn start_host(
        bind_addr: &str,
        mut session: OnlineSession,
        expected_password: Option<String>,
    ) -> anyhow::Result<Self> {
        let listener = TcpListener::bind(bind_addr)
            .with_context(|| format!("failed to bind online host at {bind_addr}"))?;
        listener
            .set_nonblocking(true)
            .context("failed to set nonblocking listener")?;

        let (cmd_tx, cmd_rx) = mpsc::channel();
        let (event_tx, event_rx) = mpsc::channel();

        thread::spawn(move || {
            host_loop(listener, &mut session, expected_password, cmd_rx, event_tx)
        });

        Ok(Self {
            role: NetworkRole::Host,
            cmd_tx,
            event_rx,
        })
    }

    pub fn start_client(
        server_addr: &str,
        room_code: &str,
        nickname: &str,
        password: Option<String>,
    ) -> anyhow::Result<Self> {
        let mut stream = TcpStream::connect(server_addr)
            .with_context(|| format!("failed to connect to {server_addr}"))?;
        stream
            .set_nodelay(true)
            .context("failed to enable TCP_NODELAY")?;

        send_json_line(
            &mut stream,
            &WireClientMessage::Hello {
                room_code: room_code.to_string(),
                nickname: nickname.to_string(),
                password,
            },
        )
        .context("failed to send hello")?;

        let mut hello_reader = BufReader::new(
            stream
                .try_clone()
                .context("failed to clone client stream")?,
        );
        let mut line = String::new();
        let read = hello_reader
            .read_line(&mut line)
            .context("failed to read hello ack")?;
        if read == 0 {
            anyhow::bail!("server closed connection during handshake");
        }

        let ack: WireServerMessage =
            serde_json::from_str(line.trim_end()).context("failed to parse hello ack")?;
        match ack {
            WireServerMessage::HelloAck {
                accepted: true,
                reason: _,
                session: _,
            } => {}
            WireServerMessage::HelloAck {
                accepted: false,
                reason,
                session: _,
            } => {
                anyhow::bail!(reason.unwrap_or_else(|| String::from("server rejected connection")))
            }
            _ => anyhow::bail!("invalid handshake response from server"),
        }

        let (cmd_tx, cmd_rx) = mpsc::channel();
        let (event_tx, event_rx) = mpsc::channel();
        thread::spawn(move || client_loop(stream, cmd_rx, event_tx));

        Ok(Self {
            role: NetworkRole::Client,
            cmd_tx,
            event_rx,
        })
    }

    pub fn send_local_action(&self, action: LocalAction) {
        let _ = self.cmd_tx.send(NetworkCommand::LocalAction(action));
    }

    pub fn try_recv_event(&self) -> Option<NetworkEvent> {
        self.event_rx.try_recv().ok()
    }

    pub fn shutdown(&self) {
        let _ = self.cmd_tx.send(NetworkCommand::Shutdown);
    }
}

pub fn resolve_advertise_addr(bind_addr: &str) -> anyhow::Result<String> {
    let bind = parse_socket_addr_v4(bind_addr)?;
    let port = bind.port();
    let ip = if bind.ip().is_unspecified() {
        detect_local_ipv4().unwrap_or(Ipv4Addr::new(127, 0, 0, 1))
    } else {
        *bind.ip()
    };
    Ok(format!("{ip}:{port}"))
}

pub fn build_invite_code(
    server_addr: &str,
    password: Option<&str>,
    include_password: bool,
) -> anyhow::Result<String> {
    let socket = parse_socket_addr_v4(server_addr)?;
    let password_bytes = password.unwrap_or("").as_bytes();
    if password_bytes.len() > INVITE_MAX_PASSWORD_BYTES {
        anyhow::bail!("password too long for invite code (max {INVITE_MAX_PASSWORD_BYTES} bytes)");
    }

    let password_len = if include_password {
        password_bytes.len()
    } else {
        0
    };
    let mut payload = Vec::with_capacity(8 + password_len);
    payload.push(1);
    payload.extend_from_slice(&socket.ip().octets());
    payload.extend_from_slice(&socket.port().to_be_bytes());
    payload.push(password_len as u8);
    if password_len > 0 {
        payload.extend_from_slice(&password_bytes[..password_len]);
    }

    obfuscate_payload(&mut payload);
    let encoded = base32_encode_no_padding(&payload);
    let prefix = if password_len > 0 {
        INVITE_PREFIX_WITH_PASSWORD
    } else {
        INVITE_PREFIX_NO_PASSWORD
    };
    Ok(format!("{prefix}{encoded}"))
}

pub fn decode_invite_code(code: &str) -> anyhow::Result<DecodedInvite> {
    let trimmed = code.trim().to_ascii_uppercase();
    let with_password = if let Some(rest) = trimmed.strip_prefix(INVITE_PREFIX_WITH_PASSWORD) {
        (true, rest)
    } else if let Some(rest) = trimmed.strip_prefix(INVITE_PREFIX_NO_PASSWORD) {
        (false, rest)
    } else {
        anyhow::bail!("invalid invite code prefix");
    };

    let mut bytes = base32_decode_no_padding(with_password.1)?;
    obfuscate_payload(&mut bytes);
    if bytes.len() < 8 {
        anyhow::bail!("invite payload is too short");
    }
    if bytes[0] != 1 {
        anyhow::bail!("unsupported invite code version");
    }

    let ip = Ipv4Addr::new(bytes[1], bytes[2], bytes[3], bytes[4]);
    let port = u16::from_be_bytes([bytes[5], bytes[6]]);
    let password_len = bytes[7] as usize;
    let expected_len = 8 + password_len;
    if bytes.len() != expected_len {
        anyhow::bail!("invite payload length mismatch");
    }
    let password = if password_len == 0 {
        None
    } else {
        Some(String::from_utf8(bytes[8..].to_vec()).context("invite password is not utf-8")?)
    };

    Ok(DecodedInvite {
        server_addr: format!("{ip}:{port}"),
        room_code: trimmed,
        password,
    })
}

fn parse_socket_addr_v4(value: &str) -> anyhow::Result<std::net::SocketAddrV4> {
    let addr: SocketAddr = value
        .trim()
        .parse()
        .with_context(|| format!("invalid socket address '{value}'"))?;
    match addr {
        SocketAddr::V4(v4) => Ok(v4),
        SocketAddr::V6(_) => anyhow::bail!("IPv6 is not supported for invite codes yet"),
    }
}

fn detect_local_ipv4() -> Option<Ipv4Addr> {
    let socket = UdpSocket::bind("0.0.0.0:0").ok()?;
    socket.connect("8.8.8.8:80").ok()?;
    let addr = socket.local_addr().ok()?;
    match addr.ip() {
        IpAddr::V4(ip) => Some(ip),
        IpAddr::V6(_) => None,
    }
}

fn obfuscate_payload(payload: &mut [u8]) {
    for (index, byte) in payload.iter_mut().enumerate() {
        let key = INVITE_OBFUSCATION_KEY[index % INVITE_OBFUSCATION_KEY.len()];
        let mix = (index as u8).wrapping_mul(31).wrapping_add(17);
        *byte ^= key ^ mix;
    }
}

fn base32_encode_no_padding(data: &[u8]) -> String {
    let mut out = String::new();
    let mut buffer: u32 = 0;
    let mut bits: usize = 0;
    for byte in data {
        buffer = (buffer << 8) | u32::from(*byte);
        bits += 8;
        while bits >= 5 {
            bits -= 5;
            let index = ((buffer >> bits) & 0b1_1111) as usize;
            out.push(char::from(INVITE_ALPHABET[index]));
        }
    }
    if bits > 0 {
        let index = ((buffer << (5 - bits)) & 0b1_1111) as usize;
        out.push(char::from(INVITE_ALPHABET[index]));
    }
    out
}

fn base32_decode_no_padding(value: &str) -> anyhow::Result<Vec<u8>> {
    let mut out = Vec::new();
    let mut buffer: u32 = 0;
    let mut bits: usize = 0;
    for ch in value.bytes() {
        let index = INVITE_ALPHABET
            .iter()
            .position(|item| *item == ch)
            .with_context(|| format!("invalid invite character '{}'", char::from(ch)))?
            as u32;
        buffer = (buffer << 5) | index;
        bits += 5;
        if bits >= 8 {
            bits -= 8;
            out.push(((buffer >> bits) & 0xFF) as u8);
        }
    }
    Ok(out)
}

fn client_loop(
    mut stream: TcpStream,
    cmd_rx: Receiver<NetworkCommand>,
    event_tx: Sender<NetworkEvent>,
) {
    let mut read_stream = match stream.try_clone() {
        Ok(s) => s,
        Err(err) => {
            let _ = event_tx.send(NetworkEvent::Status(format!(
                "Online read clone failed: {err}"
            )));
            return;
        }
    };

    let read_event_tx = event_tx.clone();
    thread::spawn(move || {
        let mut reader = BufReader::new(&mut read_stream);
        let mut line = String::new();
        loop {
            line.clear();
            match reader.read_line(&mut line) {
                Ok(0) => {
                    let _ = read_event_tx.send(NetworkEvent::Status(String::from(
                        "Disconnected from online host",
                    )));
                    break;
                }
                Ok(_) => {
                    let parsed = serde_json::from_str::<WireServerMessage>(line.trim_end());
                    match parsed {
                        Ok(WireServerMessage::Session(session)) => {
                            let _ = read_event_tx.send(NetworkEvent::SessionSync(session));
                        }
                        Ok(WireServerMessage::Status(message)) => {
                            let _ = read_event_tx.send(NetworkEvent::Status(message));
                        }
                        Ok(WireServerMessage::HelloAck { .. }) => {}
                        Err(err) => {
                            let _ = read_event_tx.send(NetworkEvent::Status(format!(
                                "Online message parse error: {err}"
                            )));
                        }
                    }
                }
                Err(err) => {
                    let _ = read_event_tx.send(NetworkEvent::Status(format!(
                        "Online socket read error: {err}"
                    )));
                    break;
                }
            }
        }
    });

    loop {
        match cmd_rx.recv() {
            Ok(NetworkCommand::Shutdown) => break,
            Ok(NetworkCommand::LocalAction(action)) => {
                let msg = WireClientMessage::Action(action_to_wire(action));
                if let Err(err) = send_json_line(&mut stream, &msg) {
                    let _ =
                        event_tx.send(NetworkEvent::Status(format!("Online send failed: {err}")));
                    break;
                }
            }
            Err(_) => break,
        }
    }
}

fn host_loop(
    listener: TcpListener,
    session: &mut OnlineSession,
    expected_password: Option<String>,
    cmd_rx: Receiver<NetworkCommand>,
    event_tx: Sender<NetworkEvent>,
) {
    let (inbound_tx, inbound_rx) = mpsc::channel::<Inbound>();
    let mut peers: HashMap<u32, PeerConnection> = HashMap::new();
    let mut next_peer_id: u32 = 1;

    let _ = event_tx.send(NetworkEvent::SessionSync(session.clone()));
    loop {
        loop {
            match listener.accept() {
                Ok((stream, _)) => {
                    let peer_id = next_peer_id;
                    next_peer_id = next_peer_id.saturating_add(1);
                    let inbound_tx_clone = inbound_tx.clone();
                    thread::spawn(move || host_peer_reader(peer_id, stream, inbound_tx_clone));
                }
                Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => break,
                Err(err) => {
                    let _ =
                        event_tx.send(NetworkEvent::Status(format!("Online accept failed: {err}")));
                    break;
                }
            }
        }

        loop {
            match inbound_rx.try_recv() {
                Ok(inbound) => handle_inbound(
                    inbound,
                    session,
                    expected_password.as_deref(),
                    &mut peers,
                    &event_tx,
                ),
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => break,
            }
        }

        loop {
            match cmd_rx.try_recv() {
                Ok(NetworkCommand::Shutdown) => {
                    broadcast(
                        &mut peers,
                        &WireServerMessage::Status(String::from("Host ended session")),
                    );
                    return;
                }
                Ok(NetworkCommand::LocalAction(action)) => {
                    let origin = session
                        .local_participant()
                        .map(|participant| participant.nickname.clone())
                        .unwrap_or_else(|| String::from("host"));
                    apply_action_to_session(session, action, &origin);
                    broadcast_state(&mut peers, session);
                    let _ = event_tx.send(NetworkEvent::SessionSync(session.clone()));
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => return,
            }
        }

        thread::sleep(Duration::from_millis(12));
    }
}

fn handle_inbound(
    inbound: Inbound,
    session: &mut OnlineSession,
    expected_password: Option<&str>,
    peers: &mut HashMap<u32, PeerConnection>,
    event_tx: &Sender<NetworkEvent>,
) {
    match inbound {
        Inbound::Hello {
            peer_id,
            room_code,
            nickname,
            password,
            stream,
        } => {
            if room_code.to_ascii_uppercase() != session.room_code {
                let mut stream = stream;
                let _ = send_json_line(
                    &mut stream,
                    &WireServerMessage::HelloAck {
                        accepted: false,
                        reason: Some(String::from("room code mismatch")),
                        session: None,
                    },
                );
                return;
            }

            if peers.len().saturating_add(1) > MAX_PEERS {
                let mut stream = stream;
                let _ = send_json_line(
                    &mut stream,
                    &WireServerMessage::HelloAck {
                        accepted: false,
                        reason: Some(String::from("room is full")),
                        session: None,
                    },
                );
                return;
            }

            if expected_password.map(str::trim).unwrap_or("")
                != password.as_deref().map(str::trim).unwrap_or("")
            {
                let mut stream = stream;
                let _ = send_json_line(
                    &mut stream,
                    &WireServerMessage::HelloAck {
                        accepted: false,
                        reason: Some(String::from("invalid room password")),
                        session: None,
                    },
                );
                return;
            }

            let mut writer = stream;
            if send_json_line(
                &mut writer,
                &WireServerMessage::HelloAck {
                    accepted: true,
                    reason: None,
                    session: Some(session.clone()),
                },
            )
            .is_err()
            {
                return;
            }

            session.participants.push(crate::online::Participant {
                nickname: nickname.clone(),
                is_local: false,
                is_host: false,
                ping_ms: 35,
                manual_extra_delay_ms: 0,
                auto_ping_delay: true,
            });

            peers.insert(peer_id, PeerConnection { nickname, writer });
            broadcast_state(peers, session);
            let _ = event_tx.send(NetworkEvent::SessionSync(session.clone()));
        }
        Inbound::Action { peer_id, action } => {
            let origin = peers
                .get(&peer_id)
                .map(|peer| peer.nickname.clone())
                .unwrap_or_else(|| String::from("peer"));
            apply_action_to_session(session, wire_to_action(action), &origin);
            broadcast_state(peers, session);
            let _ = event_tx.send(NetworkEvent::SessionSync(session.clone()));
        }
        Inbound::Disconnected { peer_id } => {
            if let Some(peer) = peers.remove(&peer_id) {
                session
                    .participants
                    .retain(|participant| participant.nickname != peer.nickname);
                broadcast_state(peers, session);
                let _ = event_tx.send(NetworkEvent::SessionSync(session.clone()));
            }
        }
        Inbound::ReadError { peer_id, error } => {
            peers.remove(&peer_id);
            let _ = event_tx.send(NetworkEvent::Status(format!("peer read error: {error}")));
        }
    }
}

fn apply_action_to_session(
    session: &mut OnlineSession,
    action: LocalAction,
    origin_nickname: &str,
) {
    match action {
        LocalAction::SetMode(mode) => session.mode = mode,
        LocalAction::SetQuality(quality) => session.quality = quality,
        LocalAction::QueueAdd(item) => {
            session.shared_queue.push(SharedQueueItem {
                path: item.path,
                title: item.title,
                delivery: item.delivery,
            });
            if session.shared_queue.len() > 512 {
                let remove = session.shared_queue.len() - 512;
                session.shared_queue.drain(0..remove);
            }
        }
        LocalAction::DelayUpdate {
            manual_extra_delay_ms,
            auto_ping_delay,
        } => {
            let index = session
                .participants
                .iter()
                .find(|participant| participant.nickname == origin_nickname)
                .and_then(|participant| {
                    session
                        .participants
                        .iter()
                        .position(|entry| entry.nickname == participant.nickname)
                })
                .or_else(|| {
                    session
                        .participants
                        .iter()
                        .position(|participant| participant.is_local)
                });
            if let Some(index) = index {
                let participant = &mut session.participants[index];
                participant.manual_extra_delay_ms = manual_extra_delay_ms;
                participant.auto_ping_delay = auto_ping_delay;
            }
        }
        LocalAction::Transport(mut envelope) => {
            let next_seq = session
                .last_transport
                .as_ref()
                .map(|entry| entry.seq.saturating_add(1))
                .unwrap_or(1);
            envelope.seq = next_seq;
            envelope.origin_nickname = origin_nickname.to_string();
            session.last_transport = Some(envelope);
        }
    }
}

fn broadcast_state(peers: &mut HashMap<u32, PeerConnection>, session: &OnlineSession) {
    broadcast(peers, &WireServerMessage::Session(session.clone()));
}

fn broadcast(peers: &mut HashMap<u32, PeerConnection>, message: &WireServerMessage) {
    let mut dead_ids = Vec::new();
    for (peer_id, peer) in peers.iter_mut() {
        if send_json_line(&mut peer.writer, message).is_err() {
            dead_ids.push(*peer_id);
        }
    }
    for peer_id in dead_ids {
        peers.remove(&peer_id);
    }
}

fn host_peer_reader(peer_id: u32, stream: TcpStream, inbound_tx: Sender<Inbound>) {
    let mut reader = BufReader::new(match stream.try_clone() {
        Ok(clone) => clone,
        Err(err) => {
            let _ = inbound_tx.send(Inbound::ReadError {
                peer_id,
                error: err.to_string(),
            });
            return;
        }
    });

    let mut first_line = String::new();
    match reader.read_line(&mut first_line) {
        Ok(0) => {
            let _ = inbound_tx.send(Inbound::Disconnected { peer_id });
            return;
        }
        Ok(_) => {}
        Err(err) => {
            let _ = inbound_tx.send(Inbound::ReadError {
                peer_id,
                error: err.to_string(),
            });
            return;
        }
    }

    let hello = serde_json::from_str::<WireClientMessage>(first_line.trim_end());
    let (room_code, nickname, password) = match hello {
        Ok(WireClientMessage::Hello {
            room_code,
            nickname,
            password,
        }) => (room_code, nickname, password),
        _ => {
            let _ = inbound_tx.send(Inbound::Disconnected { peer_id });
            return;
        }
    };

    let _ = inbound_tx.send(Inbound::Hello {
        peer_id,
        room_code,
        nickname,
        password,
        stream,
    });

    let mut line = String::new();
    loop {
        line.clear();
        match reader.read_line(&mut line) {
            Ok(0) => {
                let _ = inbound_tx.send(Inbound::Disconnected { peer_id });
                break;
            }
            Ok(_) => {
                let msg = serde_json::from_str::<WireClientMessage>(line.trim_end());
                match msg {
                    Ok(WireClientMessage::Action(action)) => {
                        let _ = inbound_tx.send(Inbound::Action { peer_id, action });
                    }
                    Ok(WireClientMessage::Hello { .. }) => {}
                    Err(err) => {
                        let _ = inbound_tx.send(Inbound::ReadError {
                            peer_id,
                            error: err.to_string(),
                        });
                    }
                }
            }
            Err(err) => {
                let _ = inbound_tx.send(Inbound::ReadError {
                    peer_id,
                    error: err.to_string(),
                });
                break;
            }
        }
    }
}

fn send_json_line<T: Serialize>(stream: &mut TcpStream, value: &T) -> anyhow::Result<()> {
    let mut bytes = serde_json::to_vec(value).context("serialize failed")?;
    bytes.push(b'\n');
    stream.write_all(&bytes).context("write failed")?;
    stream.flush().context("flush failed")?;
    Ok(())
}

#[derive(Debug)]
struct PeerConnection {
    nickname: String,
    writer: TcpStream,
}

#[derive(Debug)]
enum Inbound {
    Hello {
        peer_id: u32,
        room_code: String,
        nickname: String,
        password: Option<String>,
        stream: TcpStream,
    },
    Action {
        peer_id: u32,
        action: WireAction,
    },
    Disconnected {
        peer_id: u32,
    },
    ReadError {
        peer_id: u32,
        error: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
enum WireClientMessage {
    Hello {
        room_code: String,
        nickname: String,
        password: Option<String>,
    },
    Action(WireAction),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
enum WireServerMessage {
    HelloAck {
        accepted: bool,
        reason: Option<String>,
        session: Option<OnlineSession>,
    },
    Session(OnlineSession),
    Status(String),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
enum WireAction {
    SetMode(crate::online::OnlineRoomMode),
    SetQuality(StreamQuality),
    QueueAdd(SharedQueueItem),
    DelayUpdate {
        manual_extra_delay_ms: u16,
        auto_ping_delay: bool,
    },
    Transport(TransportEnvelope),
}

fn action_to_wire(action: LocalAction) -> WireAction {
    match action {
        LocalAction::SetMode(mode) => WireAction::SetMode(mode),
        LocalAction::SetQuality(quality) => WireAction::SetQuality(quality),
        LocalAction::QueueAdd(item) => WireAction::QueueAdd(item),
        LocalAction::DelayUpdate {
            manual_extra_delay_ms,
            auto_ping_delay,
        } => WireAction::DelayUpdate {
            manual_extra_delay_ms,
            auto_ping_delay,
        },
        LocalAction::Transport(envelope) => WireAction::Transport(envelope),
    }
}

fn wire_to_action(action: WireAction) -> LocalAction {
    match action {
        WireAction::SetMode(mode) => LocalAction::SetMode(mode),
        WireAction::SetQuality(quality) => LocalAction::SetQuality(quality),
        WireAction::QueueAdd(item) => LocalAction::QueueAdd(item),
        WireAction::DelayUpdate {
            manual_extra_delay_ms,
            auto_ping_delay,
        } => LocalAction::DelayUpdate {
            manual_extra_delay_ms,
            auto_ping_delay,
        },
        WireAction::Transport(envelope) => LocalAction::Transport(envelope),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invite_code_round_trips_without_password() {
        let code = build_invite_code("192.168.1.33:7878", None, false).expect("code build");
        let decoded = decode_invite_code(&code).expect("decode");
        assert_eq!(decoded.server_addr, "192.168.1.33:7878");
        assert_eq!(decoded.room_code, code);
        assert_eq!(decoded.password, None);
    }

    #[test]
    fn invite_code_round_trips_with_password() {
        let code = build_invite_code("10.0.0.8:9000", Some("party123"), true).expect("code build");
        let decoded = decode_invite_code(&code).expect("decode");
        assert_eq!(decoded.server_addr, "10.0.0.8:9000");
        assert_eq!(decoded.password.as_deref(), Some("party123"));
    }
}
