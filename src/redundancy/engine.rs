//! Deterministic duplicate suppression and bounded reordering engine.
#![allow(clippy::large_types_passed_by_value)]

use core::fmt;

use super::codec::MAX_REDUNDANCY_PAYLOAD_LEN;

const MAX_DEFERRED: usize = 10;
const MAX_CHANNELS: usize = 2;
// Two diagnostic reports, a delivery, and a full defer flush.
const EFFECT_CAPACITY: usize = MAX_DEFERRED + MAX_CHANNELS + 1;
/// Largest diagnosis window, as in the reference (OBS-SBB-DIAG-003).
pub const MAX_DIAGNOSIS_WINDOW: u16 = 1000;

/// Owned bounded SR PDU passed through the Redundancy Layer.
#[derive(Clone, Copy, Eq, PartialEq)]
pub struct RedundancyPayload {
    bytes: [u8; MAX_REDUNDANCY_PAYLOAD_LEN],
    length: u16,
}

impl RedundancyPayload {
    /// Copies one complete SR PDU into fixed-capacity storage.
    ///
    /// # Errors
    ///
    /// Returns an error unless the PDU width is in `28..=1101`.
    pub fn new(bytes: &[u8]) -> Result<Self, RedundancyError> {
        if !(28..=MAX_REDUNDANCY_PAYLOAD_LEN).contains(&bytes.len()) {
            return Err(RedundancyError::InvalidPayloadLength(bytes.len()));
        }
        let mut payload = Self {
            bytes: [0; MAX_REDUNDANCY_PAYLOAD_LEN],
            length: u16::try_from(bytes.len())
                .map_err(|_| RedundancyError::InvalidPayloadLength(bytes.len()))?,
        };
        payload.bytes[..bytes.len()].copy_from_slice(bytes);
        Ok(payload)
    }

    /// Returns the wrapped SR PDU bytes.
    #[must_use]
    pub fn as_slice(&self) -> &[u8] {
        &self.bytes[..usize::from(self.length)]
    }
}

impl fmt::Debug for RedundancyPayload {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RedundancyPayload")
            .field("length", &self.length)
            .finish_non_exhaustive()
    }
}

/// Validated Redundancy Layer configuration.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RedundancyConfig {
    t_seq: u32,
    defer_capacity: u8,
    transport_channels: u8,
    diagnosis_window: u16,
}

impl RedundancyConfig {
    /// Creates a validated Redundancy Layer configuration.
    ///
    /// `diagnosis_window` is the number of distinct frames after which the
    /// per-channel diagnostics are reported and reset (`N_diagnosis`).
    ///
    /// # Errors
    ///
    /// `T_seq` must be `50..=500`, defer capacity `4..=10`, transport
    /// channel count `1..=2`, and the diagnosis window
    /// `1..=MAX_DIAGNOSIS_WINDOW`.
    pub const fn new(
        t_seq: u32,
        defer_capacity: u8,
        transport_channels: u8,
        diagnosis_window: u16,
    ) -> Result<Self, RedundancyError> {
        if t_seq < 50 || t_seq > 500 {
            return Err(RedundancyError::InvalidTSeq(t_seq));
        }
        if defer_capacity < 4 || defer_capacity > 10 {
            return Err(RedundancyError::InvalidDeferCapacity(defer_capacity));
        }
        if transport_channels < 1 || transport_channels > 2 {
            return Err(RedundancyError::InvalidChannelCount(transport_channels));
        }
        if diagnosis_window == 0 || diagnosis_window > MAX_DIAGNOSIS_WINDOW {
            return Err(RedundancyError::InvalidDiagnosisWindow(diagnosis_window));
        }
        Ok(Self {
            t_seq,
            defer_capacity,
            transport_channels,
            diagnosis_window,
        })
    }

    /// Returns `T_seq` in milliseconds.
    #[must_use]
    pub const fn t_seq(self) -> u32 {
        self.t_seq
    }

