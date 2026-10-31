//! Deterministic, transport-independent primitives for `RaSTA`.
//!
//! This crate is experimental and does not yet claim protocol conformance or
//! safety certification. Current wire behaviour is based on labelled
//! observations of public implementations.

#![no_std]
#![forbid(unsafe_code)]
#![deny(missing_docs)]

#[cfg(feature = "std")]
extern crate std;

pub mod codec;
pub mod engine;
pub mod redundancy;
pub mod stack;
#[cfg(feature = "std")]
pub mod transport;
