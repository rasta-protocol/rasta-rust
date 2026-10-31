use rasta_rust::{
    codec::{
        safety_code::{Md4InitialValue, SafetyCodeLength},
        sr::{ProtocolVersion, SrCodecConfig},
    },
    engine::{
        buffers::ApplicationPayload,
        connection::{ConnectionConfig, ConnectionEvent, StepInput},
        diagnostics::DiagnosticsConfig,
    },
    redundancy::{codec::CheckCodeType, engine::RedundancyConfig},
    stack::{
        StackAction, StackActions, StackCodecConfig, StackEngine, StackWorkspace, WireDatagram,
    },
};

// The engine and workspace are large; this example keeps them on the heap.
fn stack(local: u32, remote: u32) -> (Box<StackEngine>, Box<StackWorkspace>) {
    // Report diagnostics every 5000 accepted messages, with timing intervals
    // in fifths of T_max (the SBB reference uses 150, 300, 450 and 600 ms for
    // its T_max of 750 ms).
    let diagnostics = DiagnosticsConfig::new(5_000, [200, 400, 600, 800])
        .expect("fixed diagnostics configuration is valid");
    let connection = ConnectionConfig::new(
        local,
        remote,
        ProtocolVersion::new(*b"0303").expect("fixed version is valid"),
        10,
        1_000,
        diagnostics,
    )
    .expect("fixed connection configuration is valid");
    let redundancy =
        RedundancyConfig::new(50, 4, 2, 200).expect("fixed redundancy configuration is valid");
    let codecs = StackCodecConfig {
        sr: SrCodecConfig::with_default_limits(SafetyCodeLength::LowerMd4),
        md4_initial_value: Md4InitialValue::STANDARD,
        redundancy_check_code: CheckCodeType::C,
    };
    (
        Box::new(StackEngine::new(connection, redundancy, codecs)),
        Box::new(StackWorkspace::new(connection, redundancy, codecs)),
    )
}

fn channel_zero(actions: &StackActions) -> WireDatagram {
    actions
        .iter()
        .find_map(|action| match action {
            StackAction::Transmit(datagram) if datagram.channel() == 0 => Some(*datagram),
            _ => None,
        })
        .expect("protocol step emits channel zero")
}

fn main() {
    let (mut client, mut workspace) = stack(1, 2);
    let (mut server, _) = stack(2, 1);
    // Reuse one transaction workspace and output batch across both nodes.
    // Embedded callers should place these large persistent values in static
    // storage or an arena before entering a small-stack protocol task.
    let mut actions = Box::new(StackActions::new());
    server
        .step_connection_into(
            StepInput {
                now: 0,
                event: ConnectionEvent::Open {
                    initial_sequence_number: 100,
                },
            },
            &mut workspace,
            &mut actions,
        )
        .expect("server opens");
    client
        .step_connection_into(
            StepInput {
                now: 1,
                event: ConnectionEvent::Open {
                    initial_sequence_number: 200,
                },
            },
            &mut workspace,
            &mut actions,
        )
        .expect("client opens");
    let request = channel_zero(&actions);
    server
        .receive_datagram_into(2, 0, request.as_slice(), &mut workspace, &mut actions)
        .expect("server accepts request");
    let response = channel_zero(&actions);
    client
        .receive_datagram_into(3, 0, response.as_slice(), &mut workspace, &mut actions)
        .expect("client accepts response");
    let heartbeat = channel_zero(&actions);
    server
        .receive_datagram_into(4, 0, heartbeat.as_slice(), &mut workspace, &mut actions)
        .expect("server accepts heartbeat");

    let payload =
        ApplicationPayload::new(b"application-owned custom payload").expect("payload fits");
    client
        .step_connection_into(
            StepInput {
                now: 5,
                event: ConnectionEvent::SendData(payload),
            },
            &mut workspace,
            &mut actions,
        )
        .expect("client sends");
    let sent = channel_zero(&actions);
    server
        .receive_datagram_into(6, 0, sent.as_slice(), &mut workspace, &mut actions)
        .expect("server receives");
    let delivered = server.pop_received().expect("payload was delivered");
    println!("{}", String::from_utf8_lossy(delivered.as_slice()));
}
