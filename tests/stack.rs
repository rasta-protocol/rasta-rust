use std::collections::VecDeque;

use rasta_rust::{
    codec::{
        safety_code::{Md4InitialValue, SafetyCodeLength},
        sr::{DisconnectReason, ProtocolVersion, SrCodecConfig},
    },
    engine::{
        buffers::{ApplicationData, ApplicationPayload},
        connection::{ConnectionConfig, ConnectionEvent, ConnectionState, StepInput},
        diagnostics::DiagnosticsConfig,
        timing::TimingConfig,
    },
    redundancy::{
        codec::CheckCodeType,
        engine::{RedundancyConfig, RedundancyState},
    },
    stack::{
        StackAction, StackActions, StackCodecConfig, StackEngine, StackError, WireDatagram,
        RECEIVE_QUEUE_CAPACITY,
    },
};

/// A diagnostic window of 5000 messages, with timing intervals in fifths of
/// `T_max` (the SBB reference values at 750 ms).
const fn diagnostics(t_max: u32) -> DiagnosticsConfig {
    match DiagnosticsConfig::new(
        5_000,
        [t_max / 5, 2 * t_max / 5, 3 * t_max / 5, 4 * t_max / 5],
    ) {
        Ok(config) => config,
        Err(_) => panic!("invalid diagnostics"),
    }
}

fn connection(local: u32, remote: u32) -> ConnectionConfig {
    ConnectionConfig::new(
        local,
        remote,
        ProtocolVersion::new(*b"0303").unwrap(),
        10,
        100,
        diagnostics(100),
    )
    .unwrap()
}

fn stack(local: u32, remote: u32) -> StackEngine {
    StackEngine::new(
        connection(local, remote),
        RedundancyConfig::new(50, 4, 2, 200).unwrap(),
        StackCodecConfig {
            sr: SrCodecConfig::with_default_limits(SafetyCodeLength::LowerMd4),
            md4_initial_value: Md4InitialValue::STANDARD,
            redundancy_check_code: CheckCodeType::C,
        },
    )
}

fn stack_with_immediate_ack(local: u32, remote: u32) -> StackEngine {
    stack_with_max_packet(local, remote, 1)
}

fn stack_with_max_packet(local: u32, remote: u32, max_packet: u16) -> StackEngine {
    StackEngine::new(
        ConnectionConfig::new_with_timing(
            local,
            remote,
            ProtocolVersion::new(*b"0303").unwrap(),
            20,
            TimingConfig::new(1_000, 500, 10, 1, 20).unwrap(),
            diagnostics(1_000),
        )
        .unwrap()
        .with_max_packet(max_packet)
        .unwrap(),
        RedundancyConfig::new(50, 10, 2, 200).unwrap(),
        StackCodecConfig {
            sr: SrCodecConfig::with_default_limits(SafetyCodeLength::LowerMd4),
            md4_initial_value: Md4InitialValue::STANDARD,
            redundancy_check_code: CheckCodeType::C,
        },
    )
}

fn datagram(actions: &StackActions, channel: u8) -> WireDatagram {
    actions
        .iter()
        .find_map(|action| match action {
            StackAction::Transmit(datagram) if datagram.channel() == channel => Some(*datagram),
            _ => None,
        })
        .expect("expected transport datagram")
}

#[test]
fn two_composed_nodes_handshake_and_deliver_data_over_redundant_paths() {
    let mut client = stack(1, 2);
    let mut server = stack(2, 1);

    let server_open = server
        .step_connection(StepInput {
            now: 900,
            event: ConnectionEvent::Open {
                initial_sequence_number: 100,
            },
        })
        .unwrap();
    assert!(server_open.iter().any(|action| matches!(
        action,
        StackAction::RedundancyStateChanged {
            current: RedundancyState::Up,
            ..
        }
    )));

    let client_open = client
        .step_connection(StepInput {
            now: 1_000,
            event: ConnectionEvent::Open {
                initial_sequence_number: u32::MAX - 1,
            },
        })
        .unwrap();
    let request = datagram(&client_open, 0);
    let server_response = server
        .receive_datagram(1_005, 0, request.as_slice())
        .unwrap();
    let response = datagram(&server_response, 0);
    let client_heartbeat = client
        .receive_datagram(1_010, 0, response.as_slice())
        .unwrap();
    let heartbeat = datagram(&client_heartbeat, 0);
    server
        .receive_datagram(1_015, 0, heartbeat.as_slice())
        .unwrap();

    assert_eq!(client.connection_snapshot().state, ConnectionState::Up);
    assert_eq!(server.connection_snapshot().state, ConnectionState::Up);

    let payload = ApplicationPayload::new(b"payload").unwrap();
    let sent = client
        .step_connection(StepInput {
            now: 1_020,
            event: ConnectionEvent::SendData(payload),
        })
        .unwrap();
    let channel_zero = datagram(&sent, 0);
    let channel_one = datagram(&sent, 1);
    let delivered = server
        .receive_datagram(1_025, 0, channel_zero.as_slice())
        .unwrap();
    assert!(delivered
        .iter()
        .any(|action| matches!(action, StackAction::ApplicationDataAvailable)));
    assert_eq!(server.pop_received(), Some(payload));

    let duplicate = server
        .receive_datagram(1_026, 1, channel_one.as_slice())
        .unwrap();
    assert!(duplicate.is_empty());
}

