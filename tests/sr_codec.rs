use rasta_rust::codec::sr::{
    decode, validate_data_field, CodecError, ConnectionData, DataFieldMessages, DisconnectReason,
    MessageType, ProtocolVersion, SafetyCodeLength, SrBody, SrCodecConfig, SrHeader, SrMessage,
    MAX_APPLICATION_PAYLOAD, MAX_DATA_FIELD_LEN, MAX_SR_PDU_LEN,
};

fn header() -> SrHeader {
    SrHeader {
        receiver_id: 0x0102_0304,
        sender_id: 0x1112_1314,
        sequence_number: 0x2122_2324,
        confirmed_sequence_number: 0x3132_3334,
        timestamp: 0x4142_4344,
        confirmed_timestamp: 0x5152_5354,
    }
}

fn version() -> ProtocolVersion {
    ProtocolVersion::new(*b"0303").expect("test version is valid")
}

fn encode(message: &SrMessage<'_>, config: SrCodecConfig) -> Vec<u8> {
    let mut output = vec![0xa5; MAX_SR_PDU_LEN];
    let written = message
        .encode_into(config, &mut output)
        .expect("test message encodes");
    output.truncate(written);
    output
}

#[test]
fn connection_request_has_the_exact_little_endian_layout() {
    let config = SrCodecConfig::with_default_limits(SafetyCodeLength::None);
    let message = SrMessage {
        header: header(),
        body: SrBody::ConnectionRequest(ConnectionData::new(version(), 10)),
        safety_code: &[],
    };

    let bytes = encode(&message, config);
    assert_eq!(bytes.len(), 42);
    assert_eq!(&bytes[0..4], &[0x2a, 0x00, 0x38, 0x18]);
    assert_eq!(&bytes[4..8], &[0x04, 0x03, 0x02, 0x01]);
    assert_eq!(&bytes[8..12], &[0x14, 0x13, 0x12, 0x11]);
    assert_eq!(&bytes[12..16], &[0x24, 0x23, 0x22, 0x21]);
    assert_eq!(&bytes[16..20], &[0x34, 0x33, 0x32, 0x31]);
    assert_eq!(&bytes[20..24], &[0x44, 0x43, 0x42, 0x41]);
    assert_eq!(&bytes[24..28], &[0x54, 0x53, 0x52, 0x51]);
    assert_eq!(&bytes[28..34], b"0303\x0a\x00");
    assert_eq!(&bytes[34..42], &[0; 8]);
    assert_eq!(decode(&bytes, config), Ok(message));
}

#[test]
fn every_message_type_round_trips() {
    let config = SrCodecConfig::with_default_limits(SafetyCodeLength::None);
    let connection = ConnectionData::new(version(), 2);
    let bodies = [
        SrBody::ConnectionRequest(connection),
        SrBody::ConnectionResponse(connection),
        SrBody::RetransmissionRequest,
        SrBody::RetransmissionResponse,
        SrBody::DisconnectionRequest {
            detailed_reason: 0x1234,
            reason: DisconnectReason::RetransmissionFailed,
        },
        SrBody::Heartbeat,
        SrBody::Data(b"\x07\x00payload"),
        SrBody::Data(b"\x02\x00ab\x01\x00c"),
        SrBody::RetransmittedData(b"\x05\x00retry"),
    ];

    for body in bodies {
        let message = SrMessage {
            header: header(),
            body,
            safety_code: &[],
        };
        let bytes = encode(&message, config);
        assert_eq!(decode(&bytes, config), Ok(message));
    }
}

#[test]
fn opaque_safety_code_is_preserved_at_configured_width() {
    let config = SrCodecConfig::with_default_limits(SafetyCodeLength::LowerMd4);
    let safety_code = *b"12345678";
    let message = SrMessage {
        header: header(),
        body: SrBody::Data(b"\x03\x00abc"),
        safety_code: &safety_code,
    };

    let bytes = encode(&message, config);
    assert_eq!(bytes.len(), 41);
    assert_eq!(&bytes[28..33], &[3, 0, b'a', b'b', b'c']);
    assert_eq!(&bytes[33..], &safety_code);
    assert_eq!(decode(&bytes, config), Ok(message));
}

