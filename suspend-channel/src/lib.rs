//! Channel and stream primitives.

#![warn(missing_docs, missing_debug_implementations, rust_2018_idioms)]

pub use self::{
    channel::{channel, send_once, Flush, ReceiveOnce, Receiver, SendOnce, Sender},
    error::RecvError,
    util::{StreamIter, StreamIterExt, StreamIterMut, StreamNextMut},
};

#[macro_use]
pub mod async_stream;

mod channel;

mod error;

pub mod task;

mod util;
