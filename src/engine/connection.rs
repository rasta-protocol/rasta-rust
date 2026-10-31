//! Deterministic SR connection lifecycle engine.
//!
//! The state machine covers connection establishment, closure, heartbeat
//! handling, application data with flow control, retransmission, and adaptive
//! timing. It consumes explicit events and caller-provided monotonic time values, then
//! emits effects for an adapter to execute. It performs no I/O, clock access,
//! random-number generation, allocation, or sleeping.
//!
//! Received messages are assumed to have passed wire decoding and safety-code
//! verification. This engine still checks addressing and the handshake
//! invariants before acting on them.

use core::fmt;

use crate::codec::sr::{
    ConnectionData, DisconnectReason, ProtocolVersion, SrBody, SrHeader, SrMessage,
};

use super::{
    buffers::{ApplicationData, ApplicationPayload, BufferError, ConnectionBuffers},
    diagnostics::{ConnectionDiagnostics, DiagnosticsConfig, DiagnosticsConfigurationError},
    timing::{TimingConfig, TimingConfigurationError, TimingState},
};

const EFFECT_CAPACITY: usize = 4;
const MIN_N_SEND_MAX: u16 = 2;
/// Packetization factor (`N_maxPacket`) of [`ConnectionConfig::new`]: one
/// application message per Data message.
pub const DEFAULT_MAX_PACKET: u16 = 1;
/// Largest packetization factor: messages of one byte each, with their length
/// prefixes, in the largest application-data field.
pub const MAX_MAX_PACKET: u16 = 352;
#[allow(clippy::cast_lossless)]
const _: () = assert!(MAX_MAX_PACKET as usize == crate::codec::sr::MAX_DATA_FIELD_LEN / 3);

/// Timer granularity assumed by [`ConnectionConfig::new`]: one millisecond,
/// the resolution of the protocol's millisecond timestamps.
pub const DEFAULT_TIMER_GRANULARITY: u32 = 1;
const MAX_N_SEND_MAX: u16 = 20;

/// Endpoint role during connection establishment.
///
/// The role is a protocol role for the handshake, not a function in the
/// railway system. It follows from the endpoint identifiers: the endpoint with
/// the lower identifier is the client, the one with the higher identifier is
/// the server (see [`ConnectionConfig::role`]).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Role {
    /// Actively sends the initial connection request.
    Client,
    /// Waits for a connection request and responds to it.
    Server,
}

/// Configured data that remains constant for one connection engine.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ConnectionConfig {
    local_id: u32,
    remote_id: u32,
    protocol_version: ProtocolVersion,
    n_send_max: u16,
    timing: TimingConfig,
    max_packet: u16,
    diagnostics: DiagnosticsConfig,
}

impl ConnectionConfig {
    /// Creates a validated connection configuration.
    ///
    /// `t_max` uses the same wrapping time unit as event timestamps. The
    /// heartbeat period is derived as `t_max / 4`. On an idle connection a
    /// peer's confirmation can be up to one heartbeat period (plus latency)
    /// old when it arrives, and the next one follows a heartbeat period
    /// later, so an idle connection needs `2 * t_heartbeat + latency < t_max`
    /// (see `docs/adr/0004-adaptive-timer.md`). A quarter of `t_max` leaves
    /// a margin for latency and polling, and keeps the confirmations seen by
    /// a peer fresh. Use [`Self::new_with_timing`] to set it explicitly.
    ///
    /// The timer granularity is [`DEFAULT_TIMER_GRANULARITY`] (1 ms). A host
    /// whose clock is coarser sets its granularity with
    /// [`Self::with_timer_granularity`].
    ///
    /// # Errors
    ///
    /// Returns an error when endpoint identifiers are equal, `N_SENDMAX` falls
    /// outside 2 through 20, `t_max` is zero, or the last diagnostic interval
    /// bound is not below `t_max`.
    pub const fn new(
        local_id: u32,
        remote_id: u32,
        protocol_version: ProtocolVersion,
        n_send_max: u16,
        t_max: u32,
        diagnostics: DiagnosticsConfig,
    ) -> Result<Self, ConfigurationError> {
        if local_id == remote_id {
            return Err(ConfigurationError::EqualEndpointIdentifiers(local_id));
        }
        if n_send_max < MIN_N_SEND_MAX || n_send_max > MAX_N_SEND_MAX {
            return Err(ConfigurationError::InvalidNSendMax(n_send_max));
        }
        let heartbeat = if t_max < 4 { 1 } else { t_max / 4 };
        let timing = match TimingConfig::new(
            t_max,
            heartbeat,
            DEFAULT_TIMER_GRANULARITY,
            n_send_max - 1,
            n_send_max,
        ) {
            Ok(timing) => timing,
            Err(error) => return Err(ConfigurationError::Timing(error)),
        };
        if let Err(error) = check_diagnostics(diagnostics, timing) {
            return Err(error);
        }

        Ok(Self {
            local_id,
            remote_id,
            protocol_version,
            n_send_max,
            timing,
            max_packet: DEFAULT_MAX_PACKET,
            diagnostics,
        })
    }

    /// Creates a connection configuration with explicit adaptive timing values.
    ///
    /// # Errors
    ///
    /// Returns an error when endpoint identifiers, `N_SENDMAX`, or timing
    /// configuration are invalid, or the last diagnostic interval bound is not
    /// below `T_max`.
    pub const fn new_with_timing(
        local_id: u32,
        remote_id: u32,
        protocol_version: ProtocolVersion,
        n_send_max: u16,
        timing: TimingConfig,
        diagnostics: DiagnosticsConfig,
    ) -> Result<Self, ConfigurationError> {
        if local_id == remote_id {
            return Err(ConfigurationError::EqualEndpointIdentifiers(local_id));
        }
        if n_send_max < MIN_N_SEND_MAX || n_send_max > MAX_N_SEND_MAX {
            return Err(ConfigurationError::InvalidNSendMax(n_send_max));
        }
        if timing.m_w_a() >= n_send_max {
            return Err(ConfigurationError::Timing(
                TimingConfigurationError::InvalidAcknowledgementThreshold {
                    threshold: timing.m_w_a(),
                    n_send_max,
                },
            ));
        }
        if let Err(error) = check_diagnostics(diagnostics, timing) {
            return Err(error);
        }

        Ok(Self {
            local_id,
            remote_id,
            protocol_version,
            n_send_max,
            timing,
            max_packet: DEFAULT_MAX_PACKET,
            diagnostics,
        })
    }

    /// Returns the endpoint role, derived from the identifiers: the endpoint
    /// with the lower identifier is the client and sends the connection
    /// request; the one with the higher identifier is the server.
    #[must_use]
    pub const fn role(self) -> Role {
        if self.local_id < self.remote_id {
            Role::Client
        } else {
            Role::Server
        }
    }

    /// Returns the local endpoint identifier.
    #[must_use]
    pub const fn local_id(self) -> u32 {
        self.local_id
    }

    /// Returns the remote endpoint identifier.
    #[must_use]
    pub const fn remote_id(self) -> u32 {
        self.remote_id
    }

    /// Returns the configured protocol version.
    #[must_use]
    pub const fn protocol_version(self) -> ProtocolVersion {
        self.protocol_version
    }

    /// Returns the local receive-window limit.
    #[must_use]
    pub const fn n_send_max(self) -> u16 {
        self.n_send_max
    }

    /// Returns the configured maximum time interval.
    #[must_use]
    pub const fn t_max(self) -> u32 {
        self.timing.t_max()
    }

    /// Returns the adaptive timing configuration.
    #[must_use]
    pub const fn timing(self) -> TimingConfig {
        self.timing
    }

    /// Returns the diagnostics configuration.
    #[must_use]
    pub const fn diagnostics(self) -> DiagnosticsConfig {
        self.diagnostics
    }

    /// Returns the packetization factor `N_maxPacket`: the most application
    /// messages one Data message may carry, in both directions.
    #[must_use]
    pub const fn max_packet(self) -> u16 {
        self.max_packet
    }

    /// Sets the packetization factor `N_maxPacket`. It is a project parameter:
    /// both endpoints of a connection must use the same value. The default is
    /// [`DEFAULT_MAX_PACKET`].
    ///
    /// # Errors
    ///
    /// Returns an error unless `max_packet` is in `1..=MAX_MAX_PACKET`.
    pub const fn with_max_packet(self, max_packet: u16) -> Result<Self, ConfigurationError> {
        if max_packet == 0 || max_packet > MAX_MAX_PACKET {
            return Err(ConfigurationError::InvalidMaxPacket(max_packet));
        }
        Ok(Self { max_packet, ..self })
    }

