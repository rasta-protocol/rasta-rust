//! An interactive `RaSTA` node over one or two UDP paths.
//!
//! Run two nodes in two terminals, or one node against another `RaSTA`
//! implementation; `docs/trying-it-out.md` has the commands. Each line typed
//! on standard input is sent as one application message, newline included.
//! `/close` closes the connection, `/open` opens it again, and `/quit` or the
//! end of input closes it and exits. Received messages, state changes,
//! discarded messages, and diagnostics are printed.

use std::{
    error::Error,
    io::{self, BufRead, Write},
    net::SocketAddr,
    process,
    sync::mpsc::{self, Receiver, TryRecvError},
    thread,
    time::{Duration, SystemTime, UNIX_EPOCH},
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

const USAGE: &str = "\
usage: udp_peer --local-id ID --remote-id ID --bind ADDR[,ADDR] --peer ADDR[,ADDR] [options]

  --safety-code none|lower|full   SR safety code (default lower)
  --check-code a|b|c|d|e          RL check code; a is none (default c)
  --t-max MS                      T_max in milliseconds (default 1000)
  --n-send-max N                  local N_SENDMAX, 2 to 20 (default 10)
  --max-packet N                  N_maxPacket, messages per Data message (default 1)
  --diagnostic-window N           SR and RL diagnostic window (default 20)

IDs are decimal or 0x-prefixed hexadecimal; the lower ID is the client.
--bind and --peer list the same number of paths, one or two.";

struct Options {
    local_id: u32,
    remote_id: u32,
    binds: Vec<SocketAddr>,
    peers: Vec<SocketAddr>,
    safety_code: SafetyCodeLength,
    check_code: CheckCodeType,
    t_max: u32,
    n_send_max: u16,
    max_packet: u16,
    diagnostic_window: u16,
}

fn parse_options(mut args: impl Iterator<Item = String>) -> Result<Options> {
    let mut options = Options {
        local_id: 0,
        remote_id: 0,
        binds: Vec::new(),
        peers: Vec::new(),
        safety_code: SafetyCodeLength::LowerMd4,
        check_code: CheckCodeType::C,
        t_max: 1_000,
        n_send_max: 10,
        max_packet: 1,
        diagnostic_window: 20,
    };
    let (mut local, mut remote) = (None, None);
    while let Some(flag) = args.next() {
        let value = args.next().ok_or(format!("{flag} needs a value"))?;
        match flag.as_str() {
            "--local-id" => local = Some(parse_id(&value)?),
            "--remote-id" => remote = Some(parse_id(&value)?),
            "--bind" => options.binds = parse_addresses(&value)?,
            "--peer" => options.peers = parse_addresses(&value)?,
            "--safety-code" => options.safety_code = parse_safety_code(&value)?,
            "--check-code" => options.check_code = parse_check_code(&value)?,
            "--t-max" => options.t_max = value.parse()?,
            "--n-send-max" => options.n_send_max = value.parse()?,
            "--max-packet" => options.max_packet = value.parse()?,
            "--diagnostic-window" => options.diagnostic_window = value.parse()?,
            _ => return Err(format!("unknown option {flag}").into()),
        }
    }
    options.local_id = local.ok_or("--local-id is required")?;
    options.remote_id = remote.ok_or("--remote-id is required")?;
    if options.binds.is_empty() || options.binds.len() > 2 {
        return Err("--bind needs one or two addresses".into());
    }
    if options.peers.len() != options.binds.len() {
        return Err("--peer needs as many addresses as --bind".into());
    }
    Ok(options)
}

fn parse_id(value: &str) -> Result<u32> {
    Ok(match value.strip_prefix("0x") {
        Some(hex) => u32::from_str_radix(hex, 16)?,
        None => value.parse()?,
    })
}

fn parse_addresses(value: &str) -> Result<Vec<SocketAddr>> {
    value
        .split(',')
        .map(|address| Ok(address.parse()?))
        .collect()
}

fn parse_safety_code(value: &str) -> Result<SafetyCodeLength> {
    match value {
        "none" => Ok(SafetyCodeLength::None),
        "lower" => Ok(SafetyCodeLength::LowerMd4),
        "full" => Ok(SafetyCodeLength::FullMd4),
        _ => Err(format!("unknown safety code {value}").into()),
    }
}

fn parse_check_code(value: &str) -> Result<CheckCodeType> {
    match value {
        "a" => Ok(CheckCodeType::A),
        "b" => Ok(CheckCodeType::B),
        "c" => Ok(CheckCodeType::C),
        "d" => Ok(CheckCodeType::D),
        "e" => Ok(CheckCodeType::E),
        _ => Err(format!("unknown check code {value}").into()),
    }
}

// The engine and workspace are large; this example keeps them on the heap.
struct Node {
    engine: Box<StackEngine>,
    workspace: Box<StackWorkspace>,
    actions: Box<StackActions>,
    adapter: UdpRedundancyAdapter,
}

impl Node {
    fn new(options: &Options) -> Result<Self> {
        let t_max = options.t_max;
        let diagnostics = DiagnosticsConfig::new(
            u32::from(options.diagnostic_window),
            [t_max / 5, 2 * t_max / 5, 3 * t_max / 5, 4 * t_max / 5],
        )?;
        let connection = ConnectionConfig::new(
            options.local_id,
            options.remote_id,
            ProtocolVersion::new(*b"0303")?,
            options.n_send_max,
            t_max,
            diagnostics,
        )?
        .with_max_packet(options.max_packet)?
        .with_timer_granularity(ProtocolClock::GRANULARITY)?;
        let paths = u8::try_from(options.binds.len())?;
        let redundancy = RedundancyConfig::new(50, 4, paths, options.diagnostic_window)?;
        let codecs = StackCodecConfig {
            sr: SrCodecConfig::with_default_limits(options.safety_code),
            md4_initial_value: Md4InitialValue::STANDARD,
            redundancy_check_code: options.check_code,
        };
        let mut channels = options
            .binds
            .iter()
            .zip(&options.peers)
            .map(|(bind, peer)| UdpChannel::bind(*bind, *peer));
        let first = channels.next().ok_or("no path configured")??;
        let second = channels.next().transpose()?;
        Ok(Self {
            engine: Box::new(StackEngine::new(connection, redundancy, codecs)),
            workspace: Box::new(StackWorkspace::new(connection, redundancy, codecs)),
            actions: Box::new(StackActions::new()),
            adapter: UdpRedundancyAdapter::new(first, second, options.check_code),
        })
    }

    #[allow(clippy::large_types_passed_by_value)]
    fn step(&mut self, now: u32, event: ConnectionEvent) {
        let result = self.engine.step_connection_into(
            StepInput { now, event },
            &mut self.workspace,
            &mut self.actions,
        );
        match result {
            Ok(()) => self.report(),
            Err(error) => println!("! {error}"),
        }
    }

    /// Sends one line as an application message once the connection is up.
    fn send(&mut self, now: u32, line: &str) {
        let state = self.engine.connection_snapshot().state;
        if matches!(
            state,
            ConnectionState::Closed | ConnectionState::Down | ConnectionState::Start
        ) {
            // Data before the connection is up closes it (ServiceNotAllowed).
            println!("! not connected yet ({state:?}); message not sent");
            return;
        }
        // Receiving stops while fewer than three send-buffer entries are free.
        if self.engine.send_usage().free <= 3 {
            println!("! send buffer nearly full; message not sent");
            return;
        }
        match ApplicationPayload::new(format!("{line}\n").as_bytes()) {
            Ok(payload) => self.step(now, ConnectionEvent::SendData(payload)),
            Err(error) => println!("! {error}"),
        }
    }

    /// Feeds waiting datagrams to the engine while it can take them, then
    /// services its timers.
    fn service(&mut self, now: u32) {
        let mut buffer = [0_u8; MAX_REDUNDANCY_FRAME_LEN];
        for channel in 0..self.adapter.channel_count() {
            while self.engine.can_receive_datagram() {
                let datagram = match self.adapter.receive_datagram(channel, &mut buffer) {
                    Ok(Some(datagram)) => datagram,
                    Ok(None) => break,
                    Err(error) => {
                        println!("! path {channel}: {error}");
                        break;
                    }
                };
                let result = self.engine.receive_datagram_into(
                    now,
                    channel,
                    datagram,
                    &mut self.workspace,
                    &mut self.actions,
                );
                match result {
                    Ok(()) => self.report(),
                    Err(error) => println!("! path {channel}: {error}"),
                }
            }
        }
        match self
            .engine
            .poll_into(now, &mut self.workspace, &mut self.actions)
        {
            Ok(()) => self.report(),
            Err(error) => println!("! {error}"),
        }
    }

    fn report(&mut self) {
        for action in self.actions.iter() {
            match action {
                StackAction::Transmit(datagram) => {
                    if let Err(error) = self.adapter.send_datagram(datagram) {
                        println!("! path {}: {error}", datagram.channel());
                    }
                }
                StackAction::ApplicationDataAvailable => {}
                StackAction::ConnectionStateChanged(change) => {
                    let reason = change
                        .disconnect_reason
                        .map(|reason| format!(" ({reason:?})"))
                        .unwrap_or_default();
                    println!("* SR {:?} -> {:?}{reason}", change.previous, change.current);
                    if change.current == ConnectionState::Closed {
                        println!("* type /open to connect again");
                    }
                }
                StackAction::RedundancyStateChanged { previous, current } => {
                    println!("* RL {previous:?} -> {current:?}");
                }
                StackAction::DiscardedSrMessage => println!("* discarded an invalid SR message"),
                StackAction::ConnectionDiagnostics(diagnostics) => println!(
                    "* SR diagnostics: safety code {}, address {}, type {}, sequence {}, \
                     confirmed sequence {}; T_rtd {:?}, T_alive {:?}",
                    diagnostics.safety_code_errors,
                    diagnostics.address_errors,
                    diagnostics.type_errors,
                    diagnostics.sequence_errors,
                    diagnostics.confirmed_sequence_errors,
                    diagnostics.round_trip_delays,
                    diagnostics.alive_times,
                ),
                StackAction::RedundancyDiagnostics {
                    channel,
                    diagnostics,
                } => println!(
                    "* RL diagnostics, path {channel}: frames {}, missed {}, T_drift {} ms, \
                     T_drift2 {} ms²",
                    diagnostics.frames,
                    diagnostics.missed,
                    diagnostics.drift,
                    diagnostics.drift_squared,
                ),
            }
        }
        while let Some(payload) = self.engine.pop_received() {
            let text = String::from_utf8_lossy(payload.as_slice());
            println!("< {}", text.trim_end_matches('\n'));
        }
        let _ = io::stdout().flush();
    }
}

enum Input {
    Line(String),
    End,
}

fn spawn_input_reader() -> Receiver<Input> {
    let (sender, receiver) = mpsc::channel();
    thread::spawn(move || {
        for line in io::stdin().lock().lines() {
            let Ok(line) = line else { break };
            if sender.send(Input::Line(line)).is_err() {
                return;
            }
        }
        let _ = sender.send(Input::End);
    });
    receiver
}

/// A real host supplies the initial sequence number from a proper entropy
/// source.
fn initial_sequence_number() -> u32 {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.subsec_nanos());
    nanos ^ process::id().rotate_left(16)
}