#[test]
fn rejects_invalid_lengths_before_indexing() {
    let config = SrCodecConfig::with_default_limits(SafetyCodeLength::None);
    assert_eq!(
        decode(&[0, 0, 0], config),
        Err(CodecError::InputTooShort {
            minimum: 4,
            actual: 3
        })
    );

    let mut heartbeat = encode(
        &SrMessage {
            header: header(),
            body: SrBody::Heartbeat,
            safety_code: &[],
        },
        config,
    );
    heartbeat[0] = 27;
    assert_eq!(
        decode(&heartbeat, config),
        Err(CodecError::DeclaredLengthMismatch {
            declared: 27,
            actual: 28
        })
    );
}

#[test]
fn rejects_unknown_type_and_disconnect_reason() {
    let config = SrCodecConfig::with_default_limits(SafetyCodeLength::None);
    let mut heartbeat = encode(
        &SrMessage {
            header: header(),
            body: SrBody::Heartbeat,
            safety_code: &[],
        },
        config,
    );
    heartbeat[2..4].copy_from_slice(&999_u16.to_le_bytes());
    assert_eq!(
        decode(&heartbeat, config),
        Err(CodecError::UnknownMessageType(999))
    );

    let mut disconnect = encode(
        &SrMessage {
            header: header(),
            body: SrBody::DisconnectionRequest {
                detailed_reason: 0,
                reason: DisconnectReason::Timeout,
            },
            safety_code: &[],
        },
        config,
    );
    disconnect[30..32].copy_from_slice(&99_u16.to_le_bytes());
    assert_eq!(
        decode(&disconnect, config),
        Err(CodecError::UnknownDisconnectReason(99))
    );
}

#[test]
fn data_field_structure_is_checked_on_encode_and_separately_after_decode() {
    let config = SrCodecConfig::with_default_limits(SafetyCodeLength::None);
    let data = |field| SrMessage {
        header: header(),
        body: SrBody::Data(field),
        safety_code: &[],
    };

    // A malformed field is never encoded.
    for (field, error) in [
        (&b""[..], CodecError::EmptyDataField),
        (b"\x00\x00", CodecError::EmptyApplicationMessage),
        (
            b"\x01\x00a\x00",
            CodecError::DataFieldTruncated { remaining: 1 },
        ),
        (
            b"\x04\x00abc",
            CodecError::PayloadLengthMismatch {
                declared: 4,
                actual: 3,
            },
        ),
    ] {
        assert_eq!(data(field).encoded_len(config), Err(error));
    }

    // Decoding checks only the field's size, so that the safety code is
    // checked before the field's structure.
    let mut bytes = encode(&data(b"\x03\x00abc"), config);
    bytes[28..30].copy_from_slice(&4_u16.to_le_bytes());
    let decoded = decode(&bytes, config).expect("size is valid");
    let SrBody::Data(field) = decoded.body else {
        panic!("expected data, got {decoded:?}");
    };
    assert_eq!(
        validate_data_field(field, config),
        Err(CodecError::PayloadLengthMismatch {
            declared: 4,
            actual: 3
        })
    );

    let mut header_only = encode(&data(b"\x01\x00a"), config);
    header_only.truncate(30);
    header_only[..2].copy_from_slice(&30_u16.to_le_bytes());
    assert_eq!(
        decode(&header_only, config),
        Err(CodecError::BodyLengthMismatch {
            message_type: MessageType::Data,
            expected: 31,
            actual: 30
        })
    );
}

#[test]
fn packed_messages_are_iterated_in_order() {
    let config = SrCodecConfig::with_default_limits(SafetyCodeLength::None);
    let field = b"\x01\x00a\x03\x00bcd\x02\x00ef";
    assert_eq!(validate_data_field(field, config), Ok(3));
    let messages: Vec<_> = DataFieldMessages::new(field).collect();
    assert_eq!(messages, [Ok(&b"a"[..]), Ok(b"bcd"), Ok(b"ef")]);

    // Iteration stops after the first error.
    let messages: Vec<_> = DataFieldMessages::new(b"\x01\x00a\x05\x00b\x01\x00c").collect();
    assert_eq!(
        messages,
        [
            Ok(&b"a"[..]),
            Err(CodecError::PayloadLengthMismatch {
                declared: 5,
                actual: 4
            })
        ]
    );
}