    /// Sets the granularity of the host's clock in milliseconds, keeping all
    /// other timing values.
    ///
    /// The granularity is added to every round-trip delay so that the delay is
    /// never underestimated. It is a property of the platform: a clock that
    /// advances in 10 ms ticks needs 10.
    ///
    /// # Errors
    ///
    /// Returns an error if the granularity exceeds `T_max`.
    pub const fn with_timer_granularity(
        self,
        granularity: u32,
    ) -> Result<Self, ConfigurationError> {
        let timing = self.timing;
        match TimingConfig::new(
            timing.t_max(),
            timing.t_heartbeat(),
            granularity,
            timing.m_w_a(),
            self.n_send_max,
        ) {
            Ok(timing) => Ok(Self { timing, ..self }),
            Err(error) => Err(ConfigurationError::Timing(error)),
        }
    }
}

// Diagnostic intervals partition 0..=T_max (OBS-SBB-DIAG-002).
const fn check_diagnostics(
    diagnostics: DiagnosticsConfig,
    timing: TimingConfig,
) -> Result<(), ConfigurationError> {
    if diagnostics.interval_bounds()[3] >= timing.t_max() {
        return Err(ConfigurationError::Diagnostics(
            DiagnosticsConfigurationError::LastBoundNotBelowMaximum,
        ));
    }
    Ok(())
}

/// Connection configuration errors.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ConfigurationError {
    /// Local and remote identifiers must differ.
    EqualEndpointIdentifiers(u32),
    /// `N_SENDMAX` is outside the inclusive range 2 through 20.
    InvalidNSendMax(u16),
    /// Adaptive timing configuration is invalid.
    Timing(TimingConfigurationError),
    /// The packetization factor is outside `1..=MAX_MAX_PACKET`.
    InvalidMaxPacket(u16),
    /// Diagnostics configuration is invalid.
    Diagnostics(DiagnosticsConfigurationError),
}

impl fmt::Display for ConfigurationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EqualEndpointIdentifiers(value) => {
                write!(formatter, "local and remote identifiers are both {value}")
            }
            Self::InvalidNSendMax(value) => {
                write!(formatter, "N_SENDMAX {value} is outside 2..=20")
            }
            Self::Timing(error) => write!(formatter, "invalid timing configuration: {error}"),
            Self::InvalidMaxPacket(value) => {
                write!(
                    formatter,
                    "N_maxPacket {value} is outside 1..={MAX_MAX_PACKET}"
                )
            }
            Self::Diagnostics(error) => {
                write!(formatter, "invalid diagnostics configuration: {error}")
            }
        }
    }
}

impl core::error::Error for ConfigurationError {}

/// Observable SR connection states.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ConnectionState {
    /// No active redundancy channel or SR connection.
    Closed,
    /// Server-side redundancy channel is open and awaiting a request.
    Down,
    /// Connection handshake is in progress.
    Start,
    /// Connection is established.
    Up,
    /// A retransmission request is outstanding.
    RetransmissionRequest,
    /// Retransmitted messages are being received.
    RetransmissionRunning,
}

/// Replayable state owned by the deterministic engine.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ConnectionSnapshot {
    /// Current connection state.
    pub state: ConnectionState,
    /// Sequence number assigned to the next outbound PDU.
    pub next_send_sequence: u32,
    /// Sequence number of the most recently emitted outbound PDU.
    pub last_transmitted_sequence: Option<u32>,
    /// Sequence number expected from the next inbound in-sequence PDU.
    pub next_receive_sequence: u32,
    /// Last inbound sequence number confirmed by this endpoint (`CS_T`).
    pub confirmed_send_sequence: u32,
    /// Last outbound sequence number confirmed by the peer (`CS_R`).
    pub confirmed_receive_sequence: u32,
    /// Most recently accepted peer timestamp.
    pub received_timestamp: u32,
    /// Most recently accepted peer confirmation of a local timestamp.
    pub confirmed_received_timestamp: u32,
    /// Peer receive-window limit learned during the handshake.
    pub remote_n_send_max: u16,
    /// Adaptive timing state.
    pub timing: TimingState,
    /// Diagnostics collected in the current diagnostic window.
    pub diagnostics: ConnectionDiagnostics,
}

impl ConnectionSnapshot {
    const fn closed(initial_remote_n_send_max: u16, timing: TimingConfig) -> Self {
        Self {
            state: ConnectionState::Closed,
            next_send_sequence: 0,
            last_transmitted_sequence: None,
            next_receive_sequence: 0,
            confirmed_send_sequence: 0,
            confirmed_receive_sequence: 0,
            received_timestamp: 0,
            confirmed_received_timestamp: 0,
            remote_n_send_max: initial_remote_n_send_max,
            timing: TimingState::new(timing),
            diagnostics: ConnectionDiagnostics::EMPTY,
        }
    }
}

/// A decoded connection request or response delivered to the engine.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ReceivedConnectionMessage {
    /// Decoded common header.
    pub header: SrHeader,
    /// Decoded connection parameters.
    pub data: ConnectionData,
}

/// A decoded heartbeat delivered to the engine.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ReceivedHeartbeat {
    /// Decoded common header.
    pub header: SrHeader,
}

/// A decoded application-data message delivered to the engine.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ReceivedData {
    /// Decoded common header.
    pub header: SrHeader,
    /// The received application-data field, copied without checking its
    /// structure; the engine checks it before accepting the message.
    pub data: ApplicationData,
}

/// External events consumed by the connection engine.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[allow(clippy::large_enum_variant)]
pub enum ConnectionEvent {
    /// Request connection opening with caller-supplied entropy.
    Open {
        /// Initial outbound sequence number supplied by an entropy adapter.
        initial_sequence_number: u32,
    },
    /// Request a normal user closure.
    Close,
    /// Request transmission of one application message.
    SendData(ApplicationPayload),
    /// Request transmission of several application messages in one Data
    /// message (packetization), at most `ConnectionConfig::max_packet`.
    SendPacked(ApplicationData),
    /// Deliver a verified and decoded connection request.
    ConnectionRequestReceived(ReceivedConnectionMessage),
    /// Deliver a verified and decoded connection response.
    ConnectionResponseReceived(ReceivedConnectionMessage),
    /// Deliver a verified and decoded heartbeat.
    HeartbeatReceived(ReceivedHeartbeat),
    /// Deliver a verified and decoded application-data message.
    DataReceived(ReceivedData),
    /// Deliver a verified and decoded retransmission request.
    RetransmissionRequestReceived(SrHeader),
    /// Deliver a verified and decoded retransmission response.
    RetransmissionResponseReceived(SrHeader),
    /// Deliver a verified and decoded retransmitted application-data message.
    RetransmittedDataReceived(ReceivedData),
    /// Deliver a verified and decoded disconnection request. Its reason and
    /// detailed reason are not reported (OBS-SBB-SM-014).
    DisconnectRequestReceived(SrHeader),
    /// Deliver another message type not permitted during establishment.
    OtherMessageReceived(SrHeader),
    /// Request periodic heartbeat transmission.
    SendHeartbeat,
    /// Evaluate adaptive timeout and heartbeat deadlines at the supplied time.
    PollTimers,
    /// Report a connection timeout.
    Timeout,
}

/// One event together with its caller-supplied monotonic time.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct StepInput {
    /// Current wrapping monotonic time in the configured protocol unit.
    pub now: u32,
    /// Event to process at `now`.
    pub event: ConnectionEvent,
}

/// An outbound semantic PDU emitted by the engine.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[allow(clippy::large_enum_variant)]
pub enum OutboundMessage {
    /// Connection request.
    ConnectionRequest {
        /// Common message header.
        header: SrHeader,
        /// Local connection parameters.
        data: ConnectionData,
    },
    /// Connection response.
    ConnectionResponse {
        /// Common message header.
        header: SrHeader,
        /// Local connection parameters.
        data: ConnectionData,
    },
    /// Heartbeat.
    Heartbeat {
        /// Common message header.
        header: SrHeader,
    },
    /// Application data.
    Data {
        /// Common message header.
        header: SrHeader,
        /// Application-data field with one or more application messages.
        data: ApplicationData,
    },
    /// Retransmission request.
    RetransmissionRequest {
        /// Common message header.
        header: SrHeader,
    },
    /// Retransmission response.
    RetransmissionResponse {
        /// Common message header.
        header: SrHeader,
    },
    /// Retransmitted application data.
    RetransmittedData {
        /// Common message header.
        header: SrHeader,
        /// Application-data field with one or more application messages.
        data: ApplicationData,
    },
    /// Disconnection request.
    DisconnectionRequest {
        /// Common message header.
        header: SrHeader,
        /// Implementation-specific detail, currently zero.
        detailed_reason: u16,
        /// High-level reason.
        reason: DisconnectReason,
    },
}