#[test]
fn malformed_redundancy_frame_is_rejected_atomically() {
    let mut server = stack(2, 1);
    server
        .step_connection(StepInput {
            now: 0,
            event: ConnectionEvent::Open {
                initial_sequence_number: 10,
            },
        })
        .unwrap();
    let before_connection = server.connection_snapshot();
    let before_redundancy = server.redundancy_snapshot();

    assert!(server.receive_datagram(1, 0, &[0; 28]).is_err());
    assert_eq!(server.connection_snapshot(), before_connection);
    assert_eq!(server.redundancy_snapshot(), before_redundancy);
}

#[test]
fn maximum_defer_flush_can_emit_all_required_acknowledgements() {
    let mut client = stack_with_immediate_ack(1, 2);
    let mut server = stack_with_immediate_ack(2, 1);
    server
        .step_connection(StepInput {
            now: 0,
            event: ConnectionEvent::Open {
                initial_sequence_number: 100,
            },
        })
        .unwrap();
    let request = client
        .step_connection(StepInput {
            now: 1,
            event: ConnectionEvent::Open {
                initial_sequence_number: 200,
            },
        })
        .unwrap();
    let response = server
        .receive_datagram(2, 0, datagram(&request, 0).as_slice())
        .unwrap();
    let heartbeat = client
        .receive_datagram(3, 0, datagram(&response, 0).as_slice())
        .unwrap();
    server
        .receive_datagram(4, 0, datagram(&heartbeat, 0).as_slice())
        .unwrap();

    let mut frames = Vec::new();
    for now in 10..=20 {
        let actions = client
            .step_connection(StepInput {
                now,
                event: ConnectionEvent::SendHeartbeat,
            })
            .unwrap();
        frames.push(datagram(&actions, 0));
    }
    for frame in &frames[1..] {
        assert!(server
            .receive_datagram(30, 0, frame.as_slice())
            .unwrap()
            .is_empty());
    }

    let flushed = server
        .receive_datagram(31, 0, frames[0].as_slice())
        .unwrap();
    assert_eq!(
        flushed
            .iter()
            .filter(|action| matches!(action, StackAction::Transmit(_)))
            .count(),
        22
    );
}

fn established_pair() -> (StackEngine, StackEngine) {
    established_pair_with(1)
}

fn established_pair_with(max_packet: u16) -> (StackEngine, StackEngine) {
    let mut client = stack_with_max_packet(1, 2, max_packet);
    let mut server = stack_with_max_packet(2, 1, max_packet);
    open_pair(&mut client, &mut server, 0);
    (client, server)
}

fn open_pair(client: &mut StackEngine, server: &mut StackEngine, now: u32) {
    server
        .step_connection(StepInput {
            now,
            event: ConnectionEvent::Open {
                initial_sequence_number: 100 + now,
            },
        })
        .unwrap();
    let request = client
        .step_connection(StepInput {
            now: now + 1,
            event: ConnectionEvent::Open {
                initial_sequence_number: 200 + now,
            },
        })
        .unwrap();
    let response = server
        .receive_datagram(now + 2, 0, datagram(&request, 0).as_slice())
        .unwrap();
    assert_eq!(
        server.connection_snapshot().state,
        ConnectionState::Start,
        "ConnReq rejected: {:?}",
        server.redundancy_snapshot()
    );
    let heartbeat = client
        .receive_datagram(now + 3, 0, datagram(&response, 0).as_slice())
        .unwrap();
    server
        .receive_datagram(now + 4, 0, datagram(&heartbeat, 0).as_slice())
        .unwrap();

    assert_eq!(client.connection_snapshot().state, ConnectionState::Up);
    assert_eq!(server.connection_snapshot().state, ConnectionState::Up);
}

