//! Composition of SRL, Redundancy Layer, safety code, and wire codecs.
#![allow(
    clippy::large_enum_variant,
    clippy::large_stack_arrays,
    clippy::large_types_passed_by_value
)]

use core::fmt;

use crate::{
    codec::{
        safety_code::{verify_md4_safety_code, Md4InitialValue, SafetyCodeError},
        sr::{decode_and_verify_md4, CodecError, SrBody, SrCodecConfig, MAX_SR_PDU_LEN},
    },
    engine::{
        buffers::{ApplicationData, ApplicationPayload, BufferUsage, ConnectionBuffers},
        connection::{
            ConnectionConfig, ConnectionEngine, ConnectionEvent, ConnectionSnapshot, Effect,
            EffectBatch, EngineError, ReceivedConnectionMessage, ReceivedData, ReceivedHeartbeat,
            StateChange, StepInput,
        },
        diagnostics::ConnectionDiagnostics,
    },
    redundancy::{
        codec::{
            decode as decode_redundancy, CheckCodeType, RedundancyCodecError, RedundancyFrame,
            MAX_REDUNDANCY_FRAME_LEN,
        },
        engine::{
            RedundancyConfig, RedundancyEffect, RedundancyEffects, RedundancyEngine,
            RedundancyError, RedundancyEvent, RedundancyInput, RedundancyPayload,
            RedundancySnapshot, RedundancyState, TransportChannelDiagnostics,
        },
    },
};

// A full defer flush with two-channel acknowledgements, plus one SR and two RL
// diagnostic reports.
const ACTION_CAPACITY: usize = 43;

/// Capacity of the receive queue between the Redundancy Layer and SR, as in
/// the reference (OBS-SBB-RL-007).
pub const RECEIVE_QUEUE_CAPACITY: usize = 20;
// A queued PDU is processed only while SR has this much room (OBS-SBB-RL-007).
const MIN_FREE_RECEIVE_ENTRIES: u16 = 1;
const MIN_FREE_SEND_ENTRIES: u16 = 3;
// One frame plus a full defer-queue flush, the burst ACTION_CAPACITY is sized
// for. Further queued PDUs wait for the next operation.
const MAX_PDUS_PER_OPERATION: usize = 11;

/// Codec profiles used by a composed stack.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct StackCodecConfig {
    /// SR PDU codec configuration.
    pub sr: SrCodecConfig,
    /// SR MD4 initial state.
    pub md4_initial_value: Md4InitialValue,
    /// Redundancy Layer check-code profile.
    pub redundancy_check_code: CheckCodeType,
}

/// One complete datagram ready for a configured transport channel.
#[derive(Clone, Copy, Eq, PartialEq)]
pub struct WireDatagram {
    channel: u8,
    bytes: [u8; MAX_REDUNDANCY_FRAME_LEN],
    length: u16,
}

impl WireDatagram {
    /// Returns the transport channel index.
    #[must_use]
    pub const fn channel(&self) -> u8 {
        self.channel
    }

    /// Returns the complete Redundancy Layer datagram.
    #[must_use]
    pub fn as_slice(&self) -> &[u8] {
        &self.bytes[..usize::from(self.length)]
    }
}

impl fmt::Debug for WireDatagram {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("WireDatagram")
            .field("channel", &self.channel)
            .field("length", &self.length)
            .finish_non_exhaustive()
    }
}

/// Observable output from a composed stack step.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StackAction {
    /// Send one complete UDP-compatible datagram.
    Transmit(WireDatagram),
    /// One application payload entered the SR receive buffer.
    ApplicationDataAvailable,
    /// SR connection state changed.
    ConnectionStateChanged(StateChange),
    /// Redundancy channel state changed.
    RedundancyStateChanged {
        /// Previous state.
        previous: RedundancyState,
        /// Current state.
        current: RedundancyState,
    },
    /// A received SR message was intentionally discarded.
    DiscardedSrMessage,
    /// SR diagnostics of a completed diagnostic window, or of the window cut
    /// short by closing the connection (OBS-SBB-DIAG-002).
    ConnectionDiagnostics(ConnectionDiagnostics),
    /// One transport channel's diagnostics for a completed diagnosis window
    /// (OBS-SBB-DIAG-003).
    RedundancyDiagnostics {
        /// Transport channel index.
        channel: u8,
        /// Diagnostics of the window.
        diagnostics: TransportChannelDiagnostics,
    },
}

