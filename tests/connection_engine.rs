use rasta_rust::{
    codec::sr::{ConnectionData, DisconnectReason, ProtocolVersion, SrBody, SrHeader},
    engine::{
        buffers::{ApplicationData, ApplicationPayload, ConnectionBuffers},
        connection::{
            ConfigurationError, ConnectionConfig, ConnectionEngine, ConnectionEvent,
            ConnectionSnapshot, ConnectionState, Effect, EngineError, OutboundMessage,
            ReceivedConnectionMessage, ReceivedData, ReceivedHeartbeat, Role, StateChange,
            StepInput, Transition, DEFAULT_MAX_PACKET, MAX_MAX_PACKET,
        },
        diagnostics::{ConnectionDiagnostics, DiagnosticsConfig, DiagnosticsConfigurationError},
    },
};

fn version(bytes: [u8; 4]) -> ProtocolVersion {
    ProtocolVersion::new(bytes).expect("test protocol version is valid")
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

fn config(local_id: u32, remote_id: u32) -> ConnectionConfig {
    ConnectionConfig::new(
        local_id,
        remote_id,
        version(*b"0303"),
        10,
        100,
        diagnostics(100),
    )
    .expect("test configuration is valid")
}

#[allow(clippy::large_types_passed_by_value)]
fn step(engine: &mut ConnectionEngine, now: u32, event: ConnectionEvent) -> Transition {
    let mut buffers = ConnectionBuffers::new(engine.config());
    engine
        .step(&mut buffers, StepInput { now, event })
        .expect("test transition is implemented")
}

fn transmitted(transition: &Transition, index: usize) -> OutboundMessage {
    match transition.effects.get(index) {
        Some(Effect::Transmit(message)) => *message,
        other => panic!("expected transmit effect at {index}, got {other:?}"),
    }
}

fn state_change(transition: &Transition, index: usize) -> StateChange {
    match transition.effects.get(index) {
        Some(Effect::StateChanged(change)) => *change,
        other => panic!("expected state-change effect at {index}, got {other:?}"),
    }
}

#[allow(clippy::large_types_passed_by_value)]
fn received_connection(message: OutboundMessage) -> ReceivedConnectionMessage {
    match message {
        OutboundMessage::ConnectionRequest { header, data }
        | OutboundMessage::ConnectionResponse { header, data } => {
            ReceivedConnectionMessage { header, data }
        }
        other => panic!("expected connection message, got {other:?}"),
    }
}

#[allow(clippy::large_types_passed_by_value)]
fn received_heartbeat(message: OutboundMessage) -> ReceivedHeartbeat {
    match message {
        OutboundMessage::Heartbeat { header } => ReceivedHeartbeat { header },
        other => panic!("expected heartbeat, got {other:?}"),
    }
}

#[allow(clippy::large_types_passed_by_value)]
fn received_data(message: OutboundMessage) -> ReceivedData {
    match message {
        OutboundMessage::Data { header, data }
        | OutboundMessage::RetransmittedData { header, data } => ReceivedData { header, data },
        other => panic!("expected data, got {other:?}"),
    }
}

fn next_received_data(engine: &ConnectionEngine, payload: &ApplicationPayload) -> ReceivedData {
    let config = engine.config();
    let snapshot = engine.snapshot();
    ReceivedData {
        header: SrHeader {
            receiver_id: config.local_id(),
            sender_id: config.remote_id(),
            sequence_number: snapshot.next_receive_sequence,
            confirmed_sequence_number: snapshot.confirmed_receive_sequence,
            timestamp: snapshot.received_timestamp.wrapping_add(1),
            confirmed_timestamp: snapshot.confirmed_received_timestamp,
        },
        data: ApplicationData::single(payload),
    }
}

fn handshake_pending_server_heartbeat() -> (ConnectionEngine, ConnectionEngine, ReceivedHeartbeat) {
    let mut client = ConnectionEngine::new(config(1, 2));
    let mut server = ConnectionEngine::new(config(2, 1));

    let client_open = step(
        &mut client,
        1_000,
        ConnectionEvent::Open {
            initial_sequence_number: 0xffff_fffe,
        },
    );
    let request = received_connection(transmitted(&client_open, 1));

    let server_open = step(
        &mut server,
        900,
        ConnectionEvent::Open {
            initial_sequence_number: 100,
        },
    );
    assert_eq!(server_open.current.state, ConnectionState::Down);

    let server_response = step(
        &mut server,
        1_005,
        ConnectionEvent::ConnectionRequestReceived(request),
    );
    let response = received_connection(transmitted(&server_response, 0));

    let client_heartbeat = step(
        &mut client,
        1_010,
        ConnectionEvent::ConnectionResponseReceived(response),
    );
    let heartbeat = received_heartbeat(transmitted(&client_heartbeat, 0));

    (client, server, heartbeat)
}

fn establish_connection() -> (ConnectionEngine, ConnectionEngine) {
    let (client, mut server, heartbeat) = handshake_pending_server_heartbeat();
    let server_up = step(
        &mut server,
        1_015,
        ConnectionEvent::HeartbeatReceived(heartbeat),
    );
    assert_eq!(server_up.current.state, ConnectionState::Up);
    assert_eq!(client.snapshot().state, ConnectionState::Up);

    (client, server)
}

fn establish_buffered_connection() -> (
    ConnectionEngine,
    ConnectionBuffers,
    ConnectionEngine,
    ConnectionBuffers,
) {
    establish_buffered_connection_with(config(1, 2), config(2, 1))
}

fn establish_buffered_connection_with(
    client_config: ConnectionConfig,
    server_config: ConnectionConfig,
) -> (
    ConnectionEngine,
    ConnectionBuffers,
    ConnectionEngine,
    ConnectionBuffers,
) {
    let mut client = ConnectionEngine::new(client_config);
    let mut client_buffers = ConnectionBuffers::new(client.config());
    let mut server = ConnectionEngine::new(server_config);
    let mut server_buffers = ConnectionBuffers::new(server.config());

    let client_open = client
        .step(
            &mut client_buffers,
            StepInput {
                now: 1_000,
                event: ConnectionEvent::Open {
                    initial_sequence_number: 0xffff_fffe,
                },
            },
        )
        .unwrap();
    let request = received_connection(transmitted(&client_open, 1));
    server
        .step(
            &mut server_buffers,
            StepInput {
                now: 900,
                event: ConnectionEvent::Open {
                    initial_sequence_number: 100,
                },
            },
        )
        .unwrap();
    let server_response = server
        .step(
            &mut server_buffers,
            StepInput {
                now: 1_005,
                event: ConnectionEvent::ConnectionRequestReceived(request),
            },
        )
        .unwrap();
    let response = received_connection(transmitted(&server_response, 0));
    let client_heartbeat = client
        .step(
            &mut client_buffers,
            StepInput {
                now: 1_010,
                event: ConnectionEvent::ConnectionResponseReceived(response),
            },
        )
        .unwrap();
    let heartbeat = received_heartbeat(transmitted(&client_heartbeat, 0));
    server
        .step(
            &mut server_buffers,
            StepInput {
                now: 1_015,
                event: ConnectionEvent::HeartbeatReceived(heartbeat),
            },
        )
        .unwrap();

    let server_heartbeat = server
        .step(
            &mut server_buffers,
            StepInput {
                now: 1_020,
                event: ConnectionEvent::SendHeartbeat,
            },
        )
        .unwrap();
    client
        .step(
            &mut client_buffers,
            StepInput {
                now: 1_025,
                event: ConnectionEvent::HeartbeatReceived(received_heartbeat(transmitted(
                    &server_heartbeat,
                    0,
                ))),
            },
        )
        .unwrap();

    (client, client_buffers, server, server_buffers)
}

#[test]
fn client_and_server_complete_the_handshake() {
    let mut client = ConnectionEngine::new(config(1, 2));
    let mut server = ConnectionEngine::new(config(2, 1));

    let client_open = step(
        &mut client,
        1_000,
        ConnectionEvent::Open {
            initial_sequence_number: 0xffff_fffe,
        },
    );
    assert_eq!(
        client_open.effects.get(0),
        Some(&Effect::OpenRedundancyChannel)
    );
    let request = transmitted(&client_open, 1);
    assert_eq!(
        request,
        OutboundMessage::ConnectionRequest {
            header: SrHeader {
                receiver_id: 2,
                sender_id: 1,
                sequence_number: 0xffff_fffe,
                confirmed_sequence_number: 0,
                timestamp: 1_000,
                confirmed_timestamp: 0,
            },
            data: ConnectionData::new(version(*b"0303"), 10),
        }
    );
    assert_eq!(
        state_change(&client_open, 2),
        StateChange {
            previous: ConnectionState::Closed,
            current: ConnectionState::Start,
            disconnect_reason: None,
        }
    );

    let server_open = step(
        &mut server,
        900,
        ConnectionEvent::Open {
            initial_sequence_number: 100,
        },
    );
    assert_eq!(
        server_open.effects.get(0),
        Some(&Effect::OpenRedundancyChannel)
    );
    assert_eq!(server_open.current.state, ConnectionState::Down);

    let server_response = step(
        &mut server,
        1_005,
        ConnectionEvent::ConnectionRequestReceived(received_connection(request)),
    );
    let response = transmitted(&server_response, 0);
    assert_eq!(
        response,
        OutboundMessage::ConnectionResponse {
            header: SrHeader {
                receiver_id: 1,
                sender_id: 2,
                sequence_number: 100,
                confirmed_sequence_number: 0xffff_fffe,
                timestamp: 1_005,
                confirmed_timestamp: 1_000,
            },
            data: ConnectionData::new(version(*b"0303"), 10),
        }
    );

    let client_heartbeat = step(
        &mut client,
        1_010,
        ConnectionEvent::ConnectionResponseReceived(received_connection(response)),
    );
    let heartbeat = transmitted(&client_heartbeat, 0);
    assert_eq!(
        heartbeat,
        OutboundMessage::Heartbeat {
            header: SrHeader {
                receiver_id: 2,
                sender_id: 1,
                sequence_number: 0xffff_ffff,
                confirmed_sequence_number: 100,
                timestamp: 1_010,
                confirmed_timestamp: 1_005,
            }
        }
    );
    assert_eq!(client_heartbeat.current.state, ConnectionState::Up);

    let server_up = step(
        &mut server,
        1_015,
        ConnectionEvent::HeartbeatReceived(received_heartbeat(heartbeat)),
    );
    assert_eq!(server_up.current.state, ConnectionState::Up);
    assert_eq!(server.snapshot().next_receive_sequence, 0);
    assert_eq!(server.snapshot().confirmed_send_sequence, 0xffff_ffff);
}

#[test]
fn persistent_buffers_release_confirmed_handshake_messages() {
    let (client, client_buffers, server, server_buffers) = establish_buffered_connection();

    assert_eq!(client.snapshot().state, ConnectionState::Up);
    assert_eq!(server.snapshot().state, ConnectionState::Up);
    assert_eq!(client_buffers.send_usage().used, 0);
    assert_eq!(client_buffers.unconfirmed_messages(), 0);
    assert_eq!(server_buffers.send_usage().used, 1);
    assert_eq!(server_buffers.unconfirmed_messages(), 1);
}

#[test]
fn remote_window_holds_data_until_confirmation_opens_flow() {
    let (mut client, mut buffers, _, _) = establish_buffered_connection();
    let payload = ApplicationPayload::new(&[0x5a]).unwrap();
    let mut last_transmitted_sequence = 0;

    for offset in 0..10 {
        let transition = client
            .step(
                &mut buffers,
                StepInput {
                    now: 2_000 + offset,
                    event: ConnectionEvent::SendData(payload),
                },
            )
            .unwrap();
        last_transmitted_sequence = transmitted(&transition, 0).header().sequence_number;
    }

    let held = client
        .step(
            &mut buffers,
            StepInput {
                now: 2_010,
                event: ConnectionEvent::SendData(payload),
            },
        )
        .unwrap();
    assert!(held.effects.is_empty());
    assert_eq!(buffers.unconfirmed_messages(), 10);
    assert_eq!(buffers.unsent_messages(), 1);

    let expected_peer_sequence = client.snapshot().next_receive_sequence;
    let acknowledged = client
        .step(
            &mut buffers,
            StepInput {
                now: 1_070,
                event: ConnectionEvent::HeartbeatReceived(ReceivedHeartbeat {
                    header: SrHeader {
                        receiver_id: 1,
                        sender_id: 2,
                        sequence_number: expected_peer_sequence,
                        confirmed_sequence_number: last_transmitted_sequence,
                        timestamp: 1_050,
                        confirmed_timestamp: 1_010,
                    },
                }),
            },
        )
        .unwrap();
    assert!(acknowledged.effects.is_empty());
    assert_eq!(buffers.unconfirmed_messages(), 0);
    assert_eq!(buffers.unsent_messages(), 1);

    let flush = client
        .step(
            &mut buffers,
            StepInput {
                now: 1_080,
                event: ConnectionEvent::SendHeartbeat,
            },
        )
        .unwrap();
    match transmitted(&flush, 0) {
        OutboundMessage::Data { header, .. } => {
            assert_eq!(
                header.sequence_number,
                last_transmitted_sequence.wrapping_add(1)
            );
            assert_eq!(header.timestamp, 1_080);
            assert_eq!(header.confirmed_sequence_number, expected_peer_sequence);
        }
        other => panic!("expected held data before the newer heartbeat, got {other:?}"),
    }
    assert_eq!(buffers.unconfirmed_messages(), 1);
    assert_eq!(buffers.unsent_messages(), 1);
}

#[test]
fn full_send_buffer_rejects_transition_without_partial_mutation() {
    let (mut client, mut buffers, _, _) = establish_buffered_connection();
    let payload = ApplicationPayload::new(&[1]).unwrap();

    for offset in 0..20 {
        client
            .step(
                &mut buffers,
                StepInput {
                    now: 2_000 + offset,
                    event: ConnectionEvent::SendData(payload),
                },
            )
            .unwrap();
    }
    assert_eq!(buffers.send_usage().used, 20);
    let snapshot_before = client.snapshot();
    let buffers_before = buffers.clone();

    assert_eq!(
        client.step(
            &mut buffers,
            StepInput {
                now: 3_000,
                event: ConnectionEvent::SendData(payload),
            }
        ),
        Err(EngineError::Buffer(
            rasta_rust::engine::buffers::BufferError::SendBufferFull
        ))
    );
    assert_eq!(client.snapshot(), snapshot_before);
    assert_eq!(buffers, buffers_before);
}

#[test]
fn identical_snapshots_and_inputs_replay_identically() {
    let mut first = ConnectionEngine::new(config(1, 2));
    let mut second = first;
    let mut first_buffers = ConnectionBuffers::new(first.config());
    let mut second_buffers = first_buffers.clone();
    let input = StepInput {
        now: u32::MAX,
        event: ConnectionEvent::Open {
            initial_sequence_number: u32::MAX,
        },
    };

    assert_eq!(
        first.step(&mut first_buffers, input),
        second.step(&mut second_buffers, input)
    );
    assert_eq!(first.snapshot(), second.snapshot());
    assert_eq!(first_buffers, second_buffers);
    assert_eq!(first.snapshot().next_send_sequence, 0);
}

#[test]
fn buffers_for_another_local_window_are_rejected_atomically() {
    let connection_config = config(1, 2);
    let mut engine = ConnectionEngine::new(connection_config);
    let other_config =
        ConnectionConfig::new(1, 2, version(*b"0303"), 2, 100, diagnostics(100)).unwrap();
    let mut buffers = ConnectionBuffers::new(other_config);
    let before = buffers.clone();

    assert_eq!(
        engine.step(
            &mut buffers,
            StepInput {
                now: 10,
                event: ConnectionEvent::Open {
                    initial_sequence_number: 20,
                },
            }
        ),
        Err(EngineError::Buffer(
            rasta_rust::engine::buffers::BufferError::ConfigurationMismatch {
                expected: 10,
                actual: 2,
            }
        ))
    );
    assert_eq!(engine.snapshot().state, ConnectionState::Closed);
    assert_eq!(buffers, before);
}

#[test]
fn in_sequence_heartbeat_updates_up_state_without_external_effects() {
    let (mut client, mut server) = establish_connection();
    let outbound = step(&mut server, 1_020, ConnectionEvent::SendHeartbeat);
    let heartbeat = received_heartbeat(transmitted(&outbound, 0));

    let received = step(
        &mut client,
        1_025,
        ConnectionEvent::HeartbeatReceived(heartbeat),
    );
    assert!(received.effects.is_empty());
    assert_eq!(received.current.state, ConnectionState::Up);
    assert_eq!(received.current.next_receive_sequence, 102);
    assert_eq!(received.current.confirmed_send_sequence, 101);
    assert_eq!(received.current.confirmed_receive_sequence, u32::MAX);
    assert_eq!(received.current.received_timestamp, 1_020);
    assert_eq!(received.current.confirmed_received_timestamp, 1_010);
}

#[test]
fn heartbeat_sequence_gap_requests_retransmission_without_accepting_pdu() {
    let (mut client, mut server) = establish_connection();
    let outbound = step(&mut server, 1_020, ConnectionEvent::SendHeartbeat);
    let mut heartbeat = received_heartbeat(transmitted(&outbound, 0));
    heartbeat.header.sequence_number = heartbeat.header.sequence_number.wrapping_add(1);
    let before = client.snapshot();

    let transition = step(
        &mut client,
        1_025,
        ConnectionEvent::HeartbeatReceived(heartbeat),
    );
    assert_eq!(
        transition.effects.get(0),
        Some(&Effect::DiscardReceivedMessage)
    );
    let request = transmitted(&transition, 1);
    match request {
        OutboundMessage::RetransmissionRequest { header } => {
            assert_eq!(header.sequence_number, 0);
            assert_eq!(header.confirmed_sequence_number, 100);
            assert_eq!(header.confirmed_timestamp, 1_005);
        }
        other => panic!("expected retransmission request, got {other:?}"),
    }
    assert_eq!(request.as_sr_message().body, SrBody::RetransmissionRequest);
    assert_eq!(
        state_change(&transition, 2),
        StateChange {
            previous: ConnectionState::Up,
            current: ConnectionState::RetransmissionRequest,
            disconnect_reason: None,
        }
    );
    assert_eq!(
        transition.current.next_receive_sequence,
        before.next_receive_sequence
    );
    assert_eq!(
        transition.current.received_timestamp,
        before.received_timestamp
    );
}

#[test]
fn in_sequence_data_is_buffered_and_notifies_the_application() {
    let (mut client, mut client_buffers, mut server, mut server_buffers) =
        establish_buffered_connection();
    let payload = ApplicationPayload::new(&[0xde, 0xad, 0xbe, 0xef]).unwrap();
    let outbound = server
        .step(
            &mut server_buffers,
            StepInput {
                now: 1_030,
                event: ConnectionEvent::SendData(payload),
            },
        )
        .unwrap();
    let data = received_data(transmitted(&outbound, 0));

    let transition = client
        .step(
            &mut client_buffers,
            StepInput {
                now: 1_035,
                event: ConnectionEvent::DataReceived(data),
            },
        )
        .unwrap();

    assert_eq!(
        transition.effects.get(0),
        Some(&Effect::ApplicationDataAvailable)
    );
    assert_eq!(transition.effects.len(), 1);
    assert_eq!(transition.current.state, ConnectionState::Up);
    assert_eq!(
        transition.current.next_receive_sequence,
        data.header.sequence_number.wrapping_add(1)
    );
    assert_eq!(
        transition.current.confirmed_send_sequence,
        data.header.sequence_number
    );
    assert_eq!(client_buffers.receive_usage().used, 1);
    assert_eq!(client_buffers.pop_received(), Some(payload));
    assert_eq!(client_buffers.receive_usage().used, 0);
}

#[test]
fn data_sequence_gap_requests_retransmission_without_buffering_payload() {
    let (mut client, mut client_buffers, _, _) = establish_buffered_connection();
    let payload = ApplicationPayload::new(&[1, 2, 3]).unwrap();
    let mut data = next_received_data(&client, &payload);
    data.header.sequence_number = data.header.sequence_number.wrapping_add(1);
    let snapshot_before = client.snapshot();

    let transition = client
        .step(
            &mut client_buffers,
            StepInput {
                now: 1_030,
                event: ConnectionEvent::DataReceived(data),
            },
        )
        .unwrap();

    assert_eq!(
        transition.effects.get(0),
        Some(&Effect::DiscardReceivedMessage)
    );
    assert!(matches!(
        transition.effects.get(1),
        Some(Effect::Transmit(
            OutboundMessage::RetransmissionRequest { .. }
        ))
    ));
    assert_eq!(
        transition.current.state,
        ConnectionState::RetransmissionRequest
    );
    assert_eq!(
        transition.current.next_receive_sequence,
        snapshot_before.next_receive_sequence
    );
    assert_eq!(client_buffers.receive_usage().used, 0);
    assert_eq!(client_buffers.pop_received(), None);
}

#[test]
fn retransmission_response_and_data_restore_flow() {
    let (mut client, mut client_buffers, mut server, mut server_buffers) =
        establish_buffered_connection();

    let outbound = server
        .step(
            &mut server_buffers,
            StepInput {
                now: 1_040,
                event: ConnectionEvent::SendHeartbeat,
            },
        )
        .unwrap();
    let mut heartbeat = received_heartbeat(transmitted(&outbound, 0));
    heartbeat.header.sequence_number = heartbeat.header.sequence_number.wrapping_add(1);

    let retr_req = client
        .step(
            &mut client_buffers,
            StepInput {
                now: 1_045,
                event: ConnectionEvent::HeartbeatReceived(heartbeat),
            },
        )
        .unwrap();
    assert_eq!(
        retr_req.current.state,
        ConnectionState::RetransmissionRequest
    );

    let response = client
        .step(
            &mut client_buffers,
            StepInput {
                now: 1_050,
                event: ConnectionEvent::RetransmissionResponseReceived(SrHeader {
                    receiver_id: client.config().local_id(),
                    sender_id: client.config().remote_id(),
                    sequence_number: retr_req.current.next_receive_sequence,
                    confirmed_sequence_number: retr_req.current.confirmed_receive_sequence,
                    timestamp: 1_050,
                    confirmed_timestamp: retr_req.current.confirmed_received_timestamp,
                }),
            },
        )
        .unwrap();
    assert_eq!(
        response.current.state,
        ConnectionState::RetransmissionRunning
    );

    let payload = ApplicationPayload::new(&[0xaa, 0xbb]).unwrap();
    let retransmitted = next_received_data(&client, &payload);
    let accepted = client
        .step(
            &mut client_buffers,
            StepInput {
                now: 1_055,
                event: ConnectionEvent::RetransmittedDataReceived(retransmitted),
            },
        )
        .unwrap();
    assert_eq!(
        accepted.current.state,
        ConnectionState::RetransmissionRunning
    );
    assert_eq!(
        accepted.effects.get(0),
        Some(&Effect::ApplicationDataAvailable)
    );
    assert_eq!(client_buffers.pop_received(), Some(payload));
}

#[test]
fn incoming_retransmission_request_rebuilds_sent_data() {
    let (client, client_buffers, mut server, mut server_buffers) = establish_buffered_connection();
    let payload = ApplicationPayload::new(&[0x44, 0x55]).unwrap();
    let sent = server
        .step(
            &mut server_buffers,
            StepInput {
                now: 1_030,
                event: ConnectionEvent::SendData(payload),
            },
        )
        .unwrap();
    let sent_sequence = transmitted(&sent, 0).header().sequence_number;
    let client_snapshot = client.snapshot();
    let request = SrHeader {
        receiver_id: server.config().local_id(),
        sender_id: server.config().remote_id(),
        sequence_number: server.snapshot().next_receive_sequence,
        confirmed_sequence_number: sent_sequence.wrapping_sub(1),
        timestamp: 1_035,
        confirmed_timestamp: server.snapshot().confirmed_received_timestamp,
    };

    let response = server
        .step(
            &mut server_buffers,
            StepInput {
                now: 1_035,
                event: ConnectionEvent::RetransmissionRequestReceived(request),
            },
        )
        .unwrap();
    assert!(matches!(
        transmitted(&response, 0),
        OutboundMessage::RetransmissionResponse { .. }
    ));

    let replay = server
        .step(
            &mut server_buffers,
            StepInput {
                now: 1_040,
                event: ConnectionEvent::SendHeartbeat,
            },
        )
        .unwrap();
    match transmitted(&replay, 0) {
        OutboundMessage::RetransmittedData { data, .. } => {
            assert_eq!(data, ApplicationData::single(&payload));
        }
        other => panic!("expected retransmitted data, got {other:?}"),
    }
    assert_eq!(client.snapshot(), client_snapshot);
    assert_eq!(client_buffers.receive_usage().used, 0);
}

#[test]
fn full_send_window_can_be_rebuilt_for_retransmission() {
    let (mut client, mut client_buffers, mut server, mut server_buffers) =
        establish_buffered_connection();
    let confirmation = client
        .step(
            &mut client_buffers,
            StepInput {
                now: 1_020,
                event: ConnectionEvent::SendHeartbeat,
            },
        )
        .unwrap();
    server
        .step(
            &mut server_buffers,
            StepInput {
                now: 1_025,
                event: ConnectionEvent::HeartbeatReceived(received_heartbeat(transmitted(
                    &confirmation,
                    0,
                ))),
            },
        )
        .unwrap();
    assert_eq!(server_buffers.send_usage().used, 0);

    let payload = ApplicationPayload::new(&[0x77]).unwrap();
    let mut first_sequence = None;
    for index in 0..20 {
        let transition = server
            .step(
                &mut server_buffers,
                StepInput {
                    now: 1_030 + index,
                    event: ConnectionEvent::SendData(payload),
                },
            )
            .unwrap();
        if index == 0 {
            first_sequence = Some(
                transition
                    .effects
                    .iter()
                    .find_map(|effect| match effect {
                        Effect::Transmit(message) => Some(message.header().sequence_number),
                        _ => None,
                    })
                    .unwrap(),
            );
        }
    }
    assert_eq!(server_buffers.send_usage().used, 20);

    let snapshot = server.snapshot();
    let rebuilt = server
        .step(
            &mut server_buffers,
            StepInput {
                now: 1_060,
                event: ConnectionEvent::RetransmissionRequestReceived(SrHeader {
                    receiver_id: server.config().local_id(),
                    sender_id: server.config().remote_id(),
                    sequence_number: snapshot.next_receive_sequence,
                    confirmed_sequence_number: first_sequence.unwrap().wrapping_sub(1),
                    timestamp: 1_060,
                    confirmed_timestamp: snapshot.confirmed_received_timestamp,
                }),
            },
        )
        .unwrap();

    assert!(matches!(
        transmitted(&rebuilt, 0),
        OutboundMessage::RetransmissionResponse { .. }
    ));
    assert!(server_buffers.send_usage().used > 20);
}

#[test]
fn acknowledgement_beyond_last_transmitted_sequence_is_ignored() {
    let (mut client, mut server) = establish_connection();
    let heartbeat = step(&mut server, 1_020, ConnectionEvent::SendHeartbeat);
    let mut received = received_heartbeat(transmitted(&heartbeat, 0));
    received.header.confirmed_sequence_number = client
        .snapshot()
        .last_transmitted_sequence
        .unwrap()
        .wrapping_add(1);
    let before = client.snapshot();

    let ignored = step(
        &mut client,
        1_025,
        ConnectionEvent::HeartbeatReceived(received),
    );

    // Ignored, and counted as an implausible confirmed sequence number.
    assert!(ignored.effects.is_empty());
    let mut expected = before;
    expected.diagnostics.confirmed_sequence_errors = 1;
    assert_eq!(ignored.current, expected);
}

#[test]
fn unavailable_retransmission_request_closes_connection() {
    let (_, _, mut server, mut server_buffers) = establish_buffered_connection();
    let snapshot = server.snapshot();
    let transition = server
        .step(
            &mut server_buffers,
            StepInput {
                now: 1_030,
                event: ConnectionEvent::RetransmissionRequestReceived(SrHeader {
                    receiver_id: server.config().local_id(),
                    sender_id: server.config().remote_id(),
                    sequence_number: snapshot.next_receive_sequence,
                    confirmed_sequence_number: snapshot.next_send_sequence.wrapping_add(10),
                    timestamp: 1_030,
                    confirmed_timestamp: snapshot.confirmed_received_timestamp,
                }),
            },
        )
        .unwrap();

    assert_eq!(transition.current.state, ConnectionState::Closed);
    match transmitted(&transition, 0) {
        OutboundMessage::DisconnectionRequest { reason, .. } => {
            assert_eq!(reason, DisconnectReason::RetransmissionFailed);
        }
        other => panic!("expected retransmission failure disconnect, got {other:?}"),
    }
}

#[test]
fn local_closure_transmits_disconnect_before_closing_redundancy() {
    for (event, reason) in [
        (ConnectionEvent::Timeout, DisconnectReason::Timeout),
        (ConnectionEvent::Close, DisconnectReason::UserRequest),
        (
            ConnectionEvent::Open {
                initial_sequence_number: 0,
            },
            DisconnectReason::ServiceNotAllowed,
        ),
    ] {
        let (mut client, mut buffers, _, _) = establish_buffered_connection();
        let closed = client
            .step(&mut buffers, StepInput { now: 1_030, event })
            .unwrap();

        assert_closed_with(&closed, reason);
        assert_eq!(closed.effects.len(), 4);
        assert_eq!(closed.effects.get(1), Some(&Effect::CloseRedundancyChannel));
        assert!(matches!(
            closed.effects.get(2),
            Some(Effect::Diagnostics(_))
        ));
        assert_eq!(
            state_change(&closed, 3),
            StateChange {
                previous: ConnectionState::Up,
                current: ConnectionState::Closed,
                disconnect_reason: Some(reason),
            }
        );
        assert_eq!(buffers, ConnectionBuffers::new(client.config()));
    }
}

#[test]
fn timer_poll_emits_heartbeat_then_detects_timeout() {
    let (mut client, mut client_buffers, _, _) = establish_buffered_connection();
    let heartbeat = client
        .step(
            &mut client_buffers,
            StepInput {
                now: 1_060,
                event: ConnectionEvent::PollTimers,
            },
        )
        .unwrap();
    assert!(matches!(
        transmitted(&heartbeat, 0),
        OutboundMessage::Heartbeat { .. }
    ));

    let timeout = client
        .step(
            &mut client_buffers,
            StepInput {
                now: 1_200,
                event: ConnectionEvent::PollTimers,
            },
        )
        .unwrap();
    assert_eq!(timeout.current.state, ConnectionState::Closed);
}

/// A heartbeat from the client to the server that confirms the server's local
/// timestamp `confirmed_timestamp`.
fn heartbeat_confirming(server: &ConnectionEngine, confirmed_timestamp: u32) -> ReceivedHeartbeat {
    let config = server.config();
    let snapshot = server.snapshot();
    ReceivedHeartbeat {
        header: SrHeader {
            receiver_id: config.local_id(),
            sender_id: config.remote_id(),
            sequence_number: snapshot.next_receive_sequence,
            confirmed_sequence_number: snapshot.confirmed_receive_sequence,
            timestamp: snapshot.received_timestamp.wrapping_add(50),
            confirmed_timestamp,
        },
    }
}

#[test]
fn timer_granularity_defaults_to_one_millisecond_and_can_be_set() {
    use rasta_rust::engine::connection::DEFAULT_TIMER_GRANULARITY;
    assert_eq!(DEFAULT_TIMER_GRANULARITY, 1);
    let default = config(1, 2);
    assert_eq!(default.timing().timer_granularity(), 1);

    let coarse = default.with_timer_granularity(10).unwrap();
    assert_eq!(coarse.timing().timer_granularity(), 10);
    // Everything else is kept.
    assert_eq!(coarse.timing().t_max(), default.timing().t_max());
    assert_eq!(
        coarse.timing().t_heartbeat(),
        default.timing().t_heartbeat()
    );
    assert_eq!(coarse.timing().m_w_a(), default.timing().m_w_a());
    assert_eq!((coarse.local_id(), coarse.remote_id()), (1, 2));

    // The granularity may not exceed T_max (100 ms here).
    assert!(default.with_timer_granularity(100).is_ok());
    assert!(default.with_timer_granularity(101).is_err());
}

#[test]
fn the_endpoint_with_the_lower_identifier_is_the_client() {
    assert_eq!(config(1, 2).role(), Role::Client);
    assert_eq!(config(2, 1).role(), Role::Server);
    assert_eq!(config(0xffff_fffe, 0xffff_ffff).role(), Role::Client);
    assert_eq!(config(0xffff_ffff, 0).role(), Role::Server);

    // The client opens with a connection request; the server waits in Down.
    let mut client = ConnectionEngine::new(config(7, 9));
    let opened = step(
        &mut client,
        0,
        ConnectionEvent::Open {
            initial_sequence_number: 1,
        },
    );
    assert!(matches!(
        transmitted(&opened, 1),
        OutboundMessage::ConnectionRequest { .. }
    ));
    assert_eq!(opened.current.state, ConnectionState::Start);

    let mut server = ConnectionEngine::new(config(9, 7));
    let opened = step(
        &mut server,
        0,
        ConnectionEvent::Open {
            initial_sequence_number: 1,
        },
    );
    assert_eq!(opened.current.state, ConnectionState::Down);
}

#[test]
fn a_late_but_timely_message_restarts_t_i_instead_of_timing_out() {
    // T_max is 100 and the timer granularity 1 ms. The server accepts a
    // heartbeat at 1_090 that confirms its timestamp 1_020, so
    // T_rtd = 1_090 + 1 - 1_020 = 71: later than T_max / 2 but still timely.
    let (_client, mut server) = establish_connection();
    let heartbeat = heartbeat_confirming(&server, 1_020);
    let accepted = step(
        &mut server,
        1_090,
        ConnectionEvent::HeartbeatReceived(heartbeat),
    );
    assert_eq!(accepted.current.state, ConnectionState::Up);
    assert_eq!(accepted.current.timing.timer_t_i, 29);

    // T_i runs from acceptance: the peer's confirmation may be T_max (less the
    // granularity) old, so the last poll without timeout is at 1_119.
    for now in [1_090, 1_105, 1_119] {
        let poll = step(&mut server, now, ConnectionEvent::PollTimers);
        assert_eq!(
            poll.current.state,
            ConnectionState::Up,
            "timed out at {now}"
        );
    }
    let timeout = step(&mut server, 1_120, ConnectionEvent::PollTimers);
    assert_eq!(timeout.current.state, ConnectionState::Closed);
    match transmitted(&timeout, 0) {
        OutboundMessage::DisconnectionRequest { reason, .. } => {
            assert_eq!(reason, DisconnectReason::Timeout);
        }
        other => panic!("expected timeout disconnect, got {other:?}"),
    }
}

#[test]
fn full_receive_buffer_rejects_data_without_partial_mutation() {
    let (mut client, mut buffers, _, _) = establish_buffered_connection();
    let payload = ApplicationPayload::new(&[0x5a]).unwrap();

    for offset in 0..client.config().n_send_max() {
        let data = next_received_data(&client, &payload);
        client
            .step(
                &mut buffers,
                StepInput {
                    now: 1_030 + u32::from(offset),
                    event: ConnectionEvent::DataReceived(data),
                },
            )
            .unwrap();
    }
    assert_eq!(buffers.receive_usage().free, 0);
    let snapshot_before = client.snapshot();
    let buffers_before = buffers.clone();
    let data = next_received_data(&client, &payload);

    assert_eq!(
        client.step(
            &mut buffers,
            StepInput {
                now: 1_050,
                event: ConnectionEvent::DataReceived(data),
            },
        ),
        Err(EngineError::Buffer(
            rasta_rust::engine::buffers::BufferError::ReceiveBufferFull
        ))
    );
    assert_eq!(client.snapshot(), snapshot_before);
    assert_eq!(buffers, buffers_before);
}

#[test]
fn up_heartbeat_protocol_and_timing_failures_close_or_ignore() {
    let (mut client, mut server) = establish_connection();
    let outbound = step(&mut server, 1_020, ConnectionEvent::SendHeartbeat);
    let mut heartbeat = received_heartbeat(transmitted(&outbound, 0));
    heartbeat.header.confirmed_timestamp = heartbeat.header.confirmed_timestamp.wrapping_add(100);
    let bad_confirmation = step(
        &mut client,
        1_025,
        ConnectionEvent::HeartbeatReceived(heartbeat),
    );
    match transmitted(&bad_confirmation, 0) {
        OutboundMessage::DisconnectionRequest { reason, .. } => {
            assert_eq!(reason, DisconnectReason::ProtocolSequenceError);
        }
        other => panic!("expected disconnect, got {other:?}"),
    }

    let (mut client, mut server) = establish_connection();
    let outbound = step(&mut server, 1_020, ConnectionEvent::SendHeartbeat);
    let heartbeat = received_heartbeat(transmitted(&outbound, 0));
    let timed_out = step(
        &mut client,
        1_200,
        ConnectionEvent::HeartbeatReceived(heartbeat),
    );
    match transmitted(&timed_out, 0) {
        OutboundMessage::DisconnectionRequest { reason, .. } => {
            assert_eq!(reason, DisconnectReason::Timeout);
        }
        other => panic!("expected disconnect, got {other:?}"),
    }

    let (mut client, mut server) = establish_connection();
    let outbound = step(&mut server, 1_020, ConnectionEvent::SendHeartbeat);
    let mut heartbeat = received_heartbeat(transmitted(&outbound, 0));
    heartbeat.header.sequence_number = heartbeat.header.sequence_number.wrapping_add(101);
    let before = client.snapshot();
    let ignored = step(
        &mut client,
        1_025,
        ConnectionEvent::HeartbeatReceived(heartbeat),
    );
    assert!(ignored.effects.is_empty());
    assert_eq!(ignored.current, before);
}

#[test]
fn lower_protocol_version_is_rejected_with_an_acknowledging_disconnect() {
    let mut server = ConnectionEngine::new(config(2, 1));
    step(
        &mut server,
        10,
        ConnectionEvent::Open {
            initial_sequence_number: 50,
        },
    );
    let request = ReceivedConnectionMessage {
        header: SrHeader {
            receiver_id: 2,
            sender_id: 1,
            sequence_number: 77,
            confirmed_sequence_number: 0,
            timestamp: 12,
            confirmed_timestamp: 0,
        },
        data: ConnectionData::new(version(*b"0302"), 10),
    };

    let rejected = step(
        &mut server,
        13,
        ConnectionEvent::ConnectionRequestReceived(request),
    );
    match transmitted(&rejected, 0) {
        OutboundMessage::DisconnectionRequest { header, reason, .. } => {
            assert_eq!(reason, DisconnectReason::ProtocolVersionError);
            assert_eq!(header.sequence_number, 50);
            assert_eq!(header.confirmed_sequence_number, 77);
        }
        other => panic!("expected disconnect, got {other:?}"),
    }
    assert_eq!(rejected.current.state, ConnectionState::Closed);
}

#[test]
fn invalid_address_or_connection_confirmation_is_ignored() {
    let mut server = ConnectionEngine::new(config(2, 1));
    step(
        &mut server,
        10,
        ConnectionEvent::Open {
            initial_sequence_number: 50,
        },
    );
    let before = server.snapshot();
    let invalid = ReceivedConnectionMessage {
        header: SrHeader {
            receiver_id: 999,
            sender_id: 1,
            sequence_number: 77,
            confirmed_sequence_number: 1,
            timestamp: 12,
            confirmed_timestamp: 0,
        },
        data: ConnectionData::new(version(*b"0303"), 10),
    };

    let transition = step(
        &mut server,
        13,
        ConnectionEvent::ConnectionRequestReceived(invalid),
    );
    assert!(transition.effects.is_empty());
    // The address is checked first, so only the address error is counted.
    let mut expected = before;
    expected.diagnostics.address_errors = 1;
    assert_eq!(transition.current, expected);
}

#[test]
fn server_rejects_bad_handshake_sequence_and_confirmed_timestamp() {
    let (_, mut server, mut heartbeat) = handshake_pending_server_heartbeat();
    heartbeat.header.sequence_number = heartbeat.header.sequence_number.wrapping_add(1);

    let bad_sequence = step(
        &mut server,
        1_015,
        ConnectionEvent::HeartbeatReceived(heartbeat),
    );
    match transmitted(&bad_sequence, 0) {
        OutboundMessage::DisconnectionRequest { reason, .. } => {
            assert_eq!(reason, DisconnectReason::SequenceNumberError);
        }
        other => panic!("expected disconnect, got {other:?}"),
    }

    let (_, mut server, mut heartbeat) = handshake_pending_server_heartbeat();
    heartbeat.header.confirmed_timestamp = heartbeat.header.confirmed_timestamp.wrapping_add(100);
    let bad_timestamp = step(
        &mut server,
        1_015,
        ConnectionEvent::HeartbeatReceived(heartbeat),
    );
    match transmitted(&bad_timestamp, 0) {
        OutboundMessage::DisconnectionRequest { reason, .. } => {
            assert_eq!(reason, DisconnectReason::ProtocolSequenceError);
        }
        other => panic!("expected disconnect, got {other:?}"),
    }

    let (_, mut server) = establish_connection();
    let before = server.snapshot();
    let event = ConnectionEvent::Open {
        initial_sequence_number: 1,
    };
    let closed = step(&mut server, 2_000, event);
    assert_eq!(before.state, ConnectionState::Up);
    match transmitted(&closed, 0) {
        OutboundMessage::DisconnectionRequest { reason, .. } => {
            assert_eq!(reason, DisconnectReason::ServiceNotAllowed);
        }
        other => panic!("expected disconnect, got {other:?}"),
    }
}

#[test]
fn up_data_is_buffered_and_emitted_without_allocation() {
    let (mut client, _) = establish_connection();
    let mut buffers = ConnectionBuffers::new(client.config());
    let payload = ApplicationPayload::new(&[1, 2, 3, 4]).unwrap();
    let transition = client
        .step(
            &mut buffers,
            StepInput {
                now: 2_000,
                event: ConnectionEvent::SendData(payload),
            },
        )
        .unwrap();

    assert_eq!(buffers.send_usage().used, 1);
    assert_eq!(buffers.unconfirmed_messages(), 1);
    assert_eq!(buffers.unsent_messages(), 0);
    match transmitted(&transition, 0) {
        OutboundMessage::Data { header, data } => {
            assert_eq!(header.timestamp, 2_000);
            assert_eq!(data, ApplicationData::single(&payload));
            assert_eq!(data.as_field(), &[4, 0, 1, 2, 3, 4]);
        }
        other => panic!("expected data, got {other:?}"),
    }
}

#[test]
fn semantic_outbound_message_converts_to_codec_input() {
    let mut client = ConnectionEngine::new(config(1, 2));
    let transition = step(
        &mut client,
        20,
        ConnectionEvent::Open {
            initial_sequence_number: 30,
        },
    );
    let outbound = transmitted(&transition, 1);
    let message = outbound.as_sr_message();

    assert_eq!(message.header.sequence_number, 30);
    assert_eq!(
        message.body,
        SrBody::ConnectionRequest(ConnectionData::new(version(*b"0303"), 10))
    );
    assert!(message.safety_code.is_empty());
}

#[test]
fn snapshots_are_explicit_and_copyable() {
    let snapshot = ConnectionSnapshot {
        state: ConnectionState::RetransmissionRequest,
        next_send_sequence: 1,
        last_transmitted_sequence: Some(0),
        next_receive_sequence: 2,
        confirmed_send_sequence: 3,
        confirmed_receive_sequence: 4,
        received_timestamp: 5,
        confirmed_received_timestamp: 6,
        remote_n_send_max: 7,
        timing: rasta_rust::engine::timing::TimingState {
            timer_t_i: 8,
            timer_started_at: 13,
            t_rtd: 9,
            t_alive: 10,
            last_sent_confirmed_sequence: 11,
            last_sent_timestamp: 12,
        },
        diagnostics: rasta_rust::engine::diagnostics::ConnectionDiagnostics::default(),
    };
    assert_eq!(snapshot, snapshot);
}

fn packed(messages: &[&[u8]]) -> ApplicationData {
    let mut data = ApplicationData::new();
    for message in messages {
        data.push(message).unwrap();
    }
    data
}

fn packing(local_id: u32, remote_id: u32, max_packet: u16) -> ConnectionConfig {
    config(local_id, remote_id)
        .with_max_packet(max_packet)
        .expect("test N_maxPacket is valid")
}

#[allow(clippy::large_types_passed_by_value)]
fn send(
    engine: &mut ConnectionEngine,
    buffers: &mut ConnectionBuffers,
    now: u32,
    event: ConnectionEvent,
) -> Result<Transition, EngineError> {
    engine.step(buffers, StepInput { now, event })
}

fn assert_closed_with(transition: &Transition, expected: DisconnectReason) {
    assert_eq!(transition.current.state, ConnectionState::Closed);
    match transmitted(transition, 0) {
        OutboundMessage::DisconnectionRequest { reason, .. } => assert_eq!(reason, expected),
        other => panic!("expected disconnect, got {other:?}"),
    }
}

#[test]
fn max_packet_defaults_to_one_and_is_bounded() {
    assert_eq!(config(1, 2).max_packet(), DEFAULT_MAX_PACKET);
    assert_eq!(DEFAULT_MAX_PACKET, 1);
    assert_eq!(packing(1, 2, MAX_MAX_PACKET).max_packet(), MAX_MAX_PACKET);
    for invalid in [0, MAX_MAX_PACKET + 1] {
        assert_eq!(
            config(1, 2).with_max_packet(invalid),
            Err(ConfigurationError::InvalidMaxPacket(invalid))
        );
    }
}

#[test]
fn packed_data_is_sent_in_one_message_and_delivered_one_by_one() {
    let (mut client, mut client_buffers, mut server, mut server_buffers) =
        establish_buffered_connection_with(packing(1, 2, 3), packing(2, 1, 3));
    let data = packed(&[b"a", b"bc", b"def"]);
    let used_before = server_buffers.send_usage().used;

    let sent = send(
        &mut server,
        &mut server_buffers,
        1_030,
        ConnectionEvent::SendPacked(data),
    )
    .unwrap();
    assert_eq!(
        server_buffers.send_usage().used,
        used_before + 1,
        "one Data message"
    );
    let message = transmitted(&sent, 0);
    assert!(matches!(message, OutboundMessage::Data { .. }));
    let received = received_data(message);
    assert_eq!(received.data, data);
    assert_eq!(
        received.data.as_field(),
        &[1, 0, b'a', 2, 0, b'b', b'c', 3, 0, b'd', b'e', b'f']
    );

    let accepted = send(
        &mut client,
        &mut client_buffers,
        1_035,
        ConnectionEvent::DataReceived(received),
    )
    .unwrap();
    assert_eq!(accepted.current.state, ConnectionState::Up);
    for expected in [&b"a"[..], b"bc", b"def"] {
        assert_eq!(
            client_buffers.pop_received(),
            Some(ApplicationPayload::new(expected).unwrap())
        );
    }
    assert_eq!(client_buffers.pop_received(), None);
}

#[test]
fn sending_more_than_max_packet_messages_is_rejected_without_a_state_change() {
    let (_, _, mut server, mut server_buffers) =
        establish_buffered_connection_with(packing(1, 2, 2), packing(2, 1, 2));
    let snapshot = server.snapshot();
    let usage = server_buffers.send_usage();

    for data in [packed(&[b"a", b"b", b"c"]), ApplicationData::new()] {
        assert_eq!(
            send(
                &mut server,
                &mut server_buffers,
                1_030,
                ConnectionEvent::SendPacked(data),
            ),
            Err(EngineError::InvalidApplicationData)
        );
        assert_eq!(server.snapshot(), snapshot);
        assert_eq!(server_buffers.send_usage(), usage);
    }

    send(
        &mut server,
        &mut server_buffers,
        1_030,
        ConnectionEvent::SendPacked(packed(&[b"a", b"b"])),
    )
    .unwrap();
}

#[test]
fn receiving_more_than_max_packet_messages_closes_the_connection() {
    let (mut client, mut client_buffers, _, _) = establish_buffered_connection();
    let mut received = next_received_data(&client, &ApplicationPayload::new(b"a").unwrap());
    received.data = packed(&[b"a", b"b"]);

    let closed = send(
        &mut client,
        &mut client_buffers,
        1_035,
        ConnectionEvent::DataReceived(received),
    )
    .unwrap();
    assert_closed_with(&closed, DisconnectReason::UnexpectedMessage);
    assert_eq!(client_buffers.pop_received(), None);
}

#[test]
fn a_malformed_application_data_field_closes_the_connection() {
    let malformed: [&[u8]; 3] = [&[5, 0, 1], &[1, 0, 1, 0, 0], &[1, 0, 1, 9]];
    for field in malformed {
        let (mut client, mut client_buffers, _, _) =
            establish_buffered_connection_with(packing(1, 2, 3), packing(2, 1, 3));
        let mut received = next_received_data(&client, &ApplicationPayload::new(b"a").unwrap());
        received.data = ApplicationData::from_field(field).unwrap();

        let closed = send(
            &mut client,
            &mut client_buffers,
            1_035,
            ConnectionEvent::DataReceived(received),
        )
        .unwrap();
        assert_closed_with(&closed, DisconnectReason::UnexpectedMessage);
        assert_eq!(client_buffers.pop_received(), None);
    }
}

#[test]
fn packed_data_is_retransmitted_unchanged() {
    let (_, _, mut server, mut server_buffers) =
        establish_buffered_connection_with(packing(1, 2, 3), packing(2, 1, 3));
    let data = packed(&[b"first", b"second"]);
    let sent = send(
        &mut server,
        &mut server_buffers,
        1_030,
        ConnectionEvent::SendPacked(data),
    )
    .unwrap();
    let sent_sequence = transmitted(&sent, 0).header().sequence_number;
    let request = SrHeader {
        receiver_id: server.config().local_id(),
        sender_id: server.config().remote_id(),
        sequence_number: server.snapshot().next_receive_sequence,
        confirmed_sequence_number: sent_sequence.wrapping_sub(1),
        timestamp: 1_035,
        confirmed_timestamp: server.snapshot().confirmed_received_timestamp,
    };
    send(
        &mut server,
        &mut server_buffers,
        1_035,
        ConnectionEvent::RetransmissionRequestReceived(request),
    )
    .unwrap();

    let replay = send(
        &mut server,
        &mut server_buffers,
        1_040,
        ConnectionEvent::SendHeartbeat,
    )
    .unwrap();
    match transmitted(&replay, 0) {
        OutboundMessage::RetransmittedData { data: replayed, .. } => assert_eq!(replayed, data),
        other => panic!("expected retransmitted data, got {other:?}"),
    }
}

#[test]
fn retransmitted_data_above_max_packet_closes_the_connection() {
    let (mut client, mut client_buffers, mut server, mut server_buffers) =
        establish_buffered_connection();
    let outbound = send(
        &mut server,
        &mut server_buffers,
        1_040,
        ConnectionEvent::SendHeartbeat,
    )
    .unwrap();
    let mut heartbeat = received_heartbeat(transmitted(&outbound, 0));
    heartbeat.header.sequence_number = heartbeat.header.sequence_number.wrapping_add(1);
    let retr_req = send(
        &mut client,
        &mut client_buffers,
        1_045,
        ConnectionEvent::HeartbeatReceived(heartbeat),
    )
    .unwrap();
    let response = SrHeader {
        receiver_id: client.config().local_id(),
        sender_id: client.config().remote_id(),
        sequence_number: retr_req.current.next_receive_sequence,
        confirmed_sequence_number: retr_req.current.confirmed_receive_sequence,
        timestamp: 1_050,
        confirmed_timestamp: retr_req.current.confirmed_received_timestamp,
    };
    send(
        &mut client,
        &mut client_buffers,
        1_050,
        ConnectionEvent::RetransmissionResponseReceived(response),
    )
    .unwrap();

    let mut retransmitted = next_received_data(&client, &ApplicationPayload::new(b"a").unwrap());
    retransmitted.data = packed(&[b"a", b"b"]);
    let closed = send(
        &mut client,
        &mut client_buffers,
        1_055,
        ConnectionEvent::RetransmittedDataReceived(retransmitted),
    )
    .unwrap();
    assert_closed_with(&closed, DisconnectReason::UnexpectedMessage);
    assert_eq!(client_buffers.pop_received(), None);
}

fn with_diagnostic_window(local_id: u32, remote_id: u32, window: u32) -> ConnectionConfig {
    let diagnostics = DiagnosticsConfig::new(window, [20, 40, 60, 80]).unwrap();
    ConnectionConfig::new(local_id, remote_id, version(*b"0303"), 10, 100, diagnostics).unwrap()
}

#[test]
fn diagnostic_intervals_must_end_below_maximum_time() {
    let diagnostics = DiagnosticsConfig::new(5_000, [20, 40, 60, 100]).unwrap();
    assert_eq!(
        ConnectionConfig::new(1, 2, version(*b"0303"), 10, 100, diagnostics),
        Err(ConfigurationError::Diagnostics(
            DiagnosticsConfigurationError::LastBoundNotBelowMaximum
        ))
    );
    assert!(ConnectionConfig::new(1, 2, version(*b"0303"), 10, 101, diagnostics).is_ok());
}

#[test]
fn a_sequence_gap_is_counted_and_still_handled() {
    let (mut client, mut server) = establish_connection();
    step(&mut server, 1_020, ConnectionEvent::SendHeartbeat); // lost
    let next = received_heartbeat(transmitted(
        &step(&mut server, 1_021, ConnectionEvent::SendHeartbeat),
        0,
    ));
    let transition = step(&mut client, 1_022, ConnectionEvent::HeartbeatReceived(next));
    assert_eq!(transition.current.diagnostics.sequence_errors, 1);
    assert_eq!(
        transition.current.state,
        ConnectionState::RetransmissionRequest
    );
}

#[test]
fn failed_range_and_timestamp_checks_are_not_counted() {
    let (mut client, mut server) = establish_connection();
    let template = received_heartbeat(transmitted(
        &step(&mut server, 1_020, ConnectionEvent::SendHeartbeat),
        0,
    ));
    let before = client.snapshot();
    let mut far = template;
    far.header.sequence_number = before.next_receive_sequence.wrapping_add(10 * 10 + 1);
    // A sequence gap, but behind the last accepted timestamp.
    let mut stale = template;
    stale.header.sequence_number = template.header.sequence_number.wrapping_add(1);
    stale.header.timestamp = before.received_timestamp.wrapping_sub(1);
    for message in [far, stale] {
        let transition = step(
            &mut client,
            1_025,
            ConnectionEvent::HeartbeatReceived(message),
        );
        assert!(transition.effects.is_empty());
        assert_eq!(transition.current.diagnostics, before.diagnostics);
    }
}

#[test]
fn diagnostics_are_reported_and_reset_after_each_window() {
    let (mut client, mut client_buffers, mut server, mut server_buffers) =
        establish_buffered_connection_with(
            with_diagnostic_window(1, 2, 2),
            with_diagnostic_window(2, 1, 2),
        );
    // The window may already hold messages accepted during establishment.
    let needed = 2 - client.snapshot().diagnostics.messages();
    let mut reports = Vec::new();
    for now in (0..needed).map(|index| 1_020 + 10 * index) {
        let heartbeat = send(
            &mut server,
            &mut server_buffers,
            now,
            ConnectionEvent::SendHeartbeat,
        )
        .unwrap();
        let received = send(
            &mut client,
            &mut client_buffers,
            now + 1,
            ConnectionEvent::HeartbeatReceived(received_heartbeat(transmitted(&heartbeat, 0))),
        )
        .unwrap();
        reports.extend(received.effects.iter().filter_map(|effect| match effect {
            Effect::Diagnostics(diagnostics) => Some(*diagnostics),
            _ => None,
        }));
    }
    assert_eq!(reports.len(), 1);
    assert_eq!(reports[0].messages(), 2);
    assert_eq!(reports[0].alive_times.iter().sum::<u32>(), 2);
    assert_eq!(
        client.snapshot().diagnostics,
        ConnectionDiagnostics::default()
    );
}