    /// Returns configured transport-channel count.
    #[must_use]
    pub const fn transport_channels(self) -> u8 {
        self.transport_channels
    }

    /// Returns the diagnosis window in distinct frames.
    #[must_use]
    pub const fn diagnosis_window(self) -> u16 {
        self.diagnosis_window
    }
}

/// Redundancy channel lifecycle state.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RedundancyState {
    /// Channel is inactive.
    Closed,
    /// Channel accepts send and receive events.
    Up,
}

/// Diagnostics of one transport channel over one diagnosis window
/// (OBS-SBB-DIAG-003).
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct TransportChannelDiagnostics {
    /// Distinct frames in the window (`N_diagnosis`).
    pub frames: u32,
    /// Frames that did not arrive on this channel within `T_seq` of their
    /// first arrival on any channel (`N_missed`).
    pub missed: u32,
    /// Sum of this channel's delays behind the first arrival, in milliseconds
    /// (`T_drift`).
    pub drift: u32,
    /// Sum of the squares of those delays (`T_drift2`).
    pub drift_squared: u32,
}

/// Counters retained in replayable state. They have no counterpart in the
/// reference implementations, unlike [`TransportChannelDiagnostics`].
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct RedundancyDiagnostics {
    /// Older or duplicate frames discarded.
    pub duplicates: u32,
    /// Future frames outside the admissible window discarded.
    pub out_of_window: u32,
    /// Frames admitted to the defer queue.
    pub deferred: u32,
    /// Missing sequence gaps skipped after `T_seq`.
    pub sequence_timeouts: u32,
    /// Admissible future frames discarded because the defer queue was full.
    pub defer_queue_full: u32,
}

/// Complete replayable Redundancy Layer snapshot.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RedundancySnapshot {
    /// Lifecycle state.
    pub state: RedundancyState,
    /// Sequence assigned to the next outgoing frame.
    pub next_transmit_sequence: u32,
    /// Next sequence expected from parallel channels.
    pub next_receive_sequence: u32,
    /// Current defer queue occupancy.
    pub deferred_messages: u8,
    /// Diagnostic counters.
    pub diagnostics: RedundancyDiagnostics,
}

/// Input event for one deterministic Redundancy Layer step.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RedundancyEvent {
    /// Open and reset the redundancy channel.
    Open,
    /// Close and reset the redundancy channel.
    Close,
    /// Wrap and send one SR PDU over every configured transport channel.
    Send(RedundancyPayload),
    /// Deliver one verified frame received from a transport channel.
    FrameReceived {
        /// Configured transport channel index.
        channel: u8,
        /// Redundancy-layer sequence number.
        sequence_number: u32,
        /// Wrapped SR PDU.
        payload: RedundancyPayload,
    },
    /// Evaluate `T_seq` for deferred frames.
    Poll,
}

/// Input event paired with wrapping monotonic milliseconds.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RedundancyInput {
    /// Current protocol time.
    pub now: u32,
    /// Event to process.
    pub event: RedundancyEvent,
}

/// Ordered side effect emitted by the Redundancy Layer.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RedundancyEffect {
    /// Transmit the same logical frame on one transport channel.
    Transmit {
        /// Transport channel index.
        channel: u8,
        /// Redundancy sequence number.
        sequence_number: u32,
        /// Wrapped SR PDU.
        payload: RedundancyPayload,
    },
    /// Deliver one ordered, de-duplicated SR PDU upward.
    Deliver(RedundancyPayload),
    /// Notify lifecycle transition.
    StateChanged {
        /// Previous state.
        previous: RedundancyState,
        /// Current state.
        current: RedundancyState,
    },
    /// Report one transport channel's diagnostics for a completed diagnosis
    /// window.
    Diagnostics {
        /// Transport channel index.
        channel: u8,
        /// Diagnostics of the window.
        diagnostics: TransportChannelDiagnostics,
    },
}