/// Fixed-capacity output actions from one composed operation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct StackActions {
    items: [Option<StackAction>; ACTION_CAPACITY],
    length: u8,
}

impl StackActions {
    /// Creates empty output storage, including for static placement.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            items: [None; ACTION_CAPACITY],
            length: 0,
        }
    }

    /// Returns action count.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.length as usize
    }

    /// Returns whether no actions were emitted.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.length == 0
    }

    /// Returns an action by execution-order index.
    #[must_use]
    pub fn get(&self, index: usize) -> Option<&StackAction> {
        (index < self.len())
            .then(|| self.items[index].as_ref())
            .flatten()
    }

    /// Iterates over actions in execution order.
    pub fn iter(&self) -> impl Iterator<Item = &StackAction> {
        self.items[..self.len()].iter().filter_map(Option::as_ref)
    }

    fn clear(&mut self) {
        let length = self.len();
        self.items[..length].fill(None);
        self.length = 0;
    }

    fn push(&mut self, action: StackAction) -> Result<(), StackError> {
        let index = self.len();
        if index == ACTION_CAPACITY {
            return Err(StackError::ActionCapacityExceeded);
        }
        self.items[index] = Some(action);
        self.length += 1;
        Ok(())
    }
}

impl Default for StackActions {
    fn default() -> Self {
        Self::new()
    }
}

/// Reusable transaction storage for the `*_into` operations.
///
/// Place this and [`StackActions`] in static storage or an arena when task
/// stacks are small. Construction is const; operations allocate no memory.
#[derive(Debug)]
pub struct StackWorkspace {
    candidate: StackEngine,
    scratch: StackScratch,
}

impl StackWorkspace {
    /// Creates scratch storage. It can be reused across stacks/configurations.
    #[must_use]
    pub const fn new(
        connection: ConnectionConfig,
        redundancy: RedundancyConfig,
        codecs: StackCodecConfig,
    ) -> Self {
        Self {
            candidate: StackEngine::new(connection, redundancy, codecs),
            scratch: StackScratch::new(),
        }
    }
}

// Separate incoming RL deliveries from the outgoing RL effects generated by
// each SR step. This avoids recursive batch translation and large temporaries.
#[derive(Debug)]
struct StackScratch {
    incoming: RedundancyEffects,
    outgoing: RedundancyEffects,
    connection: EffectBatch,
}

impl StackScratch {
    const fn new() -> Self {
        Self {
            incoming: RedundancyEffects::new(),
            outgoing: RedundancyEffects::new(),
            connection: EffectBatch::new(),
        }
    }
}

// SR PDUs that RL has delivered but SR has not processed yet, oldest first.
#[derive(Clone, Debug, Eq, PartialEq)]
struct ReceiveQueue {
    entries: [Option<RedundancyPayload>; RECEIVE_QUEUE_CAPACITY],
    head: usize,
    len: usize,
}

impl ReceiveQueue {
    const fn new() -> Self {
        Self {
            entries: [None; RECEIVE_QUEUE_CAPACITY],
            head: 0,
            len: 0,
        }
    }

    const fn free(&self) -> usize {
        RECEIVE_QUEUE_CAPACITY - self.len
    }

    fn push(&mut self, payload: RedundancyPayload) -> Result<(), StackError> {
        if self.len == RECEIVE_QUEUE_CAPACITY {
            return Err(StackError::ReceiveQueueFull);
        }
        self.entries[(self.head + self.len) % RECEIVE_QUEUE_CAPACITY] = Some(payload);
        self.len += 1;
        Ok(())
    }

    fn pop(&mut self) -> Option<RedundancyPayload> {
        let payload = self.entries[self.head].take()?;
        self.head = (self.head + 1) % RECEIVE_QUEUE_CAPACITY;
        self.len -= 1;
        Some(payload)
    }