fn reconnect_after_timeout(server_times_out: bool) {
    let mut client = stack(1, 2);
    let mut server = stack(2, 1);
    open_pair(&mut client, &mut server, 0);

    // Advance both RL sequence spaces beyond the receive window (40 frames).
    for now in (10..=110).step_by(2) {
        let heartbeat = client
            .step_connection(StepInput {
                now,
                event: ConnectionEvent::SendHeartbeat,
            })
            .unwrap();
        server
            .receive_datagram(now, 0, datagram(&heartbeat, 0).as_slice())
            .unwrap();
        let heartbeat = server
            .step_connection(StepInput {
                now: now + 1,
                event: ConnectionEvent::SendHeartbeat,
            })
            .unwrap();
        client
            .receive_datagram(now + 1, 0, datagram(&heartbeat, 0).as_slice())
            .unwrap();
    }
    assert_eq!(client.connection_snapshot().state, ConnectionState::Up);
    assert_eq!(server.connection_snapshot().state, ConnectionState::Up);
    assert!(client.redundancy_snapshot().next_transmit_sequence > 40);
    assert!(server.redundancy_snapshot().next_transmit_sequence > 40);

    let (closing, peer) = if server_times_out {
        (&mut server, &mut client)
    } else {
        (&mut client, &mut server)
    };
    let closed = closing.poll(1_000).unwrap();
    assert!(closed.iter().any(|action| matches!(
        action,
        StackAction::ConnectionStateChanged(change)
            if change.current == ConnectionState::Closed
                && change.disconnect_reason == Some(DisconnectReason::Timeout)
    )));
    peer.receive_datagram(1_001, 0, datagram(&closed, 0).as_slice())
        .unwrap();
    assert_eq!(peer.connection_snapshot().state, ConnectionState::Closed);
    for node in [&*closing, &*peer] {
        let snapshot = node.redundancy_snapshot();
        assert_eq!(snapshot.state, RedundancyState::Closed);
        assert_eq!(snapshot.next_transmit_sequence, 0);
        assert_eq!(snapshot.next_receive_sequence, 0);
        assert_eq!(snapshot.deferred_messages, 0);
    }

    // Reuse the same engines: either timeout direction used to strand SR in Down.
    open_pair(&mut client, &mut server, 1_010);
    for node in [&client, &server] {
        let diagnostics = node.redundancy_snapshot().diagnostics;
        assert_eq!(diagnostics.out_of_window, 0);
        assert_eq!(diagnostics.duplicates, 0);
    }
    let payload = ApplicationPayload::new(b"after reconnect").unwrap();
    let frame = send_data(&mut client, 1_020, payload.as_slice());
    server.receive_datagram(1_021, 0, frame.as_slice()).unwrap();
    assert_eq!(server.pop_received(), Some(payload));
    let frame = send_data(&mut server, 1_022, payload.as_slice());
    client.receive_datagram(1_023, 0, frame.as_slice()).unwrap();
    assert_eq!(client.pop_received(), Some(payload));
}

#[test]
fn same_stack_engines_reconnect_after_client_timeout() {
    reconnect_after_timeout(false);
}

#[test]
fn same_stack_engines_reconnect_after_server_timeout() {
    reconnect_after_timeout(true);
}

#[test]
fn unknown_disconnect_reason_is_discarded_without_postponing_timeout() {
    let (mut client, mut server) = established_pair();
    let closed = client
        .step_connection(StepInput {
            now: 10,
            event: ConnectionEvent::Close,
        })
        .unwrap();
    let before = server.connection_snapshot();
    let sequence = server.redundancy_snapshot().next_receive_sequence;
    let malformed = rewrap(&datagram(&closed, 0), sequence, 6);

    let discarded = server.receive_datagram(900, 0, &malformed).unwrap();
    assert_eq!(discarded.len(), 1);
    assert_eq!(discarded.get(0), Some(&StackAction::DiscardedSrMessage));
    assert_eq!(server.connection_snapshot(), before);
    assert_eq!(
        server.redundancy_snapshot().next_receive_sequence,
        sequence + 1
    );

    let timeout = server.poll(1_001).unwrap();
    assert_eq!(server.connection_snapshot().state, ConnectionState::Closed);
    assert!(timeout.iter().any(|action| matches!(
        action,
        StackAction::ConnectionStateChanged(change)
            if change.disconnect_reason == Some(DisconnectReason::Timeout)
    )));
}

#[test]
fn peer_disconnect_reason_is_not_reported() {
    for event in [ConnectionEvent::Close, ConnectionEvent::Timeout] {
        let (mut client, mut server) = established_pair();
        let closed = client
            .step_connection(StepInput { now: 10, event })
            .unwrap();
        let received = server
            .receive_datagram(11, 0, datagram(&closed, 0).as_slice())
            .unwrap();

        assert_eq!(server.connection_snapshot().state, ConnectionState::Closed);
        let mut changes = received.iter().filter_map(|action| match action {
            StackAction::ConnectionStateChanged(change) => Some(*change),
            _ => None,
        });
        let change = changes.next().expect("expected a state change");
        assert_eq!(change.previous, ConnectionState::Up);
        assert_eq!(change.current, ConnectionState::Closed);
        assert_eq!(change.disconnect_reason, None);
        assert_eq!(changes.next(), None);
    }
}

fn send_data(client: &mut StackEngine, now: u32, bytes: &[u8]) -> WireDatagram {
    datagram(
        &client
            .step_connection(StepInput {
                now,
                event: ConnectionEvent::SendData(ApplicationPayload::new(bytes).unwrap()),
            })
            .unwrap(),
        0,
    )
}

