# ADR 0002: Keep the protocol MD4 primitive internal and configurable

- Status: Accepted
- Date: 2026-08-20

## Context

RaSTA calculates SR safety codes with MD4 and four configurable initial state
words. The calculation is small, deterministic, and protocol-specific. Adding a
general cryptographic dependency would enlarge the assessment surface, and
general MD4 libraries do not expose a configurable initial state.

MD4 is cryptographically broken as a modern authentication hash. It is used
here because the protocol requires it, not as a project choice for general
security.

## Decision

Implement a small, safe, allocation-free MD4 primitive inside `rasta-rust`.
Expose the four initial words explicitly, and provide one-shot calculation plus
in-place SR safety-code writing and verification. Support no code, the first
eight digest bytes, and the complete sixteen-byte digest.

The implementation has no `unsafe` code or external dependencies. Known-answer
tests cover standard MD4 vectors, an SR PDU with a known safety code, custom
initial states, padding boundaries, lower/full widths, and tampering.

## Consequences

The implementation is part of the scope of any safety assessment of this
crate. It is covered by known-answer tests and by differential testing
(`../differential-testing.md`). The initial words are always explicit
configuration: the standard MD4 values are available as a constant, not as an
implied default. Documentation must continue to warn that this primitive is
not suitable for general cryptographic authentication.