impl OutboundMessage {
    /// Converts the semantic PDU to an encode-ready message without a safety code.
    ///
    /// [`SrMessage::encode_with_md4`] can calculate and append the configured
    /// safety code without changing the state-machine API.
    #[must_use]
    pub fn as_sr_message(&self) -> SrMessage<'_> {
        match self {
            Self::ConnectionRequest { header, data } => SrMessage {
                header: *header,
                body: SrBody::ConnectionRequest(*data),
                safety_code: &[],
            },
            Self::ConnectionResponse { header, data } => SrMessage {
                header: *header,
                body: SrBody::ConnectionResponse(*data),
                safety_code: &[],
            },
            Self::Heartbeat { header } => SrMessage {
                header: *header,
                body: SrBody::Heartbeat,
                safety_code: &[],
            },
            Self::Data { header, data } => SrMessage {
                header: *header,
                body: SrBody::Data(data.as_field()),
                safety_code: &[],
            },
            Self::RetransmissionRequest { header } => SrMessage {
                header: *header,
                body: SrBody::RetransmissionRequest,
                safety_code: &[],
            },
            Self::RetransmissionResponse { header } => SrMessage {
                header: *header,
                body: SrBody::RetransmissionResponse,
                safety_code: &[],
            },
            Self::RetransmittedData { header, data } => SrMessage {
                header: *header,
                body: SrBody::RetransmittedData(data.as_field()),
                safety_code: &[],
            },
            Self::DisconnectionRequest {
                header,
                detailed_reason,
                reason,
            } => SrMessage {
                header: *header,
                body: SrBody::DisconnectionRequest {
                    detailed_reason: *detailed_reason,
                    reason: *reason,
                },
                safety_code: &[],
            },
        }
    }

    /// Returns the common message header.
    #[must_use]
    pub const fn header(&self) -> SrHeader {
        match *self {
            Self::ConnectionRequest { header, .. }
            | Self::ConnectionResponse { header, .. }
            | Self::Heartbeat { header }
            | Self::Data { header, .. }
            | Self::RetransmissionRequest { header }
            | Self::RetransmissionResponse { header }
            | Self::RetransmittedData { header, .. }
            | Self::DisconnectionRequest { header, .. } => header,
        }
    }

    pub(super) fn prepare_for_transmit(mut self, now: u32, confirmed_sequence_number: u32) -> Self {
        let connection_request = matches!(self, Self::ConnectionRequest { .. });
        let header = match &mut self {
            Self::ConnectionRequest { header, .. }
            | Self::ConnectionResponse { header, .. }
            | Self::Heartbeat { header }
            | Self::Data { header, .. }
            | Self::RetransmissionRequest { header }
            | Self::RetransmissionResponse { header }
            | Self::RetransmittedData { header, .. }
            | Self::DisconnectionRequest { header, .. } => header,
        };
        header.timestamp = now;
        header.confirmed_sequence_number = if connection_request {
            0
        } else {
            confirmed_sequence_number
        };
        self
    }
}

/// State-change notification emitted for application adapters.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct StateChange {
    /// State before the transition.
    pub previous: ConnectionState,
    /// State after the transition.
    pub current: ConnectionState,
    /// Disconnection reason when this endpoint closes the connection. `None`
    /// for other transitions, including a closure by the peer's disconnection
    /// request, whose reason is not reported (OBS-SBB-SM-014).
    pub disconnect_reason: Option<DisconnectReason>,
}

/// Side effects produced by one deterministic transition.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[allow(clippy::large_enum_variant)]
pub enum Effect {
    /// Ask the redundancy adapter to open its channel.
    OpenRedundancyChannel,
    /// Ask the redundancy adapter to close its channel.
    CloseRedundancyChannel,
    /// Encode and transmit a semantic SR PDU.
    Transmit(OutboundMessage),
    /// Discard the received PDU that triggered the current transition.
    DiscardReceivedMessage,
    /// Notify an application adapter that one payload entered the receive buffer.
    ApplicationDataAvailable,
    /// Notify an application adapter about a state change.
    StateChanged(StateChange),
    /// Report the diagnostics of a completed diagnostic window, or of the
    /// window cut short by closing the redundancy channel (OBS-SBB-DIAG-002).
    Diagnostics(ConnectionDiagnostics),
}

/// Fixed-capacity, allocation-free effects from one transition.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct EffectBatch {
    items: [Option<Effect>; EFFECT_CAPACITY],
    length: u8,
}

#[allow(clippy::large_types_passed_by_value)]
impl EffectBatch {
    pub(crate) const fn new() -> Self {
        Self {
            items: [None; EFFECT_CAPACITY],
            length: 0,
        }
    }

    /// Returns the number of effects in execution order.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.length as usize
    }

    /// Returns `true` when the transition emitted no effects.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.length == 0
    }

    /// Returns one effect by execution-order index.
    #[must_use]
    pub fn get(&self, index: usize) -> Option<&Effect> {
        if index < self.len() {
            self.items[index].as_ref()
        } else {
            None
        }
    }

    /// Iterates over effects in execution order.
    pub fn iter(&self) -> impl Iterator<Item = &Effect> {
        self.items[..self.len()].iter().filter_map(Option::as_ref)
    }

    fn push(&mut self, effect: Effect) -> Result<(), EngineError> {
        let index = self.len();
        if index >= EFFECT_CAPACITY {
            return Err(EngineError::EffectCapacityExceeded);
        }
        self.items[index] = Some(effect);
        self.length += 1;
        Ok(())
    }

    fn buffered_transmit(&self) -> Option<(usize, OutboundMessage)> {
        self.iter()
            .enumerate()
            .find_map(|(index, effect)| match effect {
                Effect::Transmit(OutboundMessage::DisconnectionRequest { .. }) => None,
                Effect::Transmit(message) => Some((index, *message)),
                _ => None,
            })
    }

    fn application_data_available(&self) -> bool {
        self.iter()
            .any(|effect| matches!(effect, Effect::ApplicationDataAvailable))
    }

    fn replace(&mut self, index: usize, effect: Effect) {
        debug_assert!(index < self.len());
        self.items[index] = Some(effect);
    }

    fn remove(&mut self, index: usize) {
        debug_assert!(index < self.len());
        let final_index = self.len() - 1;
        for current in index..final_index {
            self.items[current] = self.items[current + 1];
        }
        self.items[final_index] = None;
        self.length -= 1;
    }
}

/// Complete result of one state-machine step.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Transition {
    /// Snapshot before processing the input.
    pub previous: ConnectionSnapshot,
    /// Snapshot after processing the input.
    pub current: ConnectionSnapshot,
    /// Ordered effects for adapters to execute.
    pub effects: EffectBatch,
}

/// State-machine processing errors.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EngineError {
    /// An internal fixed effect bound was insufficient.
    EffectCapacityExceeded,
    /// A fixed-capacity connection buffer could not accept a message.
    Buffer(BufferError),
    /// An application-data field to send is malformed or holds more messages
    /// than `ConnectionConfig::max_packet` allows.
    InvalidApplicationData,
    /// The state/event combination is not handled.
    TransitionNotImplemented {
        /// Current connection state.
        state: ConnectionState,
        /// Event category that was not handled.
        event: EventKind,
    },
}

impl fmt::Display for EngineError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EffectCapacityExceeded => {
                formatter.write_str("connection transition exceeded its effect bound")
            }
            Self::Buffer(error) => write!(formatter, "connection buffer error: {error}"),
            Self::InvalidApplicationData => formatter
                .write_str("application data is malformed or exceeds the packetization factor"),
            Self::TransitionNotImplemented { state, event } => {
                write!(formatter, "{event:?} is not implemented in state {state:?}")
            }
        }
    }
}

impl core::error::Error for EngineError {}

/// Stable event categories used in diagnostics.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EventKind {
    /// Open request.
    Open,
    /// Close request.
    Close,
    /// Application-data send request.
    SendData,
    /// Connection request.
    ConnectionRequest,
    /// Connection response.
    ConnectionResponse,
    /// Heartbeat.
    Heartbeat,
    /// Application-data message.
    DataReceived,
    /// Retransmission request.
    RetransmissionRequest,
    /// Retransmission response.
    RetransmissionResponse,
    /// Retransmitted application-data message.
    RetransmittedData,
    /// Disconnection request.
    DisconnectRequest,
    /// Another received message.
    OtherMessage,
    /// Heartbeat send request.
    SendHeartbeat,
    /// Timer poll.
    PollTimers,
    /// Timeout.
    Timeout,
}