// Alter only the SR bytes / RL sequence, then independently recompute the RL
// CRC. This explicitly exercises CRC-valid frames that fail SR validation.
fn rewrap(source: &WireDatagram, sequence: u32, fault: u8) -> Vec<u8> {
    use rasta_rust::{
        codec::{safety_code::write_md4_safety_code, sr::decode_and_verify_md4},
        redundancy::codec::{decode, RedundancyFrame, MAX_REDUNDANCY_FRAME_LEN},
    };
    let frame = decode(source.as_slice(), CheckCodeType::C).unwrap();
    let mut pdu = frame.payload.to_vec();
    match fault {
        0 => {}
        1 => *pdu.last_mut().unwrap() ^= 1,
        2 => {
            pdu[2] = 0;
            pdu[3] = 0;
        } // unknown SR type
        3 | 4 => {
            // An authentic Data or RetrData PDU with an empty data field.
            let config = SrCodecConfig::with_default_limits(SafetyCodeLength::LowerMd4);
            decode_and_verify_md4(&pdu, config, Md4InitialValue::STANDARD).unwrap();
            let message_type: u16 = if fault == 3 { 6_240 } else { 6_241 };
            pdu.truncate(28 + 8);
            pdu[..2].copy_from_slice(&36_u16.to_le_bytes());
            pdu[2..4].copy_from_slice(&message_type.to_le_bytes());
            write_md4_safety_code(
                &mut pdu,
                SafetyCodeLength::LowerMd4,
                Md4InitialValue::STANDARD,
            )
            .unwrap();
        }
        5 => {
            // An authentic PDU whose first length prefix runs past the field.
            let declared = u16::from_le_bytes([pdu[28], pdu[29]]) + 1;
            pdu[28..30].copy_from_slice(&declared.to_le_bytes());
            write_md4_safety_code(
                &mut pdu,
                SafetyCodeLength::LowerMd4,
                Md4InitialValue::STANDARD,
            )
            .unwrap();
        }
        6 => {
            // An authentic DiscReq with the first unrecognised reason code.
            pdu[30..32].copy_from_slice(&9_u16.to_le_bytes());
            write_md4_safety_code(
                &mut pdu,
                SafetyCodeLength::LowerMd4,
                Md4InitialValue::STANDARD,
            )
            .unwrap();
        }
        7 => {
            // An authentic PDU of an undefined message type.
            pdu[2] = 0;
            pdu[3] = 0;
            write_md4_safety_code(
                &mut pdu,
                SafetyCodeLength::LowerMd4,
                Md4InitialValue::STANDARD,
            )
            .unwrap();
        }
        _ => unreachable!(),
    }
    let mut output = vec![0; MAX_REDUNDANCY_FRAME_LEN];
    let length = RedundancyFrame {
        sequence_number: sequence,
        payload: &pdu,
    }
    .encode_into(CheckCodeType::C, &mut output)
    .unwrap();
    output.truncate(length);
    assert!(decode(&output, CheckCodeType::C).is_ok());
    output
}

#[test]
fn invalid_sr_between_valid_pdus_does_not_rollback_a_defer_flush() {
    for fault in 1..=4 {
        for timer_flush in [false, true] {
            let (mut client, mut server) = established_pair();
            let first = send_data(&mut client, 10, b"first");
            let second = send_data(&mut client, 11, b"second");
            let start = server.redundancy_snapshot().next_receive_sequence + u32::from(timer_flush);
            let first_wire = rewrap(&first, start, 0);
            let poison = rewrap(&first, start + 1, fault);
            let second_wire = rewrap(&second, start + 2, 0);
            server.receive_datagram(12, 0, &poison).unwrap();
            server.receive_datagram(12, 0, &second_wire).unwrap();
            let actions = if timer_flush {
                server.receive_datagram(12, 0, &first_wire).unwrap();
                server.poll(63).unwrap()
            } else {
                server.receive_datagram(13, 0, &first_wire).unwrap()
            };
            assert_eq!(
                actions
                    .iter()
                    .filter(|a| matches!(a, StackAction::DiscardedSrMessage))
                    .count(),
                1
            );
            assert_eq!(
                actions
                    .iter()
                    .filter(|a| matches!(a, StackAction::ApplicationDataAvailable))
                    .count(),
                2
            );
            assert_eq!(server.pop_received().unwrap().as_slice(), b"first");
            assert_eq!(server.pop_received().unwrap().as_slice(), b"second");
            assert!(server.pop_received().is_none());
            assert_eq!(
                server.redundancy_snapshot().next_receive_sequence,
                start + 3
            );
            assert_eq!(server.redundancy_snapshot().deferred_messages, 0);
            assert_eq!(server.connection_snapshot().state, ConnectionState::Up);
            // No repeated poison error, and inactivity still closes the link.
            for now in 64..=1100 {
                server.poll(now).unwrap();
            }
            assert_eq!(server.connection_snapshot().state, ConnectionState::Closed);
        }
    }
}