    fn clear(&mut self) {
        self.entries.fill(None);
        self.head = 0;
        self.len = 0;
    }

    fn copy_from(&mut self, source: &Self) {
        self.entries.copy_from_slice(&source.entries);
        self.head = source.head;
        self.len = source.len;
    }
}

/// Complete deterministic `RaSTA` stack without platform I/O.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StackEngine {
    codecs: StackCodecConfig,
    connection: ConnectionEngine,
    connection_buffers: ConnectionBuffers,
    redundancy: RedundancyEngine,
    receive_queue: ReceiveQueue,
}

impl StackEngine {
    /// Creates a closed composed stack.
    ///
    /// The configurations are validated separately, not against each other.
    /// The local `N_SENDMAX` of `connection_config` must lie within the
    /// `N_SENDMAX` range of `codecs.sr`, which
    /// [`SrCodecConfig::with_default_limits`] always covers. Otherwise encoding
    /// the first connection request or response fails, and the step that needs
    /// it returns [`StackError::SrCodec`] without changing state.
    #[must_use]
    pub const fn new(
        connection_config: ConnectionConfig,
        redundancy_config: RedundancyConfig,
        codecs: StackCodecConfig,
    ) -> Self {
        Self {
            codecs,
            connection: ConnectionEngine::new(connection_config),
            connection_buffers: ConnectionBuffers::new(connection_config),
            redundancy: RedundancyEngine::new(redundancy_config),
            receive_queue: ReceiveQueue::new(),
        }
    }

    /// Returns the SR connection snapshot.
    #[must_use]
    pub const fn connection_snapshot(&self) -> ConnectionSnapshot {
        self.connection.snapshot()
    }

    /// Returns the Redundancy Layer snapshot.
    #[must_use]
    pub const fn redundancy_snapshot(&self) -> RedundancySnapshot {
        self.redundancy.snapshot()
    }

    /// Removes the oldest delivered application payload.
    ///
    /// Freeing an entry does not process queued PDUs; the next
    /// [`Self::poll`] or received datagram does, as in the reference.
    pub fn pop_received(&mut self) -> Option<ApplicationPayload> {
        self.connection_buffers.pop_received()
    }

    /// Returns the occupancy of the SR send buffer: sent messages awaiting
    /// confirmation, heartbeats included, plus messages not yet sent.
    ///
    /// Keep at least 3 entries free. As in the reference (OBS-SBB-RL-007), SR
    /// processes no queued PDU while fewer are free, not even the peer's
    /// confirmations that would free entries, so the connection times out.
    #[must_use]
    pub const fn send_usage(&self) -> BufferUsage {
        self.connection_buffers.send_usage()
    }

    /// Returns the occupancy of the SR receive buffer.
    #[must_use]
    pub const fn receive_usage(&self) -> BufferUsage {
        self.connection_buffers.receive_usage()
    }

    /// Returns the number of received SR PDUs waiting for room in SR.
    #[must_use]
    pub const fn queued_pdus(&self) -> usize {
        self.receive_queue.len
    }

    /// Returns whether the stack can take the next transport datagram.
    ///
    /// As in the reference (OBS-SBB-RL-007), RL takes a frame only while the
    /// receive queue has more free entries than the defer queue holds frames,
    /// so that the frame and a complete defer flush fit. While this returns
    /// `false`, leave datagrams in the transport, for example the socket
    /// buffer, and call [`Self::poll`]; it processes queued PDUs as SR makes
    /// room.
    #[must_use]
    pub fn can_receive_datagram(&self) -> bool {
        self.receive_queue.free() > usize::from(self.redundancy.snapshot().deferred_messages)
    }

    /// Processes one application, lifecycle, or timer event atomically.
    ///
    /// This convenience API uses large stack temporaries. Use
    /// [`Self::step_connection_into`] with persistent storage on small stacks.
    ///
    /// # Errors
    /// Returns a transition, codec, or action-capacity error; state is unchanged.
    pub fn step_connection(&mut self, input: StepInput) -> Result<StackActions, StackError> {
        let mut candidate = self.clone();
        let mut actions = StackActions::new();
        let mut scratch = StackScratch::new();
        candidate.step_connection_inner(input, &mut scratch, &mut actions)?;
        self.copy_from(&candidate);
        Ok(actions)
    }

