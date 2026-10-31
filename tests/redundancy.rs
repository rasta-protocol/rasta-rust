use rasta_rust::redundancy::{
    codec::{
        calculate_check_code, decode, CheckCodeType, RedundancyCodecError, RedundancyFrame,
        MAX_REDUNDANCY_FRAME_LEN,
    },
    engine::{
        RedundancyConfig, RedundancyEffect, RedundancyEngine, RedundancyEvent, RedundancyInput,
        RedundancyPayload, RedundancyState,
    },
};

fn payload(byte: u8) -> RedundancyPayload {
    RedundancyPayload::new(&[byte; 28]).unwrap()
}

#[allow(clippy::large_types_passed_by_value)]
fn step(
    engine: &mut RedundancyEngine,
    now: u32,
    event: RedundancyEvent,
) -> rasta_rust::redundancy::engine::RedundancyTransition {
    engine.step(RedundancyInput { now, event }).unwrap()
}

#[test]
fn known_reflected_crc_profiles_match_standard_vectors() {
    assert_eq!(
        calculate_check_code(b"123456789", CheckCodeType::C),
        0xe306_9283
    );
    assert_eq!(calculate_check_code(b"123456789", CheckCodeType::D), 0x2189);
    assert_eq!(calculate_check_code(b"123456789", CheckCodeType::E), 0xbb3d);
}

#[test]
fn every_check_code_profile_round_trips_and_rejects_tampering() {
    for check_code in [
        CheckCodeType::A,
        CheckCodeType::B,
        CheckCodeType::C,
        CheckCodeType::D,
        CheckCodeType::E,
    ] {
        let wrapped = [0x5a; 28];
        let frame = RedundancyFrame {
            sequence_number: 0x1020_3040,
            payload: &wrapped,
        };
        let mut output = [0_u8; MAX_REDUNDANCY_FRAME_LEN];
        let written = frame.encode_into(check_code, &mut output).unwrap();
        assert_eq!(decode(&output[..written], check_code), Ok(frame));

        if check_code != CheckCodeType::A {
            output[8] ^= 1;
            assert_eq!(
                decode(&output[..written], check_code),
                Err(RedundancyCodecError::CheckCodeMismatch)
            );
        }
    }
}

#[test]
fn send_is_duplicated_across_channels_with_one_sequence_number() {
    let config = RedundancyConfig::new(50, 4, 2, 200).unwrap();
    let mut engine = RedundancyEngine::new(config);
    step(&mut engine, 0, RedundancyEvent::Open);

    let transition = step(&mut engine, 1, RedundancyEvent::Send(payload(1)));
    assert_eq!(transition.effects.len(), 2);
    assert_eq!(
        transition.effects.get(0),
        Some(&RedundancyEffect::Transmit {
            channel: 0,
            sequence_number: 0,
            payload: payload(1),
        })
    );
    assert_eq!(
        transition.effects.get(1),
        Some(&RedundancyEffect::Transmit {
            channel: 1,
            sequence_number: 0,
            payload: payload(1),
        })
    );
    assert_eq!(transition.current.next_transmit_sequence, 1);
}

#[test]
fn duplicates_are_suppressed_and_future_frames_are_drained_in_order() {
    let config = RedundancyConfig::new(50, 4, 2, 200).unwrap();
    let mut engine = RedundancyEngine::new(config);
    step(&mut engine, 0, RedundancyEvent::Open);

    let first = step(
        &mut engine,
        1,
        RedundancyEvent::FrameReceived {
            channel: 0,
            sequence_number: 0,
            payload: payload(0),
        },
    );
    assert_eq!(
        first.effects.get(0),
        Some(&RedundancyEffect::Deliver(payload(0)))
    );

    let duplicate = step(
        &mut engine,
        2,
        RedundancyEvent::FrameReceived {
            channel: 1,
            sequence_number: 0,
            payload: payload(0),
        },
    );
    assert!(duplicate.effects.is_empty());
    assert_eq!(duplicate.current.diagnostics.duplicates, 1);

    let deferred = step(
        &mut engine,
        3,
        RedundancyEvent::FrameReceived {
            channel: 1,
            sequence_number: 2,
            payload: payload(2),
        },
    );
    assert!(deferred.effects.is_empty());
    assert_eq!(deferred.current.deferred_messages, 1);

    let drained = step(
        &mut engine,
        4,
        RedundancyEvent::FrameReceived {
            channel: 0,
            sequence_number: 1,
            payload: payload(1),
        },
    );
    assert_eq!(
        drained.effects.iter().copied().collect::<Vec<_>>(),
        vec![
            RedundancyEffect::Deliver(payload(1)),
            RedundancyEffect::Deliver(payload(2))
        ]
    );
    assert_eq!(drained.current.next_receive_sequence, 3);
    assert_eq!(drained.current.deferred_messages, 0);
}

