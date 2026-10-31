# ADR 0001: Separate the deterministic core from transport I/O

- Status: Accepted
- Date: 2026-08-20

## Context

RaSTA's safety and retransmission behaviour must remain testable independently
of operating-system sockets, wall clocks, scheduling, and application models,
and reviewable without them.

## Decision

The protocol is split into these boundaries:

1. a wire codec that parses and serialises bounded PDUs without allocation;
2. a deterministic SRL/RL engine that consumes explicit events and monotonic
   time values and produces explicit effects;
3. transport adapters, such as UDP, that execute those effects; and
4. optional application-model adapters maintained outside the protocol core.

The core may not open sockets, read clocks, sleep, spawn tasks, or depend on an
async runtime. Network input is borrowed, size-bounded, and fallibly decoded.
The codec uses explicit little-endian operations and caller-provided output
buffers.

## Consequences

The state machine can be replayed deterministically and tested with simulated
time and loss. UDP and async-runtime details stay outside the safety-relevant
logic. The API is slightly more explicit because callers own buffers
and I/O effects.
