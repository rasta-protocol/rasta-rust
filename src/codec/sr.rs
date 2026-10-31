//! Codec for Safety and Retransmission Layer (SRL) PDUs.
//!
//! The wire layout is traced to the labelled observations in
//! `docs/requirements/observations.md`; it is not a conformance claim.
//! Decoding borrows its input and encoding writes into a caller-owned buffer.

use core::fmt;

use super::safety_code::{self, Md4InitialValue, SafetyCodeError};

pub use super::safety_code::SafetyCodeLength;

/// Length of the common SR header in bytes.
pub const SR_HEADER_LEN: usize = 28;

/// Maximum application message in bytes, supported by this implementation.
/// The wire format would allow larger messages.
pub const MAX_APPLICATION_PAYLOAD: usize = 1_055;

/// Size of the length prefix in front of every application message.
pub const APPLICATION_LENGTH_PREFIX: usize = 2;

/// Maximum application-data field of one Data message: all application
/// messages with their length prefixes. One message of
/// [`MAX_APPLICATION_PAYLOAD`] bytes fills it.
pub const MAX_DATA_FIELD_LEN: usize = MAX_APPLICATION_PAYLOAD + APPLICATION_LENGTH_PREFIX;

/// Maximum complete SR PDU in bytes, reached with the full MD4 safety code.
pub const MAX_SR_PDU_LEN: usize = 1_101;

const CONNECTION_BODY_END: usize = 42;
const DATA_PREFIX_END: usize = 30;
const DISCONNECT_BODY_END: usize = 32;
static ZERO_SAFETY_CODE: [u8; 16] = [0; 16];

/// Profile-dependent codec limits.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SrCodecConfig {
    safety_code_length: SafetyCodeLength,
    max_payload_length: usize,
    min_n_send_max: u16,
    max_n_send_max: u16,
}

impl SrCodecConfig {
    /// Constructs validated codec limits.
    ///
    /// # Errors
    ///
    /// Returns [`CodecError::InvalidConfiguration`] if the maximum application
    /// message exceeds the fixed buffers ([`MAX_APPLICATION_PAYLOAD`]), the
    /// minimum `N_SENDMAX` is zero, or the `N_SENDMAX` range is empty.
    /// Accepting a peer's `N_SENDMAX` of zero would stop all sending to it.
    pub fn new(
        safety_code_length: SafetyCodeLength,
        max_payload_length: usize,
        min_n_send_max: u16,
        max_n_send_max: u16,
    ) -> Result<Self, CodecError> {
        if max_payload_length > MAX_APPLICATION_PAYLOAD
            || min_n_send_max == 0
            || min_n_send_max > max_n_send_max
        {
            return Err(CodecError::InvalidConfiguration);
        }

        Ok(Self {
            safety_code_length,
            max_payload_length,
            min_n_send_max,
            max_n_send_max,
        })
    }

    /// Returns the default limits: application payloads up to
    /// [`MAX_APPLICATION_PAYLOAD`] bytes and `N_SENDMAX` from 2 through 20.
    #[must_use]
    pub const fn with_default_limits(safety_code_length: SafetyCodeLength) -> Self {
        Self {
            safety_code_length,
            max_payload_length: MAX_APPLICATION_PAYLOAD,
            min_n_send_max: 2,
            max_n_send_max: 20,
        }
    }

    /// Returns the configured safety-code width.
    #[must_use]
    pub const fn safety_code_length(self) -> SafetyCodeLength {
        self.safety_code_length
    }

    /// Returns the maximum accepted application message.
    #[must_use]
    pub const fn max_payload_length(self) -> usize {
        self.max_payload_length
    }

    /// Returns the maximum application-data field: one maximal application
    /// message with its length prefix.
    #[must_use]
    pub const fn max_data_field_length(self) -> usize {
        self.max_payload_length + APPLICATION_LENGTH_PREFIX
    }
}

/// Four ASCII decimal digits identifying a protocol version.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ProtocolVersion([u8; 4]);

