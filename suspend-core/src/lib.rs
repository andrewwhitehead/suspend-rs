//! Core utilities for suspending execution and waiting on results.

#![warn(missing_docs, missing_debug_implementations, rust_2018_idioms)]

pub use self::error::LockError;
pub use self::types::Expiry;

#[macro_use]
mod macros;

mod error;

mod raw_lock;

pub mod lock;

pub mod listen;

pub mod shared;

mod types;

pub mod util;