    /// Processes an event using reusable transaction and output storage.
    ///
    /// # Errors
    /// On error, state is unchanged and `actions` is empty.
    pub fn step_connection_into(
        &mut self,
        input: StepInput,
        workspace: &mut StackWorkspace,
        actions: &mut StackActions,
    ) -> Result<(), StackError> {
        actions.clear();
        workspace.candidate.copy_from(self);
        let result =
            workspace
                .candidate
                .step_connection_inner(input, &mut workspace.scratch, actions);
        self.finish_transaction(workspace, actions, result)
    }

    /// Decodes and processes one complete transport datagram atomically.
    ///
    /// Delivered SR PDUs enter the receive queue, and SR processes queued PDUs
    /// while it has room. Invalid SR PDUs are consumed by RL and emit
    /// `DiscardedSrMessage`. Call it only while [`Self::can_receive_datagram`]
    /// returns `true`. This convenience API uses large stack temporaries;
    /// prefer [`Self::receive_datagram_into`] on small stacks.
    ///
    /// # Errors
    /// Malformed RL frames, failed transitions, or a call while the receive
    /// queue lacks room ([`StackError::ReceiveQueueFull`]) leave state
    /// unchanged.
    pub fn receive_datagram(
        &mut self,
        now: u32,
        channel: u8,
        datagram: &[u8],
    ) -> Result<StackActions, StackError> {
        let mut candidate = self.clone();
        let mut actions = StackActions::new();
        let mut scratch = StackScratch::new();
        candidate.receive_datagram_inner(now, channel, datagram, &mut scratch, &mut actions)?;
        self.copy_from(&candidate);
        Ok(actions)
    }

    /// Receives a datagram using reusable transaction and output storage.
    ///
    /// Invalid SR PDUs emit `DiscardedSrMessage`, preserving valid deliveries
    /// in the same batch and allowing defer and inactivity timers to advance.
    ///
    /// # Errors
    /// On malformed RL frames or transition errors, state is unchanged and
    /// `actions` is empty.
    pub fn receive_datagram_into(
        &mut self,
        now: u32,
        channel: u8,
        datagram: &[u8],
        workspace: &mut StackWorkspace,
        actions: &mut StackActions,
    ) -> Result<(), StackError> {
        actions.clear();
        workspace.candidate.copy_from(self);
        let result = workspace.candidate.receive_datagram_inner(
            now,
            channel,
            datagram,
            &mut workspace.scratch,
            actions,
        );
        self.finish_transaction(workspace, actions, result)
    }

    /// Polls Redundancy Layer defer timers, processes queued PDUs while SR has
    /// room, and polls SR adaptive timers atomically.
    ///
    /// This convenience API uses large stack temporaries. Prefer
    /// [`Self::poll_into`] on small stacks.
    ///
    /// # Errors
    /// Returns a transition or codec error; state is unchanged.
    pub fn poll(&mut self, now: u32) -> Result<StackActions, StackError> {
        let mut candidate = self.clone();
        let mut actions = StackActions::new();
        let mut scratch = StackScratch::new();
        candidate.poll_inner(now, &mut scratch, &mut actions)?;
        self.copy_from(&candidate);
        Ok(actions)
    }

    /// Polls timers using reusable transaction and output storage.
    ///
    /// # Errors
    /// On transition or codec errors, state is unchanged and `actions` is empty.
    pub fn poll_into(
        &mut self,
        now: u32,
        workspace: &mut StackWorkspace,
        actions: &mut StackActions,
    ) -> Result<(), StackError> {
        actions.clear();
        workspace.candidate.copy_from(self);
        let result = workspace
            .candidate
            .poll_inner(now, &mut workspace.scratch, actions);
        self.finish_transaction(workspace, actions, result)
    }

