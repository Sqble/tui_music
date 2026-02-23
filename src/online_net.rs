use crate::online::{OnlineSession, SharedQueueItem, StreamQuality, TransportEnvelope};
use anyhow::Context;
use base64::Engine;
use rand::Rng;
use rodio::{Decoder, Source};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::fs::{self, File};
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom, Write};
use std::net::{
    IpAddr, Ipv4Addr, Shutdown as NetShutdown, SocketAddr, TcpListener, TcpStream, ToSocketAddrs,
    UdpSocket,
};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use unsafe_libopus::{
    OPUS_APPLICATION_AUDIO, OPUS_OK, OPUS_SET_BITRATE_REQUEST, OPUS_SET_VBR_REQUEST,
    OpusDecoder as RawOpusDecoder, OpusEncoder as RawOpusEncoder, opus_decode, opus_decoder_create,
    opus_decoder_destroy, opus_encode, opus_encoder_create, opus_encoder_destroy,
};

const MAX_PEERS: usize = 8;
const INVITE_PREFIX_SECURE: &str = "T2";
const INVITE_MAX_PASSWORD_BYTES: usize = 32;
const INVITE_ALPHABET: &[u8] = b"ABCDEFGHJKLMNPQRSTUVWXYZ23456789";
const INVITE_SALT_BYTES: usize = 12;
const INVITE_CIPHER_BYTES: usize = 6;
const INVITE_TAG_BYTES: usize = 8;
const STUN_MAGIC_COOKIE: u32 = 0x2112_A442;
const STUN_BINDING_REQUEST: u16 = 0x0001;
const STUN_BINDING_SUCCESS_RESPONSE: u16 = 0x0101;
const STUN_ATTR_XOR_MAPPED_ADDRESS: u16 = 0x0020;
const STUN_ATTR_MAPPED_ADDRESS: u16 = 0x0001;
const STREAM_CHUNK_BYTES: usize = 24 * 1024;
const MAX_STREAM_FILE_BYTES: u64 = 1_073_741_824;
const BALANCED_STREAM_SAMPLE_RATE: u32 = 48_000;
const BALANCED_STREAM_CHANNELS: u16 = 2;
const BALANCED_STREAM_BITS_PER_SAMPLE: u16 = 16;
const BALANCED_OPUS_FRAME_MS: u32 = 20;
const BALANCED_OPUS_BITRATE_BPS: i32 = 160_000;
const BALANCED_OPUS_MAX_PACKET_BYTES: usize = 4_000;
const BALANCED_PAYLOAD_MAGIC: &[u8; 5] = b"TTOP1";
const BALANCED_FALLBACK_READY_PCM_BYTES: u64 = 192_000;
const PING_INTERVAL: Duration = Duration::from_millis(1_500);
const PING_TIMEOUT: Duration = Duration::from_millis(5_000);
const HOME_ROOM_EMPTY_GRACE_PERIOD: Duration = Duration::from_secs(3);
const HOME_ROOM_MAX_CONNECTIONS_MIN: u16 = 2;
const HOME_ROOM_MAX_CONNECTIONS_MAX: u16 = 32;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct HomeRoomDirectoryEntry {
    pub room_name: String,
    pub room_code: String,
    pub locked: bool,
    pub current_connections: u16,
    pub max_connections: u16,
}

#[derive(Debug, Clone)]
pub struct HomeRoomResolved {
    pub room_name: String,
    pub room_code: String,
    pub room_server_addr: String,
    pub locked: bool,
    pub current_connections: u16,
    pub max_connections: u16,
}

pub struct HomeServerHandle {
    shutdown_tx: Sender<()>,
    join_handle: Option<thread::JoinHandle<()>>,
}

impl HomeServerHandle {
    pub fn shutdown(mut self) {
        let _ = self.shutdown_tx.send(());
        if let Some(handle) = self.join_handle.take() {
            let _ = handle.join();
        }
    }
}

impl Drop for HomeServerHandle {
    fn drop(&mut self) {
        let _ = self.shutdown_tx.send(());
        if let Some(handle) = self.join_handle.take() {
            let _ = handle.join();
        }
    }
}

pub struct DecodedInvite {
    pub server_addr: String,
    pub room_code: String,
}

#[derive(Debug, Clone, Copy)]
pub enum NetworkRole {
    Host,
    Client,
}

#[derive(Debug)]
pub enum NetworkEvent {
    SessionSync(OnlineSession),
    StreamTrackReady {
        requested_path: PathBuf,
        local_temp_path: PathBuf,
        format: StreamTrackFormat,
    },
    Status(String),
}

#[derive(Debug, Clone, Copy)]
pub enum StreamTrackFormat {
    LosslessOriginal,
    BalancedOpus160kVbrStereo,
}

#[derive(Debug, Clone)]
pub enum LocalAction {
    SetMode(crate::online::OnlineRoomMode),
    SetQuality(StreamQuality),
    SetNickname {
        nickname: String,
    },
    QueueAdd(SharedQueueItem),
    QueueConsume {
        expected_path: Option<PathBuf>,
    },
    DelayUpdate {
        manual_extra_delay_ms: u16,
        auto_ping_delay: bool,
    },
    Transport(TransportEnvelope),
}

#[derive(Debug)]
enum NetworkCommand {
    LocalAction(LocalAction),
    RequestTrackStream {
        path: PathBuf,
        source_nickname: Option<String>,
    },
    Shutdown,
}

pub struct OnlineNetwork {
    role: NetworkRole,
    bind_addr: Option<String>,
    cmd_tx: Sender<NetworkCommand>,
    event_rx: Receiver<NetworkEvent>,
}

impl OnlineNetwork {
    pub fn role(&self) -> &NetworkRole {
        &self.role
    }

    pub fn bind_addr(&self) -> Option<&str> {
        self.bind_addr.as_deref()
    }

    pub fn start_host(
        bind_addr: &str,
        session: OnlineSession,
        expected_password: Option<String>,
    ) -> anyhow::Result<Self> {
        Self::start_host_with_max(bind_addr, session, expected_password, MAX_PEERS)
    }

    pub fn start_host_with_max(
        bind_addr: &str,
        mut session: OnlineSession,
        expected_password: Option<String>,
        max_peers: usize,
    ) -> anyhow::Result<Self> {
        let listener = TcpListener::bind(bind_addr)
            .with_context(|| format!("failed to bind online host at {bind_addr}"))?;
        let bound_addr = listener
            .local_addr()
            .context("failed to read host listener addr")?;
        listener
            .set_nonblocking(true)
            .context("failed to set nonblocking listener")?;

        let (cmd_tx, cmd_rx) = mpsc::channel();
        let (event_tx, event_rx) = mpsc::channel();

        thread::spawn(move || {
            host_loop(
                listener,
                &mut session,
                expected_password,
                max_peers,
                cmd_rx,
                event_tx,
            )
        });

        Ok(Self {
            role: NetworkRole::Host,
            bind_addr: Some(bound_addr.to_string()),
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

        let mut reader = BufReader::new(
            stream
                .try_clone()
                .context("failed to clone client stream")?,
        );
        let mut line = String::new();
        let read = reader
            .read_line(&mut line)
            .context("failed to read hello ack")?;
        if read == 0 {
            anyhow::bail!("server closed connection during handshake");
        }

        let ack: WireServerMessage =
            serde_json::from_str(line.trim_end()).context("failed to parse hello ack")?;
        let initial_session = match ack {
            WireServerMessage::HelloAck {
                accepted: true,
                reason: _,
                session,
            } => session,
            WireServerMessage::HelloAck {
                accepted: false,
                reason,
                session: _,
            } => {
                anyhow::bail!(reason.unwrap_or_else(|| String::from("server rejected connection")))
            }
            _ => anyhow::bail!("invalid handshake response from server"),
        };

        let (cmd_tx, cmd_rx) = mpsc::channel();
        let (event_tx, event_rx) = mpsc::channel();
        if let Some(session) = initial_session {
            let _ = event_tx.send(NetworkEvent::SessionSync(session));
        }
        let local_nickname = nickname.to_string();
        thread::spawn(move || client_loop(stream, reader, local_nickname, cmd_rx, event_tx));

        Ok(Self {
            role: NetworkRole::Client,
            bind_addr: None,
            cmd_tx,
            event_rx,
        })
    }

    pub fn send_local_action(&self, action: LocalAction) {
        let _ = self.cmd_tx.send(NetworkCommand::LocalAction(action));
    }

    pub fn request_track_stream(&self, path: PathBuf, source_nickname: Option<String>) {
        let _ = self.cmd_tx.send(NetworkCommand::RequestTrackStream {
            path,
            source_nickname,
        });
    }

    pub fn try_recv_event(&self) -> Option<NetworkEvent> {
        self.event_rx.try_recv().ok()
    }

    pub fn shutdown(&self) {
        let _ = self.cmd_tx.send(NetworkCommand::Shutdown);
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
enum HomeRequest {
    Verify,
    ListRooms {
        query: Option<String>,
    },
    CreateRoom {
        room_name: String,
        owner_nickname: String,
        password: Option<String>,
        max_connections: u16,
    },
    ResolveRoom {
        room_name: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
enum HomeResponse {
    Ok,
    Rooms { rooms: Vec<HomeRoomDirectoryEntry> },
    RoomResolved { room: HomeRoomResolvedWire },
    Error { message: String },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct HomeRoomResolvedWire {
    room_name: String,
    room_code: String,
    room_server_addr: String,
    locked: bool,
    current_connections: u16,
    max_connections: u16,
}

struct HostedRoom {
    room_name: String,
    room_code: String,
    room_server_port: u16,
    network: OnlineNetwork,
    max_connections: u16,
    locked: bool,
    current_connections: u16,
    empty_since: Option<Instant>,
}

pub fn start_home_server(
    bind_addr: &str,
    room_port_range: Option<(u16, u16)>,
) -> anyhow::Result<HomeServerHandle> {
    let listener = TcpListener::bind(bind_addr)
        .with_context(|| format!("failed to bind home server at {bind_addr}"))?;
    listener
        .set_nonblocking(true)
        .context("failed to set nonblocking home listener")?;
    let (shutdown_tx, shutdown_rx) = mpsc::channel::<()>();
    let bind = listener
        .local_addr()
        .context("failed to get local home addr")?;
    let bind_addr_for_closure = bind_addr.to_string();
    let join_handle = thread::spawn(move || {
        let mut rooms: HashMap<String, HostedRoom> = HashMap::new();
        loop {
            if shutdown_rx.try_recv().is_ok() {
                break;
            }

            for room in rooms.values_mut() {
                while let Some(event) = room.network.try_recv_event() {
                    if let NetworkEvent::SessionSync(session) = event {
                        room.current_connections = session.participants.len() as u16;
                    }
                }
            }

            let mut rooms_to_close = Vec::new();
            let now = Instant::now();
            for (key, room) in &mut rooms {
                if room.current_connections == 0 {
                    if let Some(since) = room.empty_since {
                        if now.duration_since(since) >= HOME_ROOM_EMPTY_GRACE_PERIOD {
                            rooms_to_close.push(key.clone());
                        }
                    } else {
                        room.empty_since = Some(now);
                    }
                } else {
                    room.empty_since = None;
                }
            }
            for key in rooms_to_close {
                if let Some(room) = rooms.remove(&key) {
                    room.network.shutdown();
                }
            }

            match listener.accept() {
                Ok((mut stream, _)) => {
                    let _ = stream.set_nonblocking(false);
                    let mut reader = BufReader::new(match stream.try_clone() {
                        Ok(clone) => clone,
                        Err(_) => continue,
                    });
                    let mut line = String::new();
                    let read = reader.read_line(&mut line).unwrap_or_default();
                    if read == 0 {
                        continue;
                    }
                    let request = serde_json::from_str::<HomeRequest>(line.trim_end());
                    let response = match request {
                        Ok(HomeRequest::Verify) => HomeResponse::Ok,
                        Ok(HomeRequest::ListRooms { query }) => {
                            let query = query.unwrap_or_default().to_ascii_lowercase();
                            let mut items: Vec<HomeRoomDirectoryEntry> = rooms
                                .values()
                                .filter(|room| {
                                    query.is_empty()
                                        || room.room_name.to_ascii_lowercase().contains(&query)
                                })
                                .map(|room| HomeRoomDirectoryEntry {
                                    room_name: room.room_name.clone(),
                                    room_code: room.room_code.clone(),
                                    locked: room.locked,
                                    current_connections: room.current_connections,
                                    max_connections: room.max_connections,
                                })
                                .collect();
                            items.sort_by(|a, b| a.room_name.cmp(&b.room_name));
                            HomeResponse::Rooms { rooms: items }
                        }
                        Ok(HomeRequest::ResolveRoom { room_name }) => {
                            match room_by_name(&rooms, &room_name) {
                                Some(room) => HomeResponse::RoomResolved {
                                    room: home_room_resolved_wire(
                                        room,
                                        &stream,
                                        &bind_addr_for_closure,
                                    ),
                                },
                                None => HomeResponse::Error {
                                    message: String::from("room not found"),
                                },
                            }
                        }
                        Ok(HomeRequest::CreateRoom {
                            room_name,
                            owner_nickname,
                            password,
                            max_connections,
                        }) => {
                            let name = room_name.trim();
                            if name.is_empty() {
                                HomeResponse::Error {
                                    message: String::from("room name is required"),
                                }
                            } else if !(HOME_ROOM_MAX_CONNECTIONS_MIN
                                ..=HOME_ROOM_MAX_CONNECTIONS_MAX)
                                .contains(&max_connections)
                            {
                                HomeResponse::Error {
                                    message: format!(
                                        "max connections must be {}..={} ",
                                        HOME_ROOM_MAX_CONNECTIONS_MIN,
                                        HOME_ROOM_MAX_CONNECTIONS_MAX
                                    ),
                                }
                            } else if room_by_name(&rooms, name).is_some() {
                                HomeResponse::Error {
                                    message: String::from("room already exists"),
                                }
                            } else {
                                let mut session = OnlineSession::host(&owner_nickname);
                                session.room_code = name.to_ascii_uppercase();
                                session.participants.clear();
                                match start_room_host_for_home_server(
                                    bind,
                                    room_port_range,
                                    session,
                                    password
                                        .as_deref()
                                        .map(str::trim)
                                        .filter(|value| !value.is_empty())
                                        .map(str::to_string),
                                    usize::from(max_connections),
                                ) {
                                    Ok(network) => {
                                        let room_port = network
                                            .bind_addr()
                                            .and_then(|addr| addr.parse::<SocketAddr>().ok())
                                            .map(|addr| addr.port())
                                            .unwrap_or(bind.port());
                                        rooms.insert(
                                            name.to_ascii_lowercase(),
                                            HostedRoom {
                                                room_name: name.to_string(),
                                                room_code: name.to_ascii_uppercase(),
                                                room_server_port: room_port,
                                                network,
                                                max_connections,
                                                locked: password
                                                    .as_deref()
                                                    .is_some_and(|value| !value.trim().is_empty()),
                                                current_connections: 0,
                                                empty_since: None,
                                            },
                                        );
                                        match room_by_name(&rooms, name) {
                                            Some(room) => HomeResponse::RoomResolved {
                                                room: home_room_resolved_wire(
                                                    room,
                                                    &stream,
                                                    &bind_addr_for_closure,
                                                ),
                                            },
                                            None => HomeResponse::Error {
                                                message: String::from(
                                                    "room created but could not be loaded",
                                                ),
                                            },
                                        }
                                    }
                                    Err(err) => HomeResponse::Error {
                                        message: format!("failed to create room host: {err}"),
                                    },
                                }
                            }
                        }
                        Err(err) => HomeResponse::Error {
                            message: format!("invalid request: {err}"),
                        },
                    };
                    let _ = send_json_line(&mut stream, &response);
                }
                Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                    thread::sleep(Duration::from_millis(16));
                }
                Err(_) => {
                    thread::sleep(Duration::from_millis(30));
                }
            }
        }
        for (_, room) in rooms {
            room.network.shutdown();
        }
    });

    Ok(HomeServerHandle {
        shutdown_tx,
        join_handle: Some(join_handle),
    })
}

pub fn run_home_server_forever(bind_addr: &str) -> anyhow::Result<()> {
    run_home_server_forever_with_ports(bind_addr, None)
}

pub fn run_home_server_forever_with_ports(
    bind_addr: &str,
    room_port_range: Option<(u16, u16)>,
) -> anyhow::Result<()> {
    let _handle = start_home_server(bind_addr, room_port_range)?;
    loop {
        thread::sleep(Duration::from_millis(1000));
    }
}

pub fn verify_home_server(server_addr: &str) -> anyhow::Result<()> {
    match send_home_request(server_addr, &HomeRequest::Verify)? {
        HomeResponse::Ok => Ok(()),
        HomeResponse::Error { message } => anyhow::bail!(message),
        _ => anyhow::bail!("unexpected response from home server"),
    }
}

pub fn list_home_rooms(
    server_addr: &str,
    query: Option<&str>,
) -> anyhow::Result<Vec<HomeRoomDirectoryEntry>> {
    match send_home_request(
        server_addr,
        &HomeRequest::ListRooms {
            query: query.map(str::to_string),
        },
    )? {
        HomeResponse::Rooms { rooms } => Ok(rooms),
        HomeResponse::Error { message } => anyhow::bail!(message),
        _ => anyhow::bail!("unexpected response from home server"),
    }
}

pub fn create_home_room(
    server_addr: &str,
    room_name: &str,
    owner_nickname: &str,
    password: Option<&str>,
    max_connections: u16,
) -> anyhow::Result<HomeRoomResolved> {
    resolve_from_response(send_home_request(
        server_addr,
        &HomeRequest::CreateRoom {
            room_name: room_name.trim().to_string(),
            owner_nickname: owner_nickname.trim().to_string(),
            password: password
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_string),
            max_connections,
        },
    )?)
}

pub fn resolve_home_room(server_addr: &str, room_name: &str) -> anyhow::Result<HomeRoomResolved> {
    resolve_from_response(send_home_request(
        server_addr,
        &HomeRequest::ResolveRoom {
            room_name: room_name.trim().to_string(),
        },
    )?)
}

fn resolve_from_response(response: HomeResponse) -> anyhow::Result<HomeRoomResolved> {
    match response {
        HomeResponse::RoomResolved { room } => Ok(HomeRoomResolved {
            room_name: room.room_name,
            room_code: room.room_code,
            room_server_addr: room.room_server_addr,
            locked: room.locked,
            current_connections: room.current_connections,
            max_connections: room.max_connections,
        }),
        HomeResponse::Error { message } => anyhow::bail!(message),
        _ => anyhow::bail!("unexpected response from home server"),
    }
}

const HOME_CONNECT_TIMEOUT_MS: u64 = 3_000;
const HOME_READ_TIMEOUT_MS: u64 = 5_000;

fn resolve_server_addrs(server_addr: &str) -> anyhow::Result<Vec<SocketAddr>> {
    if let Ok(addr) = server_addr.parse() {
        return Ok(vec![addr]);
    }
    let (host, port) = server_addr
        .rsplit_once(':')
        .ok_or_else(|| anyhow::anyhow!("invalid address format: {server_addr}"))?;
    let port: u16 = port
        .parse()
        .with_context(|| format!("invalid port in address: {server_addr}"))?;
    let addrs: Vec<SocketAddr> = (host, port)
        .to_socket_addrs()
        .with_context(|| format!("failed to resolve host: {host}"))?
        .collect();
    if addrs.is_empty() {
        anyhow::bail!("no addresses found for host: {host}");
    }
    Ok(addrs)
}

fn send_home_request(server_addr: &str, request: &HomeRequest) -> anyhow::Result<HomeResponse> {
    let addrs = resolve_server_addrs(server_addr)?;
    let mut last_err: Option<anyhow::Error> = None;
    for addr in &addrs {
        match try_home_connect(*addr, request) {
            Ok(response) => return Ok(response),
            Err(err) => last_err = Some(err),
        }
    }
    Err(last_err
        .unwrap_or_else(|| anyhow::anyhow!("failed to connect to home server {server_addr}")))
}

fn try_home_connect(addr: SocketAddr, request: &HomeRequest) -> anyhow::Result<HomeResponse> {
    let mut stream =
        TcpStream::connect_timeout(&addr, Duration::from_millis(HOME_CONNECT_TIMEOUT_MS))
            .with_context(|| format!("failed to connect to {addr}"))?;
    stream
        .set_read_timeout(Some(Duration::from_millis(HOME_READ_TIMEOUT_MS)))
        .context("failed to set read timeout")?;
    send_json_line(&mut stream, request).context("failed to send home request")?;
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    let read = reader
        .read_line(&mut line)
        .context("failed to read home response")?;
    if read == 0 {
        anyhow::bail!("home server closed connection");
    }
    serde_json::from_str::<HomeResponse>(line.trim_end()).context("failed to parse home response")
}

fn start_room_host_for_home_server(
    home_bind_addr: SocketAddr,
    room_port_range: Option<(u16, u16)>,
    session: OnlineSession,
    password: Option<String>,
    max_connections: usize,
) -> anyhow::Result<OnlineNetwork> {
    if let Some((start_port, end_port)) = room_port_range {
        let mut last_err: Option<anyhow::Error> = None;
        for port in start_port..=end_port {
            let room_bind = SocketAddr::new(home_bind_addr.ip(), port).to_string();
            match OnlineNetwork::start_host_with_max(
                &room_bind,
                session.clone(),
                password.clone(),
                max_connections,
            ) {
                Ok(network) => return Ok(network),
                Err(err) => {
                    last_err = Some(err);
                }
            }
        }
        let detail = last_err
            .map(|err| err.to_string())
            .unwrap_or_else(|| String::from("no ports available"));
        anyhow::bail!(
            "no available room port in configured range {}-{} ({detail})",
            start_port,
            end_port
        );
    }

    let room_bind = SocketAddr::new(home_bind_addr.ip(), 0).to_string();
    OnlineNetwork::start_host_with_max(&room_bind, session, password, max_connections)
}

fn room_by_name<'a>(
    rooms: &'a HashMap<String, HostedRoom>,
    room_name: &str,
) -> Option<&'a HostedRoom> {
    rooms.get(&room_name.trim().to_ascii_lowercase())
}

fn home_room_resolved_wire(
    room: &HostedRoom,
    stream: &TcpStream,
    bind_addr: &str,
) -> HomeRoomResolvedWire {
    let local_ip = stream
        .local_addr()
        .map(|addr| addr.ip())
        .unwrap_or_else(|_| IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)));

    let use_local_addr = match local_ip {
        IpAddr::V4(v4) if v4.is_loopback() => true,
        IpAddr::V4(v4) if v4.is_unspecified() => false,
        IpAddr::V6(v6) if v6.is_loopback() => true,
        IpAddr::V6(v6) if v6.is_unspecified() => false,
        _ => false,
    };

    let final_addr = if use_local_addr {
        SocketAddr::new(local_ip, room.room_server_port).to_string()
    } else {
        let advertise_ip = resolve_advertise_addr(bind_addr)
            .ok()
            .and_then(|addr| addr.rsplit_once(':').map(|(ip, _)| ip.to_string()))
            .unwrap_or_else(|| {
                bind_addr
                    .rsplit_once(':')
                    .map(|(ip, _)| ip.to_string())
                    .unwrap_or_else(|| String::from("127.0.0.1"))
            });
        format!("{advertise_ip}:{}", room.room_server_port)
    };

    let (ip_str, port_str) = final_addr.rsplit_once(':').unwrap_or(("127.0.0.1", "0"));
    let ip: IpAddr = ip_str
        .parse()
        .unwrap_or(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)));
    let port: u16 = port_str.parse().unwrap_or(0);

    HomeRoomResolvedWire {
        room_name: room.room_name.clone(),
        room_code: room.room_code.clone(),
        room_server_addr: SocketAddr::new(ip, port).to_string(),
        locked: room.locked,
        current_connections: room.current_connections,
        max_connections: room.max_connections,
    }
}

