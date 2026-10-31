#![cfg(feature = "std")]

use std::{
    net::{Ipv4Addr, SocketAddr, SocketAddrV4, UdpSocket},
    thread,
    time::Duration,
};

use rasta_rust::{
    codec::{
        safety_code::{Md4InitialValue, SafetyCodeLength},
        sr::{ProtocolVersion, SrCodecConfig},
    },
    engine::{
        buffers::ApplicationPayload,
        connection::{ConnectionConfig, ConnectionEvent, ConnectionState, StepInput},
        diagnostics::DiagnosticsConfig,
    },
    redundancy::{
        codec::{
            CheckCodeType, RedundancyFrame, MAX_REDUNDANCY_FRAME_LEN, MAX_REDUNDANCY_PAYLOAD_LEN,
        },
        engine::RedundancyConfig,
    },
    stack::{StackAction, StackActions, StackCodecConfig, StackEngine},
    transport::{
        simulation::{DeterministicNetwork, NextFault},
        udp::{UdpAdapterError, UdpChannel, UdpRedundancyAdapter},
    },
};

fn loopback() -> SocketAddr {
    SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 0))
}

/// A diagnostic window of 5000 messages, with timing intervals in fifths of
/// `T_max` (the SBB reference values at 750 ms).
fn diagnostics(t_max: u32) -> DiagnosticsConfig {
    DiagnosticsConfig::new(
        5_000,
        [t_max / 5, 2 * t_max / 5, 3 * t_max / 5, 4 * t_max / 5],
    )
    .unwrap()
}

/// Binds the two ends of one network path.
fn udp_path() -> (UdpChannel, UdpChannel) {
    let first = UdpSocket::bind(loopback()).unwrap();
    let second = UdpSocket::bind(loopback()).unwrap();
    let (first_addr, second_addr) = (first.local_addr().unwrap(), second.local_addr().unwrap());
    (
        UdpChannel::from_socket(first, second_addr).unwrap(),
        UdpChannel::from_socket(second, first_addr).unwrap(),
    )
}

fn stack(local: u32, remote: u32, channels: u8) -> StackEngine {
    StackEngine::new(
        ConnectionConfig::new(
            local,
            remote,
            ProtocolVersion::new(*b"0303").unwrap(),
            10,
            1_000,
            diagnostics(1_000),
        )
        .unwrap(),
        RedundancyConfig::new(50, 4, channels, 200).unwrap(),
        StackCodecConfig {
            sr: SrCodecConfig::with_default_limits(SafetyCodeLength::LowerMd4),
            md4_initial_value: Md4InitialValue::STANDARD,
            redundancy_check_code: CheckCodeType::C,
        },
    )
}

fn send_all(adapter: &UdpRedundancyAdapter, actions: &StackActions) {
    for action in actions.iter() {
        if let StackAction::Transmit(datagram) = action {
            adapter.send_datagram(datagram).unwrap();
        }
    }
}

fn service(engine: &mut StackEngine, adapter: &UdpRedundancyAdapter, now: u32) {
    let mut buffer = [0; MAX_REDUNDANCY_FRAME_LEN];
    for channel in 0..adapter.channel_count() {
        while engine.can_receive_datagram() {
            let Some(datagram) = adapter.receive_datagram(channel, &mut buffer).unwrap() else {
                break;
            };
            send_all(
                adapter,
                &engine.receive_datagram(now, channel, datagram).unwrap(),
            );
        }
    }
    send_all(adapter, &engine.poll(now).unwrap());
}