impl ConnectionEvent {
    const fn kind(self) -> EventKind {
        match self {
            Self::Open { .. } => EventKind::Open,
            Self::Close => EventKind::Close,
            Self::SendData(_) | Self::SendPacked(_) => EventKind::SendData,
            Self::ConnectionRequestReceived(_) => EventKind::ConnectionRequest,
            Self::ConnectionResponseReceived(_) => EventKind::ConnectionResponse,
            Self::HeartbeatReceived(_) => EventKind::Heartbeat,
            Self::DataReceived(_) => EventKind::DataReceived,
            Self::RetransmissionRequestReceived(_) => EventKind::RetransmissionRequest,
            Self::RetransmissionResponseReceived(_) => EventKind::RetransmissionResponse,
            Self::RetransmittedDataReceived(_) => EventKind::RetransmittedData,
            Self::DisconnectRequestReceived(_) => EventKind::DisconnectRequest,
            Self::OtherMessageReceived(_) => EventKind::OtherMessage,
            Self::SendHeartbeat => EventKind::SendHeartbeat,
            Self::PollTimers => EventKind::PollTimers,
            Self::Timeout => EventKind::Timeout,
        }
    }
}

/// Deterministic state and configuration for one SR connection.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ConnectionEngine {
    config: ConnectionConfig,
    snapshot: ConnectionSnapshot,
}

#[allow(clippy::large_types_passed_by_value)]
impl ConnectionEngine {
    /// Creates an engine in the closed state.
    #[must_use]
    pub const fn new(config: ConnectionConfig) -> Self {
        Self {
            config,
            snapshot: ConnectionSnapshot::closed(config.n_send_max, config.timing),
        }
    }

    /// Returns the immutable connection configuration.
    #[must_use]
    pub const fn config(&self) -> ConnectionConfig {
        self.config
    }

    /// Returns a replayable snapshot of all mutable engine state.
    #[must_use]
    pub const fn snapshot(&self) -> ConnectionSnapshot {
        self.snapshot
    }

    /// Processes one input atomically and returns ordered external effects.
    ///
    /// The engine and caller-owned buffers are updated only when the complete
    /// transition succeeds.
    ///
    /// # Errors
    ///
    /// Returns an error for an internal effect-bound violation or a transition
    /// intentionally deferred to a later state-machine slice.
    pub fn step(
        &mut self,
        buffers: &mut ConnectionBuffers,
        input: StepInput,
    ) -> Result<Transition, EngineError> {
        let mut candidate = *self;
        let mut candidate_buffers = buffers.clone();
        let previous = self.snapshot;
        let mut effects = EffectBatch::new();
        candidate.step_in_place(&mut candidate_buffers, input, &mut effects)?;
        let transition = Transition {
            previous,
            current: candidate.snapshot,
            effects,
        };
        *self = candidate;
        *buffers = candidate_buffers;
        Ok(transition)
    }

    // Only used on a caller-owned transaction candidate. Errors may leave the
    // candidate buffers changed; the caller must discard the transaction.
    pub(crate) fn step_in_place(
        &mut self,
        buffers: &mut ConnectionBuffers,
        input: StepInput,
        effects: &mut EffectBatch,
    ) -> Result<(), EngineError> {
        effects.length = 0;
        if buffers.local_n_send_max() != self.config.n_send_max {
            return Err(EngineError::Buffer(BufferError::ConfigurationMismatch {
                expected: self.config.n_send_max,
                actual: buffers.local_n_send_max(),
            }));
        }
        let previous = self.snapshot;
        let mut candidate = *self;
        candidate.record_receive_checks(input.event);
        let unavailable_retransmission = match input.event {
            ConnectionEvent::RetransmissionRequestReceived(header)
                if matches!(
                    previous.state,
                    ConnectionState::Up
                        | ConnectionState::RetransmissionRequest
                        | ConnectionState::RetransmissionRunning
                ) && candidate.valid_common_header(header) =>
            {
                !buffers.retransmission_available(header.confirmed_sequence_number.wrapping_add(1))
            }
            _ => false,
        };
        if unavailable_retransmission {
            let ConnectionEvent::RetransmissionRequestReceived(header) = input.event else {
                return Err(EngineError::TransitionNotImplemented {
                    state: previous.state,
                    event: input.event.kind(),
                });
            };
            candidate.close_connection(
                input.now,
                DisconnectReason::RetransmissionFailed,
                Some(header),
                effects,
            )?;
        } else {
            candidate.process(input, effects)?;
        }
        Self::finalize_buffers(previous, &mut candidate, input, buffers, effects)?;
        if candidate.snapshot.state != ConnectionState::Closed {
            for message in effects.iter().filter_map(|effect| match effect {
                Effect::Transmit(message) => Some(*message),
                _ => None,
            }) {
                let header = message.header();
                candidate.snapshot.last_transmitted_sequence = Some(header.sequence_number);
                candidate
                    .snapshot
                    .timing
                    .note_transmitted(header.timestamp, header.confirmed_sequence_number);
            }
        }
        if candidate.snapshot.diagnostics.messages() >= candidate.config.diagnostics.window() {
            candidate.report_diagnostics(effects)?;
        }
        *self = candidate;
        Ok(())
    }

    /// Counts a received PDU whose safety code did not match.
    pub(crate) fn record_safety_code_error(&mut self) {
        if self.snapshot.state != ConnectionState::Closed {
            let counter = &mut self.snapshot.diagnostics.safety_code_errors;
            *counter = counter.wrapping_add(1);
        }
    }

    /// Counts a received PDU with an undefined message type.
    pub(crate) fn record_type_error(&mut self) {
        if self.snapshot.state != ConnectionState::Closed {
            let counter = &mut self.snapshot.diagnostics.type_errors;
            *counter = counter.wrapping_add(1);
        }
    }

    // Counts receive errors in the order the reference checks a received PDU:
    // address, sequence range, confirmed sequence number, timestamp order,
    // sequence order (OBS-SBB-DIAG-001). A failed range or timestamp check
    // ends the checks without a count. Counting does not change what the
    // state handlers accept.
    fn record_receive_checks(&mut self, event: ConnectionEvent) {
        // (header, range checked, timestamp checked, order checked)
        let (header, range, timestamp, order) = match event {
            ConnectionEvent::ConnectionRequestReceived(message)
            | ConnectionEvent::ConnectionResponseReceived(message) => {
                (message.header, false, false, false)
            }
            ConnectionEvent::RetransmissionResponseReceived(header) => {
                (header, false, false, false)
            }
            ConnectionEvent::DisconnectRequestReceived(header) => (header, true, false, false),
            ConnectionEvent::RetransmissionRequestReceived(header) => (header, true, false, true),
            ConnectionEvent::HeartbeatReceived(message) => (message.header, true, true, true),
            ConnectionEvent::DataReceived(message)
            | ConnectionEvent::RetransmittedDataReceived(message) => {
                (message.header, true, true, true)
            }
            _ => return,
        };
        if self.snapshot.state == ConnectionState::Closed {
            return;
        }
        let confirmed_plausible = match event {
            ConnectionEvent::ConnectionRequestReceived(_) => header.confirmed_sequence_number == 0,
            ConnectionEvent::ConnectionResponseReceived(_) => {
                header.confirmed_sequence_number == self.snapshot.next_send_sequence.wrapping_sub(1)
            }
            _ => self.confirmed_sequence_plausible(header),
        };
        let diagnostics = self.snapshot.diagnostics;
        let counted = if !self.valid_common_header(header) {
            ConnectionDiagnostics {
                address_errors: diagnostics.address_errors.wrapping_add(1),
                ..diagnostics
            }
        } else if range && !self.sequence_in_range(header) {
            return;
        } else if !confirmed_plausible {
            ConnectionDiagnostics {
                confirmed_sequence_errors: diagnostics.confirmed_sequence_errors.wrapping_add(1),
                ..diagnostics
            }
        } else if timestamp && !self.timestamp_in_order(header) {
            return;
        } else if order && header.sequence_number != self.snapshot.next_receive_sequence {
            ConnectionDiagnostics {
                sequence_errors: diagnostics.sequence_errors.wrapping_add(1),
                ..diagnostics
            }
        } else {
            return;
        };
        self.snapshot.diagnostics = counted;
    }

    fn report_diagnostics(&mut self, effects: &mut EffectBatch) -> Result<(), EngineError> {
        effects.push(Effect::Diagnostics(self.snapshot.diagnostics))?;
        self.snapshot.diagnostics = ConnectionDiagnostics::EMPTY;
        Ok(())
    }

