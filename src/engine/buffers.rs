//! Allocation-free per-connection send and receive buffers.
//!
//! The shapes and limits in this module are traced to the labelled
//! observations in `docs/requirements/observations.md`.

use core::fmt;

use crate::codec::sr::{
    CodecError, DataFieldMessages, APPLICATION_LENGTH_PREFIX, MAX_APPLICATION_PAYLOAD,
    MAX_DATA_FIELD_LEN,
};

use super::connection::{ConnectionConfig, OutboundMessage};

/// Send-buffer capacity in messages.
pub const SEND_BUFFER_CAPACITY: usize = 20;
const SEND_BUFFER_STORAGE_CAPACITY: usize = SEND_BUFFER_CAPACITY + 3;
const SEND_BUFFER_STORAGE_CAPACITY_U16: u16 = 23;
const SEND_BUFFER_CAPACITY_U16: u16 = 20;
const SEND_BUFFER_CAPACITY_U32: u32 = 20;

/// Owned application data suitable for deterministic, allocation-free queues.
#[derive(Clone, Copy, Eq, PartialEq)]
pub struct ApplicationPayload {
    bytes: [u8; MAX_APPLICATION_PAYLOAD],
    length: u16,
}

impl ApplicationPayload {
    const EMPTY: Self = Self {
        bytes: [0; MAX_APPLICATION_PAYLOAD],
        length: 0,
    };

    /// Copies one non-empty application payload into fixed-capacity storage.
    ///
    /// # Errors
    ///
    /// Returns an error if `bytes` is empty or exceeds
    /// [`MAX_APPLICATION_PAYLOAD`] bytes.
    pub fn new(bytes: &[u8]) -> Result<Self, PayloadError> {
        if bytes.is_empty() {
            return Err(PayloadError::Empty);
        }
        if bytes.len() > MAX_APPLICATION_PAYLOAD {
            return Err(PayloadError::TooLarge {
                actual: bytes.len(),
                maximum: MAX_APPLICATION_PAYLOAD,
            });
        }

        let length = u16::try_from(bytes.len()).map_err(|_| PayloadError::TooLarge {
            actual: bytes.len(),
            maximum: MAX_APPLICATION_PAYLOAD,
        })?;
        let mut payload = Self::EMPTY;
        payload.bytes[..bytes.len()].copy_from_slice(bytes);
        payload.length = length;
        Ok(payload)
    }

    /// Returns the payload bytes.
    #[must_use]
    pub fn as_slice(&self) -> &[u8] {
        &self.bytes[..self.len()]
    }

    /// Returns the payload length in bytes.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.length as usize
    }

    /// Returns whether the payload is empty.
    ///
    /// Valid constructed payloads are never empty; this is provided for the
    /// usual collection-style API and internal empty slots.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.length == 0
    }
}

impl fmt::Debug for ApplicationPayload {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ApplicationPayload")
            .field("length", &self.length)
            .field("bytes", &self.as_slice())
            .finish()
    }
}

/// The application-data field of one Data message: one or more application
/// messages, each preceded by its little-endian `u16` length, in
/// fixed-capacity storage of [`MAX_DATA_FIELD_LEN`] bytes.
///
/// Several messages in one Data message (packetization) are limited by the
/// connection's `N_maxPacket` (`ConnectionConfig::max_packet`).
#[derive(Clone, Copy, Eq, PartialEq)]
pub struct ApplicationData {
    bytes: [u8; MAX_DATA_FIELD_LEN],
    length: u16,
}

impl ApplicationData {
    const EMPTY: Self = Self {
        bytes: [0; MAX_DATA_FIELD_LEN],
        length: 0,
    };

    /// Creates an empty field; add messages with [`Self::push`].
    #[must_use]
    pub const fn new() -> Self {
        Self::EMPTY
    }

