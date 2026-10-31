# Reference provenance

Public implementations are used only as non-normative behavioural references
and future differential-test oracles. No source has been copied into the Rust
implementation. All three repositories below were observed as MIT-licensed at
the pinned revisions.

| Reference | Pinned revision | Retrieved/recorded | Role |
|---|---|---|---|
| [SBB RaSTA stack](https://github.com/SchweizerischeBundesbahnen/sbb-rasta-stack) | `81f2a8fa11285abcf83d2486e15d11081bf74224` | 2026-08-20 | Primary wire-layout and validation reference |
| [Railway-CCS rasta-protocol](https://github.com/Railway-CCS/rasta-protocol) | `aed9d83274eea0151aa12c9d84cb77bf1055d210` | 2026-08-20 | Comparison target |
| [EULYNX Live librasta](https://github.com/eulynx-live/librasta) | `be81878aa56f19775c64065983f65dea4f1db9c8` | 2026-08-20 | Comparison target (fork of Railway-CCS) |

The EULYNX Live repository is a fork of the Railway-CCS repository (both
histories start with the same initial commit), so the two are not independent
evidence where their code is shared.

The MD4 known-answer fixtures are checked independently against the SBB
`unit_test_srmd4.cc` example-message fixture and EULYNX Live `rastamd4_test.c`. 
Standard MD4 vectors are used to exercise padding boundaries;
the implementation itself remains original Rust code with no copied C source.

The connection-state observations are recorded from the pinned SBB
`srstm_sr_state_machine.c`, `srcor_sr_core.c`, `srsend_sr_send_buffer.c`, and
their unit tests. The Rust engine models the observed behaviour as explicit 
event/effect transitions; it is not a source translation.

The CRC-B known-answer test (OBS-SBB-RL-005) uses the independent fixture in
SBB `source/modules/rasta_redundancy/tests/unit_tests/test_redcrc/unit_test_redcrc.cc`,
`redcrcTest004VerifyCalculateCrcFunction`, at the pinned revision above:
1109 bytes repeating `0..=254` have numeric CRC `2448192889`. The Rust test
constructs that byte sequence and compares against the recorded constant; it
does not derive its expectation from the Rust encoder or CRC function.

## Evidence handling rules

- Record a commit hash before deriving a test or observation.
- Link each derived behaviour to an `OBS-*` identifier.
- Prefer black-box differential tests over source translation.
- Never describe agreement between implementations as proof of conformance.
- When the official specification becomes available, create a separate
  normative requirements baseline and reconcile every observation explicitly.
