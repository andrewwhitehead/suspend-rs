//! Core utilities for suspending execution and waiting on results.

#![warn(missing_docs, missing_debug_implementations, rust_2018_idioms)]
#![cfg_attr(not(feature = "std"), no_std)]

#[cfg(not(feature = "std"))]
extern crate alloc;

pub use self::error::LockError;
pub use self::types::{Expiry, ParkResult};

#[macro_use]
mod macros;

mod error;

mod raw_park;

pub mod listen;

#[cfg(feature = "std")]
pub mod scoped;

pub mod shared;

#[cfg(feature = "std")]
pub mod thread;

mod types;

pub mod util;