impl ProtocolVersion {
    /// Creates a protocol version after checking its wire representation.
    ///
    /// # Errors
    ///
    /// Returns [`CodecError::InvalidProtocolVersion`] unless all bytes are
    /// ASCII decimal digits.
    pub const fn new(bytes: [u8; 4]) -> Result<Self, CodecError> {
        let mut index = 0;
        while index < bytes.len() {
            if !bytes[index].is_ascii_digit() {
                return Err(CodecError::InvalidProtocolVersion(bytes));
            }
            index += 1;
        }
        Ok(Self(bytes))
    }

    /// Returns the four wire bytes.
    #[must_use]
    pub const fn as_bytes(self) -> [u8; 4] {
        self.0
    }
}

impl TryFrom<[u8; 4]> for ProtocolVersion {
    type Error = CodecError;

    fn try_from(value: [u8; 4]) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

/// Fields shared by all SR messages after length and type.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct SrHeader {
    /// Intended receiver identifier.
    pub receiver_id: u32,
    /// Sending node identifier.
    pub sender_id: u32,
    /// Sequence number of this message.
    pub sequence_number: u32,
    /// Last confirmed received sequence number.
    pub confirmed_sequence_number: u32,
    /// Sender timestamp.
    pub timestamp: u32,
    /// Last confirmed peer timestamp.
    pub confirmed_timestamp: u32,
}

/// Connection body fields.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ConnectionData {
    /// Four-digit protocol version.
    pub version: ProtocolVersion,
    /// Maximum number of unconfirmed sent messages.
    pub n_send_max: u16,
    /// Reserved wire bytes, emitted unchanged for explicit round trips.
    pub reserved: [u8; 8],
}

impl ConnectionData {
    /// Creates connection data with all reserved bytes cleared.
    #[must_use]
    pub const fn new(version: ProtocolVersion, n_send_max: u16) -> Self {
        Self {
            version,
            n_send_max,
            reserved: [0; 8],
        }
    }
}

/// Known SR message types and their wire values.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u16)]
pub enum MessageType {
    /// Connection request (`6200`).
    ConnectionRequest = 6_200,
    /// Connection response (`6201`).
    ConnectionResponse = 6_201,
    /// Retransmission request (`6212`).
    RetransmissionRequest = 6_212,
    /// Retransmission response (`6213`).
    RetransmissionResponse = 6_213,
    /// Disconnection request (`6216`).
    DisconnectionRequest = 6_216,
    /// Heartbeat (`6220`).
    Heartbeat = 6_220,
    /// Application data (`6240`).
    Data = 6_240,
    /// Retransmitted application data (`6241`).
    RetransmittedData = 6_241,
}

impl TryFrom<u16> for MessageType {
    type Error = CodecError;

    fn try_from(value: u16) -> Result<Self, Self::Error> {
        match value {
            6_200 => Ok(Self::ConnectionRequest),
            6_201 => Ok(Self::ConnectionResponse),
            6_212 => Ok(Self::RetransmissionRequest),
            6_213 => Ok(Self::RetransmissionResponse),
            6_216 => Ok(Self::DisconnectionRequest),
            6_220 => Ok(Self::Heartbeat),
            6_240 => Ok(Self::Data),
            6_241 => Ok(Self::RetransmittedData),
            _ => Err(CodecError::UnknownMessageType(value)),
        }
    }
}

/// Known high-level reasons for a disconnection request.
///
/// Only wire values 0 through 8 are accepted. Decoding another value returns
/// [`CodecError::UnknownDisconnectReason`]; the composed stack discards that
/// disconnection request without closing the connection or refreshing its timer.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u16)]
pub enum DisconnectReason {
    /// Requested by a user.
    UserRequest = 0,
    /// Reserved/not in use.
    NotInUse = 1,
    /// Unexpected message.
    UnexpectedMessage = 2,
    /// Sequence-number error.
    SequenceNumberError = 3,
    /// Timeout.
    Timeout = 4,
    /// Requested service is not allowed.
    ServiceNotAllowed = 5,
    /// Incompatible protocol version.
    ProtocolVersionError = 6,
    /// Retransmission failed.
    RetransmissionFailed = 7,
    /// Invalid protocol sequence.
    ProtocolSequenceError = 8,
}

impl TryFrom<u16> for DisconnectReason {
    type Error = CodecError;