#[test]
fn poisoned_head_is_consumed_and_cannot_block_timeout_in_the_same_poll() {
    let (mut client, mut server) = established_pair();
    let frame = send_data(&mut client, 10, b"poison");
    let start = server.redundancy_snapshot().next_receive_sequence;
    let poison = rewrap(&frame, start + 1, 1);
    server.receive_datagram(11, 0, &poison).unwrap();
    assert_eq!(server.redundancy_snapshot().deferred_messages, 1);
    let actions = server.poll(1100).unwrap();
    assert!(actions
        .iter()
        .any(|a| matches!(a, StackAction::DiscardedSrMessage)));
    assert_eq!(server.connection_snapshot().state, ConnectionState::Closed);
    assert_eq!(server.redundancy_snapshot().deferred_messages, 0);
    for now in 1101..=1600 {
        assert!(server.poll(now).unwrap().is_empty());
    }
}

fn workspace() -> rasta_rust::stack::StackWorkspace {
    rasta_rust::stack::StackWorkspace::new(
        connection(77, 88), // scratch is reusable across configs
        RedundancyConfig::new(50, 4, 1, 200).unwrap(),
        StackCodecConfig {
            sr: SrCodecConfig::with_default_limits(SafetyCodeLength::LowerMd4),
            md4_initial_value: Md4InitialValue::STANDARD,
            redundancy_check_code: CheckCodeType::C,
        },
    )
}

#[test]
fn reusable_storage_preserves_atomic_errors_and_hides_stale_actions() {
    let (mut client, mut server) = established_pair();
    let mut scratch = workspace();
    let mut actions = StackActions::new();
    let frame = send_data(&mut client, 10, b"first");
    let mut expected = server.clone();
    let expected_actions = expected.receive_datagram(11, 0, frame.as_slice()).unwrap();
    server
        .receive_datagram_into(11, 0, frame.as_slice(), &mut scratch, &mut actions)
        .unwrap();
    assert_eq!(server, expected);
    assert!(actions.iter().eq(expected_actions.iter()));
    let before = server.clone();
    assert!(server
        .receive_datagram_into(12, 0, &[0; 28], &mut scratch, &mut actions)
        .is_err());
    assert_eq!(server, before);
    assert!(actions.is_empty());
    assert!(actions.get(0).is_none());
    assert!(actions.iter().next().is_none());
    assert_eq!(actions, StackActions::new());

    // A connection error after previous successful output is also atomic.
    let payload = ApplicationPayload::new(b"queued").unwrap();
    loop {
        let before = server.clone();
        let input = StepInput {
            now: 12,
            event: ConnectionEvent::SendData(payload),
        };
        let expected_result = expected.step_connection(input);
        let result = server.step_connection_into(input, &mut scratch, &mut actions);
        assert_eq!(server, expected);
        match expected_result {
            Ok(expected_actions) => {
                result.unwrap();
                assert!(actions.iter().eq(expected_actions.iter()));
            }
            Err(error) => {
                assert_eq!(result, Err(error));
                assert_eq!(server, before);
                assert!(actions.get(0).is_none());
                break;
            }
        }
    }
    let expected_actions = expected.poll(1100).unwrap();
    server.poll_into(1100, &mut scratch, &mut actions).unwrap();
    assert_eq!(server, expected);
    assert!(actions.iter().eq(expected_actions.iter()));
}