    fn finalize_buffers(
        previous: ConnectionSnapshot,
        candidate: &mut Self,
        input: StepInput,
        buffers: &mut ConnectionBuffers,
        effects: &mut EffectBatch,
    ) -> Result<(), EngineError> {
        let current = candidate.snapshot;
        if current.state == ConnectionState::Closed {
            buffers.reset();
            return Ok(());
        }

        let opening = matches!(input.event, ConnectionEvent::Open { .. });
        let confirmation = (current.confirmed_receive_sequence
            != previous.confirmed_receive_sequence)
            .then_some(current.confirmed_receive_sequence);
        let buffered_transmit = effects.buffered_transmit();
        let received_payload = match input.event {
            ConnectionEvent::DataReceived(message) if effects.application_data_available() => {
                Some(message.data)
            }
            ConnectionEvent::RetransmittedDataReceived(message)
                if effects.application_data_available() =>
            {
                Some(message.data)
            }
            _ => None,
        };
        debug_assert_eq!(
            effects.application_data_available(),
            received_payload.is_some()
        );

        if let ConnectionEvent::RetransmissionRequestReceived(header) = input.event {
            if let Some((effect_index, response @ OutboundMessage::RetransmissionResponse { .. })) =
                buffered_transmit
            {
                if let Some(sequence) = confirmation {
                    buffers.acknowledge_through(sequence);
                }
                let requested_sequence = header.confirmed_sequence_number.wrapping_add(1);
                let mut next_sequence = candidate.snapshot.next_send_sequence;
                buffers
                    .rebuild_for_retransmission(requested_sequence, response, &mut next_sequence)
                    .map_err(EngineError::Buffer)?;
                if header.sequence_number != previous.next_receive_sequence {
                    candidate.snapshot.next_send_sequence = next_sequence;
                    let request = candidate.retransmission_request(input.now);
                    buffers.enqueue_send(request).map_err(EngineError::Buffer)?;
                    next_sequence = candidate.snapshot.next_send_sequence;
                }
                candidate.snapshot.next_send_sequence = next_sequence;
                if let Some(next) = buffers.next_to_transmit(
                    candidate.snapshot.remote_n_send_max,
                    input.now,
                    candidate.snapshot.confirmed_send_sequence,
                ) {
                    effects.replace(effect_index, Effect::Transmit(next));
                } else {
                    effects.remove(effect_index);
                }
                return Ok(());
            }
        }

        if !opening && buffered_transmit.is_some() && !buffers.can_enqueue_after_ack(confirmation) {
            return Err(EngineError::Buffer(BufferError::SendBufferFull));
        }
        if received_payload.is_some() && !buffers.can_push_received() {
            return Err(EngineError::Buffer(BufferError::ReceiveBufferFull));
        }

        if let Some(payload) = received_payload {
            buffers
                .push_received(payload)
                .map_err(EngineError::Buffer)?;
        }

        if opening {
            buffers.reset();
        } else if let Some(sequence) = confirmation {
            buffers.acknowledge_through(sequence);
        }

        if let Some((effect_index, generated)) = buffered_transmit {
            buffers
                .enqueue_send(generated)
                .map_err(EngineError::Buffer)?;
            if let Some(next) = buffers.next_to_transmit(
                current.remote_n_send_max,
                input.now,
                current.confirmed_send_sequence,
            ) {
                effects.replace(effect_index, Effect::Transmit(next));
            } else {
                effects.remove(effect_index);
            }
        }
        Ok(())
    }

    fn process(&mut self, input: StepInput, effects: &mut EffectBatch) -> Result<(), EngineError> {
        match self.snapshot.state {
            ConnectionState::Closed => self.process_closed(input, effects),
            ConnectionState::Down => self.process_down(input, effects),
            ConnectionState::Start => self.process_start(input, effects),
            ConnectionState::Up => self.process_up(input, effects),
            ConnectionState::RetransmissionRequest => {
                self.process_retransmission_request(input, effects)
            }
            ConnectionState::RetransmissionRunning => {
                self.process_retransmission_running(input, effects)
            }
        }
    }

    fn process_closed(
        &mut self,
        input: StepInput,
        effects: &mut EffectBatch,
    ) -> Result<(), EngineError> {
        if let ConnectionEvent::Open {
            initial_sequence_number,
        } = input.event
        {
            self.snapshot = ConnectionSnapshot {
                state: ConnectionState::Closed,
                next_send_sequence: initial_sequence_number,
                last_transmitted_sequence: None,
                next_receive_sequence: 0,
                confirmed_send_sequence: 0,
                confirmed_receive_sequence: 0,
                received_timestamp: 0,
                confirmed_received_timestamp: if self.config.role() == Role::Client {
                    input.now
                } else {
                    0
                },
                remote_n_send_max: self.config.n_send_max,
                timing: TimingState::new(self.config.timing),
                diagnostics: ConnectionDiagnostics::EMPTY,
            };
            if self.config.role() == Role::Client {
                self.snapshot
                    .timing
                    .start_establishment(self.config.timing, input.now);
            }
            effects.push(Effect::OpenRedundancyChannel)?;
            if self.config.role() == Role::Client {
                let message = self.connection_request(input.now);
                effects.push(Effect::Transmit(message))?;
                self.change_state(ConnectionState::Start, None, effects)?;
            } else {
                self.change_state(ConnectionState::Down, None, effects)?;
            }
        }
        Ok(())
    }

    fn process_down(
        &mut self,
        input: StepInput,
        effects: &mut EffectBatch,
    ) -> Result<(), EngineError> {
        match input.event {
            ConnectionEvent::Open { .. }
            | ConnectionEvent::Close
            | ConnectionEvent::SendData(_)
            | ConnectionEvent::SendPacked(_) => self.close_redundancy(None, effects)?,
            ConnectionEvent::ConnectionRequestReceived(message) => {
                if !self.valid_connection_request(message) {
                    return Ok(());
                }
                if !self.protocol_version_accepted(message.data.version) {
                    self.close_connection(
                        input.now,
                        DisconnectReason::ProtocolVersionError,
                        Some(message.header),
                        effects,
                    )?;
                    return Ok(());
                }
                self.accept_connection_request(message, input.now);
                let response = self.connection_response(input.now);
                effects.push(Effect::Transmit(response))?;
                self.change_state(ConnectionState::Start, None, effects)?;
            }
            _ => {}
        }
        Ok(())
    }

    fn process_start(
        &mut self,
        input: StepInput,
        effects: &mut EffectBatch,
    ) -> Result<(), EngineError> {
        match input.event {
            ConnectionEvent::Open { .. }
            | ConnectionEvent::SendData(_)
            | ConnectionEvent::SendPacked(_) => self.close_connection(
                input.now,
                DisconnectReason::ServiceNotAllowed,
                None,
                effects,
            )?,
            ConnectionEvent::Close => {
                self.close_connection(input.now, DisconnectReason::UserRequest, None, effects)?;
            }
            ConnectionEvent::ConnectionRequestReceived(message) => {
                if self.valid_connection_request(message) {
                    self.close_connection(
                        input.now,
                        DisconnectReason::UnexpectedMessage,
                        Some(message.header),
                        effects,
                    )?;
                }
            }
            ConnectionEvent::ConnectionResponseReceived(message) => {
                self.process_start_connection_response(input.now, message, effects)?;
            }
            ConnectionEvent::HeartbeatReceived(message) => {
                self.process_start_heartbeat(input.now, message, effects)?;
            }
            ConnectionEvent::DataReceived(message) => {
                if self.valid_common_header(message.header) {
                    self.close_connection(
                        input.now,
                        DisconnectReason::UnexpectedMessage,
                        Some(message.header),
                        effects,
                    )?;
                }
            }
            ConnectionEvent::DisconnectRequestReceived(header) => {
                if self.valid_common_header(header) {
                    self.close_redundancy(Some(header), effects)?;
                }
            }
            ConnectionEvent::RetransmissionRequestReceived(_)
            | ConnectionEvent::RetransmissionResponseReceived(_)
            | ConnectionEvent::RetransmittedDataReceived(_) => {}
            ConnectionEvent::OtherMessageReceived(header) => {
                if self.valid_common_header(header) {
                    self.close_connection(
                        input.now,
                        DisconnectReason::UnexpectedMessage,
                        Some(header),
                        effects,
                    )?;
                }
            }
            ConnectionEvent::SendHeartbeat => {
                if self.config.role() == Role::Server {
                    let heartbeat = self.heartbeat(input.now);
                    effects.push(Effect::Transmit(heartbeat))?;
                }
            }
            ConnectionEvent::PollTimers => {
                self.process_timer_poll(input.now, self.config.role() == Role::Server, effects)?;
            }
            ConnectionEvent::Timeout => {
                self.close_connection(input.now, DisconnectReason::Timeout, None, effects)?;
            }
        }
        Ok(())
    }