    /// Creates a field holding exactly one application message.
    #[must_use]
    pub fn single(payload: &ApplicationPayload) -> Self {
        let mut data = Self::EMPTY;
        let length = payload.len();
        data.bytes[..APPLICATION_LENGTH_PREFIX].copy_from_slice(&payload.length.to_le_bytes());
        data.bytes[APPLICATION_LENGTH_PREFIX..APPLICATION_LENGTH_PREFIX + length]
            .copy_from_slice(payload.as_slice());
        // At most MAX_DATA_FIELD_LEN, which fits u16.
        data.length = payload.length + 2;
        data
    }

    /// Appends one application message.
    ///
    /// # Errors
    ///
    /// Returns an error if `message` is empty, exceeds
    /// [`MAX_APPLICATION_PAYLOAD`] bytes, or does not fit the remaining field.
    pub fn push(&mut self, message: &[u8]) -> Result<(), PayloadError> {
        let payload = ApplicationPayload::new(message)?;
        let start = usize::from(self.length);
        let needed = APPLICATION_LENGTH_PREFIX + message.len();
        let available = MAX_DATA_FIELD_LEN - start;
        if needed > available {
            return Err(PayloadError::DataFieldFull { needed, available });
        }
        self.bytes[start..start + APPLICATION_LENGTH_PREFIX]
            .copy_from_slice(&payload.length.to_le_bytes());
        self.bytes[start + APPLICATION_LENGTH_PREFIX..start + needed].copy_from_slice(message);
        self.length += payload.length + 2;
        Ok(())
    }

    /// Copies a received application-data field without checking its
    /// structure; check it with [`Self::message_count`].
    ///
    /// # Errors
    ///
    /// Returns an error if `field` exceeds [`MAX_DATA_FIELD_LEN`] bytes.
    pub fn from_field(field: &[u8]) -> Result<Self, PayloadError> {
        if field.len() > MAX_DATA_FIELD_LEN {
            return Err(PayloadError::TooLarge {
                actual: field.len(),
                maximum: MAX_DATA_FIELD_LEN,
            });
        }
        let mut data = Self::EMPTY;
        data.bytes[..field.len()].copy_from_slice(field);
        data.length = u16::try_from(field.len()).map_err(|_| PayloadError::TooLarge {
            actual: field.len(),
            maximum: MAX_DATA_FIELD_LEN,
        })?;
        Ok(data)
    }

    /// Returns the field as it appears on the wire.
    #[must_use]
    pub fn as_field(&self) -> &[u8] {
        &self.bytes[..usize::from(self.length)]
    }

    /// Returns the application messages in order.
    #[must_use]
    pub fn messages(&self) -> DataFieldMessages<'_> {
        DataFieldMessages::new(self.as_field())
    }

    /// Checks the field's structure and returns its number of messages.
    ///
    /// # Errors
    ///
    /// Returns an error for an empty field, an empty message, or a length
    /// prefix that runs past the field.
    pub fn message_count(&self) -> Result<usize, CodecError> {
        let mut count = 0;
        for message in self.messages() {
            message?;
            count += 1;
        }
        if count == 0 {
            return Err(CodecError::EmptyDataField);
        }
        Ok(count)
    }

    /// Returns the field length in bytes.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.length as usize
    }

    /// Returns whether the field holds no message.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.length == 0
    }
}

impl Default for ApplicationData {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Debug for ApplicationData {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ApplicationData")
            .field("length", &self.length)
            .field("bytes", &self.as_field())
            .finish()
    }
}

/// Application-payload construction errors.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PayloadError {
    /// A payload needs at least one byte.
    Empty,
    /// An application message does not fit the remaining application-data
    /// field.
    DataFieldFull {
        /// Bytes the message needs, including its length prefix.
        needed: usize,
        /// Bytes left in the field.
        available: usize,
    },
    /// The payload exceeds [`MAX_APPLICATION_PAYLOAD`] bytes.
    TooLarge {
        /// Actual payload length.
        actual: usize,
        /// Maximum accepted payload length.
        maximum: usize,
    },
}

impl fmt::Display for PayloadError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => formatter.write_str("application payload must not be empty"),
            Self::DataFieldFull { needed, available } => write!(
                formatter,
                "application message needs {needed} bytes, {available} left in the data field"
            ),
            Self::TooLarge { actual, maximum } => {
                write!(
                    formatter,
                    "application payload length {actual} exceeds {maximum}"
                )
            }
        }
    }
}