    fn try_from(value: u16) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(Self::UserRequest),
            1 => Ok(Self::NotInUse),
            2 => Ok(Self::UnexpectedMessage),
            3 => Ok(Self::SequenceNumberError),
            4 => Ok(Self::Timeout),
            5 => Ok(Self::ServiceNotAllowed),
            6 => Ok(Self::ProtocolVersionError),
            7 => Ok(Self::RetransmissionFailed),
            8 => Ok(Self::ProtocolSequenceError),
            _ => Err(CodecError::UnknownDisconnectReason(value)),
        }
    }
}

/// The message-specific SR body.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SrBody<'a> {
    /// Connection request body.
    ConnectionRequest(ConnectionData),
    /// Connection response body.
    ConnectionResponse(ConnectionData),
    /// Retransmission request without additional body fields.
    RetransmissionRequest,
    /// Retransmission response without additional body fields.
    RetransmissionResponse,
    /// Disconnection request body.
    DisconnectionRequest {
        /// Implementation-specific detailed reason.
        detailed_reason: u16,
        /// High-level reason.
        reason: DisconnectReason,
    },
    /// Heartbeat without additional body fields.
    Heartbeat,
    /// Application data: the application-data field, one or more application
    /// messages, each preceded by its little-endian `u16` length (see
    /// [`DataFieldMessages`]).
    Data(&'a [u8]),
    /// Retransmitted application data, with the same field as [`Self::Data`].
    RetransmittedData(&'a [u8]),
}

impl SrBody<'_> {
    /// Returns the wire message type corresponding to this body.
    #[must_use]
    pub const fn message_type(&self) -> MessageType {
        match self {
            Self::ConnectionRequest(_) => MessageType::ConnectionRequest,
            Self::ConnectionResponse(_) => MessageType::ConnectionResponse,
            Self::RetransmissionRequest => MessageType::RetransmissionRequest,
            Self::RetransmissionResponse => MessageType::RetransmissionResponse,
            Self::DisconnectionRequest { .. } => MessageType::DisconnectionRequest,
            Self::Heartbeat => MessageType::Heartbeat,
            Self::Data(_) => MessageType::Data,
            Self::RetransmittedData(_) => MessageType::RetransmittedData,
        }
    }
}

/// A decoded or encode-ready SR PDU.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SrMessage<'a> {
    /// Common header fields.
    pub header: SrHeader,
    /// Message-specific body.
    pub body: SrBody<'a>,
    /// Opaque trailing safety code.
    ///
    /// The plain codec validates only its configured width.
    /// [`SrMessage::encode_with_md4`] calculates the code and
    /// [`decode_and_verify_md4`] verifies it.
    pub safety_code: &'a [u8],
}