    fn process_start_connection_response(
        &mut self,
        now: u32,
        message: ReceivedConnectionMessage,
        effects: &mut EffectBatch,
    ) -> Result<(), EngineError> {
        if !self.valid_connection_response(message) {
            return Ok(());
        }
        if self.config.role() == Role::Server {
            return self.close_connection(
                now,
                DisconnectReason::UnexpectedMessage,
                Some(message.header),
                effects,
            );
        }
        if !self.protocol_version_accepted(message.data.version) {
            return self.close_connection(
                now,
                DisconnectReason::ProtocolVersionError,
                Some(message.header),
                effects,
            );
        }

        self.accept_connection_response(message);
        let heartbeat = self.heartbeat(now);
        effects.push(Effect::Transmit(heartbeat))?;
        self.change_state(ConnectionState::Up, None, effects)
    }

    fn process_start_heartbeat(
        &mut self,
        now: u32,
        message: ReceivedHeartbeat,
        effects: &mut EffectBatch,
    ) -> Result<(), EngineError> {
        if !self.valid_sequenced_header(message.header) {
            return Ok(());
        }
        let reason = if self.config.role() == Role::Client {
            Some(DisconnectReason::UnexpectedMessage)
        } else if message.header.sequence_number != self.snapshot.next_receive_sequence {
            Some(DisconnectReason::SequenceNumberError)
        } else if !wrapping_value_in_window(
            message.header.confirmed_timestamp,
            self.snapshot.confirmed_received_timestamp,
            self.config.timing.t_max() - 1,
        ) {
            Some(DisconnectReason::ProtocolSequenceError)
        } else if !self.evaluate_timeliness(now, message.header) {
            Some(DisconnectReason::Timeout)
        } else {
            None
        };

        if let Some(reason) = reason {
            self.close_connection(now, reason, Some(message.header), effects)
        } else {
            self.accept_sequenced_message(message.header);
            self.emit_acknowledgement_if_due(now, effects)?;
            self.change_state(ConnectionState::Up, None, effects)
        }
    }

    fn process_up(
        &mut self,
        input: StepInput,
        effects: &mut EffectBatch,
    ) -> Result<(), EngineError> {
        match input.event {
            ConnectionEvent::Open { .. } => self.close_connection(
                input.now,
                DisconnectReason::ServiceNotAllowed,
                None,
                effects,
            ),
            ConnectionEvent::Close => {
                self.close_connection(input.now, DisconnectReason::UserRequest, None, effects)
            }
            ConnectionEvent::SendHeartbeat => {
                let heartbeat = self.heartbeat(input.now);
                effects.push(Effect::Transmit(heartbeat))
            }
            ConnectionEvent::PollTimers => self.process_timer_poll(input.now, true, effects),
            ConnectionEvent::HeartbeatReceived(message) => {
                self.process_up_heartbeat(input.now, message, effects)
            }
            ConnectionEvent::DataReceived(message) => {
                self.process_up_data(input.now, message, effects)
            }
            ConnectionEvent::RetransmissionRequestReceived(header) => {
                self.process_up_retransmission_request(input.now, header, effects)
            }
            ConnectionEvent::RetransmissionResponseReceived(header)
            | ConnectionEvent::OtherMessageReceived(header) => {
                if self.valid_common_header(header) {
                    self.close_connection(
                        input.now,
                        DisconnectReason::UnexpectedMessage,
                        Some(header),
                        effects,
                    )
                } else {
                    Ok(())
                }
            }
            ConnectionEvent::RetransmittedDataReceived(message) => {
                if self.valid_common_header(message.header) {
                    self.close_connection(
                        input.now,
                        DisconnectReason::UnexpectedMessage,
                        Some(message.header),
                        effects,
                    )
                } else {
                    Ok(())
                }
            }
            ConnectionEvent::DisconnectRequestReceived(header) => {
                if self.valid_common_header(header) {
                    self.close_redundancy(Some(header), effects)
                } else {
                    Ok(())
                }
            }
            ConnectionEvent::Timeout => {
                self.close_connection(input.now, DisconnectReason::Timeout, None, effects)
            }
            ConnectionEvent::ConnectionRequestReceived(message) => {
                if self.valid_connection_request(message) {
                    self.close_connection(
                        input.now,
                        DisconnectReason::UnexpectedMessage,
                        Some(message.header),
                        effects,
                    )
                } else {
                    Ok(())
                }
            }
            ConnectionEvent::ConnectionResponseReceived(message) => {
                if self.valid_connection_response(message) {
                    self.close_connection(
                        input.now,
                        DisconnectReason::UnexpectedMessage,
                        Some(message.header),
                        effects,
                    )
                } else {
                    Ok(())
                }
            }
            ConnectionEvent::SendData(payload) => {
                self.send_data(input.now, ApplicationData::single(&payload), effects)
            }
            ConnectionEvent::SendPacked(data) => self.send_data(input.now, data, effects),
        }
    }

    fn process_up_heartbeat(
        &mut self,
        now: u32,
        message: ReceivedHeartbeat,
        effects: &mut EffectBatch,
    ) -> Result<(), EngineError> {
        if !self.valid_sequenced_header(message.header) {
            return Ok(());
        }
        if message.header.sequence_number != self.snapshot.next_receive_sequence {
            effects.push(Effect::DiscardReceivedMessage)?;
            let request = self.retransmission_request(now);
            effects.push(Effect::Transmit(request))?;
            return self.change_state(ConnectionState::RetransmissionRequest, None, effects);
        }
        if !wrapping_value_in_window(
            message.header.confirmed_timestamp,
            self.snapshot.confirmed_received_timestamp,
            self.config.timing.t_max() - 1,
        ) {
            return self.close_connection(
                now,
                DisconnectReason::ProtocolSequenceError,
                Some(message.header),
                effects,
            );
        }
        if !self.evaluate_timeliness(now, message.header) {
            return self.close_connection(
                now,
                DisconnectReason::Timeout,
                Some(message.header),
                effects,
            );
        }

        self.accept_sequenced_message(message.header);
        self.emit_acknowledgement_if_due(now, effects)
    }

    fn process_up_data(
        &mut self,
        now: u32,
        message: ReceivedData,
        effects: &mut EffectBatch,
    ) -> Result<(), EngineError> {
        if !self.valid_sequenced_header(message.header) {
            return Ok(());
        }
        if message.header.sequence_number != self.snapshot.next_receive_sequence {
            effects.push(Effect::DiscardReceivedMessage)?;
            let request = self.retransmission_request(now);
            effects.push(Effect::Transmit(request))?;
            return self.change_state(ConnectionState::RetransmissionRequest, None, effects);
        }
        if !wrapping_value_in_window(
            message.header.confirmed_timestamp,
            self.snapshot.confirmed_received_timestamp,
            self.config.timing.t_max() - 1,
        ) {
            return self.close_connection(
                now,
                DisconnectReason::ProtocolSequenceError,
                Some(message.header),
                effects,
            );
        }
        if !self.evaluate_timeliness(now, message.header) {
            return self.close_connection(
                now,
                DisconnectReason::Timeout,
                Some(message.header),
                effects,
            );
        }
        if !self.data_acceptable(&message.data) {
            return self.close_connection(
                now,
                DisconnectReason::UnexpectedMessage,
                Some(message.header),
                effects,
            );
        }

        self.accept_sequenced_message(message.header);
        effects.push(Effect::ApplicationDataAvailable)?;
        self.emit_acknowledgement_if_due(now, effects)
    }

    fn process_up_retransmission_request(
        &mut self,
        now: u32,
        header: SrHeader,
        effects: &mut EffectBatch,
    ) -> Result<(), EngineError> {
        if !self.valid_control_header(header) {
            return Ok(());
        }
        let in_sequence = header.sequence_number == self.snapshot.next_receive_sequence;
        if in_sequence {
            self.accept_control_message(header);
        } else {
            self.snapshot.confirmed_receive_sequence = header.confirmed_sequence_number;
        }

        effects.push(Effect::Transmit(self.retransmission_response(now)))?;
        if in_sequence {
            Ok(())
        } else {
            self.change_state(ConnectionState::RetransmissionRequest, None, effects)
        }
    }