#[test]
fn stack_engines_connect_and_exchange_data_through_the_udp_adapter() {
    let (client_first, server_first) = udp_path();
    let (client_second, server_second) = udp_path();
    let client_adapter =
        UdpRedundancyAdapter::new(client_first, Some(client_second), CheckCodeType::C);
    let server_adapter =
        UdpRedundancyAdapter::new(server_first, Some(server_second), CheckCodeType::C);
    let mut client = stack(1, 2, 2);
    let mut server = stack(2, 1, 2);
    let open = |isn| StepInput {
        now: 0,
        event: ConnectionEvent::Open {
            initial_sequence_number: isn,
        },
    };
    send_all(&server_adapter, &server.step_connection(open(100)).unwrap());
    send_all(&client_adapter, &client.step_connection(open(200)).unwrap());

    // Protocol time advances one millisecond per round, independent of how
    // fast the host runs.
    let payload = ApplicationPayload::new(b"over UDP").unwrap();
    let mut sent = false;
    let mut received = None;
    for now in 1..1_000 {
        service(&mut client, &client_adapter, now);
        service(&mut server, &server_adapter, now);
        if !sent && server.connection_snapshot().state == ConnectionState::Up {
            let actions = client
                .step_connection(StepInput {
                    now,
                    event: ConnectionEvent::SendData(payload),
                })
                .unwrap();
            send_all(&client_adapter, &actions);
            sent = true;
        }
        received = server.pop_received();
        if received.is_some() {
            break;
        }
        thread::sleep(Duration::from_millis(1));
    }

    assert_eq!(received, Some(payload));
    // Every frame travels both paths; the second copy is discarded.
    assert!(server.redundancy_snapshot().diagnostics.duplicates > 0);
}

#[test]
fn receive_datagram_returns_raw_bytes_without_decoding() {
    let peer = UdpSocket::bind(loopback()).unwrap();
    let socket = UdpSocket::bind(loopback()).unwrap();
    let address = socket.local_addr().unwrap();
    let receiver = UdpRedundancyAdapter::new(
        UdpChannel::from_socket(socket, peer.local_addr().unwrap()).unwrap(),
        None,
        CheckCodeType::C,
    );
    let mut wire = [0; MAX_REDUNDANCY_FRAME_LEN];
    let length = RedundancyFrame {
        sequence_number: 3,
        payload: &[42; 28],
    }
    .encode_into(CheckCodeType::C, &mut wire)
    .unwrap();
    // A corrupted frame is passed through; checking it is the stack's job.
    wire[length - 1] ^= 0xff;
    let mut output = [0; MAX_REDUNDANCY_FRAME_LEN];
    assert_eq!(receiver.receive_datagram(0, &mut output).unwrap(), None);

    peer.send_to(&wire[..length], address).unwrap();
    let mut bytes = None;
    for _ in 0..50 {
        if let Some(datagram) = receiver.receive_datagram(0, &mut output).unwrap() {
            bytes = Some(datagram.to_vec());
            break;
        }
        thread::sleep(Duration::from_millis(1));
    }
    assert_eq!(bytes.as_deref(), Some(&wire[..length]));

    // The datagram size limit still applies.
    peer.send_to(&[0; MAX_REDUNDANCY_FRAME_LEN + 1], address)
        .unwrap();
    for _ in 0..50 {
        match receiver.receive_datagram(0, &mut output) {
            Err(UdpAdapterError::DatagramTooLarge(length)) => {
                assert_eq!(length, MAX_REDUNDANCY_FRAME_LEN + 1);
                return;
            }
            Ok(None) => thread::sleep(Duration::from_millis(1)),
            other => panic!("expected oversize rejection, got {other:?}"),
        }
    }
    panic!("oversize datagram did not arrive");
}

#[test]
fn send_datagram_rejects_a_channel_the_adapter_lacks() {
    let (local, _peer) = udp_path();
    let adapter = UdpRedundancyAdapter::new(local, None, CheckCodeType::C);
    let mut client = stack(1, 2, 2);
    let actions = client
        .step_connection(StepInput {
            now: 0,
            event: ConnectionEvent::Open {
                initial_sequence_number: 1,
            },
        })
        .unwrap();
    let second_path = actions
        .iter()
        .find_map(|action| match action {
            StackAction::Transmit(datagram) if datagram.channel() == 1 => Some(*datagram),
            _ => None,
        })
        .expect("connection request on the second path");

    assert!(matches!(
        adapter.send_datagram(&second_path),
        Err(UdpAdapterError::InvalidChannel(1))
    ));
}

