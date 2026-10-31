//! Idle links in deterministic simulated time (ADR 0004): heartbeats alone
//! keep a connection up, and a silent peer is detected within `T_max`.

use rasta_rust::{
    codec::{
        safety_code::{Md4InitialValue, SafetyCodeLength},
        sr::{DisconnectReason, ProtocolVersion, SrCodecConfig},
    },
    engine::{
        connection::{
            ConnectionConfig, ConnectionEvent, ConnectionState, StepInput,
            DEFAULT_TIMER_GRANULARITY,
        },
        diagnostics::DiagnosticsConfig,
        timing::TimingConfig,
    },
    redundancy::{codec::CheckCodeType, engine::RedundancyConfig},
    stack::{StackAction, StackActions, StackCodecConfig, StackEngine},
    transport::simulation::DeterministicNetwork,
};

/// Simulated polling interval; every datagram arrives one interval later.
const POLL_MS: u32 = 10;

/// A connection that left `Up`: node index, simulated time and reason.
type Drop = (usize, u32, Option<DisconnectReason>);

struct Pair {
    nodes: [Box<StackEngine>; 2],
    network: DeterministicNetwork,
    silent: [bool; 2],
    drops: Vec<Drop>,
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

fn stack(local: u32, remote: u32, timing: Option<TimingConfig>) -> Box<StackEngine> {
    let version = ProtocolVersion::new(*b"0303").unwrap();
    let connection = match timing {
        Some(timing) => ConnectionConfig::new_with_timing(
            local,
            remote,
            version,
            10,
            timing,
            diagnostics(timing.t_max()),
        ),
        None => ConnectionConfig::new(local, remote, version, 10, 1_000, diagnostics(1_000)),
    }
    .unwrap();
    Box::new(StackEngine::new(
        connection,
        RedundancyConfig::new(50, 4, 1, 200).unwrap(),
        StackCodecConfig {
            sr: SrCodecConfig::with_default_limits(SafetyCodeLength::LowerMd4),
            md4_initial_value: Md4InitialValue::STANDARD,
            redundancy_check_code: CheckCodeType::C,
        },
    ))
}

impl Pair {
    /// A client (node 0) and a server (node 1), connected at simulated time 0.
    fn connected(timing: Option<TimingConfig>) -> Self {
        let mut pair = Self {
            nodes: [stack(1, 2, timing), stack(2, 1, timing)],
            network: DeterministicNetwork::new(64).unwrap(),
            silent: [false; 2],
            drops: Vec::new(),
        };
        for (node, initial_sequence_number) in [(1, 100), (0, 7)] {
            let actions = pair.nodes[node]
                .step_connection(StepInput {
                    now: 0,
                    event: ConnectionEvent::Open {
                        initial_sequence_number,
                    },
                })
                .unwrap();
            pair.handle(node, 0, &actions);
        }
        pair.run(0, 200);
        for node in &pair.nodes {
            assert_eq!(node.connection_snapshot().state, ConnectionState::Up);
        }
        pair
    }

    fn handle(&mut self, node: usize, now: u32, actions: &StackActions) {
        for action in actions.iter() {
            match action {
                StackAction::Transmit(datagram) => {
                    let (source, destination) = if node == 0 { (0, 1) } else { (1, 0) };
                    self.network
                        .send(
                            source,
                            destination,
                            datagram.channel(),
                            now,
                            datagram.as_slice(),
                        )
                        .unwrap();
                }
                StackAction::ConnectionStateChanged(change)
                    if change.previous == ConnectionState::Up
                        && change.current != ConnectionState::Up =>
                {
                    self.drops.push((node, now, change.disconnect_reason));
                }
                _ => {}
            }
        }
    }

    /// Runs both nodes from `from` to `until`: first every node receives what
    /// is due, then every node polls its timers. A silent node does neither,
    /// and datagrams to it are lost.
    fn run(&mut self, from: u32, until: u32) {
        let mut now = from;
        while now <= until {
            for node in 0..2 {
                let index = u8::try_from(node).unwrap();
                while let Some(datagram) = self.network.receive(index, now) {
                    if self.silent[node] {
                        continue;
                    }
                    let actions = self.nodes[node]
                        .receive_datagram(now, datagram.channel, &datagram.bytes)
                        .unwrap();
                    self.handle(node, now, &actions);
                }
            }
            for node in 0..2 {
                if !self.silent[node] {
                    let actions = self.nodes[node].poll(now).unwrap();
                    self.handle(node, now, &actions);
                }
            }
            now += POLL_MS;
        }
    }
}

#[test]
fn an_idle_link_stays_up_with_the_derived_heartbeat_period() {
    // `ConnectionConfig::new` with `T_max` 1000 ms derives `T_h` 250 ms.
    let mut pair = Pair::connected(None);
    pair.run(200, 60_000);
    assert!(pair.drops.is_empty(), "dropped: {:?}", pair.drops);
}

#[test]
fn an_idle_link_stays_up_with_t_max_750_and_t_h_300() {
    // `T_max` 750 ms and `T_h` 300 ms: the SBB reference defaults, under which
    // two SBB endpoints time out on an idle link (OBS-SBB-TIME-005).
    let timing = TimingConfig::new(750, 300, DEFAULT_TIMER_GRANULARITY, 9, 10).unwrap();
    let mut pair = Pair::connected(Some(timing));
    pair.run(200, 60_000);
    assert!(pair.drops.is_empty(), "dropped: {:?}", pair.drops);
}

#[test]
fn a_silent_peer_is_detected_within_t_max() {
    let mut pair = Pair::connected(None);
    pair.run(200, 10_000);
    pair.silent[1] = true;
    pair.run(10_010, 13_000);

    assert_eq!(pair.drops.len(), 1, "drops: {:?}", pair.drops);
    let (node, at, reason) = pair.drops[0];
    assert_eq!((node, reason), (0, Some(DisconnectReason::Timeout)));
    // The client's last confirmation was sent before the server fell silent,
    // so the timeout follows within `T_max` (1000 ms) plus one poll interval.
    assert!(
        at > 10_010 && at <= 10_010 + 1_000 + POLL_MS,
        "timed out at {at}"
    );
}
