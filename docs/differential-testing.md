# Differential testing

`tools/differential_harness.py` compares deterministic JSON-lines oracle
wrappers for the pinned SBB, Railway-CCS, and EULYNX revisions recorded in
`provenance.md`.

Each wrapper receives one JSON object per line and returns one JSON object per
line. The response must echo `case`, provide `accepted`, and, when accepted,
provide lowercase hexadecimal `wire`. The fixture contains protocol inputs and
the Rust-observed wire value. Configure wrappers with:

```sh
SBB_RASTA_ORACLE=/path/to/wrapper \
RCCS_RASTA_ORACLE=/path/to/wrapper \
EULYNX_RASTA_ORACLE=/path/to/wrapper \
python3 tools/differential_harness.py
```

The harness fails on a missing configured executable, malformed response,
timeout, case mismatch, or divergent result. Without configured wrappers it
validates fixtures and reports skipped external references. Agreement is
interoperability evidence, not proof of conformance.

## MD4 safety code

The MD4 implementation is compared with the pinned SBB (`srmd4`) and EULYNX
(`rastamd4`) implementations. Common MD4 libraries cannot serve as oracles
because they do not accept a custom initial value.

A bench outside this repository computes
9208 cases: every input length from 0 to 1150 bytes, eight cases per length
(two with the standard initial value, six with random initial values; random,
all-zero and all-0xFF data). RaSTA-Rust reproduces all 9208 EULYNX full MD4
digests. RaSTA-Rust also writes and verifies 8- and 16-byte safety codes that
reject a flipped bit.

The padding-boundary vectors in `tests/safety_code.rs` come from the EULYNX
MD4 comparisons in this bench.

## Interoperability with the SBB stack

Two processes exchange RaSTA over two UDP transport channels on loopback: one
runs RaSTA-Rust (`StackEngine`), the other the complete pinned SBB stack (SR
and redundancy layer) with an integration layer written for the test. Both
use protocol version `0303`, the lower MD4 safety code with the standard
initial value, check code C (CRC-32C), `N_SENDMAX` 20 and `T_seq` 50 ms. The
bench is not part of this repository. Results (2026-10-10), with RaSTA-Rust
both as client and as server:

- Connection establishment succeeds in both role assignments.
- Data is delivered in both directions for the tested application payloads.
- With `T_max` 2000 ms and `T_h` 300 ms on both sides, idle links stay up for
  20 s. With the SBB defaults (750/300), idle links stay up only when
  RaSTA-Rust's `T_h` is short enough (OBS-SBB-TIME-007).

## Interoperability with EULYNX Live

`examples/udp_peer.rs` exchanges RaSTA with `rcat_udp` of the pinned EULYNX
Live revision over two UDP transport channels on loopback. Both use EULYNX's
local example configuration: protocol version `0303`, `N_SENDMAX` 10,
`N_maxPacket` 3 and `T_max` 10000 ms, once without a safety code and with
check code A, and once with the lower MD4 safety code and check code C
(CRC-32C). `trying-it-out.md` has the commands. Results (2026-11-01), with
RaSTA-Rust both as client and as server:

- Connection establishment succeeds in both role assignments and both
  profiles.
- Data is delivered in both directions.
- A disconnection request from either side closes both endpoints.
- With different safety-code settings on the two sides, the connection is not
  established.

## Building the pinned references

The pinned revisions build with the following commands, which keep generated
files outside this repository:

```sh
cmake -S /path/to/sbb-rasta-stack/source/modules \
  -B /tmp/rasta-ref-sbb -DCMAKE_BUILD_TYPE=Debug -DBUILD_TESTING=OFF
cmake --build /tmp/rasta-ref-sbb

cmake -S /path/to/railway-ccs-rasta \
  -B /tmp/rasta-ref-rccs -DCMAKE_BUILD_TYPE=Debug \
  -DBUILD_LOCAL_EXAMPLES=OFF -DBUILD_REMOTE_EXAMPLES=OFF -DBUILD_TESTING=OFF
cmake --build /tmp/rasta-ref-rccs

cmake -S /path/to/eulynx-librasta \
  -B /tmp/rasta-ref-eulynx -DCMAKE_BUILD_TYPE=Debug \
  -DBUILD_LOCAL_EXAMPLES=OFF -DBUILD_TESTING=OFF
cmake --build /tmp/rasta-ref-eulynx
```

SBB's top-level CMake configuration requires GoogleTest even when
`BUILD_TESTING=OFF`, so the library-only build starts at `source/modules`.
Railway-CCS and EULYNX require CUnit when `BUILD_TESTING=ON`. Their upstream
unit suites therefore require the `libgtest-dev` and `libcunit1-dev` system
packages respectively; the protocol libraries themselves build without them.

Reference-specific JSON-lines wrappers are intentionally not part of the Rust
crate. They must be reviewed against each reference's public API and license
before being configured as an oracle. Until wrappers are configured, the
harness verifies the deterministic fixture schema and Rust-observed wire
values but does not claim differential agreement.