/// Fixed-capacity effects from one step.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RedundancyEffects {
    items: [Option<RedundancyEffect>; EFFECT_CAPACITY],
    length: u8,
}

impl RedundancyEffects {
    pub(crate) const fn new() -> Self {
        Self {
            items: [None; EFFECT_CAPACITY],
            length: 0,
        }
    }

    /// Returns effect count.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.length as usize
    }

    /// Returns whether no effects were emitted.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.length == 0
    }

    /// Returns an effect by execution-order index.
    #[must_use]
    pub fn get(&self, index: usize) -> Option<&RedundancyEffect> {
        self.items
            .get(index)
            .filter(|_| index < self.len())
            .and_then(Option::as_ref)
    }

    /// Iterates over effects in execution order.
    pub fn iter(&self) -> impl Iterator<Item = &RedundancyEffect> {
        self.items[..self.len()].iter().filter_map(Option::as_ref)
    }

    fn push(&mut self, effect: RedundancyEffect) -> Result<(), RedundancyError> {
        let index = self.len();
        if index == EFFECT_CAPACITY {
            return Err(RedundancyError::EffectCapacityExceeded);
        }
        self.items[index] = Some(effect);
        self.length += 1;
        Ok(())
    }
}

/// Complete result of one redundancy step.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RedundancyTransition {
    /// Snapshot before the event.
    pub previous: RedundancySnapshot,
    /// Snapshot after the event.
    pub current: RedundancySnapshot,
    /// Ordered adapter effects.
    pub effects: RedundancyEffects,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct DeferredFrame {
    sequence_number: u32,
    payload: RedundancyPayload,
    received_at: u32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct DiagnosisEntry {
    sequence_number: u32,
    first_received_at: u32,
    received: [bool; MAX_CHANNELS],
}

impl DiagnosisEntry {
    const EMPTY: Self = Self {
        sequence_number: 0,
        first_received_at: 0,
        received: [false; MAX_CHANNELS],
    };
}

// Distinct frames of the current diagnosis window, oldest first.
#[derive(Clone, Debug, Eq, PartialEq)]
struct DiagnosisWindow {
    entries: [DiagnosisEntry; MAX_DIAGNOSIS_WINDOW as usize],
    len: u16,
    channels: [TransportChannelDiagnostics; MAX_CHANNELS],
}

impl DiagnosisWindow {
    const fn new() -> Self {
        Self {
            entries: [DiagnosisEntry::EMPTY; MAX_DIAGNOSIS_WINDOW as usize],
            len: 0,
            channels: [TransportChannelDiagnostics {
                frames: 0,
                missed: 0,
                drift: 0,
                drift_squared: 0,
            }; MAX_CHANNELS],
        }
    }

    fn reset(&mut self) {
        self.entries.fill(DiagnosisEntry::EMPTY);
        self.len = 0;
        self.channels = [TransportChannelDiagnostics::default(); MAX_CHANNELS];
    }

    fn copy_from(&mut self, source: &Self) {
        self.entries.copy_from_slice(&source.entries);
        self.len = source.len;
        self.channels = source.channels;
    }
}

/// Deterministic Redundancy Layer state machine.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RedundancyEngine {
    config: RedundancyConfig,
    snapshot: RedundancySnapshot,
    deferred: [Option<DeferredFrame>; MAX_DEFERRED],
    diagnosis: DiagnosisWindow,
}

impl RedundancyEngine {
    /// Creates a closed redundancy channel.
    #[must_use]
    pub const fn new(config: RedundancyConfig) -> Self {
        Self {
            config,
            snapshot: RedundancySnapshot {
                state: RedundancyState::Closed,
                next_transmit_sequence: 0,
                next_receive_sequence: 0,
                deferred_messages: 0,
                diagnostics: RedundancyDiagnostics {
                    duplicates: 0,
                    out_of_window: 0,
                    deferred: 0,
                    sequence_timeouts: 0,
                    defer_queue_full: 0,
                },
            },
            deferred: [None; MAX_DEFERRED],
            diagnosis: DiagnosisWindow::new(),
        }
    }