    fn process_retransmission_request(
        &mut self,
        input: StepInput,
        effects: &mut EffectBatch,
    ) -> Result<(), EngineError> {
        match input.event {
            ConnectionEvent::Open { .. } => self.close_connection(
                input.now,
                DisconnectReason::ServiceNotAllowed,
                None,
                effects,
            ),
            ConnectionEvent::Close => {
                self.close_connection(input.now, DisconnectReason::UserRequest, None, effects)
            }
            ConnectionEvent::SendData(payload) => {
                self.send_data(input.now, ApplicationData::single(&payload), effects)
            }
            ConnectionEvent::SendPacked(data) => self.send_data(input.now, data, effects),
            ConnectionEvent::RetransmissionRequestReceived(header) => {
                if !self.valid_control_header(header) {
                    return Ok(());
                }
                if header.sequence_number == self.snapshot.next_receive_sequence {
                    self.accept_control_message(header);
                    effects.push(Effect::Transmit(self.retransmission_response(input.now)))?;
                    Ok(())
                } else {
                    self.snapshot.confirmed_receive_sequence = header.confirmed_sequence_number;
                    effects.push(Effect::Transmit(self.retransmission_response(input.now)))?;
                    Ok(())
                }
            }
            ConnectionEvent::RetransmissionResponseReceived(header) => {
                if !self.valid_control_header(header) {
                    return Ok(());
                }
                if header.sequence_number == self.snapshot.next_receive_sequence {
                    self.accept_control_message(header);
                    self.emit_acknowledgement_if_due(input.now, effects)?;
                    self.change_state(ConnectionState::RetransmissionRunning, None, effects)
                } else {
                    Ok(())
                }
            }
            ConnectionEvent::RetransmittedDataReceived(_)
            | ConnectionEvent::HeartbeatReceived(_)
            | ConnectionEvent::DataReceived(_) => Ok(()),
            ConnectionEvent::DisconnectRequestReceived(header) => {
                if self.valid_common_header(header) {
                    self.close_redundancy(Some(header), effects)
                } else {
                    Ok(())
                }
            }
            ConnectionEvent::OtherMessageReceived(header) => {
                if self.valid_common_header(header) {
                    self.close_connection(
                        input.now,
                        DisconnectReason::UnexpectedMessage,
                        Some(header),
                        effects,
                    )
                } else {
                    Ok(())
                }
            }
            ConnectionEvent::SendHeartbeat => {
                effects.push(Effect::Transmit(self.heartbeat(input.now)))
            }
            ConnectionEvent::PollTimers => self.process_timer_poll(input.now, true, effects),
            ConnectionEvent::Timeout => {
                self.close_connection(input.now, DisconnectReason::Timeout, None, effects)
            }
            ConnectionEvent::ConnectionRequestReceived(message) => {
                if self.valid_connection_request(message) {
                    self.close_connection(
                        input.now,
                        DisconnectReason::UnexpectedMessage,
                        Some(message.header),
                        effects,
                    )
                } else {
                    Ok(())
                }
            }
            ConnectionEvent::ConnectionResponseReceived(message) => {
                if self.valid_connection_response(message) {
                    self.close_connection(
                        input.now,
                        DisconnectReason::UnexpectedMessage,
                        Some(message.header),
                        effects,
                    )
                } else {
                    Ok(())
                }
            }
        }
    }

    #[allow(clippy::too_many_lines)]
    fn process_retransmission_running(
        &mut self,
        input: StepInput,
        effects: &mut EffectBatch,
    ) -> Result<(), EngineError> {
        match input.event {
            ConnectionEvent::Open { .. } => self.close_connection(
                input.now,
                DisconnectReason::ServiceNotAllowed,
                None,
                effects,
            ),
            ConnectionEvent::Close => {
                self.close_connection(input.now, DisconnectReason::UserRequest, None, effects)
            }
            ConnectionEvent::SendData(payload) => {
                self.send_data(input.now, ApplicationData::single(&payload), effects)
            }
            ConnectionEvent::SendPacked(data) => self.send_data(input.now, data, effects),
            ConnectionEvent::RetransmissionRequestReceived(header) => {
                if !self.valid_common_header(header) {
                    return Ok(());
                }
                if header.sequence_number == self.snapshot.next_receive_sequence {
                    self.close_connection(
                        input.now,
                        DisconnectReason::UnexpectedMessage,
                        Some(header),
                        effects,
                    )
                } else if self.valid_sequenced_header(header) {
                    effects.push(Effect::DiscardReceivedMessage)?;
                    effects.push(Effect::Transmit(self.retransmission_request(input.now)))?;
                    self.change_state(ConnectionState::RetransmissionRequest, None, effects)
                } else {
                    self.close_connection(
                        input.now,
                        DisconnectReason::RetransmissionFailed,
                        Some(header),
                        effects,
                    )
                }
            }
            ConnectionEvent::RetransmissionResponseReceived(header)
            | ConnectionEvent::OtherMessageReceived(header) => {
                if self.valid_common_header(header) {
                    self.close_connection(
                        input.now,
                        DisconnectReason::UnexpectedMessage,
                        Some(header),
                        effects,
                    )
                } else {
                    Ok(())
                }
            }
            ConnectionEvent::HeartbeatReceived(message) => self
                .process_retransmission_running_sequenced(
                    input.now,
                    message.header,
                    None,
                    false,
                    effects,
                ),
            ConnectionEvent::DataReceived(message) => self
                .process_retransmission_running_sequenced(
                    input.now,
                    message.header,
                    Some(&message.data),
                    false,
                    effects,
                ),
            ConnectionEvent::RetransmittedDataReceived(message) => self
                .process_retransmission_running_sequenced(
                    input.now,
                    message.header,
                    Some(&message.data),
                    true,
                    effects,
                ),
            ConnectionEvent::DisconnectRequestReceived(header) => {
                if self.valid_common_header(header) {
                    self.close_redundancy(Some(header), effects)
                } else {
                    Ok(())
                }
            }
            ConnectionEvent::SendHeartbeat => {
                effects.push(Effect::Transmit(self.heartbeat(input.now)))
            }
            ConnectionEvent::PollTimers => self.process_timer_poll(input.now, true, effects),
            ConnectionEvent::Timeout => {
                self.close_connection(input.now, DisconnectReason::Timeout, None, effects)
            }
            ConnectionEvent::ConnectionRequestReceived(message) => {
                if self.valid_connection_request(message) {
                    self.close_connection(
                        input.now,
                        DisconnectReason::UnexpectedMessage,
                        Some(message.header),
                        effects,
                    )
                } else {
                    Ok(())
                }
            }
            ConnectionEvent::ConnectionResponseReceived(message) => {
                if self.valid_connection_response(message) {
                    self.close_connection(
                        input.now,
                        DisconnectReason::UnexpectedMessage,
                        Some(message.header),
                        effects,
                    )
                } else {
                    Ok(())
                }
            }
        }
    }

    fn process_retransmission_running_sequenced(
        &mut self,
        now: u32,
        header: SrHeader,
        data: Option<&ApplicationData>,
        stay_in_retransmission_running: bool,
        effects: &mut EffectBatch,
    ) -> Result<(), EngineError> {
        if !self.valid_sequenced_header(header) {
            return Ok(());
        }
        if header.sequence_number != self.snapshot.next_receive_sequence {
            effects.push(Effect::DiscardReceivedMessage)?;
            effects.push(Effect::Transmit(self.retransmission_request(now)))?;
            return self.change_state(ConnectionState::RetransmissionRequest, None, effects);
        }
        if !wrapping_value_in_window(
            header.confirmed_timestamp,
            self.snapshot.confirmed_received_timestamp,
            self.config.timing.t_max() - 1,
        ) {
            return self.close_connection(
                now,
                DisconnectReason::ProtocolSequenceError,
                Some(header),
                effects,
            );
        }
        if !self.evaluate_timeliness(now, header) {
            return self.close_connection(now, DisconnectReason::Timeout, Some(header), effects);
        }
        if data.is_some_and(|data| !self.data_acceptable(data)) {
            return self.close_connection(
                now,
                DisconnectReason::UnexpectedMessage,
                Some(header),
                effects,
            );
        }

        self.accept_sequenced_message(header);
        if data.is_some() {
            effects.push(Effect::ApplicationDataAvailable)?;
        }
        self.emit_acknowledgement_if_due(now, effects)?;
        if stay_in_retransmission_running {
            Ok(())
        } else {
            self.change_state(ConnectionState::Up, None, effects)
        }
    }

    fn process_timer_poll(
        &mut self,
        now: u32,
        heartbeat_allowed: bool,
        effects: &mut EffectBatch,
    ) -> Result<(), EngineError> {
        if self.snapshot.timing.timeout_expired(now) {
            self.close_connection(now, DisconnectReason::Timeout, None, effects)
        } else if heartbeat_allowed && self.snapshot.timing.heartbeat_due(self.config.timing, now) {
            effects.push(Effect::Transmit(self.heartbeat(now)))
        } else {
            Ok(())
        }
    }

    fn emit_acknowledgement_if_due(
        &mut self,
        now: u32,
        effects: &mut EffectBatch,
    ) -> Result<(), EngineError> {
        if self
            .snapshot
            .timing
            .acknowledgement_due(self.config.timing, self.snapshot.confirmed_send_sequence)
        {
            effects.push(Effect::Transmit(self.heartbeat(now)))?;
        }
        Ok(())
    }

    fn valid_common_header(&self, header: SrHeader) -> bool {
        header.sender_id == self.config.remote_id && header.receiver_id == self.config.local_id
    }