pub fn resolve_advertise_addr(bind_addr: &str) -> anyhow::Result<String> {
    let bind = parse_socket_addr_v4(bind_addr)?;
    let port = bind.port();
    let bind_ip = *bind.ip();
    let ip = if bind_ip.is_unspecified() || !is_public_ipv4(bind_ip) {
        detect_public_ipv4_stun()
            .or_else(detect_local_ipv4)
            .unwrap_or(Ipv4Addr::new(127, 0, 0, 1))
    } else {
        bind_ip
    };
    Ok(format!("{ip}:{port}"))
}

pub fn build_invite_code(server_addr: &str, password: &str) -> anyhow::Result<String> {
    let socket = parse_socket_addr_v4(server_addr)?;
    let password_bytes = password.trim().as_bytes();
    if password_bytes.is_empty() {
        anyhow::bail!("password is required for secure invite code");
    }
    if password_bytes.len() > INVITE_MAX_PASSWORD_BYTES {
        anyhow::bail!("password too long for invite code (max {INVITE_MAX_PASSWORD_BYTES} bytes)");
    }

    let mut salt = [0_u8; INVITE_SALT_BYTES];
    rand::rng().fill(&mut salt);

    let mut clear = [0_u8; INVITE_CIPHER_BYTES];
    clear[..4].copy_from_slice(&socket.ip().octets());
    clear[4..].copy_from_slice(&socket.port().to_be_bytes());

    let (enc_key, mac_key) = derive_invite_keys(password.trim(), &salt);
    let keystream = invite_keystream(&enc_key, INVITE_CIPHER_BYTES);
    let mut cipher = [0_u8; INVITE_CIPHER_BYTES];
    for (idx, byte) in clear.iter().enumerate() {
        cipher[idx] = *byte ^ keystream[idx];
    }

    let tag_full = invite_mac(&mac_key, &salt, &cipher);
    let mut payload =
        Vec::with_capacity(1 + INVITE_SALT_BYTES + INVITE_CIPHER_BYTES + INVITE_TAG_BYTES);
    payload.push(2);
    payload.extend_from_slice(&salt);
    payload.extend_from_slice(&cipher);
    payload.extend_from_slice(&tag_full[..INVITE_TAG_BYTES]);

    let encoded = base32_encode_no_padding(&payload);
    Ok(format!("{INVITE_PREFIX_SECURE}{encoded}"))
}