// Optimized builds exercise the actual embedded API with persistent storage
// allocated before entering the small-stack thread. Debug code has much larger
// unoptimized temporaries and is not the deployment stack budget.
// Windows std reserves 20 KiB for stack-overflow exception handling; Windows
// also rounds stack reservations to 64 KiB units. Allow 128 KiB for that host
// runtime while keeping the 64 KiB regression budget on other platforms.
#[cfg(all(feature = "std", not(debug_assertions)))]
#[test]
fn reusable_storage_handles_maximum_defer_flush_on_small_stack() {
    let stack_bytes = if cfg!(windows) { 128 * 1024 } else { 64 * 1024 };
    let (mut client, server) = established_pair();
    let mut server = Box::new(server);
    let mut scratch = Box::new(workspace());
    let mut actions = Box::new(StackActions::new());
    let mut frames = Vec::new();
    for now in 10..=20 {
        frames.push(datagram(
            &client
                .step_connection(StepInput {
                    now,
                    event: ConnectionEvent::SendHeartbeat,
                })
                .unwrap(),
            0,
        ));
    }
    std::thread::Builder::new()
        .name("stack-budget".into())
        .stack_size(stack_bytes)
        .spawn(move || {
            for frame in &frames[1..] {
                server
                    .receive_datagram_into(30, 0, frame.as_slice(), &mut scratch, &mut actions)
                    .unwrap();
                assert!(actions.is_empty());
            }
            server
                .receive_datagram_into(31, 0, frames[0].as_slice(), &mut scratch, &mut actions)
                .unwrap();
            assert_eq!(
                actions
                    .iter()
                    .filter(|a| matches!(a, StackAction::Transmit(_)))
                    .count(),
                22
            );
            server
                .step_connection_into(
                    StepInput {
                        now: 32,
                        event: ConnectionEvent::SendData(
                            ApplicationPayload::new(&[1; 1055]).unwrap(),
                        ),
                    },
                    &mut scratch,
                    &mut actions,
                )
                .unwrap();
            let wire = datagram(&actions, 0);
            let rl =
                rasta_rust::redundancy::codec::decode(wire.as_slice(), CheckCodeType::C).unwrap();
            let sent = rasta_rust::codec::sr::decode_and_verify_md4(
                rl.payload,
                SrCodecConfig::with_default_limits(SafetyCodeLength::LowerMd4),
                Md4InitialValue::STANDARD,
            )
            .unwrap();
            let snapshot = server.connection_snapshot();
            server
                .step_connection_into(
                    StepInput {
                        now: 33,
                        event: ConnectionEvent::RetransmissionRequestReceived(
                            rasta_rust::codec::sr::SrHeader {
                                receiver_id: 2,
                                sender_id: 1,
                                sequence_number: snapshot.next_receive_sequence,
                                confirmed_sequence_number: sent
                                    .header
                                    .sequence_number
                                    .wrapping_sub(1),
                                timestamp: 33,
                                confirmed_timestamp: snapshot.confirmed_received_timestamp,
                            },
                        ),
                    },
                    &mut scratch,
                    &mut actions,
                )
                .unwrap();
            let response = datagram(&actions, 0);
            let rl = rasta_rust::redundancy::codec::decode(response.as_slice(), CheckCodeType::C)
                .unwrap();
            let sr = rasta_rust::codec::sr::decode_and_verify_md4(
                rl.payload,
                SrCodecConfig::with_default_limits(SafetyCodeLength::LowerMd4),
                Md4InitialValue::STANDARD,
            )
            .unwrap();
            assert!(matches!(
                sr.body,
                rasta_rust::codec::sr::SrBody::RetransmissionResponse
            ));
            server.poll_into(1100, &mut scratch, &mut actions).unwrap();
            assert_eq!(server.connection_snapshot().state, ConnectionState::Closed);
        })
        .unwrap()
        .join()
        .unwrap();
}

// Compiling these statics also checks that nested validated configuration
// constructors allow no_std integrations to reserve storage without a large
// runtime initialization temporary.
#[cfg(feature = "std")]
#[test]
fn composed_storage_can_be_initialized_statically() {
    use rasta_rust::stack::StackWorkspace;
    const VERSION: ProtocolVersion = match ProtocolVersion::new(*b"0303") {
        Ok(value) => value,
        Err(_) => panic!("invalid version"),
    };
    const CONNECTION: ConnectionConfig =
        match ConnectionConfig::new(1, 2, VERSION, 10, 100, diagnostics(100)) {
            Ok(value) => value,
            Err(_) => panic!("invalid connection"),
        };
    const REDUNDANCY: RedundancyConfig = match RedundancyConfig::new(50, 4, 2, 200) {
        Ok(value) => value,
        Err(_) => panic!("invalid redundancy"),
    };
    const CODECS: StackCodecConfig = StackCodecConfig {
        sr: SrCodecConfig::with_default_limits(SafetyCodeLength::LowerMd4),
        md4_initial_value: Md4InitialValue::STANDARD,
        redundancy_check_code: CheckCodeType::C,
    };
    static STORAGE: std::sync::Mutex<(StackEngine, StackWorkspace, StackActions)> =
        std::sync::Mutex::new((
            StackEngine::new(CONNECTION, REDUNDANCY, CODECS),
            StackWorkspace::new(CONNECTION, REDUNDANCY, CODECS),
            StackActions::new(),
        ));
    let mut storage = STORAGE.lock().unwrap();
    let (engine, scratch, actions) = &mut *storage;
    engine
        .step_connection_into(
            StepInput {
                now: 0,
                event: ConnectionEvent::Open {
                    initial_sequence_number: 1,
                },
            },
            scratch,
            actions,
        )
        .unwrap();
    assert_eq!(engine.connection_snapshot().state, ConnectionState::Start);
}

#[test]
fn packed_data_crosses_the_composed_stack() {
    let (mut client, mut server) = established_pair_with(3);
    let mut data = ApplicationData::new();
    for message in [&b"first"[..], b"second", b"third"] {
        data.push(message).unwrap();
    }
    let sent = client
        .step_connection(StepInput {
            now: 10,
            event: ConnectionEvent::SendPacked(data),
        })
        .unwrap();
    let delivered = server
        .receive_datagram(11, 0, datagram(&sent, 0).as_slice())
        .unwrap();
    assert_eq!(
        delivered
            .iter()
            .filter(|a| matches!(a, StackAction::ApplicationDataAvailable))
            .count(),
        1
    );
    for expected in [&b"first"[..], b"second", b"third"] {
        assert_eq!(server.pop_received().unwrap().as_slice(), expected);
    }
    assert!(server.pop_received().is_none());
}