#[test]
fn a_reset_after_sending_to_a_closed_port_is_skipped() {
    let closed = UdpSocket::bind(loopback()).unwrap();
    let closed_address = closed.local_addr().unwrap();
    drop(closed);
    let adapter = UdpRedundancyAdapter::new(
        UdpChannel::bind(loopback(), closed_address).unwrap(),
        None,
        CheckCodeType::C,
    );
    // Windows reports the ICMP port-unreachable reply as a reset on the next
    // receive; other platforms report nothing on an unconnected socket.
    adapter.transmit(0, 0, &[0; 28]).unwrap();
    thread::sleep(Duration::from_millis(20));
    let mut output = [0; MAX_REDUNDANCY_FRAME_LEN];
    assert_eq!(adapter.receive_datagram(0, &mut output).unwrap(), None);
}

#[test]
fn udp_adapter_preserves_one_complete_redundancy_datagram() {
    let left_socket = UdpSocket::bind(loopback()).unwrap();
    let right_socket = UdpSocket::bind(loopback()).unwrap();
    let left_addr = left_socket.local_addr().unwrap();
    let right_addr = right_socket.local_addr().unwrap();
    let left = UdpChannel::from_socket(left_socket, right_addr).unwrap();
    let right = UdpChannel::from_socket(right_socket, left_addr).unwrap();
    let sender = UdpRedundancyAdapter::new(left, None, CheckCodeType::C);
    let receiver = UdpRedundancyAdapter::new(right, None, CheckCodeType::C);
    let payload = [0x5a; 28];

    sender.transmit(0, 7, &payload).unwrap();
    let mut buffer = [0_u8; MAX_REDUNDANCY_FRAME_LEN];
    let mut decoded = None;
    for _ in 0..50 {
        if let Some(frame) = receiver.receive(0, &mut buffer).unwrap() {
            decoded = Some((frame.sequence_number, frame.payload.to_vec()));
            break;
        }
        thread::sleep(Duration::from_millis(1));
    }

    let (sequence_number, received_payload) = decoded.expect("loopback datagram arrives");
    assert_eq!(sequence_number, 7);
    assert_eq!(received_payload, payload);
}

#[test]
fn udp_adapter_rejects_datagrams_larger_than_the_rl_maximum() {
    let sender_socket = UdpSocket::bind(loopback()).unwrap();
    let raw_sender = sender_socket.try_clone().unwrap();
    let receiver_socket = UdpSocket::bind(loopback()).unwrap();
    let sender_addr = sender_socket.local_addr().unwrap();
    let receiver_addr = receiver_socket.local_addr().unwrap();
    let receiver = UdpRedundancyAdapter::new(
        UdpChannel::from_socket(receiver_socket, sender_addr).unwrap(),
        None,
        CheckCodeType::A,
    );
    let payload = [0_u8; MAX_REDUNDANCY_PAYLOAD_LEN];
    let mut oversized = [0_u8; MAX_REDUNDANCY_FRAME_LEN + 1];
    RedundancyFrame {
        sequence_number: 0,
        payload: &payload,
    }
    .encode_into(CheckCodeType::A, &mut oversized)
    .unwrap();
    raw_sender.send_to(&oversized, receiver_addr).unwrap();

    let mut output = [0_u8; MAX_REDUNDANCY_FRAME_LEN];
    for _ in 0..50 {
        match receiver.receive(0, &mut output) {
            Err(UdpAdapterError::DatagramTooLarge(length)) => {
                assert_eq!(length, MAX_REDUNDANCY_FRAME_LEN + 1);
                return;
            }
            Ok(None) => thread::sleep(Duration::from_millis(1)),
            other => panic!("expected oversize rejection, got {other:?}"),
        }
    }
    panic!("oversize datagram did not arrive");
}