    /// Returns the replayable state.
    #[must_use]
    pub const fn snapshot(&self) -> RedundancySnapshot {
        self.snapshot
    }

    /// Processes one event atomically.
    ///
    /// # Errors
    ///
    /// Returns a typed error for invalid channel IDs, sends while closed, a
    /// full defer queue, or an internal effect-bound violation.
    pub fn step(
        &mut self,
        input: RedundancyInput,
    ) -> Result<RedundancyTransition, RedundancyError> {
        let mut candidate = self.clone();
        let previous = self.snapshot;
        let mut effects = RedundancyEffects::new();
        candidate.step_in_place(input, &mut effects)?;
        let transition = RedundancyTransition {
            previous,
            current: candidate.snapshot,
            effects,
        };
        *self = candidate;
        Ok(transition)
    }

    pub(crate) fn copy_from(&mut self, source: &Self) {
        self.config = source.config;
        self.snapshot = source.snapshot;
        self.deferred.copy_from_slice(&source.deferred);
        self.diagnosis.copy_from(&source.diagnosis);
    }

    // The composed stack already owns a transaction candidate.
    pub(crate) fn step_in_place(
        &mut self,
        input: RedundancyInput,
        effects: &mut RedundancyEffects,
    ) -> Result<(), RedundancyError> {
        effects.length = 0;
        self.process(input, effects)
    }

    fn process(
        &mut self,
        input: RedundancyInput,
        effects: &mut RedundancyEffects,
    ) -> Result<(), RedundancyError> {
        match input.event {
            RedundancyEvent::Open => self.open(effects),
            RedundancyEvent::Close => self.close(effects),
            RedundancyEvent::Send(payload) => self.send(payload, effects),
            RedundancyEvent::FrameReceived {
                channel,
                sequence_number,
                payload,
            } => self.receive(input.now, channel, sequence_number, payload, effects),
            RedundancyEvent::Poll => self.poll(input.now, effects),
        }
    }

    fn open(&mut self, effects: &mut RedundancyEffects) -> Result<(), RedundancyError> {
        if self.snapshot.state == RedundancyState::Up {
            return Ok(());
        }
        self.reset();
        self.snapshot.state = RedundancyState::Up;
        effects.push(RedundancyEffect::StateChanged {
            previous: RedundancyState::Closed,
            current: RedundancyState::Up,
        })
    }

    fn close(&mut self, effects: &mut RedundancyEffects) -> Result<(), RedundancyError> {
        if self.snapshot.state == RedundancyState::Closed {
            return Ok(());
        }
        self.reset();
        effects.push(RedundancyEffect::StateChanged {
            previous: RedundancyState::Up,
            current: RedundancyState::Closed,
        })
    }

    fn reset(&mut self) {
        self.snapshot = RedundancySnapshot {
            state: RedundancyState::Closed,
            next_transmit_sequence: 0,
            next_receive_sequence: 0,
            deferred_messages: 0,
            diagnostics: RedundancyDiagnostics::default(),
        };
        self.deferred.fill(None);
        self.diagnosis.reset();
    }

    fn send(
        &mut self,
        payload: RedundancyPayload,
        effects: &mut RedundancyEffects,
    ) -> Result<(), RedundancyError> {
        if self.snapshot.state != RedundancyState::Up {
            return Err(RedundancyError::ChannelClosed);
        }
        let sequence_number = self.snapshot.next_transmit_sequence;
        for channel in 0..self.config.transport_channels {
            effects.push(RedundancyEffect::Transmit {
                channel,
                sequence_number,
                payload,
            })?;
        }
        self.snapshot.next_transmit_sequence = sequence_number.wrapping_add(1);
        Ok(())
    }