    fn copy_from(&mut self, source: &Self) {
        self.codecs = source.codecs;
        self.connection = source.connection;
        self.connection_buffers
            .copy_from(&source.connection_buffers);
        self.redundancy.copy_from(&source.redundancy);
        self.receive_queue.copy_from(&source.receive_queue);
    }

    fn finish_transaction(
        &mut self,
        workspace: &StackWorkspace,
        actions: &mut StackActions,
        result: Result<(), StackError>,
    ) -> Result<(), StackError> {
        if result.is_ok() {
            self.copy_from(&workspace.candidate);
        } else {
            actions.clear();
        }
        result
    }

    fn step_connection_inner(
        &mut self,
        input: StepInput,
        scratch: &mut StackScratch,
        actions: &mut StackActions,
    ) -> Result<(), StackError> {
        self.connection
            .step_in_place(&mut self.connection_buffers, input, &mut scratch.connection)
            .map_err(StackError::Connection)?;
        self.translate_connection_effects(
            input.now,
            &scratch.connection,
            &mut scratch.outgoing,
            actions,
        )
    }

    fn receive_datagram_inner(
        &mut self,
        now: u32,
        channel: u8,
        datagram: &[u8],
        scratch: &mut StackScratch,
        actions: &mut StackActions,
    ) -> Result<(), StackError> {
        if !self.can_receive_datagram() {
            return Err(StackError::ReceiveQueueFull);
        }
        let frame = decode_redundancy(datagram, self.codecs.redundancy_check_code)
            .map_err(StackError::RedundancyCodec)?;
        let payload =
            RedundancyPayload::new(frame.payload).map_err(StackError::RedundancyEngine)?;
        self.redundancy
            .step_in_place(
                RedundancyInput {
                    now,
                    event: RedundancyEvent::FrameReceived {
                        channel,
                        sequence_number: frame.sequence_number,
                        payload,
                    },
                },
                &mut scratch.incoming,
            )
            .map_err(StackError::RedundancyEngine)?;
        self.translate_deliveries(now, scratch, actions)
    }

    fn poll_inner(
        &mut self,
        now: u32,
        scratch: &mut StackScratch,
        actions: &mut StackActions,
    ) -> Result<(), StackError> {
        self.redundancy
            .step_in_place(
                RedundancyInput {
                    now,
                    event: RedundancyEvent::Poll,
                },
                &mut scratch.incoming,
            )
            .map_err(StackError::RedundancyEngine)?;
        self.translate_deliveries(now, scratch, actions)?;
        self.step_connection_inner(
            StepInput {
                now,
                event: ConnectionEvent::PollTimers,
            },
            scratch,
            actions,
        )
    }

    fn translate_deliveries(
        &mut self,
        now: u32,
        scratch: &mut StackScratch,
        actions: &mut StackActions,
    ) -> Result<(), StackError> {
        for effect in scratch.incoming.iter() {
            if let RedundancyEffect::Deliver(payload) = effect {
                self.receive_queue.push(*payload)?;
            } else {
                self.emit_redundancy_action(effect, actions)?;
            }
        }
        self.process_receive_queue(now, scratch, actions)
    }

    // SR takes queued PDUs only while it has room (OBS-SBB-RL-007).
    fn process_receive_queue(
        &mut self,
        now: u32,
        scratch: &mut StackScratch,
        actions: &mut StackActions,
    ) -> Result<(), StackError> {
        for _ in 0..MAX_PDUS_PER_OPERATION {
            if self.connection_buffers.receive_usage().free < MIN_FREE_RECEIVE_ENTRIES
                || self.connection_buffers.send_usage().free < MIN_FREE_SEND_ENTRIES
            {
                break;
            }
            let Some(pdu) = self.receive_queue.pop() else {
                break;
            };
            self.process_sr_pdu(
                now,
                pdu.as_slice(),
                &mut scratch.connection,
                &mut scratch.outgoing,
                actions,
            )?;
        }
        Ok(())
    }