impl SrMessage<'_> {
    /// Computes the encoded PDU length after validating profile limits.
    ///
    /// # Errors
    ///
    /// Returns an error for an invalid payload, connection parameter, safety
    /// code width, or length that does not fit the wire field.
    pub fn encoded_len(&self, config: SrCodecConfig) -> Result<usize, CodecError> {
        validate_safety_code(self.safety_code, config)?;

        let body_end = match self.body {
            SrBody::ConnectionRequest(data) | SrBody::ConnectionResponse(data) => {
                validate_n_send_max(data.n_send_max, config)?;
                CONNECTION_BODY_END
            }
            SrBody::RetransmissionRequest | SrBody::RetransmissionResponse | SrBody::Heartbeat => {
                SR_HEADER_LEN
            }
            SrBody::DisconnectionRequest { .. } => DISCONNECT_BODY_END,
            SrBody::Data(field) | SrBody::RetransmittedData(field) => {
                validate_data_field(field, config)?;
                SR_HEADER_LEN + field.len()
            }
        };

        let length = body_end + self.safety_code.len();
        if length > usize::from(u16::MAX) {
            return Err(CodecError::MessageTooLong { length });
        }
        Ok(length)
    }

    /// Serialises this PDU into a caller-owned buffer.
    ///
    /// Returns the number of bytes written. All validation happens before the
    /// first output byte is modified.
    ///
    /// # Errors
    ///
    /// Returns an error when the message violates configured limits or the
    /// output buffer is too small.
    pub fn encode_into(
        &self,
        config: SrCodecConfig,
        output: &mut [u8],
    ) -> Result<usize, CodecError> {
        let length = self.encoded_len(config)?;
        if output.len() < length {
            return Err(CodecError::OutputTooSmall {
                required: length,
                actual: output.len(),
            });
        }

        let wire_length =
            u16::try_from(length).map_err(|_| CodecError::MessageTooLong { length })?;
        write_u16(output, 0, wire_length);
        write_u16(output, 2, self.body.message_type() as u16);
        write_u32(output, 4, self.header.receiver_id);
        write_u32(output, 8, self.header.sender_id);
        write_u32(output, 12, self.header.sequence_number);
        write_u32(output, 16, self.header.confirmed_sequence_number);
        write_u32(output, 20, self.header.timestamp);
        write_u32(output, 24, self.header.confirmed_timestamp);

        let body_end = match self.body {
            SrBody::ConnectionRequest(data) | SrBody::ConnectionResponse(data) => {
                output[28..32].copy_from_slice(&data.version.as_bytes());
                write_u16(output, 32, data.n_send_max);
                output[34..42].copy_from_slice(&data.reserved);
                CONNECTION_BODY_END
            }
            SrBody::RetransmissionRequest | SrBody::RetransmissionResponse | SrBody::Heartbeat => {
                SR_HEADER_LEN
            }
            SrBody::DisconnectionRequest {
                detailed_reason,
                reason,
            } => {
                write_u16(output, 28, detailed_reason);
                write_u16(output, 30, reason as u16);
                DISCONNECT_BODY_END
            }
            SrBody::Data(field) | SrBody::RetransmittedData(field) => {
                output[SR_HEADER_LEN..SR_HEADER_LEN + field.len()].copy_from_slice(field);
                SR_HEADER_LEN + field.len()
            }
        };

        output[body_end..length].copy_from_slice(self.safety_code);
        Ok(length)
    }

    /// Serialises this PDU and calculates its configured MD4 safety code.
    ///
    /// Any safety code already present in `self` is ignored and replaced. The
    /// configured width determines whether no code, the first eight digest
    /// bytes, or the complete digest is emitted.
    ///
    /// # Errors
    ///
    /// Returns an error when the message violates configured limits, the output
    /// buffer is too small, or safety-code calculation fails.
    pub fn encode_with_md4(
        &self,
        config: SrCodecConfig,
        initial_value: Md4InitialValue,
        output: &mut [u8],
    ) -> Result<usize, CodecError> {
        let code_length = config.safety_code_length.bytes();
        let message = Self {
            header: self.header,
            body: self.body,
            safety_code: &ZERO_SAFETY_CODE[..code_length],
        };
        let length = message.encode_into(config, output)?;
        safety_code::write_md4_safety_code(
            &mut output[..length],
            config.safety_code_length,
            initial_value,
        )?;
        Ok(length)
    }
}