#[test]
fn an_authentic_malformed_data_field_closes_while_a_corrupted_one_is_discarded() {
    for (fault, expected) in [(1, ConnectionState::Up), (5, ConnectionState::Closed)] {
        let (mut client, mut server) = established_pair_with(3);
        let sent = send_data(&mut client, 10, b"payload");
        let sequence = server.redundancy_snapshot().next_receive_sequence;
        let actions = server
            .receive_datagram(11, 0, &rewrap(&sent, sequence, fault))
            .unwrap();
        assert_eq!(
            actions
                .iter()
                .any(|a| matches!(a, StackAction::DiscardedSrMessage)),
            fault == 1
        );
        assert_eq!(server.connection_snapshot().state, expected);
        assert!(server.pop_received().is_none());
    }
}

/// A stack with a 20-entry window that acknowledges every tenth message.
fn stack_with_wide_window(local: u32, remote: u32) -> StackEngine {
    StackEngine::new(
        ConnectionConfig::new_with_timing(
            local,
            remote,
            ProtocolVersion::new(*b"0303").unwrap(),
            20,
            TimingConfig::new(1_000, 500, 10, 10, 20).unwrap(),
            diagnostics(1_000),
        )
        .unwrap(),
        RedundancyConfig::new(50, 4, 2, 200).unwrap(),
        StackCodecConfig {
            sr: SrCodecConfig::with_default_limits(SafetyCodeLength::LowerMd4),
            md4_initial_value: Md4InitialValue::STANDARD,
            redundancy_check_code: CheckCodeType::C,
        },
    )
}

fn channel_zero(actions: &StackActions) -> Vec<WireDatagram> {
    actions
        .iter()
        .filter_map(|action| match action {
            StackAction::Transmit(datagram) if datagram.channel() == 0 => Some(*datagram),
            _ => None,
        })
        .collect()
}

/// Feeds the server's socket buffer while the server can take datagrams. The
/// client receives the server's replies at once; its own replies join the
/// socket buffer.
fn pump(
    server: &mut StackEngine,
    client: &mut StackEngine,
    socket: &mut VecDeque<WireDatagram>,
    now: u32,
) {
    while server.can_receive_datagram() {
        let Some(datagram) = socket.pop_front() else {
            break;
        };
        let replies = server
            .receive_datagram(now, 0, datagram.as_slice())
            .unwrap();
        for reply in channel_zero(&replies) {
            socket.extend(channel_zero(
                &client.receive_datagram(now, 0, reply.as_slice()).unwrap(),
            ));
        }
    }
}

/// Sends numbered messages while at least three send-buffer entries stay free,
/// as `StackEngine::send_usage` asks.
fn send_while_room(
    client: &mut StackEngine,
    socket: &mut VecDeque<WireDatagram>,
    next: &mut u8,
    total: u8,
    now: u32,
) {
    while *next < total && client.send_usage().free > 3 {
        let payload = ApplicationPayload::new(&[*next]).unwrap();
        let actions = client
            .step_connection(StepInput {
                now,
                event: ConnectionEvent::SendData(payload),
            })
            .unwrap();
        socket.extend(channel_zero(&actions));
        *next += 1;
    }
}

#[test]
fn a_slow_application_delays_delivery_without_losing_messages() {
    const TOTAL: u8 = 50;
    let mut client = stack_with_wide_window(1, 2);
    let mut server = stack_with_wide_window(2, 1);
    open_pair(&mut client, &mut server, 0);
    let mut socket = VecDeque::new();
    let mut next = 0;
    let mut now = 10;

    // The server application does not read. Once its receive buffer is full,
    // further messages wait in the receive queue instead of being lost.
    for _ in 0..5 {
        send_while_room(&mut client, &mut socket, &mut next, TOTAL, now);
        pump(&mut server, &mut client, &mut socket, now);
        now += 1;
    }
    assert_eq!(server.receive_usage().free, 0);
    let queued = server.queued_pdus();
    assert!(queued > 11, "enough PDUs to exceed one operation's limit");

    // Reading frees room; the next poll processes at most eleven queued PDUs.
    let mut received = Vec::new();
    while let Some(payload) = server.pop_received() {
        received.push(payload.as_slice()[0]);
    }
    let polled = server.poll(now).unwrap();
    assert_eq!(server.queued_pdus(), queued - 11);
    for reply in channel_zero(&polled) {
        socket.extend(channel_zero(
            &client.receive_datagram(now, 0, reply.as_slice()).unwrap(),
        ));
    }

    for _ in 0..20 {
        now += 1;
        while let Some(payload) = server.pop_received() {
            received.push(payload.as_slice()[0]);
        }
        for reply in channel_zero(&server.poll(now).unwrap()) {
            socket.extend(channel_zero(
                &client.receive_datagram(now, 0, reply.as_slice()).unwrap(),
            ));
        }
        send_while_room(&mut client, &mut socket, &mut next, TOTAL, now);
        pump(&mut server, &mut client, &mut socket, now);
    }
    assert_eq!(received, (0..TOTAL).collect::<Vec<_>>());
    assert_eq!(server.connection_snapshot().state, ConnectionState::Up);
}

