//! Core utilities for suspending execution and waiting on results.

#![warn(missing_docs, missing_debug_implementations, rust_2018_idioms)]

pub use self::types::Expiry;

#[macro_use]
mod macros;

mod raw_lock;

pub mod listen;

mod types;
