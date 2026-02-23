use std::fs;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};

use tempfile::tempdir;
use tune::online::{
    OnlineSession, QueueDelivery, SharedQueueItem, TransportCommand, TransportEnvelope,
};
use tune::online_net::{LocalAction, NetworkEvent, OnlineNetwork};

fn start_host_and_two_clients() -> (OnlineNetwork, OnlineNetwork, OnlineNetwork) {
    let host_session = OnlineSession::host("host");
    let room_code = host_session.room_code.clone();
    let host = OnlineNetwork::start_host("127.0.0.1:0", host_session, None).expect("start host");
    let host_addr = host.bind_addr().expect("host bind addr").to_string();

    let source_client =
        OnlineNetwork::start_client(&host_addr, &room_code, "alice", None).expect("join source");
    let listener_client =
        OnlineNetwork::start_client(&host_addr, &room_code, "bob", None).expect("join listener");
    (host, source_client, listener_client)
}

fn wait_for_session_queue_path(network: &OnlineNetwork, path: &Path, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if let Some(NetworkEvent::SessionSync(session)) = network.try_recv_event()
            && session.shared_queue.iter().any(|item| item.path == path)
        {
            return true;
        }
        thread::sleep(Duration::from_millis(10));
    }
    false
}

fn wait_for_stream_ready(
    network: &OnlineNetwork,
    requested_path: &Path,
    timeout: Duration,
) -> Option<PathBuf> {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if let Some(NetworkEvent::StreamTrackReady {
            requested_path: ready_path,
            local_temp_path,
            ..
        }) = network.try_recv_event()
            && ready_path == requested_path
        {
            return Some(local_temp_path);
        }
        thread::sleep(Duration::from_millis(10));
    }
    None
}

#[test]
fn remote_clients_stream_via_host_relay_without_direct_peer_links() {
    let temp = tempdir().expect("tempdir");
    let source_path = temp.path().join("relay-source.bin");
    let source_bytes = b"relay-stream-test-payload";
    fs::write(&source_path, source_bytes).expect("write source");

    let (host, source_client, listener_client) = start_host_and_two_clients();

    host.send_local_action(LocalAction::QueueAdd(SharedQueueItem {
        path: source_path.clone(),
        title: String::from("relay-source"),
        delivery: QueueDelivery::HostStreamOnly,
        owner_nickname: Some(String::from("alice")),
    }));

    assert!(
        wait_for_session_queue_path(&source_client, &source_path, Duration::from_secs(3)),
        "source client did not receive shared queue ownership update"
    );

    listener_client.request_track_stream(source_path.clone(), Some(String::from("alice")));

    let streamed_path =
        wait_for_stream_ready(&listener_client, &source_path, Duration::from_secs(5))
            .expect("listener did not receive relayed stream");
    let streamed_bytes = fs::read(&streamed_path).expect("read streamed cache");
    assert_eq!(streamed_bytes, source_bytes);

    listener_client.shutdown();
    source_client.shutdown();
    host.shutdown();

    let _ = fs::remove_file(streamed_path);
}

#[test]
fn host_streams_local_track_to_remote_client() {
    let temp = tempdir().expect("tempdir");
    let source_path = temp.path().join("host-source.bin");
    let source_bytes = b"host-to-remote-payload";
    fs::write(&source_path, source_bytes).expect("write source");

    let (host, source_client, listener_client) = start_host_and_two_clients();

    listener_client.request_track_stream(source_path.clone(), None);

    let streamed_path =
        wait_for_stream_ready(&listener_client, &source_path, Duration::from_secs(5))
            .expect("listener did not receive host stream");
    let streamed_bytes = fs::read(&streamed_path).expect("read streamed cache");
    assert_eq!(streamed_bytes, source_bytes);

    listener_client.shutdown();
    source_client.shutdown();
    host.shutdown();

    let _ = fs::remove_file(streamed_path);
}

#[test]
fn host_can_pull_track_from_remote_client_owner() {
    let temp = tempdir().expect("tempdir");
    let source_path = temp.path().join("remote-source.bin");
    let source_bytes = b"remote-to-host-payload";
    fs::write(&source_path, source_bytes).expect("write source");

    let (host, source_client, listener_client) = start_host_and_two_clients();

    host.send_local_action(LocalAction::QueueAdd(SharedQueueItem {
        path: source_path.clone(),
        title: String::from("remote-source"),
        delivery: QueueDelivery::HostStreamOnly,
        owner_nickname: Some(String::from("alice")),
    }));

    assert!(
        wait_for_session_queue_path(&source_client, &source_path, Duration::from_secs(3)),
        "source client did not receive shared queue ownership update"
    );

    host.request_track_stream(source_path.clone(), Some(String::from("alice")));

    let streamed_path = wait_for_stream_ready(&host, &source_path, Duration::from_secs(5))
        .expect("host did not receive pulled stream");
    let streamed_bytes = fs::read(&streamed_path).expect("read streamed cache");
    assert_eq!(streamed_bytes, source_bytes);

    listener_client.shutdown();
    source_client.shutdown();
    host.shutdown();

    let _ = fs::remove_file(streamed_path);
}

#[test]
fn remote_transport_origin_can_stream_without_shared_queue_entry() {
    let temp = tempdir().expect("tempdir");
    let source_path = temp.path().join("transport-origin-source.bin");
    let source_bytes = b"transport-origin-stream-payload";
    fs::write(&source_path, source_bytes).expect("write source");

    let (host, source_client, listener_client) = start_host_and_two_clients();

    source_client.send_local_action(LocalAction::Transport(TransportEnvelope {
        seq: 0,
        origin_nickname: String::from("alice"),
        command: TransportCommand::PlayTrack {
            path: source_path.clone(),
            title: None,
            artist: None,
            album: None,
            provider_track_id: None,
        },
    }));

    thread::sleep(Duration::from_millis(100));
    listener_client.request_track_stream(source_path.clone(), Some(String::from("alice")));

    let streamed_path =
        wait_for_stream_ready(&listener_client, &source_path, Duration::from_secs(5))
            .expect("listener did not receive stream from transport origin");
    let streamed_bytes = fs::read(&streamed_path).expect("read streamed cache");
    assert_eq!(streamed_bytes, source_bytes);

    listener_client.shutdown();
    source_client.shutdown();
    host.shutdown();

    let _ = fs::remove_file(streamed_path);
}
