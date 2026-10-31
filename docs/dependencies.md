# Dependency and build inventory

The protocol crate has no third-party Rust dependencies. `Cargo.lock` and
`rust-toolchain.toml` pin the dependency graph and compiler channel.
The development toolchain file is excluded from the distributed crate.

The minimum supported Rust version (MSRV) is 1.83, matching `Cargo.toml`.
The timing API uses mutable references in `const fn`, which Rust 1.81 rejects.
CI builds and runs the same test suite on Linux, Windows, and macOS using both
Rust 1.83.0 and the pinned compiler. Both jobs also check the `no_std` core.

The pinned-toolchain CI job runs tests, optimized stack-budget tests, Clippy,
formatting, `no_std` checks, and builds and runs the examples on Linux,
Windows, and macOS. On Linux it also validates the differential-test fixtures.
Workflow actions are pinned to commit hashes.

Supported verification profiles:

- `cargo +1.83.0 test --locked --all-targets --all-features`;
- `cargo +1.83.0 check --locked --no-default-features`;
- `cargo check --no-default-features`: deterministic `no_std` protocol core;
- `cargo test --all-targets --all-features`: core plus standard-library
  transport, simulator, tests, and examples;
- `cargo clippy --all-targets --all-features -- -D warnings`;
- `cargo fmt --all -- --check`.