    fn receive(
        &mut self,
        now: u32,
        channel: u8,
        sequence_number: u32,
        payload: RedundancyPayload,
        effects: &mut RedundancyEffects,
    ) -> Result<(), RedundancyError> {
        if channel >= self.config.transport_channels {
            return Err(RedundancyError::InvalidChannel(channel));
        }
        if self.snapshot.state != RedundancyState::Up {
            return Ok(());
        }
        let expected = self.snapshot.next_receive_sequence;
        // Start-up rule (OBS-SBB-RL-006): while both sequence counters are
        // zero, a frame with a non-zero sequence number is discarded. This
        // also applies when both counters wrap to zero together.
        if expected == 0 && self.snapshot.next_transmit_sequence == 0 && sequence_number != 0 {
            self.snapshot.diagnostics.out_of_window =
                self.snapshot.diagnostics.out_of_window.wrapping_add(1);
            return Ok(());
        }
        if sequence_number == expected {
            self.record_diagnosis(now, channel, sequence_number, effects)?;
            self.deliver(sequence_number, payload, effects)?;
            return self.drain_contiguous(effects);
        }
        if sequence_is_older(sequence_number, expected) {
            self.record_diagnosis(now, channel, sequence_number, effects)?;
            self.snapshot.diagnostics.duplicates =
                self.snapshot.diagnostics.duplicates.wrapping_add(1);
            return Ok(());
        }
        let maximum_delta = 10 * u32::from(self.config.defer_capacity);
        if sequence_number.wrapping_sub(expected) > maximum_delta {
            self.snapshot.diagnostics.out_of_window =
                self.snapshot.diagnostics.out_of_window.wrapping_add(1);
            return Ok(());
        }
        self.record_diagnosis(now, channel, sequence_number, effects)?;
        if self
            .deferred
            .iter()
            .flatten()
            .any(|entry| entry.sequence_number == sequence_number)
        {
            self.snapshot.diagnostics.duplicates =
                self.snapshot.diagnostics.duplicates.wrapping_add(1);
            return Ok(());
        }
        let Some(slot) = self.deferred[..usize::from(self.config.defer_capacity)]
            .iter_mut()
            .find(|entry| entry.is_none())
        else {
            self.snapshot.diagnostics.defer_queue_full =
                self.snapshot.diagnostics.defer_queue_full.wrapping_add(1);
            return Ok(());
        };
        *slot = Some(DeferredFrame {
            sequence_number,
            payload,
            received_at: now,
        });
        self.snapshot.deferred_messages += 1;
        self.snapshot.diagnostics.deferred = self.snapshot.diagnostics.deferred.wrapping_add(1);
        Ok(())
    }

    fn poll(&mut self, now: u32, effects: &mut RedundancyEffects) -> Result<(), RedundancyError> {
        if self.snapshot.state != RedundancyState::Up {
            return Ok(());
        }
        let timed_out = self
            .deferred
            .iter()
            .flatten()
            .any(|entry| now.wrapping_sub(entry.received_at) > self.config.t_seq);
        if !timed_out {
            return Ok(());
        }
        let Some(oldest) = self.oldest_deferred_sequence() else {
            return Ok(());
        };
        self.snapshot.next_receive_sequence = oldest;
        self.snapshot.diagnostics.sequence_timeouts =
            self.snapshot.diagnostics.sequence_timeouts.wrapping_add(1);
        self.drain_contiguous(effects)
    }