    fn translate_connection_effects(
        &mut self,
        now: u32,
        effects: &EffectBatch,
        outgoing: &mut RedundancyEffects,
        actions: &mut StackActions,
    ) -> Result<(), StackError> {
        for effect in effects.iter() {
            let event = match effect {
                // Like the RL received buffer in the reference, the queue
                // starts empty with each redundancy channel session.
                Effect::OpenRedundancyChannel => {
                    self.receive_queue.clear();
                    Some(RedundancyEvent::Open)
                }
                Effect::CloseRedundancyChannel => {
                    self.receive_queue.clear();
                    Some(RedundancyEvent::Close)
                }
                Effect::Transmit(message) => {
                    let mut encoded = [0_u8; MAX_SR_PDU_LEN];
                    let written = message
                        .as_sr_message()
                        .encode_with_md4(
                            self.codecs.sr,
                            self.codecs.md4_initial_value,
                            &mut encoded,
                        )
                        .map_err(StackError::SrCodec)?;
                    let payload = RedundancyPayload::new(&encoded[..written])
                        .map_err(StackError::RedundancyEngine)?;
                    Some(RedundancyEvent::Send(payload))
                }
                Effect::DiscardReceivedMessage => {
                    actions.push(StackAction::DiscardedSrMessage)?;
                    None
                }
                Effect::ApplicationDataAvailable => {
                    actions.push(StackAction::ApplicationDataAvailable)?;
                    None
                }
                Effect::StateChanged(change) => {
                    actions.push(StackAction::ConnectionStateChanged(*change))?;
                    None
                }
                Effect::Diagnostics(diagnostics) => {
                    actions.push(StackAction::ConnectionDiagnostics(*diagnostics))?;
                    None
                }
            };
            if let Some(event) = event {
                self.redundancy
                    .step_in_place(RedundancyInput { now, event }, outgoing)
                    .map_err(StackError::RedundancyEngine)?;
                for effect in outgoing.iter() {
                    self.emit_redundancy_action(effect, actions)?;
                }
            }
        }
        Ok(())
    }

    fn emit_redundancy_action(
        &self,
        effect: &RedundancyEffect,
        actions: &mut StackActions,
    ) -> Result<(), StackError> {
        match effect {
            RedundancyEffect::Transmit {
                channel,
                sequence_number,
                payload,
            } => {
                let frame = RedundancyFrame {
                    sequence_number: *sequence_number,
                    payload: payload.as_slice(),
                };
                let mut bytes = [0_u8; MAX_REDUNDANCY_FRAME_LEN];
                let written = frame
                    .encode_into(self.codecs.redundancy_check_code, &mut bytes)
                    .map_err(StackError::RedundancyCodec)?;
                let length =
                    u16::try_from(written).map_err(|_| StackError::ActionCapacityExceeded)?;
                actions.push(StackAction::Transmit(WireDatagram {
                    channel: *channel,
                    bytes,
                    length,
                }))
            }
            RedundancyEffect::StateChanged { previous, current } => {
                actions.push(StackAction::RedundancyStateChanged {
                    previous: *previous,
                    current: *current,
                })
            }
            RedundancyEffect::Diagnostics {
                channel,
                diagnostics,
            } => actions.push(StackAction::RedundancyDiagnostics {
                channel: *channel,
                diagnostics: *diagnostics,
            }),
            // Open/Close/Send never deliver incoming PDUs, and receive
            // deliveries are handled before this point.
            RedundancyEffect::Deliver(_) => Err(StackError::UnexpectedDelivery),
        }
    }

    // Counts an undecodable PDU in the reference's order: a safety-code
    // mismatch first, then an undefined type. Size errors are not counted
    // (OBS-SBB-DIAG-001).
    fn record_invalid_pdu(&mut self, pdu: &[u8], error: CodecError) {
        let safety_code = verify_md4_safety_code(
            pdu,
            self.codecs.sr.safety_code_length(),
            self.codecs.md4_initial_value,
        );
        if safety_code == Err(SafetyCodeError::Mismatch) {
            self.connection.record_safety_code_error();
        } else if matches!(error, CodecError::UnknownMessageType(_)) {
            self.connection.record_type_error();
        }
    }

