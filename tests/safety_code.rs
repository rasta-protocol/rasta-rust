use rasta_rust::codec::{
    safety_code::{
        calculate_md4, verify_md4_safety_code, write_md4_safety_code, Md4InitialValue,
        SafetyCodeError, SafetyCodeLength,
    },
    sr::{decode_and_verify_md4, CodecError, SrBody, SrCodecConfig, SrHeader, SrMessage},
};

// HEADER_ONLY_MESSAGE, its standard/custom MD4 digests and the custom IV in
// header_only_message_matches_standard_and_custom_initial_values come from
// SBB sbb-rasta-stack's unit_test_srmd4.cc (MIT), revision
// 81f2a8fa11285abcf83d2486e15d11081bf74224; see docs/provenance.md.
// The upstream fixture cites DIN VDE V 0831-200 Annex A.
const HEADER_ONLY_MESSAGE: [u8; 28] = [
    0x24, 0x00, 0x4c, 0x18, 0x3f, 0xb4, 0x96, 0x00, 0xce, 0xca, 0x23, 0x00, 0x56, 0x44, 0x33, 0x22,
    0x66, 0x55, 0x44, 0x33, 0x57, 0x01, 0x00, 0x00, 0xcb, 0x00, 0x00, 0x00,
];

const HEADER_ONLY_STANDARD_MD4: [u8; 16] = [
    0x83, 0xf0, 0xd0, 0x52, 0x40, 0x6b, 0xf4, 0x92, 0xf8, 0x9f, 0x8d, 0x1e, 0x9b, 0x89, 0xc9, 0x8d,
];

fn decode_hex(hex: &str) -> [u8; 16] {
    assert_eq!(hex.len(), 32);
    let mut bytes = [0_u8; 16];
    for (output, pair) in bytes.iter_mut().zip(hex.as_bytes().chunks_exact(2)) {
        let pair = core::str::from_utf8(pair).expect("test vector is ASCII");
        *output = u8::from_str_radix(pair, 16).expect("test vector contains hexadecimal bytes");
    }
    bytes
}

#[test]
fn standard_md4_known_answer_vectors_match() {
    let vectors = [
        ("", "31d6cfe0d16ae931b73c59d7e0c089c0"),
        ("a", "bde52cb31de33e46245e05fbdbd6fb24"),
        ("abc", "a448017aaf21d8525fc10ae87aa6729d"),
        ("message digest", "d9130a8164549fe818874806e1c7014b"),
        (
            "abcdefghijklmnopqrstuvwxyz",
            "d79e1c308aa5bbcdeea8ed63df412da9",
        ),
        (
            "12345678901234567890123456789012345678901234567890123456789012345678901234567890",
            "e33b4ddc9c38f2199c3e7b164fcc0536",
        ),
    ];

    for (input, expected) in vectors {
        assert_eq!(
            calculate_md4(input.as_bytes(), Md4InitialValue::STANDARD),
            Ok(decode_hex(expected))
        );
    }
}

#[test]
fn header_only_message_matches_standard_and_custom_initial_values() {
    assert_eq!(
        calculate_md4(&HEADER_ONLY_MESSAGE, Md4InitialValue::STANDARD),
        Ok(HEADER_ONLY_STANDARD_MD4)
    );

    let custom = Md4InitialValue::new(0xafb1_6782, 0x304c_59de, 0x98ba_dcfe, 0x1032_5476);
    let expected = [
        0x93, 0x9f, 0x1c, 0x86, 0x59, 0xcf, 0xf5, 0x03, 0x95, 0x3c, 0x4e, 0xd3, 0x25, 0x96, 0x89,
        0x5d,
    ];
    assert_eq!(calculate_md4(&HEADER_ONLY_MESSAGE, custom), Ok(expected));
}