    // Records a frame's arrival for the per-channel diagnosis, reporting and
    // resetting a full window first (OBS-SBB-DIAG-003).
    fn record_diagnosis(
        &mut self,
        now: u32,
        channel: u8,
        sequence_number: u32,
        effects: &mut RedundancyEffects,
    ) -> Result<(), RedundancyError> {
        let window = &mut self.diagnosis;
        let index = usize::from(channel);
        let len = usize::from(window.len);
        if let Some(entry) = window.entries[..len]
            .iter_mut()
            .rev()
            .find(|entry| entry.sequence_number == sequence_number)
        {
            let delay = now.wrapping_sub(entry.first_received_at);
            if delay <= self.config.t_seq {
                let diagnostics = &mut window.channels[index];
                diagnostics.drift = diagnostics.drift.wrapping_add(delay);
                diagnostics.drift_squared = diagnostics
                    .drift_squared
                    .wrapping_add(delay.wrapping_mul(delay));
                entry.received[index] = true;
            }
            return Ok(());
        }
        if window.len >= self.config.diagnosis_window {
            for reported in 0..self.config.transport_channels {
                let missed = window.entries[..len]
                    .iter()
                    .filter(|entry| !entry.received[usize::from(reported)])
                    .count();
                let diagnostics = TransportChannelDiagnostics {
                    frames: u32::from(window.len),
                    missed: u32::try_from(missed).unwrap_or(u32::MAX),
                    ..window.channels[usize::from(reported)]
                };
                effects.push(RedundancyEffect::Diagnostics {
                    channel: reported,
                    diagnostics,
                })?;
            }
            window.reset();
        }
        let mut received = [false; MAX_CHANNELS];
        received[index] = true;
        window.entries[usize::from(window.len)] = DiagnosisEntry {
            sequence_number,
            first_received_at: now,
            received,
        };
        window.len += 1;
        Ok(())
    }

    fn deliver(
        &mut self,
        sequence_number: u32,
        payload: RedundancyPayload,
        effects: &mut RedundancyEffects,
    ) -> Result<(), RedundancyError> {
        effects.push(RedundancyEffect::Deliver(payload))?;
        self.snapshot.next_receive_sequence = sequence_number.wrapping_add(1);
        Ok(())
    }

    fn drain_contiguous(&mut self, effects: &mut RedundancyEffects) -> Result<(), RedundancyError> {
        loop {
            let expected = self.snapshot.next_receive_sequence;
            let Some(frame) = self
                .deferred
                .iter_mut()
                .find_map(|entry| entry.take_if(|frame| frame.sequence_number == expected))
            else {
                return Ok(());
            };
            self.snapshot.deferred_messages -= 1;
            self.deliver(frame.sequence_number, frame.payload, effects)?;
        }
    }

    fn oldest_deferred_sequence(&self) -> Option<u32> {
        self.deferred
            .iter()
            .flatten()
            .map(|entry| entry.sequence_number)
            .reduce(|oldest, candidate| {
                if sequence_is_older(candidate, oldest) {
                    candidate
                } else {
                    oldest
                }
            })
    }
}

/// Redundancy Layer configuration or transition error.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RedundancyError {
    /// `T_seq` is outside `50..=500`.
    InvalidTSeq(u32),
    /// Defer capacity is outside `4..=10`.
    InvalidDeferCapacity(u8),
    /// Transport-channel count is outside `1..=2`.
    InvalidChannelCount(u8),
    /// Diagnosis window is outside `1..=MAX_DIAGNOSIS_WINDOW`.
    InvalidDiagnosisWindow(u16),
    /// Wrapped SR PDU width is outside `28..=1101`.
    InvalidPayloadLength(usize),
    /// Send was requested while the redundancy channel was closed.
    ChannelClosed,
    /// Received channel index is not configured.
    InvalidChannel(u8),
    /// Internal fixed effect bound was insufficient.
    EffectCapacityExceeded,
}