/// Decodes one complete SR PDU using explicit profile limits.
///
/// The returned body and safety code borrow `input`; no allocation occurs.
///
/// # Errors
///
/// Returns a precise [`CodecError`] for malformed or out-of-profile input.
pub fn decode(input: &[u8], config: SrCodecConfig) -> Result<SrMessage<'_>, CodecError> {
    if input.len() < 4 {
        return Err(CodecError::InputTooShort {
            minimum: 4,
            actual: input.len(),
        });
    }

    let declared_length = usize::from(read_u16(input, 0));
    if declared_length != input.len() {
        return Err(CodecError::DeclaredLengthMismatch {
            declared: declared_length,
            actual: input.len(),
        });
    }

    let safety_length = config.safety_code_length.bytes();
    let minimum = SR_HEADER_LEN + safety_length;
    if input.len() < minimum {
        return Err(CodecError::InputTooShort {
            minimum,
            actual: input.len(),
        });
    }

    let message_type = MessageType::try_from(read_u16(input, 2))?;
    let header = SrHeader {
        receiver_id: read_u32(input, 4),
        sender_id: read_u32(input, 8),
        sequence_number: read_u32(input, 12),
        confirmed_sequence_number: read_u32(input, 16),
        timestamp: read_u32(input, 20),
        confirmed_timestamp: read_u32(input, 24),
    };

    let body_end = input.len() - safety_length;
    let body = match message_type {
        MessageType::ConnectionRequest | MessageType::ConnectionResponse => {
            require_body_end(message_type, body_end, CONNECTION_BODY_END)?;
            let version_bytes = [input[28], input[29], input[30], input[31]];
            let version = ProtocolVersion::new(version_bytes)?;
            let n_send_max = read_u16(input, 32);
            validate_n_send_max(n_send_max, config)?;
            let mut reserved = [0; 8];
            reserved.copy_from_slice(&input[34..42]);
            let data = ConnectionData {
                version,
                n_send_max,
                reserved,
            };
            if message_type == MessageType::ConnectionRequest {
                SrBody::ConnectionRequest(data)
            } else {
                SrBody::ConnectionResponse(data)
            }
        }
        MessageType::RetransmissionRequest => {
            require_body_end(message_type, body_end, SR_HEADER_LEN)?;
            SrBody::RetransmissionRequest
        }
        MessageType::RetransmissionResponse => {
            require_body_end(message_type, body_end, SR_HEADER_LEN)?;
            SrBody::RetransmissionResponse
        }
        MessageType::DisconnectionRequest => {
            require_body_end(message_type, body_end, DISCONNECT_BODY_END)?;
            SrBody::DisconnectionRequest {
                detailed_reason: read_u16(input, 28),
                reason: DisconnectReason::try_from(read_u16(input, 30))?,
            }
        }
        MessageType::Heartbeat => {
            require_body_end(message_type, body_end, SR_HEADER_LEN)?;
            SrBody::Heartbeat
        }
        MessageType::Data | MessageType::RetransmittedData => {
            // Only the field's size is checked here. Its inner structure is
            // checked after the safety code (`validate_data_field`), so that a
            // corrupted message is discarded while an authentic but malformed
            // one can be treated as a protocol violation.
            if body_end < DATA_PREFIX_END + 1 {
                return Err(CodecError::BodyLengthMismatch {
                    message_type,
                    expected: DATA_PREFIX_END + 1,
                    actual: body_end,
                });
            }
            let field = &input[SR_HEADER_LEN..body_end];
            if field.len() > config.max_data_field_length() {
                return Err(CodecError::PayloadTooLarge {
                    maximum: config.max_data_field_length(),
                    actual: field.len(),
                });
            }
            if message_type == MessageType::Data {
                SrBody::Data(field)
            } else {
                SrBody::RetransmittedData(field)
            }
        }
    };

    Ok(SrMessage {
        header,
        body,
        safety_code: &input[body_end..],
    })
}

/// Decodes one complete SR PDU and verifies its configured MD4 safety code.
///
/// Structural parsing is bounds-checked before the safety code is calculated;
/// the returned message still borrows the original input, including its code.
///
/// # Errors
///
/// Returns a codec error for malformed input or a safety-code error when the
/// received and calculated values differ.
pub fn decode_and_verify_md4(
    input: &[u8],
    config: SrCodecConfig,
    initial_value: Md4InitialValue,
) -> Result<SrMessage<'_>, CodecError> {
    let message = decode(input, config)?;
    safety_code::verify_md4_safety_code(input, config.safety_code_length, initial_value)?;
    Ok(message)
}