pub fn decode_invite_code(code: &str, password: &str) -> anyhow::Result<DecodedInvite> {
    let trimmed = code.trim().to_ascii_uppercase();
    let Some(rest) = trimmed.strip_prefix(INVITE_PREFIX_SECURE) else {
        anyhow::bail!("invalid invite code prefix");
    };

    let password = password.trim();
    if password.is_empty() {
        anyhow::bail!("password is required");
    }

    let bytes = base32_decode_no_padding(rest)?;
    let expected_len = 1 + INVITE_SALT_BYTES + INVITE_CIPHER_BYTES + INVITE_TAG_BYTES;
    if bytes.len() != expected_len {
        anyhow::bail!("invite payload length mismatch");
    }
    if bytes[0] != 2 {
        anyhow::bail!("unsupported invite code version");
    }

    let mut salt = [0_u8; INVITE_SALT_BYTES];
    salt.copy_from_slice(&bytes[1..1 + INVITE_SALT_BYTES]);
    let mut cipher = [0_u8; INVITE_CIPHER_BYTES];
    let cipher_start = 1 + INVITE_SALT_BYTES;
    cipher.copy_from_slice(&bytes[cipher_start..cipher_start + INVITE_CIPHER_BYTES]);
    let tag_start = cipher_start + INVITE_CIPHER_BYTES;
    let tag = &bytes[tag_start..tag_start + INVITE_TAG_BYTES];

    let (enc_key, mac_key) = derive_invite_keys(password, &salt);
    let expected_tag = invite_mac(&mac_key, &salt, &cipher);
    if !constant_time_eq(tag, &expected_tag[..INVITE_TAG_BYTES]) {
        anyhow::bail!("invalid invite password or code checksum");
    }

    let keystream = invite_keystream(&enc_key, INVITE_CIPHER_BYTES);
    let mut clear = [0_u8; INVITE_CIPHER_BYTES];
    for (idx, byte) in cipher.iter().enumerate() {
        clear[idx] = *byte ^ keystream[idx];
    }

    let ip = Ipv4Addr::new(clear[0], clear[1], clear[2], clear[3]);
    let port = u16::from_be_bytes([clear[4], clear[5]]);

    Ok(DecodedInvite {
        server_addr: format!("{ip}:{port}"),
        room_code: trimmed,
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

fn detect_public_ipv4_stun() -> Option<Ipv4Addr> {
    let candidates = [
        "stun.l.google.com:19302",
        "stun1.l.google.com:19302",
        "stun2.l.google.com:19302",
    ];
    for candidate in candidates {
        if let Some(ip) = query_stun_public_ipv4(candidate) {
            return Some(ip);
        }
    }
    None
}

fn query_stun_public_ipv4(server: &str) -> Option<Ipv4Addr> {
    let socket = UdpSocket::bind("0.0.0.0:0").ok()?;
    socket
        .set_read_timeout(Some(Duration::from_millis(850)))
        .ok()?;
    socket
        .set_write_timeout(Some(Duration::from_millis(850)))
        .ok()?;
    socket.connect(server).ok()?;

    let mut txid = [0_u8; 12];
    rand::rng().fill(&mut txid);
    let mut request = [0_u8; 20];
    request[0..2].copy_from_slice(&STUN_BINDING_REQUEST.to_be_bytes());
    request[2..4].copy_from_slice(&0_u16.to_be_bytes());
    request[4..8].copy_from_slice(&STUN_MAGIC_COOKIE.to_be_bytes());
    request[8..20].copy_from_slice(&txid);

    socket.send(&request).ok()?;
    let mut response = [0_u8; 1024];
    let len = socket.recv(&mut response).ok()?;
    parse_stun_mapped_ipv4(&response[..len], &txid)
}

fn parse_stun_mapped_ipv4(packet: &[u8], txid: &[u8; 12]) -> Option<Ipv4Addr> {
    if packet.len() < 20 {
        return None;
    }
    let message_type = u16::from_be_bytes([packet[0], packet[1]]);
    if message_type != STUN_BINDING_SUCCESS_RESPONSE {
        return None;
    }
    let length = usize::from(u16::from_be_bytes([packet[2], packet[3]]));
    if packet.len() < 20 + length {
        return None;
    }
    let cookie = u32::from_be_bytes([packet[4], packet[5], packet[6], packet[7]]);
    if cookie != STUN_MAGIC_COOKIE {
        return None;
    }
    if packet[8..20] != txid[..] {
        return None;
    }

    let mut cursor = 20_usize;
    let end = 20 + length;
    while cursor + 4 <= end {
        let attr_type = u16::from_be_bytes([packet[cursor], packet[cursor + 1]]);
        let attr_len = usize::from(u16::from_be_bytes([packet[cursor + 2], packet[cursor + 3]]));
        cursor += 4;
        if cursor + attr_len > end {
            return None;
        }

        let value = &packet[cursor..cursor + attr_len];
        if attr_type == STUN_ATTR_XOR_MAPPED_ADDRESS {
            if let Some(ip) = parse_xor_mapped_ipv4(value, cookie) {
                return Some(ip);
            }
        } else if attr_type == STUN_ATTR_MAPPED_ADDRESS
            && let Some(ip) = parse_mapped_ipv4(value)
        {
            return Some(ip);
        }

        let padded = (attr_len + 3) & !3;
        cursor += padded;
    }
    None
}

fn parse_xor_mapped_ipv4(value: &[u8], cookie: u32) -> Option<Ipv4Addr> {
    if value.len() < 8 || value[1] != 0x01 {
        return None;
    }
    let cookie_bytes = cookie.to_be_bytes();
    Some(Ipv4Addr::new(
        value[4] ^ cookie_bytes[0],
        value[5] ^ cookie_bytes[1],
        value[6] ^ cookie_bytes[2],
        value[7] ^ cookie_bytes[3],
    ))
}

fn parse_mapped_ipv4(value: &[u8]) -> Option<Ipv4Addr> {
    if value.len() < 8 || value[1] != 0x01 {
        return None;
    }
    Some(Ipv4Addr::new(value[4], value[5], value[6], value[7]))
}

fn is_public_ipv4(ip: Ipv4Addr) -> bool {
    !ip.is_private()
        && !ip.is_loopback()
        && !ip.is_link_local()
        && !ip.is_broadcast()
        && !ip.is_documentation()
        && !ip.is_unspecified()
}

fn derive_invite_keys(password: &str, salt: &[u8; INVITE_SALT_BYTES]) -> ([u8; 32], [u8; 32]) {
    let mut enc = Sha256::new();
    enc.update(b"tunetui-invite-enc-v2");
    enc.update(password.as_bytes());
    enc.update(salt);
    let enc_key: [u8; 32] = enc.finalize().into();

    let mut mac = Sha256::new();
    mac.update(b"tunetui-invite-mac-v2");
    mac.update(password.as_bytes());
    mac.update(salt);
    let mac_key: [u8; 32] = mac.finalize().into();

    (enc_key, mac_key)
}

fn invite_keystream(enc_key: &[u8; 32], len: usize) -> Vec<u8> {
    let mut stream = Vec::with_capacity(len);
    let mut counter: u64 = 0;
    while stream.len() < len {
        let mut digest = Sha256::new();
        digest.update(b"tunetui-invite-stream-v2");
        digest.update(enc_key);
        digest.update(counter.to_be_bytes());
        let block = digest.finalize();
        let remaining = len - stream.len();
        let take = remaining.min(block.len());
        stream.extend_from_slice(&block[..take]);
        counter = counter.saturating_add(1);
    }
    stream
}

fn invite_mac(mac_key: &[u8; 32], salt: &[u8; INVITE_SALT_BYTES], cipher: &[u8]) -> [u8; 32] {
    let mut digest = Sha256::new();
    digest.update(b"tunetui-invite-tag-v2");
    digest.update(mac_key);
    digest.update(salt);
    digest.update(cipher);
    digest.finalize().into()
}

fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    let mut diff = 0_u8;
    for (lhs, rhs) in left.iter().zip(right.iter()) {
        diff |= lhs ^ rhs;
    }
    diff == 0
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
    stream: TcpStream,
    handshake_reader: BufReader<TcpStream>,
    local_nickname: String,
    cmd_rx: Receiver<NetworkCommand>,
    event_tx: Sender<NetworkEvent>,
) {
    let writer = Arc::new(Mutex::new(stream));
    let upload_guard = Arc::new(Mutex::new(ClientUploadGuard {
        local_nickname,
        allowed_paths: HashSet::new(),
    }));
    let stream_quality = Arc::new(Mutex::new(StreamQuality::Lossless));

    let read_event_tx = event_tx.clone();
    let read_writer = Arc::clone(&writer);
    let read_upload_guard = Arc::clone(&upload_guard);
    let read_stream_quality = Arc::clone(&stream_quality);
    thread::spawn(move || {
        let mut reader = handshake_reader;
        let mut line = String::new();
        let mut inbound_streams: HashMap<u64, InboundStreamDownload> = HashMap::new();
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
                            if let Ok(mut guard) = read_upload_guard.lock() {
                                let local_nickname = guard.local_nickname.clone();
                                let allowed_paths =
                                    allowed_upload_paths_for_client(&session, &local_nickname);
                                guard.allowed_paths = allowed_paths;
                            }
                            if let Ok(mut quality) = read_stream_quality.lock() {
                                *quality = session.quality;
                            }
                            let _ = read_event_tx.send(NetworkEvent::SessionSync(session));
                        }
                        Ok(WireServerMessage::Ping { nonce }) => {
                            let _ = send_json_line_shared(
                                &read_writer,
                                &WireClientMessage::Pong { nonce },
                            );
                        }
                        Ok(WireServerMessage::StreamRequest { path, request_id }) => {
                            let permitted = read_upload_guard
                                .lock()
                                .ok()
                                .is_some_and(|guard| guard.allowed_paths.contains(&path));
                            if !permitted {
                                let _ = send_json_line_shared(
                                    &read_writer,
                                    &WireClientMessage::StreamEnd {
                                        request_id,
                                        path,
                                        success: false,
                                        error: Some(String::from(
                                            "stream denied: path not owned by this client",
                                        )),
                                    },
                                );
                                continue;
                            }
                            let quality = read_stream_quality
                                .lock()
                                .map(|value| *value)
                                .unwrap_or(StreamQuality::Lossless);
                            let stream_writer = Arc::clone(&read_writer);
                            thread::spawn(move || {
                                if let Err(err) =
                                    stream_file_to_host(&stream_writer, &path, request_id, quality)
                                {
                                    let _ = send_json_line_shared(
                                        &stream_writer,
                                        &WireClientMessage::StreamEnd {
                                            request_id,
                                            path,
                                            success: false,
                                            error: Some(format!("stream failed: {err}")),
                                        },
                                    );
                                }
                            });
                        }
                        Ok(WireServerMessage::StreamStart {
                            request_id,
                            path,
                            total_bytes,
                            payload_format,
                        }) => {
                            match InboundStreamDownload::new(&path, total_bytes, payload_format) {
                                Ok(state) => {
                                    inbound_streams.insert(request_id, state);
                                }
                                Err(err) => {
                                    let _ = read_event_tx.send(NetworkEvent::Status(format!(
                                        "Stream start failed: {err}"
                                    )));
                                }
                            }
                        }
                        Ok(WireServerMessage::StreamChunk {
                            request_id,
                            data_base64,
                        }) => {
                            let Some(state) = inbound_streams.get_mut(&request_id) else {
                                continue;
                            };
                            let decoded = base64::engine::general_purpose::STANDARD
                                .decode(data_base64.as_bytes());
                            match decoded {
                                Ok(bytes) => {
                                    let ready_now = match state.payload_format {
                                        StreamPayloadFormat::OriginalFile => {
                                            if let Err(err) = state.file.write_all(&bytes) {
                                                let _ = read_event_tx.send(NetworkEvent::Status(
                                                    format!("Stream write failed: {err}"),
                                                ));
                                                inbound_streams.remove(&request_id);
                                                continue;
                                            }
                                            false
                                        }
                                        StreamPayloadFormat::BalancedOpus160kVbr => {
                                            match ingest_balanced_stream_bytes(state, &bytes) {
                                                Ok(ready) => ready,
                                                Err(err) => {
                                                    let _ =
                                                        read_event_tx.send(NetworkEvent::Status(
                                                            format!("Stream write failed: {err}"),
                                                        ));
                                                    inbound_streams.remove(&request_id);
                                                    continue;
                                                }
                                            }
                                        }
                                    };
                                    state.received_bytes =
                                        state.received_bytes.saturating_add(bytes.len() as u64);
                                    if ready_now {
                                        let _ =
                                            read_event_tx.send(NetworkEvent::StreamTrackReady {
                                                requested_path: state.requested_path.clone(),
                                                local_temp_path: state.local_temp_path.clone(),
                                                format: stream_track_format(state.payload_format),
                                            });
                                    }
                                }
                                Err(err) => {
                                    let _ = read_event_tx.send(NetworkEvent::Status(format!(
                                        "Stream decode failed: {err}"
                                    )));
                                    inbound_streams.remove(&request_id);
                                }
                            }
                        }
                        Ok(WireServerMessage::StreamEnd {
                            request_id,
                            path,
                            success,
                            error,
                        }) => {
                            let Some(mut state) = inbound_streams.remove(&request_id) else {
                                continue;
                            };
                            if state.requested_path != path {
                                let _ = read_event_tx.send(NetworkEvent::Status(String::from(
                                    "Stream end path mismatch",
                                )));
                                let _ = fs::remove_file(&state.local_temp_path);
                                continue;
                            }
                            if !success {
                                let _ = fs::remove_file(&state.local_temp_path);
                                let _ = read_event_tx.send(NetworkEvent::Status(
                                    error.unwrap_or_else(|| String::from("Host stream failed")),
                                ));
                                continue;
                            }
                            if !stream_size_matches(
                                state.total_bytes,
                                state.received_bytes,
                                state.payload_format,
                            ) {
                                let _ = fs::remove_file(&state.local_temp_path);
                                let _ = read_event_tx.send(NetworkEvent::Status(format!(
                                    "Stream size mismatch: expected {} bytes got {} bytes",
                                    state.total_bytes, state.received_bytes
                                )));
                                continue;
                            }
                            if let Err(err) = finalize_inbound_stream(&mut state) {
                                let _ = fs::remove_file(&state.local_temp_path);
                                let _ = read_event_tx.send(NetworkEvent::Status(format!(
                                    "Stream finalize failed: {err}"
                                )));
                                continue;
                            }
                            if !state.ready_emitted {
                                let _ = read_event_tx.send(NetworkEvent::StreamTrackReady {
                                    requested_path: state.requested_path,
                                    local_temp_path: state.local_temp_path,
                                    format: stream_track_format(state.payload_format),
                                });
                            }
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
            Ok(NetworkCommand::Shutdown) => {
                if let Ok(stream) = writer.lock() {
                    let _ = stream.shutdown(NetShutdown::Both);
                }
                break;
            }
            Ok(NetworkCommand::LocalAction(action)) => {
                let msg = WireClientMessage::Action(action_to_wire(action));
                if let Err(err) = send_json_line_shared(&writer, &msg) {
                    let _ =
                        event_tx.send(NetworkEvent::Status(format!("Online send failed: {err}")));
                    break;
                }
            }
            Ok(NetworkCommand::RequestTrackStream {
                path,
                source_nickname,
            }) => {
                let msg = WireClientMessage::StreamRequest {
                    path,
                    request_id: next_request_id(),
                    source_nickname,
                };
                if let Err(err) = send_json_line_shared(&writer, &msg) {
                    let _ =
                        event_tx.send(NetworkEvent::Status(format!("Online send failed: {err}")));
                    break;
                }
            }
            Err(_) => break,
        }
    }

    if let Ok(stream) = writer.lock() {
        let _ = stream.shutdown(NetShutdown::Both);
    }
}

fn host_loop(
    listener: TcpListener,
    session: &mut OnlineSession,
    expected_password: Option<String>,
    max_peers: usize,
    cmd_rx: Receiver<NetworkCommand>,
    event_tx: Sender<NetworkEvent>,
) {
    let (inbound_tx, inbound_rx) = mpsc::channel::<Inbound>();
    let mut peers: HashMap<u32, PeerConnection> = HashMap::new();
    let mut pending_pull_requests: HashMap<(u32, u64), PathBuf> = HashMap::new();
    let mut pending_relay_requests: HashMap<(u32, u64), RelayStreamRequest> = HashMap::new();
    let mut inbound_streams: HashMap<(u32, u64), InboundStreamDownload> = HashMap::new();
    let mut pending_pings: HashMap<u32, PendingPing> = HashMap::new();
    let mut last_ping_sweep_at = Instant::now();
    let mut next_peer_id: u32 = 1;

    let _ = event_tx.send(NetworkEvent::SessionSync(session.clone()));
    loop {
        loop {
            match listener.accept() {
                Ok((stream, _)) => {
                    if stream.set_nonblocking(false).is_err() {
                        let _ = event_tx.send(NetworkEvent::Status(String::from(
                            "Online stream setup failed",
                        )));
                        continue;
                    }
                    let _ = stream.set_nodelay(true);
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
                    max_peers,
                    InboundState {
                        peers: &mut peers,
                        pending_pull_requests: &mut pending_pull_requests,
                        pending_relay_requests: &mut pending_relay_requests,
                        inbound_streams: &mut inbound_streams,
                        pending_pings: &mut pending_pings,
                    },
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
                Ok(NetworkCommand::RequestTrackStream {
                    path,
                    source_nickname,
                }) => {
                    let Some(source_nickname) = source_nickname else {
                        let _ = event_tx.send(NetworkEvent::Status(String::from(
                            "Stream request missing source peer",
                        )));
                        continue;
                    };
                    let Some((peer_id, peer)) = peers
                        .iter()
                        .find(|(_, peer)| peer.nickname.eq_ignore_ascii_case(&source_nickname))
                    else {
                        let _ = event_tx.send(NetworkEvent::Status(format!(
                            "Source peer offline: {source_nickname}",
                        )));
                        continue;
                    };
                    let request_id = next_request_id();
                    pending_pull_requests.insert((*peer_id, request_id), path.clone());
                    if let Err(err) = send_json_line_shared(
                        &peer.writer,
                        &WireServerMessage::StreamRequest { path, request_id },
                    ) {
                        pending_pull_requests.remove(&(*peer_id, request_id));
                        let _ = event_tx.send(NetworkEvent::Status(format!(
                            "Peer stream request failed: {err}",
                        )));
                    }
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => return,
            }
        }

        if last_ping_sweep_at.elapsed() >= PING_INTERVAL {
            last_ping_sweep_at = Instant::now();
            let mut timed_out_peers = Vec::new();
            pending_pings.retain(|peer_id, pending| {
                if !peers.contains_key(peer_id) {
                    return false;
                }
                if pending.sent_at.elapsed() > PING_TIMEOUT {
                    timed_out_peers.push(*peer_id);
                    return false;
                }
                true
            });
            for peer_id in timed_out_peers {
                let reason = format!("Peer timed out: {}", peer_display_name(&peers, peer_id));
                disconnect_peer(
                    peer_id,
                    session,
                    &mut InboundState {
                        peers: &mut peers,
                        pending_pull_requests: &mut pending_pull_requests,
                        pending_relay_requests: &mut pending_relay_requests,
                        inbound_streams: &mut inbound_streams,
                        pending_pings: &mut pending_pings,
                    },
                    &reason,
                    &event_tx,
                );
            }
            for (peer_id, peer) in &peers {
                if pending_pings.contains_key(peer_id) {
                    continue;
                }
                let nonce = rand::rng().random::<u64>();
                if send_json_line_shared(&peer.writer, &WireServerMessage::Ping { nonce }).is_ok() {
                    pending_pings.insert(
                        *peer_id,
                        PendingPing {
                            nonce,
                            sent_at: Instant::now(),
                        },
                    );
                }
            }
        }

        thread::sleep(Duration::from_millis(12));
    }
}

fn handle_inbound(
    inbound: Inbound,
    session: &mut OnlineSession,
    expected_password: Option<&str>,
    max_peers: usize,
    state: InboundState<'_>,
    event_tx: &Sender<NetworkEvent>,
) {
    let InboundState {
        peers,
        pending_pull_requests,
        pending_relay_requests,
        inbound_streams,
        pending_pings,
    } = state;
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

            if peers.len().saturating_add(1) > max_peers {
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

            if peers
                .values()
                .any(|peer| peer.nickname.eq_ignore_ascii_case(&nickname))
            {
                let mut stream = stream;
                let _ = send_json_line(
                    &mut stream,
                    &WireServerMessage::HelloAck {
                        accepted: false,
                        reason: Some(String::from("nickname already in use")),
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

            let has_host = session
                .participants
                .iter()
                .any(|participant| participant.is_host);
            if let Some(existing) = session
                .participants
                .iter_mut()
                .find(|participant| participant.nickname.eq_ignore_ascii_case(&nickname))
            {
                if !has_host {
                    existing.is_host = true;
                }
                existing.is_local = false;
                existing.ping_ms = 35;
                existing.manual_extra_delay_ms = 0;
                existing.auto_ping_delay = true;
            } else {
                let should_be_host = !has_host;
                session.participants.push(crate::online::Participant {
                    nickname: nickname.clone(),
                    is_local: false,
                    is_host: should_be_host,
                    ping_ms: 35,
                    manual_extra_delay_ms: 0,
                    auto_ping_delay: true,
                });
            }

            peers.insert(
                peer_id,
                PeerConnection {
                    nickname,
                    writer: Arc::new(Mutex::new(writer)),
                },
            );
            broadcast_state(peers, session);
            let _ = event_tx.send(NetworkEvent::SessionSync(session.clone()));
        }
        Inbound::Action { peer_id, action } => {
            let origin = peers
                .get(&peer_id)
                .map(|peer| peer.nickname.clone())
                .unwrap_or_else(|| String::from("peer"));
            let local_action = wire_to_action(action);
            let requested_nickname = match &local_action {
                LocalAction::SetNickname { nickname } => Some(nickname.trim().to_string()),
                _ => None,
            };
            apply_action_to_session(session, local_action, &origin);
            if let Some(updated) = requested_nickname.filter(|name| !name.is_empty())
                && session
                    .participants
                    .iter()
                    .any(|participant| participant.nickname.eq_ignore_ascii_case(&updated))
                && let Some(peer) = peers.get_mut(&peer_id)
            {
                peer.nickname = updated;
            }
            broadcast_state(peers, session);
            let _ = event_tx.send(NetworkEvent::SessionSync(session.clone()));
        }
        Inbound::Pong { peer_id, nonce } => {
            let Some(pending) = pending_pings.get(&peer_id) else {
                return;
            };
            if pending.nonce != nonce {
                return;
            }
            let rtt_ms = pending
                .sent_at
                .elapsed()
                .as_millis()
                .clamp(0, u128::from(u16::MAX)) as u16;
            pending_pings.remove(&peer_id);
            if let Some(peer) = peers.get(&peer_id)
                && let Some(participant) = session
                    .participants
                    .iter_mut()
                    .find(|entry| entry.nickname.eq_ignore_ascii_case(&peer.nickname))
            {
                participant.ping_ms = smooth_ping(participant.ping_ms, rtt_ms);
            }
        }
        Inbound::StreamRequest {
            peer_id,
            path,
            request_id,
            source_nickname,
        } => {
            let Some(requester_peer) = peers.get(&peer_id) else {
                return;
            };
            let requester_writer = Arc::clone(&requester_peer.writer);

            if let Some(source_peer_id) = resolve_stream_source_peer_id(
                session,
                peers,
                peer_id,
                &path,
                source_nickname.as_deref(),
            ) {
                let upstream_request_id = next_request_id();
                pending_relay_requests.insert(
                    (source_peer_id, upstream_request_id),
                    RelayStreamRequest {
                        requester_peer_id: peer_id,
                        requester_request_id: request_id,
                        requested_path: path.clone(),
                    },
                );

                let Some(source_peer) = peers.get(&source_peer_id) else {
                    pending_relay_requests.remove(&(source_peer_id, upstream_request_id));
                    let _ = send_json_line_shared(
                        &requester_writer,
                        &WireServerMessage::StreamEnd {
                            request_id,
                            path,
                            success: false,
                            error: Some(String::from("stream source went offline")),
                        },
                    );
                    return;
                };

                if let Err(err) = send_json_line_shared(
                    &source_peer.writer,
                    &WireServerMessage::StreamRequest {
                        path: path.clone(),
                        request_id: upstream_request_id,
                    },
                ) {
                    pending_relay_requests.remove(&(source_peer_id, upstream_request_id));
                    let _ = send_json_line_shared(
                        &requester_writer,
                        &WireServerMessage::StreamEnd {
                            request_id,
                            path,
                            success: false,
                            error: Some(format!("peer relay request failed: {err}")),
                        },
                    );
                }
                return;
            }

            let quality = session.quality;
            thread::spawn(move || {
                if let Err(err) =
                    stream_file_to_client(&requester_writer, &path, request_id, quality)
                {
                    let _ = send_json_line_shared(
                        &requester_writer,
                        &WireServerMessage::StreamEnd {
                            request_id,
                            path,
                            success: false,
                            error: Some(format!("stream failed: {err}")),
                        },
                    );
                }
            });
        }
        Inbound::StreamStart {
            peer_id,
            request_id,
            path,
            total_bytes,
            payload_format,
        } => {
            if let Some(relay) = pending_relay_requests.get(&(peer_id, request_id)) {
                let Some(requester_peer) = peers.get(&relay.requester_peer_id) else {
                    pending_relay_requests.remove(&(peer_id, request_id));
                    return;
                };
                let requester_writer = Arc::clone(&requester_peer.writer);
                if relay.requested_path != path {
                    let _ = send_json_line_shared(
                        &requester_writer,
                        &WireServerMessage::StreamEnd {
                            request_id: relay.requester_request_id,
                            path: relay.requested_path.clone(),
                            success: false,
                            error: Some(String::from("peer stream start path mismatch")),
                        },
                    );
                    pending_relay_requests.remove(&(peer_id, request_id));
                    return;
                }
                if send_json_line_shared(
                    &requester_writer,
                    &WireServerMessage::StreamStart {
                        request_id: relay.requester_request_id,
                        path,
                        total_bytes,
                        payload_format,
                    },
                )
                .is_err()
                {
                    pending_relay_requests.remove(&(peer_id, request_id));
                }
                return;
            }
            let key = (peer_id, request_id);
            let expected_path = pending_pull_requests.get(&key);
            if expected_path != Some(&path) {
                let _ = event_tx.send(NetworkEvent::Status(String::from(
                    "Peer stream start path mismatch",
                )));
                pending_pull_requests.remove(&key);
                inbound_streams.remove(&key);
                return;
            }
            match InboundStreamDownload::new(&path, total_bytes, payload_format) {
                Ok(state) => {
                    inbound_streams.insert(key, state);
                }
                Err(err) => {
                    let _ = event_tx.send(NetworkEvent::Status(format!(
                        "Peer stream start failed: {err}",
                    )));
                    pending_pull_requests.remove(&key);
                }
            }
        }
        Inbound::StreamChunk {
            peer_id,
            request_id,
            data_base64,
        } => {
            if let Some(relay) = pending_relay_requests.get(&(peer_id, request_id)) {
                let Some(requester_peer) = peers.get(&relay.requester_peer_id) else {
                    pending_relay_requests.remove(&(peer_id, request_id));
                    return;
                };
                if send_json_line_shared(
                    &requester_peer.writer,
                    &WireServerMessage::StreamChunk {
                        request_id: relay.requester_request_id,
                        data_base64,
                    },
                )
                .is_err()
                {
                    pending_relay_requests.remove(&(peer_id, request_id));
                }
                return;
            }
            let key = (peer_id, request_id);
            let Some(state) = inbound_streams.get_mut(&key) else {
                return;
            };
            match base64::engine::general_purpose::STANDARD.decode(data_base64.as_bytes()) {
                Ok(bytes) => {
                    let ready_now = match state.payload_format {
                        StreamPayloadFormat::OriginalFile => {
                            if let Err(err) = state.file.write_all(&bytes) {
                                let _ = event_tx.send(NetworkEvent::Status(format!(
                                    "Peer stream write failed: {err}",
                                )));
                                inbound_streams.remove(&key);
                                pending_pull_requests.remove(&key);
                                return;
                            }
                            false
                        }
                        StreamPayloadFormat::BalancedOpus160kVbr => {
                            match ingest_balanced_stream_bytes(state, &bytes) {
                                Ok(ready) => ready,
                                Err(err) => {
                                    let _ = event_tx.send(NetworkEvent::Status(format!(
                                        "Peer stream write failed: {err}",
                                    )));
                                    inbound_streams.remove(&key);
                                    pending_pull_requests.remove(&key);
                                    return;
                                }
                            }
                        }
                    };
                    state.received_bytes = state.received_bytes.saturating_add(bytes.len() as u64);
                    if ready_now {
                        let _ = event_tx.send(NetworkEvent::StreamTrackReady {
                            requested_path: state.requested_path.clone(),
                            local_temp_path: state.local_temp_path.clone(),
                            format: stream_track_format(state.payload_format),
                        });
                    }
                }
                Err(err) => {
                    let _ = event_tx.send(NetworkEvent::Status(format!(
                        "Peer stream decode failed: {err}",
                    )));
                    inbound_streams.remove(&key);
                    pending_pull_requests.remove(&key);
                }
            }
        }
        Inbound::StreamEnd {
            peer_id,
            request_id,
            path,
            success,
            error,
        } => {
            if let Some(relay) = pending_relay_requests.remove(&(peer_id, request_id)) {
                if let Some(requester_peer) = peers.get(&relay.requester_peer_id) {
                    let (forward_success, forward_error) = if relay.requested_path != path {
                        (false, Some(String::from("peer stream end path mismatch")))
                    } else {
                        (success, error)
                    };
                    let _ = send_json_line_shared(
                        &requester_peer.writer,
                        &WireServerMessage::StreamEnd {
                            request_id: relay.requester_request_id,
                            path: relay.requested_path,
                            success: forward_success,
                            error: forward_error,
                        },
                    );
                }
                return;
            }
            let key = (peer_id, request_id);
            let Some(mut state) = inbound_streams.remove(&key) else {
                pending_pull_requests.remove(&key);
                return;
            };
            pending_pull_requests.remove(&key);
            if state.requested_path != path {
                let _ = fs::remove_file(&state.local_temp_path);
                let _ = event_tx.send(NetworkEvent::Status(String::from(
                    "Peer stream end path mismatch",
                )));
                return;
            }
            if !success {
                let _ = fs::remove_file(&state.local_temp_path);
                let _ = event_tx.send(NetworkEvent::Status(
                    error.unwrap_or_else(|| String::from("Peer stream failed")),
                ));
                return;
            }
            if !stream_size_matches(
                state.total_bytes,
                state.received_bytes,
                state.payload_format,
            ) {
                let _ = fs::remove_file(&state.local_temp_path);
                let _ = event_tx.send(NetworkEvent::Status(format!(
                    "Peer stream size mismatch: expected {} bytes got {} bytes",
                    state.total_bytes, state.received_bytes
                )));
                return;
            }
            if let Err(err) = finalize_inbound_stream(&mut state) {
                let _ = fs::remove_file(&state.local_temp_path);
                let _ = event_tx.send(NetworkEvent::Status(format!(
                    "Peer stream finalize failed: {err}",
                )));
                return;
            }
            if !state.ready_emitted {
                let _ = event_tx.send(NetworkEvent::StreamTrackReady {
                    requested_path: state.requested_path,
                    local_temp_path: state.local_temp_path,
                    format: stream_track_format(state.payload_format),
                });
            }
        }
        Inbound::Disconnected { peer_id } => {
            disconnect_peer(
                peer_id,
                session,
                &mut InboundState {
                    peers,
                    pending_pull_requests,
                    pending_relay_requests,
                    inbound_streams,
                    pending_pings,
                },
                "Peer disconnected",
                event_tx,
            );
        }
        Inbound::ReadError { peer_id, error } => {
            disconnect_peer(
                peer_id,
                session,
                &mut InboundState {
                    peers,
                    pending_pull_requests,
                    pending_relay_requests,
                    inbound_streams,
                    pending_pings,
                },
                &format!("Peer socket error: {error}"),
                event_tx,
            );
        }
    }
}

fn disconnect_peer(
    peer_id: u32,
    session: &mut OnlineSession,
    state: &mut InboundState<'_>,
    reason: &str,
    event_tx: &Sender<NetworkEvent>,
) {
    let InboundState {
        peers,
        pending_pull_requests,
        pending_relay_requests,
        inbound_streams,
        pending_pings,
    } = state;
    let nickname = peers.remove(&peer_id).map(|peer| peer.nickname);
    pending_pull_requests.retain(|(pending_peer_id, _), _| *pending_peer_id != peer_id);
    inbound_streams.retain(|(pending_peer_id, _), _| *pending_peer_id != peer_id);
    let removed_relays: Vec<RelayStreamRequest> = pending_relay_requests
        .iter()
        .filter(|((source_peer_id, _), relay)| {
            *source_peer_id == peer_id || relay.requester_peer_id == peer_id
        })
        .map(|(_, relay)| RelayStreamRequest {
            requester_peer_id: relay.requester_peer_id,
            requester_request_id: relay.requester_request_id,
            requested_path: relay.requested_path.clone(),
        })
        .collect();
    pending_relay_requests.retain(|(source_peer_id, _), relay| {
        *source_peer_id != peer_id && relay.requester_peer_id != peer_id
    });
    for relay in removed_relays {
        if relay.requester_peer_id == peer_id {
            continue;
        }
        if let Some(requester_peer) = peers.get(&relay.requester_peer_id) {
            let _ = send_json_line_shared(
                &requester_peer.writer,
                &WireServerMessage::StreamEnd {
                    request_id: relay.requester_request_id,
                    path: relay.requested_path,
                    success: false,
                    error: Some(String::from("stream source disconnected")),
                },
            );
        }
    }
    pending_pings.remove(&peer_id);

    let changed = if let Some(name) = nickname.as_deref() {
        let before = session.participants.len();
        let mut removed_host = false;
        session.participants.retain(|participant| {
            let matches = participant.nickname.eq_ignore_ascii_case(name);
            if matches && participant.is_host {
                removed_host = true;
            }
            !matches
        });

        let mut promoted_new_host = false;
        let mut promoted_nickname = String::new();
        if removed_host && !session.participants.is_empty() {
            for (index, participant) in session.participants.iter_mut().enumerate() {
                if index == 0 {
                    if !participant.is_host {
                        participant.is_host = true;
                        promoted_new_host = true;
                        promoted_nickname = participant.nickname.clone();
                    }
                } else {
                    participant.is_host = false;
                }
            }
            if promoted_new_host {
                let _ = event_tx.send(NetworkEvent::Status(format!(
                    "Host left room. New host: {promoted_nickname}"
                )));
            }
        }

        session.participants.len() != before || promoted_new_host
    } else {
        false
    };

    if changed {
        broadcast_state(peers, session);
        let _ = event_tx.send(NetworkEvent::SessionSync(session.clone()));
    }

    let suffix = nickname.unwrap_or_else(|| format!("peer-{peer_id}"));
    let _ = event_tx.send(NetworkEvent::Status(format!("{reason}: {suffix}")));
}

fn resolve_stream_source_peer_id(
    session: &OnlineSession,
    peers: &HashMap<u32, PeerConnection>,
    requester_peer_id: u32,
    requested_path: &Path,
    requested_source_nickname: Option<&str>,
) -> Option<u32> {
    if let Some(source_nickname) = requested_source_nickname {
        let source = peers
            .iter()
            .find(|(peer_id, peer)| {
                **peer_id != requester_peer_id
                    && peer.nickname.eq_ignore_ascii_case(source_nickname)
            })
            .map(|(peer_id, _)| *peer_id);
        if source.is_some() {
            return source;
        }
    }

    if let Some(owner) = session
        .shared_queue
        .iter()
        .rev()
        .find(|item| item.path == requested_path)
        .and_then(|item| item.owner_nickname.as_deref())
    {
        let source = peers
            .iter()
            .find(|(peer_id, peer)| {
                **peer_id != requester_peer_id && peer.nickname.eq_ignore_ascii_case(owner)
            })
            .map(|(peer_id, _)| *peer_id);
        if source.is_some() {
            return source;
        }
    }

    session.last_transport.as_ref().and_then(|transport| {
        peers
            .iter()
            .find(|(peer_id, peer)| {
                **peer_id != requester_peer_id
                    && peer
                        .nickname
                        .eq_ignore_ascii_case(&transport.origin_nickname)
            })
            .map(|(peer_id, _)| *peer_id)
    })
}

fn peer_display_name(peers: &HashMap<u32, PeerConnection>, peer_id: u32) -> String {
    peers
        .get(&peer_id)
        .map(|peer| peer.nickname.clone())
        .unwrap_or_else(|| format!("peer-{peer_id}"))
}

fn apply_action_to_session(
    session: &mut OnlineSession,
    action: LocalAction,
    origin_nickname: &str,
) {
    if !action_allowed_for_origin(session, &action, origin_nickname) {
        return;
    }

    match action {
        LocalAction::SetMode(mode) => session.mode = mode,
        LocalAction::SetQuality(quality) => session.quality = quality,
        LocalAction::SetNickname { nickname } => {
            let trimmed = nickname.trim();
            if trimmed.is_empty() {
                return;
            }
            let already_used = session.participants.iter().any(|participant| {
                participant.nickname.eq_ignore_ascii_case(trimmed)
                    && !participant.nickname.eq_ignore_ascii_case(origin_nickname)
            });
            if already_used {
                return;
            }
            if let Some(participant) = session
                .participants
                .iter_mut()
                .find(|participant| participant.nickname.eq_ignore_ascii_case(origin_nickname))
            {
                let previous = participant.nickname.clone();
                participant.nickname = trimmed.to_string();
                for item in &mut session.shared_queue {
                    if item
                        .owner_nickname
                        .as_deref()
                        .is_some_and(|owner| owner.eq_ignore_ascii_case(&previous))
                    {
                        item.owner_nickname = Some(participant.nickname.clone());
                    }
                }
                if let Some(last_transport) = session.last_transport.as_mut()
                    && last_transport
                        .origin_nickname
                        .eq_ignore_ascii_case(&previous)
                {
                    last_transport.origin_nickname = participant.nickname.clone();
                }
            }
        }
        LocalAction::QueueAdd(item) => {
            session.shared_queue.push(item);
            if session.shared_queue.len() > 512 {
                let remove = session.shared_queue.len() - 512;
                session.shared_queue.drain(0..remove);
            }
        }
        LocalAction::QueueConsume { expected_path } => {
            let can_consume = match (session.shared_queue.first(), expected_path.as_ref()) {
                (Some(_), None) => true,
                (Some(next), Some(expected)) => next.path == *expected,
                _ => false,
            };
            if can_consume {
                session.shared_queue.remove(0);
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

fn action_allowed_for_origin(
    session: &OnlineSession,
    action: &LocalAction,
    origin_nickname: &str,
) -> bool {
    if session.mode != crate::online::OnlineRoomMode::HostOnly {
        return true;
    }
    if origin_is_host(session, origin_nickname) {
        return true;
    }
    matches!(
        action,
        LocalAction::DelayUpdate { .. } | LocalAction::SetNickname { .. }
    )
}

fn allowed_upload_paths_for_client(
    session: &OnlineSession,
    local_nickname: &str,
) -> HashSet<PathBuf> {
    let mut allowed_paths: HashSet<PathBuf> = session
        .shared_queue
        .iter()
        .filter(|item| {
            item.owner_nickname
                .as_deref()
                .is_some_and(|owner| owner.eq_ignore_ascii_case(local_nickname))
        })
        .map(|item| item.path.clone())
        .collect();

    if let Some(last_transport) = session.last_transport.as_ref()
        && last_transport
            .origin_nickname
            .eq_ignore_ascii_case(local_nickname)
        && let Some(path) = transport_path(&last_transport.command)
    {
        allowed_paths.insert(path.to_path_buf());
    }

    allowed_paths
}

fn transport_path(command: &crate::online::TransportCommand) -> Option<&Path> {
    match command {
        crate::online::TransportCommand::PlayTrack { path, .. }
        | crate::online::TransportCommand::SetPlaybackState { path, .. } => Some(path.as_path()),
        crate::online::TransportCommand::StopPlayback
        | crate::online::TransportCommand::SetPaused { .. } => None,
    }
}

fn origin_is_host(session: &OnlineSession, origin_nickname: &str) -> bool {
    session.participants.iter().any(|participant| {
        participant.is_host && participant.nickname.eq_ignore_ascii_case(origin_nickname)
    })
}

fn broadcast_state(peers: &mut HashMap<u32, PeerConnection>, session: &OnlineSession) {
    broadcast(peers, &WireServerMessage::Session(session.clone()));
}

fn broadcast(peers: &mut HashMap<u32, PeerConnection>, message: &WireServerMessage) {
    let mut dead_ids = Vec::new();
    for (peer_id, peer) in peers.iter_mut() {
        if send_json_line_shared(&peer.writer, message).is_err() {
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
                    Ok(WireClientMessage::Pong { nonce }) => {
                        let _ = inbound_tx.send(Inbound::Pong { peer_id, nonce });
                    }
                    Ok(WireClientMessage::StreamRequest {
                        path,
                        request_id,
                        source_nickname,
                    }) => {
                        let _ = inbound_tx.send(Inbound::StreamRequest {
                            peer_id,
                            path,
                            request_id,
                            source_nickname,
                        });
                    }
                    Ok(WireClientMessage::StreamStart {
                        request_id,
                        path,
                        total_bytes,
                        payload_format,
                    }) => {
                        let _ = inbound_tx.send(Inbound::StreamStart {
                            peer_id,
                            request_id,
                            path,
                            total_bytes,
                            payload_format,
                        });
                    }
                    Ok(WireClientMessage::StreamChunk {
                        request_id,
                        data_base64,
                    }) => {
                        let _ = inbound_tx.send(Inbound::StreamChunk {
                            peer_id,
                            request_id,
                            data_base64,
                        });
                    }
                    Ok(WireClientMessage::StreamEnd {
                        request_id,
                        path,
                        success,
                        error,
                    }) => {
                        let _ = inbound_tx.send(Inbound::StreamEnd {
                            peer_id,
                            request_id,
                            path,
                            success,
                            error,
                        });
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

fn send_json_line_shared<T: Serialize>(
    stream: &Arc<Mutex<TcpStream>>,
    value: &T,
) -> anyhow::Result<()> {
    let mut locked = stream
        .lock()
        .map_err(|_| anyhow::anyhow!("peer socket lock poisoned"))?;
    send_json_line(&mut locked, value)
}

fn stream_file_to_client(
    writer: &Arc<Mutex<TcpStream>>,
    path: &Path,
    request_id: u64,
    quality: StreamQuality,
) -> anyhow::Result<()> {
    validate_stream_source(path)?;
    match quality {
        StreamQuality::Lossless => {
            let file_size = fs::metadata(path)
                .with_context(|| format!("failed to read stream metadata for {}", path.display()))?
                .len();
            send_json_line_shared(
                writer,
                &WireServerMessage::StreamStart {
                    request_id,
                    path: path.to_path_buf(),
                    total_bytes: file_size,
                    payload_format: StreamPayloadFormat::OriginalFile,
                },
            )?;
            stream_lossless_chunks(path, |chunk| {
                let encoded = base64::engine::general_purpose::STANDARD.encode(chunk);
                send_json_line_shared(
                    writer,
                    &WireServerMessage::StreamChunk {
                        request_id,
                        data_base64: encoded,
                    },
                )
            })?;
        }
        StreamQuality::Balanced => {
            send_json_line_shared(
                writer,
                &WireServerMessage::StreamStart {
                    request_id,
                    path: path.to_path_buf(),
                    total_bytes: 0,
                    payload_format: StreamPayloadFormat::BalancedOpus160kVbr,
                },
            )?;
            stream_balanced_opus_chunks(path, |chunk| {
                let encoded = base64::engine::general_purpose::STANDARD.encode(chunk);
                send_json_line_shared(
                    writer,
                    &WireServerMessage::StreamChunk {
                        request_id,
                        data_base64: encoded,
                    },
                )
            })?;
        }
    }

    send_json_line_shared(
        writer,
        &WireServerMessage::StreamEnd {
            request_id,
            path: path.to_path_buf(),
            success: true,
            error: None,
        },
    )
}

fn stream_file_to_host(
    writer: &Arc<Mutex<TcpStream>>,
    path: &Path,
    request_id: u64,
    quality: StreamQuality,
) -> anyhow::Result<()> {
    validate_stream_source(path)?;
    match quality {
        StreamQuality::Lossless => {
            let file_size = fs::metadata(path)
                .with_context(|| format!("failed to read stream metadata for {}", path.display()))?
                .len();
            send_json_line_shared(
                writer,
                &WireClientMessage::StreamStart {
                    request_id,
                    path: path.to_path_buf(),
                    total_bytes: file_size,
                    payload_format: StreamPayloadFormat::OriginalFile,
                },
            )?;
            stream_lossless_chunks(path, |chunk| {
                let encoded = base64::engine::general_purpose::STANDARD.encode(chunk);
                send_json_line_shared(
                    writer,
                    &WireClientMessage::StreamChunk {
                        request_id,
                        data_base64: encoded,
                    },
                )
            })?;
        }
        StreamQuality::Balanced => {
            send_json_line_shared(
                writer,
                &WireClientMessage::StreamStart {
                    request_id,
                    path: path.to_path_buf(),
                    total_bytes: 0,
                    payload_format: StreamPayloadFormat::BalancedOpus160kVbr,
                },
            )?;
            stream_balanced_opus_chunks(path, |chunk| {
                let encoded = base64::engine::general_purpose::STANDARD.encode(chunk);
                send_json_line_shared(
                    writer,
                    &WireClientMessage::StreamChunk {
                        request_id,
                        data_base64: encoded,
                    },
                )
            })?;
        }
    }

    send_json_line_shared(
        writer,
        &WireClientMessage::StreamEnd {
            request_id,
            path: path.to_path_buf(),
            success: true,
            error: None,
        },
    )
}

fn stream_lossless_chunks<F>(path: &Path, mut send_chunk: F) -> anyhow::Result<()>
where
    F: FnMut(&[u8]) -> anyhow::Result<()>,
{
    let mut file = File::open(path)
        .with_context(|| format!("failed to open stream source {}", path.display()))?;
    let mut buffer = vec![0_u8; STREAM_CHUNK_BYTES];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        send_chunk(&buffer[..read])?;
    }
    Ok(())
}

fn stream_balanced_opus_chunks<F>(source_path: &Path, mut send_chunk: F) -> anyhow::Result<()>
where
    F: FnMut(&[u8]) -> anyhow::Result<()>,
{
    let source_file = File::open(source_path)
        .with_context(|| format!("failed to open stream source {}", source_path.display()))?;
    let decoder = Decoder::try_from(source_file)
        .with_context(|| format!("failed to decode {}", source_path.display()))?;
    let source_rate = decoder.sample_rate().max(1);
    let source_channels = usize::from(decoder.channels()).max(1);

    let header = balanced_opus_header_bytes();
    send_chunk(&header)?;
    let mut payload_bytes_written: u64 = u64::try_from(header.len()).unwrap_or(u64::MAX);

    let mut channel_buffer = Vec::with_capacity(source_channels);
    let resample_step = f64::from(source_rate) / f64::from(BALANCED_STREAM_SAMPLE_RATE);
    let mut next_output_src_pos = 0.0_f64;
    let mut prev_frame: Option<(f32, f32)> = None;
    let mut input_frame_index: u64 = 0;
    let frame_samples =
        usize::try_from((BALANCED_STREAM_SAMPLE_RATE * BALANCED_OPUS_FRAME_MS) / 1_000)
            .unwrap_or(960)
            .max(1);
    let mut pcm_frame =
        Vec::with_capacity(frame_samples.saturating_mul(usize::from(BALANCED_STREAM_CHANNELS)));
    let mut packet_buf = vec![0_u8; BALANCED_OPUS_MAX_PACKET_BYTES];
    let mut wire_packet = vec![0_u8; BALANCED_OPUS_MAX_PACKET_BYTES + 2];
    let mut encoder = ManagedOpusEncoder::new(
        BALANCED_STREAM_SAMPLE_RATE,
        i32::from(BALANCED_STREAM_CHANNELS),
    )?;
    encoder.set_bitrate(BALANCED_OPUS_BITRATE_BPS)?;
    encoder.set_vbr(true)?;

    for sample in decoder {
        channel_buffer.push(sample);
        if channel_buffer.len() < source_channels {
            continue;
        }
        let (left, right) = if source_channels == 1 {
            let v = channel_buffer[0];
            (v, v)
        } else {
            (channel_buffer[0], channel_buffer[1])
        };
        channel_buffer.clear();
        let current = (left, right);

        if let Some((prev_left, prev_right)) = prev_frame {
            while next_output_src_pos <= input_frame_index as f64 {
                let segment_start = (input_frame_index.saturating_sub(1)) as f64;
                let frac = ((next_output_src_pos - segment_start).clamp(0.0, 1.0)) as f32;
                let mixed_left = prev_left + (current.0 - prev_left) * frac;
                let mixed_right = prev_right + (current.1 - prev_right) * frac;
                let pcm_left = (mixed_left.clamp(-1.0, 1.0) * i16::MAX as f32).round() as i16;
                let pcm_right = (mixed_right.clamp(-1.0, 1.0) * i16::MAX as f32).round() as i16;
                pcm_frame.push(pcm_left);
                pcm_frame.push(pcm_right);
                if pcm_frame.len()
                    == frame_samples.saturating_mul(usize::from(BALANCED_STREAM_CHANNELS))
                {
                    emit_balanced_opus_packet(
                        &mut encoder,
                        &pcm_frame,
                        &mut packet_buf,
                        &mut wire_packet,
                        &mut payload_bytes_written,
                        &mut send_chunk,
                    )?;
                    pcm_frame.clear();
                }
                next_output_src_pos += resample_step;
            }
        }

        prev_frame = Some(current);
        input_frame_index = input_frame_index.saturating_add(1);
    }

    if !pcm_frame.is_empty() {
        pcm_frame.resize(
            frame_samples.saturating_mul(usize::from(BALANCED_STREAM_CHANNELS)),
            0,
        );
        emit_balanced_opus_packet(
            &mut encoder,
            &pcm_frame,
            &mut packet_buf,
            &mut wire_packet,
            &mut payload_bytes_written,
            &mut send_chunk,
        )?;
    }
    Ok(())
}

fn emit_balanced_opus_packet<F>(
    encoder: &mut ManagedOpusEncoder,
    pcm_frame: &[i16],
    packet_buf: &mut [u8],
    wire_packet: &mut [u8],
    payload_bytes_written: &mut u64,
    send_chunk: &mut F,
) -> anyhow::Result<()>
where
    F: FnMut(&[u8]) -> anyhow::Result<()>,
{
    let encoded_size = encoder.encode(pcm_frame, packet_buf)?;
    let packet_len = u16::try_from(encoded_size).context("opus packet too large")?;
    wire_packet[..2].copy_from_slice(&packet_len.to_le_bytes());
    wire_packet[2..(2 + encoded_size)].copy_from_slice(&packet_buf[..encoded_size]);
    send_chunk(&wire_packet[..(2 + encoded_size)])?;
    *payload_bytes_written = payload_bytes_written
        .saturating_add(2)
        .saturating_add(u64::from(packet_len));
    if *payload_bytes_written > MAX_STREAM_FILE_BYTES {
        anyhow::bail!("balanced stream source exceeds size limit");
    }
    Ok(())
}

fn balanced_opus_header_bytes() -> [u8; 13] {
    let mut header = [0_u8; 13];
    header[..5].copy_from_slice(BALANCED_PAYLOAD_MAGIC);
    header[5..9].copy_from_slice(&BALANCED_STREAM_SAMPLE_RATE.to_le_bytes());
    header[9..11].copy_from_slice(&BALANCED_STREAM_CHANNELS.to_le_bytes());
    let frame_samples =
        u16::try_from((BALANCED_STREAM_SAMPLE_RATE * BALANCED_OPUS_FRAME_MS) / 1_000)
            .unwrap_or(960);
    header[11..13].copy_from_slice(&frame_samples.to_le_bytes());
    header
}

#[cfg(test)]
fn transcode_balanced_stream_to_opus_payload(source_path: &Path) -> anyhow::Result<PathBuf> {
    let mut output_path = std::env::temp_dir();
    output_path.push("tunetui_stream_cache");
    fs::create_dir_all(&output_path).with_context(|| {
        format!(
            "failed to create stream cache dir {}",
            output_path.display()
        )
    })?;
    let micros = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_micros();
    output_path.push(format!("balanced_{}.topus", micros));

    let mut output = File::create(&output_path)
        .with_context(|| format!("failed to create balanced stream {}", output_path.display()))?;
    stream_balanced_opus_chunks(source_path, |chunk| {
        output.write_all(chunk).with_context(|| {
            format!(
                "failed writing balanced stream chunk to {}",
                output_path.display()
            )
        })
    })?;
    output.flush()?;
    Ok(output_path)
}

struct ManagedOpusEncoder {
    raw: *mut RawOpusEncoder,
    channels: i32,
}

impl ManagedOpusEncoder {
    fn new(sample_rate: u32, channels: i32) -> anyhow::Result<Self> {
        let mut error = 0_i32;
        let raw = unsafe {
            opus_encoder_create(
                i32::try_from(sample_rate).unwrap_or(i32::MAX),
                channels,
                OPUS_APPLICATION_AUDIO,
                &mut error,
            )
        };
        if raw.is_null() || error != OPUS_OK {
            anyhow::bail!("failed to initialize opus encoder (error code {error})");
        }
        Ok(Self { raw, channels })
    }

    fn set_bitrate(&mut self, bitrate_bps: i32) -> anyhow::Result<()> {
        let ret = unsafe {
            unsafe_libopus::opus_encoder_ctl!(self.raw, OPUS_SET_BITRATE_REQUEST, bitrate_bps)
        };
        if ret != OPUS_OK {
            anyhow::bail!("failed to configure opus bitrate (error code {ret})");
        }
        Ok(())
    }

    fn set_vbr(&mut self, enabled: bool) -> anyhow::Result<()> {
        let value = if enabled { 1 } else { 0 };
        let ret =
            unsafe { unsafe_libopus::opus_encoder_ctl!(self.raw, OPUS_SET_VBR_REQUEST, value) };
        if ret != OPUS_OK {
            anyhow::bail!("failed to configure opus vbr (error code {ret})");
        }
        Ok(())
    }

    fn encode(&mut self, pcm_frame: &[i16], packet_buf: &mut [u8]) -> anyhow::Result<usize> {
        if self.channels <= 0 {
            anyhow::bail!("invalid opus channel configuration");
        }
        let channels = usize::try_from(self.channels).unwrap_or(1).max(1);
        let frame_size_per_channel = pcm_frame.len() / channels;
        let encoded = unsafe {
            opus_encode(
                self.raw,
                pcm_frame.as_ptr(),
                i32::try_from(frame_size_per_channel).unwrap_or(i32::MAX),
                packet_buf.as_mut_ptr(),
                i32::try_from(packet_buf.len()).unwrap_or(i32::MAX),
            )
        };
        if encoded < 0 {
            anyhow::bail!("opus encode failed (error code {encoded})");
        }
        Ok(usize::try_from(encoded).unwrap_or(0))
    }
}

impl Drop for ManagedOpusEncoder {
    fn drop(&mut self) {
        if !self.raw.is_null() {
            unsafe {
                opus_encoder_destroy(self.raw);
            }
        }
    }
}

struct ManagedOpusDecoder {
    raw: *mut RawOpusDecoder,
    channels: i32,
}

impl ManagedOpusDecoder {
    fn new(sample_rate: u32, channels: i32) -> anyhow::Result<Self> {
        let mut error = 0_i32;
        let raw = unsafe {
            opus_decoder_create(
                i32::try_from(sample_rate).unwrap_or(i32::MAX),
                channels,
                &mut error,
            )
        };
        if raw.is_null() || error != OPUS_OK {
            anyhow::bail!("failed to initialize opus decoder (error code {error})");
        }
        Ok(Self { raw, channels })
    }

    fn decode(
        &mut self,
        packet: &[u8],
        pcm_buffer: &mut [i16],
        decode_fec: bool,
    ) -> anyhow::Result<usize> {
        let decoded = unsafe {
            let channels = usize::try_from(self.channels).unwrap_or(1).max(1);
            let frame_size_per_channel = pcm_buffer.len() / channels;
            opus_decode(
                self.raw,
                packet.as_ptr(),
                i32::try_from(packet.len()).unwrap_or(i32::MAX),
                pcm_buffer.as_mut_ptr(),
                i32::try_from(frame_size_per_channel).unwrap_or(i32::MAX),
                if decode_fec { 1 } else { 0 },
            )
        };
        if decoded < 0 {
            anyhow::bail!("opus decode failed (error code {decoded})");
        }
        Ok(usize::try_from(decoded).unwrap_or(0))
    }
}

impl Drop for ManagedOpusDecoder {
    fn drop(&mut self) {
        if !self.raw.is_null() {
            unsafe {
                opus_decoder_destroy(self.raw);
            }
        }
    }
}

#[cfg(test)]
fn decode_balanced_opus_payload_to_wav(
    payload_path: &Path,
    requested_path: &Path,
) -> anyhow::Result<PathBuf> {
    let mut payload = File::open(payload_path)
        .with_context(|| format!("failed to open balanced payload {}", payload_path.display()))?;

    let mut magic = [0_u8; 5];
    payload
        .read_exact(&mut magic)
        .context("invalid balanced payload header")?;
    if &magic != BALANCED_PAYLOAD_MAGIC {
        anyhow::bail!("invalid balanced payload magic");
    }

    let mut sr_bytes = [0_u8; 4];
    payload
        .read_exact(&mut sr_bytes)
        .context("missing balanced payload sample rate")?;
    let sample_rate = u32::from_le_bytes(sr_bytes);

    let mut channels_bytes = [0_u8; 2];
    payload
        .read_exact(&mut channels_bytes)
        .context("missing balanced payload channels")?;
    let channels = u16::from_le_bytes(channels_bytes);

    let mut frame_bytes = [0_u8; 2];
    payload
        .read_exact(&mut frame_bytes)
        .context("missing balanced payload frame size")?;
    let frame_samples = usize::from(u16::from_le_bytes(frame_bytes));

    if sample_rate == 0 || frame_samples == 0 {
        anyhow::bail!("invalid balanced payload parameters");
    }
    if channels != 2 {
        anyhow::bail!(
            "unsupported balanced payload channels: {channels} (expected 2; legacy mono payloads are blocked)"
        );
    }

    let mut decoder = ManagedOpusDecoder::new(sample_rate, i32::from(channels))?;
    let wav_path = create_decoded_wav_cache_path(requested_path)?;
    let mut wav_file = File::create(&wav_path)
        .with_context(|| format!("failed to create decoded WAV {}", wav_path.display()))?;
    write_wav_header_placeholder(&mut wav_file, sample_rate, channels)?;

    let channels_usize = usize::from(channels).max(1);
    let mut pcm_buffer = vec![
        0_i16;
        frame_samples
            .saturating_mul(channels_usize)
            .saturating_mul(6)
            .max(frame_samples.saturating_mul(channels_usize))
    ];
    let mut packet_len_bytes = [0_u8; 2];
    let mut wav_data_bytes: u64 = 0;

    loop {
        match payload.read_exact(&mut packet_len_bytes) {
            Ok(()) => {}
            Err(err) if err.kind() == std::io::ErrorKind::UnexpectedEof => break,
            Err(err) => return Err(err).context("failed to read balanced packet length"),
        }
        let packet_len = usize::from(u16::from_le_bytes(packet_len_bytes));
        if packet_len == 0 {
            continue;
        }
        let mut packet = vec![0_u8; packet_len];
        payload
            .read_exact(&mut packet)
            .context("failed to read balanced packet")?;

        let decoded_per_channel = decoder.decode(&packet, &mut pcm_buffer, false)?;
        if decoded_per_channel == 0 {
            continue;
        }
        let sample_count = decoded_per_channel.saturating_mul(channels_usize);
        let mut bytes = Vec::with_capacity(sample_count.saturating_mul(2));
        for sample in pcm_buffer.iter().take(sample_count) {
            bytes.extend_from_slice(&sample.to_le_bytes());
        }
        wav_file
            .write_all(&bytes)
            .with_context(|| format!("failed writing decoded WAV {}", wav_path.display()))?;
        wav_data_bytes =
            wav_data_bytes.saturating_add(u64::try_from(bytes.len()).unwrap_or(u64::MAX));
        if wav_data_bytes > MAX_STREAM_FILE_BYTES {
            let _ = fs::remove_file(&wav_path);
            anyhow::bail!("decoded balanced stream exceeds size limit");
        }
    }

    finalize_wav_header(&mut wav_file, wav_data_bytes)?;
    wav_file.flush()?;
    Ok(wav_path)
}

#[cfg(test)]
fn create_decoded_wav_cache_path(requested_path: &Path) -> anyhow::Result<PathBuf> {
    create_stream_cache_path(requested_path, StreamPayloadFormat::OriginalFile).map(|mut path| {
        path.set_extension("wav");
        path
    })
}

fn write_wav_header_placeholder(
    file: &mut File,
    sample_rate: u32,
    channels: u16,
) -> anyhow::Result<()> {
    let unknown_size = u32::MAX;
    file.write_all(b"RIFF")?;
    file.write_all(&unknown_size.to_le_bytes())?;
    file.write_all(b"WAVE")?;
    file.write_all(b"fmt ")?;
    file.write_all(&16_u32.to_le_bytes())?;
    file.write_all(&1_u16.to_le_bytes())?;
    file.write_all(&channels.to_le_bytes())?;
    file.write_all(&sample_rate.to_le_bytes())?;
    let bytes_per_sample = u32::from(BALANCED_STREAM_BITS_PER_SAMPLE / 8);
    let byte_rate = sample_rate
        .saturating_mul(u32::from(channels))
        .saturating_mul(bytes_per_sample);
    file.write_all(&byte_rate.to_le_bytes())?;
    let block_align = channels.saturating_mul(BALANCED_STREAM_BITS_PER_SAMPLE / 8);
    file.write_all(&block_align.to_le_bytes())?;
    file.write_all(&BALANCED_STREAM_BITS_PER_SAMPLE.to_le_bytes())?;
    file.write_all(b"data")?;
    file.write_all(&unknown_size.to_le_bytes())?;
    Ok(())
}

fn finalize_wav_header(file: &mut File, data_bytes: u64) -> anyhow::Result<()> {
    let data_bytes_u32 = u32::try_from(data_bytes).context("balanced stream WAV too large")?;
    let riff_size = 36_u32.saturating_add(data_bytes_u32);
    file.seek(SeekFrom::Start(4))?;
    file.write_all(&riff_size.to_le_bytes())?;
    file.seek(SeekFrom::Start(40))?;
    file.write_all(&data_bytes_u32.to_le_bytes())?;
    file.flush()?;
    Ok(())
}

fn validate_stream_source(path: &Path) -> anyhow::Result<()> {
    let metadata = fs::metadata(path)
        .with_context(|| format!("failed to read stream metadata for {}", path.display()))?;
    if !metadata.is_file() {
        anyhow::bail!("stream source must be a file");
    }
    if metadata.len() > MAX_STREAM_FILE_BYTES {
        anyhow::bail!("stream source exceeds size limit");
    }
    Ok(())
}

fn stream_size_matches(expected: u64, received: u64, payload_format: StreamPayloadFormat) -> bool {
    match payload_format {
        StreamPayloadFormat::OriginalFile => expected == received,
        StreamPayloadFormat::BalancedOpus160kVbr => expected == 0 || expected == received,
    }
}

fn stream_track_format(payload_format: StreamPayloadFormat) -> StreamTrackFormat {
    match payload_format {
        StreamPayloadFormat::OriginalFile => StreamTrackFormat::LosslessOriginal,
        StreamPayloadFormat::BalancedOpus160kVbr => StreamTrackFormat::BalancedOpus160kVbrStereo,
    }
}

fn next_request_id() -> u64 {
    rand::rng().random()
}

fn smooth_ping(previous: u16, sample: u16) -> u16 {
    if previous == 0 {
        sample
    } else {
        ((u32::from(previous) * 3 + u32::from(sample)) / 4) as u16
    }
}

#[derive(Debug)]
struct PeerConnection {
    nickname: String,
    writer: Arc<Mutex<TcpStream>>,
}

#[derive(Debug)]
struct PendingPing {
    nonce: u64,
    sent_at: Instant,
}

struct InboundState<'a> {
    peers: &'a mut HashMap<u32, PeerConnection>,
    pending_pull_requests: &'a mut HashMap<(u32, u64), PathBuf>,
    pending_relay_requests: &'a mut HashMap<(u32, u64), RelayStreamRequest>,
    inbound_streams: &'a mut HashMap<(u32, u64), InboundStreamDownload>,
    pending_pings: &'a mut HashMap<u32, PendingPing>,
}

#[derive(Debug)]
struct RelayStreamRequest {
    requester_peer_id: u32,
    requester_request_id: u64,
    requested_path: PathBuf,
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
    Pong {
        peer_id: u32,
        nonce: u64,
    },
    StreamRequest {
        peer_id: u32,
        path: PathBuf,
        request_id: u64,
        source_nickname: Option<String>,
    },
    StreamStart {
        peer_id: u32,
        request_id: u64,
        path: PathBuf,
        total_bytes: u64,
        payload_format: StreamPayloadFormat,
    },
    StreamChunk {
        peer_id: u32,
        request_id: u64,
        data_base64: String,
    },
    StreamEnd {
        peer_id: u32,
        request_id: u64,
        path: PathBuf,
        success: bool,
        error: Option<String>,
    },
    Disconnected {
        peer_id: u32,
    },
    ReadError {
        peer_id: u32,
        error: String,
    },
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
enum StreamPayloadFormat {
    OriginalFile,
    BalancedOpus160kVbr,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
enum WireClientMessage {
    Hello {
        room_code: String,
        nickname: String,
        password: Option<String>,
    },
    Action(WireAction),
    Pong {
        nonce: u64,
    },
    StreamRequest {
        path: PathBuf,
        request_id: u64,
        #[serde(default)]
        source_nickname: Option<String>,
    },
    StreamStart {
        request_id: u64,
        path: PathBuf,
        total_bytes: u64,
        payload_format: StreamPayloadFormat,
    },
    StreamChunk {
        request_id: u64,
        data_base64: String,
    },
    StreamEnd {
        request_id: u64,
        path: PathBuf,
        success: bool,
        error: Option<String>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
enum WireServerMessage {
    HelloAck {
        accepted: bool,
        reason: Option<String>,
        session: Option<OnlineSession>,
    },
    Session(OnlineSession),
    Ping {
        nonce: u64,
    },
    StreamRequest {
        path: PathBuf,
        request_id: u64,
    },
    StreamStart {
        request_id: u64,
        path: PathBuf,
        total_bytes: u64,
        payload_format: StreamPayloadFormat,
    },
    StreamChunk {
        request_id: u64,
        data_base64: String,
    },
    StreamEnd {
        request_id: u64,
        path: PathBuf,
        success: bool,
        error: Option<String>,
    },
    Status(String),
}

struct InboundStreamDownload {
    requested_path: PathBuf,
    local_temp_path: PathBuf,
    file: File,
    received_bytes: u64,
    total_bytes: u64,
    payload_format: StreamPayloadFormat,
    header_parsed: bool,
    packet_buffer: Vec<u8>,
    decoder: Option<ManagedOpusDecoder>,
    pcm_buffer: Vec<i16>,
    wav_data_bytes: u64,
    ready_emitted: bool,
}

#[derive(Debug)]
struct ClientUploadGuard {
    local_nickname: String,
    allowed_paths: HashSet<PathBuf>,
}

impl InboundStreamDownload {
    fn new(
        requested_path: &Path,
        total_bytes: u64,
        payload_format: StreamPayloadFormat,
    ) -> anyhow::Result<Self> {
        let local_temp_path = create_stream_cache_path(requested_path, payload_format)?;
        let file = File::create(&local_temp_path).with_context(|| {
            format!(
                "failed to create stream cache {}",
                local_temp_path.display()
            )
        })?;
        let mut state = Self {
            requested_path: requested_path.to_path_buf(),
            local_temp_path,
            file,
            received_bytes: 0,
            total_bytes,
            payload_format,
            header_parsed: false,
            packet_buffer: Vec::new(),
            decoder: None,
            pcm_buffer: Vec::new(),
            wav_data_bytes: 0,
            ready_emitted: false,
        };
        if payload_format == StreamPayloadFormat::OriginalFile {
            state.header_parsed = true;
        }
        Ok(state)
    }
}

fn ingest_balanced_stream_bytes(
    state: &mut InboundStreamDownload,
    bytes: &[u8],
) -> anyhow::Result<bool> {
    state.packet_buffer.extend_from_slice(bytes);

    if !state.header_parsed {
        if state.packet_buffer.len() < 13 {
            return Ok(false);
        }
        let mut header = [0_u8; 13];
        header.copy_from_slice(&state.packet_buffer[..13]);
        if &header[..5] != BALANCED_PAYLOAD_MAGIC {
            anyhow::bail!("invalid balanced payload magic");
        }
        let sample_rate = u32::from_le_bytes([header[5], header[6], header[7], header[8]]);
        let channels = u16::from_le_bytes([header[9], header[10]]);
        let frame_samples = usize::from(u16::from_le_bytes([header[11], header[12]]));
        if sample_rate == 0 || frame_samples == 0 {
            anyhow::bail!("invalid balanced payload parameters");
        }
        if channels != 2 {
            anyhow::bail!(
                "unsupported balanced payload channels: {channels} (expected 2; legacy mono payloads are blocked)"
            );
        }
        write_wav_header_placeholder(&mut state.file, sample_rate, channels)?;
        state.decoder = Some(ManagedOpusDecoder::new(sample_rate, i32::from(channels))?);
        state.pcm_buffer = vec![
            0_i16;
            frame_samples
                .saturating_mul(usize::from(channels))
                .saturating_mul(6)
        ];
        state.header_parsed = true;
        state.packet_buffer.drain(0..13);
    }

    let mut ready_now = false;
    let mut consumed = 0_usize;
    while state.packet_buffer.len().saturating_sub(consumed) >= 2 {
        let len_off = consumed;
        let packet_len = usize::from(u16::from_le_bytes([
            state.packet_buffer[len_off],
            state.packet_buffer[len_off + 1],
        ]));
        if packet_len == 0 {
            consumed = consumed.saturating_add(2);
            continue;
        }
        let packet_start = len_off.saturating_add(2);
        let packet_end = packet_start.saturating_add(packet_len);
        if packet_end > state.packet_buffer.len() {
            break;
        }
        let decoder = state
            .decoder
            .as_mut()
            .ok_or_else(|| anyhow::anyhow!("balanced decoder not initialized"))?;
        let packet = &state.packet_buffer[packet_start..packet_end];
        let decoded_per_channel = decoder.decode(packet, &mut state.pcm_buffer, false)?;
        if decoded_per_channel > 0 {
            let sample_count = decoded_per_channel.saturating_mul(2);
            let mut pcm_bytes = Vec::with_capacity(sample_count.saturating_mul(2));
            for sample in state.pcm_buffer.iter().take(sample_count) {
                pcm_bytes.extend_from_slice(&sample.to_le_bytes());
            }
            state.file.write_all(&pcm_bytes)?;
            state.wav_data_bytes = state
                .wav_data_bytes
                .saturating_add(u64::try_from(pcm_bytes.len()).unwrap_or(u64::MAX));
            if state.wav_data_bytes > MAX_STREAM_FILE_BYTES {
                anyhow::bail!("decoded balanced stream exceeds size limit");
            }
            if !state.ready_emitted && state.wav_data_bytes >= BALANCED_FALLBACK_READY_PCM_BYTES {
                state.file.flush()?;
                state.ready_emitted = true;
                ready_now = true;
            }
        }
        consumed = packet_end;
    }
    if consumed > 0 {
        state.packet_buffer.drain(0..consumed);
    }
    Ok(ready_now)
}

fn finalize_inbound_stream(state: &mut InboundStreamDownload) -> anyhow::Result<()> {
    match state.payload_format {
        StreamPayloadFormat::OriginalFile => {
            state.file.flush()?;
            Ok(())
        }
        StreamPayloadFormat::BalancedOpus160kVbr => {
            let _ = ingest_balanced_stream_bytes(state, &[])?;
            if !state.header_parsed {
                anyhow::bail!("missing balanced stream header");
            }
            if !state.packet_buffer.is_empty() {
                anyhow::bail!("incomplete balanced stream packet payload");
            }
            finalize_wav_header(&mut state.file, state.wav_data_bytes)?;
            state.file.flush()?;
            Ok(())
        }
    }
}

fn create_stream_cache_path(
    source: &Path,
    payload_format: StreamPayloadFormat,
) -> anyhow::Result<PathBuf> {
    let mut dir = std::env::temp_dir();
    dir.push("tunetui_stream_cache");
    fs::create_dir_all(&dir)
        .with_context(|| format!("failed to create stream cache dir {}", dir.display()))?;

    let stem = source
        .file_stem()
        .and_then(|name| name.to_str())
        .map(sanitize_cache_name)
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| String::from("track"));
    let ext = match payload_format {
        StreamPayloadFormat::OriginalFile => source
            .extension()
            .and_then(|value| value.to_str())
            .map(sanitize_cache_name)
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| String::from("bin")),
        StreamPayloadFormat::BalancedOpus160kVbr => String::from("wav"),
    };
    let micros = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_micros();
    dir.push(format!("{}_{}.{}", stem, micros, ext));
    Ok(dir)
}

fn sanitize_cache_name(input: &str) -> String {
    input
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
                ch
            } else {
                '_'
            }
        })
        .take(64)
        .collect()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
enum WireAction {
    SetMode(crate::online::OnlineRoomMode),
    SetQuality(StreamQuality),
    SetNickname {
        nickname: String,
    },
    QueueAdd(SharedQueueItem),
    QueueConsume {
        expected_path: Option<PathBuf>,
    },
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
        LocalAction::SetNickname { nickname } => WireAction::SetNickname { nickname },
        LocalAction::QueueAdd(item) => WireAction::QueueAdd(item),
        LocalAction::QueueConsume { expected_path } => WireAction::QueueConsume { expected_path },
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
        WireAction::SetNickname { nickname } => LocalAction::SetNickname { nickname },
        WireAction::QueueAdd(item) => LocalAction::QueueAdd(item),
        WireAction::QueueConsume { expected_path } => LocalAction::QueueConsume { expected_path },
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
    use std::fs::{self, File};
    use std::io::Write;
    use std::net::TcpListener;
    use std::path::Path;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn unique_temp_file(name: &str, ext: &str) -> PathBuf {
        let mut path = std::env::temp_dir();
        let micros = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_micros();
        path.push(format!("tunetui_test_{}_{}.{}", name, micros, ext));
        path
    }

    #[test]
    fn invite_code_round_trips_with_password_key() {
        let code = build_invite_code("192.168.1.33:7878", "party123").expect("code build");
        let decoded = decode_invite_code(&code, "party123").expect("decode");
        assert_eq!(decoded.server_addr, "192.168.1.33:7878");
        assert_eq!(decoded.room_code, code);
    }

    #[test]
    fn invite_code_rejects_wrong_password() {
        let code = build_invite_code("10.0.0.8:9000", "party123").expect("code build");
        let decoded = decode_invite_code(&code, "wrong-pass");
        assert!(decoded.is_err());
    }

    #[test]
    fn invite_code_requires_password() {
        let code = build_invite_code("10.0.0.8:9000", "party123").expect("code build");
        let decoded = decode_invite_code(&code, "");
        assert!(decoded.is_err());
    }

    #[test]
    fn invite_code_uses_secure_prefix() {
        let code = build_invite_code("10.0.0.8:9000", "party123").expect("code build");
        assert!(code.starts_with(INVITE_PREFIX_SECURE));
    }

    #[test]
    fn parses_xor_mapped_ipv4_from_stun_response() {
        let txid = [1_u8, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12];
        let mapped = [74_u8, 199, 151, 6];
        let cookie = STUN_MAGIC_COOKIE.to_be_bytes();
        let xored = [
            mapped[0] ^ cookie[0],
            mapped[1] ^ cookie[1],
            mapped[2] ^ cookie[2],
            mapped[3] ^ cookie[3],
        ];

        let mut packet = Vec::new();
        packet.extend_from_slice(&STUN_BINDING_SUCCESS_RESPONSE.to_be_bytes());
        packet.extend_from_slice(&12_u16.to_be_bytes());
        packet.extend_from_slice(&STUN_MAGIC_COOKIE.to_be_bytes());
        packet.extend_from_slice(&txid);
        packet.extend_from_slice(&STUN_ATTR_XOR_MAPPED_ADDRESS.to_be_bytes());
        packet.extend_from_slice(&8_u16.to_be_bytes());
        packet.push(0);
        packet.push(0x01);
        packet.extend_from_slice(&0_u16.to_be_bytes());
        packet.extend_from_slice(&xored);

        let parsed = parse_stun_mapped_ipv4(&packet, &txid).expect("parsed mapped ip");
        assert_eq!(parsed, Ipv4Addr::new(74, 199, 151, 6));
    }

    #[test]
    fn stream_wire_messages_preserve_request_id() {
        let msg = WireServerMessage::StreamRequest {
            path: PathBuf::from("track.flac"),
            request_id: 42,
        };
        let encoded = serde_json::to_string(&msg).expect("serialize");
        let decoded: WireServerMessage = serde_json::from_str(&encoded).expect("deserialize");
        match decoded {
            WireServerMessage::StreamRequest { path, request_id } => {
                assert_eq!(path, PathBuf::from("track.flac"));
                assert_eq!(request_id, 42);
            }
            other => panic!("unexpected message: {other:?}"),
        }
    }

    #[test]
    fn client_stream_request_round_trip_preserves_source_nickname() {
        let msg = WireClientMessage::StreamRequest {
            path: PathBuf::from("track.flac"),
            request_id: 88,
            source_nickname: Some(String::from("dj-peer")),
        };
        let encoded = serde_json::to_string(&msg).expect("serialize");
        let decoded: WireClientMessage = serde_json::from_str(&encoded).expect("deserialize");
        match decoded {
            WireClientMessage::StreamRequest {
                path,
                request_id,
                source_nickname,
            } => {
                assert_eq!(path, PathBuf::from("track.flac"));
                assert_eq!(request_id, 88);
                assert_eq!(source_nickname.as_deref(), Some("dj-peer"));
            }
            other => panic!("unexpected message: {other:?}"),
        }
    }

    #[test]
    fn stream_start_round_trip_preserves_payload_format() {
        let msg = WireServerMessage::StreamStart {
            request_id: 7,
            path: PathBuf::from("track.flac"),
            total_bytes: 123,
            payload_format: StreamPayloadFormat::BalancedOpus160kVbr,
        };
        let encoded = serde_json::to_string(&msg).expect("serialize");
        let decoded: WireServerMessage = serde_json::from_str(&encoded).expect("deserialize");
        match decoded {
            WireServerMessage::StreamStart {
                request_id,
                path,
                total_bytes,
                payload_format,
            } => {
                assert_eq!(request_id, 7);
                assert_eq!(path, PathBuf::from("track.flac"));
                assert_eq!(total_bytes, 123);
                assert_eq!(payload_format, StreamPayloadFormat::BalancedOpus160kVbr);
            }
            other => panic!("unexpected message: {other:?}"),
        }
    }

    #[test]
    fn upload_guard_allows_local_last_transport_path() {
        let mut session = OnlineSession::join("ROOM22", "alice");
        session.last_transport = Some(crate::online::TransportEnvelope {
            seq: 1,
            origin_nickname: String::from("alice"),
            command: crate::online::TransportCommand::PlayTrack {
                path: PathBuf::from("live-set.flac"),
                title: None,
                artist: None,
                album: None,
                provider_track_id: None,
            },
        });

        let allowed = allowed_upload_paths_for_client(&session, "alice");
        assert!(allowed.contains(&PathBuf::from("live-set.flac")));
    }

    #[test]
    fn upload_guard_ignores_non_local_last_transport_path() {
        let mut session = OnlineSession::join("ROOM22", "alice");
        session.last_transport = Some(crate::online::TransportEnvelope {
            seq: 1,
            origin_nickname: String::from("bob"),
            command: crate::online::TransportCommand::SetPlaybackState {
                path: PathBuf::from("other.flac"),
                title: None,
                artist: None,
                album: None,
                provider_track_id: None,
                position_ms: 500,
                paused: false,
            },
        });

        let allowed = allowed_upload_paths_for_client(&session, "alice");
        assert!(!allowed.contains(&PathBuf::from("other.flac")));
    }

    #[test]
    fn stream_cache_path_uses_wav_extension_for_balanced_payload() {
        let path = create_stream_cache_path(
            Path::new("artist/song.flac"),
            StreamPayloadFormat::BalancedOpus160kVbr,
        )
        .expect("cache path");
        assert_eq!(path.extension().and_then(|ext| ext.to_str()), Some("wav"));
    }

    #[test]
    fn balanced_opus_encode_decode_round_trip_accepts_stereo_payload() {
        let source_path = unique_temp_file("balanced_source", "wav");
        let mut source_file = File::create(&source_path).expect("create source wav");
        write_wav_header_placeholder(&mut source_file, BALANCED_STREAM_SAMPLE_RATE, 2)
            .expect("write source wav header");

        let frames = usize::try_from(BALANCED_STREAM_SAMPLE_RATE / 10).unwrap_or(4_800);
        let mut data_bytes: u64 = 0;
        for _ in 0..frames {
            let left: i16 = 1_024;
            let right: i16 = -1_024;
            source_file
                .write_all(&left.to_le_bytes())
                .expect("write left sample");
            source_file
                .write_all(&right.to_le_bytes())
                .expect("write right sample");
            data_bytes = data_bytes.saturating_add(4);
        }
        finalize_wav_header(&mut source_file, data_bytes).expect("finalize source wav");

        let payload_path = transcode_balanced_stream_to_opus_payload(&source_path)
            .expect("encode balanced payload");
        let decoded_path = decode_balanced_opus_payload_to_wav(&payload_path, &source_path)
            .expect("decode balanced payload");

        let decoded_size = fs::metadata(&decoded_path).expect("decoded metadata").len();
        assert!(decoded_size > 44);

        let _ = fs::remove_file(source_path);
        let _ = fs::remove_file(payload_path);
        let _ = fs::remove_file(decoded_path);
    }

    #[test]
    fn balanced_decoder_rejects_legacy_mono_payload() {
        let payload_path = unique_temp_file("balanced_payload_mono", "topus");
        let mut payload = File::create(&payload_path).expect("create payload");
        payload
            .write_all(BALANCED_PAYLOAD_MAGIC)
            .expect("write magic");
        payload
            .write_all(&BALANCED_STREAM_SAMPLE_RATE.to_le_bytes())
            .expect("write sample rate");
        payload
            .write_all(&1_u16.to_le_bytes())
            .expect("write mono channel header");
        payload
            .write_all(&960_u16.to_le_bytes())
            .expect("write frame samples");
        payload.flush().expect("flush payload");

        let requested = unique_temp_file("requested_track", "mp3");
        let err = decode_balanced_opus_payload_to_wav(&payload_path, &requested)
            .expect_err("mono payload should be rejected");
        assert!(
            err.to_string().contains("legacy mono payloads are blocked"),
            "unexpected error: {err}"
        );

        let _ = fs::remove_file(payload_path);
    }

    #[test]
    fn balanced_stream_size_check_allows_unknown_total_bytes() {
        assert!(stream_size_matches(
            0,
            42,
            StreamPayloadFormat::BalancedOpus160kVbr
        ));
        assert!(!stream_size_matches(
            12,
            42,
            StreamPayloadFormat::OriginalFile
        ));
    }

    #[test]
    fn queue_consume_removes_front_when_expected_matches() {
        let mut session = OnlineSession::host("host");
        session.shared_queue.push(crate::online::SharedQueueItem {
            path: PathBuf::from("a.flac"),
            title: String::from("a"),
            delivery: crate::online::QueueDelivery::HostStreamOnly,
            owner_nickname: Some(String::from("host")),
        });
        session.shared_queue.push(crate::online::SharedQueueItem {
            path: PathBuf::from("b.flac"),
            title: String::from("b"),
            delivery: crate::online::QueueDelivery::HostStreamOnly,
            owner_nickname: Some(String::from("host")),
        });

        apply_action_to_session(
            &mut session,
            LocalAction::QueueConsume {
                expected_path: Some(PathBuf::from("a.flac")),
            },
            "host",
        );

        assert_eq!(session.shared_queue.len(), 1);
        assert_eq!(session.shared_queue[0].path, PathBuf::from("b.flac"));
    }

    #[test]
    fn queue_consume_keeps_queue_when_expected_mismatch() {
        let mut session = OnlineSession::host("host");
        session.shared_queue.push(crate::online::SharedQueueItem {
            path: PathBuf::from("a.flac"),
            title: String::from("a"),
            delivery: crate::online::QueueDelivery::HostStreamOnly,
            owner_nickname: Some(String::from("host")),
        });

        apply_action_to_session(
            &mut session,
            LocalAction::QueueConsume {
                expected_path: Some(PathBuf::from("b.flac")),
            },
            "host",
        );

        assert_eq!(session.shared_queue.len(), 1);
        assert_eq!(session.shared_queue[0].path, PathBuf::from("a.flac"));
    }

    #[test]
    fn host_only_blocks_listener_queue_add_network_action() {
        let mut session = OnlineSession::host("host");
        session.mode = crate::online::OnlineRoomMode::HostOnly;
        session.participants.push(crate::online::Participant {
            nickname: String::from("listener"),
            is_local: false,
            is_host: false,
            ping_ms: 0,
            manual_extra_delay_ms: 0,
            auto_ping_delay: true,
        });

        apply_action_to_session(
            &mut session,
            LocalAction::QueueAdd(crate::online::SharedQueueItem {
                path: PathBuf::from("a.flac"),
                title: String::from("a"),
                delivery: crate::online::QueueDelivery::HostStreamOnly,
                owner_nickname: Some(String::from("listener")),
            }),
            "listener",
        );

        assert!(session.shared_queue.is_empty());
    }

    #[test]
    fn host_only_allows_listener_delay_update_network_action() {
        let mut session = OnlineSession::host("host");
        session.mode = crate::online::OnlineRoomMode::HostOnly;
        session.participants.push(crate::online::Participant {
            nickname: String::from("listener"),
            is_local: false,
            is_host: false,
            ping_ms: 12,
            manual_extra_delay_ms: 0,
            auto_ping_delay: true,
        });

        apply_action_to_session(
            &mut session,
            LocalAction::DelayUpdate {
                manual_extra_delay_ms: 75,
                auto_ping_delay: false,
            },
            "listener",
        );

        let listener = session
            .participants
            .iter()
            .find(|participant| participant.nickname == "listener")
            .expect("listener participant");
        assert_eq!(listener.manual_extra_delay_ms, 75);
        assert!(!listener.auto_ping_delay);
    }

    #[test]
    fn nickname_update_renames_participant_and_owned_queue_items() {
        let mut session = OnlineSession::host("host");
        session.shared_queue.push(crate::online::SharedQueueItem {
            path: PathBuf::from("a.flac"),
            title: String::from("a"),
            delivery: crate::online::QueueDelivery::HostStreamOnly,
            owner_nickname: Some(String::from("host")),
        });

        apply_action_to_session(
            &mut session,
            LocalAction::SetNickname {
                nickname: String::from("dj"),
            },
            "host",
        );

        assert_eq!(session.participants[0].nickname, "dj");
        assert_eq!(
            session.shared_queue[0].owner_nickname.as_deref(),
            Some("dj")
        );
    }

    #[test]
    fn host_only_allows_listener_nickname_update() {
        let mut session = OnlineSession::host("host");
        session.mode = crate::online::OnlineRoomMode::HostOnly;
        session.participants.push(crate::online::Participant {
            nickname: String::from("listener"),
            is_local: false,
            is_host: false,
            ping_ms: 12,
            manual_extra_delay_ms: 0,
            auto_ping_delay: true,
        });

        apply_action_to_session(
            &mut session,
            LocalAction::SetNickname {
                nickname: String::from("listener2"),
            },
            "listener",
        );

        assert!(
            session
                .participants
                .iter()
                .any(|participant| participant.nickname == "listener2")
        );
    }

    #[test]
    fn validate_stream_source_rejects_missing_path() {
        let result = validate_stream_source(Path::new("does_not_exist.flac"));
        assert!(result.is_err());
    }

    #[test]
    fn ping_wire_messages_round_trip() {
        let ping = WireServerMessage::Ping { nonce: 123 };
        let encoded_ping = serde_json::to_string(&ping).expect("serialize ping");
        let decoded_ping: WireServerMessage =
            serde_json::from_str(&encoded_ping).expect("deserialize ping");
        assert!(matches!(
            decoded_ping,
            WireServerMessage::Ping { nonce: 123 }
        ));

        let pong = WireClientMessage::Pong { nonce: 123 };
        let encoded_pong = serde_json::to_string(&pong).expect("serialize pong");
        let decoded_pong: WireClientMessage =
            serde_json::from_str(&encoded_pong).expect("deserialize pong");
        assert!(matches!(
            decoded_pong,
            WireClientMessage::Pong { nonce: 123 }
        ));
    }

    #[test]
    fn smooth_ping_prefers_recent_history() {
        assert_eq!(smooth_ping(0, 38), 38);
        assert_eq!(smooth_ping(100, 20), 80);
    }

    #[test]
    fn disconnect_peer_removes_matching_participant_case_insensitive() {
        let mut session = OnlineSession::host("host");
        session.participants.push(crate::online::Participant {
            nickname: String::from("ListenerA"),
            is_local: false,
            is_host: false,
            ping_ms: 25,
            manual_extra_delay_ms: 0,
            auto_ping_delay: true,
        });

        let listener = TcpListener::bind("127.0.0.1:0").expect("bind listener");
        let addr = listener.local_addr().expect("listener addr");
        let client_stream = TcpStream::connect(addr).expect("connect client stream");
        let (server_stream, _) = listener.accept().expect("accept server stream");

        let mut peers = HashMap::new();
        peers.insert(
            9,
            PeerConnection {
                nickname: String::from("listenera"),
                writer: Arc::new(Mutex::new(server_stream)),
            },
        );
        drop(client_stream);

        let mut pending_pull_requests = HashMap::new();
        let mut pending_relay_requests = HashMap::new();
        let mut inbound_streams = HashMap::new();
        let mut pending_pings = HashMap::new();
        pending_pings.insert(
            9,
            PendingPing {
                nonce: 1,
                sent_at: Instant::now(),
            },
        );
        let (event_tx, event_rx) = mpsc::channel();

        disconnect_peer(
            9,
            &mut session,
            &mut InboundState {
                peers: &mut peers,
                pending_pull_requests: &mut pending_pull_requests,
                pending_relay_requests: &mut pending_relay_requests,
                inbound_streams: &mut inbound_streams,
                pending_pings: &mut pending_pings,
            },
            "Peer disconnected",
            &event_tx,
        );

        assert!(
            !session
                .participants
                .iter()
                .any(|participant| participant.nickname.eq_ignore_ascii_case("listenera"))
        );
        assert!(peers.is_empty());
        assert!(pending_pings.is_empty());

        let statuses: Vec<String> = event_rx
            .try_iter()
            .filter_map(|event| match event {
                NetworkEvent::Status(message) => Some(message),
                _ => None,
            })
            .collect();
        assert!(
            statuses
                .iter()
                .any(|line| line.contains("Peer disconnected: listenera"))
        );
    }

    #[test]
    fn disconnecting_host_promotes_earliest_joined_participant() {
        let mut session = OnlineSession::host("host");
        session.participants.push(crate::online::Participant {
            nickname: String::from("alpha"),
            is_local: false,
            is_host: false,
            ping_ms: 20,
            manual_extra_delay_ms: 0,
            auto_ping_delay: true,
        });
        session.participants.push(crate::online::Participant {
            nickname: String::from("beta"),
            is_local: false,
            is_host: false,
            ping_ms: 22,
            manual_extra_delay_ms: 0,
            auto_ping_delay: true,
        });

        let listener = TcpListener::bind("127.0.0.1:0").expect("bind listener");
        let addr = listener.local_addr().expect("listener addr");
        let client_stream = TcpStream::connect(addr).expect("connect client stream");
        let (server_stream, _) = listener.accept().expect("accept server stream");

        let mut peers = HashMap::new();
        peers.insert(
            1,
            PeerConnection {
                nickname: String::from("HOST"),
                writer: Arc::new(Mutex::new(server_stream)),
            },
        );
        drop(client_stream);

        let mut pending_pull_requests = HashMap::new();
        let mut pending_relay_requests = HashMap::new();
        let mut inbound_streams = HashMap::new();
        let mut pending_pings = HashMap::new();
        let (event_tx, event_rx) = mpsc::channel();

        disconnect_peer(
            1,
            &mut session,
            &mut InboundState {
                peers: &mut peers,
                pending_pull_requests: &mut pending_pull_requests,
                pending_relay_requests: &mut pending_relay_requests,
                inbound_streams: &mut inbound_streams,
                pending_pings: &mut pending_pings,
            },
            "Peer disconnected",
            &event_tx,
        );

        assert_eq!(session.participants.len(), 2);
        assert_eq!(session.participants[0].nickname, "alpha");
        assert!(session.participants[0].is_host);
        assert!(!session.participants[1].is_host);

        let statuses: Vec<String> = event_rx
            .try_iter()
            .filter_map(|event| match event {
                NetworkEvent::Status(message) => Some(message),
                _ => None,
            })
            .collect();
        assert!(
            statuses
                .iter()
                .any(|line| line.contains("Host left room. New host: alpha"))
        );
    }

    #[test]
    fn home_server_created_room_accepts_local_client_join() {
        let probe = TcpListener::bind("127.0.0.1:0").expect("bind probe port");
        let port = probe.local_addr().expect("probe addr").port();
        drop(probe);

        let home_addr = format!("127.0.0.1:{port}");
        let handle = start_home_server(&home_addr, None).expect("start home server");

        verify_home_server(&home_addr).expect("verify home server");
        let room =
            create_home_room(&home_addr, "roomname", "hoster", None, 8).expect("create room");
        let client =
            OnlineNetwork::start_client(&room.room_server_addr, &room.room_code, "hoster", None)
                .expect("join created room");

        client.shutdown();
        handle.shutdown();
    }

    #[test]
    fn home_server_created_room_client_stays_connected_briefly() {
        let probe = TcpListener::bind("127.0.0.1:0").expect("bind probe port");
        let port = probe.local_addr().expect("probe addr").port();
        drop(probe);

        let home_addr = format!("127.0.0.1:{port}");
        let handle = start_home_server(&home_addr, None).expect("start home server");
        verify_home_server(&home_addr).expect("verify home server");
        let room =
            create_home_room(&home_addr, "roomname", "hoster", None, 8).expect("create room");
        let client =
            OnlineNetwork::start_client(&room.room_server_addr, &room.room_code, "hoster", None)
                .expect("join created room");

        thread::sleep(Duration::from_millis(200));
        let statuses: Vec<String> = std::iter::from_fn(|| client.try_recv_event())
            .filter_map(|event| match event {
                NetworkEvent::Status(message) => Some(message),
                _ => None,
            })
            .collect();
        assert!(
            !statuses
                .iter()
                .any(|message| message.contains("Disconnected from online host")),
            "unexpected disconnect statuses: {statuses:?}"
        );

        client.shutdown();
        handle.shutdown();
    }

    #[test]
    fn resolve_server_addrs_parses_socket_addr() {
        let addrs = resolve_server_addrs("127.0.0.1:7878").expect("resolve");
        assert_eq!(addrs.len(), 1);
        assert_eq!(addrs[0], "127.0.0.1:7878".parse::<SocketAddr>().unwrap());
    }

    #[test]
    fn resolve_server_addrs_rejects_missing_port() {
        assert!(resolve_server_addrs("127.0.0.1").is_err());
    }

    #[test]
    fn direct_host_client_same_nickname_stays_connected_briefly() {
        let mut session = OnlineSession::host("hoster");
        session.room_code = String::from("ROOM");
        session.participants.clear();
        let host = OnlineNetwork::start_host_with_max("127.0.0.1:0", session, None, 8)
            .expect("start direct host");
        let host_addr = host.bind_addr().expect("host addr").to_string();

        let client = OnlineNetwork::start_client(&host_addr, "ROOM", "hoster", None)
            .expect("join direct host");
        thread::sleep(Duration::from_millis(2200));

        let statuses: Vec<String> = std::iter::from_fn(|| client.try_recv_event())
            .filter_map(|event| match event {
                NetworkEvent::Status(message) => Some(message),
                _ => None,
            })
            .collect();
        assert!(
            !statuses
                .iter()
                .any(|message| message.contains("Disconnected from online host")),
            "unexpected disconnect statuses: {statuses:?}"
        );

        client.shutdown();
        host.shutdown();
    }
}