#[test]
fn data_field_and_message_sizes_are_bounded() {
    let config = SrCodecConfig::with_default_limits(SafetyCodeLength::None);
    let mut largest = vec![0x5a; MAX_DATA_FIELD_LEN];
    largest[..2].copy_from_slice(&1_055_u16.to_le_bytes());
    let message = SrMessage {
        header: header(),
        body: SrBody::Data(&largest),
        safety_code: &[],
    };
    let bytes = encode(&message, config);
    assert_eq!(bytes.len(), MAX_SR_PDU_LEN - 16);
    assert_eq!(decode(&bytes, config), Ok(message));

    let mut oversized = largest.clone();
    oversized.push(0);
    assert_eq!(
        SrMessage {
            body: SrBody::Data(&oversized),
            ..message
        }
        .encoded_len(config),
        Err(CodecError::PayloadTooLarge {
            maximum: 1_057,
            actual: 1_058
        })
    );

    // A lower configured maximum bounds the whole field: one message of that
    // size, or several smaller ones with their length prefixes.
    let small = SrCodecConfig::new(SafetyCodeLength::None, 4, 2, 20).expect("valid limits");
    assert_eq!(validate_data_field(b"\x04\x00abcd", small), Ok(1));
    assert_eq!(validate_data_field(b"\x01\x00a\x01\x00b", small), Ok(2));
    assert_eq!(
        validate_data_field(b"\x02\x00ab\x01\x00c", small),
        Err(CodecError::PayloadTooLarge {
            maximum: 6,
            actual: 7
        })
    );
    assert_eq!(
        validate_data_field(b"\x05\x00abcde", small),
        Err(CodecError::PayloadTooLarge {
            maximum: 6,
            actual: 7
        })
    );
}

#[test]
fn rejects_invalid_version_n_send_max_and_safety_width() {
    assert_eq!(
        ProtocolVersion::new(*b"03x3"),
        Err(CodecError::InvalidProtocolVersion(*b"03x3"))
    );

    let no_safety = SrCodecConfig::with_default_limits(SafetyCodeLength::None);
    let connection = SrMessage {
        header: header(),
        body: SrBody::ConnectionRequest(ConnectionData::new(version(), 21)),
        safety_code: &[],
    };
    assert_eq!(
        connection.encoded_len(no_safety),
        Err(CodecError::InvalidNSendMax {
            minimum: 2,
            maximum: 20,
            actual: 21
        })
    );

    let lower_md4 = SrCodecConfig::with_default_limits(SafetyCodeLength::LowerMd4);
    let heartbeat = SrMessage {
        header: header(),
        body: SrBody::Heartbeat,
        safety_code: &[],
    };
    assert_eq!(
        heartbeat.encoded_len(lower_md4),
        Err(CodecError::SafetyCodeLengthMismatch {
            expected: 8,
            actual: 0
        })
    );
}

#[test]
fn encoding_is_atomic_when_output_is_too_small() {
    let config = SrCodecConfig::with_default_limits(SafetyCodeLength::None);
    let message = SrMessage {
        header: header(),
        body: SrBody::Heartbeat,
        safety_code: &[],
    };
    let mut output = [0xa5; 27];
    assert_eq!(
        message.encode_into(config, &mut output),
        Err(CodecError::OutputTooSmall {
            required: 28,
            actual: 27
        })
    );
    assert_eq!(output, [0xa5; 27]);
}

#[test]
fn message_type_values_match_observation() {
    assert_eq!(MessageType::ConnectionRequest as u16, 6_200);
    assert_eq!(MessageType::ConnectionResponse as u16, 6_201);
    assert_eq!(MessageType::RetransmissionRequest as u16, 6_212);
    assert_eq!(MessageType::RetransmissionResponse as u16, 6_213);
    assert_eq!(MessageType::DisconnectionRequest as u16, 6_216);
    assert_eq!(MessageType::Heartbeat as u16, 6_220);
    assert_eq!(MessageType::Data as u16, 6_240);
    assert_eq!(MessageType::RetransmittedData as u16, 6_241);
}

#[test]
fn codec_limits_must_fit_the_fixed_buffers_and_a_usable_window() {
    let new =
        |max_payload, min, max| SrCodecConfig::new(SafetyCodeLength::None, max_payload, min, max);
    assert!(new(MAX_APPLICATION_PAYLOAD, 1, 20).is_ok());
    for (max_payload, min, max) in [
        (MAX_APPLICATION_PAYLOAD + 1, 2, 20),
        (usize::MAX, 2, 20),
        (MAX_APPLICATION_PAYLOAD, 0, 20),
        (MAX_APPLICATION_PAYLOAD, 0, 0),
        (MAX_APPLICATION_PAYLOAD, 3, 2),
    ] {
        assert_eq!(
            new(max_payload, min, max),
            Err(CodecError::InvalidConfiguration)
        );
    }
}