impl core::error::Error for PayloadError {}

/// Current occupancy of one fixed-capacity buffer.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BufferUsage {
    /// Number of occupied entries.
    pub used: u16,
    /// Number of available entries.
    pub free: u16,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct SendBuffer {
    entries: [Option<OutboundMessage>; SEND_BUFFER_STORAGE_CAPACITY],
    remove_index: u16,
    read_index: u16,
    write_index: u16,
    used: u16,
    not_sent: u16,
}

#[allow(clippy::large_types_passed_by_value)]
impl SendBuffer {
    #[allow(clippy::large_stack_arrays)]
    const fn new() -> Self {
        Self {
            entries: [None; SEND_BUFFER_STORAGE_CAPACITY],
            remove_index: 0,
            read_index: 0,
            write_index: 0,
            used: 0,
            not_sent: 0,
        }
    }

    fn reset(&mut self) {
        *self = Self::new();
    }

    const fn usage(&self) -> BufferUsage {
        BufferUsage {
            used: self.used,
            free: SEND_BUFFER_CAPACITY_U16.saturating_sub(self.used),
        }
    }

    const fn not_sent(&self) -> u16 {
        self.not_sent
    }

    const fn unconfirmed(&self) -> u16 {
        self.used - self.not_sent
    }

    fn acknowledged_count(&self, confirmed_sequence_number: u32) -> u16 {
        let sent = self.used - self.not_sent;
        let mut count = 0;
        while count < sent {
            let index = advance(self.remove_index, count);
            let Some(message) = self.entries[usize::from(index)] else {
                break;
            };
            let sequence = message.header().sequence_number;
            if confirmed_sequence_number.wrapping_sub(sequence) >= SEND_BUFFER_CAPACITY_U32 {
                break;
            }
            count += 1;
            if sequence == confirmed_sequence_number {
                break;
            }
        }
        count
    }

    fn acknowledge_through(&mut self, confirmed_sequence_number: u32) {
        let count = self.acknowledged_count(confirmed_sequence_number);
        for _ in 0..count {
            self.entries[usize::from(self.remove_index)] = None;
            self.remove_index = advance(self.remove_index, 1);
            self.used -= 1;
        }
    }

    fn enqueue(&mut self, message: OutboundMessage) {
        debug_assert!(usize::from(self.used) < SEND_BUFFER_STORAGE_CAPACITY);
        self.entries[usize::from(self.write_index)] = Some(message);
        self.write_index = advance(self.write_index, 1);
        self.used += 1;
        self.not_sent += 1;
    }