    fn process_sr_pdu(
        &mut self,
        now: u32,
        pdu: &[u8],
        connection_effects: &mut EffectBatch,
        outgoing: &mut RedundancyEffects,
        actions: &mut StackActions,
    ) -> Result<(), StackError> {
        let message =
            match decode_and_verify_md4(pdu, self.codecs.sr, self.codecs.md4_initial_value) {
                Ok(message) => message,
                Err(error) => {
                    self.record_invalid_pdu(pdu, error);
                    // RL has consumed this sequence. A malformed SR PDU must not
                    // roll back earlier deliveries or remain at the head of the
                    // defer queue.
                    return actions.push(StackAction::DiscardedSrMessage);
                }
            };
        let event = match message.body {
            SrBody::ConnectionRequest(data) => {
                ConnectionEvent::ConnectionRequestReceived(ReceivedConnectionMessage {
                    header: message.header,
                    data,
                })
            }
            SrBody::ConnectionResponse(data) => {
                ConnectionEvent::ConnectionResponseReceived(ReceivedConnectionMessage {
                    header: message.header,
                    data,
                })
            }
            SrBody::RetransmissionRequest => {
                ConnectionEvent::RetransmissionRequestReceived(message.header)
            }
            SrBody::RetransmissionResponse => {
                ConnectionEvent::RetransmissionResponseReceived(message.header)
            }
            // The peer's reason is not reported (OBS-SBB-SM-014).
            SrBody::DisconnectionRequest { .. } => {
                ConnectionEvent::DisconnectRequestReceived(message.header)
            }
            SrBody::Heartbeat => ConnectionEvent::HeartbeatReceived(ReceivedHeartbeat {
                header: message.header,
            }),
            // The field's structure is checked by the engine: the safety code
            // is valid, so a malformed field is a protocol violation.
            SrBody::Data(field) => {
                let Ok(data) = ApplicationData::from_field(field) else {
                    return actions.push(StackAction::DiscardedSrMessage);
                };
                ConnectionEvent::DataReceived(ReceivedData {
                    header: message.header,
                    data,
                })
            }
            SrBody::RetransmittedData(field) => {
                let Ok(data) = ApplicationData::from_field(field) else {
                    return actions.push(StackAction::DiscardedSrMessage);
                };
                ConnectionEvent::RetransmittedDataReceived(ReceivedData {
                    header: message.header,
                    data,
                })
            }
        };
        self.connection
            .step_in_place(
                &mut self.connection_buffers,
                StepInput { now, event },
                connection_effects,
            )
            .map_err(StackError::Connection)?;
        self.translate_connection_effects(now, connection_effects, outgoing, actions)
    }
}

/// Composed stack operation failure.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StackError {
    /// SR connection transition failed.
    Connection(EngineError),
    /// SR PDU codec or safety-code verification failed.
    SrCodec(CodecError),
    /// Redundancy transition failed.
    RedundancyEngine(RedundancyError),
    /// Redundancy frame codec failed.
    RedundancyCodec(RedundancyCodecError),
    /// A fixed composed action bound was insufficient.
    ActionCapacityExceeded,
    /// The receive queue cannot take another frame; check
    /// [`StackEngine::can_receive_datagram`] before reading the transport.
    ReceiveQueueFull,
    /// Internal invariant violated: the Redundancy Layer delivered a PDU
    /// outside a receive operation.
    UnexpectedDelivery,
}

impl fmt::Display for StackError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Connection(error) => write!(formatter, "connection engine: {error}"),
            Self::SrCodec(error) => write!(formatter, "SR codec: {error}"),
            Self::RedundancyEngine(error) => write!(formatter, "redundancy engine: {error}"),
            Self::RedundancyCodec(error) => write!(formatter, "redundancy codec: {error}"),
            Self::ActionCapacityExceeded => {
                formatter.write_str("composed stack action capacity exceeded")
            }
            Self::ReceiveQueueFull => {
                formatter.write_str("receive queue cannot take another frame")
            }
            Self::UnexpectedDelivery => {
                formatter.write_str("redundancy layer delivered a PDU outside a receive operation")
            }
        }
    }
}

impl core::error::Error for StackError {}