fn main() {
    let options = match parse_options(std::env::args().skip(1)) {
        Ok(options) => options,
        Err(error) => {
            eprintln!("{error}\n\n{USAGE}");
            process::exit(2);
        }
    };
    let mut node = match Node::new(&options) {
        Ok(node) => node,
        Err(error) => {
            eprintln!("{error}");
            process::exit(1);
        }
    };
    let input = spawn_input_reader();
    let clock = ProtocolClock::start();
    println!(
        "* node 0x{:X}, peer 0x{:X}; type a line to send it, or /close, /open, /quit",
        options.local_id, options.remote_id
    );
    let open = |now| {
        (
            now,
            ConnectionEvent::Open {
                initial_sequence_number: initial_sequence_number(),
            },
        )
    };
    let (now, event) = open(clock.now());
    node.step(now, event);
    loop {
        match input.try_recv() {
            Ok(Input::Line(line)) => match line.as_str() {
                "/close" => node.step(clock.now(), ConnectionEvent::Close),
                "/open" => {
                    let (now, event) = open(clock.now());
                    node.step(now, event);
                }
                "/quit" => break,
                _ => node.send(clock.now(), &line),
            },
            Ok(Input::End) | Err(TryRecvError::Disconnected) => break,
            Err(TryRecvError::Empty) => {}
        }
        node.service(clock.now());
        thread::sleep(Duration::from_millis(1));
    }
    if node.engine.connection_snapshot().state != ConnectionState::Closed {
        node.step(clock.now(), ConnectionEvent::Close);
    }
}