/// Padding-boundary vectors with a custom initial value. The expected digests
/// come from the pinned EULYNX `rastamd4`; see `docs/differential-testing.md`.
#[test]
fn padding_boundary_vectors_match_known_answers() {
    let initial_value = Md4InitialValue::new(0x0123_4567, 0x89ab_cdef, 0xfedc_ba98, 0x7654_3210);
    let mut data = [0_u8; 128];
    for (index, byte) in data.iter_mut().enumerate() {
        *byte = u8::try_from((7 * index + 3) % 256).expect("reduced modulo 256");
    }
    let vectors = [
        (55, "8ee1105d6dac9e84ea71cb7f60a1d7ca"),
        (56, "f374dbc2b672a87f0868a968749a12ff"),
        (57, "b4f5aef02d4c7ce289e9b8818300e55b"),
        (63, "c92fe70c1693f0a232dd1d2466be59f6"),
        (64, "446c082b1ee8a2952eb7c785a2a21deb"),
        (119, "24bc450ca650136b876ed79701836f25"),
        (120, "c2c6a2ec30f64ade0e7e2cd20be0965a"),
        (127, "052aed9193cb102f9eeecbccd03d21f2"),
        (128, "de798e9f4230dac384a10fbc7ce4e235"),
    ];
    for (length, expected) in vectors {
        assert_eq!(
            calculate_md4(&data[..length], initial_value),
            Ok(decode_hex(expected)),
            "length {length}"
        );
    }
}

#[test]
fn codec_emits_and_verifies_a_lower_md4_pdu() {
    let config = SrCodecConfig::with_default_limits(SafetyCodeLength::LowerMd4);
    let message = SrMessage {
        header: SrHeader {
            receiver_id: 0x0096_b43f,
            sender_id: 0x0023_cace,
            sequence_number: 0x2233_4456,
            confirmed_sequence_number: 0x3344_5566,
            timestamp: 0x0000_0157,
            confirmed_timestamp: 0x0000_00cb,
        },
        body: SrBody::Heartbeat,
        safety_code: &[],
    };
    let mut encoded = [0_u8; 36];

    assert_eq!(
        message.encode_with_md4(config, Md4InitialValue::STANDARD, &mut encoded),
        Ok(36)
    );
    assert_eq!(&encoded[..28], &HEADER_ONLY_MESSAGE);
    assert_eq!(&encoded[28..], &HEADER_ONLY_STANDARD_MD4[..8]);

    let decoded = decode_and_verify_md4(&encoded, config, Md4InitialValue::STANDARD)
        .expect("header-only PDU safety code verifies");
    assert_eq!(decoded.header, message.header);
    assert_eq!(decoded.body, SrBody::Heartbeat);
    assert_eq!(decoded.safety_code, &HEADER_ONLY_STANDARD_MD4[..8]);
}

#[test]
fn codec_rejects_a_tampered_but_structurally_valid_pdu() {
    let config = SrCodecConfig::with_default_limits(SafetyCodeLength::FullMd4);
    let message = SrMessage {
        header: SrHeader::default(),
        body: SrBody::Heartbeat,
        safety_code: &[],
    };
    let mut encoded = [0_u8; 44];
    message
        .encode_with_md4(config, Md4InitialValue::STANDARD, &mut encoded)
        .expect("test message encodes");
    encoded[4] ^= 0x01;

    assert_eq!(
        decode_and_verify_md4(&encoded, config, Md4InitialValue::STANDARD),
        Err(CodecError::SafetyCodeFailure(SafetyCodeError::Mismatch))
    );
}

#[test]
fn low_level_safety_code_api_checks_boundaries_and_tampering() {
    let mut short = [0_u8; 7];
    assert_eq!(
        write_md4_safety_code(
            &mut short,
            SafetyCodeLength::LowerMd4,
            Md4InitialValue::STANDARD
        ),
        Err(SafetyCodeError::InputTooShort {
            minimum: 8,
            actual: 7
        })
    );

    let mut pdu = [0_u8; 24];
    pdu[..8].copy_from_slice(b"message!");
    write_md4_safety_code(
        &mut pdu,
        SafetyCodeLength::FullMd4,
        Md4InitialValue::STANDARD,
    )
    .expect("bounded PDU can be sealed");
    verify_md4_safety_code(&pdu, SafetyCodeLength::FullMd4, Md4InitialValue::STANDARD)
        .expect("fresh safety code verifies");

    pdu[23] ^= 0x80;
    assert_eq!(
        verify_md4_safety_code(&pdu, SafetyCodeLength::FullMd4, Md4InitialValue::STANDARD),
        Err(SafetyCodeError::Mismatch)
    );
}