/// Errors returned by SR encoding and decoding.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CodecError {
    /// Codec limits are internally inconsistent or not wire-representable.
    InvalidConfiguration,
    /// Input ended before the minimum safe parse boundary.
    InputTooShort {
        /// Required number of bytes.
        minimum: usize,
        /// Available number of bytes.
        actual: usize,
    },
    /// The header length and datagram length differ.
    DeclaredLengthMismatch {
        /// Length declared in the PDU.
        declared: usize,
        /// Length supplied to the decoder.
        actual: usize,
    },
    /// An unknown message-type value was received.
    UnknownMessageType(u16),
    /// A fixed-size body had the wrong size.
    BodyLengthMismatch {
        /// Parsed message type.
        message_type: MessageType,
        /// Expected end offset before the safety code.
        expected: usize,
        /// Actual end offset before the safety code.
        actual: usize,
    },
    /// The data length field disagreed with the PDU boundary.
    PayloadLengthMismatch {
        /// Length declared in the data body.
        declared: usize,
        /// Number of payload bytes actually present.
        actual: usize,
    },
    /// An application-data field contained no application message.
    EmptyDataField,
    /// An application message had length zero.
    EmptyApplicationMessage,
    /// An application-data field ended inside a length prefix.
    DataFieldTruncated {
        /// Bytes left after the last complete message.
        remaining: usize,
    },
    /// An application payload exceeded the configured limit.
    PayloadTooLarge {
        /// Configured maximum.
        maximum: usize,
        /// Supplied payload length.
        actual: usize,
    },
    /// Protocol-version bytes were not four ASCII decimal digits.
    InvalidProtocolVersion([u8; 4]),
    /// `N_SENDMAX` was outside the configured inclusive range.
    InvalidNSendMax {
        /// Configured inclusive minimum.
        minimum: u16,
        /// Configured inclusive maximum.
        maximum: u16,
        /// Supplied value.
        actual: u16,
    },
    /// A disconnect reason did not match a known value.
    UnknownDisconnectReason(u16),
    /// The opaque safety code had the wrong configured width.
    SafetyCodeLengthMismatch {
        /// Configured number of bytes.
        expected: usize,
        /// Supplied number of bytes.
        actual: usize,
    },
    /// Safety-code calculation or verification failed.
    SafetyCodeFailure(SafetyCodeError),
    /// The complete message cannot fit its `u16` wire length.
    MessageTooLong {
        /// Computed message length.
        length: usize,
    },
    /// The caller-provided output buffer was too small.
    OutputTooSmall {
        /// Required output capacity.
        required: usize,
        /// Supplied output capacity.
        actual: usize,
    },
}

impl fmt::Display for CodecError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidConfiguration => formatter.write_str("invalid SR codec configuration"),
            Self::InputTooShort { minimum, actual } => {
                write!(
                    formatter,
                    "SR PDU needs at least {minimum} bytes, got {actual}"
                )
            }
            Self::DeclaredLengthMismatch { declared, actual } => write!(
                formatter,
                "SR PDU declares {declared} bytes but datagram contains {actual}"
            ),
            Self::UnknownMessageType(value) => {
                write!(formatter, "unknown SR message type {value}")
            }
            Self::BodyLengthMismatch {
                message_type,
                expected,
                actual,
            } => write!(
                formatter,
                "{message_type:?} body ends at {actual}, expected {expected}"
            ),
            Self::PayloadLengthMismatch { declared, actual } => write!(
                formatter,
                "SR payload declares {declared} bytes but contains {actual}"
            ),
            Self::EmptyDataField => formatter.write_str("application-data field is empty"),
            Self::EmptyApplicationMessage => {
                formatter.write_str("application message has length zero")
            }
            Self::DataFieldTruncated { remaining } => write!(
                formatter,
                "application-data field ends {remaining} bytes into a length prefix"
            ),
            Self::PayloadTooLarge { maximum, actual } => {
                write!(
                    formatter,
                    "SR payload has {actual} bytes, maximum is {maximum}"
                )
            }
            Self::InvalidProtocolVersion(bytes) => {
                write!(formatter, "invalid protocol version bytes {bytes:?}")
            }
            Self::InvalidNSendMax {
                minimum,
                maximum,
                actual,
            } => write!(
                formatter,
                "N_SENDMAX {actual} is outside {minimum}..={maximum}"
            ),
            Self::UnknownDisconnectReason(value) => {
                write!(formatter, "unknown disconnect reason {value}")
            }
            Self::SafetyCodeLengthMismatch { expected, actual } => write!(
                formatter,
                "safety code has {actual} bytes, expected {expected}"
            ),
            Self::SafetyCodeFailure(error) => error.fmt(formatter),
            Self::MessageTooLong { length } => {
                write!(formatter, "SR PDU length {length} does not fit u16")
            }
            Self::OutputTooSmall { required, actual } => write!(
                formatter,
                "output buffer has {actual} bytes, needs {required}"
            ),
        }
    }
}

impl core::error::Error for CodecError {}

impl From<SafetyCodeError> for CodecError {
    fn from(value: SafetyCodeError) -> Self {
        Self::SafetyCodeFailure(value)
    }
}

