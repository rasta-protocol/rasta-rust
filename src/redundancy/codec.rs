//! Bounded Redundancy Layer frame codec and check-code profiles.

use core::fmt;

/// Redundancy header width.
pub const REDUNDANCY_HEADER_LEN: usize = 8;
/// Maximum wrapped SR PDU width.
pub const MAX_REDUNDANCY_PAYLOAD_LEN: usize = 1_101;
/// Maximum complete Redundancy Layer frame width.
pub const MAX_REDUNDANCY_FRAME_LEN: usize = 1_113;
const MIN_REDUNDANCY_PAYLOAD_LEN: usize = 28;

/// Redundancy Layer check-code profiles.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CheckCodeType {
    /// No check code.
    A,
    /// Non-reflected 32-bit CRC with polynomial `0xEE5B42FD`.
    B,
    /// CRC-32C.
    C,
    /// CRC-16/KERMIT.
    D,
    /// CRC-16/ARC.
    E,
}

impl CheckCodeType {
    /// Returns the trailing check-code width.
    #[must_use]
    pub const fn bytes(self) -> usize {
        match self {
            Self::A => 0,
            Self::B | Self::C => 4,
            Self::D | Self::E => 2,
        }
    }
}

/// Borrowed validated Redundancy Layer frame.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RedundancyFrame<'a> {
    /// Redundancy-layer sequence number.
    pub sequence_number: u32,
    /// Wrapped complete SR PDU.
    pub payload: &'a [u8],
}

impl RedundancyFrame<'_> {
    /// Returns the encoded frame width for a check-code profile.
    ///
    /// # Errors
    ///
    /// Returns an error when the wrapped SR PDU is outside the allowed bounds.
    pub fn encoded_len(&self, check_code: CheckCodeType) -> Result<usize, RedundancyCodecError> {
        validate_payload(self.payload)?;
        Ok(REDUNDANCY_HEADER_LEN + self.payload.len() + check_code.bytes())
    }

    /// Encodes this frame and appends its configured check code.
    ///
    /// # Errors
    ///
    /// Returns an error for invalid payload size or insufficient output space.
    pub fn encode_into(
        &self,
        check_code: CheckCodeType,
        output: &mut [u8],
    ) -> Result<usize, RedundancyCodecError> {
        let encoded_len = self.encoded_len(check_code)?;
        if output.len() < encoded_len {
            return Err(RedundancyCodecError::OutputTooSmall {
                required: encoded_len,
                actual: output.len(),
            });
        }
        let encoded_len_u16 =
            u16::try_from(encoded_len).map_err(|_| RedundancyCodecError::FrameTooLarge)?;
        output[..encoded_len].fill(0);
        output[..2].copy_from_slice(&encoded_len_u16.to_le_bytes());
        output[4..8].copy_from_slice(&self.sequence_number.to_le_bytes());
        let payload_end = REDUNDANCY_HEADER_LEN + self.payload.len();
        output[REDUNDANCY_HEADER_LEN..payload_end].copy_from_slice(self.payload);
        write_check_code(&mut output[..encoded_len], check_code);
        Ok(encoded_len)
    }
}

/// Decodes and verifies one complete Redundancy Layer frame.
///
/// # Errors
///
/// Returns a typed error for malformed lengths, non-zero reserved bytes,
/// payload bounds, or a check-code mismatch.
pub fn decode(
    input: &[u8],
    check_code: CheckCodeType,
) -> Result<RedundancyFrame<'_>, RedundancyCodecError> {
    let minimum = REDUNDANCY_HEADER_LEN + check_code.bytes();
    if input.len() < minimum {
        return Err(RedundancyCodecError::InputTooShort {
            minimum,
            actual: input.len(),
        });
    }
    let declared = usize::from(u16::from_le_bytes([input[0], input[1]]));
    if declared != input.len() {
        return Err(RedundancyCodecError::DeclaredLengthMismatch {
            declared,
            actual: input.len(),
        });
    }
    if input[2] != 0 || input[3] != 0 {
        return Err(RedundancyCodecError::ReservedBytesNotZero);
    }
    if !verify_check_code(input, check_code) {
        return Err(RedundancyCodecError::CheckCodeMismatch);
    }
    let payload_end = input.len() - check_code.bytes();
    let payload = &input[REDUNDANCY_HEADER_LEN..payload_end];
    validate_payload(payload)?;
    Ok(RedundancyFrame {
        sequence_number: u32::from_le_bytes([input[4], input[5], input[6], input[7]]),
        payload,
    })
}

/// Calculates the numeric check code for test vectors and diagnostics.
#[must_use]
pub fn calculate_check_code(input: &[u8], check_code: CheckCodeType) -> u32 {
    match check_code {
        CheckCodeType::A => 0,
        CheckCodeType::B => crc_msb(input, 32, 0xee5b_42fd, 0, 0),
        CheckCodeType::C => crc_lsb(input, 32, 0x82f6_3b78, u32::MAX, u32::MAX),
        CheckCodeType::D => crc_lsb(input, 16, 0x8408, 0, 0),
        CheckCodeType::E => crc_lsb(input, 16, 0xa001, 0, 0),
    }
}

