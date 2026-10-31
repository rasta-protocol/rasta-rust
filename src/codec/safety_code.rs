//! `RaSTA` safety-code calculation and verification primitives.
//!
//! The SR safety code is MD4 over the complete encoded PDU except for the
//! trailing safety-code bytes. The length field already includes those
//! trailing bytes. A configurable four-word initial value is used instead of
//! assuming the standard MD4 initial value.
//!
//! MD4 is implemented here for protocol interoperability. It is not a modern
//! cryptographic authentication mechanism and must not be reused as one.

use core::fmt;

const MD4_BLOCK_LEN: usize = 64;
const MD4_DIGEST_LEN: usize = 16;

/// The configured width of the trailing SR safety code.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SafetyCodeLength {
    /// No safety code.
    ///
    /// Without a safety code, messages from different `RaSTA` networks are
    /// not told apart by their initial values, and corruption is detected
    /// only by the redundancy layer's check code, if one is configured.
    None,
    /// The first eight bytes of the MD4 result.
    LowerMd4,
    /// The complete sixteen-byte MD4 result.
    FullMd4,
}

impl SafetyCodeLength {
    /// Returns the width in bytes.
    #[must_use]
    pub const fn bytes(self) -> usize {
        match self {
            Self::None => 0,
            Self::LowerMd4 => 8,
            Self::FullMd4 => MD4_DIGEST_LEN,
        }
    }
}

/// Four configurable initial state words for `RaSTA`'s MD4 calculation.
///
/// Each `RaSTA` network uses its own initial value: a message from an
/// endpoint in another network fails the safety-code check. Endpoint
/// identifiers are unique only within one network, so endpoints that must
/// not talk to each other need different initial values.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Md4InitialValue {
    /// Initial state word A.
    pub a: u32,
    /// Initial state word B.
    pub b: u32,
    /// Initial state word C.
    pub c: u32,
    /// Initial state word D.
    pub d: u32,
}

impl Md4InitialValue {
    /// The standard MD4 initial state.
    pub const STANDARD: Self = Self {
        a: 0x6745_2301,
        b: 0xefcd_ab89,
        c: 0x98ba_dcfe,
        d: 0x1032_5476,
    };

    /// Creates a configurable initial value from its four state words.
    #[must_use]
    pub const fn new(a: u32, b: u32, c: u32, d: u32) -> Self {
        Self { a, b, c, d }
    }
}

/// Safety-code calculation or verification errors.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SafetyCodeError {
    /// The input is shorter than its configured trailing safety code.
    InputTooShort {
        /// Minimum required input length.
        minimum: usize,
        /// Supplied input length.
        actual: usize,
    },
    /// The input bit length cannot be represented by MD4's 64-bit field.
    InputTooLong,
    /// The received safety code differs from the calculated value.
    Mismatch,
}

impl fmt::Display for SafetyCodeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InputTooShort { minimum, actual } => write!(
                formatter,
                "safety-coded PDU needs at least {minimum} bytes, got {actual}"
            ),
            Self::InputTooLong => formatter.write_str("input is too long for MD4"),
            Self::Mismatch => formatter.write_str("SR safety code mismatch"),
        }
    }
}

impl core::error::Error for SafetyCodeError {}

/// Calculates an MD4 digest with a configurable initial state.
///
/// # Errors
///
/// Returns [`SafetyCodeError::InputTooLong`] if the input bit length cannot be
/// represented in MD4's 64-bit padding field.
pub fn calculate_md4(
    input: &[u8],
    initial_value: Md4InitialValue,
) -> Result<[u8; MD4_DIGEST_LEN], SafetyCodeError> {
    let byte_length = u64::try_from(input.len()).map_err(|_| SafetyCodeError::InputTooLong)?;
    let bit_length = byte_length
        .checked_mul(8)
        .ok_or(SafetyCodeError::InputTooLong)?;

    let mut state = [
        initial_value.a,
        initial_value.b,
        initial_value.c,
        initial_value.d,
    ];
    let mut blocks = input.chunks_exact(MD4_BLOCK_LEN);
    for block in &mut blocks {
        let block: &[u8; MD4_BLOCK_LEN] = block
            .try_into()
            .map_err(|_| SafetyCodeError::InputTooLong)?;
        compress(&mut state, block);
    }

    let remainder = blocks.remainder();
    let padded_length = if remainder.len() < 56 { 64 } else { 128 };
    let mut padding = [0_u8; 2 * MD4_BLOCK_LEN];
    padding[..remainder.len()].copy_from_slice(remainder);
    padding[remainder.len()] = 0x80;
    padding[padded_length - 8..padded_length].copy_from_slice(&bit_length.to_le_bytes());

    for block in padding[..padded_length].chunks_exact(MD4_BLOCK_LEN) {
        let block: &[u8; MD4_BLOCK_LEN] = block
            .try_into()
            .map_err(|_| SafetyCodeError::InputTooLong)?;
        compress(&mut state, block);
    }

    let mut digest = [0_u8; MD4_DIGEST_LEN];
    for (word, output) in state.iter().zip(digest.chunks_exact_mut(4)) {
        output.copy_from_slice(&word.to_le_bytes());
    }
    Ok(digest)
}

