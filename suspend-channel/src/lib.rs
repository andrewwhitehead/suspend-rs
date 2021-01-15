//! Channel and stream primitives.

#![warn(missing_docs, missing_debug_implementations, rust_2018_idioms)]
#![cfg_attr(not(feature = "std"), no_std)]

#[cfg(not(feature = "std"))]
extern crate alloc;

pub use self::{
    channel::{channel, send_once, Flush, ReceiveOnce, Receiver, SendOnce, Sender},
    error::{RecvError, TrySendError},
    util::{StreamIterExt, StreamNextMut},
};

#[cfg(feature = "std")]
pub use self::util::{StreamIter, StreamIterMut};

#[cfg(feature = "std")]
#[macro_use]
pub mod async_stream;

mod channel;

mod error;

pub mod task;

mod util;