    fn next_to_transmit(
        &mut self,
        remote_n_send_max: u16,
        now: u32,
        confirmed_sequence_number: u32,
    ) -> Option<OutboundMessage> {
        if self.not_sent == 0 || self.unconfirmed() >= remote_n_send_max {
            return None;
        }
        let index = usize::from(self.read_index);
        let message = self.entries[index]?.prepare_for_transmit(now, confirmed_sequence_number);
        self.entries[index] = Some(message);
        self.read_index = advance(self.read_index, 1);
        self.not_sent -= 1;
        Some(message)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ReceiveBuffer {
    entries: [ApplicationData; SEND_BUFFER_CAPACITY],
    /// Bytes of the oldest entry already handed out.
    head_offset: u16,
    read_index: u16,
    write_index: u16,
    used: u16,
    capacity: u16,
}

#[allow(clippy::large_types_passed_by_value)]
impl ReceiveBuffer {
    #[allow(clippy::large_stack_arrays)]
    const fn new(capacity: u16) -> Self {
        Self {
            entries: [ApplicationData::EMPTY; SEND_BUFFER_CAPACITY],
            head_offset: 0,
            read_index: 0,
            write_index: 0,
            used: 0,
            capacity,
        }
    }

    fn reset(&mut self) {
        let capacity = self.capacity;
        *self = Self::new(capacity);
    }

    const fn usage(&self) -> BufferUsage {
        BufferUsage {
            used: self.used,
            free: self.capacity - self.used,
        }
    }

    fn push(&mut self, data: ApplicationData) -> Result<(), BufferError> {
        if self.used == self.capacity {
            return Err(BufferError::ReceiveBufferFull);
        }
        self.entries[usize::from(self.write_index)] = data;
        self.write_index = advance_with_capacity(self.write_index, self.capacity);
        self.used += 1;
        Ok(())
    }

    /// Hands out the next application message of the oldest entry and
    /// releases the entry once all its messages are out.
    fn pop(&mut self) -> Option<ApplicationPayload> {
        while self.used > 0 {
            let head = &self.entries[usize::from(self.read_index)];
            let rest = &head.as_field()[usize::from(self.head_offset)..];
            // Entries were checked by the engine before they were pushed.
            let next = DataFieldMessages::new(rest)
                .next()
                .and_then(Result::ok)
                .and_then(|message| ApplicationPayload::new(message).ok());
            if let Some(payload) = next {
                self.head_offset += payload.length + 2;
                if usize::from(self.head_offset) >= head.len() {
                    self.release_head();
                }
                return Some(payload);
            }
            self.release_head();
        }
        None
    }

    fn release_head(&mut self) {
        self.entries[usize::from(self.read_index)] = ApplicationData::EMPTY;
        self.read_index = advance_with_capacity(self.read_index, self.capacity);
        self.head_offset = 0;
        self.used -= 1;
    }
}

/// Per-connection buffer errors.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BufferError {
    /// No physical send-buffer entry remains.
    SendBufferFull,
    /// The configured local receive window is full.
    ReceiveBufferFull,
    /// The sequence requested for retransmission is no longer buffered.
    RetransmissionUnavailable(u32),
    /// The buffers were created for a different local receive-window limit.
    ConfigurationMismatch {
        /// Limit required by the connection engine.
        expected: u16,
        /// Limit used to construct the supplied buffers.
        actual: u16,
    },
}

impl fmt::Display for BufferError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::SendBufferFull => formatter.write_str("send buffer is full"),
            Self::ReceiveBufferFull => formatter.write_str("receive buffer is full"),
            Self::RetransmissionUnavailable(sequence) => {
                write!(
                    formatter,
                    "sequence {sequence} is unavailable for retransmission"
                )
            }
            Self::ConfigurationMismatch { expected, actual } => write!(
                formatter,
                "buffer N_SENDMAX {actual} does not match connection N_SENDMAX {expected}"
            ),
        }
    }
}

impl core::error::Error for BufferError {}

/// Caller-owned allocation-free buffers for one connection engine.
///
/// Keeping this value separate from [`super::connection::ConnectionEngine`]
/// makes the memory cost explicit and lets an integration choose static,
/// stack, arena, or other caller-controlled placement without allocation in
/// the protocol crate.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ConnectionBuffers {
    send: SendBuffer,
    receive: ReceiveBuffer,
}

#[allow(clippy::large_types_passed_by_value)]
impl ConnectionBuffers {
    /// Creates empty buffers sized for a validated connection configuration.
    #[must_use]
    pub const fn new(config: ConnectionConfig) -> Self {
        Self {
            send: SendBuffer::new(),
            receive: ReceiveBuffer::new(config.n_send_max()),
        }
    }

    pub(crate) fn copy_from(&mut self, source: &Self) {
        self.send.entries.copy_from_slice(&source.send.entries);
        self.send.remove_index = source.send.remove_index;
        self.send.read_index = source.send.read_index;
        self.send.write_index = source.send.write_index;
        self.send.used = source.send.used;
        self.send.not_sent = source.send.not_sent;
        self.receive
            .entries
            .copy_from_slice(&source.receive.entries);
        self.receive.head_offset = source.receive.head_offset;
        self.receive.read_index = source.receive.read_index;
        self.receive.write_index = source.receive.write_index;
        self.receive.used = source.receive.used;
        self.receive.capacity = source.receive.capacity;
    }

    /// Returns send-buffer occupancy, including sent and unsent messages.
    #[must_use]
    pub const fn send_usage(&self) -> BufferUsage {
        self.send.usage()
    }