/// Calculates and writes the configured trailing safety code in place.
///
/// `pdu` must already contain the final wire length and reserve the configured
/// number of trailing bytes. Those trailing bytes are excluded from the MD4
/// input and replaced with the calculated result.
///
/// # Errors
///
/// Returns an error if the PDU is shorter than the configured code or cannot be
/// represented by the MD4 length field.
pub fn write_md4_safety_code(
    pdu: &mut [u8],
    safety_code_length: SafetyCodeLength,
    initial_value: Md4InitialValue,
) -> Result<(), SafetyCodeError> {
    let code_length = safety_code_length.bytes();
    let message_length = checked_message_length(pdu.len(), code_length)?;
    if code_length == 0 {
        return Ok(());
    }

    let digest = calculate_md4(&pdu[..message_length], initial_value)?;
    pdu[message_length..].copy_from_slice(&digest[..code_length]);
    Ok(())
}

/// Verifies the configured trailing safety code.
///
/// The comparison accumulates differences across every code byte so a mismatch
/// does not return at the first differing position.
///
/// # Errors
///
/// Returns an error if the PDU is too short, too long for MD4, or contains a
/// safety code that differs from the calculated value.
pub fn verify_md4_safety_code(
    pdu: &[u8],
    safety_code_length: SafetyCodeLength,
    initial_value: Md4InitialValue,
) -> Result<(), SafetyCodeError> {
    let code_length = safety_code_length.bytes();
    let message_length = checked_message_length(pdu.len(), code_length)?;
    if code_length == 0 {
        return Ok(());
    }

    let digest = calculate_md4(&pdu[..message_length], initial_value)?;
    let mut difference = 0_u8;
    for (received, calculated) in pdu[message_length..]
        .iter()
        .zip(digest[..code_length].iter())
    {
        difference |= received ^ calculated;
    }

    if difference == 0 {
        Ok(())
    } else {
        Err(SafetyCodeError::Mismatch)
    }
}

fn checked_message_length(pdu_length: usize, code_length: usize) -> Result<usize, SafetyCodeError> {
    pdu_length
        .checked_sub(code_length)
        .ok_or(SafetyCodeError::InputTooShort {
            minimum: code_length,
            actual: pdu_length,
        })
}

fn compress(state: &mut [u32; 4], block: &[u8; MD4_BLOCK_LEN]) {
    const TARGET_ORDER: [usize; 4] = [0, 3, 2, 1];
    const ROUND_1_SHIFTS: [u32; 4] = [3, 7, 11, 19];
    const ROUND_2_WORDS: [usize; 16] = [0, 4, 8, 12, 1, 5, 9, 13, 2, 6, 10, 14, 3, 7, 11, 15];
    const ROUND_2_SHIFTS: [u32; 4] = [3, 5, 9, 13];
    const ROUND_3_WORDS: [usize; 16] = [0, 8, 4, 12, 2, 10, 6, 14, 1, 9, 5, 13, 3, 11, 7, 15];
    const ROUND_3_SHIFTS: [u32; 4] = [3, 9, 11, 15];

    let mut words = [0_u32; 16];
    for (word, bytes) in words.iter_mut().zip(block.chunks_exact(4)) {
        *word = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
    }

    let initial = *state;
    let mut work = initial;
    for index in 0..16 {
        apply_step(
            &mut work,
            TARGET_ORDER[index % 4],
            words[index],
            ROUND_1_SHIFTS[index % 4],
            0,
            round_f,
        );
    }
    for index in 0..16 {
        apply_step(
            &mut work,
            TARGET_ORDER[index % 4],
            words[ROUND_2_WORDS[index]],
            ROUND_2_SHIFTS[index % 4],
            0x5a82_7999,
            round_g,
        );
    }
    for index in 0..16 {
        apply_step(
            &mut work,
            TARGET_ORDER[index % 4],
            words[ROUND_3_WORDS[index]],
            ROUND_3_SHIFTS[index % 4],
            0x6ed9_eba1,
            round_h,
        );
    }

    for (state_word, (initial_word, work_word)) in
        state.iter_mut().zip(initial.iter().zip(work.iter()))
    {
        *state_word = initial_word.wrapping_add(*work_word);
    }
}

fn apply_step(
    state: &mut [u32; 4],
    target: usize,
    word: u32,
    shift: u32,
    constant: u32,
    function: fn(u32, u32, u32) -> u32,
) {
    let value = state[target]
        .wrapping_add(function(
            state[(target + 1) % 4],
            state[(target + 2) % 4],
            state[(target + 3) % 4],
        ))
        .wrapping_add(word)
        .wrapping_add(constant);
    state[target] = value.rotate_left(shift);
}

const fn round_f(x: u32, y: u32, z: u32) -> u32 {
    z ^ (x & (y ^ z))
}

const fn round_g(x: u32, y: u32, z: u32) -> u32 {
    (x & (y | z)) | (y & z)
}

const fn round_h(x: u32, y: u32, z: u32) -> u32 {
    x ^ y ^ z
}