impl fmt::Display for RedundancyError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidTSeq(value) => write!(formatter, "T_seq {value} is outside 50..=500"),
            Self::InvalidDeferCapacity(value) => {
                write!(formatter, "defer capacity {value} is outside 4..=10")
            }
            Self::InvalidChannelCount(value) => {
                write!(formatter, "channel count {value} is outside 1..=2")
            }
            Self::InvalidDiagnosisWindow(value) => {
                write!(
                    formatter,
                    "diagnosis window {value} is outside 1..={MAX_DIAGNOSIS_WINDOW}"
                )
            }
            Self::InvalidPayloadLength(value) => {
                write!(
                    formatter,
                    "wrapped SR PDU length {value} is outside 28..=1101"
                )
            }
            Self::ChannelClosed => formatter.write_str("redundancy channel is closed"),
            Self::InvalidChannel(value) => {
                write!(formatter, "transport channel {value} is invalid")
            }
            Self::EffectCapacityExceeded => {
                formatter.write_str("redundancy transition exceeded its effect bound")
            }
        }
    }
}

impl core::error::Error for RedundancyError {}

const fn sequence_is_older(candidate: u32, reference: u32) -> bool {
    reference.wrapping_sub(candidate.wrapping_add(1)) < u32::MAX / 2
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn receive_and_transmit_sequences_wrap_with_deferred_frames_and_duplicates() {
        let mut engine = RedundancyEngine::new(RedundancyConfig::new(50, 4, 2, 200).unwrap());
        let payload = RedundancyPayload::new(&[1; 28]).unwrap();
        engine
            .step(RedundancyInput {
                now: 0,
                event: RedundancyEvent::Open,
            })
            .unwrap();
        // Reach the rollover boundary without executing 2^32 events.
        engine.snapshot.next_receive_sequence = u32::MAX - 1;
        engine.snapshot.next_transmit_sequence = u32::MAX;
        let send = engine
            .step(RedundancyInput {
                now: 1,
                event: RedundancyEvent::Send(payload),
            })
            .unwrap();
        assert_eq!(send.current.next_transmit_sequence, 0);
        assert!(send.effects.iter().all(|effect| matches!(
            effect,
            RedundancyEffect::Transmit {
                sequence_number: u32::MAX,
                ..
            }
        )));
        let frame = |sequence_number| RedundancyInput {
            now: 2,
            event: RedundancyEvent::FrameReceived {
                channel: 0,
                sequence_number,
                payload,
            },
        };
        assert!(engine.step(frame(0)).unwrap().effects.is_empty());
        assert!(engine.step(frame(u32::MAX)).unwrap().effects.is_empty());
        let drained = engine.step(frame(u32::MAX - 1)).unwrap();
        assert_eq!(drained.effects.len(), 3);
        assert_eq!(drained.current.next_receive_sequence, 1);
        for sequence in [u32::MAX - 1, u32::MAX, 0] {
            assert!(engine.step(frame(sequence)).unwrap().effects.is_empty());
        }
        assert_eq!(engine.snapshot.diagnostics.duplicates, 3);

        // Both counters at zero after a wrap count as start-up
        // (OBS-SBB-RL-006): a frame other than 0 is discarded until this side
        // transmits.
        engine.snapshot.next_receive_sequence = 0;
        let discarded = engine.step(frame(1)).unwrap();
        assert!(discarded.effects.is_empty());
        assert_eq!(discarded.current.deferred_messages, 0);
        assert_eq!(discarded.current.diagnostics.out_of_window, 1);
        let send = engine
            .step(RedundancyInput {
                now: 2,
                event: RedundancyEvent::Send(payload),
            })
            .unwrap();
        assert!(send.effects.iter().all(|effect| matches!(
            effect,
            RedundancyEffect::Transmit {
                sequence_number: 0,
                ..
            }
        )));
        // After transmitting, the same frame is deferred and reached by T_seq.
        assert!(engine.step(frame(1)).unwrap().effects.is_empty());
        assert_eq!(engine.snapshot.deferred_messages, 1);
        let expired = engine
            .step(RedundancyInput {
                now: 53,
                event: RedundancyEvent::Poll,
            })
            .unwrap();
        assert_eq!(expired.effects.len(), 1);
        assert_eq!(expired.current.next_receive_sequence, 2);
    }
}
