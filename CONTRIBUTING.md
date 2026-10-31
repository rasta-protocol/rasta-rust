# Contributing

RaSTA-Rust is safety-oriented infrastructure. Fast iteration is welcome, but
wire behaviour and protocol-state changes must remain reviewable.

Every change should:

1. identify its source as a normative requirement, a labelled observation, or
   a project design decision;
2. include tests for valid input, boundary values, and malformed input;
3. avoid panics, unsafe Rust, implicit host endianness, unbounded input, and
   hidden I/O in the protocol core;
4. pass formatting, Clippy, and the complete test suite; and
5. update the provenance and traceability documents when reference material
   changes.

Until the official specification has been acquired and reviewed, do not label
behaviour as “RaSTA compliant”. Use observation identifiers from
`docs/requirements/observations.md` instead.