fn validate_safety_code(safety_code: &[u8], config: SrCodecConfig) -> Result<(), CodecError> {
    let expected = config.safety_code_length.bytes();
    if safety_code.len() != expected {
        return Err(CodecError::SafetyCodeLengthMismatch {
            expected,
            actual: safety_code.len(),
        });
    }
    Ok(())
}

/// Application messages in an application-data field, in order.
///
/// Each item is one message or the error at which the field stops being
/// well-formed; iteration ends after an error.
#[derive(Clone, Debug)]
pub struct DataFieldMessages<'a> {
    rest: &'a [u8],
    failed: bool,
}

impl<'a> DataFieldMessages<'a> {
    /// Iterates over the messages of `field`.
    #[must_use]
    pub const fn new(field: &'a [u8]) -> Self {
        Self {
            rest: field,
            failed: false,
        }
    }
}

impl<'a> Iterator for DataFieldMessages<'a> {
    type Item = Result<&'a [u8], CodecError>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.failed || self.rest.is_empty() {
            return None;
        }
        let result = if self.rest.len() < APPLICATION_LENGTH_PREFIX {
            Err(CodecError::DataFieldTruncated {
                remaining: self.rest.len(),
            })
        } else {
            let declared = usize::from(u16::from_le_bytes([self.rest[0], self.rest[1]]));
            let available = self.rest.len() - APPLICATION_LENGTH_PREFIX;
            if declared == 0 {
                Err(CodecError::EmptyApplicationMessage)
            } else if declared > available {
                Err(CodecError::PayloadLengthMismatch {
                    declared,
                    actual: available,
                })
            } else {
                let end = APPLICATION_LENGTH_PREFIX + declared;
                let message = &self.rest[APPLICATION_LENGTH_PREFIX..end];
                self.rest = &self.rest[end..];
                Ok(message)
            }
        };
        if result.is_err() {
            self.failed = true;
        }
        Some(result)
    }
}

/// Checks the structure of an application-data field and returns the number
/// of application messages in it.
///
/// # Errors
///
/// Returns an error for an empty field, a message of length zero or above the
/// configured maximum, a length prefix that runs past the field, or a field
/// larger than [`SrCodecConfig::max_data_field_length`].
pub fn validate_data_field(field: &[u8], config: SrCodecConfig) -> Result<usize, CodecError> {
    if field.len() > config.max_data_field_length() {
        return Err(CodecError::PayloadTooLarge {
            maximum: config.max_data_field_length(),
            actual: field.len(),
        });
    }
    let mut count = 0;
    for message in DataFieldMessages::new(field) {
        validate_payload(message?.len(), config)?;
        count += 1;
    }
    if count == 0 {
        return Err(CodecError::EmptyDataField);
    }
    Ok(count)
}

fn validate_payload(length: usize, config: SrCodecConfig) -> Result<(), CodecError> {
    if length > config.max_payload_length {
        return Err(CodecError::PayloadTooLarge {
            maximum: config.max_payload_length,
            actual: length,
        });
    }
    Ok(())
}

fn validate_n_send_max(value: u16, config: SrCodecConfig) -> Result<(), CodecError> {
    if value < config.min_n_send_max || value > config.max_n_send_max {
        return Err(CodecError::InvalidNSendMax {
            minimum: config.min_n_send_max,
            maximum: config.max_n_send_max,
            actual: value,
        });
    }
    Ok(())
}

fn require_body_end(
    message_type: MessageType,
    actual: usize,
    expected: usize,
) -> Result<(), CodecError> {
    if actual != expected {
        return Err(CodecError::BodyLengthMismatch {
            message_type,
            expected,
            actual,
        });
    }
    Ok(())
}

fn read_u16(input: &[u8], offset: usize) -> u16 {
    u16::from_le_bytes([input[offset], input[offset + 1]])
}

fn read_u32(input: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes([
        input[offset],
        input[offset + 1],
        input[offset + 2],
        input[offset + 3],
    ])
}

fn write_u16(output: &mut [u8], offset: usize, value: u16) {
    output[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
}

fn write_u32(output: &mut [u8], offset: usize, value: u32) {
    output[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}