    /// Returns receive-buffer occupancy.
    #[must_use]
    pub const fn receive_usage(&self) -> BufferUsage {
        self.receive.usage()
    }

    /// Returns the number of queued messages not yet emitted for transmission.
    #[must_use]
    pub const fn unsent_messages(&self) -> u16 {
        self.send.not_sent()
    }

    /// Returns the number of sent messages not yet confirmed by the peer.
    #[must_use]
    pub const fn unconfirmed_messages(&self) -> u16 {
        self.send.unconfirmed()
    }

    /// Removes and returns the oldest received application message, if one
    /// is queued. Messages packed into one Data message come out one by one.
    pub fn pop_received(&mut self) -> Option<ApplicationPayload> {
        self.receive.pop()
    }

    pub(super) fn reset(&mut self) {
        self.send.reset();
        self.receive.reset();
    }

    pub(super) const fn local_n_send_max(&self) -> u16 {
        self.receive.capacity
    }

    pub(super) fn can_enqueue_after_ack(&self, confirmation: Option<u32>) -> bool {
        let acknowledged =
            confirmation.map_or(0, |sequence| self.send.acknowledged_count(sequence));
        usize::from(self.send.used - acknowledged) < SEND_BUFFER_CAPACITY
    }

    pub(super) fn acknowledge_through(&mut self, confirmed_sequence_number: u32) {
        self.send.acknowledge_through(confirmed_sequence_number);
    }

    pub(super) fn enqueue_send(&mut self, message: OutboundMessage) -> Result<(), BufferError> {
        self.send.try_enqueue(message)
    }

    pub(super) fn next_to_transmit(
        &mut self,
        remote_n_send_max: u16,
        now: u32,
        confirmed_sequence_number: u32,
    ) -> Option<OutboundMessage> {
        self.send
            .next_to_transmit(remote_n_send_max, now, confirmed_sequence_number)
    }

    pub(super) const fn can_push_received(&self) -> bool {
        self.receive.used < self.receive.capacity
    }

    pub(super) fn push_received(&mut self, data: ApplicationData) -> Result<(), BufferError> {
        self.receive.push(data)
    }

    pub(super) fn rebuild_for_retransmission(
        &mut self,
        requested_sequence: u32,
        response: OutboundMessage,
        next_sequence: &mut u32,
    ) -> Result<(), BufferError> {
        let sent = self.send.used - self.send.not_sent;
        let requested_offset = (0..self.send.used).find(|offset| {
            let index = advance(self.send.remove_index, *offset);
            self.send.entries[usize::from(index)]
                .is_some_and(|message| message.header().sequence_number == requested_sequence)
        });
        let Some(requested_offset) = requested_offset else {
            return Err(BufferError::RetransmissionUnavailable(requested_sequence));
        };

        let mut rebuilt = SendBuffer::new();
        rebuilt.try_enqueue(response)?;
        let mut retransmission_open = true;
        for offset in requested_offset..self.send.used {
            let index = advance(self.send.remove_index, offset);
            let Some(message) = self.send.entries[usize::from(index)] else {
                continue;
            };
            let was_sent = offset < sent;
            match (was_sent, message) {
                (
                    true,
                    OutboundMessage::Data { header, data, .. }
                    | OutboundMessage::RetransmittedData { header, data, .. },
                ) => rebuilt.try_enqueue(OutboundMessage::RetransmittedData {
                    header: resequence(header, next_sequence),
                    data,
                })?,
                (false, OutboundMessage::RetransmittedData { header, data, .. })
                    if retransmission_open =>
                {
                    rebuilt.try_enqueue(OutboundMessage::RetransmittedData {
                        header: resequence(header, next_sequence),
                        data,
                    })?;
                }
                (false, OutboundMessage::Data { header, data, .. }) => {
                    retransmission_open = false;
                    rebuilt.try_enqueue(OutboundMessage::Data {
                        header: resequence(header, next_sequence),
                        data,
                    })?;
                }
                // An unsent RetrReq survives the rebuild; it ends an open
                // replay stream with a Heartbeat first.
                (false, OutboundMessage::RetransmissionRequest { header }) => {
                    if retransmission_open {
                        rebuilt.try_enqueue(OutboundMessage::Heartbeat {
                            header: resequence(header, next_sequence),
                        })?;
                        retransmission_open = false;
                    }
                    rebuilt.try_enqueue(OutboundMessage::RetransmissionRequest {
                        header: resequence(header, next_sequence),
                    })?;
                }
                _ => {}
            }
        }

        if retransmission_open {
            let mut header = response.header();
            header = resequence(header, next_sequence);
            rebuilt.try_enqueue(OutboundMessage::Heartbeat { header })?;
        }
        self.send = rebuilt;
        Ok(())
    }