    fn evaluate_timeliness(&mut self, now: u32, header: SrHeader) -> bool {
        self.snapshot
            .timing
            .evaluate(
                self.config.timing,
                now,
                header.confirmed_timestamp,
                self.snapshot.confirmed_received_timestamp,
            )
            .respected
    }

    fn valid_connection_request(&self, message: ReceivedConnectionMessage) -> bool {
        self.valid_common_header(message.header) && message.header.confirmed_sequence_number == 0
    }

    fn valid_connection_response(&self, message: ReceivedConnectionMessage) -> bool {
        self.valid_common_header(message.header)
            && message.header.confirmed_sequence_number
                == self.snapshot.next_send_sequence.wrapping_sub(1)
    }

    fn valid_sequenced_header(&self, header: SrHeader) -> bool {
        self.valid_control_header(header) && self.timestamp_in_order(header)
    }

    fn valid_control_header(&self, header: SrHeader) -> bool {
        self.valid_common_header(header)
            && self.sequence_in_range(header)
            && self.confirmed_sequence_plausible(header)
    }

    fn sequence_in_range(&self, header: SrHeader) -> bool {
        let sequence_delta = header
            .sequence_number
            .wrapping_sub(self.snapshot.next_receive_sequence);
        sequence_delta <= 10 * u32::from(self.config.n_send_max)
    }

    fn confirmed_sequence_plausible(&self, header: SrHeader) -> bool {
        let confirmed_delta = header
            .confirmed_sequence_number
            .wrapping_sub(self.snapshot.confirmed_receive_sequence);
        let Some(last_transmitted_sequence) = self.snapshot.last_transmitted_sequence else {
            return false;
        };
        let maximum_confirmed_delta =
            last_transmitted_sequence.wrapping_sub(self.snapshot.confirmed_receive_sequence);
        confirmed_delta <= maximum_confirmed_delta
    }

    fn timestamp_in_order(&self, header: SrHeader) -> bool {
        wrapping_value_in_window(
            header.timestamp,
            self.snapshot.received_timestamp,
            self.config.timing.t_max() - 1,
        )
    }

    fn protocol_version_accepted(&self, received: ProtocolVersion) -> bool {
        received.as_bytes() >= self.config.protocol_version.as_bytes()
    }

    fn accept_connection_request(&mut self, message: ReceivedConnectionMessage, now: u32) {
        self.snapshot.next_receive_sequence = message.header.sequence_number.wrapping_add(1);
        self.snapshot.confirmed_send_sequence = message.header.sequence_number;
        self.snapshot.confirmed_receive_sequence = self.snapshot.next_send_sequence.wrapping_sub(1);
        self.snapshot.received_timestamp = message.header.timestamp;
        self.snapshot.confirmed_received_timestamp = now;
        self.snapshot.remote_n_send_max = message.data.n_send_max;
        self.snapshot
            .timing
            .start_establishment(self.config.timing, now);
    }

    fn accept_connection_response(&mut self, message: ReceivedConnectionMessage) {
        self.snapshot.next_receive_sequence = message.header.sequence_number.wrapping_add(1);
        self.snapshot.confirmed_send_sequence = message.header.sequence_number;
        self.snapshot.confirmed_receive_sequence = message.header.confirmed_sequence_number;
        self.snapshot.received_timestamp = message.header.timestamp;
        self.snapshot.remote_n_send_max = message.data.n_send_max;
    }

    // Only called for timely messages, after `evaluate_timeliness`.
    fn accept_sequenced_message(&mut self, header: SrHeader) {
        self.snapshot.next_receive_sequence = header.sequence_number.wrapping_add(1);
        self.snapshot.confirmed_send_sequence = header.sequence_number;
        self.snapshot.confirmed_receive_sequence = header.confirmed_sequence_number;
        self.snapshot.received_timestamp = header.timestamp;
        self.snapshot.confirmed_received_timestamp = header.confirmed_timestamp;
        self.snapshot.diagnostics.record_timing(
            self.config.diagnostics,
            self.snapshot.timing.t_rtd,
            self.snapshot.timing.t_alive,
        );
    }

    fn accept_control_message(&mut self, header: SrHeader) {
        self.snapshot.next_receive_sequence = header.sequence_number.wrapping_add(1);
        self.snapshot.confirmed_send_sequence = header.sequence_number;
        self.snapshot.confirmed_receive_sequence = header.confirmed_sequence_number;
    }

    fn connection_request(&mut self, now: u32) -> OutboundMessage {
        self.snapshot.received_timestamp = 0;
        OutboundMessage::ConnectionRequest {
            header: self.next_outbound_header(now, true),
            data: self.local_connection_data(),
        }
    }

    fn connection_response(&mut self, now: u32) -> OutboundMessage {
        OutboundMessage::ConnectionResponse {
            header: self.next_outbound_header(now, false),
            data: self.local_connection_data(),
        }
    }

    fn heartbeat(&mut self, now: u32) -> OutboundMessage {
        OutboundMessage::Heartbeat {
            header: self.next_outbound_header(now, false),
        }
    }

    fn data(&mut self, now: u32, data: ApplicationData) -> OutboundMessage {
        OutboundMessage::Data {
            header: self.next_outbound_header(now, false),
            data,
        }
    }

    /// Queues one Data message carrying `data`.
    fn send_data(
        &mut self,
        now: u32,
        data: ApplicationData,
        effects: &mut EffectBatch,
    ) -> Result<(), EngineError> {
        if !self.data_acceptable(&data) {
            return Err(EngineError::InvalidApplicationData);
        }
        let message = self.data(now, data);
        effects.push(Effect::Transmit(message))
    }

    /// Whether an application-data field is well-formed and holds at most
    /// `N_maxPacket` messages.
    fn data_acceptable(&self, data: &ApplicationData) -> bool {
        data.message_count()
            .is_ok_and(|count| count <= usize::from(self.config.max_packet))
    }

    fn retransmission_request(&mut self, now: u32) -> OutboundMessage {
        OutboundMessage::RetransmissionRequest {
            header: self.next_outbound_header(now, false),
        }
    }

    fn retransmission_response(&mut self, now: u32) -> OutboundMessage {
        OutboundMessage::RetransmissionResponse {
            header: self.next_outbound_header(now, false),
        }
    }

    fn local_connection_data(&self) -> ConnectionData {
        ConnectionData::new(self.config.protocol_version, self.config.n_send_max)
    }

    fn next_outbound_header(&mut self, now: u32, connection_request: bool) -> SrHeader {
        let sequence_number = self.snapshot.next_send_sequence;
        self.snapshot.next_send_sequence = sequence_number.wrapping_add(1);
        SrHeader {
            receiver_id: self.config.remote_id,
            sender_id: self.config.local_id,
            sequence_number,
            confirmed_sequence_number: if connection_request {
                0
            } else {
                self.snapshot.confirmed_send_sequence
            },
            timestamp: now,
            confirmed_timestamp: if connection_request {
                0
            } else {
                self.snapshot.received_timestamp
            },
        }
    }

    fn close_connection(
        &mut self,
        now: u32,
        reason: DisconnectReason,
        incoming: Option<SrHeader>,
        effects: &mut EffectBatch,
    ) -> Result<(), EngineError> {
        if let Some(header) = incoming {
            self.snapshot.confirmed_send_sequence = header.sequence_number;
        }
        let message = OutboundMessage::DisconnectionRequest {
            header: self.next_outbound_header(now, false),
            detailed_reason: 0,
            reason,
        };
        effects.push(Effect::Transmit(message))?;
        // Send DiscReq on the active channel before resetting RL sequence state.
        effects.push(Effect::CloseRedundancyChannel)?;
        self.report_diagnostics(effects)?;
        self.change_state(ConnectionState::Closed, Some(reason), effects)
    }

    fn close_redundancy(
        &mut self,
        incoming: Option<SrHeader>,
        effects: &mut EffectBatch,
    ) -> Result<(), EngineError> {
        if let Some(header) = incoming {
            self.snapshot.confirmed_send_sequence = header.sequence_number;
        }
        effects.push(Effect::CloseRedundancyChannel)?;
        self.report_diagnostics(effects)?;
        self.change_state(ConnectionState::Closed, None, effects)
    }

    fn change_state(
        &mut self,
        state: ConnectionState,
        disconnect_reason: Option<DisconnectReason>,
        effects: &mut EffectBatch,
    ) -> Result<(), EngineError> {
        let previous = self.snapshot.state;
        self.snapshot.state = state;
        effects.push(Effect::StateChanged(StateChange {
            previous,
            current: state,
            disconnect_reason,
        }))
    }
}

const fn wrapping_value_in_window(value: u32, start: u32, maximum_delta: u32) -> bool {
    value.wrapping_sub(start) <= maximum_delta
}