#[test]
fn deterministic_network_injects_loss_duplication_corruption_delay_and_reordering() {
    let mut network = DeterministicNetwork::new(16).unwrap();
    network.inject(0, NextFault::Drop).unwrap();
    network.send(0, 1, 0, 0, b"drop").unwrap();
    assert!(network.is_empty());

    network.inject(0, NextFault::Duplicate).unwrap();
    network.send(0, 1, 0, 0, b"dupe").unwrap();
    assert_eq!(network.receive(1, 0).unwrap().bytes, b"dupe");
    assert_eq!(network.receive(1, 0).unwrap().bytes, b"dupe");

    network.inject(0, NextFault::Corrupt).unwrap();
    network.send(0, 1, 0, 0, b"crc").unwrap();
    assert_ne!(network.receive(1, 0).unwrap().bytes, b"crc");

    network.inject(0, NextFault::Delay(10)).unwrap();
    network.send(0, 1, 0, 0, b"late").unwrap();
    assert!(network.receive(1, 9).is_none());
    assert_eq!(network.receive(1, 10).unwrap().bytes, b"late");

    network.inject(0, NextFault::Reorder).unwrap();
    network.send(0, 1, 0, 20, b"first").unwrap();
    network.send(0, 1, 0, 20, b"second").unwrap();
    assert_eq!(network.receive(1, 20).unwrap().bytes, b"second");
    assert_eq!(network.receive(1, 20).unwrap().bytes, b"first");
}

#[test]
fn deterministic_network_models_asymmetric_outage_recovery_restart_and_stale_packets() {
    let mut network = DeterministicNetwork::new(16).unwrap();
    network.set_link_available(0, 1, 0, false).unwrap();
    network.send(0, 1, 0, 0, b"outbound-drop").unwrap();
    network.send(1, 0, 0, 0, b"reverse-works").unwrap();
    assert_eq!(network.receive(0, 0).unwrap().bytes, b"reverse-works");
    assert!(network.receive(1, 0).is_none());

    network.set_link_available(0, 1, 0, true).unwrap();
    network.send(0, 1, 0, 1, b"recovered").unwrap();
    assert_eq!(network.receive(1, 1).unwrap().bytes, b"recovered");

    network.inject(1, NextFault::Delay(100)).unwrap();
    network.send(0, 1, 1, 2, b"stale-after-restart").unwrap();
    network.restart_endpoint(1).unwrap();
    assert!(network.receive(1, 102).is_none());
}

#[test]
fn udp_skips_unconfigured_peers_and_bounds_noise_processing() {
    let peer = UdpSocket::bind(loopback()).unwrap();
    let noise = UdpSocket::bind(loopback()).unwrap();
    let socket = UdpSocket::bind(loopback()).unwrap();
    let address = socket.local_addr().unwrap();
    let receiver = UdpRedundancyAdapter::new(
        UdpChannel::from_socket(socket, peer.local_addr().unwrap()).unwrap(),
        None,
        CheckCodeType::C,
    );
    let mut wire = [0; MAX_REDUNDANCY_FRAME_LEN];
    let length = RedundancyFrame {
        sequence_number: 7,
        payload: &[42; 28],
    }
    .encode_into(CheckCodeType::C, &mut wire)
    .unwrap();
    let mut output = [0; MAX_REDUNDANCY_FRAME_LEN];
    assert!(receiver.receive(0, &mut output).unwrap().is_none());
    noise.send_to(b"unconfigured", address).unwrap();
    peer.send_to(&wire[..length], address).unwrap();
    let mut delivered = false;
    for _ in 0..50 {
        if let Some(frame) = receiver.receive(0, &mut output).unwrap() {
            assert_eq!(frame.sequence_number, 7);
            assert_eq!(frame.payload, &[42; 28]);
            delivered = true;
            break;
        }
        thread::sleep(Duration::from_millis(1));
    }
    assert!(delivered, "legitimate packet after unexpected peer");
    // A full work budget of noise yields without exposing it to the codec.
    for _ in 0..32 {
        noise.send_to(b"noise", address).unwrap();
    }
    peer.send_to(&wire[..length], address).unwrap();
    thread::sleep(Duration::from_millis(5));
    assert!(receiver.receive(0, &mut output).unwrap().is_none());
    let frame = receiver
        .receive(0, &mut output)
        .unwrap()
        .expect("legitimate packet after noise budget");
    assert_eq!(frame.sequence_number, 7);
    assert!(receiver.receive(0, &mut output).unwrap().is_none());
}
