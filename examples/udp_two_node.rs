//! Two `StackEngine` nodes connect over two UDP loopback paths, exchange one
//! application message, and close the connection.

use std::{
    error::Error,
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
        codec::{CheckCodeType, MAX_REDUNDANCY_FRAME_LEN},
        engine::RedundancyConfig,
    },
    stack::{StackAction, StackActions, StackCodecConfig, StackEngine, StackWorkspace},
    transport::udp::{ProtocolClock, UdpChannel, UdpRedundancyAdapter},
};

type Result<T> = std::result::Result<T, Box<dyn Error>>;

fn loopback() -> SocketAddr {
    SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 0))
}

/// Binds the two ends of one network path.
fn path() -> Result<(UdpChannel, UdpChannel)> {
    let first = UdpSocket::bind(loopback())?;
    let second = UdpSocket::bind(loopback())?;
    let (first_addr, second_addr) = (first.local_addr()?, second.local_addr()?);
    Ok((
        UdpChannel::from_socket(first, second_addr)?,
        UdpChannel::from_socket(second, first_addr)?,
    ))
}

fn configuration(
    local: u32,
    remote: u32,
) -> (ConnectionConfig, RedundancyConfig, StackCodecConfig) {
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
    .and_then(|config| config.with_timer_granularity(ProtocolClock::GRANULARITY))
    .expect("fixed connection configuration is valid");
    let redundancy =
        RedundancyConfig::new(50, 4, 2, 200).expect("fixed redundancy configuration is valid");
    let codecs = StackCodecConfig {
        sr: SrCodecConfig::with_default_limits(SafetyCodeLength::LowerMd4),
        md4_initial_value: Md4InitialValue::STANDARD,
        redundancy_check_code: CheckCodeType::C,
    };
    (connection, redundancy, codecs)
}

// The engine, workspace, and actions are large; this example keeps them on
// the heap.
struct Node {
    name: &'static str,
    engine: Box<StackEngine>,
    adapter: UdpRedundancyAdapter,
}

impl Node {
    fn new(name: &'static str, local: u32, remote: u32, paths: [UdpChannel; 2]) -> Self {
        let (connection, redundancy, codecs) = configuration(local, remote);
        let [first, second] = paths;
        Self {
            name,
            engine: Box::new(StackEngine::new(connection, redundancy, codecs)),
            adapter: UdpRedundancyAdapter::new(first, Some(second), codecs.redundancy_check_code),
        }
    }

    fn state(&self) -> ConnectionState {
        self.engine.connection_snapshot().state
    }

    #[allow(clippy::large_types_passed_by_value)]
    fn step(&mut self, now: u32, event: ConnectionEvent, io: &mut Io) -> Result<()> {
        self.engine.step_connection_into(
            StepInput { now, event },
            &mut io.workspace,
            &mut io.actions,
        )?;
        self.execute(&io.actions)
    }

    /// Feeds waiting datagrams to the engine while it can take them, then
    /// services its timers. Datagrams it cannot take yet stay in the socket.
    fn service(&mut self, now: u32, io: &mut Io) -> Result<()> {
        let mut buffer = [0_u8; MAX_REDUNDANCY_FRAME_LEN];
        for channel in 0..self.adapter.channel_count() {
            while self.engine.can_receive_datagram() {
                let Some(datagram) = self.adapter.receive_datagram(channel, &mut buffer)? else {
                    break;
                };
                self.engine.receive_datagram_into(
                    now,
                    channel,
                    datagram,
                    &mut io.workspace,
                    &mut io.actions,
                )?;
                self.execute(&io.actions)?;
            }
        }
        self.engine
            .poll_into(now, &mut io.workspace, &mut io.actions)?;
        self.execute(&io.actions)
    }

    fn execute(&self, actions: &StackActions) -> Result<()> {
        for action in actions.iter() {
            match action {
                StackAction::Transmit(datagram) => self.adapter.send_datagram(datagram)?,
                StackAction::ConnectionStateChanged(change) => println!(
                    "{}: {:?} -> {:?}",
                    self.name, change.previous, change.current
                ),
                _ => {}
            }
        }
        Ok(())
    }
}

/// Transaction workspace and output batch, reused by both nodes in turn.
struct Io {
    workspace: StackWorkspace,
    actions: StackActions,
}

impl Io {
    fn new() -> Box<Self> {
        let (connection, redundancy, codecs) = configuration(1, 2);
        Box::new(Self {
            workspace: StackWorkspace::new(connection, redundancy, codecs),
            actions: StackActions::new(),
        })
    }
}

fn main() -> Result<()> {
    let (client_first, server_first) = path()?;
    let (client_second, server_second) = path()?;
    let mut client = Node::new("client", 1, 2, [client_first, client_second]);
    let mut server = Node::new("server", 2, 1, [server_first, server_second]);
    let mut io = Io::new();
    let clock = ProtocolClock::start();

    // A real host supplies random initial sequence numbers.
    server.step(
        clock.now(),
        ConnectionEvent::Open {
            initial_sequence_number: 0x1000,
        },
        &mut io,
    )?;
    client.step(
        clock.now(),
        ConnectionEvent::Open {
            initial_sequence_number: 0x2000,
        },
        &mut io,
    )?;

    let payload = ApplicationPayload::new(b"hello over two UDP paths").expect("payload fits");
    let mut sent = false;
    for _ in 0..2_000 {
        client.service(clock.now(), &mut io)?;
        server.service(clock.now(), &mut io)?;
        if !sent && client.state() == ConnectionState::Up && server.state() == ConnectionState::Up {
            client.step(clock.now(), ConnectionEvent::SendData(payload), &mut io)?;
            sent = true;
        }
        if let Some(received) = server.engine.pop_received() {
            println!(
                "server received {:?}",
                String::from_utf8_lossy(received.as_slice())
            );
            client.step(clock.now(), ConnectionEvent::Close, &mut io)?;
            for _ in 0..1_000 {
                server.service(clock.now(), &mut io)?;
                if server.state() == ConnectionState::Closed {
                    return Ok(());
                }
                thread::sleep(Duration::from_millis(1));
            }
            return Err("server did not receive the disconnection request".into());
        }
        thread::sleep(Duration::from_millis(1));
    }
    Err("timed out before the message arrived".into())
}
