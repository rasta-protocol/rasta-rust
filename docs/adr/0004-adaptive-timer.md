# ADR 0004: Run the adaptive timer `T_i` from message acceptance

- Status: Accepted
- Date: 2026-10-09

## Context

Each timeout-relevant message (Heartbeat, Data, Retransmitted Data) carries
the peer's confirmation of a local timestamp (`CTS`). On acceptance the engine
computes the round-trip delay `T_rtd = T_local + granularity - CTS` and the
adaptive timer `T_i = T_max - T_rtd`: the part of `T_max` that remains after
the round trip. A message with `T_rtd > T_max` is not timely and closes the
connection. `T_max` bounds the age of the peer's most recent confirmation.

What remains to decide is from which moment `T_i` runs. Three readings are
possible:

1. **From acceptance.** The timeout falls at `CTS + T_max`: the confirmation
   may be at most `T_max` old, which is the meaning of `T_max`.
2. **From the confirmed timestamp.** The timeout falls at
   `CTS + T_max - T_rtd`, so the round-trip delay counts twice. This is
   inconsistent with the timeliness check: a message with
   `T_max / 2 < T_rtd <= T_max` is accepted as timely, yet its timer has
   already expired at acceptance. It also cannot keep idle connections up
   unless the heartbeat period is short. Without application data, each side
   sends a Heartbeat when its own heartbeat period `T_h` has passed, so a
   confirmation can already be up to `T_h` plus latency old when it arrives.
   This reading then needs roughly `3 * T_h + latency < T_max`.
3. **A fixed `T_max` from acceptance, ignoring `T_rtd`.** The timeout falls at
   `CTS + T_rtd + T_max`, so a confirmation may be up to `T_max + T_rtd` old.
   The timer no longer adapts to the round-trip delay, and supervision is
   weaker than `T_max` states.

## Decision

`T_i = T_max - T_rtd` is (re)started when a timeout-relevant message is
accepted, and the next accepted one replaces it (reading 1). While a
connection is being established, `T_i` runs with the full `T_max` from the
moment the client opens or the server accepts the connection request. The
connection times out when `T_i` expires.

Equivalently, the peer's most recent confirmation of a local timestamp may be
at most `T_max` (less the timer granularity) old. The timeliness check on
acceptance is unchanged.

This decision has not been confirmed against the normative specification.

## Consequences

- An idle connection stays up while the two heartbeat periods and the
  latency together stay below `T_max`; with equal periods,
  `2 * T_h + latency < T_max`. With `T_h = T_max / 2` it is marginal.
  `ConnectionConfig::new` therefore derives `T_h = T_max / 4`; explicit timing
  remains available through `ConnectionConfig::new_with_timing`.
- A peer that runs its timer under reading 2 needs roughly
  `2 * T_h(peer) + T_h(local) + latency < T_max`. A shorter local heartbeat
  period can make such a link work; it is a configuration choice for that
  peer, not a default.
- The rule only affects local supervision. The wire format and the messages
  sent are unchanged.
- `TimingState` records when `T_i` was started (`timer_started_at`), so the
  timer stays part of the replayable snapshot.

## Verification

- `src/engine/timing.rs` unit tests and
  `tests/connection_engine.rs::a_late_but_timely_message_restarts_t_i_instead_of_timing_out`:
  a message with `T_rtd > T_max / 2` is accepted without timing out, and the
  timeout follows exactly `T_max` after the confirmed timestamp. The engine
  test fails under reading 2.
- `tests/idle_link.rs`: in deterministic simulated time, idle connections stay
  up both with the derived defaults and with `T_max = 750`, `T_h = 300`, and a
  silent peer times out within `T_max`. The 750/300 case fails under
  reading 2.
