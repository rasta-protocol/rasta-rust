use rasta_rust::{
    codec::{
        safety_code::{Md4InitialValue, SafetyCodeLength},
        sr::{decode, decode_and_verify_md4, SrCodecConfig, MAX_SR_PDU_LEN},
    },
    redundancy::codec::{decode as decode_redundancy, CheckCodeType, MAX_REDUNDANCY_FRAME_LEN},
};

fn next_byte(state: &mut u32) -> u8 {
    *state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
    state.to_le_bytes()[3]
}

#[test]
fn all_parsers_reject_or_accept_arbitrary_bytes_without_panicking() {
    let profiles = [
        SafetyCodeLength::None,
        SafetyCodeLength::LowerMd4,
        SafetyCodeLength::FullMd4,
    ];
    let check_codes = [
        CheckCodeType::A,
        CheckCodeType::B,
        CheckCodeType::C,
        CheckCodeType::D,
        CheckCodeType::E,
    ];
    let mut bytes = [0_u8; MAX_REDUNDANCY_FRAME_LEN + 1];
    let mut state = 0x52a5_7e11;

    for length in 0..=bytes.len() {
        for byte in &mut bytes[..length] {
            *byte = next_byte(&mut state);
        }
        for safety_code in profiles {
            let config = SrCodecConfig::with_default_limits(safety_code);
            let input = &bytes[..length.min(MAX_SR_PDU_LEN + 1)];
            let _ = decode(input, config);
            let _ = decode_and_verify_md4(input, config, Md4InitialValue::STANDARD);
        }
        for check_code in check_codes {
            let _ = decode_redundancy(&bytes[..length], check_code);
        }
    }
}