    pub(super) fn retransmission_available(&self, sequence: u32) -> bool {
        (0..self.send.used).any(|offset| {
            let index = advance(self.send.remove_index, offset);
            self.send.entries[usize::from(index)]
                .is_some_and(|message| message.header().sequence_number == sequence)
        })
    }
}

impl SendBuffer {
    #[allow(clippy::large_types_passed_by_value)]
    fn try_enqueue(&mut self, message: OutboundMessage) -> Result<(), BufferError> {
        if usize::from(self.used) == SEND_BUFFER_STORAGE_CAPACITY {
            return Err(BufferError::SendBufferFull);
        }
        self.enqueue(message);
        Ok(())
    }
}

fn resequence(
    mut header: crate::codec::sr::SrHeader,
    next_sequence: &mut u32,
) -> crate::codec::sr::SrHeader {
    header.sequence_number = *next_sequence;
    *next_sequence = next_sequence.wrapping_add(1);
    header
}

const fn advance(index: u16, increment: u16) -> u16 {
    (index + increment) % SEND_BUFFER_STORAGE_CAPACITY_U16
}

const fn advance_with_capacity(index: u16, capacity: u16) -> u16 {
    (index + 1) % capacity
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        codec::sr::{ProtocolVersion, SrHeader},
        engine::diagnostics::DiagnosticsConfig,
    };

    fn config(n_send_max: u16) -> ConnectionConfig {
        ConnectionConfig::new(
            1,
            2,
            ProtocolVersion::new(*b"0303").unwrap(),
            n_send_max,
            100,
            DiagnosticsConfig::new(5_000, [20, 40, 60, 80]).unwrap(),
        )
        .unwrap()
    }

    fn heartbeat(sequence_number: u32) -> OutboundMessage {
        OutboundMessage::Heartbeat {
            header: SrHeader {
                sequence_number,
                ..SrHeader::default()
            },
        }
    }

    #[test]
    fn receive_buffer_is_fifo_and_wraps_at_configured_capacity() {
        let mut buffers = ConnectionBuffers::new(config(2));
        let first = ApplicationPayload::new(&[1]).unwrap();
        let second = ApplicationPayload::new(&[2, 3]).unwrap();
        let third = ApplicationPayload::new(&[4]).unwrap();

        buffers
            .push_received(ApplicationData::single(&first))
            .unwrap();
        buffers
            .push_received(ApplicationData::single(&second))
            .unwrap();
        assert_eq!(
            buffers.push_received(ApplicationData::single(&third)),
            Err(BufferError::ReceiveBufferFull)
        );
        assert_eq!(buffers.pop_received(), Some(first));
        buffers
            .push_received(ApplicationData::single(&third))
            .unwrap();
        assert_eq!(buffers.pop_received(), Some(second));
        assert_eq!(buffers.pop_received(), Some(third));
        assert_eq!(buffers.pop_received(), None);
    }

    #[test]
    fn packed_messages_come_out_one_by_one_and_free_their_entry_last() {
        let mut buffers = ConnectionBuffers::new(config(2));
        let mut packed = ApplicationData::new();
        for message in [&[1_u8][..], &[2, 3], &[4, 5, 6]] {
            packed.push(message).unwrap();
        }
        assert_eq!(packed.message_count(), Ok(3));
        buffers.push_received(packed).unwrap();
        buffers
            .push_received(ApplicationData::single(
                &ApplicationPayload::new(&[7]).unwrap(),
            ))
            .unwrap();

        let payload = |bytes: &[u8]| Some(ApplicationPayload::new(bytes).unwrap());
        assert_eq!(buffers.pop_received(), payload(&[1]));
        assert_eq!(buffers.receive_usage().used, 2, "the packed entry stays");
        assert_eq!(buffers.pop_received(), payload(&[2, 3]));
        assert_eq!(buffers.pop_received(), payload(&[4, 5, 6]));
        assert_eq!(
            buffers.receive_usage().used,
            1,
            "released after its last message"
        );
        assert_eq!(buffers.pop_received(), payload(&[7]));
        assert_eq!(buffers.pop_received(), None);
        assert_eq!(buffers.receive_usage().used, 0);
    }

    #[test]
    fn application_data_is_built_and_checked() {
        let mut data = ApplicationData::new();
        assert_eq!(data.message_count(), Err(CodecError::EmptyDataField));
        assert_eq!(data.push(&[]), Err(PayloadError::Empty));
        data.push(&[0xaa; MAX_APPLICATION_PAYLOAD - 2]).unwrap();
        // Two bytes are left: not enough for a prefix and one byte.
        assert_eq!(
            data.push(&[1]),
            Err(PayloadError::DataFieldFull {
                needed: 3,
                available: 2
            })
        );
        assert_eq!(data.len(), MAX_DATA_FIELD_LEN - 2);

        let single = ApplicationData::single(
            &ApplicationPayload::new(&[9; MAX_APPLICATION_PAYLOAD]).unwrap(),
        );
        assert_eq!(single.len(), MAX_DATA_FIELD_LEN);
        assert_eq!(single.message_count(), Ok(1));

        // Received fields are copied as they are and checked separately.
        let field = |bytes: &[u8]| ApplicationData::from_field(bytes).unwrap().message_count();
        assert_eq!(field(&[2, 0, 1, 2, 1, 0, 3]), Ok(2));
        assert_eq!(field(&[0, 0]), Err(CodecError::EmptyApplicationMessage));
        assert_eq!(
            field(&[1, 0, 1, 5]),
            Err(CodecError::DataFieldTruncated { remaining: 1 })
        );
        assert_eq!(
            field(&[4, 0, 1, 2]),
            Err(CodecError::PayloadLengthMismatch {
                declared: 4,
                actual: 2
            })
        );
        assert!(ApplicationData::from_field(&[0; MAX_DATA_FIELD_LEN + 1]).is_err());
    }

    #[test]
    fn send_buffer_tracks_flow_control_and_wrapping_confirmations() {
        let mut buffers = ConnectionBuffers::new(config(2));
        buffers.enqueue_send(heartbeat(u32::MAX)).unwrap();
        buffers.enqueue_send(heartbeat(0)).unwrap();
        buffers.enqueue_send(heartbeat(1)).unwrap();

        assert_eq!(
            buffers.next_to_transmit(2, 10, 7),
            Some(OutboundMessage::Heartbeat {
                header: SrHeader {
                    sequence_number: u32::MAX,
                    timestamp: 10,
                    confirmed_sequence_number: 7,
                    ..SrHeader::default()
                }
            })
        );
        assert_eq!(
            buffers
                .next_to_transmit(2, 11, 8)
                .unwrap()
                .header()
                .sequence_number,
            0
        );
        assert_eq!(buffers.next_to_transmit(2, 12, 9), None);
        assert_eq!(buffers.unconfirmed_messages(), 2);
        assert_eq!(buffers.unsent_messages(), 1);

        buffers.acknowledge_through(0);
        assert_eq!(buffers.unconfirmed_messages(), 0);
        assert_eq!(
            buffers
                .next_to_transmit(2, 13, 10)
                .unwrap()
                .header()
                .sequence_number,
            1
        );
        assert_eq!(buffers.send_usage().used, 1);
    }

    #[test]
    fn physical_send_capacity_rejects_without_overwriting_retained_messages() {
        let mut buffers = ConnectionBuffers::new(config(20));
        for sequence in 0..23 {
            buffers.enqueue_send(heartbeat(sequence)).unwrap();
        }
        let before = buffers.clone();
        assert_eq!(
            buffers.enqueue_send(heartbeat(23)),
            Err(BufferError::SendBufferFull)
        );
        assert_eq!(buffers, before);
    }

    fn header(sequence_number: u32) -> SrHeader {
        SrHeader {
            sequence_number,
            ..SrHeader::default()
        }
    }

    fn rebuilt_kinds(buffers: &mut ConnectionBuffers) -> ([&'static str; 8], usize) {
        let mut kinds = [""; 8];
        let mut count = 0;
        while let Some(message) = buffers.next_to_transmit(20, 0, 0) {
            kinds[count] = match message {
                OutboundMessage::RetransmissionResponse { .. } => "RetrResp",
                OutboundMessage::RetransmittedData { .. } => "RetrData",
                OutboundMessage::Data { .. } => "Data",
                OutboundMessage::RetransmissionRequest { .. } => "RetrReq",
                OutboundMessage::Heartbeat { .. } => "Heartbeat",
                _ => "other",
            };
            count += 1;
        }
        (kinds, count)
    }

    #[test]
    fn unsent_retransmission_request_after_unsent_data_survives_rebuild() {
        let mut buffers = ConnectionBuffers::new(config(10));
        let data = ApplicationData::single(&ApplicationPayload::new(&[7]).unwrap());
        buffers
            .enqueue_send(OutboundMessage::Data {
                header: header(1),
                data,
            })
            .unwrap();
        // Sent; a remote window of one is then exhausted.
        buffers.next_to_transmit(1, 0, 0).unwrap();
        buffers
            .enqueue_send(OutboundMessage::Data {
                header: header(2),
                data,
            })
            .unwrap();
        buffers
            .enqueue_send(OutboundMessage::RetransmissionRequest { header: header(3) })
            .unwrap();

        let mut next = 10;
        buffers
            .rebuild_for_retransmission(
                1,
                OutboundMessage::RetransmissionResponse { header: header(9) },
                &mut next,
            )
            .unwrap();

        let (kinds, count) = rebuilt_kinds(&mut buffers);
        assert_eq!(
            &kinds[..count],
            &["RetrResp", "RetrData", "Data", "RetrReq"]
        );
        // The supplied response keeps its header; RetrData, Data and RetrReq
        // are resequenced.
        assert_eq!(next, 13);
    }

    #[test]
    fn unsent_retransmission_request_ends_an_open_replay_with_a_heartbeat() {
        let mut buffers = ConnectionBuffers::new(config(10));
        let data = ApplicationData::single(&ApplicationPayload::new(&[7]).unwrap());
        buffers
            .enqueue_send(OutboundMessage::Data {
                header: header(1),
                data,
            })
            .unwrap();
        buffers.next_to_transmit(1, 0, 0).unwrap();
        buffers
            .enqueue_send(OutboundMessage::RetransmissionRequest { header: header(2) })
            .unwrap();
        buffers
            .enqueue_send(OutboundMessage::RetransmissionRequest { header: header(3) })
            .unwrap();

        let mut next = 10;
        buffers
            .rebuild_for_retransmission(
                1,
                OutboundMessage::RetransmissionResponse { header: header(9) },
                &mut next,
            )
            .unwrap();

        let (kinds, count) = rebuilt_kinds(&mut buffers);
        assert_eq!(
            &kinds[..count],
            &["RetrResp", "RetrData", "Heartbeat", "RetrReq", "RetrReq"]
        );
        assert_eq!(next, 14);
    }

    #[test]
    fn payload_bounds_match_the_public_api() {
        assert_eq!(ApplicationPayload::new(&[]), Err(PayloadError::Empty));
        assert!(ApplicationPayload::new(&[0; MAX_APPLICATION_PAYLOAD]).is_ok());
        assert_eq!(
            ApplicationPayload::new(&[0; MAX_APPLICATION_PAYLOAD + 1]),
            Err(PayloadError::TooLarge {
                actual: MAX_APPLICATION_PAYLOAD + 1,
                maximum: MAX_APPLICATION_PAYLOAD,
            })
        );
    }
}