/// A server whose send buffer holds 18 unconfirmed messages, so that SR
/// processes no queued PDU.
fn established_pair_with_blocked_server() -> (StackEngine, StackEngine) {
    let mut client = stack_with_wide_window(1, 2);
    let mut server = stack_with_wide_window(2, 1);
    open_pair(&mut client, &mut server, 0);
    for byte in 0..18 {
        server
            .step_connection(StepInput {
                now: 10,
                event: ConnectionEvent::SendData(ApplicationPayload::new(&[byte]).unwrap()),
            })
            .unwrap();
    }
    assert_eq!(server.send_usage().free, 2);
    (client, server)
}

#[test]
fn datagrams_wait_in_the_transport_while_the_receive_queue_lacks_room() {
    let (mut client, mut server) = established_pair_with_blocked_server();
    let frame = send_data(&mut client, 11, b"repeated");
    let first = server.redundancy_snapshot().next_receive_sequence;
    // Repeating one SR PDU under new RL sequence numbers fills the queue.
    for offset in 0..u32::try_from(RECEIVE_QUEUE_CAPACITY).unwrap() {
        assert!(server.can_receive_datagram());
        server
            .receive_datagram(12, 0, &rewrap(&frame, first + offset, 0))
            .unwrap();
    }
    assert_eq!(server.queued_pdus(), RECEIVE_QUEUE_CAPACITY);
    assert!(!server.can_receive_datagram());

    let before = server.clone();
    let next = rewrap(&frame, first + 20, 0);
    assert_eq!(
        server.receive_datagram(13, 0, &next),
        Err(StackError::ReceiveQueueFull)
    );
    assert_eq!(server, before);
}

#[test]
fn a_nearly_full_send_buffer_stops_receiving_as_in_the_reference() {
    // The server has sent 18 messages that the client never confirms.
    let (mut client, mut server) = established_pair_with_blocked_server();

    // A message from the client waits, although it could confirm some.
    let frame = send_data(&mut client, 11, b"waits");
    server.receive_datagram(12, 0, frame.as_slice()).unwrap();
    server.poll(13).unwrap();
    assert_eq!(server.queued_pdus(), 1);
    assert_eq!(server.pop_received(), None);

    // Nothing refreshes the server's timer, so the connection times out and
    // the queue is cleared with the redundancy channel.
    let closed = server.poll(1_100).unwrap();
    assert!(closed.iter().any(|action| matches!(
        action,
        StackAction::ConnectionStateChanged(change)
            if change.disconnect_reason == Some(DisconnectReason::Timeout)
    )));
    assert_eq!(server.queued_pdus(), 0);
    assert!(server.can_receive_datagram());
}

#[test]
fn undecodable_pdus_are_counted_in_the_reference_order() {
    let (mut client, mut server) = established_pair();
    let frame = send_data(&mut client, 10, b"counted");
    let sequence = server.redundancy_snapshot().next_receive_sequence;
    // A corrupted safety code, an undefined type whose safety code no longer
    // matches, and an authentic undefined type: the safety code is checked
    // first.
    for (offset, fault) in [(0, 1), (1, 2), (2, 7)] {
        let discarded = server
            .receive_datagram(11, 0, &rewrap(&frame, sequence + offset, fault))
            .unwrap();
        assert_eq!(discarded.get(0), Some(&StackAction::DiscardedSrMessage));
    }
    let diagnostics = server.connection_snapshot().diagnostics;
    assert_eq!(diagnostics.safety_code_errors, 2);
    assert_eq!(diagnostics.type_errors, 1);
}

#[test]
fn closing_reports_the_diagnostics_of_the_cut_short_window() {
    let (mut client, mut server) = established_pair();
    let frame = send_data(&mut client, 10, b"one");
    let sequence = server.redundancy_snapshot().next_receive_sequence;
    server
        .receive_datagram(11, 0, &rewrap(&frame, sequence, 1))
        .unwrap();
    let closed = server
        .step_connection(StepInput {
            now: 12,
            event: ConnectionEvent::Close,
        })
        .unwrap();
    let reports: Vec<_> = closed
        .iter()
        .filter_map(|action| match action {
            StackAction::ConnectionDiagnostics(diagnostics) => Some(*diagnostics),
            _ => None,
        })
        .collect();
    assert_eq!(reports.len(), 1);
    assert_eq!(reports[0].safety_code_errors, 1);
    assert_eq!(
        server.connection_snapshot().diagnostics,
        rasta_rust::engine::diagnostics::ConnectionDiagnostics::default()
    );
}
