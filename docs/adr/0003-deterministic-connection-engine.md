# ADR 0003: Model SR connection behaviour as atomic event/effect transitions

- Status: Accepted
- Date: 2026-08-20

## Context

A connection's behaviour depends on random numbers, time, buffers,
notifications, and transport. If state transitions read, call, or perform
those directly, exhaustive replay and isolated transition testing become hard.

## Decision

Represent one connection as a copyable configuration plus a complete snapshot.
Each step consumes one explicit event and a caller-supplied wrapping monotonic
time. Initial sequence-number entropy is also supplied by the caller. A step
returns a fixed-capacity ordered effect batch for transport and application
adapters.

Transitions are atomic: the engine commits its candidate snapshot only after
the whole transition succeeds. No step reads a clock, opens a channel, sends a
packet, allocates memory, or invokes a callback directly.

Outbound effects contain semantic PDUs that convert to the existing SR codec
types. This keeps state decisions separate from byte encoding and safety-code
generation.

## Consequences

Runs can be recorded and replayed exactly, including sequence-number and time
wraparound. Adapters must execute effects in order and surface execution
failures separately. A fixed effect bound is part of the reviewed design and
is checked fallibly rather than assumed.