#[test]
fn t_seq_skips_a_missing_gap_across_time_wraparound() {
    let config = RedundancyConfig::new(50, 4, 1, 200).unwrap();
    let mut engine = RedundancyEngine::new(config);
    step(&mut engine, 0, RedundancyEvent::Open);
    step(
        &mut engine,
        u32::MAX - 20,
        RedundancyEvent::FrameReceived {
            channel: 0,
            sequence_number: 0,
            payload: payload(0),
        },
    );
    step(
        &mut engine,
        u32::MAX - 10,
        RedundancyEvent::FrameReceived {
            channel: 0,
            sequence_number: 2,
            payload: payload(2),
        },
    );

    let before = step(&mut engine, 20, RedundancyEvent::Poll);
    assert!(before.effects.is_empty());
    let after = step(&mut engine, 50, RedundancyEvent::Poll);
    assert_eq!(
        after.effects.get(0),
        Some(&RedundancyEffect::Deliver(payload(2)))
    );
    assert_eq!(after.current.next_receive_sequence, 3);
    assert_eq!(after.current.diagnostics.sequence_timeouts, 1);

    let closed = step(&mut engine, 51, RedundancyEvent::Close);
    assert_eq!(closed.current.state, RedundancyState::Closed);
}

#[test]
fn check_code_b_matches_an_independent_known_answer() {
    // OBS-SBB-RL-005; pinned SBB revision 81f2a8fa11285abcf83d2486e15d11081bf74224,
    // unit_test_redcrc.cc, redcrcTest004: 1109 bytes repeating 0..=254.
    // Expected value is the external fixture, not computed by the Rust codec.
    let mut input = [0_u8; 1109];
    for (byte, value) in input.iter_mut().zip((0_u8..=254).cycle()) {
        *byte = value;
    }
    assert_eq!(
        calculate_check_code(&input, CheckCodeType::B),
        2_448_192_889
    );
}

#[test]
fn full_defer_queue_discards_and_counts_frames_then_recovers_after_drain() {
    for capacity in 4..=10 {
        let mut engine =
            RedundancyEngine::new(RedundancyConfig::new(50, capacity, 2, 200).unwrap());
        step(&mut engine, 0, RedundancyEvent::Open);
        let frame = |sequence_number| RedundancyEvent::FrameReceived {
            channel: 0,
            sequence_number,
            payload: payload(1),
        };
        step(&mut engine, 1, frame(0));
        for sequence in 2..=u32::from(capacity) + 1 {
            step(&mut engine, 2, frame(sequence));
        }
        let mut expected = engine.snapshot();
        expected.diagnostics.defer_queue_full += 1;
        let overflow = frame(u32::from(capacity) + 2);
        let discarded = step(&mut engine, 3, overflow);
        assert!(discarded.effects.is_empty());
        assert_eq!(discarded.current, expected);
        assert_eq!(discarded.current.deferred_messages, capacity);
        // Duplicates do not need a free queue slot.
        assert!(step(&mut engine, 4, frame(2)).effects.is_empty());
        let drained = step(&mut engine, 5, frame(1));
        assert_eq!(drained.effects.len(), usize::from(capacity) + 1);
        assert_eq!(drained.current.deferred_messages, 0);
        assert_eq!(step(&mut engine, 6, overflow).effects.len(), 1);
    }
}

#[test]
fn diagnosis_window_must_fit_its_storage() {
    use rasta_rust::redundancy::engine::{RedundancyError, MAX_DIAGNOSIS_WINDOW};
    assert!(RedundancyConfig::new(50, 4, 2, 1).is_ok());
    assert!(RedundancyConfig::new(50, 4, 2, MAX_DIAGNOSIS_WINDOW).is_ok());
    for window in [0, MAX_DIAGNOSIS_WINDOW + 1] {
        assert_eq!(
            RedundancyConfig::new(50, 4, 2, window),
            Err(RedundancyError::InvalidDiagnosisWindow(window))
        );
    }
}

#[test]
fn channel_diagnostics_report_late_and_missing_copies_per_window() {
    use rasta_rust::redundancy::engine::TransportChannelDiagnostics;
    let mut engine = RedundancyEngine::new(RedundancyConfig::new(50, 4, 2, 3).unwrap());
    step(&mut engine, 0, RedundancyEvent::Open);
    let frame = |channel, sequence_number| RedundancyEvent::FrameReceived {
        channel,
        sequence_number,
        payload: payload(1),
    };
    step(&mut engine, 100, frame(0, 0));
    step(&mut engine, 105, frame(1, 0)); // 5 ms behind
    step(&mut engine, 110, frame(0, 1));
    step(&mut engine, 120, frame(0, 2)); // never arrives on channel 1
    step(&mut engine, 170, frame(1, 1)); // 60 ms behind: later than T_seq

    // The fourth distinct frame completes the window of three.
    let fourth = step(&mut engine, 180, frame(0, 3));
    let reports: Vec<_> = fourth
        .effects
        .iter()
        .filter_map(|effect| match effect {
            RedundancyEffect::Diagnostics {
                channel,
                diagnostics,
            } => Some((*channel, *diagnostics)),
            _ => None,
        })
        .collect();
    assert_eq!(
        reports,
        [
            (
                0,
                TransportChannelDiagnostics {
                    frames: 3,
                    missed: 0,
                    drift: 0,
                    drift_squared: 0,
                }
            ),
            (
                1,
                TransportChannelDiagnostics {
                    frames: 3,
                    missed: 2,
                    drift: 5,
                    drift_squared: 25,
                }
            ),
        ]
    );
    assert!(matches!(
        fourth.effects.iter().last(),
        Some(RedundancyEffect::Deliver(_))
    ));
}