/// Redundancy frame decoding or encoding failure.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RedundancyCodecError {
    /// Input is shorter than the fixed header and configured check code.
    InputTooShort {
        /// Required width.
        minimum: usize,
        /// Supplied width.
        actual: usize,
    },
    /// The wire length does not equal the datagram width.
    DeclaredLengthMismatch {
        /// Declared width.
        declared: usize,
        /// Datagram width.
        actual: usize,
    },
    /// Reserved header bytes were not zero.
    ReservedBytesNotZero,
    /// Wrapped SR PDU is too short.
    PayloadTooShort {
        /// Required minimum.
        minimum: usize,
        /// Actual width.
        actual: usize,
    },
    /// Wrapped SR PDU exceeds the maximum.
    PayloadTooLarge {
        /// Accepted maximum.
        maximum: usize,
        /// Actual width.
        actual: usize,
    },
    /// Complete frame width cannot be represented on the wire.
    FrameTooLarge,
    /// Caller output buffer is too small.
    OutputTooSmall {
        /// Required width.
        required: usize,
        /// Supplied width.
        actual: usize,
    },
    /// Trailing check code differs from the calculated value.
    CheckCodeMismatch,
}

impl fmt::Display for RedundancyCodecError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InputTooShort { minimum, actual } => {
                write!(
                    formatter,
                    "redundancy frame needs {minimum} bytes, got {actual}"
                )
            }
            Self::DeclaredLengthMismatch { declared, actual } => {
                write!(
                    formatter,
                    "declared frame length {declared} differs from {actual}"
                )
            }
            Self::ReservedBytesNotZero => formatter.write_str("reserved bytes are not zero"),
            Self::PayloadTooShort { minimum, actual } => {
                write!(
                    formatter,
                    "wrapped SR PDU needs {minimum} bytes, got {actual}"
                )
            }
            Self::PayloadTooLarge { maximum, actual } => {
                write!(
                    formatter,
                    "wrapped SR PDU length {actual} exceeds {maximum}"
                )
            }
            Self::FrameTooLarge => formatter.write_str("redundancy frame is too large"),
            Self::OutputTooSmall { required, actual } => {
                write!(formatter, "output needs {required} bytes, got {actual}")
            }
            Self::CheckCodeMismatch => formatter.write_str("redundancy check code mismatch"),
        }
    }
}

impl core::error::Error for RedundancyCodecError {}

fn validate_payload(payload: &[u8]) -> Result<(), RedundancyCodecError> {
    if payload.len() < MIN_REDUNDANCY_PAYLOAD_LEN {
        return Err(RedundancyCodecError::PayloadTooShort {
            minimum: MIN_REDUNDANCY_PAYLOAD_LEN,
            actual: payload.len(),
        });
    }
    if payload.len() > MAX_REDUNDANCY_PAYLOAD_LEN {
        return Err(RedundancyCodecError::PayloadTooLarge {
            maximum: MAX_REDUNDANCY_PAYLOAD_LEN,
            actual: payload.len(),
        });
    }
    Ok(())
}

fn write_check_code(frame: &mut [u8], check_code: CheckCodeType) {
    let width = check_code.bytes();
    if width == 0 {
        return;
    }
    let payload_end = frame.len() - width;
    let value = calculate_check_code(&frame[..payload_end], check_code).to_le_bytes();
    frame[payload_end..].copy_from_slice(&value[..width]);
}

fn verify_check_code(frame: &[u8], check_code: CheckCodeType) -> bool {
    let width = check_code.bytes();
    if width == 0 {
        return true;
    }
    let payload_end = frame.len() - width;
    let expected = calculate_check_code(&frame[..payload_end], check_code).to_le_bytes();
    let mut difference = 0;
    for (received, calculated) in frame[payload_end..].iter().zip(expected.iter()) {
        difference |= received ^ calculated;
    }
    difference == 0
}

fn crc_msb(input: &[u8], width: u32, polynomial: u32, initial: u32, xor_out: u32) -> u32 {
    let top = 1_u32 << (width - 1);
    let mask = if width == 32 {
        u32::MAX
    } else {
        (1_u32 << width) - 1
    };
    let mut crc = initial;
    for byte in input {
        crc ^= u32::from(*byte) << (width - 8);
        for _ in 0..8 {
            crc = if crc & top == 0 {
                crc << 1
            } else {
                (crc << 1) ^ polynomial
            } & mask;
        }
    }
    (crc ^ xor_out) & mask
}

fn crc_lsb(input: &[u8], width: u32, polynomial: u32, initial: u32, xor_out: u32) -> u32 {
    let mask = if width == 32 {
        u32::MAX
    } else {
        (1_u32 << width) - 1
    };
    let mut crc = initial;
    for byte in input {
        crc ^= u32::from(*byte);
        for _ in 0..8 {
            crc = if crc & 1 == 0 {
                crc >> 1
            } else {
                (crc >> 1) ^ polynomial
            };
        }
    }
    (crc ^ xor_out) & mask
}
