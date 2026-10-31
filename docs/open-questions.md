# Open questions

Each question keeps its identifier when it is answered. The date is when the
question was raised. Answers that rest on observations or project decisions
rather than the normative specification remain subject to Q-003.

## Open

- **Q-001** (2026-08-30): Which conformance target to pursue: the 2015
  profile, the 2025 draft profile, or both.
- **Q-003** (2026-08-30): Exact normative confirmation of the implemented
  `T_i`, retransmission, and RL algorithms when the licensed standard becomes
  available, including the project decisions in `adr/` and
  `state-machine-status.md`.
- **Q-004** (2026-08-30): Which buildable command-line wrappers should be
  maintained for each pinned C reference. The differential harness defines a
  stable JSON-lines contract, but repository-specific wrappers remain external
  to avoid importing C code or toolchains into the Rust package.
- **Q-005** (2026-08-30): Which two C stacks should be mandatory
  interoperability peers in CI once redistributable binaries or reproducible
  builds are available.
- **Q-006** (2026-08-30): Whether CI should install GoogleTest and CUnit to
  run the pinned references' upstream suites (SBB uses GoogleTest; Railway-CCS
  and EULYNX use CUnit).
- **Q-011** (2026-09-27): Which separate security layer, for example IPsec, a
  VPN, or network segmentation, to recommend and test with this
  implementation. The safety code detects random errors but does not
  authenticate messages.
- **Q-012** (2026-09-27): Whether the configuration ranges SBB validates
  become requirements: `T_max` 750–2000 ms, `T_h` 300–750 ms, `M_W_A` 1–19,
  `N_maxPacket` 1, an SR diagnostic window of 100–10000 messages, and an RL
  diagnosis window of 10–1000 frames. The derived default `T_h = T_max / 4` falls below SBB's
  minimum `T_h`, and SBB endpoints with `T_max` 750 ms and `T_h` 300 ms time
  out on an idle link (OBS-SBB-TIME-005, OBS-SBB-TIME-007).
- **Q-013** (2026-09-27): Whether one `StackEngine` should keep handling
  exactly one connection. SBB handles several connections in one stack
  instance.
- **Q-014** (2026-09-27): Whether the application should learn the reason of a
  received disconnection request and set the detailed reason of its own. The
  implementation follows SBB in not reporting the peer's reason
  (OBS-SBB-SM-014) and always sends detailed reason zero; Railway-CCS reports
  the peer's reason (OBS-RCCS-SM-001).

## Answered

- **Q-002** (2026-08-30): Which envelope to use for application payloads, for
  example JSON, CBOR, or postcard. The protocol crate carries application
  messages as raw bytes and leaves application models to adapters outside the
  core (`adr/0001-core-codec-boundary.md`), so the choice belongs to the
  integration layer.
- **Q-007** (2026-09-27): From which moment the adaptive timer `T_i` runs.
  From message acceptance (`adr/0004-adaptive-timer.md`).
- **Q-008** (2026-09-27): Whether an SR closure also closes the RL channel.
  Every SR closure closes and resets the RL channel, as in SBB
  (`state-machine-status.md`).
- **Q-009** (2026-09-27): Whether the receive side should apply backpressure.
  Yes, as in SBB: received PDUs wait in a 20-entry receive queue until SR has
  room, and the host leaves datagrams in its transport while the queue lacks
  room (OBS-SBB-RL-007).
- **Q-010** (2026-09-27): Which error counters and diagnostics to provide.
  Those of SBB: five SR error counters, `T_rtd` and `T_alive` distributions
  over a diagnostic window, and per-channel RL diagnostics, reported as stack
  actions (OBS-SBB-DIAG-001 to OBS-SBB-DIAG-003).
